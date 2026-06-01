/*
 * Broodlink workspace-api — Chat, Sessions, Model endpoints.
 * Ported from the workspace app chat_routes/session_routes/model_routes + llm_core.
 *
 * Implemented now (chat-mode first cut):
 *   - Session CRUD + message history (ws_sessions, ws_chat_messages)
 *   - Model-endpoint CRUD with encrypted api keys (ws_model_endpoints)
 *   - POST /api/chat_stream: real SSE streaming proxy to OpenAI-compatible,
 *     Anthropic, and Ollama providers, with message persistence + token metrics.
 *
 * Deferred (separate subsystems): the agent tool-loop, RAG/memory/web injection,
 * research, MCP tools, document tools, auto-name, skill extraction. `mode=agent`
 * currently behaves like `mode=chat`.
 *
 * The SSE event shape matches the workspace app byte-for-byte (`data: {json}\n\n`,
 * `data: [DONE]\n\n`, `event: error\ndata: {json}`).
 */

use std::collections::HashMap;
use std::convert::Infallible;
use std::sync::Arc;

use axum::extract::{Multipart, Path, State};
use axum::http::HeaderMap;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::Json;
use chrono::{DateTime, Utc};
use futures::StreamExt;
use serde_json::{json, Value};
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use uuid::Uuid;

use crate::{owner_from, AppState, WsError};

// ---------------------------------------------------------------------------
// Schema
// ---------------------------------------------------------------------------

