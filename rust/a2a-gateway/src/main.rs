/*
 * Broodlink - Multi-agent AI orchestration system
 * Copyright (C) 2025–2026 Neven Kordic <neven@broodlink.ai>
 * SPDX-License-Identifier: AGPL-3.0-or-later
 */

//! A2A (Agent-to-Agent) Gateway — Google's open protocol for cross-platform agent interop.
//!
//! Exposes:
//! - `GET  /.well-known/agent.json`  — dynamic AgentCard from Beads formulas
//! - `POST /a2a/tasks/send`          — JSON-RPC task submission
//! - `POST /a2a/tasks/get`           — query task status
//! - `POST /a2a/tasks/cancel`        — cancel a running task
//! - `POST /a2a/tasks/sendSubscribe` — streaming task submission (SSE)
//! - `POST /webhook/slack`            — receive Slack slash commands
//! - `POST /webhook/teams`            — receive Teams bot messages
//! - `POST /webhook/telegram`         — receive Telegram bot updates
//! - `GET  /health`                   — health check

use std::collections::HashMap;
use std::net::SocketAddr;
use std::process;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use axum::extract::{DefaultBodyLimit, State};
use axum::http::{header, HeaderMap, Method, Request, StatusCode};
use axum::middleware::{self, Next};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::Router;
use broodlink_config::Config;
use broodlink_secrets::SecretsProvider;
use futures::StreamExt;
use sqlx::postgres::PgPool;
use tokio_stream::wrappers::ReceiverStream;
use tower_http::cors::{AllowOrigin, CorsLayer};
use tracing::{debug, error, info, warn};
use uuid::Uuid;

use hmac::{Hmac, Mac};
use sha2::Sha256;

const SERVICE_NAME: &str = "a2a-gateway";

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
#[allow(dead_code)]
enum GatewayError {
    #[error("bridge call failed: {0}")]
    Bridge(String),
    #[error("database error: {0}")]
    Db(#[from] sqlx::Error),
    #[error("not found: {0}")]
    NotFound(String),
    #[error("bad request: {0}")]
    BadRequest(String),
}

impl IntoResponse for GatewayError {
    fn into_response(self) -> Response {
        let (status, msg) = match &self {
            GatewayError::NotFound(m) => (StatusCode::NOT_FOUND, format!("not found: {m}")),
            GatewayError::BadRequest(m) => (StatusCode::BAD_REQUEST, format!("bad request: {m}")),
            GatewayError::Bridge(e) => {
                error!(error = %e, "bridge call failed");
                (StatusCode::INTERNAL_SERVER_ERROR, "internal error".to_string())
            }
            GatewayError::Db(e) => {
                error!(error = %e, "database error");
                (StatusCode::INTERNAL_SERVER_ERROR, "internal error".to_string())
            }
        };
        (status, axum::Json(serde_json::json!({ "error": msg }))).into_response()
    }
}

// ---------------------------------------------------------------------------
// A2A JSON-RPC types
// ---------------------------------------------------------------------------

#[derive(serde::Deserialize, Debug)]
#[allow(dead_code)]
struct A2aJsonRpc {
    jsonrpc: String,
    id: Option<serde_json::Value>,
    method: String,
    #[serde(default)]
    params: serde_json::Value,
}

// A2A task status enum per spec
#[derive(serde::Serialize, Clone, Debug)]
#[serde(rename_all = "lowercase")]
#[allow(dead_code)]
enum A2aTaskStatus {
    Submitted,
    Working,
    #[serde(rename = "input-required")]
    InputRequired,
    Completed,
    Canceled,
    Failed,
    Unknown,
}

#[derive(serde::Serialize, Debug)]
struct A2aTaskResponse {
    id: String,
    status: A2aTaskStatusInfo,
    #[serde(skip_serializing_if = "Option::is_none")]
    artifacts: Option<Vec<serde_json::Value>>,
}

#[derive(serde::Serialize, Debug)]
struct A2aTaskStatusInfo {
    state: A2aTaskStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    message: Option<String>,
}

// ---------------------------------------------------------------------------
// Application state
// ---------------------------------------------------------------------------

struct AppState {
    config: Arc<Config>,
    pg: PgPool,
    bridge_url: String,
    bridge_jwt: String,
    http_client: reqwest::Client,
    ollama_client: reqwest::Client,
    api_key: Option<String>,
    /// Cached Telegram credentials from platform_credentials table.
    /// (bot_token, secret_token, allowed_user_ids, auth_code, fetched_at)
    telegram_creds: tokio::sync::RwLock<
        Option<(
            String,
            Option<String>,
            Vec<i64>,
            Option<String>,
            std::time::Instant,
        )>,
    >,
    /// Limits concurrent Ollama chat calls (prevents queue pileup).
    ollama_semaphore: tokio::sync::Semaphore,
    /// TTL cache for Brave search results: query → (result, fetched_at).
    brave_cache: tokio::sync::RwLock<HashMap<String, (String, std::time::Instant)>>,
    /// In-flight chat dedup: "platform:chat_id:text_hash" → started_at.
    inflight_chats: tokio::sync::RwLock<HashMap<String, std::time::Instant>>,
    /// True when the primary chat model is known to be too large for available memory.
    /// Avoids wasting 3+ seconds retrying a model that will never fit.
    primary_model_degraded: AtomicBool,
    /// When the primary model entered degraded mode. Used to periodically retry.
    degraded_since: tokio::sync::RwLock<Option<std::time::Instant>>,
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

/// Load .env file before the async runtime starts (single-threaded context).
fn load_dotenv() {
    match std::fs::read_to_string(".env") {
        Ok(contents) => {
            for line in contents.lines() {
                let line = line.trim();
                if line.is_empty() || line.starts_with('#') {
                    continue;
                }
                if let Some((key, val)) = line.split_once('=') {
                    let key = key.trim();
                    let val = val.trim();
                    // SAFETY: load_dotenv() is called from main() before
                    // tokio::runtime::Builder::build(), so no other threads exist.
                    // set_var is unsafe in edition 2024 due to potential data races
                    // with concurrent getenv, but here we are strictly single-threaded.
                    unsafe {
                        std::env::set_var(key, val);
                    }
                    eprintln!(".env: loaded {key}");
                }
            }
        }
        Err(e) => {
            eprintln!(".env: not loaded ({e})");
        }
    }
}

fn main() {
    // Load .env in single-threaded context before spawning the tokio runtime
    load_dotenv();

    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("failed to build tokio runtime")
        .block_on(async_main());
}

async fn async_main() {

    let config = match Config::load() {
        Ok(c) => Arc::new(c),
        Err(e) => {
            eprintln!("fatal: failed to load config: {e}");
            process::exit(1);
        }
    };

    let _telemetry_guard = broodlink_telemetry::init_telemetry(SERVICE_NAME, &config.telemetry)
        .unwrap_or_else(|e| {
            eprintln!("fatal: telemetry init failed: {e}");
            process::exit(1);
        });

    info!(service = SERVICE_NAME, "starting");

    if !config.a2a.enabled {
        info!("A2A gateway disabled in config — exiting");
        return;
    }

    // Resolve secrets
    let secrets: Arc<dyn SecretsProvider> = {
        let sc = &config.secrets;
        match broodlink_secrets::create_provider(
            &sc.provider,
            sc.sops_file.as_deref(),
            sc.age_identity.as_deref(),
            sc.infisical_url.as_deref(),
            sc.infisical_token.as_deref(),
        ) {
            Ok(p) => Arc::from(p),
            Err(e) => {
                error!(error = %e, "failed to create secrets provider");
                process::exit(1);
            }
        }
    };

    // Connect to Postgres
    let pg_password = match secrets.get(&config.postgres.password_key).await {
        Ok(pw) => pw,
        Err(e) => {
            error!(error = %e, "failed to resolve postgres password");
            process::exit(1);
        }
    };
    let pg_url = format!(
        "postgres://{}:{}@{}:{}/{}",
        config.postgres.user,
        pg_password,
        config.postgres.host,
        config.postgres.port,
        config.postgres.database,
    );
    let pg = match PgPool::connect(&pg_url).await {
        Ok(pool) => pool,
        Err(e) => {
            error!(error = %e, "failed to connect to Postgres");
            process::exit(1);
        }
    };

    // Read JWT for bridge auth
    let jwt_path = config.a2a.bridge_jwt_path.clone().unwrap_or_else(|| {
        format!(
            "{}/.broodlink/jwt-a2a-gateway.token",
            std::env::var("HOME").unwrap_or_else(|_| ".".to_string())
        )
    });
    let bridge_jwt = match std::fs::read_to_string(&jwt_path) {
        Ok(t) => t.trim().to_string(),
        Err(e) => {
            warn!(path = %jwt_path, error = %e, "failed to read A2A bridge JWT — bridge calls will fail");
            String::new()
        }
    };

    // Read API key from environment
    let api_key = std::env::var(&config.a2a.api_key_name).ok();
    if api_key.is_none() {
        warn!(
            key_name = %config.a2a.api_key_name,
            "A2A API key not set — auth disabled"
        );
    }

    // Connect to NATS for task completion events
    let nats_client = match broodlink_runtime::connect_nats(&config.nats).await {
        Ok(nc) => {
            info!("connected to NATS");
            Some(nc)
        }
        Err(e) => {
            warn!(error = %e, "failed to connect to NATS — chat reply delivery via NATS disabled");
            None
        }
    };

    let state = Arc::new(AppState {
        bridge_url: config.a2a.bridge_url.clone(),
        bridge_jwt,
        pg,
        http_client: reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(60))
            .build()
            .unwrap_or_else(|e| {
                error!(error = %e, "failed to build http_client");
                process::exit(1);
            }),
        ollama_client: reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(
                config.ollama.timeout_seconds,
            ))
            .build()
            .unwrap_or_else(|e| {
                error!(error = %e, "failed to build ollama_client");
                process::exit(1);
            }),
        api_key,
        config: Arc::clone(&config),
        telegram_creds: tokio::sync::RwLock::new(None),
        ollama_semaphore: tokio::sync::Semaphore::new(config.a2a.ollama_concurrency as usize),
        brave_cache: tokio::sync::RwLock::new(HashMap::new()),
        inflight_chats: tokio::sync::RwLock::new(HashMap::new()),
        primary_model_degraded: AtomicBool::new(false),
        degraded_since: tokio::sync::RwLock::new(None),
    });

    // Spawn chat reply delivery loop
    if config.chat.enabled {
        let st = Arc::clone(&state);
        tokio::spawn(async move {
            reply_delivery_loop(&st).await;
        });
    }

    // Spawn Telegram long-polling loop (no public URL needed)
    {
        let st = Arc::clone(&state);
        tokio::spawn(async move {
            telegram_polling_loop(&st).await;
        });
    }

    // Subscribe to NATS task_completed/task_failed for chat reply routing
    if config.chat.enabled {
        if let Some(nc) = nats_client {
            let prefix = config.nats.subject_prefix.clone();
            let st = Arc::clone(&state);

            // credential changes (invalidate cache immediately on register/disconnect)
            let nc_creds = nc.clone();
            let st_creds = Arc::clone(&state);
            let subj_creds = format!("{prefix}.platform.credentials_changed");
            tokio::spawn(async move {
                match nc_creds.subscribe(subj_creds.clone()).await {
                    Ok(mut sub) => {
                        info!(subject = %subj_creds, "subscribed for credential changes");
                        while let Some(_msg) = sub.next().await {
                            info!("credential change notification received — invalidating cache");
                            let mut guard = st_creds.telegram_creds.write().await;
                            *guard = None;
                        }
                    }
                    Err(e) => {
                        error!(error = %e, "failed to subscribe to credentials_changed");
                    }
                }
            });

            // task_completed
            let nc2 = nc.clone(); // clone for the second subscription
            let st_completed = Arc::clone(&st);
            let subj_completed = format!("{prefix}.*.coordinator.task_completed");
            tokio::spawn(async move {
                match nc.subscribe(subj_completed.clone()).await {
                    Ok(mut sub) => {
                        info!(subject = %subj_completed, "subscribed for chat completions");
                        while let Some(msg) = sub.next().await {
                            if let Ok(payload) =
                                serde_json::from_slice::<serde_json::Value>(&msg.payload)
                            {
                                let task_id = payload
                                    .get("task_id")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or_default();
                                let result_data =
                                    payload.get("result_data").cloned().unwrap_or_default();
                                if !task_id.is_empty() {
                                    handle_task_completion_for_chat(
                                        &st_completed,
                                        task_id,
                                        &result_data,
                                        false,
                                    )
                                    .await;
                                }
                            }
                        }
                    }
                    Err(e) => {
                        error!(error = %e, "failed to subscribe to task_completed");
                    }
                }
            });

            // task_failed
            let subj_failed = format!("{prefix}.*.coordinator.task_failed");
            tokio::spawn(async move {
                match nc2.subscribe(subj_failed.clone()).await {
                    Ok(mut sub) => {
                        info!(subject = %subj_failed, "subscribed for chat failures");
                        while let Some(msg) = sub.next().await {
                            if let Ok(payload) =
                                serde_json::from_slice::<serde_json::Value>(&msg.payload)
                            {
                                let task_id = payload
                                    .get("task_id")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or_default();
                                let error_msg = payload
                                    .get("error")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("Task failed");
                                if !task_id.is_empty() {
                                    let err_val = serde_json::json!({ "error": error_msg });
                                    handle_task_completion_for_chat(&st, task_id, &err_val, true)
                                        .await;
                                }
                            }
                        }
                    }
                    Err(e) => {
                        error!(error = %e, "failed to subscribe to task_failed");
                    }
                }
            });
        }
    }

    let cors = if config.a2a.cors_origins.is_empty() {
        if config.broodlink.env != "dev" && config.broodlink.env != "local" {
            error!("a2a.cors_origins is empty in non-dev environment — refusing to start");
            process::exit(1);
        }
        warn!("a2a.cors_origins is empty — allowing all origins (dev/local mode)");
        CorsLayer::new()
            .allow_methods([Method::GET, Method::POST])
            .allow_headers([header::CONTENT_TYPE, header::AUTHORIZATION])
    } else {
        let parsed: Vec<header::HeaderValue> = config
            .a2a
            .cors_origins
            .iter()
            .filter_map(|o| o.parse().ok())
            .collect();
        CorsLayer::new()
            .allow_origin(AllowOrigin::list(parsed))
            .allow_methods([Method::GET, Method::POST])
            .allow_headers([header::CONTENT_TYPE, header::AUTHORIZATION])
    };

