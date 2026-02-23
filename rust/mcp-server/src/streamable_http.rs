/*
 * Broodlink - Multi-agent AI orchestration system
 * Copyright (C) 2025–2026 Neven Kordic <neven@broodlink.ai>
 * SPDX-License-Identifier: AGPL-3.0-or-later
 */

//! MCP Streamable HTTP transport (2025-03-26 spec).
//!
//! Single `/mcp` endpoint:
//! - POST: JSON-RPC request → JSON response (or 202 for notifications)
//! - GET:  SSE channel for server-initiated messages (returns 405 for legacy clients)
//!
//! Session management via `Mcp-Session-Id` header with configurable timeout.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::process;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::extract::State;
use axum::http::{header, HeaderMap, Method, StatusCode};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::Router;
use tokio::sync::RwLock;
use tokio_stream::wrappers::ReceiverStream;
use tower_http::cors::{AllowOrigin, CorsLayer};
use tracing::{error, info};
use uuid::Uuid;

use crate::handler::{self, BridgeClient};
use crate::protocol;

// ---------------------------------------------------------------------------
// Session state
// ---------------------------------------------------------------------------

struct Session {
    #[allow(dead_code)]
    created_at: Instant,
    last_activity: Instant,
}

pub struct HttpState {
    pub client: BridgeClient,
    sessions: RwLock<HashMap<String, Session>>,
    session_timeout: Duration,
    allowed_origins: Vec<String>,
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

pub async fn run_http_transport(config: Arc<broodlink_config::Config>, client: BridgeClient) {
    let session_timeout_mins = config.mcp_server.session_timeout_minutes;
    let cors_origins = config.mcp_server.cors_origins.clone();

    let state = Arc::new(HttpState {
        client,
        sessions: RwLock::new(HashMap::new()),
        session_timeout: Duration::from_secs(u64::from(session_timeout_mins) * 60),
        allowed_origins: cors_origins.clone(),
    });

    // Spawn session reaper
    let reaper_state = Arc::clone(&state);
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(60));
        loop {
            interval.tick().await;
            let mut sessions = reaper_state.sessions.write().await;
            let timeout = reaper_state.session_timeout;
            let before = sessions.len();
            sessions.retain(|_, s| s.last_activity.elapsed() < timeout);
            let reaped = before - sessions.len();
            if reaped > 0 {
                info!(
                    reaped = reaped,
                    remaining = sessions.len(),
                    "reaped expired sessions"
                );
            }
        }
    });

    // Build CORS layer
    let cors = build_cors_layer(&cors_origins);

    let app = Router::new()
        .route("/mcp", post(mcp_post_handler).get(mcp_get_handler))
        .route("/health", get(health_handler))
        .layer(cors)
        .with_state(state);

    let port = config.mcp_server.port;
    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    info!(addr = %addr, "MCP Streamable HTTP server listening");

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

fn build_cors_layer(origins: &[String]) -> CorsLayer {
    let allowed_headers = [
        header::CONTENT_TYPE,
        header::ACCEPT,
        header::HeaderName::from_static("mcp-session-id"),
    ];

    if origins.is_empty() {
        return CorsLayer::new()
            .allow_methods([Method::GET, Method::POST])
            .allow_headers(allowed_headers)
            .expose_headers([header::HeaderName::from_static("mcp-session-id")]);
    }

    let parsed: Vec<header::HeaderValue> = origins.iter().filter_map(|o| o.parse().ok()).collect();

    CorsLayer::new()
        .allow_origin(AllowOrigin::list(parsed))
        .allow_methods([Method::GET, Method::POST])
        .allow_headers(allowed_headers)
        .expose_headers([header::HeaderName::from_static("mcp-session-id")])
}

// ---------------------------------------------------------------------------
// Origin validation
// ---------------------------------------------------------------------------

fn validate_origin(headers: &HeaderMap, allowed: &[String]) -> Result<(), StatusCode> {
    // If no origins configured, skip validation
    if allowed.is_empty() {
        return Ok(());
    }

    let origin = headers.get(header::ORIGIN).and_then(|v| v.to_str().ok());

    match origin {
        Some(o) if allowed.iter().any(|a| a == o) => Ok(()),
        Some(_) => Err(StatusCode::FORBIDDEN),
        // No Origin header — allow (same-origin requests don't send Origin)
        None => Ok(()),
    }
}

