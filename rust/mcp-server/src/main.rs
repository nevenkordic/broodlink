/*
 * Broodlink - Multi-agent AI orchestration system
 * Copyright (C) 2025–2026 Neven Kordic <neven@broodlink.ai>
 * SPDX-License-Identifier: AGPL-3.0-or-later
 */

mod handler;
mod protocol;
mod streamable_http;

use std::net::SocketAddr;
use std::process;
use std::sync::Arc;

use axum::extract::{DefaultBodyLimit, State};
use axum::http::{header, Request};
use axum::middleware::{self, Next};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::Router;
use broodlink_config::Config;
use futures::stream::Stream;
use handler::BridgeClient;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio_stream::wrappers::ReceiverStream;
use tracing::{error, info, warn};

const SERVICE_NAME: &str = "mcp-server";

// ---------------------------------------------------------------------------
// Application state
// ---------------------------------------------------------------------------

struct McpState {
    client: BridgeClient,
    #[allow(dead_code)]
    config: Arc<Config>,
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

    // Read JWT token for authenticating with beads-bridge
    let jwt_path = config.mcp_server.jwt_token_path.clone().unwrap_or_else(|| {
        let agent = &config.mcp_server.agent_id;
        format!(
            "{}/.broodlink/jwt-{agent}.token",
            std::env::var("HOME").unwrap_or_else(|_| ".".to_string())
        )
    });

    let jwt_token = match std::fs::read_to_string(&jwt_path) {
        Ok(t) => t.trim().to_string(),
        Err(e) => {
            error!(path = %jwt_path, error = %e, "JWT token required but not found");
            process::exit(1);
        }
    };

    let bridge_url = config.mcp_server.bridge_url.clone();
    let status_url = format!("http://127.0.0.1:{}", config.status_api.port);

    // Resolve status-api key from the env var named in config (e.g. BROODLINK_STATUS_API_KEY)
    let status_api_key = std::env::var(&config.status_api.api_key_name).unwrap_or_else(|_| {
        error!(
            key_name = %config.status_api.api_key_name,
            "status API key env var not set — exiting"
        );
        process::exit(1);
    });

    let client = BridgeClient::new(bridge_url, jwt_token, status_url, status_api_key);

    // Select transport based on config
    let transport = config.mcp_server.transport.as_str();
    match transport {
        "http" => {
            info!(
                port = config.mcp_server.port,
                "starting Streamable HTTP transport"
            );
            streamable_http::run_http_transport(config, client).await;
        }
        "sse" => {
            info!(
                port = config.mcp_server.port,
                "starting legacy SSE transport"
            );
            run_sse_transport(config, client).await;
        }
        "stdio" => {
            info!("starting stdio transport");
            run_stdio_transport(client).await;
        }
        _ => {
            error!(
                transport = transport,
                "unknown transport — expected http, sse, or stdio"
            );
            process::exit(1);
        }
    }
}

// ---------------------------------------------------------------------------
// Stdio transport — line-delimited JSON-RPC over stdin/stdout
// ---------------------------------------------------------------------------

async fn run_stdio_transport(client: BridgeClient) {
    let stdin = tokio::io::stdin();
    let mut stdout = tokio::io::stdout();
    let reader = BufReader::new(stdin);
    let mut lines = reader.lines();

    while let Ok(Some(line)) = lines.next_line().await {
        if line.trim().is_empty() {
            continue;
        }

        let response = process_jsonrpc_line(&client, &line).await;

        if let Some(resp) = response {
            let mut out = resp.into_bytes();
            out.push(b'\n');
            if let Err(e) = stdout.write_all(&out).await {
                error!(error = %e, "failed to write to stdout");
                break;
            }
            if let Err(e) = stdout.flush().await {
                error!(error = %e, "failed to flush stdout");
                break;
            }
        }
    }
}

async fn process_jsonrpc_line(client: &BridgeClient, line: &str) -> Option<String> {
    let req: protocol::JsonRpcRequest = match serde_json::from_str(line) {
        Ok(r) => r,
        Err(e) => {
            warn!(error = %e, "failed to parse JSON-RPC request");
            return Some(protocol::serialize_response(
                None,
                Err((protocol::PARSE_ERROR, format!("parse error: {e}"))),
            ));
        }
    };

    // Notifications (no id) don't get responses
    let id = req.id.clone();
    let is_notification = id.is_none();

    let result = handler::dispatch(client, &req.method, &req.params).await;

    if is_notification {
        return None;
    }

    Some(protocol::serialize_response(id, result))
}

// ---------------------------------------------------------------------------
// Security headers middleware (OWASP A05)
// ---------------------------------------------------------------------------