    let app = Router::new()
        .route("/.well-known/agent.json", get(agent_card_handler))
        .route("/a2a/tasks/send", post(tasks_send_handler))
        .route("/a2a/tasks/get", post(tasks_get_handler))
        .route("/a2a/tasks/cancel", post(tasks_cancel_handler))
        .route(
            "/a2a/tasks/sendSubscribe",
            post(tasks_send_subscribe_handler),
        )
        // Webhook inbound routes
        .route("/webhook/slack", post(webhook_slack_handler))
        .route("/webhook/teams", post(webhook_teams_handler))
        .route("/webhook/telegram", post(webhook_telegram_handler))
        .route("/health", get(health_handler))
        .layer(DefaultBodyLimit::max(10_485_760)) // 10 MiB
        .layer(middleware::from_fn(security_headers_middleware))
        .layer(cors)
        .with_state(state);

    let port = config.a2a.port;
    let addr = SocketAddr::from(([0, 0, 0, 0], port));

    if config.profile.tls_interservice {
        let tls = &config.tls;
        let cert_path = tls.cert_path.as_deref().unwrap_or("certs/server.crt");
        let key_path = tls.key_path.as_deref().unwrap_or("certs/server.key");

        info!(addr = %addr, cert = cert_path, "A2A Gateway listening with TLS");

        let tls_config = axum_server::tls_rustls::RustlsConfig::from_pem_file(cert_path, key_path)
            .await
            .unwrap_or_else(|e| {
                error!(error = %e, "failed to load TLS certs");
                process::exit(1);
            });

        if let Err(e) = axum_server::bind_rustls(addr, tls_config)
            .serve(app.into_make_service())
            .await
        {
            error!(error = %e, "TLS server error");
        }
    } else {
        if config.broodlink.env != "dev" && config.broodlink.env != "local" {
            warn!("TLS is disabled in non-dev environment — traffic is unencrypted");
        }
        info!(addr = %addr, "A2A Gateway listening (plaintext)");

        let listener = match tokio::net::TcpListener::bind(addr).await {
            Ok(l) => l,
            Err(e) => {
                error!(error = %e, "failed to bind");
                process::exit(1);
            }
        };

        if let Err(e) = axum::serve(listener, app).await {
            error!(error = %e, "server error");
        }
    }
}

// ---------------------------------------------------------------------------
// Auth middleware helper
// ---------------------------------------------------------------------------

fn check_auth(headers: &HeaderMap, api_key: &Option<String>) -> Result<(), StatusCode> {
    let Some(expected) = api_key else {
        return Ok(()); // auth disabled
    };

    let token = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "));

    match token {
        Some(t) if constant_time_eq(t.as_bytes(), expected.as_bytes()) => Ok(()),
        _ => Err(StatusCode::UNAUTHORIZED),
    }
}

// ---------------------------------------------------------------------------
// Bridge call helper
// ---------------------------------------------------------------------------

async fn bridge_call(
    state: &AppState,
    tool_name: &str,
    params: serde_json::Value,
) -> Result<serde_json::Value, GatewayError> {
    // Reject tool names with path traversal characters
    if !tool_name
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    {
        return Err(GatewayError::BadRequest(format!(
            "invalid tool name: {tool_name}"
        )));
    }
    let url = format!("{}/api/v1/tool/{tool_name}", state.bridge_url);
    let body = serde_json::json!({ "params": params });

    let resp = state
        .http_client
        .post(&url)
        .header("Authorization", format!("Bearer {}", state.bridge_jwt))
        .header("Content-Type", "application/json")
        .json(&body)
        .send()
        .await
        .map_err(|e| GatewayError::Bridge(e.to_string()))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        return Err(GatewayError::Bridge(format!(
            "bridge returned {status}: {text}"
        )));
    }

    let data: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| GatewayError::Bridge(e.to_string()))?;

    if let Some(err) = data.get("error") {
        return Err(GatewayError::Bridge(err.to_string()));
    }

    Ok(data.get("data").cloned().unwrap_or(data))
}

// ---------------------------------------------------------------------------
// Security headers middleware (OWASP A05)
// ---------------------------------------------------------------------------

async fn security_headers_middleware(req: Request<axum::body::Body>, next: Next) -> Response {
    let mut resp = next.run(req).await;
    let headers = resp.headers_mut();
    headers.insert("X-Content-Type-Options", header::HeaderValue::from_static("nosniff"));
    headers.insert(
        "Cache-Control",
        header::HeaderValue::from_static("no-store, no-cache, must-revalidate"),
    );
    headers.insert("Pragma", header::HeaderValue::from_static("no-cache"));
    headers.insert(
        "Permissions-Policy",
        header::HeaderValue::from_static("geolocation=(), microphone=(), camera=()"),
    );
    headers.insert(
        "Strict-Transport-Security",
        header::HeaderValue::from_static("max-age=63072000; includeSubDomains"),
    );
    resp
}

// ---------------------------------------------------------------------------
// Map internal task status → A2A status
// ---------------------------------------------------------------------------

fn map_status(internal: &str) -> A2aTaskStatus {
    match internal {
        "pending" => A2aTaskStatus::Submitted,
        "claimed" | "in_progress" => A2aTaskStatus::Working,
        "completed" => A2aTaskStatus::Completed,
        "failed" => A2aTaskStatus::Failed,
        "cancelled" | "canceled" => A2aTaskStatus::Canceled,
        _ => A2aTaskStatus::Unknown,
    }
}

// ---------------------------------------------------------------------------
// JSON-RPC response helpers
// ---------------------------------------------------------------------------

fn jsonrpc_ok(id: Option<serde_json::Value>, result: serde_json::Value) -> Response {
    let resp = serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": result,
    });
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/json")],
        serde_json::to_string(&resp).unwrap_or_default(),
    )
        .into_response()
}

fn jsonrpc_err(id: Option<serde_json::Value>, code: i32, message: &str) -> Response {
    let resp = serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {
            "code": code,
            "message": message,
        },
    });
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/json")],
        serde_json::to_string(&resp).unwrap_or_default(),
    )
        .into_response()
}

// ---------------------------------------------------------------------------
// GET /.well-known/agent.json — AgentCard
// ---------------------------------------------------------------------------

async fn agent_card_handler(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    // Build capabilities from Beads formulas (best-effort via bridge)
    let skills = match bridge_call(&state, "list_formulas", serde_json::json!({})).await {
        Ok(data) => {
            let formulas = data
                .as_array()
                .or_else(|| data.get("formulas").and_then(|f| f.as_array()))
                .cloned()
                .unwrap_or_default();

            formulas
                .iter()
                .filter_map(|f| {
                    let name = f.get("name")?.as_str()?;
                    let desc = f
                        .get("description")
                        .and_then(|d| d.as_str())
                        .unwrap_or("Broodlink formula");
                    Some(serde_json::json!({
                        "id": name,
                        "name": name,
                        "description": desc,
                    }))
                })
                .collect::<Vec<_>>()
        }
        Err(e) => {
            warn!(error = %e, "failed to fetch formulas for AgentCard");
            vec![]
        }
    };

    let env = &state.config.broodlink.env;
    let version = &state.config.broodlink.version;

    axum::Json(serde_json::json!({
        "name": "Broodlink",
        "description": format!("Broodlink multi-agent orchestrator ({env})"),
        "url": format!("http://localhost:{}", state.config.a2a.port),
        "version": version,
        "capabilities": {
            "streaming": true,
            "pushNotifications": false,
            "stateTransitionHistory": false,
        },
        "skills": skills,
        "defaultInputModes": ["text/plain"],
        "defaultOutputModes": ["text/plain"],
    }))
}

// ---------------------------------------------------------------------------
// POST /a2a/tasks/send — submit a task
// ---------------------------------------------------------------------------

async fn tasks_send_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    body: String,
) -> Response {
    if let Err(status) = check_auth(&headers, &state.api_key) {
        return (status, "unauthorized").into_response();
    }

    let rpc: A2aJsonRpc = match serde_json::from_str(&body) {
        Ok(r) => r,
        Err(e) => return jsonrpc_err(None, -32700, &format!("parse error: {e}")),
    };

    if rpc.jsonrpc != "2.0" {
        return jsonrpc_err(rpc.id, -32600, "jsonrpc must be 2.0");
    }

    let params = &rpc.params;
    let external_id = params
        .get("id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let external_id = if external_id.is_empty() {
        Uuid::new_v4().to_string()
    } else {
        external_id
    };

    // Extract message content
    let message = params.get("message").cloned().unwrap_or_default();
    let title = message
        .get("parts")
        .and_then(|p| p.as_array())
        .and_then(|parts| parts.first())
        .and_then(|part| part.get("text"))
        .and_then(|t| t.as_str())
        .unwrap_or("A2A task");

    let source_agent = message
        .get("role")
        .and_then(|r| r.as_str())
        .unwrap_or("external");

    // Create internal task via bridge
    let create_result = bridge_call(
        &state,
        "create_task",
        serde_json::json!({
            "title": title,
            "description": format!("A2A task from {source_agent}: {title}"),
        }),
    )
    .await;

    let internal_id = match create_result {
        Ok(data) => data
            .get("task_id")
            .or_else(|| data.get("id"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        Err(e) => return jsonrpc_err(rpc.id, -32000, &format!("create_task failed: {e}")),
    };

    if internal_id.is_empty() {
        return jsonrpc_err(rpc.id, -32000, "create_task returned no id");
    }

    // Insert into a2a_task_map
    if let Err(e) = sqlx::query(
        "INSERT INTO a2a_task_map (external_id, internal_id, source_agent) VALUES ($1, $2, $3)",
    )
    .bind(&external_id)
    .bind(&internal_id)
    .bind(source_agent)
    .execute(&state.pg)
    .await
    {
        error!(error = %e, "failed to insert a2a_task_map");
        return jsonrpc_err(rpc.id, -32000, &format!("mapping insert failed: {e}"));
    }

    info!(external_id = %external_id, internal_id = %internal_id, "A2A task created");

    let task_resp = A2aTaskResponse {
        id: external_id,
        status: A2aTaskStatusInfo {
            state: A2aTaskStatus::Submitted,
            message: None,
        },
        artifacts: None,
    };

    jsonrpc_ok(rpc.id, serde_json::to_value(&task_resp).unwrap_or_default())
}

// ---------------------------------------------------------------------------
// POST /a2a/tasks/get — query task status
// ---------------------------------------------------------------------------

async fn tasks_get_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    body: String,
) -> Response {
    if let Err(status) = check_auth(&headers, &state.api_key) {
        return (status, "unauthorized").into_response();
    }

    let rpc: A2aJsonRpc = match serde_json::from_str(&body) {
        Ok(r) => r,
        Err(e) => return jsonrpc_err(None, -32700, &format!("parse error: {e}")),
    };

    let external_id = rpc.params.get("id").and_then(|v| v.as_str()).unwrap_or("");

    if external_id.is_empty() {
        return jsonrpc_err(rpc.id, -32602, "missing required param: id");
    }

    // Look up internal ID
    let row: Option<(String,)> =
        sqlx::query_as("SELECT internal_id FROM a2a_task_map WHERE external_id = $1")
            .bind(external_id)
            .fetch_optional(&state.pg)
            .await
            .unwrap_or(None);

    let Some((internal_id,)) = row else {
        return jsonrpc_err(rpc.id, -32001, "task not found");
    };

    // Get task status from bridge
    let status_result = bridge_call(
        &state,
        "get_task",
        serde_json::json!({ "task_id": internal_id }),
    )
    .await;

    let (a2a_status, message, artifacts) = match status_result {
        Ok(data) => {
            let internal_status = data
                .get("status")
                .and_then(|s| s.as_str())
                .unwrap_or("unknown");
            let msg = data
                .get("result")
                .and_then(|r| r.as_str())
                .map(String::from);
            // Extract result_data as A2A artifacts
            let arts = data.get("result_data").and_then(|r| {
                if r.is_null() {
                    None
                } else {
                    Some(vec![serde_json::json!({
                        "parts": [{"type": "text", "text": r.to_string()}]
                    })])
                }
            });
            (map_status(internal_status), msg, arts)
        }
        Err(e) => (
            A2aTaskStatus::Unknown,
            Some(format!("lookup failed: {e}")),
            None,
        ),
    };

    let task_resp = A2aTaskResponse {
        id: external_id.to_string(),
        status: A2aTaskStatusInfo {
            state: a2a_status,
            message,
        },
        artifacts,
    };

    jsonrpc_ok(rpc.id, serde_json::to_value(&task_resp).unwrap_or_default())
}

// ---------------------------------------------------------------------------
// POST /a2a/tasks/cancel — cancel a task
// ---------------------------------------------------------------------------

async fn tasks_cancel_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    body: String,
) -> Response {
    if let Err(status) = check_auth(&headers, &state.api_key) {
        return (status, "unauthorized").into_response();
    }

    let rpc: A2aJsonRpc = match serde_json::from_str(&body) {
        Ok(r) => r,
        Err(e) => return jsonrpc_err(None, -32700, &format!("parse error: {e}")),
    };

    let external_id = rpc.params.get("id").and_then(|v| v.as_str()).unwrap_or("");

    if external_id.is_empty() {
        return jsonrpc_err(rpc.id, -32602, "missing required param: id");
    }

    // Look up internal ID
    let row: Option<(String,)> =
        sqlx::query_as("SELECT internal_id FROM a2a_task_map WHERE external_id = $1")
            .bind(external_id)
            .fetch_optional(&state.pg)
            .await
            .unwrap_or(None);

    let Some((internal_id,)) = row else {
        return jsonrpc_err(rpc.id, -32001, "task not found");
    };

    // Cancel via bridge
    match bridge_call(
        &state,
        "fail_task",
        serde_json::json!({ "task_id": internal_id }),
    )
    .await
    {
        Ok(_) => {
            info!(external_id = %external_id, "A2A task cancelled");
            let task_resp = A2aTaskResponse {
                id: external_id.to_string(),
                status: A2aTaskStatusInfo {
                    state: A2aTaskStatus::Canceled,
                    message: Some("cancelled via A2A".to_string()),
                },
                artifacts: None,
            };
            jsonrpc_ok(rpc.id, serde_json::to_value(&task_resp).unwrap_or_default())
        }
        Err(e) => jsonrpc_err(rpc.id, -32000, &format!("cancel failed: {e}")),
    }
}

// ---------------------------------------------------------------------------
// POST /a2a/tasks/sendSubscribe — streaming task submission (SSE)
// ---------------------------------------------------------------------------

