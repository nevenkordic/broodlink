/*
 * Broodlink workspace-api — Memory.
 * Ported from the workspace app routes/memory_routes.py, converged onto Broodlink.
 *
 * Storage model: a canonical owner-scoped table (ws_memories) holds the fields
 * Broodlink's tool API doesn't expose (pinned / uses / category / owner /
 * timestamp / session link), and every write is MIRRORED into Broodlink's
 * shared memory (Qdrant) via MCP store_memory/delete_memory — so memories the
 * user adds here are visible to the whole agent fleet and reachable by the
 * agent tool-loop's semantic_search. Reads/owner-scoping come from the table.
 *
 * Deferred (need the LLM): /extract, /import, /audit. They return safe shapes.
 */

use std::sync::Arc;

use axum::extract::{Multipart, Path, State};
use axum::http::HeaderMap;
use axum::Json;
use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{owner_from, AppState, WsError};

pub async fn ensure_memory_schema(pg: &sqlx::PgPool) -> Result<(), sqlx::Error> {
    sqlx::raw_sql(
        r#"
        CREATE TABLE IF NOT EXISTS ws_memories (
            id         TEXT PRIMARY KEY,
            owner      TEXT,
            text       TEXT NOT NULL,
            category   TEXT NOT NULL DEFAULT 'fact',
            source     TEXT NOT NULL DEFAULT 'user',
            session_id TEXT,
            pinned     BOOLEAN NOT NULL DEFAULT FALSE,
            uses       INTEGER NOT NULL DEFAULT 0,
            created_at TIMESTAMPTZ NOT NULL DEFAULT now()
        );
        CREATE INDEX IF NOT EXISTS ws_memories_owner_idx ON ws_memories(owner);
        "#,
    )
    .execute(pg)
    .await?;
    Ok(())
}

#[derive(sqlx::FromRow)]
struct MemRow {
    id: String,
    owner: Option<String>,
    text: String,
    category: String,
    source: String,
    session_id: Option<String>,
    pinned: bool,
    uses: i32,
    created_at: DateTime<Utc>,
}

fn mem_json(m: &MemRow) -> Value {
    json!({
        "id": m.id,
        "text": m.text,
        "timestamp": m.created_at.timestamp(),
        "source": m.source,
        "category": m.category,
        "owner": m.owner,
        "session_id": m.session_id,
        "pinned": m.pinned,
        "uses": m.uses,
    })
}

const SELECT: &str = "SELECT id, owner, text, category, source, session_id, pinned, uses, created_at FROM ws_memories";

// --- Broodlink mirror (best-effort; never blocks the user-facing write) ------

fn mirror_store(id: String, text: String, category: String, source: String) {
    tokio::spawn(async move {
        let mcp = crate::mcp::McpClient::from_env();
        let _ = mcp
            .call_tool(
                "store_memory",
                json!({ "topic": id, "content": text, "tags": format!("{category},{source},workspace") }),
            )
            .await;
    });
}

fn mirror_delete(id: String) {
    tokio::spawn(async move {
        let mcp = crate::mcp::McpClient::from_env();
        let _ = mcp.call_tool("delete_memory", json!({ "topic": id })).await;
    });
}

// ---------------------------------------------------------------------------
// CRUD
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct AddBody {
    #[serde(default)]
    text: String,
    #[serde(default = "default_category")]
    category: String,
    #[serde(default = "default_source")]
    source: String,
    session_id: Option<String>,
}
fn default_category() -> String {
    "fact".into()
}
fn default_source() -> String {
    "user".into()
}

