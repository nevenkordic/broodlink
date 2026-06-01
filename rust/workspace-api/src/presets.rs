/*
 * Broodlink workspace-api — Chat presets.
 * Ported from the workspace app preset_routes (was file-based; here in Postgres).
 *
 * Implemented: the custom preset, user templates (CRUD), and preset groups.
 * Stubbed (needs LLM): /api/presets/expand (character-prompt generation).
 */

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::HeaderMap;
use axum::Json;
use rand::Rng;
use serde_json::{json, Value};

use crate::{owner_from, AppState, WsError};

pub async fn ensure_presets_schema(pg: &sqlx::PgPool) -> Result<(), sqlx::Error> {
    sqlx::raw_sql(
        r#"
        CREATE TABLE IF NOT EXISTS ws_presets (
            owner      TEXT NOT NULL,
            pid        TEXT NOT NULL,
            kind       TEXT NOT NULL,
            data       TEXT NOT NULL,
            updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
            PRIMARY KEY (owner, pid)
        );
        "#,
    )
    .execute(pg)
    .await?;
    Ok(())
}

fn default_custom() -> Value {
    json!({
        "name": "Custom",
        "temperature": 0.7,
        "max_tokens": 0,
        "system_prompt": "",
        "enabled": false,
        "inject_prefix": Value::Null,
        "inject_suffix": Value::Null
    })
}

async fn load(state: &AppState, owner: &str, pid: &str) -> Result<Option<Value>, WsError> {
    let row: Option<String> =
        sqlx::query_scalar("SELECT data FROM ws_presets WHERE owner = $1 AND pid = $2")
            .bind(owner)
            .bind(pid)
            .fetch_optional(&state.pg)
            .await?;
    Ok(row.and_then(|s| serde_json::from_str(&s).ok()))
}

async fn save(
    state: &AppState,
    owner: &str,
    pid: &str,
    kind: &str,
    data: &Value,
) -> Result<(), WsError> {
    sqlx::query(
        "INSERT INTO ws_presets (owner, pid, kind, data, updated_at) VALUES ($1,$2,$3,$4,now()) \
         ON CONFLICT (owner, pid) DO UPDATE SET data = EXCLUDED.data, kind = EXCLUDED.kind, updated_at = now()",
    )
    .bind(owner)
    .bind(pid)
    .bind(kind)
    .bind(data.to_string())
    .execute(&state.pg)
    .await?;
    Ok(())
}

// GET /api/presets — custom + all templates, keyed by id.
pub async fn list(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Json<Value>, WsError> {
    let owner = owner_from(&headers);
    let custom = load(&state, &owner, "custom")
        .await?
        .unwrap_or_else(default_custom);
    let mut out = serde_json::Map::new();
    out.insert("custom".into(), custom);
    let rows = sqlx::query_as::<_, (String, String)>(
        "SELECT pid, data FROM ws_presets WHERE owner = $1 AND kind = 'template'",
    )
    .bind(&owner)
    .fetch_all(&state.pg)
    .await?;
    for (pid, data) in rows {
        if let Ok(v) = serde_json::from_str::<Value>(&data) {
            out.insert(pid, v);
        }
    }
    Ok(Json(Value::Object(out)))
}

pub async fn set_custom(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Result<Json<Value>, WsError> {
    let owner = owner_from(&headers);
    save(&state, &owner, "custom", "custom", &body).await?;
    Ok(Json(json!({ "success": true, "message": "saved" })))
}

pub async fn list_templates(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Json<Value>, WsError> {
    let owner = owner_from(&headers);
    let rows = sqlx::query_as::<_, (String, String)>(
        "SELECT pid, data FROM ws_presets WHERE owner = $1 AND kind = 'template' ORDER BY updated_at DESC",
    )
    .bind(&owner)
    .fetch_all(&state.pg)
    .await?;
    let templates: Vec<Value> = rows
        .into_iter()
        .filter_map(|(pid, data)| {
            serde_json::from_str::<Value>(&data).ok().map(|mut v| {
                v["id"] = json!(pid);
                v
            })
        })
        .collect();
    Ok(Json(json!(templates)))
}

pub async fn upsert_template(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(mut body): Json<Value>,
) -> Result<Json<Value>, WsError> {
    let owner = owner_from(&headers);
    let pid = body["id"]
        .as_str()
        .filter(|s| !s.is_empty())
        .map(String::from)
        .unwrap_or_else(|| {
            let hex: String = (0..8)
                .map(|_| format!("{:x}", rand::thread_rng().gen::<u8>() & 0xf))
                .collect();
            format!("user-{hex}")
        });
    body["id"] = json!(pid);
    save(&state, &owner, &pid, "template", &body).await?;
    Ok(Json(
        json!({ "success": true, "template": body, "message": "saved" }),
    ))
}

pub async fn delete_template(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Value>, WsError> {
    let owner = owner_from(&headers);
    sqlx::query("DELETE FROM ws_presets WHERE owner = $1 AND pid = $2 AND kind = 'template'")
        .bind(&owner)
        .bind(&id)
        .execute(&state.pg)
        .await?;
    Ok(Json(json!({ "success": true, "message": "deleted" })))
}

pub async fn get_groups(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Json<Value>, WsError> {
    let owner = owner_from(&headers);
    let groups = load(&state, &owner, "groups")
        .await?
        .unwrap_or_else(|| json!([]));
    Ok(Json(json!({ "groups": groups })))
}

pub async fn set_groups(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Result<Json<Value>, WsError> {
    let owner = owner_from(&headers);
    let groups = body.get("groups").cloned().unwrap_or(body);
    save(&state, &owner, "groups", "groups", &groups).await?;
    Ok(Json(json!({ "ok": true })))
}

/// Expand rough character/persona notes into a full system prompt via the LLM.
pub async fn expand(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Json<Value> {
    let owner = owner_from(&headers);
    let sys = "Expand the user's rough character/persona notes into a complete, detailed \
               system prompt for an AI assistant to roleplay. Output only the system prompt.";
    let user = format!(
        "Name: {}\nNotes: {}",
        body["name"].as_str().unwrap_or(""),
        body["prompt"].as_str().unwrap_or("")
    );
    match crate::chat::complete_text(&state, &owner, sys, &user).await {
        Ok(s) => Json(json!({ "success": true, "prompt": s.trim(), "message": "" })),
        Err(e) => {
            Json(json!({ "success": false, "prompt": Value::Null, "message": e.to_string() }))
        }
    }
}