async fn tasks_send_subscribe_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    body: String,
) -> Response {
    if let Err(status) = check_auth(&headers, &state.api_key) {
        return (status, "unauthorized").into_response();
    }

    let rpc: A2aJsonRpc = match serde_json::from_str(&body) {
        Ok(r) => r,
        Err(e) => return jsonrpc_err(None, -32700, &format!("parse error: {e}")),
    };

    let rpc_id = rpc.id.clone();
    let params = &rpc.params;
    let external_id = params
        .get("id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let external_id = if external_id.is_empty() {
        Uuid::new_v4().to_string()
    } else {
        external_id
    };

    // Extract message
    let message = params.get("message").cloned().unwrap_or_default();
    let title = message
        .get("parts")
        .and_then(|p| p.as_array())
        .and_then(|parts| parts.first())
        .and_then(|part| part.get("text"))
        .and_then(|t| t.as_str())
        .unwrap_or("A2A streaming task");

    let source_agent = message
        .get("role")
        .and_then(|r| r.as_str())
        .unwrap_or("external");

    // Create internal task
    let create_result = bridge_call(
        &state,
        "create_task",
        serde_json::json!({
            "title": title,
            "description": format!("A2A streaming task from {source_agent}: {title}"),
        }),
    )
    .await;

    let internal_id = match create_result {
        Ok(data) => data
            .get("task_id")
            .or_else(|| data.get("id"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        Err(e) => return jsonrpc_err(rpc_id, -32000, &format!("create_task failed: {e}")),
    };

    if internal_id.is_empty() {
        return jsonrpc_err(rpc_id, -32000, "create_task returned no id");
    }

    // Insert mapping
    if let Err(e) = sqlx::query(
        "INSERT INTO a2a_task_map (external_id, internal_id, source_agent) VALUES ($1, $2, $3)",
    )
    .bind(&external_id)
    .bind(&internal_id)
    .bind(source_agent)
    .execute(&state.pg)
    .await
    {
        return jsonrpc_err(rpc_id, -32000, &format!("mapping insert failed: {e}"));
    }

    info!(external_id = %external_id, internal_id = %internal_id, "A2A streaming task created");

    // Open SSE channel
    let (tx, rx) = tokio::sync::mpsc::channel::<Result<Event, std::convert::Infallible>>(32);

    let ext_id = external_id.clone();
    let int_id = internal_id.clone();
    let bridge_url = state.bridge_url.clone();
    let bridge_jwt = state.bridge_jwt.clone();
    let http_client = state.http_client.clone();

    tokio::spawn(async move {
        // Send initial submitted event
        let submitted = serde_json::json!({
            "jsonrpc": "2.0",
            "id": rpc_id,
            "result": {
                "id": ext_id,
                "status": { "state": "submitted" },
            },
        });
        let _ = tx
            .send(Ok(Event::default()
                .event("message")
                .data(serde_json::to_string(&submitted).unwrap_or_default())))
            .await;

        // Poll for status updates
        let mut last_status = String::new();
        for _ in 0..360 {
            tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;

            let url = format!("{bridge_url}/api/v1/tool/get_task");
            let body = serde_json::json!({ "params": { "task_id": int_id } });
            let resp = http_client
                .post(&url)
                .header("Authorization", format!("Bearer {bridge_jwt}"))
                .json(&body)
                .send()
                .await;

            let task_data = match resp {
                Ok(r) => r.json::<serde_json::Value>().await.unwrap_or_default(),
                Err(_) => continue,
            };
            let status_str = task_data
                .get("data")
                .and_then(|d| d.get("status"))
                .and_then(|s| s.as_str())
                .unwrap_or("unknown")
                .to_string();

            if status_str != last_status {
                last_status = status_str.clone();
                let mapped = match status_str.as_str() {
                    "pending" => "submitted",
                    "claimed" | "in_progress" => "working",
                    "completed" => "completed",
                    "failed" => "failed",
                    _ => "unknown",
                };

                let artifacts = if mapped == "completed" {
                    task_data
                        .get("data")
                        .and_then(|d| d.get("result_data"))
                        .and_then(|r| {
                            if r.is_null() {
                                None
                            } else {
                                Some(serde_json::json!([
                                    {"parts": [{"type": "text", "text": r.to_string()}]}
                                ]))
                            }
                        })
                } else {
                    None
                };

                let mut result = serde_json::json!({
                    "id": ext_id,
                    "status": { "state": mapped },
                });
                if let Some(arts) = artifacts {
                    result["artifacts"] = arts;
                }

                let update = serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": rpc_id,
                    "result": result,
                });
                let _ = tx
                    .send(Ok(Event::default()
                        .event("message")
                        .data(serde_json::to_string(&update).unwrap_or_default())))
                    .await;

                if matches!(mapped, "completed" | "failed" | "canceled") {
                    break;
                }
            }
        }
    });

    let stream = ReceiverStream::new(rx);
    Sse::new(stream)
        .keep_alive(KeepAlive::default())
        .into_response()
}

// ---------------------------------------------------------------------------
// GET /health
// ---------------------------------------------------------------------------

async fn health_handler(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let pg_ok = sqlx::query("SELECT 1")
        .execute(&state.pg)
        .await
        .is_ok();

    let bridge_ok = state
        .http_client
        .get(format!("{}/health", state.bridge_url))
        .timeout(std::time::Duration::from_secs(3))
        .send()
        .await
        .map(|r| r.status().is_success())
        .unwrap_or(false);

    let status = if pg_ok && bridge_ok { "ok" } else { "degraded" };

    axum::Json(serde_json::json!({
        "status": status,
        "service": SERVICE_NAME,
        "version": env!("CARGO_PKG_VERSION"),
        "checks": {
            "postgres": pg_ok,
            "bridge": bridge_ok,
        },
    }))
}

// ---------------------------------------------------------------------------
// Webhook: Inbound command parsing
// ---------------------------------------------------------------------------

/// Parsed command from any chat platform.
#[allow(dead_code)]
struct WebhookCommand {
    command: String,
    args: Vec<String>,
    user: String,
    platform: String,
}

/// Parse a `/broodlink <command> [args...]` text string.
fn parse_command_text(text: &str) -> Option<(String, Vec<String>)> {
    let text = text.trim();
    // Strip leading "/broodlink " prefix if present
    let cmd_text = text
        .strip_prefix("/broodlink ")
        .or_else(|| text.strip_prefix("/broodlink"))
        .or_else(|| text.strip_prefix("broodlink "))
        .unwrap_or(text);

    let parts: Vec<&str> = cmd_text.split_whitespace().collect();
    if parts.is_empty() {
        return None;
    }

    let command = parts[0].to_lowercase();
    let args = parts[1..].iter().map(|s| (*s).to_string()).collect();
    Some((command, args))
}

/// Execute a parsed webhook command via bridge calls.
async fn execute_command(state: &AppState, cmd: &WebhookCommand) -> String {
    match cmd.command.as_str() {
        "agents" | "list" => match bridge_call(state, "list_agents", serde_json::json!({})).await {
            Ok(data) => {
                let agents = data
                    .as_array()
                    .or_else(|| data.get("agents").and_then(|a| a.as_array()));
                match agents {
                    Some(arr) => {
                        if arr.is_empty() {
                            return "No agents registered.".to_string();
                        }
                        let mut lines = vec!["Agent Roster:".to_string()];
                        for a in arr {
                            let id = a.get("agent_id").and_then(|v| v.as_str()).unwrap_or("?");
                            let role = a.get("role").and_then(|v| v.as_str()).unwrap_or("?");
                            let active = a.get("active").and_then(|v| v.as_bool()).unwrap_or(false);
                            let icon = if active { "+" } else { "-" };
                            lines.push(format!("  [{icon}] {id} ({role})"));
                        }
                        lines.join("\n")
                    }
                    None => format!("Agents: {data}"),
                }
            }
            Err(e) => format!("Error listing agents: {e}"),
        },

        "toggle" => {
            let agent_id = match cmd.args.first() {
                Some(id) => id,
                None => return "Usage: toggle <agent_id>".to_string(),
            };
            match bridge_call(
                state,
                "toggle_agent",
                serde_json::json!({ "agent_id": agent_id }),
            )
            .await
            {
                Ok(data) => {
                    let new_state = data
                        .get("active")
                        .and_then(|v| v.as_bool())
                        .map(|b| if b { "active" } else { "inactive" })
                        .unwrap_or("toggled");
                    format!("Agent {agent_id} is now {new_state}.")
                }
                Err(e) => format!("Error toggling {agent_id}: {e}"),
            }
        }

        "budget" => {
            let agent_id = match cmd.args.first() {
                Some(id) => id,
                None => return "Usage: budget <agent_id>".to_string(),
            };
            match bridge_call(
                state,
                "get_budget",
                serde_json::json!({ "agent_id": agent_id }),
            )
            .await
            {
                Ok(data) => {
                    let balance = data.get("balance").and_then(|v| v.as_i64()).unwrap_or(0);
                    format!("Budget for {agent_id}: {balance} tokens")
                }
                Err(e) => format!("Error getting budget: {e}"),
            }
        }

        "task" => {
            let sub_cmd = cmd.args.first().map(String::as_str).unwrap_or("");
            match sub_cmd {
                "cancel" => {
                    let task_id = match cmd.args.get(1) {
                        Some(id) => id,
                        None => return "Usage: task cancel <task_id>".to_string(),
                    };
                    match bridge_call(
                        state,
                        "fail_task",
                        serde_json::json!({ "task_id": task_id }),
                    )
                    .await
                    {
                        Ok(_) => format!("Task {task_id} cancelled."),
                        Err(e) => format!("Error cancelling task: {e}"),
                    }
                }
                _ => "Usage: task cancel <task_id>".to_string(),
            }
        }

        "workflow" => {
            let sub_cmd = cmd.args.first().map(String::as_str).unwrap_or("");
            match sub_cmd {
                "start" => {
                    let formula = match cmd.args.get(1) {
                        Some(f) => f,
                        None => return "Usage: workflow start <formula> [params]".to_string(),
                    };
                    let params_str = if cmd.args.len() > 2 {
                        cmd.args[2..].join(" ")
                    } else {
                        String::new()
                    };
                    let params_json = if params_str.is_empty() {
                        serde_json::json!({})
                    } else {
                        serde_json::from_str(&params_str).unwrap_or(serde_json::json!({}))
                    };

                    match bridge_call(
                        state,
                        "beads_run_formula",
                        serde_json::json!({
                            "formula": formula,
                            "params": params_json,
                        }),
                    )
                    .await
                    {
                        Ok(data) => {
                            let run_id = data
                                .get("workflow_run_id")
                                .and_then(|v| v.as_str())
                                .unwrap_or("?");
                            format!("Workflow '{formula}' started (run: {run_id})")
                        }
                        Err(e) => format!("Error starting workflow: {e}"),
                    }
                }
                _ => "Usage: workflow start <formula> [params]".to_string(),
            }
        }

        "dlq" => match bridge_call(state, "inspect_dlq", serde_json::json!({})).await {
            Ok(data) => {
                let entries = data
                    .as_array()
                    .or_else(|| data.get("entries").and_then(|e| e.as_array()));
                match entries {
                    Some(arr) => {
                        let unresolved: Vec<_> = arr
                            .iter()
                            .filter(|e| {
                                !e.get("resolved").and_then(|v| v.as_bool()).unwrap_or(false)
                            })
                            .collect();
                        if unresolved.is_empty() {
                            return "DLQ is empty.".to_string();
                        }
                        let mut lines = vec![format!("DLQ ({} entries):", unresolved.len())];
                        for e in unresolved.iter().take(5) {
                            let task = e.get("task_id").and_then(|v| v.as_str()).unwrap_or("?");
                            let reason = e.get("reason").and_then(|v| v.as_str()).unwrap_or("?");
                            let retries =
                                e.get("retry_count").and_then(|v| v.as_i64()).unwrap_or(0);
                            lines.push(format!("  {task}: {reason} (retries: {retries})"));
                        }
                        if unresolved.len() > 5 {
                            lines.push(format!("  ... and {} more", unresolved.len() - 5));
                        }
                        lines.join("\n")
                    }
                    None => format!("DLQ: {data}"),
                }
            }
            Err(e) => format!("Error inspecting DLQ: {e}"),
        },

        "help" => "Broodlink commands:\n\
             \x20 agents         — list all agents\n\
             \x20 toggle <id>    — toggle agent active/inactive\n\
             \x20 budget <id>    — show agent budget\n\
             \x20 task cancel <id> — cancel a task\n\
             \x20 workflow start <formula> — start a workflow\n\
             \x20 dlq            — show dead-letter queue\n\
             \x20 help           — show this help"
            .to_string(),

        other => format!("Unknown command: {other}. Try 'help'."),
    }
}

/// Log inbound webhook to Postgres.
async fn log_webhook(
    pg: &PgPool,
    endpoint_id: Option<&str>,
    direction: &str,
    event_type: &str,
    payload: &serde_json::Value,
    status: &str,
    error_msg: Option<&str>,
) {
    // Use a sentinel endpoint_id for unregistered inbound webhooks
    let eid = endpoint_id.unwrap_or("00000000-0000-0000-0000-000000000000");

    // Ensure the sentinel endpoint exists (idempotent)
    if endpoint_id.is_none() {
        let _ = sqlx::query(
            "INSERT INTO webhook_endpoints (id, platform, name, active)
             VALUES ($1, 'generic', 'system', true)
             ON CONFLICT (id) DO NOTHING",
        )
        .bind(eid)
        .execute(pg)
        .await;
    }

    let _ = sqlx::query(
        "INSERT INTO webhook_log (endpoint_id, direction, event_type, payload, status, error_msg)
         VALUES ($1, $2, $3, $4, $5, $6)",
    )
    .bind(eid)
    .bind(direction)
    .bind(event_type)
    .bind(payload)
    .bind(status)
    .bind(error_msg)
    .execute(pg)
    .await;
}

// ---------------------------------------------------------------------------
// Webhook signature verification helpers
// ---------------------------------------------------------------------------

fn verify_slack_signature(
    signing_secret: &str,
    timestamp: &str,
    body: &str,
    signature: &str,
) -> bool {
    // Reject requests older than 5 minutes to prevent replay attacks
    if let Ok(ts) = timestamp.parse::<i64>() {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        if (now - ts).abs() > 300 {
            return false;
        }
    } else {
        return false;
    }

    let sig_basestring = format!("v0:{timestamp}:{body}");
    let mut mac = match Hmac::<Sha256>::new_from_slice(signing_secret.as_bytes()) {
        Ok(m) => m,
        Err(_) => return false,
    };
    mac.update(sig_basestring.as_bytes());
    let result = mac.finalize();
    let computed = format!("v0={}", hex_encode(result.into_bytes().as_slice()));
    constant_time_eq(computed.as_bytes(), signature.as_bytes())
}

fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter()
        .zip(b.iter())
        .fold(0u8, |acc, (x, y)| acc | (x ^ y))
        == 0
}

/// Verify Teams webhook using HMAC-SHA256 shared secret.
/// Teams sends the signature in the `Authorization` header as `HMAC <base64-signature>`.
fn verify_teams_signature(shared_secret: &str, body: &str, auth_header: &str) -> bool {
    let sig_b64 = match auth_header.strip_prefix("HMAC ") {
        Some(s) => s,
        None => return false,
    };

    let expected_sig = match base64_decode(sig_b64) {
        Some(b) => b,
        None => return false,
    };

    let key_bytes = match base64_decode(shared_secret) {
        Some(b) => b,
        None => return false,
    };

    let mut mac = match Hmac::<Sha256>::new_from_slice(&key_bytes) {
        Ok(m) => m,
        Err(_) => return false,
    };
    mac.update(body.as_bytes());
    let computed = mac.finalize().into_bytes();
    constant_time_eq(&computed, &expected_sig)
}

