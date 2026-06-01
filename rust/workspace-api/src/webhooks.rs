/*
 * Broodlink workspace-api — Webhooks.
 * Ported from the workspace app webhook_routes + webhook_manager.
 * CRUD + fire-and-forget delivery with optional HMAC-SHA256 signing. Secrets
 * encrypted at rest. `fire()` is exposed for chat/session events to call.
 */

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::HeaderMap;
use axum::Json;
use chrono::{DateTime, Utc};
use hmac::{Hmac, Mac};
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::Sha256;
use uuid::Uuid;

use crate::{owner_from, AppState, WsError};

const ALLOWED_EVENTS: &[&str] = &[
    "session.created",
    "chat.completed",
    "chat.message",
    "webhook.test",
];

pub async fn ensure_webhooks_schema(pg: &sqlx::PgPool) -> Result<(), sqlx::Error> {
    sqlx::raw_sql(
        r#"
        CREATE TABLE IF NOT EXISTS ws_webhooks (
            id                TEXT PRIMARY KEY,
            owner             TEXT,
            name              TEXT NOT NULL,
            url               TEXT NOT NULL,
            secret            TEXT,
            events            TEXT NOT NULL DEFAULT '',
            is_active         BOOLEAN NOT NULL DEFAULT TRUE,
            last_triggered_at TIMESTAMPTZ,
            last_status_code  INTEGER,
            last_error        TEXT,
            created_at        TIMESTAMPTZ NOT NULL DEFAULT now()
        );
        CREATE INDEX IF NOT EXISTS ws_webhooks_owner_idx ON ws_webhooks(owner);
        "#,
    )
    .execute(pg)
    .await?;
    Ok(())
}

fn webhook_json(
    id: &str,
    name: &str,
    url: &str,
    secret: &Option<String>,
    events: &str,
    is_active: bool,
    last_at: &Option<DateTime<Utc>>,
    last_code: Option<i32>,
    last_err: &Option<String>,
    created: &DateTime<Utc>,
) -> Value {
    json!({
        "id": id, "name": name, "url": url,
        "has_secret": secret.as_ref().map(|s| !s.is_empty()).unwrap_or(false),
        "events": events.split(',').filter(|s| !s.is_empty()).collect::<Vec<_>>(),
        "is_active": is_active,
        "last_triggered_at": last_at.map(|d| d.to_rfc3339()),
        "last_status_code": last_code,
        "last_error": last_err,
        "created_at": created.to_rfc3339(),
    })
}

type Row = (
    String,
    String,
    String,
    Option<String>,
    String,
    bool,
    Option<DateTime<Utc>>,
    Option<i32>,
    Option<String>,
    DateTime<Utc>,
);
const WSELECT: &str = "SELECT id, name, url, secret, events, is_active, last_triggered_at, last_status_code, last_error, created_at FROM ws_webhooks";

