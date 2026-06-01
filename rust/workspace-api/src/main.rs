/*
 * Broodlink - Multi-agent AI orchestration system
 * Copyright (C) 2025–2026 Neven Kordic <neven@broodlink.ai>
 * SPDX-License-Identifier: AGPL-3.0-or-later
 *
 * workspace-api: the user-facing Broodlink web application (notes, tasks,
 * calendar, email, documents) — the interface absorbed from the workspace app, now
 * served from Rust on top of Broodlink's Postgres + service stack.
 */

#![allow(clippy::module_name_repetitions)]

#[cfg(test)]
mod boottest;

mod auth;
mod calendar;
mod chat;
mod compare;
mod contacts;
mod crypto;
mod documents;
mod email;
mod email_net;
mod embeddings;
mod gallery;
mod hwfit;
mod mcp;
mod memory;
mod notes;
mod presets;
mod research;
mod search;
mod signatures;
mod skills;
mod speech;
mod tasks;
mod vault;
mod webhooks;

use std::collections::HashMap;
use std::net::SocketAddr;
use std::process;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::RwLock;

use axum::extract::State;
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{delete, get, post, put};
use axum::{Json, Router};
use broodlink_config::Config;
use broodlink_secrets::SecretsProvider;
use rand::Rng;
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;
use tower_http::cors::{AllowOrigin, CorsLayer};
use tower_http::services::ServeDir;
use tracing::{error, info};

const SERVICE_NAME: &str = "workspace-api";
const SERVICE_VERSION: &str = env!("CARGO_PKG_VERSION");

// ---------------------------------------------------------------------------
// Shared state
// ---------------------------------------------------------------------------

pub struct AppState {
    pub pg: PgPool,
    pub config: Arc<Config>,
    pub ui_dir: String,
    /// Failed-login throttle: username -> (count, window start).
    pub login_attempts: RwLock<HashMap<String, (u32, Instant)>>,
    /// Encrypts stored credentials (email/CalDAV passwords) at rest.
    pub cipher: crypto::Cipher,
}

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

#[derive(thiserror::Error, Debug)]
pub enum WsError {
    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),
    #[error("not found: {0}")]
    NotFound(String),
    #[error("bad request: {0}")]
    BadRequest(String),
    #[error("unauthorized: {0}")]
    Unauthorized(String),
    #[error("forbidden: {0}")]
    Forbidden(String),
    #[error("internal error: {0}")]
    Internal(String),
}

impl IntoResponse for WsError {
    fn into_response(self) -> Response {
        let (status, msg) = match &self {
            Self::Database(e) => {
                error!(error = %e, "database error");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal database error".to_string(),
                )
            }
            Self::NotFound(m) => (StatusCode::NOT_FOUND, m.clone()),
            Self::BadRequest(m) => (StatusCode::BAD_REQUEST, m.clone()),
            Self::Unauthorized(m) => (StatusCode::UNAUTHORIZED, m.clone()),
            Self::Forbidden(m) => (StatusCode::FORBIDDEN, m.clone()),
            Self::Internal(m) => {
                error!(error = %m, "internal error");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal error".to_string(),
                )
            }
        };
        (status, Json(serde_json::json!({ "detail": msg }))).into_response()
    }
}

// ---------------------------------------------------------------------------
// Owner resolution
//
// Full session auth is still owned by the (Python) auth subsystem and has not
// been ported yet. Until it is, the owner is read from the `bl_owner` cookie
// or the `X-Owner` header, defaulting to "default". This keeps the per-owner
// data model intact so the auth port is a drop-in later.
// ---------------------------------------------------------------------------

pub fn owner_from(headers: &HeaderMap) -> String {
    // The auth middleware stamps `x-owner` with the authenticated username for
    // every protected request, so this is the trusted source of truth.
    if let Some(val) = headers.get("x-owner").and_then(|v| v.to_str().ok()) {
        if !val.is_empty() {
            return val.to_string();
        }
    }
    "default".to_string()
}

// ---------------------------------------------------------------------------
// Static SPA serving (mirrors the workspace app: static files + nonce-injected HTML)
// ---------------------------------------------------------------------------