pub async fn ensure_chat_schema(pg: &sqlx::PgPool) -> Result<(), sqlx::Error> {
    sqlx::raw_sql(
        r#"
        CREATE TABLE IF NOT EXISTS ws_model_endpoints (
            id            TEXT PRIMARY KEY,
            owner         TEXT,
            name          TEXT NOT NULL,
            base_url      TEXT NOT NULL,
            api_key       TEXT NOT NULL DEFAULT '',
            is_enabled    BOOLEAN NOT NULL DEFAULT TRUE,
            model_type    TEXT NOT NULL DEFAULT 'llm',
            supports_tools BOOLEAN,
            cached_models TEXT,
            created_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
            updated_at    TIMESTAMPTZ NOT NULL DEFAULT now()
        );
        CREATE INDEX IF NOT EXISTS ws_model_endpoints_owner_idx ON ws_model_endpoints(owner);

        CREATE TABLE IF NOT EXISTS ws_sessions (
            id                  TEXT PRIMARY KEY,
            owner               TEXT,
            name                TEXT NOT NULL DEFAULT 'New chat',
            endpoint_url        TEXT NOT NULL DEFAULT '',
            model               TEXT NOT NULL DEFAULT '',
            endpoint_id         TEXT,
            rag                 BOOLEAN NOT NULL DEFAULT FALSE,
            archived            BOOLEAN NOT NULL DEFAULT FALSE,
            folder              TEXT,
            is_important        BOOLEAN NOT NULL DEFAULT FALSE,
            message_count       INTEGER NOT NULL DEFAULT 0,
            total_input_tokens  INTEGER NOT NULL DEFAULT 0,
            total_output_tokens INTEGER NOT NULL DEFAULT 0,
            mode                TEXT,
            created_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
            updated_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
            last_message_at     TIMESTAMPTZ
        );
        CREATE INDEX IF NOT EXISTS ws_sessions_owner_idx ON ws_sessions(owner);

        CREATE TABLE IF NOT EXISTS ws_chat_messages (
            id         TEXT PRIMARY KEY,
            session_id TEXT NOT NULL REFERENCES ws_sessions(id) ON DELETE CASCADE,
            role       TEXT NOT NULL,
            content    TEXT NOT NULL,
            metadata   TEXT,
            created_at TIMESTAMPTZ NOT NULL DEFAULT now()
        );
        CREATE INDEX IF NOT EXISTS ws_chat_messages_session_idx ON ws_chat_messages(session_id);
        "#,
    )
    .execute(pg)
    .await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Multipart helper (browser FormData → text field map; files ignored)
// ---------------------------------------------------------------------------

pub async fn form_map(mut mp: Multipart) -> Result<HashMap<String, String>, WsError> {
    let mut m = HashMap::new();
    while let Some(field) = mp
        .next_field()
        .await
        .map_err(|e| WsError::BadRequest(e.to_string()))?
    {
        let name = field.name().unwrap_or("").to_string();
        if name.is_empty() {
            continue;
        }
        if let Ok(text) = field.text().await {
            m.insert(name, text);
        }
    }
    Ok(m)
}

// ---------------------------------------------------------------------------
// Sessions
// ---------------------------------------------------------------------------

#[derive(sqlx::FromRow)]
struct SessionRow {
    id: String,
    name: String,
    endpoint_url: String,
    model: String,
    endpoint_id: Option<String>,
    rag: bool,
    archived: bool,
    folder: Option<String>,
    is_important: bool,
    message_count: i32,
    total_input_tokens: i32,
    total_output_tokens: i32,
    mode: Option<String>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
    last_message_at: Option<DateTime<Utc>>,
}

fn session_json(s: &SessionRow) -> Value {
    json!({
        "id": s.id,
        "name": s.name,
        "model": s.model,
        "endpoint_url": s.endpoint_url,
        "endpoint_id": s.endpoint_id,
        "rag": s.rag,
        "archived": s.archived,
        "folder": s.folder,
        "is_important": s.is_important,
        "message_count": s.message_count,
        "total_tokens": s.total_input_tokens + s.total_output_tokens,
        "mode": s.mode,
        "created_at": s.created_at.to_rfc3339(),
        "updated_at": s.updated_at.to_rfc3339(),
        "last_message_at": s.last_message_at.map(|d| d.to_rfc3339()),
    })
}

const SESSION_SELECT: &str = "SELECT id, name, endpoint_url, model, endpoint_id, rag, archived, \
    folder, is_important, message_count, total_input_tokens, total_output_tokens, mode, \
    created_at, updated_at, last_message_at FROM ws_sessions";

pub async fn list_sessions(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Json<Value>, WsError> {
    let owner = owner_from(&headers);
    let rows = sqlx::query_as::<_, SessionRow>(&format!(
        "{SESSION_SELECT} WHERE owner = $1 AND archived = FALSE \
         ORDER BY is_important DESC, COALESCE(last_message_at, updated_at) DESC"
    ))
    .bind(&owner)
    .fetch_all(&state.pg)
    .await?;
    let sessions: Vec<Value> = rows.iter().map(session_json).collect();
    Ok(Json(json!(sessions)))
}

async fn fetch_session(state: &AppState, id: &str, owner: &str) -> Result<SessionRow, WsError> {
    let row = sqlx::query_as::<_, SessionRow>(&format!("{SESSION_SELECT} WHERE id = $1"))
        .bind(id)
        .fetch_optional(&state.pg)
        .await?
        .ok_or_else(|| WsError::NotFound("session not found".into()))?;
    // owner scoping (legacy NULL-owner rows are shared)
    let row_owner: Option<String> =
        sqlx::query_scalar("SELECT owner FROM ws_sessions WHERE id = $1")
            .bind(id)
            .fetch_one(&state.pg)
            .await?;
    if row_owner.as_deref().map(|o| o != owner).unwrap_or(false) {
        return Err(WsError::NotFound("session not found".into()));
    }
    Ok(row)
}

pub async fn create_session(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    mp: Multipart,
) -> Result<Json<Value>, WsError> {
    let owner = owner_from(&headers);
    let f = form_map(mp).await?;
    let id = Uuid::new_v4().to_string();
    let name = f
        .get("name")
        .cloned()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "New chat".into());
    let endpoint_url = f.get("endpoint_url").cloned().unwrap_or_default();
    let model = f.get("model").cloned().unwrap_or_default();
    let endpoint_id = f.get("endpoint_id").cloned().filter(|s| !s.is_empty());
    let rag = f.get("rag").map(|v| v == "true").unwrap_or(false);

    sqlx::query(
        "INSERT INTO ws_sessions (id, owner, name, endpoint_url, model, endpoint_id, rag) \
         VALUES ($1,$2,$3,$4,$5,$6,$7)",
    )
    .bind(&id)
    .bind(&owner)
    .bind(&name)
    .bind(&endpoint_url)
    .bind(&model)
    .bind(&endpoint_id)
    .bind(rag)
    .execute(&state.pg)
    .await?;

    Ok(Json(
        json!({ "id": id, "name": name, "model": model, "rag": rag, "archived": false }),
    ))
}

pub async fn patch_session(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(sid): Path<String>,
    mp: Multipart,
) -> Result<Json<Value>, WsError> {
    let owner = owner_from(&headers);
    fetch_session(&state, &sid, &owner).await?;
    let f = form_map(mp).await?;

    sqlx::query(
        "UPDATE ws_sessions SET \
            name = COALESCE($2, name), \
            folder = COALESCE($3, folder), \
            model = COALESCE($4, model), \
            endpoint_url = COALESCE($5, endpoint_url), \
            updated_at = now() \
         WHERE id = $1",
    )
    .bind(&sid)
    .bind(f.get("name"))
    .bind(f.get("folder"))
    .bind(f.get("model").filter(|s| !s.is_empty()))
    .bind(f.get("endpoint_url").filter(|s| !s.is_empty()))
    .execute(&state.pg)
    .await?;

    let s = fetch_session(&state, &sid, &owner).await?;
    Ok(Json(session_json(&s)))
}

pub async fn delete_session(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(sid): Path<String>,
) -> Result<Json<Value>, WsError> {
    let owner = owner_from(&headers);
    let s = fetch_session(&state, &sid, &owner).await?;
    if s.is_important {
        return Err(WsError::BadRequest(
            "cannot delete a starred session".into(),
        ));
    }
    sqlx::query("DELETE FROM ws_sessions WHERE id = $1")
        .bind(&sid)
        .execute(&state.pg)
        .await?;
    Ok(Json(json!({ "ok": true })))
}

pub async fn history(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(sid): Path<String>,
) -> Result<Json<Value>, WsError> {
    let owner = owner_from(&headers);
    fetch_session(&state, &sid, &owner).await?;
    let rows = sqlx::query_as::<_, (String, String, Option<String>)>(
        "SELECT role, content, metadata FROM ws_chat_messages WHERE session_id = $1 ORDER BY created_at ASC",
    )
    .bind(&sid)
    .fetch_all(&state.pg)
    .await?;
    let history: Vec<Value> = rows
        .into_iter()
        .map(|(role, content, metadata)| {
            json!({
                "role": role,
                "content": content,
                "metadata": metadata.and_then(|m| serde_json::from_str::<Value>(&m).ok()).unwrap_or(Value::Null),
            })
        })
        .collect();
    Ok(Json(json!({ "history": history })))
}

// ---------------------------------------------------------------------------
// Model endpoints
// ---------------------------------------------------------------------------

pub async fn add_endpoint(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    mp: Multipart,
) -> Result<Json<Value>, WsError> {
    let owner = owner_from(&headers);
    let f = form_map(mp).await?;
    let base_url = f
        .get("base_url")
        .cloned()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| WsError::BadRequest("base_url is required".into()))?;
    let name = f
        .get("name")
        .cloned()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| base_url.clone());
    let api_key = f.get("api_key").cloned().unwrap_or_default();
    let model_type = f.get("model_type").cloned().unwrap_or_else(|| "llm".into());
    let supports_tools = f.get("supports_tools").and_then(|v| match v.as_str() {
        "true" => Some(true),
        "false" => Some(false),
        _ => None,
    });
    let skip_probe = f.get("skip_probe").map(|v| v == "true").unwrap_or(false);

    // Probe /models unless skipped.
    let mut models: Vec<String> = Vec::new();
    let mut status = "online";
    if !skip_probe {
        match probe_models(&base_url, &api_key).await {
            Ok(m) => models = m,
            Err(_) => status = "offline",
        }
    }

    let id = format!("ep_{}", Uuid::new_v4().simple());
    sqlx::query(
        "INSERT INTO ws_model_endpoints (id, owner, name, base_url, api_key, model_type, supports_tools, cached_models) \
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8)",
    )
    .bind(&id)
    .bind(&owner)
    .bind(&name)
    .bind(&base_url)
    .bind(state.cipher.encrypt(&api_key))
    .bind(&model_type)
    .bind(supports_tools)
    .bind(serde_json::to_string(&models).ok())
    .execute(&state.pg)
    .await?;

    Ok(Json(json!({
        "id": id, "name": name, "base_url": base_url, "models": models,
        "online": status == "online", "status": status
    })))
}