// ---------------------------------------------------------------------------
// POST /mcp — JSON-RPC request handler
// ---------------------------------------------------------------------------

async fn mcp_post_handler(
    State(state): State<Arc<HttpState>>,
    headers: HeaderMap,
    body: String,
) -> Response {
    // Origin check
    if let Err(status) = validate_origin(&headers, &state.allowed_origins) {
        return (status, "forbidden: invalid origin").into_response();
    }

    // Parse JSON-RPC request
    let req: protocol::JsonRpcRequest = match serde_json::from_str(&body) {
        Ok(r) => r,
        Err(e) => {
            let resp = protocol::serialize_response(
                None,
                Err((protocol::PARSE_ERROR, format!("parse error: {e}"))),
            );
            return (
                StatusCode::OK,
                [(header::CONTENT_TYPE, "application/json")],
                resp,
            )
                .into_response();
        }
    };

    let id = req.id.clone();
    let is_notification = id.is_none();
    let is_initialize = req.method == "initialize";

    // Session management
    let session_id = headers
        .get("mcp-session-id")
        .and_then(|v| v.to_str().ok())
        .map(String::from);

    if is_initialize {
        // Generate new session
        let new_session_id = Uuid::new_v4().to_string();
        {
            let mut sessions = state.sessions.write().await;
            sessions.insert(
                new_session_id.clone(),
                Session {
                    created_at: Instant::now(),
                    last_activity: Instant::now(),
                },
            );
        }

        let result = handler::dispatch(&state.client, &req.method, &req.params).await;
        let resp_str = protocol::serialize_response(id, result);

        let mut resp = (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "application/json")],
            resp_str,
        )
            .into_response();
        resp.headers_mut().insert(
            header::HeaderName::from_static("mcp-session-id"),
            match header::HeaderValue::from_str(&new_session_id) {
                Ok(v) => v,
                Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
            },
        );
        return resp;
    }

    // All non-initialize requests require a valid session
    match &session_id {
        Some(sid) => {
            let mut sessions = state.sessions.write().await;
            match sessions.get_mut(sid) {
                Some(session) => {
                    session.last_activity = Instant::now();
                }
                None => {
                    let resp = protocol::serialize_response(
                        id,
                        Err((
                            protocol::INVALID_REQUEST,
                            "invalid or expired session".to_string(),
                        )),
                    );
                    return (
                        StatusCode::BAD_REQUEST,
                        [(header::CONTENT_TYPE, "application/json")],
                        resp,
                    )
                        .into_response();
                }
            }
        }
        None => {
            let resp = protocol::serialize_response(
                id,
                Err((
                    protocol::INVALID_REQUEST,
                    "missing Mcp-Session-Id header".to_string(),
                )),
            );
            return (
                StatusCode::BAD_REQUEST,
                [(header::CONTENT_TYPE, "application/json")],
                resp,
            )
                .into_response();
        }
    }

    // Handle notifications/cancelled
    if req.method == "notifications/cancelled" {
        info!("client cancelled request");
        return StatusCode::ACCEPTED.into_response();
    }

    // Notifications (no id) return 202 Accepted
    if is_notification {
        let _ = handler::dispatch(&state.client, &req.method, &req.params).await;
        return StatusCode::ACCEPTED.into_response();
    }

    // Standard request → dispatch and return JSON
    let result = handler::dispatch(&state.client, &req.method, &req.params).await;
    let resp_str = protocol::serialize_response(id, result);

    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/json")],
        resp_str,
    )
        .into_response()
}

// ---------------------------------------------------------------------------
// GET /mcp — SSE channel for server-initiated messages
// ---------------------------------------------------------------------------