const SPA_ROUTES: &[&str] = &[
    "/notes",
    "/calendar",
    "/cookbook",
    "/email",
    "/memory",
    "/gallery",
    "/tasks",
    "/library",
];

async fn serve_html(ui_dir: &str, file: &str) -> Response {
    let path = format!("{ui_dir}/{file}");
    match tokio::fs::read_to_string(&path).await {
        Ok(raw) => {
            // Per-request CSP nonce for inline <script> tags.
            let nonce: String = {
                let mut rng = rand::thread_rng();
                (0..16)
                    .map(|_| format!("{:02x}", rng.gen::<u8>()))
                    .collect()
            };
            let html = raw.replace("{{CSP_NONCE}}", &nonce);
            let csp = format!(
                "default-src 'self'; script-src 'self' 'nonce-{nonce}' https://cdn.jsdelivr.net; \
                 style-src 'self' 'unsafe-inline' https://cdn.jsdelivr.net; \
                 font-src 'self' data:; img-src 'self' data: blob:; connect-src 'self'"
            );
            (
                [
                    (header::CONTENT_SECURITY_POLICY, csp),
                    (header::CACHE_CONTROL, "no-cache".to_string()),
                ],
                Html(html),
            )
                .into_response()
        }
        Err(_) => (StatusCode::NOT_FOUND, "UI asset not found").into_response(),
    }
}

async fn serve_index(State(state): State<Arc<AppState>>) -> Response {
    serve_html(&state.ui_dir, "index.html").await
}

async fn serve_login(State(state): State<Arc<AppState>>) -> Response {
    serve_html(&state.ui_dir, "login.html").await
}

// ---------------------------------------------------------------------------
// Router
// ---------------------------------------------------------------------------

