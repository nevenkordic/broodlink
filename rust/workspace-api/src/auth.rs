/*
 * Broodlink workspace-api — Authentication & sessions.
 *
 * Backed by Broodlink's shared `dashboard_users` + `dashboard_sessions` tables
 * (the same store the operations dashboard uses), so there is ONE login across
 * Broodlink. The HTTP surface mirrors the workspace app's auth contract (cookie
 * `broodlink_session`, /api/auth/ endpoints) so the absorbed login UI works
 * unchanged. `owner` for notes/tasks == the authenticated username.
 *
 * 2FA (TOTP) is not yet ported — the dashboard_users schema carries no TOTP
 * columns and no TOTP/QR crate is vendored. The 2FA endpoints respond cleanly
 * (status: disabled) so the login + settings UI does not error.
 */

use std::sync::Arc;
use std::time::Instant;

use axum::extract::{Request, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Json, Redirect, Response};
use chrono::{Duration, Utc};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{AppState, WsError};

const COOKIE: &str = "broodlink_session";
const REMEMBER_TTL_HOURS: i64 = 24 * 7;

// ---------------------------------------------------------------------------
// Schema (idempotent; matches migrations/021_dashboard_auth.sql)
// ---------------------------------------------------------------------------

pub async fn ensure_auth_schema(pg: &sqlx::PgPool) -> Result<(), sqlx::Error> {
    sqlx::raw_sql(
        r#"
        CREATE TABLE IF NOT EXISTS dashboard_users (
            id            VARCHAR(36) PRIMARY KEY,
            username      VARCHAR(100) NOT NULL UNIQUE,
            password_hash VARCHAR(255) NOT NULL,
            role          VARCHAR(20) NOT NULL DEFAULT 'viewer',
            display_name  VARCHAR(255),
            active        BOOLEAN DEFAULT TRUE,
            last_login    TIMESTAMPTZ,
            created_at    TIMESTAMPTZ DEFAULT NOW(),
            updated_at    TIMESTAMPTZ DEFAULT NOW()
        );
        CREATE TABLE IF NOT EXISTS dashboard_sessions (
            id         VARCHAR(36) PRIMARY KEY,
            user_id    VARCHAR(36) NOT NULL REFERENCES dashboard_users(id) ON DELETE CASCADE,
            expires_at TIMESTAMPTZ NOT NULL,
            created_at TIMESTAMPTZ DEFAULT NOW()
        );
        CREATE INDEX IF NOT EXISTS idx_dashboard_sessions_expires ON dashboard_sessions(expires_at);
        "#,
    )
    .execute(pg)
    .await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn cookie_value(headers: &HeaderMap, name: &str) -> Option<String> {
    let raw = headers.get(header::COOKIE)?.to_str().ok()?;
    for part in raw.split(';') {
        let part = part.trim();
        if let Some(v) = part.strip_prefix(&format!("{name}=")) {
            if !v.is_empty() {
                return Some(v.to_string());
            }
        }
    }
    None
}

fn secure_cookies() -> bool {
    std::env::var("SECURE_COOKIES")
        .map(|v| v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

fn set_cookie_header(token: &str, remember: bool) -> String {
    let secure = if secure_cookies() { "; Secure" } else { "" };
    let max_age = if remember {
        format!("; Max-Age={}", REMEMBER_TTL_HOURS * 3600)
    } else {
        String::new()
    };
    format!("{COOKIE}={token}; HttpOnly; Path=/; SameSite=Lax{secure}{max_age}")
}

fn role_is_admin(role: &str) -> bool {
    role.eq_ignore_ascii_case("admin")
}

fn privileges_for(role: &str) -> Value {
    let admin = role_is_admin(role);
    json!({
        "can_use_agent": true,
        "can_use_browser": true,
        "can_use_bash": admin,
        "can_use_documents": true,
        "can_use_research": true,
        "can_generate_images": true,
        "can_manage_memory": true,
        "max_messages_per_day": 0,
        "allowed_models": [],
    })
}

pub struct SessionUser {
    pub username: String,
    pub role: String,
}

/// Resolve a session token to its (active) user, if the session is unexpired.
pub async fn resolve_session(pg: &sqlx::PgPool, token: &str) -> Option<SessionUser> {
    let row: Option<(String, String, bool)> = sqlx::query_as(
        "SELECT u.username, u.role, u.active \
         FROM dashboard_sessions s JOIN dashboard_users u ON u.id = s.user_id \
         WHERE s.id = $1 AND s.expires_at > NOW()",
    )
    .bind(token)
    .fetch_optional(pg)
    .await
    .ok()
    .flatten();
    let (username, role, active) = row?;
    if !active {
        return None;
    }
    Some(SessionUser { username, role })
}

async fn user_count(pg: &sqlx::PgPool) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM dashboard_users")
        .fetch_one(pg)
        .await
        .unwrap_or(0)
}

fn rate_limited_check(_username: &str) {}

// ---------------------------------------------------------------------------
// Middleware
// ---------------------------------------------------------------------------

const EXEMPT_EXACT: &[&str] = &[
    "/login",
    "/api/auth/login",
    "/api/auth/setup",
    "/api/auth/signup",
    "/api/auth/status",
    "/api/auth/logout",
    "/api/auth/features",
    "/api/health",
];

fn is_asset(path: &str) -> bool {
    path.starts_with("/static")
        || matches!(
            path.rsplit('.').next(),
            Some(
                "js" | "css"
                    | "png"
                    | "svg"
                    | "jpg"
                    | "jpeg"
                    | "gif"
                    | "webp"
                    | "ico"
                    | "woff"
                    | "woff2"
                    | "ttf"
                    | "map"
                    | "json"
                    | "webmanifest"
                    | "txt"
            )
        )
}

pub async fn auth_mw(State(state): State<Arc<AppState>>, mut req: Request, next: Next) -> Response {
    let path = req.uri().path().to_string();

    if EXEMPT_EXACT.contains(&path.as_str()) || is_asset(&path) {
        return next.run(req).await;
    }

    let token = cookie_value(req.headers(), COOKIE);
    let user = match token {
        Some(t) => resolve_session(&state.pg, &t).await,
        None => None,
    };

    match user {
        Some(u) => {
            // Stamp the trusted owner for downstream handlers, overriding any
            // client-supplied X-Owner.
            if let Ok(val) = HeaderValue::from_str(&u.username) {
                req.headers_mut().insert("x-owner", val);
            }
            next.run(req).await
        }
        None => {
            if path.starts_with("/api/") {
                (
                    StatusCode::UNAUTHORIZED,
                    Json(json!({ "error": "Not authenticated" })),
                )
                    .into_response()
            } else {
                Redirect::to("/login").into_response()
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Endpoints
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct SetupBody {
    username: String,
    password: String,
}

pub async fn setup(
    State(state): State<Arc<AppState>>,
    Json(body): Json<SetupBody>,
) -> Result<Json<Value>, WsError> {
    if user_count(&state.pg).await > 0 {
        return Err(WsError::BadRequest("already configured".into()));
    }
    if body.password.len() < 8 {
        return Err(WsError::BadRequest(
            "password must be at least 8 characters".into(),
        ));
    }
    let username = body.username.trim().to_lowercase();
    if username.is_empty() {
        return Err(WsError::BadRequest("username required".into()));
    }
    let cost = state.config.dashboard_auth.bcrypt_cost;
    let hash = bcrypt::hash(&body.password, cost)
        .map_err(|e| WsError::Internal(format!("bcrypt: {e}")))?;
    sqlx::query(
        "INSERT INTO dashboard_users (id, username, password_hash, role, active) \
         VALUES ($1, $2, $3, 'admin', TRUE)",
    )
    .bind(Uuid::new_v4().to_string())
    .bind(&username)
    .bind(&hash)
    .execute(&state.pg)
    .await?;
    Ok(Json(
        json!({ "ok": true, "message": "Admin account created" }),
    ))
}

#[derive(Deserialize)]
pub struct LoginBody {
    username: String,
    password: String,
    #[serde(default = "default_remember")]
    remember: bool,
    #[serde(default)]
    #[allow(dead_code)]
    totp_code: Option<String>,
}

fn default_remember() -> bool {
    true
}

pub async fn login(
    State(state): State<Arc<AppState>>,
    Json(body): Json<LoginBody>,
) -> Result<Response, WsError> {
    let username = body.username.trim().to_lowercase();

    // Rate limit: 5 failed attempts / 5 min / username.
    {
        let attempts = state.login_attempts.read().await;
        if let Some((count, since)) = attempts.get(&username) {
            if since.elapsed().as_secs() < 300 && *count >= 5 {
                return Err(WsError::BadRequest(
                    "too many login attempts, try again later".into(),
                ));
            }
        }
    }
    rate_limited_check(&username);

    let user: Option<(String, String, String, bool)> = sqlx::query_as(
        "SELECT id, password_hash, role, active FROM dashboard_users WHERE username = $1",
    )
    .bind(&username)
    .fetch_optional(&state.pg)
    .await?;

    let record_failure = || async {
        let mut attempts = state.login_attempts.write().await;
        let entry = attempts
            .entry(username.clone())
            .or_insert((0, Instant::now()));
        if entry.1.elapsed().as_secs() >= 300 {
            *entry = (1, Instant::now());
        } else {
            entry.0 += 1;
        }
    };

    let (user_id, password_hash, role, active) = match user {
        Some(u) => u,
        None => {
            record_failure().await;
            return Err(WsError::Unauthorized("invalid username or password".into()));
        }
    };
    if !active {
        return Err(WsError::Unauthorized("account is deactivated".into()));
    }

    let valid = bcrypt::verify(&body.password, &password_hash)
        .map_err(|e| WsError::Internal(format!("bcrypt: {e}")))?;
    if !valid {
        record_failure().await;
        return Err(WsError::Unauthorized("invalid username or password".into()));
    }

    // Success — clear failure counter, mint session.
    state.login_attempts.write().await.remove(&username);

    let token = Uuid::new_v4().to_string();
    let ttl_hours = if body.remember {
        REMEMBER_TTL_HOURS
    } else {
        i64::from(state.config.dashboard_auth.session_ttl_hours)
    };
    let expires_at = Utc::now() + Duration::hours(ttl_hours);

    sqlx::query("INSERT INTO dashboard_sessions (id, user_id, expires_at) VALUES ($1, $2, $3)")
        .bind(&token)
        .bind(&user_id)
        .bind(expires_at)
        .execute(&state.pg)
        .await?;
    sqlx::query("UPDATE dashboard_users SET last_login = NOW() WHERE id = $1")
        .bind(&user_id)
        .execute(&state.pg)
        .await?;

    let mut resp = Json(json!({ "ok": true, "username": username })).into_response();
    if let Ok(v) = HeaderValue::from_str(&set_cookie_header(&token, body.remember)) {
        resp.headers_mut().insert(header::SET_COOKIE, v);
    }
    let _ = role;
    Ok(resp)
}

pub async fn logout(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Response, WsError> {
    if let Some(token) = cookie_value(&headers, COOKIE) {
        sqlx::query("DELETE FROM dashboard_sessions WHERE id = $1")
            .bind(&token)
            .execute(&state.pg)
            .await?;
    }
    let mut resp = Json(json!({ "ok": true })).into_response();
    if let Ok(v) = HeaderValue::from_str(&format!("{COOKIE}=; Path=/; Max-Age=0")) {
        resp.headers_mut().insert(header::SET_COOKIE, v);
    }
    Ok(resp)
}

pub async fn status(State(state): State<Arc<AppState>>, headers: HeaderMap) -> Json<Value> {
    let configured = user_count(&state.pg).await > 0;
    let user = match cookie_value(&headers, COOKIE) {
        Some(t) => resolve_session(&state.pg, &t).await,
        None => None,
    };
    match user {
        Some(u) => Json(json!({
            "configured": configured,
            "authenticated": true,
            "username": u.username,
            "is_admin": role_is_admin(&u.role),
            "signup_enabled": false,
            "privileges": privileges_for(&u.role),
        })),
        None => Json(json!({
            "configured": configured,
            "authenticated": false,
            "username": Value::Null,
            "is_admin": false,
            "signup_enabled": false,
        })),
    }
}

// Open registration is not enabled in this store.
pub async fn signup() -> Result<Json<Value>, WsError> {
    Err(WsError::Forbidden("registration is disabled".into()))
}

// --- 2FA: not yet ported (no TOTP columns / crate). Graceful responses. ---

pub async fn twofa_status() -> Json<Value> {
    Json(json!({ "enabled": false }))
}

pub async fn twofa_unavailable() -> Result<Json<Value>, WsError> {
    Err(WsError::BadRequest(
        "two-factor auth is not available yet".into(),
    ))
}
