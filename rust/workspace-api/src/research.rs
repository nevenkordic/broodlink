/*
 * Broodlink workspace-api — Deep Research.
 * Ported from the workspace app research_routes. Data plane (run records,
 * library, archive, spinoff) is implemented; the actual multi-step LLM+search
 * research loop is not ported, so `start` records the run as failed-pending.
 */

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{Html, IntoResponse, Response};
use axum::Json;
use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{owner_from, AppState, WsError};

pub async fn ensure_research_schema(pg: &sqlx::PgPool) -> Result<(), sqlx::Error> {
    sqlx::raw_sql(
        r#"
        CREATE TABLE IF NOT EXISTS ws_research (
            session_id   TEXT PRIMARY KEY,
            owner        TEXT,
            query        TEXT NOT NULL,
            status       TEXT NOT NULL DEFAULT 'running',
            result       TEXT,
            sources      TEXT NOT NULL DEFAULT '[]',
            category     TEXT,
            archived     BOOLEAN NOT NULL DEFAULT FALSE,
            started_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
            completed_at TIMESTAMPTZ
        );
        CREATE INDEX IF NOT EXISTS ws_research_owner_idx ON ws_research(owner);
        "#,
    )
    .execute(pg)
    .await?;
    Ok(())
}

#[derive(Deserialize)]
pub struct StartBody {
    query: String,
    category: Option<String>,
}

pub async fn start(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(b): Json<StartBody>,
) -> Result<Json<Value>, WsError> {
    let owner = owner_from(&headers);
    let id = format!("rp-{}", Uuid::new_v4().simple());
    sqlx::query(
        "INSERT INTO ws_research (session_id, owner, query, status, category) VALUES ($1,$2,$3,'running',$4)",
    )
    .bind(&id)
    .bind(&owner)
    .bind(&b.query)
    .bind(&b.category)
    .execute(&state.pg)
    .await?;

    // Single-round pipeline: web search → LLM synthesis. (Multi-round
    // iterative deepening is a future enhancement.)
    let st = Arc::clone(&state);
    let (sid, query, o) = (id.clone(), b.query.clone(), owner.clone());
    tokio::spawn(async move {
        let sources = crate::search::fetch(&query, 6).await;
        let mut ctx = String::new();
        for (i, s) in sources.iter().enumerate() {
            ctx.push_str(&format!(
                "[{}] {}\n{}\n{}\n\n",
                i + 1,
                s["title"].as_str().unwrap_or(""),
                s["url"].as_str().unwrap_or(""),
                s["snippet"].as_str().unwrap_or("")
            ));
        }
        let sys = "You are a research assistant. Write a thorough, well-structured report \
                   answering the user's query using ONLY the provided sources. Cite sources as [n].";
        let user = format!("Query: {query}\n\nSources:\n{ctx}");
        let (status, result) = match crate::chat::complete_text(&st, &o, sys, &user).await {
            Ok(r) => ("done", r),
            Err(e) => ("error", format!("research failed: {e}")),
        };
        let sources_json = serde_json::to_string(
            &sources
                .iter()
                .map(|s| json!({ "title": s["title"], "url": s["url"], "snippet": s["snippet"] }))
                .collect::<Vec<_>>(),
        )
        .unwrap_or_else(|_| "[]".into());
        let _ = sqlx::query(
            "UPDATE ws_research SET status = $2, result = $3, sources = $4, completed_at = now() WHERE session_id = $1",
        )
        .bind(&sid)
        .bind(status)
        .bind(&result)
        .bind(&sources_json)
        .execute(&st.pg)
        .await;
    });

    Ok(Json(
        json!({ "session_id": id, "status": "running", "query": b.query }),
    ))
}

async fn fetch(state: &AppState, id: &str, owner: &str) -> Result<Value, WsError> {
    let row = sqlx::query_as::<_, (String, Option<String>, String, String, Option<String>, String, Option<String>, bool, DateTime<Utc>, Option<DateTime<Utc>>)>(
        "SELECT session_id, owner, query, status, result, sources, category, archived, started_at, completed_at \
         FROM ws_research WHERE session_id = $1",
    )
    .bind(id)
    .fetch_optional(&state.pg)
    .await?
    .ok_or_else(|| WsError::NotFound("research not found".into()))?;
    if row.1.as_deref().map(|o| o != owner).unwrap_or(false) {
        return Err(WsError::NotFound("research not found".into()));
    }
    Ok(json!({
        "id": row.0, "session_id": row.0, "query": row.2, "status": row.3,
        "result": row.4, "sources": serde_json::from_str::<Value>(&row.5).unwrap_or(json!([])),
        "category": row.6, "archived": row.7,
        "started_at": row.8.timestamp(), "completed_at": row.9.map(|d| d.timestamp()),
        "source_count": serde_json::from_str::<Value>(&row.5).ok().and_then(|v| v.as_array().map(|a| a.len())).unwrap_or(0),
    }))
}

