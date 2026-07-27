/*
 * Broodlink workspace-api — Image gallery.
 * Ported from the workspace app gallery_routes/helpers. Implements the data
 * plane: upload (file saved under data/generated_images), library, albums,
 * tags, favorite, rename, patch, delete, stats, and image serving.
 *
 * Stubbed (need image-gen / vision / image libs): ai-tag(-batch), rotate,
 * download-zip, and all image processing (inpaint, upscale, remove-bg, …).
 */

use std::sync::Arc;

use axum::body::Body;
use axum::extract::{Multipart, Path, Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{owner_from, AppState, WsError};

const IMG_DIR: &str = "data/generated_images";

pub async fn ensure_gallery_schema(pg: &sqlx::PgPool) -> Result<(), sqlx::Error> {
    sqlx::raw_sql(
        r#"
        CREATE TABLE IF NOT EXISTS ws_gallery_albums (
            id          TEXT PRIMARY KEY,
            owner       TEXT,
            name        TEXT NOT NULL,
            description TEXT NOT NULL DEFAULT '',
            cover_id    TEXT,
            created_at  TIMESTAMPTZ NOT NULL DEFAULT now()
        );
        CREATE TABLE IF NOT EXISTS ws_gallery_images (
            id         TEXT PRIMARY KEY,
            owner      TEXT,
            filename   TEXT NOT NULL UNIQUE,
            prompt     TEXT NOT NULL DEFAULT '',
            model      TEXT,
            size       TEXT,
            quality    TEXT,
            tags       TEXT NOT NULL DEFAULT '',
            ai_tags    TEXT NOT NULL DEFAULT '',
            session_id TEXT,
            album_id   TEXT,
            is_active  BOOLEAN NOT NULL DEFAULT TRUE,
            favorite   BOOLEAN NOT NULL DEFAULT FALSE,
            file_hash  TEXT,
            width      INTEGER,
            height     INTEGER,
            file_size  INTEGER,
            created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
            updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
        );
        CREATE INDEX IF NOT EXISTS ws_gallery_images_owner_idx ON ws_gallery_images(owner);
        "#,
    )
    .execute(pg)
    .await?;
    Ok(())
}

#[derive(sqlx::FromRow)]
struct GImg {
    id: String,
    filename: String,
    prompt: String,
    model: Option<String>,
    size: Option<String>,
    quality: Option<String>,
    tags: String,
    ai_tags: String,
    session_id: Option<String>,
    album_id: Option<String>,
    is_active: bool,
    favorite: bool,
    width: Option<i32>,
    height: Option<i32>,
    file_size: Option<i32>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

fn img_json(i: &GImg) -> Value {
    json!({
        "id": i.id,
        "filename": i.filename,
        "url": format!("/api/generated-image/{}", i.filename),
        "prompt": i.prompt,
        "model": i.model,
        "size": i.size,
        "quality": i.quality,
        "tags": i.tags,
        "ai_tags": i.ai_tags,
        "user_tags": i.tags,
        "session_id": i.session_id,
        "session_name": Value::Null,
        "album_id": i.album_id,
        "is_active": i.is_active,
        "favorite": i.favorite,
        "taken_at": Value::Null,
        "camera": Value::Null,
        "gps": Value::Null,
        "width": i.width,
        "height": i.height,
        "file_size": i.file_size,
        "created_at": i.created_at.to_rfc3339(),
        "updated_at": i.updated_at.to_rfc3339(),
    })
}

const GSELECT: &str = "SELECT id, filename, prompt, model, size, quality, tags, ai_tags, session_id, \
    album_id, is_active, favorite, width, height, file_size, created_at, updated_at FROM ws_gallery_images";

async fn fetch_img(state: &AppState, id: &str, owner: &str) -> Result<GImg, WsError> {
    let row = sqlx::query_as::<_, GImg>(&format!("{GSELECT} WHERE id = $1 AND is_active = TRUE"))
        .bind(id)
        .fetch_optional(&state.pg)
        .await?
        .ok_or_else(|| WsError::NotFound("image not found".into()))?;
    let o: Option<String> = sqlx::query_scalar("SELECT owner FROM ws_gallery_images WHERE id = $1")
        .bind(id)
        .fetch_one(&state.pg)
        .await?;
    if o.as_deref().map(|x| x != owner).unwrap_or(false) {
        return Err(WsError::NotFound("image not found".into()));
    }
    Ok(row)
}

// ---------------------------------------------------------------------------
// Upload + serve
// ---------------------------------------------------------------------------

pub async fn upload(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    mut mp: Multipart,
) -> Result<Json<Value>, WsError> {
    let owner = owner_from(&headers);
    let mut bytes: Option<Vec<u8>> = None;
    let mut ext = "png".to_string();
    let mut album_id: Option<String> = None;
    while let Some(field) = mp
        .next_field()
        .await
        .map_err(|e| WsError::BadRequest(e.to_string()))?
    {
        match field.name().unwrap_or("") {
            "file" | "image" => {
                if let Some(fname) = field.file_name() {
                    if let Some(e) = fname.rsplit('.').next() {
                        if e.len() <= 5 {
                            ext = e.to_lowercase();
                        }
                    }
                }
                bytes = Some(
                    field
                        .bytes()
                        .await
                        .map_err(|e| WsError::BadRequest(e.to_string()))?
                        .to_vec(),
                );
            }
            "album_id" => album_id = field.text().await.ok().filter(|s| !s.is_empty()),
            _ => {}
        }
    }
    let data = bytes.ok_or_else(|| WsError::BadRequest("no file".into()))?;
    if data.is_empty() {
        return Err(WsError::BadRequest("empty file".into()));
    }
    let hash = hex::encode(Sha256::digest(&data));

    // Dedup per owner.
    if let Some((eid, efile)) = sqlx::query_as::<_, (String, String)>(
        "SELECT id, filename FROM ws_gallery_images WHERE owner = $1 AND file_hash = $2 AND is_active = TRUE",
    )
    .bind(&owner)
    .bind(&hash)
    .fetch_optional(&state.pg)
    .await?
    {
        return Ok(Json(json!({ "ok": false, "duplicate": true, "filename": efile, "id": eid, "message": "Duplicate photo skipped" })));
    }

    let id = Uuid::new_v4().to_string();
    let filename = format!(
        "{}.{}",
        Uuid::new_v4().simple().to_string()[..12].to_string(),
        ext
    );
    tokio::fs::create_dir_all(IMG_DIR)
        .await
        .map_err(|e| WsError::Internal(e.to_string()))?;
    tokio::fs::write(format!("{IMG_DIR}/{filename}"), &data)
        .await
        .map_err(|e| WsError::Internal(e.to_string()))?;

    sqlx::query(
        "INSERT INTO ws_gallery_images (id, owner, filename, model, album_id, file_hash, file_size) \
         VALUES ($1,$2,$3,'imported',$4,$5,$6)",
    )
    .bind(&id)
    .bind(&owner)
    .bind(&filename)
    .bind(&album_id)
    .bind(&hash)
    .bind(data.len() as i32)
    .execute(&state.pg)
    .await?;

    Ok(Json(json!({ "ok": true, "filename": filename, "id": id })))
}

/// Serve a stored image by filename (content-addressed, long cache).
pub async fn serve_image(Path(filename): Path<String>) -> Response {
    // Reject path traversal.
    if filename.contains('/') || filename.contains("..") {
        return (StatusCode::BAD_REQUEST, "bad filename").into_response();
    }
    match tokio::fs::read(format!("{IMG_DIR}/{filename}")).await {
        Ok(bytes) => {
            let ct = match filename.rsplit('.').next().unwrap_or("") {
                "jpg" | "jpeg" => "image/jpeg",
                "webp" => "image/webp",
                "gif" => "image/gif",
                "mp4" => "video/mp4",
                "webm" => "video/webm",
                _ => "image/png",
            };
            (
                [
                    (header::CONTENT_TYPE, ct.to_string()),
                    (
                        header::CACHE_CONTROL,
                        "public, max-age=31536000, immutable".to_string(),
                    ),
                ],
                Body::from(bytes),
            )
                .into_response()
        }
        Err(_) => (StatusCode::NOT_FOUND, "not found").into_response(),
    }
}

// ---------------------------------------------------------------------------
// Library / tags / stats
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct LibQuery {
    search: Option<String>,
    tag: Option<String>,
    model: Option<String>,
    album: Option<String>,
    #[serde(default)]
    favorites: bool,
    #[serde(default = "default_sort")]
    sort: String,
    #[serde(default)]
    offset: i64,
    #[serde(default = "default_limit")]
    limit: i64,
}
fn default_sort() -> String {
    "recent".into()
}
fn default_limit() -> i64 {
    24
}

pub async fn library(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(q): Query<LibQuery>,
) -> Result<Json<Value>, WsError> {
    let owner = owner_from(&headers);
    let order = match q.sort.as_str() {
        "oldest" => "created_at ASC",
        _ => "created_at DESC",
    };
    let mut sql = format!("{GSELECT} WHERE owner = $1 AND is_active = TRUE");
    let mut n = 1;
    let mut binds: Vec<String> = Vec::new();
    if let Some(s) = &q.search {
        n += 1;
        sql.push_str(&format!(
            " AND (prompt ILIKE ${n} OR tags ILIKE ${n} OR ai_tags ILIKE ${n})"
        ));
        binds.push(format!("%{}%", s.replace('%', "\\%")));
    }
    if let Some(t) = &q.tag {
        n += 1;
        sql.push_str(&format!(" AND (tags ILIKE ${n} OR ai_tags ILIKE ${n})"));
        binds.push(format!("%{t}%"));
    }
    if let Some(m) = &q.model {
        n += 1;
        sql.push_str(&format!(" AND model = ${n}"));
        binds.push(m.clone());
    }
    if let Some(a) = &q.album {
        n += 1;
        sql.push_str(&format!(" AND album_id = ${n}"));
        binds.push(a.clone());
    }
    if q.favorites {
        sql.push_str(" AND favorite = TRUE");
    }
    sql.push_str(&format!(
        " ORDER BY {order} OFFSET {} LIMIT {}",
        q.offset.max(0),
        q.limit.clamp(1, 100)
    ));

    let mut query = sqlx::query_as::<_, GImg>(&sql).bind(&owner);
    for b in &binds {
        query = query.bind(b);
    }
    let rows = query.fetch_all(&state.pg).await?;
    let items: Vec<Value> = rows.iter().map(img_json).collect();

    let total: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM ws_gallery_images WHERE owner = $1 AND is_active = TRUE",
    )
    .bind(&owner)
    .fetch_one(&state.pg)
    .await?;
    let models: Vec<String> = sqlx::query_scalar(
        "SELECT DISTINCT model FROM ws_gallery_images WHERE owner = $1 AND is_active = TRUE AND model IS NOT NULL",
    )
    .bind(&owner)
    .fetch_all(&state.pg)
    .await
    .unwrap_or_default();
    Ok(Json(
        json!({ "items": items, "total": total, "total_tagged": 0, "tags": collect_tags(&rows), "models": models }),
    ))
}

fn collect_tags(rows: &[GImg]) -> Vec<String> {
    let mut set = std::collections::BTreeSet::new();
    for r in rows {
        for t in r.tags.split(',').chain(r.ai_tags.split(',')) {
            let t = t.trim();
            if !t.is_empty() {
                set.insert(t.to_string());
            }
        }
    }
    set.into_iter().collect()
}

pub async fn tags(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Json<Value>, WsError> {
    let owner = owner_from(&headers);
    let rows =
        sqlx::query_as::<_, GImg>(&format!("{GSELECT} WHERE owner = $1 AND is_active = TRUE"))
            .bind(&owner)
            .fetch_all(&state.pg)
            .await?;
    Ok(Json(json!({ "tags": collect_tags(&rows) })))
}

pub async fn stats(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Json<Value>, WsError> {
    let owner = owner_from(&headers);
    let (count, size): (i64, Option<i64>) = sqlx::query_as(
        "SELECT COUNT(*), COALESCE(SUM(file_size),0) FROM ws_gallery_images WHERE owner = $1 AND is_active = TRUE",
    )
    .bind(&owner)
    .fetch_one(&state.pg)
    .await?;
    let favorites: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM ws_gallery_images WHERE owner = $1 AND is_active = TRUE AND favorite = TRUE",
    )
    .bind(&owner)
    .fetch_one(&state.pg)
    .await?;
    let albums: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM ws_gallery_albums WHERE owner = $1")
        .bind(&owner)
        .fetch_one(&state.pg)
        .await?;
    let total_size = size.unwrap_or(0);
    Ok(Json(json!({
        "total_photos": count, "total_size": total_size,
        "total_size_human": format!("{:.1} MB", total_size as f64 / 1_048_576.0),
        "favorites": favorites, "albums": albums
    })))
}

// ---------------------------------------------------------------------------
// Single-image mutations
// ---------------------------------------------------------------------------

pub async fn detail(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Value>, WsError> {
    let owner = owner_from(&headers);
    Ok(Json(img_json(&fetch_img(&state, &id, &owner).await?)))
}

#[derive(Deserialize)]
pub struct PatchBody {
    tags: Option<String>,
    favorite: Option<bool>,
    album_id: Option<String>,
}

pub async fn patch(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(b): Json<PatchBody>,
) -> Result<Json<Value>, WsError> {
    let owner = owner_from(&headers);
    fetch_img(&state, &id, &owner).await?;
    sqlx::query(
        "UPDATE ws_gallery_images SET tags = COALESCE($2, tags), favorite = COALESCE($3, favorite), \
            album_id = COALESCE($4, album_id), updated_at = now() WHERE id = $1",
    )
    .bind(&id)
    .bind(&b.tags)
    .bind(b.favorite)
    .bind(&b.album_id)
    .execute(&state.pg)
    .await?;
    Ok(Json(img_json(&fetch_img(&state, &id, &owner).await?)))
}

#[derive(Deserialize)]
pub struct RenameBody {
    #[serde(default)]
    name: String,
}

pub async fn rename(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(b): Json<RenameBody>,
) -> Result<Json<Value>, WsError> {
    let owner = owner_from(&headers);
    fetch_img(&state, &id, &owner).await?;
    sqlx::query("UPDATE ws_gallery_images SET prompt = $2, updated_at = now() WHERE id = $1")
        .bind(&id)
        .bind(&b.name)
        .execute(&state.pg)
        .await?;
    Ok(Json(json!({ "ok": true, "name": b.name })))
}

pub async fn favorite(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Value>, WsError> {
    let owner = owner_from(&headers);
    fetch_img(&state, &id, &owner).await?;
    let fav: bool = sqlx::query_scalar(
        "UPDATE ws_gallery_images SET favorite = NOT favorite, updated_at = now() WHERE id = $1 RETURNING favorite",
    )
    .bind(&id)
    .fetch_one(&state.pg)
    .await?;
    Ok(Json(json!({ "ok": true, "favorite": fav })))
}

pub async fn delete_one(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Value>, WsError> {
    let owner = owner_from(&headers);
    let img = fetch_img(&state, &id, &owner).await?;
    let _ = tokio::fs::remove_file(format!("{IMG_DIR}/{}", img.filename)).await;
    sqlx::query("UPDATE ws_gallery_images SET is_active = FALSE WHERE id = $1")
        .bind(&id)
        .execute(&state.pg)
        .await?;
    Ok(Json(json!({ "status": "deleted", "id": id })))
}

// ---------------------------------------------------------------------------
// Albums
// ---------------------------------------------------------------------------

pub async fn list_albums(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Json<Value>, WsError> {
    let owner = owner_from(&headers);
    let rows = sqlx::query_as::<_, (String, String, String, Option<String>, DateTime<Utc>)>(
        "SELECT id, name, description, cover_id, created_at FROM ws_gallery_albums WHERE owner = $1 ORDER BY created_at DESC",
    )
    .bind(&owner)
    .fetch_all(&state.pg)
    .await?;
    let mut albums = Vec::new();
    for (id, name, description, cover_id, created) in rows {
        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM ws_gallery_images WHERE album_id = $1 AND is_active = TRUE",
        )
        .bind(&id)
        .fetch_one(&state.pg)
        .await
        .unwrap_or(0);
        let cover_url = cover_id.map(|c| format!("/api/generated-image/{c}"));
        albums.push(json!({
            "id": id, "name": name, "description": description,
            "cover_url": cover_url, "count": count, "created_at": created.to_rfc3339()
        }));
    }
    Ok(Json(json!({ "albums": albums })))
}

#[derive(Deserialize)]
pub struct AlbumBody {
    #[serde(default)]
    name: String,
    #[serde(default)]
    description: String,
}

pub async fn create_album(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(b): Json<AlbumBody>,
) -> Result<Json<Value>, WsError> {
    let owner = owner_from(&headers);
    let id = Uuid::new_v4().to_string();
    sqlx::query(
        "INSERT INTO ws_gallery_albums (id, owner, name, description) VALUES ($1,$2,$3,$4)",
    )
    .bind(&id)
    .bind(&owner)
    .bind(&b.name)
    .bind(&b.description)
    .execute(&state.pg)
    .await?;
    Ok(Json(json!({ "ok": true, "id": id, "name": b.name })))
}

pub async fn update_album(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(b): Json<AlbumBody>,
) -> Result<Json<Value>, WsError> {
    let owner = owner_from(&headers);
    sqlx::query(
        "UPDATE ws_gallery_albums SET name = $3, description = $4 WHERE id = $1 AND owner = $2",
    )
    .bind(&id)
    .bind(&owner)
    .bind(&b.name)
    .bind(&b.description)
    .execute(&state.pg)
    .await?;
    Ok(Json(json!({ "ok": true })))
}

pub async fn delete_album(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Value>, WsError> {
    let owner = owner_from(&headers);
    sqlx::query("DELETE FROM ws_gallery_albums WHERE id = $1 AND owner = $2")
        .bind(&id)
        .bind(&owner)
        .execute(&state.pg)
        .await?;
    Ok(Json(json!({ "status": "deleted" })))
}

#[derive(Deserialize)]
pub struct AlbumImage {
    image_id: String,
}

pub async fn album_add(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(album_id): Path<String>,
    Json(b): Json<AlbumImage>,
) -> Result<Json<Value>, WsError> {
    let owner = owner_from(&headers);
    sqlx::query("UPDATE ws_gallery_images SET album_id = $1 WHERE id = $2 AND owner = $3")
        .bind(&album_id)
        .bind(&b.image_id)
        .bind(&owner)
        .execute(&state.pg)
        .await?;
    Ok(Json(json!({ "ok": true })))
}

pub async fn album_remove(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(_album_id): Path<String>,
    Json(b): Json<AlbumImage>,
) -> Result<Json<Value>, WsError> {
    let owner = owner_from(&headers);
    sqlx::query("UPDATE ws_gallery_images SET album_id = NULL WHERE id = $1 AND owner = $2")
        .bind(&b.image_id)
        .bind(&owner)
        .execute(&state.pg)
        .await?;
    Ok(Json(json!({ "ok": true })))
}

// --- image generation / vision: not yet ported ----------------------------

pub async fn not_ported() -> Result<Json<Value>, WsError> {
    Err(WsError::BadRequest(
        "image generation / vision not yet ported".into(),
    ))
}