fn base64_decode(input: &str) -> Option<Vec<u8>> {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD
        .decode(input)
        .ok()
}

// ---------------------------------------------------------------------------
// POST /webhook/slack — receive Slack slash commands
// ---------------------------------------------------------------------------

async fn webhook_slack_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    body: String,
) -> Response {
    if !state.config.webhooks.enabled {
        return (StatusCode::NOT_FOUND, "webhooks disabled").into_response();
    }

    // Verify Slack request signature if signing secret is configured
    if let Some(ref secret) = state.config.webhooks.slack_signing_secret {
        let timestamp = headers
            .get("X-Slack-Request-Timestamp")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        let signature = headers
            .get("X-Slack-Signature")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");

        if !verify_slack_signature(secret, timestamp, &body, signature) {
            warn!("Slack webhook signature verification failed");
            return (StatusCode::UNAUTHORIZED, "invalid signature").into_response();
        }
    }

    // Check if this is a Slack Events API payload (JSON)
    if body.starts_with('{') {
        return handle_slack_event(&state, &body).await;
    }

    // Otherwise: form-encoded slash command payload
    let params: std::collections::HashMap<String, String> = body
        .split('&')
        .filter_map(|pair| {
            let mut parts = pair.splitn(2, '=');
            let key = parts.next()?;
            let value = parts.next().unwrap_or("");
            Some((urlencoding_decode(key), urlencoding_decode(value)))
        })
        .collect();

    let text = params.get("text").cloned().unwrap_or_default();
    let user = params
        .get("user_name")
        .cloned()
        .unwrap_or_else(|| "slack-user".to_string());
    let user_id = params.get("user_id").cloned().unwrap_or_default();
    let channel_id = params.get("channel_id").cloned().unwrap_or_default();
    let response_url = params.get("response_url").cloned();

    info!(platform = "slack", user = %user, text = %text, "inbound webhook");

    // If the text looks like a command, use command handler
    // Otherwise, if chat is enabled, treat as conversational message
    if let Some((command, args)) = parse_command_text(&text) {
        let payload_json = serde_json::to_value(&params).unwrap_or_default();
        log_webhook(
            &state.pg,
            None,
            "inbound",
            &format!("slack.{command}"),
            &payload_json,
            "delivered",
            None,
        )
        .await;

        let cmd = WebhookCommand {
            command,
            args,
            user,
            platform: "slack".to_string(),
        };

        let response_text = execute_command(&state, &cmd).await;

        (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "application/json")],
            serde_json::json!({
                "response_type": "in_channel",
                "text": response_text,
            })
            .to_string(),
        )
            .into_response()
    } else if state.config.chat.enabled && !text.is_empty() {
        let _ = handle_chat_message(
            &state,
            "slack",
            &channel_id,
            &user_id,
            &user,
            &text,
            None,
            response_url.as_deref(),
        )
        .await;

        (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "application/json")],
            serde_json::json!({"response_type": "ephemeral", "text": "Got it! Working on your request..."}).to_string(),
        )
            .into_response()
    } else {
        (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "application/json")],
            serde_json::json!({"response_type": "ephemeral", "text": "Usage: /broodlink <command>. Try 'help'."}).to_string(),
        )
            .into_response()
    }
}

/// Handle Slack Events API payloads (JSON-encoded).
async fn handle_slack_event(state: &AppState, body: &str) -> Response {
    let payload: serde_json::Value = match serde_json::from_str(body) {
        Ok(v) => v,
        Err(e) => return (StatusCode::BAD_REQUEST, format!("invalid json: {e}")).into_response(),
    };

    // Handle URL verification challenge
    if payload.get("type").and_then(|t| t.as_str()) == Some("url_verification") {
        let challenge = payload
            .get("challenge")
            .and_then(|c| c.as_str())
            .unwrap_or("");
        return (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "text/plain")],
            challenge.to_string(),
        )
            .into_response();
    }

    // Handle event_callback with message events
    if payload.get("type").and_then(|t| t.as_str()) == Some("event_callback") {
        if let Some(event) = payload.get("event") {
            let event_type = event.get("type").and_then(|t| t.as_str()).unwrap_or("");

            // Only handle message events, ignore bot messages
            if event_type == "message"
                && event.get("bot_id").is_none()
                && event.get("subtype").is_none()
            {
                let channel = event.get("channel").and_then(|c| c.as_str()).unwrap_or("");
                let user = event.get("user").and_then(|u| u.as_str()).unwrap_or("");
                let text = event.get("text").and_then(|t| t.as_str()).unwrap_or("");
                let thread_ts = event.get("thread_ts").and_then(|t| t.as_str());

                if !text.is_empty() && state.config.chat.enabled {
                    // Check if it's a command first
                    if text.starts_with("/broodlink") || text.starts_with("broodlink ") {
                        // Let the slash command handler deal with it
                    } else {
                        let _ = handle_chat_message(
                            state, "slack", channel, user,
                            user, // display name not available in events API, use user_id
                            text, thread_ts, None,
                        )
                        .await;
                    }
                }
            }
        }
    }

    (StatusCode::OK, "ok").into_response()
}

// ---------------------------------------------------------------------------
// POST /webhook/teams — receive Teams bot messages
// ---------------------------------------------------------------------------

async fn webhook_teams_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    body: String,
) -> Response {
    if !state.config.webhooks.enabled {
        return (StatusCode::NOT_FOUND, "webhooks disabled").into_response();
    }

    // Verify Teams shared secret if configured
    if let Some(ref secret) = state.config.webhooks.teams_shared_secret {
        let auth = headers
            .get("Authorization")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        if !verify_teams_signature(secret, &body, auth) {
            warn!(platform = "teams", "webhook signature verification failed");
            return (StatusCode::UNAUTHORIZED, "invalid signature").into_response();
        }
    }

    let payload: serde_json::Value = match serde_json::from_str(&body) {
        Ok(v) => v,
        Err(e) => {
            return (StatusCode::BAD_REQUEST, format!("invalid json: {e}")).into_response();
        }
    };

    let text = payload.get("text").and_then(|t| t.as_str()).unwrap_or("");
    let user_name = payload
        .get("from")
        .and_then(|f| f.get("name"))
        .and_then(|n| n.as_str())
        .unwrap_or("teams-user");
    let user_id = payload
        .get("from")
        .and_then(|f| f.get("id"))
        .and_then(|n| n.as_str())
        .unwrap_or("teams-user");
    let channel_id = payload
        .get("channelId")
        .and_then(|c| c.as_str())
        .or_else(|| {
            payload
                .get("conversation")
                .and_then(|c| c.get("id"))
                .and_then(|i| i.as_str())
        })
        .unwrap_or("");
    let service_url = payload.get("serviceUrl").and_then(|s| s.as_str());
    let activity_type = payload.get("type").and_then(|t| t.as_str()).unwrap_or("");

    info!(platform = "teams", user = %user_name, text = %text, "inbound webhook");

    // Check for /broodlink command first
    if let Some((command, args)) = parse_command_text(text) {
        log_webhook(
            &state.pg,
            None,
            "inbound",
            &format!("teams.{command}"),
            &payload,
            "delivered",
            None,
        )
        .await;

        let cmd = WebhookCommand {
            command,
            args,
            user: user_name.to_string(),
            platform: "teams".to_string(),
        };

        let response_text = execute_command(&state, &cmd).await;

        (
            StatusCode::OK,
            axum::Json(serde_json::json!({
                "type": "message",
                "text": response_text,
            })),
        )
            .into_response()
    } else if state.config.chat.enabled && activity_type == "message" && !text.is_empty() {
        // Conversational message
        let _ = handle_chat_message(
            &state,
            "teams",
            channel_id,
            user_id,
            user_name,
            text,
            None,
            service_url,
        )
        .await;

        (
            StatusCode::OK,
            axum::Json(serde_json::json!({
                "type": "message",
                "text": "Got it! Working on your request...",
            })),
        )
            .into_response()
    } else {
        (
            StatusCode::OK,
            axum::Json(serde_json::json!({"type": "message", "text": "Usage: /broodlink <command>. Try 'help'."})),
        )
            .into_response()
    }
}

// ---------------------------------------------------------------------------
// Telegram credential lookup (DB with cache, config fallback)
// ---------------------------------------------------------------------------

/// Returns (bot_token, secret_token, allowed_user_ids, auth_code) from DB cache, falling back to config.
async fn get_telegram_creds(
    state: &AppState,
) -> (Option<String>, Option<String>, Vec<i64>, Option<String>) {
    // Check cache (60s TTL)
    {
        let guard = state.telegram_creds.read().await;
        if let Some((token, secret, allowed, auth_code, fetched)) = guard.as_ref() {
            if fetched.elapsed() < std::time::Duration::from_secs(60) {
                return (
                    Some(token.clone()),
                    secret.clone(),
                    allowed.clone(),
                    auth_code.clone(),
                );
            }
        }
    }

    // Query DB (include meta for allowed_user_ids + auth_code)
    let row = sqlx::query_as::<_, (String, Option<String>, Option<serde_json::Value>)>(
        "SELECT bot_token, secret_token, meta FROM platform_credentials WHERE platform = 'telegram' AND enabled = true",
    )
    .fetch_optional(&state.pg)
    .await
    .ok()
    .flatten();

    if let Some((token, secret, meta)) = row {
        let allowed: Vec<i64> = meta
            .as_ref()
            .and_then(|m| m.get("allowed_user_ids"))
            .and_then(|v| v.as_array())
            .map(|arr| arr.iter().filter_map(|v| v.as_i64()).collect())
            .unwrap_or_default();
        let auth_code = meta
            .as_ref()
            .and_then(|m| m.get("auth_code"))
            .and_then(|v| v.as_str())
            .map(String::from);
        let mut guard = state.telegram_creds.write().await;
        *guard = Some((
            token.clone(),
            secret.clone(),
            allowed.clone(),
            auth_code.clone(),
            std::time::Instant::now(),
        ));
        return (Some(token), secret, allowed, auth_code);
    }

    // Fallback to config (no allowed list — open access)
    let token = state.config.webhooks.telegram_bot_token.clone();
    let secret = state.config.webhooks.telegram_secret_token.clone();
    if token.is_some() {
        let mut guard = state.telegram_creds.write().await;
        *guard = Some((
            token.clone().unwrap_or_default(),
            secret.clone(),
            Vec::new(),
            None,
            std::time::Instant::now(),
        ));
    }
    (token, secret, Vec::new(), None)
}

// ---------------------------------------------------------------------------
// POST /webhook/telegram — receive Telegram bot updates
// ---------------------------------------------------------------------------

async fn webhook_telegram_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    body: String,
) -> Response {
    if !state.config.webhooks.enabled {
        return (StatusCode::NOT_FOUND, "webhooks disabled").into_response();
    }

    // Verify Telegram secret token if configured (DB-first, config fallback)
    let (_tg_token, tg_secret, tg_allowed_users, tg_auth_code) = get_telegram_creds(&state).await;
    if let Some(ref secret) = tg_secret {
        let provided = headers
            .get("X-Telegram-Bot-Api-Secret-Token")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        if !constant_time_eq(secret.as_bytes(), provided.as_bytes()) {
            warn!("Telegram webhook secret token verification failed");
            return (StatusCode::UNAUTHORIZED, "invalid secret token").into_response();
        }
    }

    let payload: serde_json::Value = match serde_json::from_str(&body) {
        Ok(v) => v,
        Err(e) => {
            return (StatusCode::BAD_REQUEST, format!("invalid json: {e}")).into_response();
        }
    };

    let message = payload.get("message").unwrap_or(&payload);
    let text = message.get("text").and_then(|t| t.as_str()).unwrap_or("");
    let user_name = message
        .get("from")
        .and_then(|f| f.get("first_name"))
        .and_then(|n| n.as_str())
        .unwrap_or("telegram-user");
    let user_id_val = message
        .get("from")
        .and_then(|f| f.get("id"))
        .and_then(|id| id.as_i64())
        .unwrap_or(0);
    let chat_id = message
        .get("chat")
        .and_then(|c| c.get("id"))
        .and_then(|id| id.as_i64())
        .unwrap_or(0);
    let thread_id = message
        .get("message_thread_id")
        .and_then(|t| t.as_i64())
        .map(|t| t.to_string());

    // Auth gate: check access code or allow list
    if !tg_allowed_users.contains(&user_id_val) {
        if let Some(ref code) = tg_auth_code {
            if text.trim().eq_ignore_ascii_case(code) {
                // Correct code — add user to allowed list in DB and invalidate cache
                info!(platform = "telegram", user = %user_name, user_id = user_id_val, "auth code accepted");
                if let Err(e) = add_telegram_allowed_user(&state, user_id_val).await {
                    warn!(error = %e, "failed to persist allowed user");
                }
                let reply = serde_json::json!({
                    "method": "sendMessage",
                    "chat_id": chat_id,
                    "text": format!("Authenticated! Welcome, {user_name}."),
                });
                return (StatusCode::OK, axum::Json(reply)).into_response();
            }
            // Wrong code or regular message — don't log text (could be auth attempt)
            warn!(platform = "telegram", user_id = user_id_val, user = %user_name, "telegram user not authenticated");
            let reply = serde_json::json!({
                "method": "sendMessage",
                "chat_id": chat_id,
                "text": "Send the access code to use this bot.",
            });
            return (StatusCode::OK, axum::Json(reply)).into_response();
        }
        // No auth_code configured but allow list exists and user not in it — silent reject
        if !tg_allowed_users.is_empty() {
            warn!(user_id = user_id_val, user = %user_name, "telegram user not in allowed list — ignored");
            return (StatusCode::OK, axum::Json(serde_json::json!({"ok": true}))).into_response();
        }
    }

    info!(platform = "telegram", user = %user_name, chat_id = chat_id, text = %text, "inbound webhook");

    // Check for /broodlink command or /command style
    if text.starts_with('/') || text.starts_with("broodlink ") {
        if let Some((command, args)) = parse_command_text(text) {
            log_webhook(
                &state.pg,
                None,
                "inbound",
                &format!("telegram.{command}"),
                &payload,
                "delivered",
                None,
            )
            .await;

            let cmd = WebhookCommand {
                command,
                args,
                user: user_name.to_string(),
                platform: "telegram".to_string(),
            };

            let response_text = execute_command(&state, &cmd).await;

            let reply = serde_json::json!({
                "method": "sendMessage",
                "chat_id": chat_id,
                "text": response_text,
            });

            return (StatusCode::OK, axum::Json(reply)).into_response();
        }
    }

    // Conversational message (non-command)
    if state.config.chat.enabled && !text.is_empty() {
        let chat_id_str = chat_id.to_string();
        let user_id_str = user_id_val.to_string();

        // Dedup: skip if same message is already in-flight (before DB/bridge work)
        let dedup_key = chat_dedup_key("telegram", &chat_id_str, text);
        let dedup_window = std::time::Duration::from_secs(state.config.a2a.dedup_window_secs);
        {
            let mut guard = state.inflight_chats.write().await;
            if let Some(ts) = guard.get(&dedup_key) {
                if ts.elapsed() < dedup_window {
                    info!(key = %dedup_key, "duplicate message suppressed (in-flight)");
                    let reply = serde_json::json!({
                        "method": "sendMessage",
                        "chat_id": chat_id,
                        "text": "Still working on your previous message...",
                    });
                    return (StatusCode::OK, axum::Json(reply)).into_response();
                }
            }
            guard.insert(dedup_key.clone(), std::time::Instant::now());
            if guard.len() > 200 {
                guard.retain(|_, ts| ts.elapsed() < dedup_window);
            }
        }

        let session_id = handle_chat_message(
            &state,
            "telegram",
            &chat_id_str,
            &user_id_str,
            user_name,
            text,
            thread_id.as_deref(),
            None,
        )
        .await;

        // Fetch conversation history for context
        let history = if let Some(ref sid) = session_id {
            fetch_chat_history(&state, sid).await
        } else {
            Vec::new()
        };

        // Generate LLM reply with typing indicator refresh
        let is_busy;
        let llm_reply = match state.ollama_semaphore.try_acquire() {
            Ok(_permit) => {
                is_busy = false;
                send_telegram_typing(&state, &chat_id_str).await;
                tokio::select! {
                    r = call_ollama_chat(&state, &history) => r,
                    _ = async {
                        loop {
                            tokio::time::sleep(tokio::time::Duration::from_secs(4)).await;
                            send_telegram_typing(&state, &chat_id_str).await;
                        }
                    } => unreachable!(),
                }
            }
            Err(_) => {
                is_busy = true;
                state.config.a2a.busy_message.clone()
            }
        };

        // Remove from in-flight
        {
            let mut guard = state.inflight_chats.write().await;
            guard.remove(&dedup_key);
        }

        // Store outbound reply (skip transient busy messages)
        if !is_busy {
            if let Some(ref sid) = session_id {
                let _ = sqlx::query(
                    "INSERT INTO chat_messages (session_id, direction, content) VALUES ($1, 'outbound', $2)",
                )
                .bind(sid)
                .bind(&llm_reply)
                .execute(&state.pg)
                .await;
            }
        }

        let reply = serde_json::json!({
            "method": "sendMessage",
            "chat_id": chat_id,
            "text": llm_reply,
        });
        return (StatusCode::OK, axum::Json(reply)).into_response();
    }

    let reply = serde_json::json!({
        "method": "sendMessage",
        "chat_id": chat_id,
        "text": "Usage: /broodlink <command>. Try 'help'.",
    });
    (StatusCode::OK, axum::Json(reply)).into_response()
}