pub async fn status(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Value>, WsError> {
    let owner = owner_from(&headers);
    let r = fetch(&state, &id, &owner).await?;
    Ok(Json(
        json!({ "status": r["status"], "progress": {}, "query": r["query"], "started_at": r["started_at"] }),
    ))
}

pub async fn detail(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Value>, WsError> {
    let owner = owner_from(&headers);
    Ok(Json(fetch(&state, &id, &owner).await?))
}

pub async fn result(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Value>, WsError> {
    let owner = owner_from(&headers);
    let r = fetch(&state, &id, &owner).await?;
    Ok(Json(
        json!({ "result": r["result"], "sources": r["sources"], "raw_findings": [] }),
    ))
}

#[derive(Deserialize)]
pub struct LibQuery {
    search: Option<String>,
    #[serde(default)]
    archived: bool,
    #[serde(default = "default_limit")]
    limit: i64,
}
fn default_limit() -> i64 {
    50
}

pub async fn library(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(q): Query<LibQuery>,
) -> Result<Json<Value>, WsError> {
    let owner = owner_from(&headers);
    let mut sql = String::from(
        "SELECT session_id, query, category, status, sources, archived, started_at, completed_at \
         FROM ws_research WHERE owner = $1 AND archived = $2",
    );
    if q.search.is_some() {
        sql.push_str(" AND query ILIKE $3");
    }
    sql.push_str(&format!(
        " ORDER BY started_at DESC LIMIT {}",
        q.limit.clamp(1, 200)
    ));
    let mut query = sqlx::query_as::<
        _,
        (
            String,
            String,
            Option<String>,
            String,
            String,
            bool,
            DateTime<Utc>,
            Option<DateTime<Utc>>,
        ),
    >(&sql)
    .bind(&owner)
    .bind(q.archived);
    if let Some(s) = &q.search {
        query = query.bind(format!("%{}%", s.replace('%', "\\%")));
    }
    let rows = query.fetch_all(&state.pg).await?;
    let research: Vec<Value> = rows
        .into_iter()
        .map(|(id, query, category, status, sources, archived, started, completed)| {
            let sc = serde_json::from_str::<Value>(&sources).ok().and_then(|v| v.as_array().map(|a| a.len())).unwrap_or(0);
            json!({
                "id": id, "query": query, "category": category, "source_count": sc,
                "status": status, "archived": archived,
                "started_at": started.timestamp(), "completed_at": completed.map(|d| d.timestamp()),
            })
        })
        .collect();
    let total = research.len();
    Ok(Json(json!({ "research": research, "total": total })))
}

#[derive(Deserialize)]
pub struct ArchiveQuery {
    #[serde(default)]
    archived: bool,
}

pub async fn archive(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(q): Query<ArchiveQuery>,
) -> Result<Json<Value>, WsError> {
    let owner = owner_from(&headers);
    sqlx::query("UPDATE ws_research SET archived = $3 WHERE session_id = $1 AND owner = $2")
        .bind(&id)
        .bind(&owner)
        .bind(q.archived)
        .execute(&state.pg)
        .await?;
    Ok(Json(
        json!({ "ok": true, "id": id, "archived": q.archived }),
    ))
}

pub async fn delete_one(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Value>, WsError> {
    let owner = owner_from(&headers);
    sqlx::query("DELETE FROM ws_research WHERE session_id = $1 AND owner = $2")
        .bind(&id)
        .bind(&owner)
        .execute(&state.pg)
        .await?;
    Ok(Json(json!({ "deleted": true })))
}

pub async fn cancel(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Value>, WsError> {
    let owner = owner_from(&headers);
    sqlx::query("UPDATE ws_research SET status = 'cancelled' WHERE session_id = $1 AND owner = $2 AND status = 'running'")
        .bind(&id)
        .bind(&owner)
        .execute(&state.pg)
        .await?;
    Ok(Json(json!({ "cancelled": true })))
}

pub async fn report(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Response, WsError> {
    let owner = owner_from(&headers);
    let r = fetch(&state, &id, &owner).await?;
    let html = format!(
        "<!doctype html><meta charset=utf-8><title>Research</title><body style='font-family:system-ui;max-width:760px;margin:2rem auto'>\
         <h1>{}</h1><pre style='white-space:pre-wrap'>{}</pre></body>",
        r["query"].as_str().unwrap_or(""),
        r["result"].as_str().unwrap_or("")
    );
    Ok(Html(html).into_response())
}

pub async fn stream(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Sse<futures::stream::Once<std::future::Ready<Result<Event, std::convert::Infallible>>>> {
    let owner = owner_from(&headers);
    let status = fetch(&state, &id, &owner)
        .await
        .map(|r| r["status"].as_str().unwrap_or("error").to_string())
        .unwrap_or_else(|_| "error".into());
    let ev = Event::default().data(json!({ "status": status, "final": true }).to_string());
    Sse::new(futures::stream::once(std::future::ready(Ok(ev)))).keep_alive(KeepAlive::default())
}