pub async fn add(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(body): Json<AddBody>,
) -> Result<Json<Value>, WsError> {
    let owner = owner_from(&headers);
    let text = body.text.trim().to_string();
    if text.is_empty() {
        return Err(WsError::BadRequest("memory text is required".into()));
    }
    // Dedup exact text per owner.
    let dup: Option<String> =
        sqlx::query_scalar("SELECT id FROM ws_memories WHERE owner = $1 AND text = $2")
            .bind(&owner)
            .bind(&text)
            .fetch_optional(&state.pg)
            .await?;
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM ws_memories WHERE owner = $1")
        .bind(&owner)
        .fetch_one(&state.pg)
        .await?;
    if dup.is_some() {
        return Ok(Json(
            json!({ "ok": true, "count": count, "message": "Memory already exists" }),
        ));
    }

    let id = Uuid::new_v4().to_string();
    let pinned = body.category == "identity";
    sqlx::query(
        "INSERT INTO ws_memories (id, owner, text, category, source, session_id, pinned) \
         VALUES ($1,$2,$3,$4,$5,$6,$7)",
    )
    .bind(&id)
    .bind(&owner)
    .bind(&text)
    .bind(&body.category)
    .bind(&body.source)
    .bind(&body.session_id)
    .bind(pinned)
    .execute(&state.pg)
    .await?;

    mirror_store(id, text, body.category.clone(), body.source.clone());
    Ok(Json(json!({ "ok": true, "count": count + 1 })))
}

