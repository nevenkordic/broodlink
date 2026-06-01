/*
 * Broodlink workspace-api — Embeddings model management.
 * Ported from the workspace app embedding_routes. The catalog + custom-endpoint
 * config are real (per-owner); local fastembed download/compute is not wired
 * (Broodlink uses its own embedding-worker + Qdrant for shared memory), so the
 * download/delete operations report unavailable.
 */

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::HeaderMap;
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::{owner_from, AppState, WsError};

pub async fn ensure_embeddings_schema(pg: &sqlx::PgPool) -> Result<(), sqlx::Error> {
    sqlx::raw_sql(
        r#"
        CREATE TABLE IF NOT EXISTS ws_embedding_config (
            owner TEXT PRIMARY KEY,
            url   TEXT NOT NULL DEFAULT '',
            model TEXT NOT NULL DEFAULT ''
        );
        "#,
    )
    .execute(pg)
    .await?;
    Ok(())
}

const CATALOG: &[(&str, u32, f64, &str)] = &[
    (
        "sentence-transformers/all-MiniLM-L6-v2",
        384,
        0.09,
        "Fast & tiny, good default",
    ),
    (
        "BAAI/bge-small-en-v1.5",
        384,
        0.07,
        "Small, strong retrieval",
    ),
    (
        "nomic-ai/nomic-embed-text-v1.5-Q",
        768,
        0.13,
        "Quantized, 768d",
    ),
    ("BAAI/bge-base-en-v1.5", 768, 0.21, "Mid-range"),
    ("BAAI/bge-large-en-v1.5", 1024, 1.2, "Highest quality"),
];

pub async fn models() -> Json<Value> {
    let list: Vec<Value> = CATALOG
        .iter()
        .enumerate()
        .map(|(i, (m, dim, size, desc))| {
            json!({
                "model": m, "dim": dim, "size_gb": size, "description": desc,
                "downloaded": false, "downloading": false, "active": i == 0,
                "recommended": i == 0, "cached_size_mb": 0.0
            })
        })
        .collect();
    Json(json!(list))
}

pub async fn model_status(Path(model): Path<String>) -> Json<Value> {
    Json(json!({ "model": model, "downloaded": false, "downloading": false }))
}

pub async fn download(Path(_model): Path<String>) -> Result<Json<Value>, WsError> {
    Err(WsError::BadRequest(
        "local fastembed download not wired — Broodlink uses its embedding-worker + Qdrant".into(),
    ))
}

pub async fn delete_model(Path(model): Path<String>) -> Json<Value> {
    Json(json!({ "deleted": false, "model": model }))
}

pub async fn get_endpoint(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Json<Value>, WsError> {
    let owner = owner_from(&headers);
    let row = sqlx::query_as::<_, (String, String)>(
        "SELECT url, model FROM ws_embedding_config WHERE owner = $1",
    )
    .bind(&owner)
    .fetch_optional(&state.pg)
    .await?;
    let (url, model) = row.unwrap_or_default();
    Ok(Json(
        json!({ "url": url, "model": model, "active": !url.is_empty() }),
    ))
}

#[derive(Deserialize)]
pub struct EndpointForm {
    #[serde(default)]
    url: String,
    #[serde(default)]
    model: String,
}

pub async fn set_endpoint(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    axum::extract::Form(f): axum::extract::Form<EndpointForm>,
) -> Result<Json<Value>, WsError> {
    let owner = owner_from(&headers);
    if !(f.url.starts_with("http://") || f.url.starts_with("https://")) {
        return Err(WsError::BadRequest("invalid url".into()));
    }
    sqlx::query(
        "INSERT INTO ws_embedding_config (owner, url, model) VALUES ($1,$2,$3) \
         ON CONFLICT (owner) DO UPDATE SET url = EXCLUDED.url, model = EXCLUDED.model",
    )
    .bind(&owner)
    .bind(&f.url)
    .bind(&f.model)
    .execute(&state.pg)
    .await?;
    Ok(Json(
        json!({ "success": true, "url": f.url, "model": f.model }),
    ))
}

pub async fn clear_endpoint(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Json<Value>, WsError> {
    let owner = owner_from(&headers);
    sqlx::query("DELETE FROM ws_embedding_config WHERE owner = $1")
        .bind(&owner)
        .execute(&state.pg)
        .await?;
    Ok(Json(json!({ "success": true })))
}