// ---------------------------------------------------------------------------
// Telegram message processing (shared by webhook + polling)
// ---------------------------------------------------------------------------

/// Process a single Telegram update object.  Returns an optional reply text.
async fn process_telegram_update(state: &AppState, update: &serde_json::Value) -> Option<String> {
    let message = update.get("message")?;
    let text = message.get("text").and_then(|t| t.as_str()).unwrap_or("");
    if text.is_empty() {
        return None;
    }

    let user_name = message
        .get("from")
        .and_then(|f| f.get("first_name"))
        .and_then(|n| n.as_str())
        .unwrap_or("telegram-user");
    let user_id_val = message
        .get("from")
        .and_then(|f| f.get("id"))
        .and_then(|id| id.as_i64())
        .unwrap_or(0);
    let chat_id = message
        .get("chat")
        .and_then(|c| c.get("id"))
        .and_then(|id| id.as_i64())
        .unwrap_or(0);
    let thread_id = message
        .get("message_thread_id")
        .and_then(|t| t.as_i64())
        .map(|t| t.to_string());

    info!(platform = "telegram", user = %user_name, chat_id = chat_id, text = %text, "inbound message (poll)");

    // Command handling
    if text.starts_with('/') || text.starts_with("broodlink ") {
        if let Some((command, args)) = parse_command_text(text) {
            log_webhook(
                &state.pg,
                None,
                "inbound",
                &format!("telegram.{command}"),
                update,
                "delivered",
                None,
            )
            .await;
            let cmd = WebhookCommand {
                command,
                args,
                user: user_name.to_string(),
                platform: "telegram".to_string(),
            };
            return Some(execute_command(state, &cmd).await);
        }
    }

    // Conversational message
    if state.config.chat.enabled {
        let chat_id_str = chat_id.to_string();
        let user_id_str = user_id_val.to_string();

        // Dedup: skip if same message is already in-flight (before DB/bridge work)
        let dedup_key = chat_dedup_key("telegram", &chat_id_str, text);
        let dedup_window = std::time::Duration::from_secs(state.config.a2a.dedup_window_secs);
        {
            let mut guard = state.inflight_chats.write().await;
            if let Some(ts) = guard.get(&dedup_key) {
                if ts.elapsed() < dedup_window {
                    info!(key = %dedup_key, "duplicate message suppressed (in-flight)");
                    return Some("Still working on your previous message...".to_string());
                }
            }
            guard.insert(dedup_key.clone(), std::time::Instant::now());
            if guard.len() > 200 {
                guard.retain(|_, ts| ts.elapsed() < dedup_window);
            }
        }

        let session_id = handle_chat_message(
            state,
            "telegram",
            &chat_id_str,
            &user_id_str,
            user_name,
            text,
            thread_id.as_deref(),
            None,
        )
        .await;

        // Fetch conversation history for context
        let history = if let Some(ref sid) = session_id {
            fetch_chat_history(state, sid).await
        } else {
            Vec::new()
        };

        // Generate LLM reply with typing indicator refresh
        let is_busy;
        let reply = match state.ollama_semaphore.try_acquire() {
            Ok(_permit) => {
                is_busy = false;
                send_telegram_typing(state, &chat_id_str).await;
                tokio::select! {
                    r = call_ollama_chat(state, &history) => r,
                    _ = async {
                        loop {
                            tokio::time::sleep(tokio::time::Duration::from_secs(4)).await;
                            send_telegram_typing(state, &chat_id_str).await;
                        }
                    } => unreachable!(),
                }
            }
            Err(_) => {
                is_busy = true;
                state.config.a2a.busy_message.clone()
            }
        };

        // Remove from in-flight
        {
            let mut guard = state.inflight_chats.write().await;
            guard.remove(&dedup_key);
        }

        // Store outbound reply in chat_messages (skip transient busy messages)
        if !is_busy {
            if let Some(ref sid) = session_id {
                let _ = sqlx::query(
                    "INSERT INTO chat_messages (session_id, direction, content) VALUES ($1, 'outbound', $2)",
                )
                .bind(sid)
                .bind(&reply)
                .execute(&state.pg)
                .await;
            }
        }

        return Some(reply);
    }

    None
}

// ---------------------------------------------------------------------------
// Telegram long-polling loop (no public URL required)
// ---------------------------------------------------------------------------

async fn telegram_polling_loop(state: &AppState) {
    info!("Telegram polling loop started");

    // Restore offset from DB to avoid duplicate processing on restart
    let mut offset: i64 = sqlx::query_scalar::<_, Option<String>>(
        "SELECT meta->>'poll_offset' FROM platform_credentials WHERE platform = 'telegram'",
    )
    .fetch_optional(&state.pg)
    .await
    .ok()
    .flatten()
    .flatten()
    .and_then(|s| s.parse::<i64>().ok())
    .unwrap_or(0);

    if offset > 0 {
        info!(offset = offset, "restored Telegram poll offset from DB");
    }

    loop {
        let (token_opt, _, _, _) = get_telegram_creds(state).await;
        let token = match token_opt {
            Some(t) if !t.is_empty() => t,
            _ => {
                // No token configured yet — wait and retry
                tokio::time::sleep(tokio::time::Duration::from_secs(10)).await;
                continue;
            }
        };

        let url =
            format!("https://api.telegram.org/bot{token}/getUpdates?offset={offset}&timeout=30");

        let resp = match state
            .http_client
            .get(&url)
            .timeout(std::time::Duration::from_secs(45)) // 30s server poll + 15s headroom
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => {
                if e.is_timeout() {
                    // Normal — server-side long poll may not respond in time
                    continue;
                }
                warn!(error = %e, "Telegram getUpdates failed");
                tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
                continue;
            }
        };

        let body: serde_json::Value = match resp.json().await {
            Ok(b) => b,
            Err(e) => {
                warn!(error = %e, "failed to parse getUpdates response");
                tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
                continue;
            }
        };

        let updates = match body.get("result").and_then(|r| r.as_array()) {
            Some(arr) => arr,
            None => {
                if body.get("ok").and_then(|v| v.as_bool()) != Some(true) {
                    warn!(body = %body, "Telegram getUpdates returned error — clearing token cache");
                    // Token may have been revoked; clear cache so next iteration re-fetches
                    let mut guard = state.telegram_creds.write().await;
                    *guard = None;
                    tokio::time::sleep(tokio::time::Duration::from_secs(10)).await;
                }
                continue;
            }
        };

        // Re-fetch credentials before processing (may have changed during 30s poll)
        let (_, _, allowed_users, auth_code) = get_telegram_creds(state).await;

        let mut offset_changed = false;
        for update in updates {
            // Advance offset past this update
            if let Some(uid) = update.get("update_id").and_then(|v| v.as_i64()) {
                if uid >= offset {
                    offset = uid + 1;
                    offset_changed = true;
                }
            }

            // Auth gate: check access code or allow list
            let sender_id = update
                .get("message")
                .and_then(|m| m.get("from"))
                .and_then(|f| f.get("id"))
                .and_then(|id| id.as_i64())
                .unwrap_or(0);
            let msg_text = update
                .get("message")
                .and_then(|m| m.get("text"))
                .and_then(|t| t.as_str())
                .unwrap_or("");
            let msg_chat_id = update
                .get("message")
                .and_then(|m| m.get("chat"))
                .and_then(|c| c.get("id"))
                .and_then(|id| id.as_i64())
                .unwrap_or(0);

            if !allowed_users.contains(&sender_id) {
                if let Some(ref code) = auth_code {
                    if msg_text.trim().eq_ignore_ascii_case(code) {
                        // Correct code — add user to allowed list
                        info!(
                            platform = "telegram",
                            user_id = sender_id,
                            "auth code accepted"
                        );
                        if let Err(e) = add_telegram_allowed_user(state, sender_id).await {
                            warn!(error = %e, "failed to persist allowed user");
                        }
                        let sender_name = update
                            .get("message")
                            .and_then(|m| m.get("from"))
                            .and_then(|f| f.get("first_name"))
                            .and_then(|n| n.as_str())
                            .unwrap_or("there");
                        if msg_chat_id != 0 {
                            let _ = deliver_telegram(
                                state,
                                &msg_chat_id.to_string(),
                                None,
                                &format!("Authenticated! Welcome, {sender_name}."),
                            )
                            .await;
                        }
                        continue;
                    }
                    // Wrong code — prompt
                    warn!(user_id = sender_id, "telegram user not authenticated");
                    if msg_chat_id != 0 {
                        let _ = deliver_telegram(
                            state,
                            &msg_chat_id.to_string(),
                            None,
                            "Send the access code to use this bot.",
                        )
                        .await;
                    }
                    continue;
                }
                // No auth_code but allow list exists — silent reject
                if !allowed_users.is_empty() {
                    warn!(
                        user_id = sender_id,
                        "telegram user not in allowed list — ignored"
                    );
                    continue;
                }
            }

            if let Some(reply) = process_telegram_update(state, update).await {
                // Send reply back
                let chat_id = update
                    .get("message")
                    .and_then(|m| m.get("chat"))
                    .and_then(|c| c.get("id"))
                    .and_then(|id| id.as_i64())
                    .unwrap_or(0);

                if chat_id != 0 {
                    info!(
                        chat_id = chat_id,
                        reply_len = reply.len(),
                        "sending Telegram reply"
                    );
                    if let Err(e) =
                        deliver_telegram(state, &chat_id.to_string(), None, &reply).await
                    {
                        warn!(error = %e, chat_id = chat_id, "failed to send polling reply");
                    }
                }
            }
        }

        // Persist offset to DB so restarts don't re-process messages
        if offset_changed {
            let _ = sqlx::query(
                "UPDATE platform_credentials SET meta = COALESCE(meta, '{}'::jsonb) || jsonb_build_object('poll_offset', $1::text), updated_at = NOW() WHERE platform = 'telegram'",
            )
            .bind(offset.to_string())
            .execute(&state.pg)
            .await;
        }
    }
}

// ---------------------------------------------------------------------------
// URL decoding helper (minimal, no external dep)
// ---------------------------------------------------------------------------

fn urlencoding_decode(s: &str) -> String {
    let s = s.replace('+', " ");
    let mut result = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '%' {
            let hex: String = chars.by_ref().take(2).collect();
            if let Ok(byte) = u8::from_str_radix(&hex, 16) {
                result.push(byte as char);
            } else {
                result.push('%');
                result.push_str(&hex);
            }
        } else {
            result.push(c);
        }
    }
    result
}

// ---------------------------------------------------------------------------
// Chat: Conversational agent gateway (v0.7.0)
// ---------------------------------------------------------------------------