pub async fn list(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Json<Value>, WsError> {
    let owner = owner_from(&headers);
    let rows = sqlx::query_as::<_, MemRow>(&format!(
        "{SELECT} WHERE owner = $1 ORDER BY pinned DESC, created_at DESC"
    ))
    .bind(&owner)
    .fetch_all(&state.pg)
    .await?;
    let memory: Vec<Value> = rows.iter().map(mem_json).collect();
    Ok(Json(json!({ "memory": memory })))
}

async fn fetch_owned(state: &AppState, id: &str, owner: &str) -> Result<MemRow, WsError> {
    let row = sqlx::query_as::<_, MemRow>(&format!("{SELECT} WHERE id = $1"))
        .bind(id)
        .fetch_optional(&state.pg)
        .await?
        .ok_or_else(|| WsError::NotFound("memory not found".into()))?;
    if row.owner.as_deref().map(|o| o != owner).unwrap_or(false) {
        return Err(WsError::NotFound("memory not found".into()));
    }
    Ok(row)
}

pub async fn get_one(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Value>, WsError> {
    let owner = owner_from(&headers);
    let row = fetch_owned(&state, &id, &owner).await?;
    Ok(Json(json!({ "memory": mem_json(&row) })))
}

pub async fn update(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    mp: Multipart,
) -> Result<Json<Value>, WsError> {
    let owner = owner_from(&headers);
    fetch_owned(&state, &id, &owner).await?;
    let f = crate::chat::form_map(mp).await?;
    let text = f
        .get("text")
        .cloned()
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| WsError::BadRequest("text is required".into()))?;
    let category = f.get("category").cloned();
    sqlx::query(
        "UPDATE ws_memories SET text = $2, category = COALESCE($3, category), created_at = now() WHERE id = $1",
    )
    .bind(&id)
    .bind(&text)
    .bind(&category)
    .execute(&state.pg)
    .await?;
    mirror_store(
        id,
        text,
        category.unwrap_or_else(|| "fact".into()),
        "user".into(),
    );
    Ok(Json(
        json!({ "ok": true, "message": "Memory updated successfully" }),
    ))
}

pub async fn delete_one(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Value>, WsError> {
    let owner = owner_from(&headers);
    fetch_owned(&state, &id, &owner).await?;
    sqlx::query("DELETE FROM ws_memories WHERE id = $1")
        .bind(&id)
        .execute(&state.pg)
        .await?;
    mirror_delete(id);
    Ok(Json(
        json!({ "ok": true, "message": "Memory deleted successfully" }),
    ))
}

pub async fn pin(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    mp: Multipart,
) -> Result<Json<Value>, WsError> {
    let owner = owner_from(&headers);
    fetch_owned(&state, &id, &owner).await?;
    let f = crate::chat::form_map(mp).await?;
    let pinned = f.get("pinned").map(|v| v == "true").unwrap_or(false);
    sqlx::query("UPDATE ws_memories SET pinned = $2 WHERE id = $1")
        .bind(&id)
        .bind(pinned)
        .execute(&state.pg)
        .await?;
    Ok(Json(json!({ "ok": true, "pinned": pinned })))
}

pub async fn search(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    mp: Multipart,
) -> Result<Json<Value>, WsError> {
    let owner = owner_from(&headers);
    let f = crate::chat::form_map(mp).await?;
    let query = f.get("query").cloned().unwrap_or_default();
    let rows = keyword_search(&state, &owner, &query, 20).await?;
    let memories: Vec<Value> = rows.iter().map(mem_json).collect();
    Ok(Json(
        json!({ "memories": memories, "total": memories.len(), "query": query }),
    ))
}

async fn keyword_search(
    state: &AppState,
    owner: &str,
    query: &str,
    limit: i64,
) -> Result<Vec<MemRow>, WsError> {
    let pattern = format!("%{}%", query.replace('%', "\\%"));
    let rows = sqlx::query_as::<_, MemRow>(&format!(
        "{SELECT} WHERE owner = $1 AND text ILIKE $2 ORDER BY pinned DESC, created_at DESC LIMIT $3"
    ))
    .bind(owner)
    .bind(&pattern)
    .bind(limit)
    .fetch_all(&state.pg)
    .await?;
    Ok(rows)
}

pub async fn timeline(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Json<Value>, WsError> {
    let owner = owner_from(&headers);
    let rows = sqlx::query_as::<_, MemRow>(&format!(
        "{SELECT} WHERE owner = $1 ORDER BY created_at DESC"
    ))
    .bind(&owner)
    .fetch_all(&state.pg)
    .await?;
    let timeline: Vec<Value> = rows
        .iter()
        .map(|m| {
            let mut v = mem_json(m);
            v["timestamp_str"] = json!(m.created_at.format("%Y-%m-%d %H:%M:%S").to_string());
            v["session_name"] = json!("Unknown");
            v
        })
        .collect();
    Ok(Json(json!({ "timeline": timeline, "total": rows.len() })))
}

pub async fn by_session(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(sid): Path<String>,
) -> Result<Json<Value>, WsError> {
    let owner = owner_from(&headers);
    let rows = sqlx::query_as::<_, MemRow>(&format!(
        "{SELECT} WHERE owner = $1 AND session_id = $2 ORDER BY created_at DESC"
    ))
    .bind(&owner)
    .bind(&sid)
    .fetch_all(&state.pg)
    .await?;
    let memories: Vec<Value> = rows.iter().map(mem_json).collect();
    Ok(Json(json!({
        "session_id": sid, "session_name": "Unknown",
        "memory_count": memories.len(), "memories": memories
    })))
}

// --- LLM-dependent endpoints (deferred) -----------------------------------

pub async fn extract(headers: HeaderMap) -> Json<Value> {
    let _ = owner_from(&headers);
    Json(json!({ "suggestions": [] }))
}

pub async fn audit(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Json<Value>, WsError> {
    let owner = owner_from(&headers);
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM ws_memories WHERE owner = $1")
        .bind(&owner)
        .fetch_one(&state.pg)
        .await?;
    Ok(Json(
        json!({ "ok": true, "before": count, "after": count, "removed": 0, "already_tidy": true }),
    ))
}

// ---------------------------------------------------------------------------
// Chat recall — used by chat.rs to inject relevant memories into the prompt.
// Returns [{text, category, type}], and bumps `uses` on what it surfaces.
// ---------------------------------------------------------------------------

pub async fn recall_for_chat(state: &AppState, owner: &str, message: &str) -> Vec<Value> {
    let mut out = Vec::new();
    let mut used_ids = Vec::new();

    // Pinned: always included.
    if let Ok(pinned) = sqlx::query_as::<_, MemRow>(&format!(
        "{SELECT} WHERE owner = $1 AND pinned = TRUE ORDER BY created_at DESC LIMIT 20"
    ))
    .bind(owner)
    .fetch_all(&state.pg)
    .await
    {
        for m in &pinned {
            out.push(json!({ "text": m.text, "category": m.category, "type": "pinned" }));
            used_ids.push(m.id.clone());
        }
    }

    // Recalled: top keyword matches (excluding pinned).
    if !message.trim().is_empty() {
        if let Ok(rows) = keyword_search(state, owner, message, 3).await {
            for m in rows.into_iter().filter(|m| !m.pinned) {
                out.push(json!({ "text": m.text, "category": m.category, "type": "recalled" }));
                used_ids.push(m.id);
            }
        }
    }

    if !used_ids.is_empty() {
        let _ = sqlx::query("UPDATE ws_memories SET uses = uses + 1 WHERE id = ANY($1)")
            .bind(&used_ids)
            .execute(&state.pg)
            .await;
    }
    out
}
