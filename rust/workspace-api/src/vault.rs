/*
 * Broodlink workspace-api — Secrets vault.
 * Ported from the workspace app vault_routes (a thin wrapper over the Bitwarden
 * `bw` CLI). Config is stored per-owner; the `bw` subprocess integration is not
 * wired here, so lock/unlock/login report the CLI as unavailable.
 */

use std::sync::Arc;

use axum::extract::State;
use axum::http::HeaderMap;
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::{owner_from, AppState, WsError};

pub async fn ensure_vault_schema(pg: &sqlx::PgPool) -> Result<(), sqlx::Error> {
    sqlx::raw_sql(
        r#"
        CREATE TABLE IF NOT EXISTS ws_vault_config (
            owner       TEXT PRIMARY KEY,
            server_url  TEXT NOT NULL DEFAULT '',
            email       TEXT NOT NULL DEFAULT ''
        );
        "#,
    )
    .execute(pg)
    .await?;
    Ok(())
}

fn bw_available() -> bool {
    // Look for the `bw` binary on PATH + common npm-global locations.
    let candidates = ["bw", "/opt/homebrew/bin/bw", "/usr/local/bin/bw"];
    candidates.iter().any(|p| std::path::Path::new(p).exists())
        || std::env::var("PATH").ok().map_or(false, |path| {
            path.split(':')
                .any(|d| std::path::Path::new(d).join("bw").exists())
        })
}

pub async fn get_config(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Json<Value>, WsError> {
    let owner = owner_from(&headers);
    let row = sqlx::query_as::<_, (String, String)>(
        "SELECT server_url, email FROM ws_vault_config WHERE owner = $1",
    )
    .bind(&owner)
    .fetch_optional(&state.pg)
    .await?;
    let (server_url, email) = row.unwrap_or_default();
    Ok(Json(json!({
        "server_url": server_url,
        "email": email,
        "unlocked": false,
        "unlocked_at": Value::Null,
        "bw_installed": bw_available(),
    })))
}

#[derive(Deserialize)]
pub struct ConfigBody {
    #[serde(default)]
    server_url: String,
    #[serde(default)]
    email: String,
}

pub async fn set_config(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(b): Json<ConfigBody>,
) -> Result<Json<Value>, WsError> {
    let owner = owner_from(&headers);
    sqlx::query(
        "INSERT INTO ws_vault_config (owner, server_url, email) VALUES ($1,$2,$3) \
         ON CONFLICT (owner) DO UPDATE SET server_url = EXCLUDED.server_url, email = EXCLUDED.email",
    )
    .bind(&owner)
    .bind(&b.server_url)
    .bind(&b.email)
    .execute(&state.pg)
    .await?;
    Ok(Json(json!({ "ok": true })))
}

// The bw-CLI operations are not wired in this build.
pub async fn cli_unavailable() -> Json<Value> {
    Json(json!({ "ok": false, "error": "Bitwarden CLI (bw) integration not yet ported" }))
}

pub async fn lock() -> Json<Value> {
    Json(json!({ "ok": true, "message": "Vault locked" }))
}