pub async fn list_endpoints(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Json<Value>, WsError> {
    let owner = owner_from(&headers);
    let rows = sqlx::query_as::<_, (String, String, String, String, bool, String, Option<bool>, Option<String>)>(
        "SELECT id, name, base_url, api_key, is_enabled, model_type, supports_tools, cached_models \
         FROM ws_model_endpoints WHERE owner = $1 ORDER BY created_at ASC",
    )
    .bind(&owner)
    .fetch_all(&state.pg)
    .await?;
    let out: Vec<Value> = rows
        .into_iter()
        .map(
            |(id, name, base_url, api_key, is_enabled, model_type, supports_tools, cached)| {
                let models: Vec<String> = cached
                    .and_then(|c| serde_json::from_str(&c).ok())
                    .unwrap_or_default();
                json!({
                    "id": id, "name": name, "base_url": base_url,
                    "has_key": !api_key.is_empty(), "is_enabled": is_enabled,
                    "models": models, "model_type": model_type, "supports_tools": supports_tools,
                    "online": true, "status": "online"
                })
            },
        )
        .collect();
    Ok(Json(json!(out)))
}

pub async fn delete_endpoint(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(ep_id): Path<String>,
) -> Result<Json<Value>, WsError> {
    let owner = owner_from(&headers);
    sqlx::query("DELETE FROM ws_model_endpoints WHERE id = $1 AND owner = $2")
        .bind(&ep_id)
        .bind(&owner)
        .execute(&state.pg)
        .await?;
    Ok(Json(json!({ "ok": true })))
}

pub async fn list_models(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Json<Value>, WsError> {
    let owner = owner_from(&headers);
    let rows = sqlx::query_as::<_, (String, String, String, String, bool, Option<String>)>(
        "SELECT id, name, base_url, model_type, is_enabled, cached_models \
         FROM ws_model_endpoints WHERE owner = $1 AND is_enabled = TRUE ORDER BY created_at ASC",
    )
    .bind(&owner)
    .fetch_all(&state.pg)
    .await?;
    let items: Vec<Value> = rows
        .into_iter()
        .map(|(id, name, base_url, model_type, _enabled, cached)| {
            let models: Vec<String> = cached
                .and_then(|c| serde_json::from_str(&c).ok())
                .unwrap_or_default();
            json!({
                "host": "custom",
                "url": chat_completions_url(&base_url),
                "models": models,
                "models_display": models,
                "models_extra": [],
                "models_extra_display": [],
                "endpoint_id": id,
                "endpoint_name": name,
                "category": "api",
                "model_type": model_type,
                "offline": false,
            })
        })
        .collect();
    Ok(Json(json!({ "hosts": [], "items": items })))
}

pub async fn default_chat(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Json<Value>, WsError> {
    let owner = owner_from(&headers);
    let row = sqlx::query_as::<_, (String, String, Option<String>)>(
        "SELECT id, base_url, cached_models FROM ws_model_endpoints \
         WHERE owner = $1 AND is_enabled = TRUE ORDER BY created_at ASC LIMIT 1",
    )
    .bind(&owner)
    .fetch_optional(&state.pg)
    .await?;
    match row {
        Some((id, base_url, cached)) => {
            let model = cached
                .and_then(|c| serde_json::from_str::<Vec<String>>(&c).ok())
                .and_then(|v| v.into_iter().next())
                .unwrap_or_default();
            Ok(Json(json!({
                "endpoint_id": id,
                "endpoint_url": chat_completions_url(&base_url),
                "model": model
            })))
        }
        None => Ok(Json(
            json!({ "endpoint_id": Value::Null, "endpoint_url": "", "model": "" }),
        )),
    }
}

async fn probe_models(base_url: &str, api_key: &str) -> Result<Vec<String>, String> {
    let url = format!("{}/models", base_url.trim_end_matches('/'));
    let client = reqwest::Client::new();
    let mut req = client.get(&url);
    if !api_key.is_empty() {
        req = req.bearer_auth(api_key);
    }
    let resp = req.send().await.map_err(|e| e.to_string())?;
    let v: Value = resp.json().await.map_err(|e| e.to_string())?;
    let models = v["data"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|m| m["id"].as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    Ok(models)
}

// ---------------------------------------------------------------------------
// Provider plumbing
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq)]
enum Provider {
    OpenAi,
    Anthropic,
    Ollama,
}

fn detect_provider(url: &str) -> Provider {
    let u = url.to_lowercase();
    if u.contains("anthropic.com") {
        Provider::Anthropic
    } else if u.contains("localhost:11434") || u.contains("ollama") {
        Provider::Ollama
    } else {
        Provider::OpenAi
    }
}

pub fn chat_completions_url(base: &str) -> String {
    let b = base.trim_end_matches('/');
    if b.contains("/chat/completions") || b.contains("/messages") || b.contains("/api/chat") {
        b.to_string()
    } else {
        format!("{b}/chat/completions")
    }
}

fn normalize_url(provider: Provider, url: &str) -> String {
    let u = url.trim_end_matches('/');
    match provider {
        Provider::Anthropic => {
            if u.ends_with("/messages") {
                u.to_string()
            } else if u.contains("/v1") {
                format!(
                    "{}/messages",
                    u.split("/v1").next().unwrap_or(u).trim_end_matches('/')
                )
                .replace("/messages", "/v1/messages")
            } else {
                format!("{u}/v1/messages")
            }
        }
        Provider::Ollama => {
            if u.ends_with("/api/chat") {
                u.to_string()
            } else {
                format!("{}/api/chat", u.trim_end_matches("/v1"))
            }
        }
        Provider::OpenAi => {
            if u.contains("/chat/completions") {
                u.to_string()
            } else {
                format!("{u}/chat/completions")
            }
        }
    }
}

/// A streamed fragment of an OpenAI tool call (assembled across deltas by index).
#[derive(Default, Debug, PartialEq, Clone)]
struct ToolCallFrag {
    index: u32,
    id: Option<String>,
    name: Option<String>,
    args: Option<String>,
}

/// One parsed line from an upstream stream.
#[derive(Default, Debug, PartialEq)]
struct LineEvent {
    delta: Option<String>,
    done: bool,
    input_tokens: Option<u32>,
    output_tokens: Option<u32>,
    tool_calls: Vec<ToolCallFrag>,
    finish: Option<String>,
}

fn parse_openai_line(line: &str) -> LineEvent {
    let line = line.trim();
    let data = match line.strip_prefix("data:") {
        Some(d) => d.trim(),
        None => return LineEvent::default(),
    };
    if data == "[DONE]" {
        return LineEvent {
            done: true,
            ..Default::default()
        };
    }
    let v: Value = match serde_json::from_str(data) {
        Ok(v) => v,
        Err(_) => return LineEvent::default(),
    };
    let delta = v["choices"][0]["delta"]["content"]
        .as_str()
        .map(String::from);
    let input_tokens = v["usage"]["prompt_tokens"].as_u64().map(|n| n as u32);
    let output_tokens = v["usage"]["completion_tokens"].as_u64().map(|n| n as u32);
    let mut tool_calls = Vec::new();
    if let Some(arr) = v["choices"][0]["delta"]["tool_calls"].as_array() {
        for tc in arr {
            tool_calls.push(ToolCallFrag {
                index: tc["index"].as_u64().unwrap_or(0) as u32,
                id: tc["id"].as_str().map(String::from),
                name: tc["function"]["name"].as_str().map(String::from),
                args: tc["function"]["arguments"].as_str().map(String::from),
            });
        }
    }
    let finish = v["choices"][0]["finish_reason"].as_str().map(String::from);
    LineEvent {
        delta,
        done: false,
        input_tokens,
        output_tokens,
        tool_calls,
        finish,
    }
}

fn parse_anthropic_line(line: &str) -> LineEvent {
    let line = line.trim();
    let data = match line.strip_prefix("data:") {
        Some(d) => d.trim(),
        None => return LineEvent::default(),
    };
    let v: Value = match serde_json::from_str(data) {
        Ok(v) => v,
        Err(_) => return LineEvent::default(),
    };
    match v["type"].as_str() {
        Some("content_block_delta") => LineEvent {
            delta: v["delta"]["text"].as_str().map(String::from),
            ..Default::default()
        },
        Some("message_start") => LineEvent {
            input_tokens: v["message"]["usage"]["input_tokens"]
                .as_u64()
                .map(|n| n as u32),
            ..Default::default()
        },
        Some("message_delta") => LineEvent {
            output_tokens: v["usage"]["output_tokens"].as_u64().map(|n| n as u32),
            ..Default::default()
        },
        Some("message_stop") => LineEvent {
            done: true,
            ..Default::default()
        },
        _ => LineEvent::default(),
    }
}

fn parse_ollama_line(line: &str) -> LineEvent {
    let line = line.trim();
    if line.is_empty() {
        return LineEvent::default();
    }
    let v: Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(_) => return LineEvent::default(),
    };
    LineEvent {
        delta: v["message"]["content"].as_str().map(String::from),
        done: v["done"].as_bool().unwrap_or(false),
        input_tokens: v["prompt_eval_count"].as_u64().map(|n| n as u32),
        output_tokens: v["eval_count"].as_u64().map(|n| n as u32),
        ..Default::default()
    }
}

fn parse_line(provider: Provider, line: &str) -> LineEvent {
    match provider {
        Provider::OpenAi => parse_openai_line(line),
        Provider::Anthropic => parse_anthropic_line(line),
        Provider::Ollama => parse_ollama_line(line),
    }
}

fn build_payload(provider: Provider, model: &str, messages: &[(String, String)]) -> Value {
    match provider {
        Provider::Anthropic => {
            let system = messages
                .iter()
                .filter(|(r, _)| r == "system")
                .map(|(_, c)| c.clone())
                .collect::<Vec<_>>()
                .join("\n\n");
            let msgs: Vec<Value> = messages
                .iter()
                .filter(|(r, _)| r != "system")
                .map(|(r, c)| json!({ "role": r, "content": c }))
                .collect();
            json!({
                "model": model,
                "system": system,
                "messages": msgs,
                "max_tokens": 4096,
                "stream": true,
            })
        }
        Provider::Ollama => json!({
            "model": model,
            "messages": messages.iter().map(|(r, c)| json!({"role": r, "content": c})).collect::<Vec<_>>(),
            "stream": true,
        }),
        Provider::OpenAi => json!({
            "model": model,
            "messages": messages.iter().map(|(r, c)| json!({"role": r, "content": c})).collect::<Vec<_>>(),
            "stream": true,
            "stream_options": { "include_usage": true },
        }),
    }
}

// ---------------------------------------------------------------------------
// Chat stream
// ---------------------------------------------------------------------------

fn ev(v: &Value) -> Result<Event, Infallible> {
    Ok(Event::default().data(v.to_string()))
}

pub async fn chat_stream(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    mp: Multipart,
) -> Sse<ReceiverStream<Result<Event, Infallible>>> {
    let owner = owner_from(&headers);
    let form = form_map(mp).await.unwrap_or_default();
    let (tx, rx) = mpsc::channel::<Result<Event, Infallible>>(64);

    tokio::spawn(async move {
        if let Err(e) = run_chat(state, owner, form, &tx).await {
            let _ = tx
                .send(Ok(Event::default().event("error").data(
                    json!({ "error": e.to_string(), "status": 500 }).to_string(),
                )))
                .await;
        }
        let _ = tx.send(Ok(Event::default().data("[DONE]"))).await;
    });

    Sse::new(ReceiverStream::new(rx)).keep_alive(KeepAlive::default())
}

async fn run_chat(
    state: Arc<AppState>,
    owner: String,
    form: HashMap<String, String>,
    tx: &mpsc::Sender<Result<Event, Infallible>>,
) -> Result<(), WsError> {
    let message = form.get("message").cloned().unwrap_or_default();
    let sid = form
        .get("session")
        .cloned()
        .ok_or_else(|| WsError::BadRequest("session required".into()))?;
    let session = fetch_session(&state, &sid, &owner).await?;

    // Resolve endpoint/model/key.
    let model = form
        .get("model")
        .cloned()
        .filter(|s| !s.is_empty())
        .unwrap_or(session.model.clone());
    let endpoint_url = form
        .get("endpoint_url")
        .cloned()
        .filter(|s| !s.is_empty())
        .unwrap_or(session.endpoint_url.clone());
    if endpoint_url.is_empty() || model.is_empty() {
        return Err(WsError::BadRequest(
            "session has no model/endpoint configured".into(),
        ));
    }
    let api_key = resolve_api_key(&state, &owner, &session, &endpoint_url).await;
    let provider = detect_provider(&endpoint_url);

    // Persist the user message.
    insert_message(&state, &sid, "user", &message).await?;

    // Assemble history (system preface + prior turns + new message).
    let mut messages: Vec<(String, String)> =
        vec![("system".into(), "You are a helpful assistant.".into())];
    let prior = sqlx::query_as::<_, (String, String)>(
        "SELECT role, content FROM ws_chat_messages WHERE session_id = $1 ORDER BY created_at ASC",
    )
    .bind(&sid)
    .fetch_all(&state.pg)
    .await?;
    for (role, content) in prior {
        messages.push((role, content));
    }

    // Inject relevant memories (pinned + keyword-recalled) into the prompt.
    let recalled = crate::memory::recall_for_chat(&state, &owner, &message).await;
    if !recalled.is_empty() {
        let block = recalled
            .iter()
            .filter_map(|m| m["text"].as_str())
            .map(|t| format!("- {t}"))
            .collect::<Vec<_>>()
            .join("\n");
        messages.insert(
            1,
            (
                "system".into(),
                format!("Memory context (use only if relevant):\n{block}"),
            ),
        );
        let _ = tx
            .send(ev(&json!({ "type": "memories_used", "data": recalled })))
            .await;
    }

    // Agent mode (tool-calling) is wired to Broodlink's MCP tools, OpenAI-only
    // for now; other providers fall through to plain chat.
    let mode = form.get("mode").map(|s| s.as_str()).unwrap_or("chat");
    if mode == "agent" && provider == Provider::OpenAi {
        return run_agent(state, sid, model, endpoint_url, api_key, messages, tx).await;
    }

    let _ = tx
        .send(ev(
            &json!({ "type": "model_info", "model": model, "suffix": Value::Null }),
        ))
        .await;

    // Stream from the provider.
    let url = normalize_url(provider, &endpoint_url);
    let payload = build_payload(provider, &model, &messages);
    let client = reqwest::Client::new();
    let mut req = client.post(&url).json(&payload);
    req = match provider {
        Provider::Anthropic => req
            .header("x-api-key", &api_key)
            .header("anthropic-version", "2023-06-01"),
        Provider::Ollama => req,
        Provider::OpenAi => {
            if api_key.is_empty() {
                req
            } else {
                req.bearer_auth(&api_key)
            }
        }
    };

    let resp = req
        .send()
        .await
        .map_err(|e| WsError::Internal(e.to_string()))?;
    if !resp.status().is_success() {
        let status = resp.status().as_u16();
        let text = resp.text().await.unwrap_or_default();
        let _ = tx
            .send(Ok(Event::default().event("error").data(
                json!({ "error": "upstream error", "status": status, "text": text }).to_string(),
            )))
            .await;
        return Ok(());
    }

    let started = std::time::Instant::now();
    let mut full = String::new();
    let mut in_tokens = 0u32;
    let mut out_tokens = 0u32;
    let mut buf = String::new();
    let mut stream = resp.bytes_stream();

    while let Some(chunk) = stream.next().await {
        let bytes = chunk.map_err(|e| WsError::Internal(e.to_string()))?;
        buf.push_str(&String::from_utf8_lossy(&bytes));
        while let Some(pos) = buf.find('\n') {
            let line: String = buf.drain(..=pos).collect();
            let le = parse_line(provider, &line);
            if let Some(d) = le.delta {
                if !d.is_empty() {
                    full.push_str(&d);
                    let _ = tx.send(ev(&json!({ "delta": d }))).await;
                }
            }
            if let Some(t) = le.input_tokens {
                in_tokens = t;
            }
            if let Some(t) = le.output_tokens {
                out_tokens = t;
            }
            if le.done {
                buf.clear();
                break;
            }
        }
    }

    // Persist assistant message + update counters.
    let msg_id = insert_message(&state, &sid, "assistant", &full).await?;
    let elapsed = started.elapsed().as_secs_f64();
    sqlx::query(
        "UPDATE ws_sessions SET message_count = message_count + 2, \
            total_input_tokens = total_input_tokens + $2, \
            total_output_tokens = total_output_tokens + $3, \
            last_message_at = now(), updated_at = now() WHERE id = $1",
    )
    .bind(&sid)
    .bind(in_tokens as i32)
    .bind(out_tokens as i32)
    .execute(&state.pg)
    .await?;

    let tps = if elapsed > 0.0 {
        out_tokens as f64 / elapsed
    } else {
        0.0
    };
    let _ = tx
        .send(ev(&json!({
            "type": "metrics",
            "data": {
                "input_tokens": in_tokens,
                "output_tokens": out_tokens,
                "tokens_per_second": tps,
                "response_time": elapsed,
                "model": model,
            }
        })))
        .await;
    let _ = tx
        .send(ev(&json!({ "type": "message_saved", "id": msg_id })))
        .await;
    Ok(())
}

// ---------------------------------------------------------------------------
// Agent loop (OpenAI tool-calling) over Broodlink's MCP tools
// ---------------------------------------------------------------------------

const MAX_AGENT_ROUNDS: u32 = 8;

#[derive(Default, Clone)]
struct AccumTC {
    id: String,
    name: String,
    args: String,
}

/// Stream one OpenAI completion, emitting text deltas live and assembling any
/// tool calls. Returns (content, tool_calls, input_tokens, output_tokens).
async fn stream_openai_once(
    http: &reqwest::Client,
    url: &str,
    api_key: &str,
    payload: Value,
    tx: &mpsc::Sender<Result<Event, Infallible>>,
) -> Result<(String, Vec<AccumTC>, u32, u32), WsError> {
    let mut req = http.post(url).json(&payload);
    if !api_key.is_empty() {
        req = req.bearer_auth(api_key);
    }
    let resp = req
        .send()
        .await
        .map_err(|e| WsError::Internal(e.to_string()))?;
    if !resp.status().is_success() {
        let status = resp.status().as_u16();
        let text = resp.text().await.unwrap_or_default();
        return Err(WsError::Internal(format!("upstream {status}: {text}")));
    }

    let mut full = String::new();
    let mut in_tok = 0u32;
    let mut out_tok = 0u32;
    let mut tcs: std::collections::BTreeMap<u32, AccumTC> = std::collections::BTreeMap::new();
    let mut buf = String::new();
    let mut stream = resp.bytes_stream();

    while let Some(chunk) = stream.next().await {
        let bytes = chunk.map_err(|e| WsError::Internal(e.to_string()))?;
        buf.push_str(&String::from_utf8_lossy(&bytes));
        while let Some(pos) = buf.find('\n') {
            let line: String = buf.drain(..=pos).collect();
            let le = parse_openai_line(&line);
            if let Some(d) = le.delta {
                if !d.is_empty() {
                    full.push_str(&d);
                    let _ = tx.send(ev(&json!({ "delta": d }))).await;
                }
            }
            if let Some(t) = le.input_tokens {
                in_tok = t;
            }
            if let Some(t) = le.output_tokens {
                out_tok = t;
            }
            for frag in le.tool_calls {
                let e = tcs.entry(frag.index).or_default();
                if let Some(id) = frag.id {
                    e.id = id;
                }
                if let Some(name) = frag.name {
                    e.name = name;
                }
                if let Some(args) = frag.args {
                    e.args.push_str(&args);
                }
            }
            if le.done {
                buf.clear();
                break;
            }
        }
    }
    Ok((full, tcs.into_values().collect(), in_tok, out_tok))
}

async fn run_agent(
    state: Arc<AppState>,
    sid: String,
    model: String,
    endpoint_url: String,
    api_key: String,
    base_msgs: Vec<(String, String)>,
    tx: &mpsc::Sender<Result<Event, Infallible>>,
) -> Result<(), WsError> {
    let _ = tx
        .send(ev(
            &json!({ "type": "model_info", "model": model, "suffix": "Agent" }),
        ))
        .await;

    let mcp = crate::mcp::McpClient::from_env();
    let tools = match mcp.list_tools().await {
        Ok(t) => crate::mcp::to_openai_tools(&t),
        Err(e) => {
            tracing::warn!(error = %e, "MCP tools/list failed; agent runs without tools");
            Vec::new()
        }
    };

    let mut messages: Vec<Value> = base_msgs
        .iter()
        .map(|(r, c)| json!({ "role": r, "content": c }))
        .collect();
    let url = normalize_url(Provider::OpenAi, &endpoint_url);
    let http = reqwest::Client::new();
    let started = std::time::Instant::now();
    let mut total_in = 0u32;
    let mut total_out = 0u32;
    let mut final_text = String::new();

    for round in 1..=MAX_AGENT_ROUNDS {
        let mut payload = json!({
            "model": model,
            "messages": messages,
            "stream": true,
            "stream_options": { "include_usage": true },
        });
        if !tools.is_empty() {
            payload["tools"] = json!(tools);
            payload["tool_choice"] = json!("auto");
        }

        let (content, tcs, in_t, out_t) =
            stream_openai_once(&http, &url, &api_key, payload, tx).await?;
        total_in += in_t;
        total_out += out_t;

        if tcs.is_empty() {
            final_text = content;
            break;
        }

        // Record the assistant's tool-call turn.
        let tool_calls_json: Vec<Value> = tcs
            .iter()
            .map(|t| {
                json!({
                    "id": t.id,
                    "type": "function",
                    "function": { "name": t.name, "arguments": t.args }
                })
            })
            .collect();
        let mut asst = json!({ "role": "assistant" });
        asst["content"] = if content.is_empty() {
            Value::Null
        } else {
            json!(content)
        };
        asst["tool_calls"] = json!(tool_calls_json);
        messages.push(asst);

        // Execute each tool via Broodlink MCP and feed results back.
        for t in &tcs {
            let args: Value = serde_json::from_str(&t.args).unwrap_or_else(|_| json!({}));
            let _ = tx
                .send(ev(&json!({
                    "type": "tool_start", "tool": t.name, "round": round, "command": t.args
                })))
                .await;
            let result = mcp
                .call_tool(&t.name, args)
                .await
                .unwrap_or_else(|e| format!("tool error: {e}"));
            let _ = tx
                .send(ev(&json!({
                    "type": "tool_output", "tool": t.name, "output": result, "round": round
                })))
                .await;
            messages.push(json!({ "role": "tool", "tool_call_id": t.id, "content": result }));
        }
        let _ = tx
            .send(ev(
                &json!({ "type": "agent_step", "round": round, "status": "continuing" }),
            ))
            .await;

        if round == MAX_AGENT_ROUNDS {
            final_text = "[agent stopped: reached max tool rounds]".into();
        }
    }

    let msg_id = insert_message(&state, &sid, "assistant", &final_text).await?;
    let elapsed = started.elapsed().as_secs_f64();
    sqlx::query(
        "UPDATE ws_sessions SET message_count = message_count + 2, \
            total_input_tokens = total_input_tokens + $2, \
            total_output_tokens = total_output_tokens + $3, \
            last_message_at = now(), updated_at = now() WHERE id = $1",
    )
    .bind(&sid)
    .bind(total_in as i32)
    .bind(total_out as i32)
    .execute(&state.pg)
    .await?;

    let tps = if elapsed > 0.0 {
        total_out as f64 / elapsed
    } else {
        0.0
    };
    let _ = tx
        .send(ev(&json!({
            "type": "metrics",
            "data": {
                "input_tokens": total_in, "output_tokens": total_out,
                "tokens_per_second": tps, "response_time": elapsed, "model": model
            }
        })))
        .await;
    let _ = tx
        .send(ev(&json!({ "type": "message_saved", "id": msg_id })))
        .await;
    Ok(())
}

async fn insert_message(
    state: &AppState,
    sid: &str,
    role: &str,
    content: &str,
) -> Result<String, WsError> {
    let id = Uuid::new_v4().to_string();
    sqlx::query(
        "INSERT INTO ws_chat_messages (id, session_id, role, content) VALUES ($1,$2,$3,$4)",
    )
    .bind(&id)
    .bind(sid)
    .bind(role)
    .bind(content)
    .execute(&state.pg)
    .await?;
    Ok(id)
}

/// Resolve the decrypted API key for the session's endpoint (by endpoint_id, or
/// by matching base_url among the owner's endpoints).
async fn resolve_api_key(
    state: &AppState,
    owner: &str,
    session: &SessionRow,
    endpoint_url: &str,
) -> String {
    if let Some(eid) = &session.endpoint_id {
        if let Ok(Some(k)) =
            sqlx::query_scalar::<_, String>("SELECT api_key FROM ws_model_endpoints WHERE id = $1")
                .bind(eid)
                .fetch_optional(&state.pg)
                .await
        {
            return state.cipher.decrypt(&k);
        }
    }
    // Fallback: match by base_url prefix.
    if let Ok(rows) = sqlx::query_as::<_, (String, String)>(
        "SELECT base_url, api_key FROM ws_model_endpoints WHERE owner = $1",
    )
    .bind(owner)
    .fetch_all(&state.pg)
    .await
    {
        for (base, key) in rows {
            if endpoint_url.starts_with(base.trim_end_matches('/')) {
                return state.cipher.decrypt(&key);
            }
        }
    }
    String::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn openai_delta_and_usage() {
        let d = parse_openai_line(r#"data: {"choices":[{"delta":{"content":"Hello"}}]}"#);
        assert_eq!(d.delta.as_deref(), Some("Hello"));
        assert!(!d.done);
        let done = parse_openai_line("data: [DONE]");
        assert!(done.done);
        let usage = parse_openai_line(
            r#"data: {"choices":[],"usage":{"prompt_tokens":12,"completion_tokens":34}}"#,
        );
        assert_eq!(usage.input_tokens, Some(12));
        assert_eq!(usage.output_tokens, Some(34));
    }

    #[test]
    fn anthropic_delta() {
        let d = parse_anthropic_line(
            r#"data: {"type":"content_block_delta","delta":{"type":"text_delta","text":"Hi"}}"#,
        );
        assert_eq!(d.delta.as_deref(), Some("Hi"));
        let stop = parse_anthropic_line(r#"data: {"type":"message_stop"}"#);
        assert!(stop.done);
    }

    #[test]
    fn ollama_delta() {
        let d = parse_ollama_line(r#"{"message":{"content":"yo"},"done":false}"#);
        assert_eq!(d.delta.as_deref(), Some("yo"));
        let done = parse_ollama_line(r#"{"message":{"content":""},"done":true,"eval_count":7}"#);
        assert!(done.done);
        assert_eq!(done.output_tokens, Some(7));
    }

    #[test]
    fn provider_detect_and_url() {
        assert!(matches!(
            detect_provider("https://api.anthropic.com/v1/messages"),
            Provider::Anthropic
        ));
        assert!(matches!(
            detect_provider("http://localhost:11434"),
            Provider::Ollama
        ));
        assert!(matches!(
            detect_provider("https://api.openai.com/v1/chat/completions"),
            Provider::OpenAi
        ));
        assert_eq!(
            chat_completions_url("https://api.openai.com/v1"),
            "https://api.openai.com/v1/chat/completions"
        );
        assert_eq!(
            normalize_url(Provider::Ollama, "http://localhost:11434/v1"),
            "http://localhost:11434/api/chat"
        );
    }
}
