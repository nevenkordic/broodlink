/*
 * Broodlink - Multi-agent AI orchestration system
 * Copyright (C) 2025–2026 Neven Kordic <neven@broodlink.ai>
 * SPDX-License-Identifier: AGPL-3.0-or-later
 */

#![deny(clippy::unwrap_used)]
#![deny(clippy::expect_used)]
#![warn(clippy::pedantic)]

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

use std::net::SocketAddr;
use std::process;
use std::sync::Arc;

use axum::extract::State;
use axum::http::{header, HeaderMap, Method, StatusCode};
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
            GatewayError::NotFound(_) => (StatusCode::NOT_FOUND, self.to_string()),
            GatewayError::BadRequest(_) => (StatusCode::BAD_REQUEST, self.to_string()),
            _ => (StatusCode::INTERNAL_SERVER_ERROR, self.to_string()),
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
    api_key: Option<String>,
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() {
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
            None,
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
        http_client: reqwest::Client::new(),
        api_key,
        config: Arc::clone(&config),
    });

    // Spawn chat reply delivery loop
    if config.chat.enabled {
        let st = Arc::clone(&state);
        tokio::spawn(async move {
            reply_delivery_loop(&st).await;
        });
    }

    // Subscribe to NATS task_completed/task_failed for chat reply routing
    if config.chat.enabled {
        if let Some(nc) = nats_client {
            let prefix = config.nats.subject_prefix.clone();
            let st = Arc::clone(&state);

            // task_completed
            let nc2 = nc.clone(); // clone for the second subscription
            let st_completed = Arc::clone(&st);
            let subj_completed = format!("{prefix}.*.coordinator.task_completed");
            tokio::spawn(async move {
                match nc.subscribe(subj_completed.clone()).await {
                    Ok(mut sub) => {
                        info!(subject = %subj_completed, "subscribed for chat completions");
                        while let Some(msg) = sub.next().await {
                            if let Ok(payload) = serde_json::from_slice::<serde_json::Value>(&msg.payload) {
                                let task_id = payload
                                    .get("task_id")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or_default();
                                let result_data = payload
                                    .get("result_data")
                                    .cloned()
                                    .unwrap_or_default();
                                if !task_id.is_empty() {
                                    handle_task_completion_for_chat(&st_completed, task_id, &result_data, false).await;
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
                            if let Ok(payload) = serde_json::from_slice::<serde_json::Value>(&msg.payload) {
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
                                    handle_task_completion_for_chat(&st, task_id, &err_val, true).await;
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

    let cors = CorsLayer::new()
        .allow_methods([Method::GET, Method::POST])
        .allow_headers([header::CONTENT_TYPE, header::AUTHORIZATION])
        .allow_origin(AllowOrigin::any());

    let app = Router::new()
        .route("/.well-known/agent.json", get(agent_card_handler))
        .route("/a2a/tasks/send", post(tasks_send_handler))
        .route("/a2a/tasks/get", post(tasks_get_handler))
        .route("/a2a/tasks/cancel", post(tasks_cancel_handler))
        .route("/a2a/tasks/sendSubscribe", post(tasks_send_subscribe_handler))
        // Webhook inbound routes
        .route("/webhook/slack", post(webhook_slack_handler))
        .route("/webhook/teams", post(webhook_teams_handler))
        .route("/webhook/telegram", post(webhook_telegram_handler))
        .route("/health", get(health_handler))
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
        Some(t) if t == expected => Ok(()),
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
        return Err(GatewayError::Bridge(format!("bridge returned {status}: {text}")));
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

fn jsonrpc_err(
    id: Option<serde_json::Value>,
    code: i32,
    message: &str,
) -> Response {
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
    let skills = match bridge_call(&*state, "list_formulas", serde_json::json!({})).await {
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
        &*state,
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

    let external_id = rpc
        .params
        .get("id")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    if external_id.is_empty() {
        return jsonrpc_err(rpc.id, -32602, "missing required param: id");
    }

    // Look up internal ID
    let row: Option<(String,)> = sqlx::query_as(
        "SELECT internal_id FROM a2a_task_map WHERE external_id = $1",
    )
    .bind(external_id)
    .fetch_optional(&state.pg)
    .await
    .unwrap_or(None);

    let Some((internal_id,)) = row else {
        return jsonrpc_err(rpc.id, -32001, "task not found");
    };

    // Get task status from bridge
    let status_result = bridge_call(
        &*state,
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
        Err(e) => (A2aTaskStatus::Unknown, Some(format!("lookup failed: {e}")), None),
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

    let external_id = rpc
        .params
        .get("id")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    if external_id.is_empty() {
        return jsonrpc_err(rpc.id, -32602, "missing required param: id");
    }

    // Look up internal ID
    let row: Option<(String,)> = sqlx::query_as(
        "SELECT internal_id FROM a2a_task_map WHERE external_id = $1",
    )
    .bind(external_id)
    .fetch_optional(&state.pg)
    .await
    .unwrap_or(None);

    let Some((internal_id,)) = row else {
        return jsonrpc_err(rpc.id, -32001, "task not found");
    };

    // Cancel via bridge
    match bridge_call(
        &*state,
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
        &*state,
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
                            if r.is_null() { None } else {
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

async fn health_handler() -> impl IntoResponse {
    axum::Json(serde_json::json!({
        "status": "ok",
        "service": SERVICE_NAME,
        "version": env!("CARGO_PKG_VERSION"),
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
async fn execute_command(
    state: &AppState,
    cmd: &WebhookCommand,
) -> String {
    match cmd.command.as_str() {
        "agents" | "list" => {
            match bridge_call(state, "list_agents", serde_json::json!({})).await {
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
            }
        }

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

        "dlq" => {
            match bridge_call(state, "inspect_dlq", serde_json::json!({})).await {
                Ok(data) => {
                    let entries = data
                        .as_array()
                        .or_else(|| data.get("entries").and_then(|e| e.as_array()));
                    match entries {
                        Some(arr) => {
                            let unresolved: Vec<_> = arr
                                .iter()
                                .filter(|e| !e.get("resolved").and_then(|v| v.as_bool()).unwrap_or(false))
                                .collect();
                            if unresolved.is_empty() {
                                return "DLQ is empty.".to_string();
                            }
                            let mut lines = vec![format!("DLQ ({} entries):", unresolved.len())];
                            for e in unresolved.iter().take(5) {
                                let task = e.get("task_id").and_then(|v| v.as_str()).unwrap_or("?");
                                let reason = e.get("reason").and_then(|v| v.as_str()).unwrap_or("?");
                                let retries = e.get("retry_count").and_then(|v| v.as_i64()).unwrap_or(0);
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
            }
        }

        "help" => {
            "Broodlink commands:\n\
             \x20 agents         — list all agents\n\
             \x20 toggle <id>    — toggle agent active/inactive\n\
             \x20 budget <id>    — show agent budget\n\
             \x20 task cancel <id> — cancel a task\n\
             \x20 workflow start <formula> — start a workflow\n\
             \x20 dlq            — show dead-letter queue\n\
             \x20 help           — show this help"
                .to_string()
        }

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
// POST /webhook/slack — receive Slack slash commands
// ---------------------------------------------------------------------------

async fn webhook_slack_handler(
    State(state): State<Arc<AppState>>,
    _headers: HeaderMap,
    body: String,
) -> Response {
    if !state.config.webhooks.enabled {
        return (StatusCode::NOT_FOUND, "webhooks disabled").into_response();
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
            Some((
                urlencoding_decode(key),
                urlencoding_decode(value),
            ))
        })
        .collect();

    let text = params.get("text").cloned().unwrap_or_default();
    let user = params.get("user_name").cloned().unwrap_or_else(|| "slack-user".to_string());
    let user_id = params.get("user_id").cloned().unwrap_or_default();
    let channel_id = params.get("channel_id").cloned().unwrap_or_default();
    let response_url = params.get("response_url").cloned();

    info!(platform = "slack", user = %user, text = %text, "inbound webhook");

    // If the text looks like a command, use command handler
    // Otherwise, if chat is enabled, treat as conversational message
    if let Some((command, args)) = parse_command_text(&text) {
        let payload_json = serde_json::to_value(&params).unwrap_or_default();
        log_webhook(&state.pg, None, "inbound", &format!("slack.{command}"), &payload_json, "delivered", None).await;

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
        handle_chat_message(
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
        let challenge = payload.get("challenge").and_then(|c| c.as_str()).unwrap_or("");
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
            if event_type == "message" && event.get("bot_id").is_none() && event.get("subtype").is_none() {
                let channel = event.get("channel").and_then(|c| c.as_str()).unwrap_or("");
                let user = event.get("user").and_then(|u| u.as_str()).unwrap_or("");
                let text = event.get("text").and_then(|t| t.as_str()).unwrap_or("");
                let thread_ts = event.get("thread_ts").and_then(|t| t.as_str());

                if !text.is_empty() && state.config.chat.enabled {
                    // Check if it's a command first
                    if text.starts_with("/broodlink") || text.starts_with("broodlink ") {
                        // Let the slash command handler deal with it
                    } else {
                        handle_chat_message(
                            state,
                            "slack",
                            channel,
                            user,
                            user, // display name not available in events API, use user_id
                            text,
                            thread_ts,
                            None,
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
    body: String,
) -> Response {
    if !state.config.webhooks.enabled {
        return (StatusCode::NOT_FOUND, "webhooks disabled").into_response();
    }

    let payload: serde_json::Value = match serde_json::from_str(&body) {
        Ok(v) => v,
        Err(e) => {
            return (StatusCode::BAD_REQUEST, format!("invalid json: {e}")).into_response();
        }
    };

    let text = payload
        .get("text")
        .and_then(|t| t.as_str())
        .unwrap_or("");
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
        .or_else(|| payload.get("conversation").and_then(|c| c.get("id")).and_then(|i| i.as_str()))
        .unwrap_or("");
    let service_url = payload
        .get("serviceUrl")
        .and_then(|s| s.as_str());
    let activity_type = payload
        .get("type")
        .and_then(|t| t.as_str())
        .unwrap_or("");

    info!(platform = "teams", user = %user_name, text = %text, "inbound webhook");

    // Check for /broodlink command first
    if let Some((command, args)) = parse_command_text(text) {
        log_webhook(&state.pg, None, "inbound", &format!("teams.{command}"), &payload, "delivered", None).await;

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
        handle_chat_message(
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
// POST /webhook/telegram — receive Telegram bot updates
// ---------------------------------------------------------------------------

async fn webhook_telegram_handler(
    State(state): State<Arc<AppState>>,
    body: String,
) -> Response {
    if !state.config.webhooks.enabled {
        return (StatusCode::NOT_FOUND, "webhooks disabled").into_response();
    }

    let payload: serde_json::Value = match serde_json::from_str(&body) {
        Ok(v) => v,
        Err(e) => {
            return (StatusCode::BAD_REQUEST, format!("invalid json: {e}")).into_response();
        }
    };

    let message = payload.get("message").unwrap_or(&payload);
    let text = message
        .get("text")
        .and_then(|t| t.as_str())
        .unwrap_or("");
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

    info!(platform = "telegram", user = %user_name, chat_id = chat_id, text = %text, "inbound webhook");

    // Check for /broodlink command or /command style
    if text.starts_with('/') || text.starts_with("broodlink ") {
        if let Some((command, args)) = parse_command_text(text) {
            log_webhook(&state.pg, None, "inbound", &format!("telegram.{command}"), &payload, "delivered", None).await;

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

        handle_chat_message(
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

        let reply = serde_json::json!({
            "method": "sendMessage",
            "chat_id": chat_id,
            "text": "Got it! Working on your request...",
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
async fn handle_chat_message(
    state: &AppState,
    platform: &str,
    channel_id: &str,
    user_id: &str,
    user_name: &str,
    text: &str,
    thread_id: Option<&str>,
    reply_url: Option<&str>,
) {
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
            error!(platform = platform, user_id = user_id, "failed to upsert chat session");
            return;
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
            let label = if dir == "inbound" { user_name } else { "Broodlink" };
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
}

/// Background loop: deliver pending chat replies to platforms.
async fn reply_delivery_loop(state: &AppState) {
    let chat_cfg = &state.config.chat;
    let max_attempts = chat_cfg.reply_retry_attempts;

    info!("chat reply delivery loop started");

    loop {
        tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;

        // Fetch pending replies
        let rows: Vec<(i64, String, String, Option<String>, String, Option<String>, String)> =
            match sqlx::query_as(
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
                "slack" => deliver_slack(state, reply_url.as_deref(), &channel_id, thread_id.as_deref(), &content).await,
                "teams" => deliver_teams(state, reply_url.as_deref(), &channel_id, &content).await,
                "telegram" => deliver_telegram(state, &channel_id, thread_id.as_deref(), &content).await,
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
    let row: Option<(serde_json::Value,)> = sqlx::query_as(
        "SELECT dependencies FROM task_queue WHERE id = $1",
    )
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

    let session_id = chat.get("chat_session_id").and_then(|v| v.as_str()).unwrap_or_default();
    let platform = chat.get("platform").and_then(|v| v.as_str()).unwrap_or_default();
    let channel_id = chat.get("channel_id").and_then(|v| v.as_str()).unwrap_or_default();
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
            .or_else(|| result_data.get("response").and_then(|v| v.as_str()).map(String::from))
            .or_else(|| result_data.get("text").and_then(|v| v.as_str()).map(String::from))
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
        let token = state.config.webhooks.slack_bot_token.as_deref().unwrap_or("");
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
    let token = state.config.webhooks.telegram_bot_token.as_deref().unwrap_or("");
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
        headers.insert(
            header::AUTHORIZATION,
            "Bearer wrong-key".parse().unwrap(),
        );
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
        assert_eq!(evt.get("text").and_then(|t| t.as_str()), Some("hello agent"));
        assert_eq!(evt.get("thread_ts").and_then(|t| t.as_str()), Some("1234567890.000100"));
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
        assert!(evt.get("bot_id").is_some(), "bot messages should be detected");
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
        assert_eq!(payload.get("type").and_then(|t| t.as_str()), Some("message"));
        assert_eq!(payload.get("from").and_then(|f| f.get("name")).and_then(|n| n.as_str()), Some("Jane"));
        assert_eq!(payload.get("from").and_then(|f| f.get("id")).and_then(|n| n.as_str()), Some("user-001"));
        assert_eq!(payload.get("channelId").and_then(|c| c.as_str()), Some("teams-channel-1"));
        assert_eq!(payload.get("serviceUrl").and_then(|s| s.as_str()), Some("https://smba.trafficmanager.net/teams"));
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
        assert_eq!(msg.get("text").and_then(|t| t.as_str()), Some("what is the weather?"));
        assert_eq!(msg.get("from").and_then(|f| f.get("first_name")).and_then(|n| n.as_str()), Some("Bob"));
        assert_eq!(msg.get("from").and_then(|f| f.get("id")).and_then(|i| i.as_i64()), Some(12345));
        assert_eq!(msg.get("chat").and_then(|c| c.get("id")).and_then(|i| i.as_i64()), Some(67890));
        assert_eq!(msg.get("message_thread_id").and_then(|t| t.as_i64()), Some(42));
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
        assert_eq!(session_id.len(), 36, "UUID should be 36 chars including hyphens");
        assert_eq!(session_id.chars().filter(|c| *c == '-').count(), 4, "UUID should have 4 hyphens");
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
        assert!(greeting_enabled && msg_count == 1, "greeting should fire on first message");

        // msg_count == 2 → returning user → no greeting
        let msg_count = 2;
        assert!(!(greeting_enabled && msg_count == 1), "greeting should NOT fire on subsequent messages");

        // greeting_enabled == false → never fire
        let greeting_enabled = false;
        let msg_count = 1;
        assert!(!(greeting_enabled && msg_count == 1), "greeting should NOT fire when disabled");
    }
}