async fn mcp_get_handler(State(state): State<Arc<HttpState>>, headers: HeaderMap) -> Response {
    // Origin check
    if let Err(status) = validate_origin(&headers, &state.allowed_origins) {
        return (status, "forbidden: invalid origin").into_response();
    }

    // Check for legacy SSE client (looking for "endpoint" event)
    // The Accept header for legacy SSE is text/event-stream
    // For streamable HTTP, the client should have a session
    let session_id = headers.get("mcp-session-id").and_then(|v| v.to_str().ok());

    if session_id.is_none() {
        // Could be a legacy client trying the old SSE protocol
        return (
            StatusCode::METHOD_NOT_ALLOWED,
            "This endpoint uses the MCP Streamable HTTP protocol. \
             Use POST /mcp with initialize to start a session.",
        )
            .into_response();
    }

    // Validate session
    {
        let sessions = state.sessions.read().await;
        if !sessions.contains_key(session_id.unwrap_or_default()) {
            return (StatusCode::BAD_REQUEST, "invalid session").into_response();
        }
    }

    // Open SSE channel for server-initiated messages
    let (tx, rx) = tokio::sync::mpsc::channel::<Result<Event, std::convert::Infallible>>(32);

    // Send a keepalive ping event to confirm connection
    tokio::spawn(async move {
        let event = Event::default().event("ping").data("{}");
        let _ = tx.send(Ok(event)).await;
        // Keep channel open until client disconnects
        tokio::time::sleep(tokio::time::Duration::from_secs(3600)).await;
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
        "service": "mcp-server",
        "transport": "streamable-http",
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
    fn test_validate_origin_empty_allowed_passes() {
        let headers = HeaderMap::new();
        assert!(validate_origin(&headers, &[]).is_ok());
    }

    #[test]
    fn test_validate_origin_no_header_passes() {
        let headers = HeaderMap::new();
        let allowed = vec!["http://localhost:1313".to_string()];
        assert!(validate_origin(&headers, &allowed).is_ok());
    }

    #[test]
    fn test_validate_origin_matching_passes() {
        let mut headers = HeaderMap::new();
        headers.insert(header::ORIGIN, "http://localhost:1313".parse().unwrap());
        let allowed = vec!["http://localhost:1313".to_string()];
        assert!(validate_origin(&headers, &allowed).is_ok());
    }

    #[test]
    fn test_validate_origin_mismatch_returns_403() {
        let mut headers = HeaderMap::new();
        headers.insert(header::ORIGIN, "http://evil.com".parse().unwrap());
        let allowed = vec!["http://localhost:1313".to_string()];
        assert_eq!(
            validate_origin(&headers, &allowed),
            Err(StatusCode::FORBIDDEN)
        );
    }

    #[tokio::test]
    async fn test_session_creation_and_lookup() {
        let state = HttpState {
            client: BridgeClient::new(
                "http://localhost:3310".to_string(),
                "fake".to_string(),
                "http://localhost:3312".to_string(),
                "dev-api-key".to_string(),
            ),
            sessions: RwLock::new(HashMap::new()),
            session_timeout: Duration::from_secs(1800),
            allowed_origins: vec![],
        };

        // Insert a session
        let sid = "test-session-123".to_string();
        {
            let mut sessions = state.sessions.write().await;
            sessions.insert(
                sid.clone(),
                Session {
                    created_at: Instant::now(),
                    last_activity: Instant::now(),
                },
            );
        }

        // Verify it exists
        let sessions = state.sessions.read().await;
        assert!(sessions.contains_key(&sid));
        assert_eq!(sessions.len(), 1);
    }

    #[tokio::test]
    async fn test_session_expiry() {
        let state = HttpState {
            client: BridgeClient::new(
                "http://localhost:3310".to_string(),
                "fake".to_string(),
                "http://localhost:3312".to_string(),
                "dev-api-key".to_string(),
            ),
            sessions: RwLock::new(HashMap::new()),
            session_timeout: Duration::from_millis(10),
            allowed_origins: vec![],
        };

        // Insert session
        {
            let mut sessions = state.sessions.write().await;
            sessions.insert(
                "expired".to_string(),
                Session {
                    created_at: Instant::now(),
                    last_activity: Instant::now(),
                },
            );
        }

        // Wait for expiry
        tokio::time::sleep(Duration::from_millis(20)).await;

        // Reap
        let mut sessions = state.sessions.write().await;
        sessions.retain(|_, s| s.last_activity.elapsed() < state.session_timeout);
        assert!(sessions.is_empty(), "expired session should be reaped");
    }
}