fn build_router(state: Arc<AppState>) -> Router {
    let cors = if state.config.workspace_api.cors_origins.is_empty() {
        CorsLayer::new()
    } else {
        let origins: Vec<_> = state
            .config
            .workspace_api
            .cors_origins
            .iter()
            .filter_map(|o| o.parse().ok())
            .collect();
        CorsLayer::new().allow_origin(AllowOrigin::list(origins))
    };

    let api = Router::new()
        // --- auth ---
        .route("/auth/setup", post(auth::setup))
        .route("/auth/login", post(auth::login))
        .route("/auth/logout", post(auth::logout))
        .route("/auth/status", get(auth::status))
        .route("/auth/signup", post(auth::signup))
        .route("/auth/2fa/status", get(auth::twofa_status))
        .route("/auth/2fa/setup", post(auth::twofa_unavailable))
        .route("/auth/2fa/confirm", post(auth::twofa_unavailable))
        .route("/auth/2fa/disable", post(auth::twofa_unavailable))
        // --- notes ---
        .route("/notes", get(notes::list).post(notes::create))
        .route("/notes/reorder", post(notes::reorder))
        .route("/notes/fire-reminder", post(notes::fire_reminder))
        .route(
            "/notes/:id",
            get(notes::get_one)
                .put(notes::update)
                .delete(notes::delete_one),
        )
        .route("/notes/:id/pin", post(notes::toggle_pin))
        .route("/notes/:id/archive", post(notes::toggle_archive))
        .route("/notes/:id/items/:index/toggle", post(notes::toggle_item))
        // --- tasks ---
        .route("/tasks", get(tasks::list).post(tasks::create))
        .route("/tasks/notifications", get(tasks::notifications))
        .route(
            "/tasks/onboarding",
            get(tasks::get_onboarding).post(tasks::set_onboarding),
        )
        .route("/tasks/runs/recent", get(tasks::recent_runs))
        .route(
            "/tasks/meta/output-targets",
            get(tasks::meta_output_targets),
        )
        .route("/tasks/meta/actions", get(tasks::meta_actions))
        .route("/tasks/meta/events", get(tasks::meta_events))
        .route(
            "/tasks/:id",
            get(tasks::get_one)
                .put(tasks::update)
                .delete(tasks::delete_one),
        )
        .route("/tasks/:id/pause", post(tasks::pause))
        .route("/tasks/:id/resume", post(tasks::resume))
        .route("/tasks/:id/run", post(tasks::run_now))
        .route("/tasks/:id/runs", get(tasks::task_runs))
        // --- calendar ---
        .route(
            "/calendar/config",
            get(calendar::get_config).post(calendar::set_config),
        )
        .route("/calendar/test", post(calendar::test_config))
        .route("/calendar/sync", post(calendar::sync))
        .route(
            "/calendar/calendars",
            get(calendar::list_calendars).post(calendar::create_calendar),
        )
        .route(
            "/calendar/calendars/:id",
            put(calendar::update_calendar).delete(calendar::delete_calendar),
        )
        .route(
            "/calendar/events",
            get(calendar::list_events).post(calendar::create_event),
        )
        .route(
            "/calendar/events/:uid",
            put(calendar::update_event).delete(calendar::delete_event),
        )
        .route("/calendar/import", post(calendar::import_ics))
        .route("/calendar/export/:id", get(calendar::export_calendar))
        .route("/calendar/quick-parse", post(calendar::quick_parse))
        // --- email: config / accounts / settings (real) ---
        .route(
            "/email/config",
            get(email::get_config).put(email::put_config),
        )
        .route(
            "/email/accounts",
            get(email::list_accounts).post(email::create_account),
        )
        .route(
            "/email/accounts/:id",
            put(email::update_account).delete(email::delete_account),
        )
        .route("/email/accounts/test", post(email::test_accounts))
        .route(
            "/email/accounts/:id/set-default",
            post(email::set_default_account),
        )
        .route("/email/style", get(email::get_style).put(email::put_style))
        // --- email: scheduled send (queue real; dispatch phase 2) ---
        .route("/email/scheduled", get(email::list_scheduled))
        .route("/email/schedule", post(email::schedule))
        .route("/email/scheduled/:id", delete(email::delete_scheduled))
        // --- email: IMAP/SMTP/LLM (phase 2 stubs) ---
        .route("/email/list", get(email::list_mail))
        .route("/email/folders", get(email::folders))
        .route("/email/search", get(email::search))
        .route("/email/contacts", get(email::contacts))
        .route("/email/resolve-contact", get(email::contacts))
        .route("/email/read/:uid", get(email::read_mail))
        .route("/email/mark-read/:uid", post(email::mark_read))
        .route("/email/mark-unread/:uid", post(email::mark_unread))
        .route("/email/mark-answered/:uid", post(email::mark_answered))
        .route("/email/clear-answered/:uid", post(email::clear_answered))
        .route("/email/archive/:uid", post(email::archive))
        .route("/email/move/:uid", post(email::move_mail))
        .route("/email/delete/:uid", delete(email::delete_mail))
        .route(
            "/email/delete-permanent/:uid",
            delete(email::delete_permanent),
        )
        .route("/email/send", post(email::send))
        .route("/email/draft", post(email::draft))
        .route("/email/urgency-state", get(email::urgency_state))
        .route("/email/summarize", post(email::llm_action))
        .route("/email/ai-reply", post(email::llm_action))
        .route("/email/extract-style", post(email::llm_action))
        // --- chat / sessions / models ---
        .route("/chat_stream", post(chat::chat_stream))
        .route("/sessions", get(chat::list_sessions))
        .route("/session", post(chat::create_session))
        .route(
            "/session/:id",
            axum::routing::patch(chat::patch_session).delete(chat::delete_session),
        )
        .route("/history/:id", get(chat::history))
        .route("/models", get(chat::list_models))
        .route("/default-chat", get(chat::default_chat))
        .route(
            "/model-endpoints",
            get(chat::list_endpoints).post(chat::add_endpoint),
        )
        .route("/model-endpoints/:id", delete(chat::delete_endpoint))
        // --- memory (converged onto Broodlink Qdrant via MCP mirror) ---
        .route("/memory", get(memory::list))
        .route("/memory/add", post(memory::add))
        .route("/memory/search", post(memory::search))
        .route("/memory/extract", post(memory::extract))
        .route("/memory/audit", post(memory::audit))
        .route("/memory/timeline", get(memory::timeline))
        .route("/memory/by-session/:sid", get(memory::by_session))
        .route(
            "/memory/:id",
            get(memory::get_one)
                .put(memory::update)
                .delete(memory::delete_one),
        )
        .route("/memory/:id/pin", post(memory::pin))
        // --- documents + editor drafts ---
        .route("/document", post(documents::create))
        .route("/documents/library", get(documents::library))
        .route("/documents/export-zip", post(documents::not_ported))
        .route("/documents/import-pdf", post(documents::not_ported))
        .route("/documents/tidy", post(documents::not_ported))
        .route("/documents/ai-tidy", post(documents::not_ported))
        .route("/documents/:session_id", get(documents::by_session))
        .route(
            "/document/:id",
            get(documents::get_one)
                .put(documents::update)
                .patch(documents::patch)
                .delete(documents::delete_one),
        )
        .route("/document/:id/archive", post(documents::archive))
        .route("/document/:id/versions", get(documents::versions))
        .route("/document/:id/version/:num", get(documents::version_at))
        .route("/document/:id/restore/:num", post(documents::restore))
        .route(
            "/editor-drafts",
            get(documents::list_drafts).post(documents::create_draft),
        )
        .route(
            "/editor-drafts/:id",
            get(documents::get_draft)
                .put(documents::update_draft)
                .delete(documents::delete_draft),
        )
        // --- presets ---
        .route("/presets", get(presets::list))
        .route("/presets/custom", post(presets::set_custom))
        .route(
            "/presets/templates",
            get(presets::list_templates).post(presets::upsert_template),
        )
        .route("/presets/templates/:id", delete(presets::delete_template))
        .route(
            "/presets/groups",
            get(presets::get_groups).post(presets::set_groups),
        )
        .route("/presets/expand", post(presets::expand))
        // --- skills ---
        .route("/skills", get(skills::list))
        .route("/skills/index", get(skills::index))
        .route("/skills/add", post(skills::add))
        .route("/skills/search", post(skills::search))
        .route("/skills/builtin", get(skills::builtin_list))
        .route(
            "/skills/builtin/:name",
            get(skills::builtin_get)
                .put(skills::builtin_set)
                .delete(skills::builtin_delete),
        )
        .route(
            "/skills/:id",
            get(skills::get_one)
                .put(skills::update)
                .delete(skills::delete_one),
        )
        .route(
            "/skills/:id/markdown",
            get(skills::get_markdown).post(skills::set_markdown),
        )
        // --- compare ---
        .route("/compare/start", post(compare::start))
        .route("/compare/record", post(compare::record))
        .route("/compare/history", get(compare::history))
        .route("/compare/:id/vote", post(compare::vote))
        .route("/compare/:id", delete(compare::delete_one))
        // --- web search ---
        .route("/search", post(search::search))
        .route("/search/config", get(search::config))
        .route("/search/providers", get(search::providers))
        .route("/search/query", post(search::query))
        // --- contacts ---
        .route("/contacts/list", get(contacts::list))
        .route("/contacts/search", get(contacts::search))
        .route("/contacts/add", post(contacts::add))
        .route("/contacts/import", post(contacts::import))
        .route("/contacts/export", get(contacts::export))
        .route("/contacts/clear", delete(contacts::clear))
        .route(
            "/contacts/config",
            get(contacts::get_config).put(contacts::set_config),
        )
        .route(
            "/contacts/:uid",
            put(contacts::update).delete(contacts::delete_one),
        )
        // --- signatures ---
        .route(
            "/signatures",
            get(signatures::list).post(signatures::create),
        )
        .route("/signatures/:id", delete(signatures::delete_one))
        // --- gallery ---
        .route("/generated-image/:filename", get(gallery::serve_image))
        .route("/gallery/upload", post(gallery::upload))
        .route("/gallery/library", get(gallery::library))
        .route("/gallery/tags", get(gallery::tags))
        .route("/gallery/stats", get(gallery::stats))
        .route("/gallery/download-zip", post(gallery::not_ported))
        .route(
            "/gallery/albums",
            get(gallery::list_albums).post(gallery::create_album),
        )
        .route(
            "/gallery/albums/:id",
            put(gallery::update_album).delete(gallery::delete_album),
        )
        .route("/gallery/albums/:id/add", post(gallery::album_add))
        .route("/gallery/albums/:id/remove", post(gallery::album_remove))
        .route(
            "/gallery/:id",
            get(gallery::detail)
                .patch(gallery::patch)
                .delete(gallery::delete_one),
        )
        .route("/gallery/:id/favorite", post(gallery::favorite))
        .route("/gallery/:id/rename", post(gallery::rename))
        .route("/gallery/:id/rotate", post(gallery::not_ported))
        .route("/gallery/:id/replace", post(gallery::not_ported))
        .route("/gallery/:id/ai-tag", post(gallery::not_ported))
        .route("/image/inpaint", post(gallery::not_ported))
        .route("/image/upscale-local", post(gallery::not_ported))
        .route("/image/remove-bg", post(gallery::not_ported))
        // --- webhooks ---
        .route("/webhooks", get(webhooks::list).post(webhooks::create))
        .route("/webhooks/:id/test", post(webhooks::test))
        .route(
            "/webhooks/:id",
            delete(webhooks::delete_one).patch(webhooks::toggle),
        )
        // --- vault ---
        .route(
            "/vault/config",
            get(vault::get_config).post(vault::set_config),
        )
        .route("/vault/login", post(vault::cli_unavailable))
        .route("/vault/unlock", post(vault::cli_unavailable))
        .route("/vault/lock", post(vault::lock))
        .route("/vault/logout", post(vault::lock))
        // --- hwfit / cookbook (native hardware scan + fit scoring) ---
        .route("/hwfit/system", get(hwfit::system_endpoint))
        .route("/hwfit/models", get(hwfit::models))
        .route("/hwfit/image-models", get(hwfit::image_models))
        // --- deep research (data plane; LLM loop deferred) ---
        .route("/research/start", post(research::start))
        .route("/research/library", get(research::library))
        .route("/research/status/:id", get(research::status))
        .route("/research/detail/:id", get(research::detail))
        .route("/research/result/:id", post(research::result))
        .route("/research/result-peek/:id", post(research::result))
        .route("/research/report/:id", get(research::report))
        .route("/research/stream/:id", get(research::stream))
        .route("/research/cancel/:id", post(research::cancel))
        .route("/research/:id/archive", post(research::archive))
        .route("/research/:id", delete(research::delete_one))
        // --- speech (stt/tts: stats + cache; engines deferred) ---
        .route("/stt/stats", get(speech::stt_stats))
        .route("/stt/transcribe", post(speech::stt_transcribe))
        .route("/tts/stats", get(speech::tts_stats))
        .route("/tts/synthesize", post(speech::tts_synthesize))
        .route("/tts/clear-cache", post(speech::tts_clear_cache))
        // --- embeddings (catalog + endpoint config; local compute deferred) ---
        .route("/embeddings/models", get(embeddings::models))
        .route(
            "/embeddings/models/:model/download",
            post(embeddings::download),
        )
        .route(
            "/embeddings/models/:model/status",
            get(embeddings::model_status),
        )
        .route(
            "/embeddings/models/:model",
            delete(embeddings::delete_model),
        )
        .route(
            "/embeddings/endpoint",
            get(embeddings::get_endpoint)
                .post(embeddings::set_endpoint)
                .delete(embeddings::clear_endpoint),
        );

    Router::new()
        .route("/", get(serve_index))
        .route("/login", get(serve_login))
        .nest("/api", api)
        .nest_service("/static", ServeDir::new(&state.ui_dir))
        // SPA deep links all resolve to index.html
        .route(SPA_ROUTES[0], get(serve_index))
        .route(SPA_ROUTES[1], get(serve_index))
        .route(SPA_ROUTES[2], get(serve_index))
        .route(SPA_ROUTES[3], get(serve_index))
        .route(SPA_ROUTES[4], get(serve_index))
        .route(SPA_ROUTES[5], get(serve_index))
        .route(SPA_ROUTES[6], get(serve_index))
        .route(SPA_ROUTES[7], get(serve_index))
        // root-level PWA assets (manifest, service worker, favicon, …)
        .fallback_service(ServeDir::new(&state.ui_dir))
        .layer(axum::middleware::from_fn_with_state(
            Arc::clone(&state),
            auth::auth_mw,
        ))
        .layer(cors)
        .with_state(state)
}