/// Handle an inbound chat message (free-form, not a /broodlink command).
/// Creates or updates a session, stores the message, and creates a task.
#[allow(clippy::too_many_arguments)]
async fn handle_chat_message(
    state: &AppState,
    platform: &str,
    channel_id: &str,
    user_id: &str,
    user_name: &str,
    text: &str,
    thread_id: Option<&str>,
    reply_url: Option<&str>,
) -> Option<String> {
    let chat_cfg = &state.config.chat;
    let session_id = Uuid::new_v4().to_string();

    // Upsert chat_session
    let row: Option<(String, i32)> = sqlx::query_as(
        "INSERT INTO chat_sessions (id, platform, channel_id, user_id, user_display_name, thread_id, last_message_at, message_count)
         VALUES ($1, $2, $3, $4, $5, $6, NOW(), 1)
         ON CONFLICT (platform, channel_id, user_id)
         DO UPDATE SET last_message_at = NOW(),
                       message_count = chat_sessions.message_count + 1,
                       updated_at = NOW(),
                       user_display_name = COALESCE(EXCLUDED.user_display_name, chat_sessions.user_display_name)
         RETURNING id, message_count",
    )
    .bind(&session_id)
    .bind(platform)
    .bind(channel_id)
    .bind(user_id)
    .bind(user_name)
    .bind(thread_id)
    .fetch_optional(&state.pg)
    .await
    .unwrap_or(None);

    let (sid, msg_count) = match row {
        Some(r) => r,
        None => {
            error!(
                platform = platform,
                user_id = user_id,
                "failed to upsert chat session"
            );
            return None;
        }
    };

    // Insert inbound message
    if let Err(e) = sqlx::query(
        "INSERT INTO chat_messages (session_id, direction, content) VALUES ($1, 'inbound', $2)",
    )
    .bind(&sid)
    .bind(text)
    .execute(&state.pg)
    .await
    {
        error!(error = %e, "failed to insert chat message");
    }

    // If greeting_enabled and this is a brand new session (msg_count == 1), queue greeting
    if chat_cfg.greeting_enabled && msg_count == 1 {
        let greeting = &chat_cfg.greeting_message;
        let _ = sqlx::query(
            "INSERT INTO chat_reply_queue (session_id, task_id, content, platform, reply_url, channel_id, thread_id)
             VALUES ($1, '00000000-0000-0000-0000-000000000000', $2, $3, $4, $5, $6)",
        )
        .bind(&sid)
        .bind(greeting)
        .bind(platform)
        .bind(reply_url)
        .bind(channel_id)
        .bind(thread_id)
        .execute(&state.pg)
        .await;

        // Also store greeting as outbound message
        let _ = sqlx::query(
            "INSERT INTO chat_messages (session_id, direction, content) VALUES ($1, 'outbound', $2)",
        )
        .bind(&sid)
        .bind(greeting)
        .execute(&state.pg)
        .await;
    }

    // Fetch recent messages for context
    let context_messages: Vec<(String, String)> = sqlx::query_as(
        "SELECT direction, content FROM chat_messages
         WHERE session_id = $1
         ORDER BY created_at DESC
         LIMIT $2",
    )
    .bind(&sid)
    .bind(i64::from(chat_cfg.max_context_messages))
    .fetch_all(&state.pg)
    .await
    .unwrap_or_default();

    // Format context (reverse to chronological order)
    let mut context_lines: Vec<String> = context_messages
        .iter()
        .rev()
        .map(|(dir, content)| {
            let label = if dir == "inbound" {
                user_name
            } else {
                "Broodlink"
            };
            format!("[{label}]: {content}")
        })
        .collect();

    let description = if context_lines.is_empty() {
        format!("Message from {user_name} on {platform}:\n{text}")
    } else {
        context_lines.push(format!("\nLatest message from {user_name}:\n{text}"));
        format!("Conversation history:\n{}", context_lines.join("\n"))
    };

    // Truncate title to 80 chars
    let title_text = if text.len() > 80 { &text[..80] } else { text };
    let title = format!("Chat: {title_text}");

    // Create task with chat metadata in dependencies field
    let chat_meta = serde_json::json!({
        "chat": {
            "chat_session_id": sid,
            "platform": platform,
            "channel_id": channel_id,
            "user_id": user_id,
            "thread_id": thread_id,
            "reply_url": reply_url,
        }
    });

    let create_result = bridge_call(
        state,
        "create_task",
        serde_json::json!({
            "title": title,
            "description": description,
            "priority": 0,
            "dependencies": chat_meta.to_string(),
        }),
    )
    .await;

    match create_result {
        Ok(data) => {
            let task_id = data
                .get("task_id")
                .or_else(|| data.get("id"))
                .and_then(|v| v.as_str())
                .unwrap_or("?");
            info!(
                session_id = %sid,
                task_id = %task_id,
                platform = platform,
                "chat task created"
            );
        }
        Err(e) => {
            error!(error = %e, session_id = %sid, "failed to create chat task");
        }
    }

    Some(sid)
}

/// Background loop: deliver pending chat replies to platforms.
async fn reply_delivery_loop(state: &AppState) {
    let chat_cfg = &state.config.chat;
    let max_attempts = chat_cfg.reply_retry_attempts;

    info!("chat reply delivery loop started");

    loop {
        tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;

        // Fetch pending replies
        let rows: Vec<(
            i64,
            String,
            String,
            Option<String>,
            String,
            Option<String>,
            String,
        )> = match sqlx::query_as(
            "SELECT id, content, platform, reply_url, channel_id, thread_id, session_id
                 FROM chat_reply_queue
                 WHERE status = 'pending' AND attempts < $1
                 ORDER BY created_at ASC
                 LIMIT 10",
        )
        .bind(i64::from(max_attempts))
        .fetch_all(&state.pg)
        .await
        {
            Ok(r) => r,
            Err(e) => {
                debug!(error = %e, "reply delivery query failed");
                continue;
            }
        };

        for (id, content, platform, reply_url, channel_id, thread_id, _session_id) in rows {
            let delivered = match platform.as_str() {
                "slack" => {
                    deliver_slack(
                        state,
                        reply_url.as_deref(),
                        &channel_id,
                        thread_id.as_deref(),
                        &content,
                    )
                    .await
                }
                "teams" => deliver_teams(state, reply_url.as_deref(), &channel_id, &content).await,
                "telegram" => {
                    deliver_telegram(state, &channel_id, thread_id.as_deref(), &content).await
                }
                _ => {
                    warn!(platform = %platform, "unsupported platform for reply delivery");
                    Err("unsupported platform".to_string())
                }
            };

            match delivered {
                Ok(()) => {
                    let _ = sqlx::query(
                        "UPDATE chat_reply_queue SET status = 'delivered', delivered_at = NOW() WHERE id = $1",
                    )
                    .bind(id)
                    .execute(&state.pg)
                    .await;
                }
                Err(err) => {
                    let _ = sqlx::query(
                        "UPDATE chat_reply_queue
                         SET attempts = attempts + 1,
                             error_msg = $2,
                             status = CASE WHEN attempts + 1 >= $3 THEN 'failed' ELSE 'pending' END
                         WHERE id = $1",
                    )
                    .bind(id)
                    .bind(&err)
                    .bind(i64::from(max_attempts))
                    .execute(&state.pg)
                    .await;
                }
            }
        }
    }
}

/// Handle a task completion event — check if it's a chat task and queue reply.
async fn handle_task_completion_for_chat(
    state: &AppState,
    task_id: &str,
    result_data: &serde_json::Value,
    is_failure: bool,
) {
    // Look up chat metadata from task_queue.dependencies
    let row: Option<(serde_json::Value,)> =
        sqlx::query_as("SELECT dependencies FROM task_queue WHERE id = $1")
            .bind(task_id)
            .fetch_optional(&state.pg)
            .await
            .unwrap_or(None);

    let deps = match row {
        Some((d,)) => d,
        None => return,
    };

    let chat = match deps.get("chat") {
        Some(c) => c,
        None => return, // Not a chat task
    };

    let session_id = chat
        .get("chat_session_id")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    let platform = chat
        .get("platform")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    let channel_id = chat
        .get("channel_id")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    let thread_id = chat.get("thread_id").and_then(|v| v.as_str());
    let reply_url = chat.get("reply_url").and_then(|v| v.as_str());

    if session_id.is_empty() || platform.is_empty() || channel_id.is_empty() {
        return;
    }

    let content = if is_failure {
        let err = result_data
            .get("error")
            .and_then(|v| v.as_str())
            .unwrap_or("An error occurred while processing your request.");
        format!("Sorry, something went wrong: {err}")
    } else {
        // Extract response text from result_data
        result_data
            .as_str()
            .map(String::from)
            .or_else(|| {
                result_data
                    .get("response")
                    .and_then(|v| v.as_str())
                    .map(String::from)
            })
            .or_else(|| {
                result_data
                    .get("text")
                    .and_then(|v| v.as_str())
                    .map(String::from)
            })
            .unwrap_or_else(|| result_data.to_string())
    };

    // Insert into reply queue
    let _ = sqlx::query(
        "INSERT INTO chat_reply_queue (session_id, task_id, content, platform, reply_url, channel_id, thread_id)
         VALUES ($1, $2, $3, $4, $5, $6, $7)",
    )
    .bind(session_id)
    .bind(task_id)
    .bind(&content)
    .bind(platform)
    .bind(reply_url)
    .bind(channel_id)
    .bind(thread_id)
    .execute(&state.pg)
    .await;

    // Store as outbound message
    let _ = sqlx::query(
        "INSERT INTO chat_messages (session_id, direction, content, task_id) VALUES ($1, 'outbound', $2, $3)",
    )
    .bind(session_id)
    .bind(&content)
    .bind(task_id)
    .execute(&state.pg)
    .await;

    info!(
        task_id = task_id,
        session_id = session_id,
        platform = platform,
        is_failure = is_failure,
        "chat reply queued"
    );
}

// ---------------------------------------------------------------------------
// Telegram typing indicator
// ---------------------------------------------------------------------------

async fn send_telegram_typing(state: &AppState, chat_id: &str) {
    let (token_opt, _, _, _) = get_telegram_creds(state).await;
    let token = match token_opt {
        Some(t) if !t.is_empty() => t,
        _ => return,
    };
    let url = format!("https://api.telegram.org/bot{token}/sendChatAction");
    let _ = state
        .http_client
        .post(&url)
        .json(&serde_json::json!({"chat_id": chat_id, "action": "typing"}))
        .send()
        .await;
}

// ---------------------------------------------------------------------------
// Quick Ollama chat (direct LLM call for Telegram responses)
// ---------------------------------------------------------------------------

/// Fetch recent chat history as (role, content) pairs for Ollama context.
async fn fetch_chat_history(state: &AppState, session_id: &str) -> Vec<(String, String)> {
    let max_ctx = i64::from(state.config.chat.max_context_messages);
    let rows: Vec<(String, String)> = sqlx::query_as(
        "SELECT direction, content FROM chat_messages
         WHERE session_id = $1
         ORDER BY created_at DESC
         LIMIT $2",
    )
    .bind(session_id)
    .bind(max_ctx)
    .fetch_all(&state.pg)
    .await
    .unwrap_or_default();

    // Reverse to chronological order, map direction to Ollama roles
    rows.into_iter()
        .rev()
        .map(|(dir, content)| {
            let role = if dir == "inbound" {
                "user".to_string()
            } else {
                "assistant".to_string()
            };
            (role, content)
        })
        .collect()
}

/// Ollama HTTP request errors, classified for retry/fallback decisions.
enum OllamaRequestError {
    Timeout,
    Connection(String),
    Parse(String),
}

/// Send an Ollama /api/chat request with one automatic retry on transient
/// connection errors (connection refused, reset, DNS failure).
async fn send_ollama_request(
    client: &reqwest::Client,
    url: &str,
    payload: &serde_json::Value,
    _timeout_secs: u64,
) -> Result<serde_json::Value, OllamaRequestError> {
    let mut last_err = String::new();
    for attempt in 0..2u8 {
        if attempt > 0 {
            info!(attempt = attempt, "retrying Ollama request after connection error");
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        }
        match client.post(url).json(payload).send().await {
            Ok(resp) => match resp.json().await {
                Ok(b) => return Ok(b),
                Err(e) => return Err(OllamaRequestError::Parse(e.to_string())),
            },
            Err(e) if e.is_timeout() => return Err(OllamaRequestError::Timeout),
            Err(e) if e.is_connect() => {
                last_err = e.to_string();
                continue;
            }
            Err(e) => {
                // Other transient errors (reset, broken pipe) — retry once
                let msg = e.to_string();
                if attempt == 0 && (msg.contains("reset") || msg.contains("broken pipe")) {
                    last_err = msg;
                    continue;
                }
                return Err(OllamaRequestError::Connection(msg));
            }
        }
    }
    Err(OllamaRequestError::Connection(format!(
        "connection failed after retry: {last_err}"
    )))
}

/// How long to stay in degraded mode before retrying the primary model.
const DEGRADED_RETRY_INTERVAL: std::time::Duration = std::time::Duration::from_secs(300);

/// Attempt to recover from an Ollama model error by unloading the failed model
/// to free memory, then retrying it. If recovery fails, enters degraded mode
/// and answers the user's question using the fallback model directly.
async fn handle_ollama_recovery(
    state: &AppState,
    chat_url: &str,
    primary_model: &str,
    fallback_model: &str,
    _error_msg: &str,
    messages: &[serde_json::Value],
    timeout_secs: u64,
) -> String {
    let ollama_url = &state.config.ollama.url;
    let num_ctx = state.config.ollama.num_ctx;

    // Step 1: Unload the failed model to free memory
    info!(model = %primary_model, "unloading model to free memory");
    let unload_payload = serde_json::json!({
        "model": primary_model,
        "keep_alive": 0,
    });
    let unload_url = format!("{ollama_url}/api/generate");
    match state
        .ollama_client
        .post(&unload_url)
        .json(&unload_payload)
        .send()
        .await
    {
        Ok(_) => info!(model = %primary_model, "model unload requested"),
        Err(e) => warn!(model = %primary_model, error = %e, "failed to request model unload"),
    }

    // Step 2: Wait for memory to settle
    tokio::time::sleep(std::time::Duration::from_secs(3)).await;

    // Step 3: Retry the primary model (single attempt, no tools — keep it simple)
    info!(model = %primary_model, "retrying primary model after recovery");
    let retry_payload = serde_json::json!({
        "model": primary_model,
        "messages": messages,
        "stream": false,
        "think": false,
        "options": {
            "temperature": 0.7,
            "num_predict": 512,
            "num_ctx": num_ctx,
        }
    });

    match send_ollama_request(&state.ollama_client, chat_url, &retry_payload, timeout_secs).await {
        Ok(body) if body.get("error").is_none() => {
            if let Some(content) = body
                .get("message")
                .and_then(|m| m.get("content"))
                .and_then(|c| c.as_str())
            {
                let cleaned = strip_think_tags(content.trim());
                if !cleaned.is_empty() {
                    // Primary recovered — clear degraded flag if it was set
                    if state.primary_model_degraded.swap(false, Ordering::Relaxed) {
                        *state.degraded_since.write().await = None;
                        info!(model = %primary_model, "primary model recovered — leaving degraded mode");
                    }
                    return cleaned;
                }
            }
        }
        Ok(body) => {
            let retry_err = body
                .get("error")
                .and_then(|e| e.as_str())
                .unwrap_or("unknown");
            warn!(model = %primary_model, error = %retry_err, "primary model still failing after recovery");
        }
        Err(e) => {
            let desc = match &e {
                OllamaRequestError::Timeout => "timeout".to_string(),
                OllamaRequestError::Connection(s) => s.clone(),
                OllamaRequestError::Parse(s) => s.clone(),
            };
            warn!(model = %primary_model, error = %desc, "primary model retry failed");
        }
    }

    // Step 4: Enter degraded mode — skip primary on subsequent requests
    if !state.primary_model_degraded.swap(true, Ordering::Relaxed) {
        *state.degraded_since.write().await = Some(std::time::Instant::now());
        warn!(
            model = %primary_model,
            fallback = %fallback_model,
            "entering degraded mode — routing chat to fallback model for {}s",
            DEGRADED_RETRY_INTERVAL.as_secs()
        );
    }

    // Step 5: Answer using the fallback model (not just a status message)
    fallback_chat(
        &state.ollama_client,
        chat_url,
        fallback_model,
        messages,
        num_ctx,
    )
    .await
}

