/*
 * Broodlink workspace-api — Documents (multi-tab editor) + editor drafts.
 * Ported from the workspace app document_routes / editor_draft_routes.
 *
 * Implemented: document CRUD, version history (list/view/restore with 60s
 * user-edit coalescing), library (search/sort/filter/paginate), per-session
 * listing, and editor-draft CRUD.
 *
 * Stubbed (need a PDF lib / vision LLM): import-pdf, render-pages, page PNG,
 * render/export-pdf, ai-fill-annotations, extract-pdf-text, ai-tidy,
 * prepare-signed-reply, export-zip.
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

pub async fn ensure_documents_schema(pg: &sqlx::PgPool) -> Result<(), sqlx::Error> {
    sqlx::raw_sql(
        r#"
        CREATE TABLE IF NOT EXISTS ws_documents (
            id              TEXT PRIMARY KEY,
            owner           TEXT,
            session_id      TEXT,
            title           TEXT NOT NULL DEFAULT 'Untitled',
            language        TEXT,
            current_content TEXT NOT NULL DEFAULT '',
            version_count   INTEGER NOT NULL DEFAULT 1,
            is_active       BOOLEAN NOT NULL DEFAULT TRUE,
            archived        BOOLEAN NOT NULL DEFAULT FALSE,
            source_email_uid        TEXT,
            source_email_folder     TEXT,
            source_email_account_id TEXT,
            source_email_message_id TEXT,
            created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
            updated_at      TIMESTAMPTZ NOT NULL DEFAULT now()
        );
        CREATE INDEX IF NOT EXISTS ws_documents_owner_idx ON ws_documents(owner);
        CREATE INDEX IF NOT EXISTS ws_documents_session_idx ON ws_documents(session_id);

        CREATE TABLE IF NOT EXISTS ws_document_versions (
            id             TEXT PRIMARY KEY,
            document_id    TEXT NOT NULL REFERENCES ws_documents(id) ON DELETE CASCADE,
            version_number INTEGER NOT NULL,
            content        TEXT NOT NULL,
            summary        TEXT,
            source         TEXT NOT NULL DEFAULT 'ai',
            created_at     TIMESTAMPTZ NOT NULL DEFAULT now()
        );
        CREATE INDEX IF NOT EXISTS ws_doc_versions_doc_idx ON ws_document_versions(document_id);

        CREATE TABLE IF NOT EXISTS ws_editor_drafts (
            id              TEXT PRIMARY KEY,
            owner           TEXT,
            name            TEXT NOT NULL DEFAULT 'Untitled',
            source_image_id TEXT,
            width           INTEGER,
            height          INTEGER,
            payload         TEXT NOT NULL,
            thumbnail       TEXT,
            is_active       BOOLEAN NOT NULL DEFAULT TRUE,
            created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
            updated_at      TIMESTAMPTZ NOT NULL DEFAULT now()
        );
        CREATE INDEX IF NOT EXISTS ws_editor_drafts_owner_idx ON ws_editor_drafts(owner);
        "#,
    )
    .execute(pg)
    .await?;
    Ok(())
}

fn ts(dt: &DateTime<Utc>) -> String {
    dt.format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

#[derive(sqlx::FromRow)]
struct DocRow {
    id: String,
    owner: Option<String>,
    session_id: Option<String>,
    title: String,
    language: Option<String>,
    current_content: String,
    version_count: i32,
    is_active: bool,
    archived: bool,
    source_email_uid: Option<String>,
    source_email_folder: Option<String>,
    source_email_account_id: Option<String>,
    source_email_message_id: Option<String>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

fn doc_json(d: &DocRow) -> Value {
    json!({
        "id": d.id,
        "session_id": d.session_id,
        "title": d.title,
        "language": d.language,
        "current_content": d.current_content,
        "version_count": d.version_count,
        "is_active": d.is_active,
        "archived": d.archived,
        "created_at": ts(&d.created_at),
        "updated_at": ts(&d.updated_at),
        "source_email_uid": d.source_email_uid,
        "source_email_folder": d.source_email_folder,
        "source_email_account_id": d.source_email_account_id,
        "source_email_message_id": d.source_email_message_id,
    })
}

const DSELECT: &str = "SELECT id, owner, session_id, title, language, current_content, \
    version_count, is_active, archived, source_email_uid, source_email_folder, \
    source_email_account_id, source_email_message_id, created_at, updated_at FROM ws_documents";

async fn fetch_owned(state: &AppState, id: &str, owner: &str) -> Result<DocRow, WsError> {
    let row = sqlx::query_as::<_, DocRow>(&format!("{DSELECT} WHERE id = $1 AND is_active = TRUE"))
        .bind(id)
        .fetch_optional(&state.pg)
        .await?
        .ok_or_else(|| WsError::NotFound("document not found".into()))?;
    if row.owner.as_deref().map(|o| o != owner).unwrap_or(false) {
        return Err(WsError::NotFound("document not found".into()));
    }
    Ok(row)
}

// ---------------------------------------------------------------------------
// Document CRUD
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct DocCreate {
    session_id: Option<String>,
    #[serde(default = "default_title")]
    title: String,
    language: Option<String>,
    #[serde(default)]
    content: String,
}
fn default_title() -> String {
    "Untitled".into()
}

pub async fn create(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(body): Json<DocCreate>,
) -> Result<Json<Value>, WsError> {
    let owner = owner_from(&headers);
    let id = Uuid::new_v4().to_string();
    let row = sqlx::query_as::<_, DocRow>(&format!(
        "INSERT INTO ws_documents (id, owner, session_id, title, language, current_content) \
         VALUES ($1,$2,$3,$4,$5,$6) \
         RETURNING {cols}",
        cols = DSELECT
            .trim_start_matches("SELECT ")
            .split(" FROM")
            .next()
            .unwrap()
    ))
    .bind(&id)
    .bind(&owner)
    .bind(&body.session_id)
    .bind(&body.title)
    .bind(&body.language)
    .bind(&body.content)
    .fetch_one(&state.pg)
    .await?;

    // Initial version.
    sqlx::query(
        "INSERT INTO ws_document_versions (id, document_id, version_number, content, source) \
         VALUES ($1,$2,1,$3,'user')",
    )
    .bind(Uuid::new_v4().to_string())
    .bind(&id)
    .bind(&body.content)
    .execute(&state.pg)
    .await?;

    Ok(Json(doc_json(&row)))
}

pub async fn get_one(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Value>, WsError> {
    let owner = owner_from(&headers);
    Ok(Json(doc_json(&fetch_owned(&state, &id, &owner).await?)))
}

#[derive(Deserialize)]
pub struct DocUpdate {
    content: String,
    summary: Option<String>,
}

pub async fn update(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<DocUpdate>,
) -> Result<Json<Value>, WsError> {
    let owner = owner_from(&headers);
    let doc = fetch_owned(&state, &id, &owner).await?;

    // Coalesce within 60s of the last user-sourced version.
    let last = sqlx::query_as::<_, (String, String, DateTime<Utc>)>(
        "SELECT id, source, created_at FROM ws_document_versions \
         WHERE document_id = $1 ORDER BY version_number DESC LIMIT 1",
    )
    .bind(&id)
    .fetch_optional(&state.pg)
    .await?;

    let coalesce = matches!(&last, Some((_, src, at)) if src == "user" && (Utc::now() - *at).num_seconds() < 60);
    if let (true, Some((vid, _, _))) = (coalesce, &last) {
        sqlx::query("UPDATE ws_document_versions SET content = $2, summary = COALESCE($3, summary) WHERE id = $1")
            .bind(vid)
            .bind(&body.content)
            .bind(&body.summary)
            .execute(&state.pg)
            .await?;
    } else {
        sqlx::query(
            "INSERT INTO ws_document_versions (id, document_id, version_number, content, summary, source) \
             VALUES ($1,$2,$3,$4,$5,'user')",
        )
        .bind(Uuid::new_v4().to_string())
        .bind(&id)
        .bind(doc.version_count + 1)
        .bind(&body.content)
        .bind(&body.summary)
        .execute(&state.pg)
        .await?;
        sqlx::query("UPDATE ws_documents SET version_count = version_count + 1 WHERE id = $1")
            .bind(&id)
            .execute(&state.pg)
            .await?;
    }

    let row = sqlx::query_as::<_, DocRow>(&format!(
        "UPDATE ws_documents SET current_content = $2, updated_at = now() WHERE id = $1 RETURNING {cols}",
        cols = DSELECT.trim_start_matches("SELECT ").split(" FROM").next().unwrap()
    ))
    .bind(&id)
    .bind(&body.content)
    .fetch_one(&state.pg)
    .await?;
    Ok(Json(doc_json(&row)))
}

#[derive(Deserialize)]
pub struct DocPatch {
    title: Option<String>,
    language: Option<String>,
    session_id: Option<String>,
}

pub async fn patch(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<DocPatch>,
) -> Result<Json<Value>, WsError> {
    let owner = owner_from(&headers);
    fetch_owned(&state, &id, &owner).await?;
    // Empty-string session_id unlinks.
    let session_id = body
        .session_id
        .map(|s| if s.is_empty() { None } else { Some(s) });
    let row =
        sqlx::query_as::<_, DocRow>(&format!(
        "UPDATE ws_documents SET title = COALESCE($2, title), language = COALESCE($3, language), \
            session_id = CASE WHEN $4 THEN $5 ELSE session_id END, updated_at = now() \
         WHERE id = $1 RETURNING {cols}",
        cols = DSELECT.trim_start_matches("SELECT ").split(" FROM").next().unwrap()
    ))
        .bind(&id)
        .bind(&body.title)
        .bind(&body.language)
        .bind(session_id.is_some())
        .bind(session_id.flatten())
        .fetch_one(&state.pg)
        .await?;
    Ok(Json(doc_json(&row)))
}

pub async fn delete_one(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Value>, WsError> {
    let owner = owner_from(&headers);
    fetch_owned(&state, &id, &owner).await?;
    sqlx::query("UPDATE ws_documents SET is_active = FALSE WHERE id = $1")
        .bind(&id)
        .execute(&state.pg)
        .await?;
    Ok(Json(json!({ "status": "deleted", "id": id })))
}

#[derive(Deserialize)]
pub struct ArchiveQuery {
    #[serde(default = "default_true")]
    archived: bool,
}
fn default_true() -> bool {
    true
}

pub async fn archive(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(q): Query<ArchiveQuery>,
) -> Result<Json<Value>, WsError> {
    let owner = owner_from(&headers);
    fetch_owned(&state, &id, &owner).await?;
    sqlx::query("UPDATE ws_documents SET archived = $2, updated_at = now() WHERE id = $1")
        .bind(&id)
        .bind(q.archived)
        .execute(&state.pg)
        .await?;
    Ok(Json(
        json!({ "ok": true, "id": id, "archived": q.archived }),
    ))
}

// ---------------------------------------------------------------------------
// Versions
// ---------------------------------------------------------------------------

fn version_json(
    r: &(
        String,
        String,
        i32,
        String,
        Option<String>,
        String,
        DateTime<Utc>,
    ),
) -> Value {
    json!({
        "id": r.0, "document_id": r.1, "version_number": r.2, "content": r.3,
        "summary": r.4, "source": r.5, "created_at": ts(&r.6),
    })
}

const VSELECT: &str =
    "SELECT id, document_id, version_number, content, summary, source, created_at \
    FROM ws_document_versions";

pub async fn versions(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Value>, WsError> {
    let owner = owner_from(&headers);
    fetch_owned(&state, &id, &owner).await?;
    let rows = sqlx::query_as::<
        _,
        (
            String,
            String,
            i32,
            String,
            Option<String>,
            String,
            DateTime<Utc>,
        ),
    >(&format!(
        "{VSELECT} WHERE document_id = $1 ORDER BY version_number ASC"
    ))
    .bind(&id)
    .fetch_all(&state.pg)
    .await?;
    Ok(Json(json!(rows
        .iter()
        .map(version_json)
        .collect::<Vec<_>>())))
}

pub async fn version_at(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path((id, num)): Path<(String, i32)>,
) -> Result<Json<Value>, WsError> {
    let owner = owner_from(&headers);
    fetch_owned(&state, &id, &owner).await?;
    let row = sqlx::query_as::<
        _,
        (
            String,
            String,
            i32,
            String,
            Option<String>,
            String,
            DateTime<Utc>,
        ),
    >(&format!(
        "{VSELECT} WHERE document_id = $1 AND version_number = $2"
    ))
    .bind(&id)
    .bind(num)
    .fetch_optional(&state.pg)
    .await?
    .ok_or_else(|| WsError::NotFound("version not found".into()))?;
    Ok(Json(version_json(&row)))
}

pub async fn restore(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path((id, num)): Path<(String, i32)>,
) -> Result<Json<Value>, WsError> {
    let owner = owner_from(&headers);
    let doc = fetch_owned(&state, &id, &owner).await?;
    let content: String = sqlx::query_scalar(
        "SELECT content FROM ws_document_versions WHERE document_id = $1 AND version_number = $2",
    )
    .bind(&id)
    .bind(num)
    .fetch_optional(&state.pg)
    .await?
    .ok_or_else(|| WsError::NotFound("version not found".into()))?;

    sqlx::query(
        "INSERT INTO ws_document_versions (id, document_id, version_number, content, summary, source) \
         VALUES ($1,$2,$3,$4,$5,'user')",
    )
    .bind(Uuid::new_v4().to_string())
    .bind(&id)
    .bind(doc.version_count + 1)
    .bind(&content)
    .bind(format!("Restored v{num}"))
    .execute(&state.pg)
    .await?;

    let row = sqlx::query_as::<_, DocRow>(&format!(
        "UPDATE ws_documents SET current_content = $2, version_count = version_count + 1, \
            updated_at = now() WHERE id = $1 RETURNING {cols}",
        cols = DSELECT
            .trim_start_matches("SELECT ")
            .split(" FROM")
            .next()
            .unwrap()
    ))
    .bind(&id)
    .bind(&content)
    .fetch_one(&state.pg)
    .await?;
    Ok(Json(doc_json(&row)))
}

// ---------------------------------------------------------------------------
// Library + per-session
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct LibraryQuery {
    search: Option<String>,
    language: Option<String>,
    #[serde(default = "default_sort")]
    sort: String,
    #[serde(default)]
    offset: i64,
    #[serde(default = "default_limit")]
    limit: i64,
    #[serde(default)]
    archived: bool,
}
fn default_sort() -> String {
    "recent".into()
}
fn default_limit() -> i64 {
    20
}

pub async fn library(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(q): Query<LibraryQuery>,
) -> Result<Json<Value>, WsError> {
    let owner = owner_from(&headers);
    let order = match q.sort.as_str() {
        "oldest" => "created_at ASC",
        "edits" => "version_count DESC",
        "alpha" => "lower(title) ASC",
        _ => "updated_at DESC",
    };
    let mut sql = format!("{DSELECT} WHERE owner = $1 AND is_active = TRUE AND archived = $2");
    if q.search.is_some() {
        sql.push_str(" AND (title ILIKE $3 OR current_content ILIKE $3)");
    }
    if q.language.is_some() {
        sql.push_str(if q.search.is_some() {
            " AND language = $4"
        } else {
            " AND language = $3"
        });
    }
    sql.push_str(&format!(
        " ORDER BY {order} OFFSET {} LIMIT {}",
        q.offset.max(0),
        q.limit.clamp(1, 100)
    ));

    let mut query = sqlx::query_as::<_, DocRow>(&sql)
        .bind(&owner)
        .bind(q.archived);
    let pat = q
        .search
        .as_ref()
        .map(|s| format!("%{}%", s.replace('%', "\\%")));
    if let Some(p) = &pat {
        query = query.bind(p);
    }
    if let Some(lang) = &q.language {
        query = query.bind(lang);
    }
    let rows = query.fetch_all(&state.pg).await?;

    let documents: Vec<Value> = rows
        .iter()
        .map(|d| {
            json!({
                "id": d.id,
                "session_id": d.session_id,
                "session_name": Value::Null,
                "title": d.title,
                "language": d.language,
                "preview": d.current_content.chars().take(500).collect::<String>(),
                "version_count": d.version_count,
                "created_at": ts(&d.created_at),
                "updated_at": ts(&d.updated_at),
            })
        })
        .collect();

    let total: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM ws_documents WHERE owner = $1 AND is_active = TRUE AND archived = $2",
    )
    .bind(&owner)
    .bind(q.archived)
    .fetch_one(&state.pg)
    .await?;
    let langs = sqlx::query_as::<_, (Option<String>, i64)>(
        "SELECT language, COUNT(*) FROM ws_documents WHERE owner = $1 AND is_active = TRUE \
         GROUP BY language",
    )
    .bind(&owner)
    .fetch_all(&state.pg)
    .await?;
    let mut languages = serde_json::Map::new();
    for (l, c) in langs {
        if let Some(l) = l {
            languages.insert(l, json!(c));
        }
    }
    let session_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(DISTINCT session_id) FROM ws_documents WHERE owner = $1 AND is_active = TRUE AND session_id IS NOT NULL",
    )
    .bind(&owner)
    .fetch_one(&state.pg)
    .await?;

    Ok(Json(json!({
        "documents": documents, "total": total,
        "languages": Value::Object(languages), "session_count": session_count
    })))
}

pub async fn by_session(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(session_id): Path<String>,
) -> Result<Json<Value>, WsError> {
    let owner = owner_from(&headers);
    let rows = sqlx::query_as::<_, DocRow>(&format!(
        "{DSELECT} WHERE owner = $1 AND session_id = $2 AND is_active = TRUE ORDER BY updated_at DESC"
    ))
    .bind(&owner)
    .bind(&session_id)
    .fetch_all(&state.pg)
    .await?;
    Ok(Json(json!(rows.iter().map(doc_json).collect::<Vec<_>>())))
}

// ---------------------------------------------------------------------------
// Editor drafts
// ---------------------------------------------------------------------------

#[derive(sqlx::FromRow)]
struct DraftRow {
    id: String,
    name: String,
    source_image_id: Option<String>,
    width: Option<i32>,
    height: Option<i32>,
    payload: String,
    thumbnail: Option<String>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

fn draft_summary(d: &DraftRow) -> Value {
    json!({
        "id": d.id, "name": d.name, "source_image_id": d.source_image_id,
        "width": d.width, "height": d.height, "thumbnail": d.thumbnail,
        "created_at": ts(&d.created_at), "updated_at": ts(&d.updated_at),
    })
}

const DRSELECT: &str = "SELECT id, name, source_image_id, width, height, payload, thumbnail, created_at, updated_at FROM ws_editor_drafts";

pub async fn list_drafts(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Json<Value>, WsError> {
    let owner = owner_from(&headers);
    let rows = sqlx::query_as::<_, DraftRow>(&format!(
        "{DRSELECT} WHERE owner = $1 AND is_active = TRUE ORDER BY updated_at DESC"
    ))
    .bind(&owner)
    .fetch_all(&state.pg)
    .await?;
    Ok(Json(
        json!({ "drafts": rows.iter().map(draft_summary).collect::<Vec<_>>() }),
    ))
}

pub async fn get_draft(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Value>, WsError> {
    let owner = owner_from(&headers);
    let row = sqlx::query_as::<_, DraftRow>(&format!(
        "{DRSELECT} WHERE id = $1 AND owner = $2 AND is_active = TRUE"
    ))
    .bind(&id)
    .bind(&owner)
    .fetch_optional(&state.pg)
    .await?
    .ok_or_else(|| WsError::NotFound("draft not found".into()))?;
    let mut v = draft_summary(&row);
    v["payload"] = serde_json::from_str(&row.payload).unwrap_or(Value::Null);
    Ok(Json(v))
}

#[derive(Deserialize)]
pub struct DraftCreate {
    #[serde(default = "default_draft_name")]
    name: String,
    source_image_id: Option<String>,
    width: Option<i32>,
    height: Option<i32>,
    payload: Value,
    thumbnail: Option<String>,
}
fn default_draft_name() -> String {
    "Untitled".into()
}

pub async fn create_draft(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(body): Json<DraftCreate>,
) -> Result<Json<Value>, WsError> {
    let owner = owner_from(&headers);
    let id = Uuid::new_v4().to_string();
    let row = sqlx::query_as::<_, DraftRow>(&format!(
        "INSERT INTO ws_editor_drafts (id, owner, name, source_image_id, width, height, payload, thumbnail) \
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8) RETURNING {cols}",
        cols = DRSELECT.trim_start_matches("SELECT ").split(" FROM").next().unwrap()
    ))
    .bind(&id)
    .bind(&owner)
    .bind(&body.name)
    .bind(&body.source_image_id)
    .bind(body.width)
    .bind(body.height)
    .bind(body.payload.to_string())
    .bind(&body.thumbnail)
    .fetch_one(&state.pg)
    .await?;
    Ok(Json(draft_summary(&row)))
}

#[derive(Deserialize)]
pub struct DraftUpdate {
    name: Option<String>,
    width: Option<i32>,
    height: Option<i32>,
    payload: Option<Value>,
    thumbnail: Option<String>,
}

pub async fn update_draft(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<DraftUpdate>,
) -> Result<Json<Value>, WsError> {
    let owner = owner_from(&headers);
    let row = sqlx::query_as::<_, DraftRow>(&format!(
        "UPDATE ws_editor_drafts SET name = COALESCE($3, name), width = COALESCE($4, width), \
            height = COALESCE($5, height), payload = COALESCE($6, payload), \
            thumbnail = COALESCE($7, thumbnail), updated_at = now() \
         WHERE id = $1 AND owner = $2 AND is_active = TRUE RETURNING {cols}",
        cols = DRSELECT
            .trim_start_matches("SELECT ")
            .split(" FROM")
            .next()
            .unwrap()
    ))
    .bind(&id)
    .bind(&owner)
    .bind(&body.name)
    .bind(body.width)
    .bind(body.height)
    .bind(body.payload.map(|v| v.to_string()))
    .bind(&body.thumbnail)
    .fetch_optional(&state.pg)
    .await?
    .ok_or_else(|| WsError::NotFound("draft not found".into()))?;
    Ok(Json(draft_summary(&row)))
}

pub async fn delete_draft(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Value>, WsError> {
    let owner = owner_from(&headers);
    sqlx::query("UPDATE ws_editor_drafts SET is_active = FALSE WHERE id = $1 AND owner = $2")
        .bind(&id)
        .bind(&owner)
        .execute(&state.pg)
        .await?;
    Ok(Json(json!({ "status": "deleted", "id": id })))
}

// --- PDF / vision / zip: not yet ported -----------------------------------

pub async fn not_ported() -> Result<Json<Value>, WsError> {
    Err(WsError::BadRequest(
        "PDF / vision document features not yet ported".into(),
    ))
}