// ---------------------------------------------------------------------------
// Bootstrap
// ---------------------------------------------------------------------------

async fn init_state() -> Result<AppState, WsError> {
    let config = Config::load().map_err(|e| WsError::Internal(e.to_string()))?;
    let config = Arc::new(config);

    let secrets: Arc<dyn SecretsProvider> = {
        let sc = &config.secrets;
        let provider = broodlink_secrets::create_provider(
            &sc.provider,
            sc.sops_file.as_deref(),
            sc.age_identity.as_deref(),
            sc.infisical_url.as_deref(),
            sc.infisical_token.as_deref(),
        )
        .map_err(|e| WsError::Internal(e.to_string()))?;
        Arc::from(provider)
    };

    let pg_password = secrets
        .get(&config.postgres.password_key)
        .await
        .map_err(|e| WsError::Internal(e.to_string()))?;

    let pg_url = format!(
        "postgres://{}:{}@{}:{}/{}",
        config.postgres.user,
        pg_password,
        config.postgres.host,
        config.postgres.port,
        config.postgres.database,
    );
    let pg = PgPoolOptions::new()
        .min_connections(config.postgres.min_connections)
        .max_connections(config.postgres.max_connections)
        .acquire_timeout(Duration::from_secs(5))
        .idle_timeout(Duration::from_secs(300))
        .max_lifetime(Duration::from_secs(1800))
        .connect(&pg_url)
        .await?;
    info!("postgres pool connected");

    ensure_schema(&pg).await?;
    auth::ensure_auth_schema(&pg).await?;
    calendar::ensure_calendar_schema(&pg).await?;
    email::ensure_email_schema(&pg).await?;
    chat::ensure_chat_schema(&pg).await?;
    memory::ensure_memory_schema(&pg).await?;
    documents::ensure_documents_schema(&pg).await?;
    presets::ensure_presets_schema(&pg).await?;
    skills::ensure_skills_schema(&pg).await?;
    compare::ensure_compare_schema(&pg).await?;
    signatures::ensure_signatures_schema(&pg).await?;
    contacts::ensure_contacts_schema(&pg).await?;
    gallery::ensure_gallery_schema(&pg).await?;
    webhooks::ensure_webhooks_schema(&pg).await?;
    vault::ensure_vault_schema(&pg).await?;
    research::ensure_research_schema(&pg).await?;
    embeddings::ensure_embeddings_schema(&pg).await?;
    info!("workspace schema ready");

    let ui_dir = config.workspace_api.ui_dir.clone();

    let cipher = crypto::Cipher::load_or_create("data/.workspace_key")
        .map_err(|e| WsError::Internal(format!("encryption key: {e}")))?;

    Ok(AppState {
        pg,
        config,
        ui_dir,
        login_attempts: RwLock::new(HashMap::new()),
        cipher,
    })
}

