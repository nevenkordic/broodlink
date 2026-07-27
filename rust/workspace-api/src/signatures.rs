/*
 * Broodlink workspace-api — Saved signatures (drawn PNG stamps).
 * Ported from the workspace app signature_routes. PNG data is encrypted at rest
 * (reuses the crypto module), matching the original's at-rest protection.
 */

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::HeaderMap;
use axum::Json;
use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{owner_from, AppState, WsError};

pub async fn ensure_signatures_schema(pg: &sqlx::PgPool) -> Result<(), sqlx::Error> {
    sqlx::raw_sql(
        r#"
        CREATE TABLE IF NOT EXISTS ws_signatures (
            id         TEXT PRIMARY KEY,
            owner      TEXT,
            name       TEXT NOT NULL DEFAULT 'Signature',
            data_png   TEXT NOT NULL,
            width      INTEGER,
            height     INTEGER,
            svg        TEXT,
            created_at TIMESTAMPTZ NOT NULL DEFAULT now()
        );
        CREATE INDEX IF NOT EXISTS ws_signatures_owner_idx ON ws_signatures(owner);
        "#,
    )
    .execute(pg)
    .await?;
    Ok(())
}

pub async fn list(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Json<Value>, WsError> {
    let owner = owner_from(&headers);
    let rows = sqlx::query_as::<
        _,
        (
            String,
            String,
            String,
            Option<i32>,
            Option<i32>,
            DateTime<Utc>,
        ),
    >(
        "SELECT id, name, data_png, width, height, created_at FROM ws_signatures \
         WHERE owner = $1 ORDER BY created_at DESC",
    )
    .bind(&owner)
    .fetch_all(&state.pg)
    .await?;
    let signatures: Vec<Value> = rows
        .into_iter()
        .map(|(id, name, enc, width, height, created)| {
            let png = state.cipher.decrypt(&enc);
            json!({
                "id": id, "name": name,
                "data_url": format!("data:image/png;base64,{png}"),
                "width": width, "height": height,
                "created_at": created.format("%Y-%m-%dT%H:%M:%SZ").to_string(),
            })
        })
        .collect();
    Ok(Json(json!({ "signatures": signatures })))
}

#[derive(Deserialize)]
pub struct SigCreate {
    #[serde(default = "default_name")]
    name: String,
    #[serde(default)]
    data: String,
    width: Option<i32>,
    height: Option<i32>,
    svg: Option<String>,
}
fn default_name() -> String {
    "Signature".into()
}

pub async fn create(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(body): Json<SigCreate>,
) -> Result<Json<Value>, WsError> {
    let owner = owner_from(&headers);
    // Accept with or without the data: prefix.
    let b64 = body
        .data
        .strip_prefix("data:image/png;base64,")
        .unwrap_or(&body.data)
        .trim()
        .to_string();
    let decoded = base64::decode(b64.as_bytes())
        .map_err(|_| WsError::BadRequest("invalid base64 PNG".into()))?;
    if decoded.is_empty() {
        return Err(WsError::BadRequest("empty PNG".into()));
    }

    let id = Uuid::new_v4().to_string();
    let enc = state.cipher.encrypt(&b64);
    let enc_svg = body.svg.as_ref().map(|s| state.cipher.encrypt(s));
    let created: DateTime<Utc> = sqlx::query_scalar(
        "INSERT INTO ws_signatures (id, owner, name, data_png, width, height, svg) \
         VALUES ($1,$2,$3,$4,$5,$6,$7) RETURNING created_at",
    )
    .bind(&id)
    .bind(&owner)
    .bind(&body.name)
    .bind(&enc)
    .bind(body.width)
    .bind(body.height)
    .bind(&enc_svg)
    .fetch_one(&state.pg)
    .await?;

    Ok(Json(json!({
        "id": id, "name": body.name,
        "data_url": format!("data:image/png;base64,{b64}"),
        "width": body.width, "height": body.height,
        "created_at": created.format("%Y-%m-%dT%H:%M:%SZ").to_string(),
    })))
}

pub async fn delete_one(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Value>, WsError> {
    let owner = owner_from(&headers);
    let found: Option<Option<String>> =
        sqlx::query_scalar("SELECT owner FROM ws_signatures WHERE id = $1")
            .bind(&id)
            .fetch_optional(&state.pg)
            .await?;
    match found {
        None => return Err(WsError::NotFound("signature not found".into())),
        Some(o) if o.as_deref().map(|x| x != owner).unwrap_or(false) => {
            return Err(WsError::Forbidden("not your signature".into()))
        }
        _ => {}
    }
    sqlx::query("DELETE FROM ws_signatures WHERE id = $1")
        .bind(&id)
        .execute(&state.pg)
        .await?;
    Ok(Json(json!({ "deleted": id })))
}
