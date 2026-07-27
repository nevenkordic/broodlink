/*
 * Broodlink workspace-api — Notes endpoints.
 * Ported from the workspace app routes/note_routes.py; JSON contract preserved so the
 * existing frontend works unchanged.
 */

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::Json;
use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{owner_from, AppState, WsError};

#[derive(sqlx::FromRow)]
struct NoteRow {
    id: String,
    owner: Option<String>,
    title: String,
    content: Option<String>,
    items: Option<String>,
    note_type: String,
    color: Option<String>,
    label: Option<String>,
    pinned: bool,
    archived: bool,
    due_date: Option<String>,
    source: String,
    session_id: Option<String>,
    sort_order: i32,
    image_url: Option<String>,
    repeat: String,
    ai_classification: Option<String>,
    ai_content_hash: Option<String>,
    agent_session_id: Option<String>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

fn ts(dt: &DateTime<Utc>) -> String {
    dt.format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

/// Parse a stored JSON string into a Value, or Null on absence/parse failure.
fn parse_json(s: &Option<String>) -> Value {
    match s {
        Some(raw) if !raw.is_empty() => serde_json::from_str(raw).unwrap_or(Value::Null),
        _ => Value::Null,
    }
}

fn note_json(n: &NoteRow) -> Value {
    json!({
        "id": n.id,
        "owner": n.owner,
        "title": n.title,
        "content": n.content,
        "items": parse_json(&n.items),
        "note_type": n.note_type,
        "color": n.color,
        "label": n.label,
        "pinned": n.pinned,
        "archived": n.archived,
        "due_date": n.due_date,
        "source": n.source,
        "session_id": n.session_id,
        "sort_order": n.sort_order,
        "image_url": n.image_url,
        "repeat": n.repeat,
        "ai_classification": parse_json(&n.ai_classification),
        "ai_content_hash": n.ai_content_hash,
        "agent_session_id": n.agent_session_id,
        "created_at": ts(&n.created_at),
        "updated_at": ts(&n.updated_at),
    })
}

const SELECT: &str = "SELECT id, owner, title, content, items, note_type, color, label, pinned, \
    archived, due_date, source, session_id, sort_order, image_url, repeat, ai_classification, \
    ai_content_hash, agent_session_id, created_at, updated_at FROM ws_notes";

async fn fetch_owned(state: &AppState, id: &str, owner: &str) -> Result<NoteRow, WsError> {
    let row = sqlx::query_as::<_, NoteRow>(&format!("{SELECT} WHERE id = $1"))
        .bind(id)
        .fetch_optional(&state.pg)
        .await?
        .ok_or_else(|| WsError::NotFound("note not found".into()))?;
    if row.owner.as_deref() != Some(owner) && row.owner.is_some() {
        return Err(WsError::NotFound("note not found".into()));
    }
    Ok(row)
}

// ---------------------------------------------------------------------------
// GET /api/notes
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct ListQuery {
    archived: Option<bool>,
    label: Option<String>,
}

pub async fn list(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(q): Query<ListQuery>,
) -> Result<Json<Value>, WsError> {
    let owner = owner_from(&headers);
    let archived = q.archived.unwrap_or(false);

    let mut sql = format!("{SELECT} WHERE owner = $1 AND archived = $2");
    if q.label.is_some() {
        sql.push_str(" AND label = $3");
    }
    sql.push_str(if archived {
        " ORDER BY updated_at DESC"
    } else {
        " ORDER BY pinned DESC, sort_order ASC, updated_at DESC"
    });

    let mut query = sqlx::query_as::<_, NoteRow>(&sql)
        .bind(&owner)
        .bind(archived);
    if let Some(label) = &q.label {
        query = query.bind(label);
    }
    let rows = query.fetch_all(&state.pg).await?;
    let notes: Vec<Value> = rows.iter().map(note_json).collect();
    Ok(Json(json!({ "notes": notes })))
}

// ---------------------------------------------------------------------------
// POST /api/notes
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct NoteCreate {
    #[serde(default)]
    title: String,
    content: Option<String>,
    items: Option<Value>,
    #[serde(default = "default_note_type")]
    note_type: String,
    color: Option<String>,
    label: Option<String>,
    #[serde(default)]
    pinned: bool,
    due_date: Option<String>,
    #[serde(default = "default_source")]
    source: String,
    session_id: Option<String>,
    image_url: Option<String>,
    #[serde(default = "default_repeat")]
    repeat: String,
    sort_order: Option<i32>,
}

fn default_note_type() -> String {
    "note".into()
}
fn default_source() -> String {
    "user".into()
}
fn default_repeat() -> String {
    "none".into()
}

pub async fn create(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(body): Json<NoteCreate>,
) -> Result<Json<Value>, WsError> {
    let owner = owner_from(&headers);
    let id = Uuid::new_v4().to_string();
    let items = body.items.map(|v| v.to_string());

    let row = sqlx::query_as::<_, NoteRow>(&format!(
        "INSERT INTO ws_notes \
         (id, owner, title, content, items, note_type, color, label, pinned, due_date, \
          source, session_id, image_url, repeat, sort_order) \
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15) \
         RETURNING id, owner, title, content, items, note_type, color, label, pinned, archived, \
         due_date, source, session_id, sort_order, image_url, repeat, ai_classification, \
         ai_content_hash, agent_session_id, created_at, updated_at"
    ))
    .bind(&id)
    .bind(&owner)
    .bind(&body.title)
    .bind(&body.content)
    .bind(&items)
    .bind(&body.note_type)
    .bind(&body.color)
    .bind(&body.label)
    .bind(body.pinned)
    .bind(&body.due_date)
    .bind(&body.source)
    .bind(&body.session_id)
    .bind(&body.image_url)
    .bind(&body.repeat)
    .bind(body.sort_order.unwrap_or(0))
    .fetch_one(&state.pg)
    .await?;

    Ok(Json(note_json(&row)))
}

// ---------------------------------------------------------------------------
// GET /api/notes/:id
// ---------------------------------------------------------------------------

pub async fn get_one(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Value>, WsError> {
    let owner = owner_from(&headers);
    let row = fetch_owned(&state, &id, &owner).await?;
    Ok(Json(note_json(&row)))
}

// ---------------------------------------------------------------------------
// PUT /api/notes/:id  (partial update via COALESCE)
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct NoteUpdate {
    title: Option<String>,
    content: Option<String>,
    items: Option<Value>,
    note_type: Option<String>,
    color: Option<String>,
    label: Option<String>,
    pinned: Option<bool>,
    archived: Option<bool>,
    due_date: Option<String>,
    image_url: Option<String>,
    repeat: Option<String>,
    sort_order: Option<i32>,
    agent_session_id: Option<String>,
}

pub async fn update(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<NoteUpdate>,
) -> Result<Json<Value>, WsError> {
    let owner = owner_from(&headers);
    fetch_owned(&state, &id, &owner).await?;

    let items = body.items.map(|v| v.to_string());
    let row = sqlx::query_as::<_, NoteRow>(&format!(
        "UPDATE ws_notes SET \
            title = COALESCE($2, title), \
            content = COALESCE($3, content), \
            items = COALESCE($4, items), \
            note_type = COALESCE($5, note_type), \
            color = COALESCE($6, color), \
            label = COALESCE($7, label), \
            pinned = COALESCE($8, pinned), \
            archived = COALESCE($9, archived), \
            due_date = COALESCE($10, due_date), \
            image_url = COALESCE($11, image_url), \
            repeat = COALESCE($12, repeat), \
            sort_order = COALESCE($13, sort_order), \
            agent_session_id = COALESCE($14, agent_session_id), \
            updated_at = now() \
         WHERE id = $1 \
         RETURNING id, owner, title, content, items, note_type, color, label, pinned, archived, \
         due_date, source, session_id, sort_order, image_url, repeat, ai_classification, \
         ai_content_hash, agent_session_id, created_at, updated_at"
    ))
    .bind(&id)
    .bind(&body.title)
    .bind(&body.content)
    .bind(&items)
    .bind(&body.note_type)
    .bind(&body.color)
    .bind(&body.label)
    .bind(body.pinned)
    .bind(body.archived)
    .bind(&body.due_date)
    .bind(&body.image_url)
    .bind(&body.repeat)
    .bind(body.sort_order)
    .bind(&body.agent_session_id)
    .fetch_one(&state.pg)
    .await?;

    Ok(Json(note_json(&row)))
}

// ---------------------------------------------------------------------------
// DELETE /api/notes/:id
// ---------------------------------------------------------------------------

pub async fn delete_one(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Value>, WsError> {
    let owner = owner_from(&headers);
    fetch_owned(&state, &id, &owner).await?;
    sqlx::query("DELETE FROM ws_notes WHERE id = $1")
        .bind(&id)
        .execute(&state.pg)
        .await?;
    Ok(Json(json!({ "ok": true })))
}

// ---------------------------------------------------------------------------
// POST /api/notes/:id/pin  &  /archive  (toggles)
// ---------------------------------------------------------------------------

pub async fn toggle_pin(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Value>, WsError> {
    let owner = owner_from(&headers);
    fetch_owned(&state, &id, &owner).await?;
    let pinned: bool =
        sqlx::query_scalar("UPDATE ws_notes SET pinned = NOT pinned, updated_at = now() WHERE id = $1 RETURNING pinned")
            .bind(&id)
            .fetch_one(&state.pg)
            .await?;
    Ok(Json(json!({ "ok": true, "pinned": pinned })))
}

pub async fn toggle_archive(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Value>, WsError> {
    let owner = owner_from(&headers);
    fetch_owned(&state, &id, &owner).await?;
    let archived: bool =
        sqlx::query_scalar("UPDATE ws_notes SET archived = NOT archived, updated_at = now() WHERE id = $1 RETURNING archived")
            .bind(&id)
            .fetch_one(&state.pg)
            .await?;
    Ok(Json(json!({ "ok": true, "archived": archived })))
}

// ---------------------------------------------------------------------------
// POST /api/notes/:id/items/:index/toggle
// ---------------------------------------------------------------------------

pub async fn toggle_item(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path((id, index)): Path<(String, usize)>,
) -> Result<Json<Value>, WsError> {
    let owner = owner_from(&headers);
    let row = fetch_owned(&state, &id, &owner).await?;

    let mut items = match parse_json(&row.items) {
        Value::Array(a) => a,
        _ => return Err(WsError::BadRequest("note has no checklist items".into())),
    };
    let item = items
        .get_mut(index)
        .ok_or_else(|| WsError::BadRequest("item index out of range".into()))?;
    let done = item.get("done").and_then(Value::as_bool).unwrap_or(false);
    item["done"] = json!(!done);

    let serialized = Value::Array(items.clone()).to_string();
    sqlx::query("UPDATE ws_notes SET items = $2, updated_at = now() WHERE id = $1")
        .bind(&id)
        .bind(&serialized)
        .execute(&state.pg)
        .await?;

    Ok(Json(json!({ "ok": true, "items": items })))
}

// ---------------------------------------------------------------------------
// POST /api/notes/reorder
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct ReorderBody {
    ids: Vec<String>,
}

pub async fn reorder(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(body): Json<ReorderBody>,
) -> Result<Json<Value>, WsError> {
    let owner = owner_from(&headers);
    let mut count = 0i64;
    let mut tx = state.pg.begin().await?;
    for (order, id) in body.ids.iter().enumerate() {
        let res = sqlx::query(
            "UPDATE ws_notes SET sort_order = $1, updated_at = now() WHERE id = $2 AND owner = $3",
        )
        .bind(order as i32)
        .bind(id)
        .bind(&owner)
        .execute(&mut *tx)
        .await?;
        count += res.rows_affected() as i64;
    }
    tx.commit().await?;
    Ok(Json(json!({ "ok": true, "count": count })))
}

// ---------------------------------------------------------------------------
// POST /api/notes/fire-reminder
//
// Reminder delivery (email / ntfy / browser-queue / optional LLM synthesis)
// belongs to the notifications subsystem, which has not been ported yet.
// The endpoint returns the documented response shape so the frontend's
// reminder scanner does not error; channels report not-sent for now.
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct FireReminder {
    #[allow(dead_code)]
    note_id: String,
}

pub async fn fire_reminder(
    State(_state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(_body): Json<FireReminder>,
) -> Result<Json<Value>, WsError> {
    let _owner = owner_from(&headers);
    Ok(Json(json!({
        "synthesis": null,
        "email_sent": false,
        "email_error": "",
        "ntfy_sent": false,
        "ntfy_error": "",
        "browser_sent": false
    })))
}