/// Idempotent schema bootstrap. Notes/tasks are user-facing app data and live
/// in Postgres alongside Broodlink's other hot-path tables.
async fn ensure_schema(pg: &PgPool) -> Result<(), sqlx::Error> {
    sqlx::raw_sql(
        r#"
        CREATE TABLE IF NOT EXISTS ws_notes (
            id                TEXT PRIMARY KEY,
            owner             TEXT,
            title             TEXT NOT NULL DEFAULT '',
            content           TEXT,
            items             TEXT,
            note_type         TEXT NOT NULL DEFAULT 'note',
            color             TEXT,
            label             TEXT,
            pinned            BOOLEAN NOT NULL DEFAULT FALSE,
            archived          BOOLEAN NOT NULL DEFAULT FALSE,
            due_date          TEXT,
            source            TEXT NOT NULL DEFAULT 'user',
            session_id        TEXT,
            sort_order        INTEGER NOT NULL DEFAULT 0,
            image_url         TEXT,
            repeat            TEXT NOT NULL DEFAULT 'none',
            ai_classification TEXT,
            ai_content_hash   TEXT,
            agent_session_id  TEXT,
            created_at        TIMESTAMPTZ NOT NULL DEFAULT now(),
            updated_at        TIMESTAMPTZ NOT NULL DEFAULT now()
        );
        CREATE INDEX IF NOT EXISTS ws_notes_owner_idx ON ws_notes(owner);
        "#,
    )
    .execute(pg)
    .await?;

    sqlx::raw_sql(
        r#"
        CREATE TABLE IF NOT EXISTS ws_scheduled_tasks (
            id                    TEXT PRIMARY KEY,
            owner                 TEXT,
            name                  TEXT NOT NULL,
            prompt                TEXT,
            task_type             TEXT NOT NULL DEFAULT 'llm',
            action                TEXT,
            schedule              TEXT,
            scheduled_time        TEXT,
            scheduled_day         INTEGER,
            scheduled_date        TIMESTAMPTZ,
            cron_expression       TEXT,
            trigger_type          TEXT NOT NULL DEFAULT 'schedule',
            trigger_event         TEXT,
            trigger_count         INTEGER,
            trigger_counter       INTEGER NOT NULL DEFAULT 0,
            next_run              TIMESTAMPTZ,
            last_run              TIMESTAMPTZ,
            status                TEXT NOT NULL DEFAULT 'active',
            output_target         TEXT NOT NULL DEFAULT 'session',
            session_id            TEXT,
            model                 TEXT,
            endpoint_url          TEXT,
            run_count             INTEGER NOT NULL DEFAULT 0,
            webhook_token         TEXT,
            then_task_id          TEXT,
            notifications_enabled BOOLEAN NOT NULL DEFAULT TRUE,
            created_at            TIMESTAMPTZ NOT NULL DEFAULT now(),
            updated_at            TIMESTAMPTZ NOT NULL DEFAULT now()
        );
        CREATE INDEX IF NOT EXISTS ws_tasks_owner_idx ON ws_scheduled_tasks(owner);
        CREATE INDEX IF NOT EXISTS ws_tasks_next_run_idx ON ws_scheduled_tasks(next_run);
        "#,
    )
    .execute(pg)
    .await?;

    sqlx::raw_sql(
        r#"
        CREATE TABLE IF NOT EXISTS ws_task_runs (
            id          TEXT PRIMARY KEY,
            task_id     TEXT NOT NULL REFERENCES ws_scheduled_tasks(id) ON DELETE CASCADE,
            started_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
            finished_at TIMESTAMPTZ,
            status      TEXT NOT NULL DEFAULT 'running',
            result      TEXT,
            error       TEXT,
            tokens_used INTEGER,
            steps       TEXT,
            model       TEXT
        );
        CREATE INDEX IF NOT EXISTS ws_task_runs_task_idx ON ws_task_runs(task_id);
        "#,
    )
    .execute(pg)
    .await?;

    Ok(())
}