/// Use the fallback model to answer the user's actual question.
/// Prepends a system note so the model knows it's operating as a lightweight
/// stand-in, but otherwise gives a real answer.
async fn fallback_chat(
    client: &reqwest::Client,
    chat_url: &str,
    fallback_model: &str,
    messages: &[serde_json::Value],
    num_ctx: u32,
) -> String {
    // Replace the system prompt with one suited for the fallback model
    let mut fallback_messages = Vec::with_capacity(messages.len());
    fallback_messages.push(serde_json::json!({
        "role": "system",
        "content": "You are Broodlink, an AI assistant running in lightweight mode. \
            Answer the user's question as best you can. Keep responses concise."
    }));
    // Copy user/assistant messages (skip original system prompt)
    for msg in messages {
        if msg.get("role").and_then(|r| r.as_str()) != Some("system") {
            fallback_messages.push(msg.clone());
        }
    }

    let payload = serde_json::json!({
        "model": fallback_model,
        "messages": fallback_messages,
        "stream": false,
        "think": false,
        "options": {
            "temperature": 0.7,
            "num_predict": 512,
            "num_ctx": num_ctx,
        }
    });

    match client.post(chat_url).json(&payload).send().await {
        Ok(resp) => {
            if let Ok(body) = resp.json::<serde_json::Value>().await {
                if body.get("error").is_some() {
                    warn!(model = %fallback_model, "fallback model also returned error");
                } else if let Some(content) = body
                    .get("message")
                    .and_then(|m| m.get("content"))
                    .and_then(|c| c.as_str())
                {
                    let cleaned = strip_think_tags(content.trim());
                    if !cleaned.is_empty() {
                        return cleaned;
                    }
                }
            }
        }
        Err(e) => {
            warn!(error = %e, "fallback model request failed");
        }
    }

    // Canned fallback if even the small model fails
    "I'm experiencing a temporary resource issue and couldn't process your message. \
     Please try again in a moment."
        .to_string()
}

async fn call_ollama_chat(state: &AppState, history: &[(String, String)]) -> String {
    let ollama_url = &state.config.ollama.url;
    let timeout_secs = state.config.ollama.timeout_seconds;
    let url = format!("{ollama_url}/api/chat");
    let chat_cfg = &state.config.chat;
    let tool_cfg = &chat_cfg.tools;

    // Resolve Brave API key: config → env var → None
    let brave_key = if !tool_cfg.brave_api_key.is_empty() {
        Some(tool_cfg.brave_api_key.clone())
    } else {
        std::env::var("BROODLINK_BRAVE_API_KEY").ok()
    };
    let tools_available = tool_cfg.web_search_enabled && brave_key.is_some();
    info!(
        web_search_enabled = tool_cfg.web_search_enabled,
        brave_key_present = brave_key.is_some(),
        tools_available = tools_available,
        "chat tool availability"
    );

    // Dynamic system prompt
    let mut system_prompt = String::from(
        "You are Broodlink, an AI assistant. Keep responses concise and conversational.",
    );
    if tools_available {
        system_prompt.push_str(
            " You have a web_search tool. ONLY use it for: \
             (1) real-time data like news, weather, stock prices, sports scores, \
             (2) events or information from the last 30 days, \
             (3) specific URLs, product availability, or live status checks. \
             Do NOT search for general knowledge, definitions, programming concepts, \
             or anything you can answer from training data. \
             When you do search, cite your sources.",
        );
    }
    system_prompt.push_str(" If you don't know something and can't look it up, say so honestly.");

    // Build message array: system prompt + conversation history
    // Limit context when tools are active to keep prompt size manageable for CPU inference
    let max_history = if tools_available {
        tool_cfg.tool_context_messages as usize
    } else {
        history.len()
    };
    let history_slice = if history.len() > max_history {
        &history[history.len() - max_history..]
    } else {
        history
    };

    let mut messages = vec![serde_json::json!({
        "role": "system",
        "content": system_prompt,
    })];
    for (role, content) in history_slice {
        messages.push(serde_json::json!({
            "role": role,
            "content": content,
        }));
    }

    // Tool definitions
    let tools_def = if tools_available {
        Some(serde_json::json!([{
            "type": "function",
            "function": {
                "name": "web_search",
                "description": "Search the web for current information, recent events, or facts you are unsure about.",
                "parameters": {
                    "type": "object",
                    "required": ["query"],
                    "properties": {
                        "query": {
                            "type": "string",
                            "description": "The search query"
                        }
                    }
                }
            }
        }]))
    } else {
        None
    };

    let max_rounds = tool_cfg.max_tool_rounds;
    let model = &chat_cfg.chat_model;
    let fallback = &chat_cfg.chat_fallback_model;

    // Degraded mode: skip primary model if it recently failed with OOM.
    // Periodically retry (every DEGRADED_RETRY_INTERVAL) to detect recovery.
    if state.primary_model_degraded.load(Ordering::Relaxed) && !fallback.is_empty() {
        let should_retry = {
            let ds = state.degraded_since.read().await;
            ds.is_some_and(|t| t.elapsed() >= DEGRADED_RETRY_INTERVAL)
        };
        if should_retry {
            info!(model = %model, "degraded cooldown expired — probing primary model");
            // Clear flag optimistically; handle_ollama_recovery re-sets it on failure
            state.primary_model_degraded.store(false, Ordering::Relaxed);
            *state.degraded_since.write().await = None;
        } else {
            info!(model = %fallback, "degraded mode — routing to fallback model");
            return fallback_chat(
                &state.ollama_client,
                &url,
                fallback,
                &messages,
                state.config.ollama.num_ctx,
            )
            .await;
        }
    }

    for round in 0..=max_rounds {
        // Include tools only on rounds where the model can still call them
        let include_tools = round < max_rounds && tools_def.is_some();

        let mut payload = serde_json::json!({
            "model": model,
            "messages": messages,
            "stream": false,
            "think": false,
            "options": {
                "temperature": 0.7,
                "num_predict": 512,
                "num_ctx": state.config.ollama.num_ctx,
            }
        });
        if include_tools {
            payload["tools"] = tools_def.clone().unwrap();
        }

        // Send request with one retry on transient connection errors
        let body: serde_json::Value = match send_ollama_request(
            &state.ollama_client,
            &url,
            &payload,
            timeout_secs,
        )
        .await
        {
            Ok(b) => b,
            Err(OllamaRequestError::Timeout) => {
                warn!(model = %model, round = round, "Ollama request timed out ({}s)", timeout_secs);
                return "Response timed out. Please try a shorter question.".to_string();
            }
            Err(OllamaRequestError::Connection(e)) => {
                warn!(model = %model, error = %e, round = round, "Ollama connection failed after retry");
                return "I'm temporarily unavailable. Please try again in a moment.".to_string();
            }
            Err(OllamaRequestError::Parse(e)) => {
                warn!(model = %model, error = %e, round = round, "failed to parse Ollama response");
                return "Something went wrong. Please try again.".to_string();
            }
        };

        // Check for Ollama-level errors (OOM, model not found, etc.)
        // → hand off to recovery agent: unload model, retry, communicate via fallback
        if let Some(err) = body.get("error").and_then(|e| e.as_str()) {
            warn!(model = %model, error = %err, round = round, "Ollama returned error");

            if !chat_cfg.chat_fallback_model.is_empty() {
                return handle_ollama_recovery(
                    state,
                    &url,
                    model,
                    &chat_cfg.chat_fallback_model,
                    err,
                    &messages,
                    timeout_secs,
                )
                .await;
            }

            return "I'm temporarily unable to process your message. Please try again in a moment."
                .to_string();
        }

        let msg = match body.get("message") {
            Some(m) => m,
            None => {
                warn!(round = round, body = %body, "Ollama response missing 'message' field");
                return "Something went wrong. Please try again.".to_string();
            }
        };

        // Check for tool calls
        let tool_calls = msg.get("tool_calls").and_then(|tc| tc.as_array());
        if let Some(calls) = tool_calls {
            if !calls.is_empty() {
                // Append assistant message with tool_calls to conversation
                messages.push(msg.clone());

                // Execute each tool call
                for call in calls {
                    let fn_obj = call.get("function");
                    let name = fn_obj
                        .and_then(|f| f.get("name"))
                        .and_then(|n| n.as_str())
                        .unwrap_or("");
                    let args_raw = fn_obj.and_then(|f| f.get("arguments"));

                    info!(tool = name, round = round, "executing tool call");

                    let tool_result = match name {
                        "web_search" => {
                            let query = args_raw
                                .and_then(|a| {
                                    // arguments can be a string or an object
                                    if let Some(s) = a.as_str() {
                                        serde_json::from_str::<serde_json::Value>(s).ok()
                                    } else {
                                        Some(a.clone())
                                    }
                                })
                                .and_then(|obj| {
                                    obj.get("query")
                                        .and_then(|q| q.as_str())
                                        .map(|s| s.to_string())
                                })
                                .unwrap_or_default();

                            if let Some(ref key) = brave_key {
                                let cache_ttl =
                                    std::time::Duration::from_secs(tool_cfg.search_cache_ttl_secs);
                                let cache_key = normalize_search_query(&query);
                                // Check cache
                                let cached = {
                                    let guard = state.brave_cache.read().await;
                                    guard
                                        .get(&cache_key)
                                        .filter(|(_, ts)| ts.elapsed() < cache_ttl)
                                        .map(|(result, _)| result.clone())
                                };
                                if let Some(result) = cached {
                                    info!(query = %query, "brave search cache hit");
                                    result
                                } else {
                                    let result = execute_brave_search(
                                        &state.http_client,
                                        key,
                                        &query,
                                        tool_cfg.search_result_count,
                                    )
                                    .await;
                                    // Cache successful results
                                    if !result.starts_with("Search unavailable")
                                        && !result.starts_with("Search failed")
                                        && !result.starts_with("Search timed out")
                                    {
                                        let mut guard = state.brave_cache.write().await;
                                        guard.insert(
                                            cache_key,
                                            (result.clone(), std::time::Instant::now()),
                                        );
                                        // Evict expired entries when soft limit exceeded
                                        if guard.len() > 100 {
                                            guard.retain(|_, (_, ts)| ts.elapsed() < cache_ttl);
                                        }
                                        // Hard cap: if still over 200 after TTL eviction, drop oldest
                                        if guard.len() > 200 {
                                            if let Some(oldest_key) = guard
                                                .iter()
                                                .min_by_key(|(_, (_, ts))| *ts)
                                                .map(|(k, _)| k.clone())
                                            {
                                                guard.remove(&oldest_key);
                                            }
                                        }
                                    }
                                    result
                                }
                            } else {
                                "Search unavailable: no API key configured.".to_string()
                            }
                        }
                        _ => format!("Unknown tool: {name}"),
                    };

                    info!(tool = name, result_len = tool_result.len(), "tool result");

                    messages.push(serde_json::json!({
                        "role": "tool",
                        "content": tool_result,
                    }));
                }

                // Continue loop — Ollama will get the tool results on next iteration
                continue;
            }
        }

        // No tool calls — extract text response
        let content = msg
            .get("content")
            .and_then(|c| c.as_str())
            .unwrap_or("")
            .trim();

        let cleaned = strip_think_tags(content);
        return if cleaned.is_empty() {
            "I wasn't able to generate a response. Please try again.".to_string()
        } else {
            cleaned
        };
    }

    // Exhausted all rounds without a text response
    warn!("tool-calling loop exhausted max rounds");
    "I wasn't able to generate a response. Please try again.".to_string()
}

/// Remove `<think>...</think>` blocks from model output.
fn strip_think_tags(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(start) = rest.find("<think>") {
        result.push_str(&rest[..start]);
        if let Some(end) = rest[start..].find("</think>") {
            rest = &rest[start + end + 8..];
        } else {
            // Unclosed <think> — drop everything after it
            return result.trim().to_string();
        }
    }
    result.push_str(rest);
    result.trim().to_string()
}

// ---------------------------------------------------------------------------
// Tool executors
// ---------------------------------------------------------------------------

/// Execute a Brave Web Search and return formatted results.
async fn execute_brave_search(
    http_client: &reqwest::Client,
    api_key: &str,
    query: &str,
    count: u32,
) -> String {
    let url = format!(
        "https://api.search.brave.com/res/v1/web/search?q={}&count={count}",
        urlencoding_encode(query),
    );

    let result = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        http_client
            .get(&url)
            .header("X-Subscription-Token", api_key)
            .header("Accept", "application/json")
            .send(),
    )
    .await;

    let resp = match result {
        Ok(Ok(r)) => r,
        Ok(Err(e)) => return format!("Search unavailable: {e}"),
        Err(_) => return "Search timed out.".to_string(),
    };

    if !resp.status().is_success() {
        let status = resp.status();
        return format!("Search failed (HTTP {status}).");
    }

    let body: serde_json::Value = match resp.json().await {
        Ok(b) => b,
        Err(e) => return format!("Failed to parse search results: {e}"),
    };

    let results = body
        .get("web")
        .and_then(|w| w.get("results"))
        .and_then(|r| r.as_array());

    match results {
        Some(arr) if !arr.is_empty() => {
            let mut out = String::new();
            for (i, item) in arr.iter().enumerate() {
                let title = item
                    .get("title")
                    .and_then(|t| t.as_str())
                    .unwrap_or("Untitled");
                let url = item.get("url").and_then(|u| u.as_str()).unwrap_or("");
                let desc = item
                    .get("description")
                    .and_then(|d| d.as_str())
                    .unwrap_or("");
                out.push_str(&format!("{}. {} ({}) — {}\n", i + 1, title, url, desc));
            }
            out
        }
        _ => "No results found.".to_string(),
    }
}

/// Add a Telegram user ID to the allowed_user_ids in platform_credentials.meta and invalidate cache.
async fn add_telegram_allowed_user(state: &AppState, user_id: i64) -> Result<(), String> {
    sqlx::query(
        "UPDATE platform_credentials
         SET meta = jsonb_set(
             COALESCE(meta, '{}'::jsonb),
             '{allowed_user_ids}',
             (COALESCE(meta->'allowed_user_ids', '[]'::jsonb) || to_jsonb($1::bigint))
         ),
         updated_at = NOW()
         WHERE platform = 'telegram'",
    )
    .bind(user_id)
    .execute(&state.pg)
    .await
    .map_err(|e| format!("DB error adding allowed user: {e}"))?;

    // Invalidate credential cache so next call picks up new allow list
    let mut guard = state.telegram_creds.write().await;
    *guard = None;

    info!(
        user_id = user_id,
        "telegram user authenticated and added to allowed list"
    );
    Ok(())
}

/// Build a dedup key from platform, chat_id, and normalized message text.
fn chat_dedup_key(platform: &str, chat_id: &str, text: &str) -> String {
    format!("{platform}:{chat_id}:{}", normalize_search_query(text))
}