pub async fn list(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Json<Value>, WsError> {
    let owner = owner_from(&headers);
    let rows = sqlx::query_as::<_, Row>(&format!(
        "{WSELECT} WHERE owner = $1 ORDER BY created_at DESC"
    ))
    .bind(&owner)
    .fetch_all(&state.pg)
    .await?;
    let out: Vec<Value> = rows
        .iter()
        .map(|r| webhook_json(&r.0, &r.1, &r.2, &r.3, &r.4, r.5, &r.6, r.7, &r.8, &r.9))
        .collect();
    Ok(Json(json!(out)))
}

#[derive(Deserialize)]
pub struct CreateForm {
    name: String,
    url: String,
    #[serde(default)]
    secret: String,
    #[serde(default)]
    events: String,
}

pub async fn create(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    axum::extract::Form(f): axum::extract::Form<CreateForm>,
) -> Result<Json<Value>, WsError> {
    let owner = owner_from(&headers);
    let events: Vec<&str> = f
        .events
        .split(',')
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .collect();
    if events.is_empty() || !events.iter().all(|e| ALLOWED_EVENTS.contains(e)) {
        return Err(WsError::BadRequest("invalid or empty events".into()));
    }
    if !(f.url.starts_with("http://") || f.url.starts_with("https://")) {
        return Err(WsError::BadRequest("url must be http(s)".into()));
    }
    let id = Uuid::new_v4().simple().to_string()[..8].to_string();
    let secret = if f.secret.is_empty() {
        None
    } else {
        Some(state.cipher.encrypt(&f.secret))
    };
    sqlx::query(
        "INSERT INTO ws_webhooks (id, owner, name, url, secret, events) VALUES ($1,$2,$3,$4,$5,$6)",
    )
    .bind(&id)
    .bind(&owner)
    .bind(&f.name)
    .bind(&f.url)
    .bind(&secret)
    .bind(events.join(","))
    .execute(&state.pg)
    .await?;
    Ok(Json(json!({ "id": id, "name": f.name })))
}

pub async fn toggle(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Value>, WsError> {
    let owner = owner_from(&headers);
    let active: Option<bool> = sqlx::query_scalar(
        "UPDATE ws_webhooks SET is_active = NOT is_active WHERE id = $1 AND owner = $2 RETURNING is_active",
    )
    .bind(&id)
    .bind(&owner)
    .fetch_optional(&state.pg)
    .await?;
    let active = active.ok_or_else(|| WsError::NotFound("webhook not found".into()))?;
    Ok(Json(json!({ "id": id, "is_active": active })))
}

pub async fn delete_one(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Value>, WsError> {
    let owner = owner_from(&headers);
    sqlx::query("DELETE FROM ws_webhooks WHERE id = $1 AND owner = $2")
        .bind(&id)
        .bind(&owner)
        .execute(&state.pg)
        .await?;
    Ok(Json(json!({ "status": "deleted" })))
}

pub async fn test(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Value>, WsError> {
    let owner = owner_from(&headers);
    let row = sqlx::query_as::<_, (String, Option<String>)>(
        "SELECT url, secret FROM ws_webhooks WHERE id = $1 AND owner = $2",
    )
    .bind(&id)
    .bind(&owner)
    .fetch_optional(&state.pg)
    .await?
    .ok_or_else(|| WsError::NotFound("webhook not found".into()))?;
    let (url, secret) = row;
    let secret = secret.map(|s| state.cipher.decrypt(&s));
    let payload = json!({ "message": "Test ping from Broodlink" });
    let state2 = Arc::clone(&state);
    tokio::spawn(async move {
        deliver(&state2, &id, &url, &secret, "webhook.test", payload).await;
    });
    Ok(Json(json!({ "status": "sent" })))
}

/// Deliver one webhook, recording status/error. Best-effort.
async fn deliver(
    state: &AppState,
    id: &str,
    url: &str,
    secret: &Option<String>,
    event: &str,
    data: Value,
) {
    let body =
        json!({ "event": event, "timestamp": Utc::now().to_rfc3339(), "data": data }).to_string();
    let client = reqwest::Client::new();
    let mut req = client
        .post(url)
        .header("Content-Type", "application/json")
        .header("X-Broodlink-Event", event)
        .header("User-Agent", "Broodlink-Webhook/1.0");
    if let Some(sec) = secret {
        if !sec.is_empty() {
            if let Ok(mut mac) = Hmac::<Sha256>::new_from_slice(sec.as_bytes()) {
                mac.update(body.as_bytes());
                let sig = hex::encode(mac.finalize().into_bytes());
                req = req.header("X-Broodlink-Signature", sig);
            }
        }
    }
    let (code, err): (Option<i32>, Option<String>) = match req.body(body).send().await {
        Ok(resp) => (Some(resp.status().as_u16() as i32), None),
        Err(e) => (None, Some(e.to_string())),
    };
    let _ = sqlx::query(
        "UPDATE ws_webhooks SET last_triggered_at = now(), last_status_code = $2, last_error = $3 WHERE id = $1",
    )
    .bind(id)
    .bind(code)
    .bind(&err)
    .execute(&state.pg)
    .await;
}

/// Fire an event to all active webhooks subscribed to it (called by chat/session).
pub async fn fire(state: &AppState, owner: &str, event: &str, data: Value) {
    let rows = sqlx::query_as::<_, (String, String, Option<String>)>(
        "SELECT id, url, secret FROM ws_webhooks WHERE owner = $1 AND is_active = TRUE AND events LIKE $2",
    )
    .bind(owner)
    .bind(format!("%{event}%"))
    .fetch_all(&state.pg)
    .await
    .unwrap_or_default();
    for (id, url, secret) in rows {
        let secret = secret.map(|s| state.cipher.decrypt(&s));
        deliver(state, &id, &url, &secret, event, data.clone()).await;
    }
}