#[tokio::main]
async fn main() {
    let boot_config = Config::load().unwrap_or_else(|e| {
        eprintln!("fatal: failed to load config: {e}");
        process::exit(1);
    });

    let _telemetry_guard =
        broodlink_telemetry::init_telemetry(SERVICE_NAME, &boot_config.telemetry).unwrap_or_else(
            |e| {
                eprintln!("fatal: telemetry init failed: {e}");
                process::exit(1);
            },
        );

    info!(
        service = SERVICE_NAME,
        version = SERVICE_VERSION,
        "starting"
    );

    if !boot_config.workspace_api.enabled {
        info!("workspace_api disabled in config — exiting");
        return;
    }

    let state = match init_state().await {
        Ok(s) => s,
        Err(e) => {
            error!(error = %e, "fatal: failed to initialise");
            process::exit(1);
        }
    };

    let port = state.config.workspace_api.port;
    let shared = Arc::new(state);

    // Background dispatcher for scheduled emails.
    tokio::spawn(email::run_scheduled_poller(Arc::clone(&shared)));

    let app = build_router(Arc::clone(&shared));
    let addr = SocketAddr::from(([127, 0, 0, 1], port));

    info!(addr = %addr, "listening");
    let listener = match tokio::net::TcpListener::bind(addr).await {
        Ok(l) => l,
        Err(e) => {
            error!(error = %e, "failed to bind");
            process::exit(1);
        }
    };

    if let Err(e) = axum::serve(listener, app.into_make_service()).await {
        error!(error = %e, "server error");
        process::exit(1);
    }
}
