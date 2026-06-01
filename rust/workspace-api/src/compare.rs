/*
 * Broodlink workspace-api — Compare (blind multi-model A/B).
 * Ported from the workspace app compare_routes. The actual generation reuses
 * /api/chat_stream (two sessions stream independently); this module manages the
 * comparison record, blind mapping, and votes.
 */

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::HeaderMap;
use axum::Json;
use chrono::{DateTime, Utc};
use rand::Rng;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{owner_from, AppState, WsError};

pub async fn ensure_compare_schema(pg: &sqlx::PgPool) -> Result<(), sqlx::Error> {
    sqlx::raw_sql(
        r#"
        CREATE TABLE IF NOT EXISTS ws_comparisons (
            id            TEXT PRIMARY KEY,
            owner         TEXT,
            prompt        TEXT NOT NULL,
            model_a       TEXT NOT NULL,
            model_b       TEXT NOT NULL,
            endpoint_a    TEXT NOT NULL DEFAULT '',
            endpoint_b    TEXT NOT NULL DEFAULT '',
            winner        TEXT,
            is_blind      BOOLEAN NOT NULL DEFAULT TRUE,
            blind_mapping TEXT,
            voted_at      TIMESTAMPTZ,
            created_at    TIMESTAMPTZ NOT NULL DEFAULT now()
        );
        CREATE INDEX IF NOT EXISTS ws_comparisons_owner_idx ON ws_comparisons(owner);
        "#,
    )
    .execute(pg)
    .await?;
    Ok(())
}

async fn make_cmp_session(
    state: &AppState,
    owner: &str,
    model: &str,
    endpoint: &str,
) -> Result<String, WsError> {
    let id = Uuid::new_v4().to_string();
    sqlx::query(
        "INSERT INTO ws_sessions (id, owner, name, endpoint_url, model, mode) \
         VALUES ($1,$2,$3,$4,$5,'compare')",
    )
    .bind(&id)
    .bind(owner)
    .bind(format!("[CMP] {model}"))
    .bind(crate::chat::chat_completions_url(endpoint))
    .bind(model)
    .execute(&state.pg)
    .await?;
    Ok(id)
}

#[derive(serde::Deserialize)]
pub struct StartForm {
    prompt: String,
    model_a: String,
    model_b: String,
    #[serde(default)]
    endpoint_a: String,
    #[serde(default)]
    endpoint_b: String,
    #[serde(default)]
    is_blind: Option<String>,
}

pub async fn start(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    axum::extract::Form(f): axum::extract::Form<StartForm>,
) -> Result<Json<Value>, WsError> {
    let owner = owner_from(&headers);
    let is_blind = f.is_blind.as_deref() != Some("false");
    let id = format!("comp_{}", Uuid::new_v4().simple());
    let session_left = make_cmp_session(&state, &owner, &f.model_a, &f.endpoint_a).await?;
    let session_right = make_cmp_session(&state, &owner, &f.model_b, &f.endpoint_b).await?;

    // Blind mapping: which physical side shows model a vs b.
    let swap = is_blind && rand::thread_rng().gen::<bool>();
    let mapping = if swap {
        json!({ "left": "b", "right": "a" })
    } else {
        json!({ "left": "a", "right": "b" })
    };

    sqlx::query(
        "INSERT INTO ws_comparisons (id, owner, prompt, model_a, model_b, endpoint_a, endpoint_b, is_blind, blind_mapping) \
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9)",
    )
    .bind(&id)
    .bind(&owner)
    .bind(&f.prompt)
    .bind(&f.model_a)
    .bind(&f.model_b)
    .bind(&f.endpoint_a)
    .bind(&f.endpoint_b)
    .bind(is_blind)
    .bind(mapping.to_string())
    .execute(&state.pg)
    .await?;

    let (model_left, model_right) = if swap {
        (f.model_b.clone(), f.model_a.clone())
    } else {
        (f.model_a.clone(), f.model_b.clone())
    };
    Ok(Json(json!({
        "id": id,
        "session_left": session_left,
        "session_right": session_right,
        "model_left": model_left,
        "model_right": model_right,
        "is_blind": is_blind,
        "mapping": mapping,
    })))
}

#[derive(serde::Deserialize)]
pub struct VoteForm {
    winner: String,
}