async fn security_headers_middleware(req: Request<axum::body::Body>, next: Next) -> Response {
    let mut resp = next.run(req).await;
    let headers = resp.headers_mut();
    headers.insert(
        "X-Content-Type-Options",
        header::HeaderValue::from_static("nosniff"),
    );
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
// SSE transport — axum HTTP server on configured port
// ---------------------------------------------------------------------------

async fn run_sse_transport(config: Arc<Config>, client: BridgeClient) {
    let state = Arc::new(McpState {
        client,
        config: Arc::clone(&config),
    });

    let app = Router::new()
        .route("/sse", get(sse_handler))
        .route("/message", post(message_handler))
        .route("/health", get(health_handler))
        .layer(DefaultBodyLimit::max(1_048_576)) // 1 MiB
        .layer(middleware::from_fn(security_headers_middleware))
        .with_state(state);

    let addr = SocketAddr::from(([0, 0, 0, 0], config.mcp_server.port));
    info!(addr = %addr, "MCP SSE server listening");

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

/// SSE endpoint — clients connect here to receive server→client messages.
async fn sse_handler(
    State(state): State<Arc<McpState>>,
) -> Sse<impl Stream<Item = Result<Event, std::convert::Infallible>>> {
    let _ = &state; // State available for future use

    let (tx, rx) = tokio::sync::mpsc::channel::<Result<Event, std::convert::Infallible>>(32);

    // Send the endpoint event so the client knows where to POST
    let endpoint_msg = "/message".to_string();
    tokio::spawn(async move {
        let event = Event::default().event("endpoint").data(endpoint_msg);
        let _ = tx.send(Ok(event)).await;
        // Keep the channel open — the tx will be stored for sending responses
        // In a production MCP server, you'd store tx in shared state keyed by session
        // For now, the channel stays open until the client disconnects
        tokio::time::sleep(tokio::time::Duration::from_secs(3600)).await;
    });

    let stream = ReceiverStream::new(rx);
    Sse::new(stream).keep_alive(KeepAlive::default())
}

/// Message endpoint — clients POST JSON-RPC requests here.
async fn message_handler(
    State(state): State<Arc<McpState>>,
    axum::Json(req): axum::Json<protocol::JsonRpcRequest>,
) -> impl IntoResponse {
    let id = req.id.clone();
    let result = handler::dispatch(&state.client, &req.method, &req.params).await;
    let response_str = protocol::serialize_response(id, result);

    axum::http::Response::builder()
        .status(200)
        .header("content-type", "application/json")
        .body(response_str)
        .unwrap_or_else(|_| {
            axum::http::Response::builder()
                .status(500)
                .body(String::new())
                .unwrap_or_default()
        })
}

async fn health_handler(State(state): State<Arc<McpState>>) -> impl IntoResponse {
    let bridge_ok = state.client.check_health().await;

    let status = if bridge_ok { "ok" } else { "degraded" };

    axum::Json(serde_json::json!({
        "status": status,
        "service": SERVICE_NAME,
        "version": env!("CARGO_PKG_VERSION"),
        "checks": {
            "bridge": bridge_ok,
        },
    }))
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_process_valid_request() {
        let client = BridgeClient::new(
            "http://localhost:3310".to_string(),
            "fake".to_string(),
            "http://localhost:3312".to_string(),
            "dev-api-key".to_string(),
        );
        let line = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#;
        let resp = process_jsonrpc_line(&client, line).await;
        assert!(resp.is_some());
        let v: serde_json::Value = serde_json::from_str(&resp.unwrap()).unwrap();
        assert_eq!(v["jsonrpc"], "2.0");
        assert_eq!(v["id"], 1);
        assert!(v["result"]["protocolVersion"].is_string());
    }

    #[tokio::test]
    async fn test_process_invalid_json() {
        let client = BridgeClient::new(
            "http://localhost:3310".to_string(),
            "fake".to_string(),
            "http://localhost:3312".to_string(),
            "dev-api-key".to_string(),
        );
        let resp = process_jsonrpc_line(&client, "not json").await;
        assert!(resp.is_some());
        let v: serde_json::Value = serde_json::from_str(&resp.unwrap()).unwrap();
        assert_eq!(v["error"]["code"], protocol::PARSE_ERROR);
    }

    #[tokio::test]
    async fn test_process_notification_no_response() {
        let client = BridgeClient::new(
            "http://localhost:3310".to_string(),
            "fake".to_string(),
            "http://localhost:3312".to_string(),
            "dev-api-key".to_string(),
        );
        let line = r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#;
        let resp = process_jsonrpc_line(&client, line).await;
        assert!(resp.is_none(), "notifications should not get a response");
    }
}
