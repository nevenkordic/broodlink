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
use sqlx::postgres::PgPool;
use tokio_stream::wrappers::ReceiverStream;
use tower_http::cors::{AllowOrigin, CorsLayer};
use tracing::{error, info, warn};
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

    let state = Arc::new(AppState {
        bridge_url: config.a2a.bridge_url.clone(),
        bridge_jwt,
        pg,
        http_client: reqwest::Client::new(),
        api_key,
        config: Arc::clone(&config),
    });

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
}