pub async fn vote(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    axum::extract::Form(f): axum::extract::Form<VoteForm>,
) -> Result<Json<Value>, WsError> {
    let owner = owner_from(&headers);
    let row = sqlx::query_as::<_, (String, String, Option<String>, Option<String>)>(
        "SELECT model_a, model_b, blind_mapping, owner FROM ws_comparisons WHERE id = $1",
    )
    .bind(&id)
    .fetch_optional(&state.pg)
    .await?
    .ok_or_else(|| WsError::NotFound("comparison not found".into()))?;
    let (model_a, model_b, mapping, row_owner) = row;
    if row_owner.as_deref().map(|o| o != owner).unwrap_or(false) {
        return Err(WsError::NotFound("comparison not found".into()));
    }
    let map: Value = mapping
        .and_then(|m| serde_json::from_str(&m).ok())
        .unwrap_or(json!({"left":"a","right":"b"}));
    // Translate physical side → model slot.
    let slot = match f.winner.as_str() {
        "left" => map["left"].as_str().unwrap_or("a").to_string(),
        "right" => map["right"].as_str().unwrap_or("b").to_string(),
        other => other.to_string(), // "tie" or direct
    };
    sqlx::query("UPDATE ws_comparisons SET winner = $2, voted_at = now() WHERE id = $1")
        .bind(&id)
        .bind(&slot)
        .execute(&state.pg)
        .await?;
    let winner_name = match slot.as_str() {
        "a" => model_a.clone(),
        "b" => model_b.clone(),
        _ => "tie".into(),
    };
    Ok(Json(json!({
        "winner": winner_name, "model_a": model_a, "model_b": model_b,
        "revealed": { "left": if map["left"]=="a" {&model_a} else {&model_b},
                      "right": if map["right"]=="a" {&model_a} else {&model_b} }
    })))
}

#[derive(serde::Deserialize)]
pub struct RecordBody {
    prompt: String,
    models: Vec<String>,
    winner: Option<String>,
    #[serde(default)]
    is_blind: bool,
}

pub async fn record(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(b): Json<RecordBody>,
) -> Result<Json<Value>, WsError> {
    let owner = owner_from(&headers);
    let id = format!("comp_{}", Uuid::new_v4().simple());
    let model_a = b.models.first().cloned().unwrap_or_default();
    let model_b = b.models.get(1).cloned().unwrap_or_default();
    let mapping = json!({ "models": b.models });
    sqlx::query(
        "INSERT INTO ws_comparisons (id, owner, prompt, model_a, model_b, is_blind, blind_mapping, winner, voted_at) \
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8, now())",
    )
    .bind(&id)
    .bind(&owner)
    .bind(b.prompt.chars().take(500).collect::<String>())
    .bind(&model_a)
    .bind(&model_b)
    .bind(b.is_blind)
    .bind(mapping.to_string())
    .bind(&b.winner)
    .execute(&state.pg)
    .await?;
    Ok(Json(json!({ "status": "ok", "id": id })))
}

pub async fn history(
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
            String,
            Option<String>,
            bool,
            Option<DateTime<Utc>>,
            DateTime<Utc>,
        ),
    >(
        "SELECT id, prompt, model_a, model_b, winner, is_blind, voted_at, created_at \
         FROM ws_comparisons WHERE owner = $1 ORDER BY created_at DESC LIMIT 50",
    )
    .bind(&owner)
    .fetch_all(&state.pg)
    .await?;
    let comparisons: Vec<Value> = rows
        .into_iter()
        .map(|(id, prompt, a, b, winner, blind, voted, created)| {
            json!({
                "id": id,
                "prompt": prompt.chars().take(100).collect::<String>(),
                "model_a": a, "model_b": b, "winner": winner, "is_blind": blind,
                "voted_at": voted.map(|d| d.to_rfc3339()), "created_at": created.to_rfc3339(),
            })
        })
        .collect();
    Ok(Json(json!({ "comparisons": comparisons })))
}

pub async fn delete_one(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Value>, WsError> {
    let owner = owner_from(&headers);
    sqlx::query("DELETE FROM ws_comparisons WHERE id = $1 AND owner = $2")
        .bind(&id)
        .bind(&owner)
        .execute(&state.pg)
        .await?;
    Ok(Json(json!({ "status": "deleted" })))
}