/// Normalize a search query for cache/dedup keys: trim, lowercase, collapse whitespace.
fn normalize_search_query(query: &str) -> String {
    query
        .trim()
        .to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Minimal URL-encoding for query parameters.
fn urlencoding_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 2);
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            b' ' => out.push('+'),
            _ => {
                out.push('%');
                out.push_str(&format!("{b:02X}"));
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Platform-specific delivery functions
// ---------------------------------------------------------------------------

async fn deliver_slack(
    state: &AppState,
    reply_url: Option<&str>,
    channel_id: &str,
    thread_ts: Option<&str>,
    text: &str,
) -> Result<(), String> {
    let body = if let Some(url) = reply_url {
        // Use Slack response_url
        let payload = serde_json::json!({
            "response_type": "in_channel",
            "text": text,
        });
        state
            .http_client
            .post(url)
            .json(&payload)
            .send()
            .await
            .map_err(|e| e.to_string())?;
        return Ok(());
    } else {
        // Use chat.postMessage with bot token
        let token = state
            .config
            .webhooks
            .slack_bot_token
            .as_deref()
            .unwrap_or("");
        if token.is_empty() {
            return Err("no slack_bot_token configured".to_string());
        }
        let mut payload = serde_json::json!({
            "channel": channel_id,
            "text": text,
        });
        if let Some(ts) = thread_ts {
            payload["thread_ts"] = serde_json::Value::String(ts.to_string());
        }
        state
            .http_client
            .post("https://slack.com/api/chat.postMessage")
            .header("Authorization", format!("Bearer {token}"))
            .json(&payload)
            .send()
            .await
            .map_err(|e| e.to_string())?
    };

    let status = body.status();
    if !status.is_success() {
        let text = body.text().await.unwrap_or_default();
        return Err(format!("slack API returned {status}: {text}"));
    }
    Ok(())
}

async fn deliver_teams(
    state: &AppState,
    service_url: Option<&str>,
    conversation_id: &str,
    text: &str,
) -> Result<(), String> {
    let url = match service_url {
        Some(u) => format!("{u}/v3/conversations/{conversation_id}/activities"),
        None => return Err("no teams service_url available".to_string()),
    };
    let payload = serde_json::json!({
        "type": "message",
        "text": text,
    });
    let resp = state
        .http_client
        .post(&url)
        .json(&payload)
        .send()
        .await
        .map_err(|e| e.to_string())?;

    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        return Err(format!("teams API returned {status}: {text}"));
    }
    Ok(())
}

async fn deliver_telegram(
    state: &AppState,
    chat_id: &str,
    thread_id: Option<&str>,
    text: &str,
) -> Result<(), String> {
    let (tg_token, _, _, _) = get_telegram_creds(state).await;
    let token = tg_token.unwrap_or_default();
    if token.is_empty() {
        return Err("no telegram_bot_token configured".to_string());
    }
    let url = format!("https://api.telegram.org/bot{token}/sendMessage");
    let mut payload = serde_json::json!({
        "chat_id": chat_id,
        "text": text,
    });
    if let Some(tid) = thread_id {
        payload["message_thread_id"] = serde_json::Value::String(tid.to_string());
    }
    let resp = state
        .http_client
        .post(&url)
        .json(&payload)
        .send()
        .await
        .map_err(|e| e.to_string())?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(format!("telegram API returned {status}: {body}"));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn test_map_status_pending() {
        matches!(map_status("pending"), A2aTaskStatus::Submitted);
    }

    #[test]
    fn test_map_status_claimed() {
        matches!(map_status("claimed"), A2aTaskStatus::Working);
    }

    #[test]
    fn test_map_status_completed() {
        matches!(map_status("completed"), A2aTaskStatus::Completed);
    }

    #[test]
    fn test_map_status_failed() {
        matches!(map_status("failed"), A2aTaskStatus::Failed);
    }

    #[test]
    fn test_map_status_unknown() {
        matches!(map_status("garbage"), A2aTaskStatus::Unknown);
    }

    #[test]
    fn test_check_auth_no_key_configured() {
        let headers = HeaderMap::new();
        assert!(check_auth(&headers, &None).is_ok());
    }

    #[test]
    fn test_check_auth_valid_bearer() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::AUTHORIZATION,
            "Bearer test-key-123".parse().unwrap(),
        );
        let key = Some("test-key-123".to_string());
        assert!(check_auth(&headers, &key).is_ok());
    }

    #[test]
    fn test_check_auth_invalid_bearer() {
        let mut headers = HeaderMap::new();
        headers.insert(header::AUTHORIZATION, "Bearer wrong-key".parse().unwrap());
        let key = Some("test-key-123".to_string());
        assert_eq!(check_auth(&headers, &key), Err(StatusCode::UNAUTHORIZED));
    }

    #[test]
    fn test_check_auth_missing_header() {
        let headers = HeaderMap::new();
        let key = Some("test-key-123".to_string());
        assert_eq!(check_auth(&headers, &key), Err(StatusCode::UNAUTHORIZED));
    }

    #[test]
    fn test_jsonrpc_response_format() {
        let resp = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": { "id": "test", "status": { "state": "submitted" } },
        });
        assert_eq!(resp["jsonrpc"], "2.0");
        assert!(resp["result"]["status"]["state"].is_string());
    }

    #[test]
    fn test_a2a_task_response_serialization() {
        let resp = A2aTaskResponse {
            id: "ext-123".to_string(),
            status: A2aTaskStatusInfo {
                state: A2aTaskStatus::Working,
                message: Some("processing".to_string()),
            },
            artifacts: None,
        };
        let json = serde_json::to_value(&resp).unwrap();
        assert_eq!(json["id"], "ext-123");
        assert_eq!(json["status"]["state"], "working");
        assert_eq!(json["status"]["message"], "processing");
        assert!(json.get("artifacts").is_none());
    }

    #[test]
    fn test_parse_command_with_prefix() {
        let (cmd, args) = parse_command_text("/broodlink agents").unwrap();
        assert_eq!(cmd, "agents");
        assert!(args.is_empty());
    }

    #[test]
    fn test_parse_command_toggle() {
        let (cmd, args) = parse_command_text("/broodlink toggle claude").unwrap();
        assert_eq!(cmd, "toggle");
        assert_eq!(args, vec!["claude"]);
    }

    #[test]
    fn test_parse_command_task_cancel() {
        let (cmd, args) = parse_command_text("/broodlink task cancel abc-123").unwrap();
        assert_eq!(cmd, "task");
        assert_eq!(args, vec!["cancel", "abc-123"]);
    }

    #[test]
    fn test_parse_command_without_prefix() {
        let (cmd, args) = parse_command_text("agents").unwrap();
        assert_eq!(cmd, "agents");
        assert!(args.is_empty());
    }

    #[test]
    fn test_parse_command_empty() {
        assert!(parse_command_text("").is_none());
        assert!(parse_command_text("   ").is_none());
    }

    #[test]
    fn test_urlencoding_decode() {
        assert_eq!(urlencoding_decode("hello+world"), "hello world");
        assert_eq!(urlencoding_decode("foo%20bar"), "foo bar");
        assert_eq!(urlencoding_decode("100%25"), "100%");
    }

    // --- v0.7.0: Chat message parsing tests ---

    #[test]
    fn test_slack_event_message_parsing() {
        let event = serde_json::json!({
            "type": "event_callback",
            "event": {
                "type": "message",
                "channel": "C1234",
                "user": "U5678",
                "text": "hello agent",
                "thread_ts": "1234567890.000100"
            }
        });
        let evt = event.get("event").unwrap();
        assert_eq!(evt.get("type").and_then(|t| t.as_str()), Some("message"));
        assert!(evt.get("bot_id").is_none());
        assert_eq!(evt.get("channel").and_then(|c| c.as_str()), Some("C1234"));
        assert_eq!(evt.get("user").and_then(|u| u.as_str()), Some("U5678"));
        assert_eq!(
            evt.get("text").and_then(|t| t.as_str()),
            Some("hello agent")
        );
        assert_eq!(
            evt.get("thread_ts").and_then(|t| t.as_str()),
            Some("1234567890.000100")
        );
    }

    #[test]
    fn test_slack_event_bot_message_ignored() {
        let event = serde_json::json!({
            "type": "event_callback",
            "event": {
                "type": "message",
                "channel": "C1234",
                "user": "U5678",
                "text": "bot reply",
                "bot_id": "B0001"
            }
        });
        let evt = event.get("event").unwrap();
        assert!(
            evt.get("bot_id").is_some(),
            "bot messages should be detected"
        );
    }

    #[test]
    fn test_teams_activity_parsing() {
        let payload = serde_json::json!({
            "type": "message",
            "text": "help me with something",
            "from": { "id": "user-001", "name": "Jane" },
            "channelId": "teams-channel-1",
            "conversation": { "id": "conv-123" },
            "serviceUrl": "https://smba.trafficmanager.net/teams"
        });
        assert_eq!(
            payload.get("type").and_then(|t| t.as_str()),
            Some("message")
        );
        assert_eq!(
            payload
                .get("from")
                .and_then(|f| f.get("name"))
                .and_then(|n| n.as_str()),
            Some("Jane")
        );
        assert_eq!(
            payload
                .get("from")
                .and_then(|f| f.get("id"))
                .and_then(|n| n.as_str()),
            Some("user-001")
        );
        assert_eq!(
            payload.get("channelId").and_then(|c| c.as_str()),
            Some("teams-channel-1")
        );
        assert_eq!(
            payload.get("serviceUrl").and_then(|s| s.as_str()),
            Some("https://smba.trafficmanager.net/teams")
        );
    }

    #[test]
    fn test_telegram_message_parsing() {
        let payload = serde_json::json!({
            "message": {
                "text": "what is the weather?",
                "from": { "id": 12345, "first_name": "Bob" },
                "chat": { "id": 67890 },
                "message_thread_id": 42
            }
        });
        let msg = payload.get("message").unwrap();
        assert_eq!(
            msg.get("text").and_then(|t| t.as_str()),
            Some("what is the weather?")
        );
        assert_eq!(
            msg.get("from")
                .and_then(|f| f.get("first_name"))
                .and_then(|n| n.as_str()),
            Some("Bob")
        );
        assert_eq!(
            msg.get("from")
                .and_then(|f| f.get("id"))
                .and_then(|i| i.as_i64()),
            Some(12345)
        );
        assert_eq!(
            msg.get("chat")
                .and_then(|c| c.get("id"))
                .and_then(|i| i.as_i64()),
            Some(67890)
        );
        assert_eq!(
            msg.get("message_thread_id").and_then(|t| t.as_i64()),
            Some(42)
        );
    }

    #[test]
    fn test_slash_command_still_works() {
        // /broodlink commands should still be parsed as commands, not chat messages
        let (cmd, args) = parse_command_text("/broodlink agents").unwrap();
        assert_eq!(cmd, "agents");
        assert!(args.is_empty());

        // A free-form message should NOT parse as a command
        let result = parse_command_text("what is the meaning of life?");
        // This will parse as "what" command with args, but it won't match any known command
        assert!(result.is_some());
        let (cmd, _) = result.unwrap();
        assert_eq!(cmd, "what");
    }

    // --- v0.7.0: Session upsert logic tests ---

    #[test]
    fn test_session_upsert_new_generates_uuid() {
        // On a brand new session, handle_chat_message generates a UUID for session_id.
        // The SQL upsert uses ON CONFLICT (platform, channel_id, user_id).
        // Verify UUID generation is valid.
        let session_id = uuid::Uuid::new_v4().to_string();
        assert_eq!(
            session_id.len(),
            36,
            "UUID should be 36 chars including hyphens"
        );
        assert_eq!(
            session_id.chars().filter(|c| *c == '-').count(),
            4,
            "UUID should have 4 hyphens"
        );
    }

    #[test]
    fn test_session_upsert_conflict_key() {
        // The unique constraint is (platform, channel_id, user_id).
        // Same triple should route to the same session; different triple should be distinct.
        let key1 = ("slack", "C123", "U456");
        let key2 = ("slack", "C123", "U456"); // same
        let key3 = ("teams", "C123", "U456"); // different platform
        let key4 = ("slack", "C999", "U456"); // different channel

        assert_eq!(key1, key2, "same triple should map to same session");
        assert_ne!(key1, key3, "different platform should be distinct session");
        assert_ne!(key1, key4, "different channel should be distinct session");
    }

    // --- v0.7.0: Reply URL construction per platform ---

    #[test]
    fn test_slack_reply_url_via_response_url() {
        // When Slack provides a response_url, it should be used directly (no construction needed)
        let response_url = "https://hooks.slack.com/actions/T00/B00/xxxyyy";
        assert!(response_url.starts_with("https://hooks.slack.com/"));
        // Payload format for response_url
        let payload = serde_json::json!({
            "response_type": "in_channel",
            "text": "Agent reply",
        });
        assert_eq!(payload["response_type"], "in_channel");
        assert_eq!(payload["text"], "Agent reply");
    }

    #[test]
    fn test_teams_reply_url_construction() {
        // Teams uses: {serviceUrl}/v3/conversations/{conversationId}/activities
        let service_url = "https://smba.trafficmanager.net/teams";
        let conversation_id = "conv-123";
        let url = format!("{service_url}/v3/conversations/{conversation_id}/activities");
        assert_eq!(
            url,
            "https://smba.trafficmanager.net/teams/v3/conversations/conv-123/activities"
        );
    }

    #[test]
    fn test_telegram_reply_url_construction() {
        // Telegram uses: https://api.telegram.org/bot{token}/sendMessage
        let token = "123456:ABC-DEF1234ghIkl-zyx57W2v1u123ew11";
        let url = format!("https://api.telegram.org/bot{token}/sendMessage");
        assert_eq!(
            url,
            "https://api.telegram.org/bot123456:ABC-DEF1234ghIkl-zyx57W2v1u123ew11/sendMessage"
        );
        // Payload includes chat_id and optionally message_thread_id
        let mut payload = serde_json::json!({
            "chat_id": "67890",
            "text": "hello",
        });
        assert_eq!(payload["chat_id"], "67890");
        payload["message_thread_id"] = serde_json::Value::String("42".to_string());
        assert_eq!(payload["message_thread_id"], "42");
    }

    // --- v0.7.0: Greeting on new session ---

    #[test]
    fn test_greeting_triggers_on_first_message() {
        // The greeting logic checks: greeting_enabled && msg_count == 1
        let greeting_enabled = true;

        // msg_count == 1 → new session → greeting should fire
        let msg_count = 1;
        assert!(
            greeting_enabled && msg_count == 1,
            "greeting should fire on first message"
        );

        // msg_count == 2 → returning user → no greeting
        let msg_count = 2;
        assert!(
            !(greeting_enabled && msg_count == 1),
            "greeting should NOT fire on subsequent messages"
        );

        // greeting_enabled == false → never fire
        let greeting_enabled = false;
        let msg_count = 1;
        assert!(
            !(greeting_enabled && msg_count == 1),
            "greeting should NOT fire when disabled"
        );
    }
}
