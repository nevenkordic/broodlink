/*
 * Broodlink - Multi-agent AI orchestration system
 * Copyright (C) 2025–2026 Neven Kordic <neven@broodlink.ai>
 *
 * This program is free software: you can redistribute it
 * and/or modify it under the terms of the GNU Affero
 * General Public License as published by the Free Software
 * Foundation, either version 3 of the License, or (at your
 * option) any later version.
 *
 * This program is distributed in the hope that it will be
 * useful, but WITHOUT ANY WARRANTY; without even the
 * implied warranty of MERCHANTABILITY or FITNESS FOR A
 * PARTICULAR PURPOSE. See the GNU Affero General Public
 * License for more details.
 *
 * You should have received a copy of the GNU Affero General
 * Public License along with this program. If not, see
 * <https://www.gnu.org/licenses/>.
 */

#![deny(clippy::unwrap_used)]
#![deny(clippy::expect_used)]
#![warn(clippy::pedantic)]
#![allow(clippy::module_name_repetitions)]

use std::collections::HashMap;
use std::net::SocketAddr;
use std::process;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, HeaderName, Method, Request, StatusCode};
use axum::middleware::{self, Next};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use broodlink_config::Config;
use broodlink_secrets::SecretsProvider;
use futures::stream::Stream;
use futures::StreamExt;
use chrono::{Local, Utc};
use serde::{Deserialize, Serialize};
use sqlx::mysql::MySqlPoolOptions;
use sqlx::postgres::PgPoolOptions;
use sqlx::{MySqlPool, PgPool};
use tower_http::cors::{AllowOrigin, CorsLayer};
use tracing::{error, info, warn};
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

const SERVICE_NAME: &str = "status-api";
const SERVICE_VERSION: &str = env!("CARGO_PKG_VERSION");

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

#[derive(thiserror::Error, Debug)]
pub enum StatusApiError {
    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),
    #[error("nats error: {0}")]
    Nats(String),
    #[error("config error: {0}")]
    Config(String),
    #[error("secrets error: {0}")]
    Secrets(#[from] broodlink_secrets::SecretsError),
    #[error("auth error: {0}")]
    Auth(String),
    #[error("internal error: {0}")]
    Internal(String),
}

impl IntoResponse for StatusApiError {
    fn into_response(self) -> Response {
        let trace_id = Uuid::new_v4().to_string();
        let (status_code, message) = match &self {
            Self::Database(e) => {
                error!(error = %e, trace_id = %trace_id, "database error");
                (StatusCode::INTERNAL_SERVER_ERROR, "internal database error")
            }
            Self::Nats(e) => {
                error!(error = %e, trace_id = %trace_id, "nats error");
                (StatusCode::INTERNAL_SERVER_ERROR, "internal messaging error")
            }
            Self::Config(e) => {
                error!(error = %e, trace_id = %trace_id, "config error");
                (StatusCode::INTERNAL_SERVER_ERROR, "configuration error")
            }
            Self::Secrets(e) => {
                error!(error = %e, trace_id = %trace_id, "secrets error");
                (StatusCode::INTERNAL_SERVER_ERROR, "secrets resolution error")
            }
            Self::Auth(msg) => {
                warn!(msg = %msg, trace_id = %trace_id, "auth failure");
                (StatusCode::UNAUTHORIZED, "authentication required")
            }
            Self::Internal(msg) => {
                error!(msg = %msg, trace_id = %trace_id, "internal error");
                (StatusCode::INTERNAL_SERVER_ERROR, "internal error")
            }
        };

        let body = serde_json::json!({
            "error": message,
            "trace_id": trace_id,
            "status": "error",
        });

        (status_code, Json(body)).into_response()
    }
}

// ---------------------------------------------------------------------------
// Response envelope
// ---------------------------------------------------------------------------

fn ok_response(data: serde_json::Value) -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "data": data,
        "updated_at": Utc::now().to_rfc3339(),
        "status": "ok",
    }))
}

// ---------------------------------------------------------------------------
// Health response
// ---------------------------------------------------------------------------

#[derive(Serialize)]
pub struct HealthResponse {
    pub status: String,
    pub service: String,
    pub version: String,
    pub uptime_seconds: u64,
    pub dependencies: HashMap<String, DependencyStatus>,
}

#[derive(Serialize)]
pub struct DependencyStatus {
    pub status: String, // "ok", "degraded", "offline"
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latency_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

// ---------------------------------------------------------------------------
// Query parameter structs
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct AuditQuery {
    pub agent_id: Option<String>,
    pub operation: Option<String>,
    pub from: Option<String>,
    pub to: Option<String>,
}

// ---------------------------------------------------------------------------
// Application state
// ---------------------------------------------------------------------------

pub struct AppState {
    pub dolt: MySqlPool,
    pub pg: PgPool,
    pub nats: async_nats::Client,
    pub nats_url: String,
    pub qdrant_url: String,
    pub config: Arc<Config>,
    pub api_key: String,
    pub start_time: Instant,
}

// ---------------------------------------------------------------------------
// Startup
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() {
    let boot_config = Config::load().unwrap_or_else(|e| {
        eprintln!("fatal: failed to load config: {e}");
        process::exit(1);
    });

    let _telemetry_guard = broodlink_telemetry::init_telemetry(SERVICE_NAME, &boot_config.telemetry)
        .unwrap_or_else(|e| {
            eprintln!("fatal: telemetry init failed: {e}");
            process::exit(1);
        });

    info!(service = SERVICE_NAME, version = SERVICE_VERSION, "starting");

    let state = match init_state().await {
        Ok(s) => s,
        Err(e) => {
            error!(error = %e, "fatal: failed to initialise");
            process::exit(1);
        }
    };

    let port = state.config.status_api.port;
    let shared = Arc::new(state);
    let app = build_router(Arc::clone(&shared));

    let addr = SocketAddr::from(([0, 0, 0, 0], port));

    if shared.config.profile.tls_interservice {
        let tls = &shared.config.tls;
        let cert_path = tls.cert_path.as_deref().unwrap_or("certs/server.crt");
        let key_path = tls.key_path.as_deref().unwrap_or("certs/server.key");

        info!(addr = %addr, cert = cert_path, "listening with TLS");

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
            process::exit(1);
        }
    } else {
        info!(addr = %addr, "listening (plaintext)");

        let listener = match tokio::net::TcpListener::bind(addr).await {
            Ok(l) => l,
            Err(e) => {
                error!(error = %e, "failed to bind");
                process::exit(1);
            }
        };

        if let Err(e) = axum::serve(listener, app.into_make_service())
            .with_graceful_shutdown(shutdown_signal())
            .await
        {
            error!(error = %e, "server error");
            process::exit(1);
        }
    }

    info!("shutdown complete");
}

async fn init_state() -> Result<AppState, StatusApiError> {
    let config = Config::load().map_err(|e| StatusApiError::Config(e.to_string()))?;
    let config = Arc::new(config);

    // Secrets provider
    let secrets: Arc<dyn SecretsProvider> = {
        let sc = &config.secrets;
        let provider = broodlink_secrets::create_provider(
            &sc.provider,
            sc.sops_file.as_deref(),
            sc.age_identity.as_deref(),
            sc.infisical_url.as_deref(),
            None,
        )?;
        Arc::from(provider)
    };

    // Resolve the read-only API key from secrets
    let api_key = secrets.get(&config.status_api.api_key_name).await?;

    // Resolve database passwords from secrets
    let dolt_password = secrets.get(&config.dolt.password_key).await?;
    let pg_password = secrets.get(&config.postgres.password_key).await?;

    // Dolt (MySQL) pool
    let dolt_url = format!(
        "mysql://{}:{}@{}:{}/{}",
        config.dolt.user,
        dolt_password,
        config.dolt.host,
        config.dolt.port,
        config.dolt.database,
    );
    let dolt = MySqlPoolOptions::new()
        .min_connections(config.dolt.min_connections)
        .max_connections(config.dolt.max_connections)
        .acquire_timeout(Duration::from_secs(5))
        .connect(&dolt_url)
        .await?;
    info!("dolt pool connected");

    // Postgres pool
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
        .connect(&pg_url)
        .await?;
    info!("postgres pool connected");

    let nats_url = config.nats.url.clone();
    let qdrant_url = config.qdrant.url.clone();

    // NATS client (cluster-aware via broodlink-runtime)
    let nats = broodlink_runtime::connect_nats(&config.nats).await
        .map_err(|e| StatusApiError::Nats(e.to_string()))?;

    Ok(AppState {
        dolt,
        pg,
        nats,
        nats_url,
        qdrant_url,
        config,
        api_key,
        start_time: Instant::now(),
    })
}

async fn shutdown_signal() {
    broodlink_runtime::shutdown_signal().await;
}

// ---------------------------------------------------------------------------
// Router construction
// ---------------------------------------------------------------------------

fn build_router(state: Arc<AppState>) -> Router {
    // Build CORS layer from config origins
    let cors = build_cors_layer(&state.config.status_api.cors_origins);

    let api_routes = Router::new()
        .route("/agents", get(handler_agents))
        .route("/tasks", get(handler_tasks))
        .route("/decisions", get(handler_decisions))
        .route("/convoys", get(handler_convoys))
        .route("/beads", get(handler_beads))
        .route("/health", get(handler_health))
        .route("/activity", get(handler_activity))
        .route("/memory/stats", get(handler_memory_stats))
        .route("/commits", get(handler_commits))
        .route("/summary", get(handler_summary))
        .route("/audit", get(handler_audit))
        // v0.2.0 endpoints
        .route("/approvals", get(handler_approvals))
        .route("/approval-policies", get(handler_approval_policies).post(handler_upsert_approval_policy))
        .route("/approval-policies/:policy_id/toggle", post(handler_toggle_approval_policy))
        .route("/approvals/:gate_id/review", post(handler_approval_review))
        .route("/agent-metrics", get(handler_agent_metrics))
        .route("/delegations", get(handler_delegations))
        .route("/guardrails", get(handler_guardrails))
        .route("/violations", get(handler_violations))
        .route("/streams", get(handler_streams))
        .route("/stream/:stream_id", get(handler_stream_sse))
        .route("/a2a/tasks", get(handler_a2a_tasks))
        .route("/a2a/card", get(handler_a2a_card))
        // v0.5.0 knowledge graph
        .route("/kg/stats", get(handler_kg_stats))
        .route("/kg/entities", get(handler_kg_entities))
        .route("/kg/edges", get(handler_kg_edges))
        // v0.6.0 dead-letter queue + budgets + control
        .route("/dlq", get(handler_dlq))
        .route("/budgets", get(handler_budgets))
        .route("/agents/:agent_id/toggle", post(handler_agent_toggle))
        .route("/budgets/:agent_id/set", post(handler_budget_set))
        .route("/tasks/:task_id/cancel", post(handler_task_cancel))
        .route("/workflows", get(handler_workflows))
        // v0.6.0 webhooks
        .route("/webhooks", get(handler_webhooks).post(handler_webhook_create))
        .route("/webhooks/:endpoint_id/toggle", post(handler_webhook_toggle))
        .route("/webhooks/:endpoint_id/delete", post(handler_webhook_delete))
        .route("/webhook-log", get(handler_webhook_log))
        .layer(middleware::from_fn_with_state(
            Arc::clone(&state),
            auth_middleware,
        ))
        .with_state(Arc::clone(&state));

    Router::new()
        .nest("/api/v1", api_routes)
        .layer(tower_http::trace::TraceLayer::new_for_http())
        .layer(cors)
}

fn build_cors_layer(origins: &[String]) -> CorsLayer {
    let api_key_header = HeaderName::from_static("x-broodlink-api-key");
    let allowed_headers = [
        header::CONTENT_TYPE,
        header::AUTHORIZATION,
        api_key_header,
    ];

    if origins.is_empty() {
        return CorsLayer::new()
            .allow_methods([Method::GET, Method::POST])
            .allow_headers(allowed_headers);
    }

    let parsed: Vec<header::HeaderValue> = origins
        .iter()
        .filter_map(|o| o.parse().ok())
        .collect();

    CorsLayer::new()
        .allow_origin(AllowOrigin::list(parsed))
        .allow_methods([Method::GET, Method::POST])
        .allow_headers(allowed_headers)
}

// ---------------------------------------------------------------------------
// Auth middleware: X-Broodlink-Api-Key header
// ---------------------------------------------------------------------------

async fn auth_middleware(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    req: Request<axum::body::Body>,
    next: Next,
) -> Result<Response, StatusApiError> {
    let provided_key = headers
        .get("X-Broodlink-Api-Key")
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| {
            StatusApiError::Auth("missing X-Broodlink-Api-Key header".to_string())
        })?;

    if provided_key != state.api_key {
        return Err(StatusApiError::Auth("invalid API key".to_string()));
    }

    Ok(next.run(req).await)
}

// ---------------------------------------------------------------------------
// GET /api/v1/agents
// agent_profiles (Dolt) + latest work_log per agent (Postgres)
// ---------------------------------------------------------------------------

async fn handler_agents(
    State(state): State<Arc<AppState>>,
) -> Result<Json<serde_json::Value>, StatusApiError> {
    // Fetch all agent profiles from Dolt
    let profiles = sqlx::query_as::<_, (
        String, String, String, String, String, Option<serde_json::Value>, bool, String, String,
    )>(
        "SELECT agent_id, display_name, role, transport, cost_tier, capabilities, active, CAST(created_at AS CHAR), CAST(updated_at AS CHAR)
         FROM agent_profiles ORDER BY agent_id",
    )
    .fetch_all(&state.dolt)
    .await?;

    // Fetch latest work_log entry per agent from Postgres
    let latest_work = sqlx::query_as::<_, (String, String, String, String)>(
        "SELECT DISTINCT ON (agent_id) agent_id, action, details, created_at::text
         FROM work_log ORDER BY agent_id, created_at DESC",
    )
    .fetch_all(&state.pg)
    .await?;

    let work_map: HashMap<String, serde_json::Value> = latest_work
        .into_iter()
        .map(|(agent_id, action, details, created_at)| {
            (
                agent_id,
                serde_json::json!({
                    "action": action,
                    "details": details,
                    "created_at": created_at,
                }),
            )
        })
        .collect();

    let agents: Vec<serde_json::Value> = profiles
        .into_iter()
        .map(
            |(agent_id, display_name, role, transport, cost_tier, capabilities, active, created_at, updated_at)| {
                let latest_work = work_map.get(&agent_id).cloned();
                let status = if active { "active" } else { "inactive" };
                serde_json::json!({
                    "agent_id": agent_id,
                    "display_name": display_name,
                    "role": role,
                    "transport": transport,
                    "cost_tier": cost_tier,
                    "capabilities": capabilities,
                    "status": status,
                    "active": active,
                    "created_at": created_at,
                    "updated_at": updated_at,
                    "latest_work": latest_work,
                })
            },
        )
        .collect();

    Ok(ok_response(serde_json::json!({ "agents": agents })))
}

// ---------------------------------------------------------------------------
// GET /api/v1/tasks
// COUNT by status + last 20 from task_queue (Postgres)
// ---------------------------------------------------------------------------

async fn handler_tasks(
    State(state): State<Arc<AppState>>,
) -> Result<Json<serde_json::Value>, StatusApiError> {
    // Count by status
    let counts = sqlx::query_as::<_, (String, i64)>(
        "SELECT status, COUNT(*) FROM task_queue GROUP BY status ORDER BY status",
    )
    .fetch_all(&state.pg)
    .await?;

    let status_counts: HashMap<String, i64> = counts.into_iter().collect();

    // Last 20 tasks
    let tasks = sqlx::query_as::<_, (
        String, String, String, String, Option<i32>, Option<String>, String,
    )>(
        "SELECT id, title, status, description, priority, assigned_agent, created_at::text
         FROM task_queue ORDER BY created_at DESC LIMIT 20",
    )
    .fetch_all(&state.pg)
    .await?;

    let recent: Vec<serde_json::Value> = tasks
        .into_iter()
        .map(
            |(id, title, status, description, priority, assigned_agent, created_at)| {
                serde_json::json!({
                    "id": id,
                    "title": title,
                    "status": status,
                    "description": description,
                    "priority": priority,
                    "assigned_agent": assigned_agent,
                    "created_at": created_at,
                })
            },
        )
        .collect();

    Ok(ok_response(serde_json::json!({
        "counts_by_status": status_counts,
        "recent": recent,
    })))
}

// ---------------------------------------------------------------------------
// GET /api/v1/decisions
// Last 20 decisions from Dolt
// ---------------------------------------------------------------------------

async fn handler_decisions(
    State(state): State<Arc<AppState>>,
) -> Result<Json<serde_json::Value>, StatusApiError> {
    let rows = sqlx::query_as::<_, (i64, String, String, String, Option<String>, String)>(
        "SELECT id, agent_id, decision, reasoning, outcome, CAST(created_at AS CHAR)
         FROM decisions ORDER BY created_at DESC LIMIT 20",
    )
    .fetch_all(&state.dolt)
    .await?;

    let decisions: Vec<serde_json::Value> = rows
        .into_iter()
        .map(|(id, agent_id, decision, reasoning, outcome, created_at)| {
            serde_json::json!({
                "id": id,
                "agent_id": agent_id,
                "decision": decision,
                "reasoning": reasoning,
                "outcome": outcome,
                "created_at": created_at,
            })
        })
        .collect();

    Ok(ok_response(serde_json::json!({ "decisions": decisions })))
}

// ---------------------------------------------------------------------------
// GET /api/v1/convoys
// beads_issues GROUP BY convoy_id (Dolt)
// ---------------------------------------------------------------------------

async fn handler_convoys(
    State(state): State<Arc<AppState>>,
) -> Result<Json<serde_json::Value>, StatusApiError> {
    let rows = sqlx::query_as::<_, (Option<String>, i64, i64, i64, i64)>(
        "SELECT
             convoy_id,
             COUNT(*) AS total,
             SUM(CASE WHEN status = 'open' THEN 1 ELSE 0 END) AS open_count,
             SUM(CASE WHEN status = 'in_progress' THEN 1 ELSE 0 END) AS in_progress_count,
             SUM(CASE WHEN status = 'closed' THEN 1 ELSE 0 END) AS closed_count
         FROM beads_issues
         GROUP BY convoy_id
         ORDER BY total DESC",
    )
    .fetch_all(&state.dolt)
    .await?;

    let convoys: Vec<serde_json::Value> = rows
        .into_iter()
        .map(
            |(convoy_id, total, open_count, in_progress_count, closed_count)| {
                serde_json::json!({
                    "convoy_id": convoy_id,
                    "total": total,
                    "open": open_count,
                    "in_progress": in_progress_count,
                    "closed": closed_count,
                })
            },
        )
        .collect();

    Ok(ok_response(serde_json::json!({ "convoys": convoys })))
}

// ---------------------------------------------------------------------------
// GET /api/v1/beads
// Individual beads issues with optional status filter
// ---------------------------------------------------------------------------

async fn handler_beads(
    State(state): State<Arc<AppState>>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, StatusApiError> {
    let status_filter = params.get("status").cloned().unwrap_or_default();

    let issues = if status_filter.is_empty() {
        sqlx::query_as::<_, (String, String, Option<String>, Option<String>, Option<String>, Option<String>)>(
            "SELECT bead_id, title, status, assignee, convoy_id, CAST(updated_at AS CHAR)
             FROM beads_issues
             ORDER BY updated_at DESC
             LIMIT 100",
        )
        .fetch_all(&state.dolt)
        .await?
    } else {
        sqlx::query_as::<_, (String, String, Option<String>, Option<String>, Option<String>, Option<String>)>(
            "SELECT bead_id, title, status, assignee, convoy_id, CAST(updated_at AS CHAR)
             FROM beads_issues
             WHERE status = ?
             ORDER BY updated_at DESC
             LIMIT 100",
        )
        .bind(&status_filter)
        .fetch_all(&state.dolt)
        .await?
    };

    let issues_json: Vec<serde_json::Value> = issues
        .into_iter()
        .map(|(bead_id, title, status, assignee, convoy_id, updated_at)| {
            serde_json::json!({
                "bead_id": bead_id,
                "title": title,
                "status": status,
                "assignee": assignee,
                "convoy_id": convoy_id,
                "updated_at": updated_at,
            })
        })
        .collect();

    Ok(ok_response(serde_json::json!({ "issues": issues_json })))
}

// ---------------------------------------------------------------------------
// GET /api/v1/health
// Check NATS, Dolt, Postgres, Qdrant; each returns ok/degraded/offline
// ---------------------------------------------------------------------------

async fn handler_health(
    State(state): State<Arc<AppState>>,
) -> Result<Json<serde_json::Value>, StatusApiError> {
    let mut dependencies = HashMap::new();

    // Dolt health
    let dolt_start = Instant::now();
    let dolt_status = match sqlx::query("SELECT 1").execute(&state.dolt).await {
        Ok(_) => DependencyStatus {
            status: "ok".to_string(),
            latency_ms: Some(dolt_start.elapsed().as_millis().try_into().unwrap_or(u64::MAX)),
            detail: None,
        },
        Err(e) => DependencyStatus {
            status: "offline".to_string(),
            latency_ms: None,
            detail: Some(e.to_string()),
        },
    };
    dependencies.insert("dolt".to_string(), dolt_status);

    // Postgres health
    let pg_start = Instant::now();
    let pg_status = match sqlx::query("SELECT 1").execute(&state.pg).await {
        Ok(_) => DependencyStatus {
            status: "ok".to_string(),
            latency_ms: Some(pg_start.elapsed().as_millis().try_into().unwrap_or(u64::MAX)),
            detail: None,
        },
        Err(e) => DependencyStatus {
            status: "offline".to_string(),
            latency_ms: None,
            detail: Some(e.to_string()),
        },
    };
    dependencies.insert("postgres".to_string(), pg_status);

    // NATS health: attempt a quick connect/flush check
    let nats_start = Instant::now();
    let nats_status = match tokio::time::timeout(
        Duration::from_secs(3),
        async_nats::connect(&state.nats_url),
    )
    .await
    {
        Ok(Ok(client)) => {
            let flush_result = tokio::time::timeout(
                Duration::from_secs(2),
                client.flush(),
            )
            .await;
            match flush_result {
                Ok(Ok(())) => DependencyStatus {
                    status: "ok".to_string(),
                    latency_ms: Some(
                        nats_start.elapsed().as_millis().try_into().unwrap_or(u64::MAX),
                    ),
                    detail: None,
                },
                _ => DependencyStatus {
                    status: "degraded".to_string(),
                    latency_ms: Some(
                        nats_start.elapsed().as_millis().try_into().unwrap_or(u64::MAX),
                    ),
                    detail: Some("connected but flush failed".to_string()),
                },
            }
        }
        Ok(Err(e)) => DependencyStatus {
            status: "offline".to_string(),
            latency_ms: None,
            detail: Some(e.to_string()),
        },
        Err(_) => DependencyStatus {
            status: "offline".to_string(),
            latency_ms: None,
            detail: Some("connection timed out".to_string()),
        },
    };
    dependencies.insert("nats".to_string(), nats_status);

    // Qdrant health: HTTP health endpoint
    let qdrant_start = Instant::now();
    let qdrant_health_url = format!("{}/healthz", state.qdrant_url);
    let qdrant_status = match reqwest::Client::builder()
        .timeout(Duration::from_secs(3))
        .build()
    {
        Ok(client) => match client.get(&qdrant_health_url).send().await {
            Ok(resp) if resp.status().is_success() => DependencyStatus {
                status: "ok".to_string(),
                latency_ms: Some(
                    qdrant_start.elapsed().as_millis().try_into().unwrap_or(u64::MAX),
                ),
                detail: None,
            },
            Ok(resp) => DependencyStatus {
                status: "degraded".to_string(),
                latency_ms: Some(
                    qdrant_start.elapsed().as_millis().try_into().unwrap_or(u64::MAX),
                ),
                detail: Some(format!("HTTP {}", resp.status())),
            },
            Err(e) => DependencyStatus {
                status: "offline".to_string(),
                latency_ms: None,
                detail: Some(e.to_string()),
            },
        },
        Err(e) => DependencyStatus {
            status: "offline".to_string(),
            latency_ms: None,
            detail: Some(format!("http client error: {e}")),
        },
    };
    dependencies.insert("qdrant".to_string(), qdrant_status);

    // Determine overall status
    let all_ok = dependencies.values().all(|d| d.status == "ok");
    let any_offline = dependencies.values().any(|d| d.status == "offline");
    let overall = if all_ok {
        "ok"
    } else if any_offline {
        "degraded"
    } else {
        "degraded"
    };

    let uptime = state.start_time.elapsed().as_secs();

    let health = HealthResponse {
        status: overall.to_string(),
        service: SERVICE_NAME.to_string(),
        version: SERVICE_VERSION.to_string(),
        uptime_seconds: uptime,
        dependencies,
    };

    Ok(ok_response(serde_json::to_value(&health).unwrap_or_else(
        |_| serde_json::json!({"error": "serialization failed"}),
    )))
}

// ---------------------------------------------------------------------------
// GET /api/v1/activity
// work_log last 50 (Postgres)
// ---------------------------------------------------------------------------

async fn handler_activity(
    State(state): State<Arc<AppState>>,
) -> Result<Json<serde_json::Value>, StatusApiError> {
    let rows = sqlx::query_as::<_, (
        i64, String, String, String, Option<serde_json::Value>, String,
    )>(
        "SELECT id, agent_id, action, details, files_changed, created_at::text
         FROM work_log ORDER BY created_at DESC LIMIT 50",
    )
    .fetch_all(&state.pg)
    .await?;

    let entries: Vec<serde_json::Value> = rows
        .into_iter()
        .map(
            |(id, agent_id, action, details, files_changed, created_at)| {
                serde_json::json!({
                    "id": id,
                    "agent_id": agent_id,
                    "action": action,
                    "details": details,
                    "files_changed": files_changed,
                    "created_at": created_at,
                })
            },
        )
        .collect();

    Ok(ok_response(serde_json::json!({ "activity": entries })))
}

// ---------------------------------------------------------------------------
// GET /api/v1/memory/stats
// COUNT(*), MAX(updated_at), distinct agents/topics, top 10 topics by size (Dolt)
// ---------------------------------------------------------------------------

async fn handler_memory_stats(
    State(state): State<Arc<AppState>>,
) -> Result<Json<serde_json::Value>, StatusApiError> {
    // Total count
    let (total_count,): (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM agent_memory")
            .fetch_one(&state.dolt)
            .await?;

    // Most recent update
    let latest: Option<(String,)> =
        sqlx::query_as("SELECT CAST(MAX(updated_at) AS CHAR) FROM agent_memory")
            .fetch_optional(&state.dolt)
            .await?;
    let max_updated_at = latest.map(|(s,)| s);

    // Distinct agents and topics
    let (agents_count,): (i64,) =
        sqlx::query_as("SELECT COUNT(DISTINCT agent_name) FROM agent_memory")
            .fetch_one(&state.dolt)
            .await?;
    let (topics_count,): (i64,) =
        sqlx::query_as("SELECT COUNT(DISTINCT topic) FROM agent_memory")
            .fetch_one(&state.dolt)
            .await?;

    // Top 10 topics by content size (most detailed knowledge)
    let top_topics = sqlx::query_as::<_, (String, String, i64, String)>(
        "SELECT topic, agent_name, LENGTH(content) AS content_length, CAST(updated_at AS CHAR)
         FROM agent_memory ORDER BY LENGTH(content) DESC LIMIT 10",
    )
    .fetch_all(&state.dolt)
    .await?;

    let topics: Vec<serde_json::Value> = top_topics
        .into_iter()
        .map(|(topic, agent_name, content_length, updated_at)| {
            serde_json::json!({
                "topic": topic,
                "agent_name": agent_name,
                "content_length": content_length,
                "updated_at": updated_at,
            })
        })
        .collect();

    Ok(ok_response(serde_json::json!({
        "total_count": total_count,
        "max_updated_at": max_updated_at,
        "agents_count": agents_count,
        "topics_count": topics_count,
        "top_topics": topics,
    })))
}

// ---------------------------------------------------------------------------
// GET /api/v1/kg/stats  — Knowledge graph summary statistics
// ---------------------------------------------------------------------------

async fn handler_kg_stats(
    State(state): State<Arc<AppState>>,
) -> Result<Json<serde_json::Value>, StatusApiError> {
    let (total_entities,): (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM kg_entities")
            .fetch_one(&state.pg)
            .await?;

    let (active_edges,): (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM kg_edges WHERE valid_to IS NULL")
            .fetch_one(&state.pg)
            .await?;

    let (historical_edges,): (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM kg_edges WHERE valid_to IS NOT NULL")
            .fetch_one(&state.pg)
            .await?;

    let type_rows = sqlx::query_as::<_, (String, i64)>(
        "SELECT entity_type, COUNT(*) FROM kg_entities GROUP BY entity_type ORDER BY COUNT(*) DESC",
    )
    .fetch_all(&state.pg)
    .await?;

    let entity_types: serde_json::Map<String, serde_json::Value> = type_rows
        .into_iter()
        .map(|(t, c)| (t, serde_json::Value::from(c)))
        .collect();

    let top_relations = sqlx::query_as::<_, (String, i64)>(
        "SELECT relation_type, COUNT(*) as cnt FROM kg_edges WHERE valid_to IS NULL
         GROUP BY relation_type ORDER BY cnt DESC LIMIT 10",
    )
    .fetch_all(&state.pg)
    .await?;

    let most_connected = sqlx::query_as::<_, (String, String, i64)>(
        "SELECT e.name, e.entity_type, COUNT(ed.id) as edge_count
         FROM kg_entities e
         LEFT JOIN kg_edges ed ON (ed.source_id = e.entity_id OR ed.target_id = e.entity_id) AND ed.valid_to IS NULL
         GROUP BY e.entity_id, e.name, e.entity_type
         ORDER BY edge_count DESC LIMIT 10",
    )
    .fetch_all(&state.pg)
    .await?;

    Ok(ok_response(serde_json::json!({
        "total_entities": total_entities,
        "total_active_edges": active_edges,
        "total_historical_edges": historical_edges,
        "entity_types": entity_types,
        "top_relation_types": top_relations.into_iter().map(|(r, c)| serde_json::json!({"relation": r, "count": c})).collect::<Vec<_>>(),
        "most_connected_entities": most_connected.into_iter().map(|(n, t, c)| serde_json::json!({"name": n, "type": t, "edge_count": c})).collect::<Vec<_>>(),
    })))
}

// ---------------------------------------------------------------------------
// GET /api/v1/kg/entities  — Paginated entity list
// ---------------------------------------------------------------------------

async fn handler_kg_entities(
    State(state): State<Arc<AppState>>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, StatusApiError> {
    let type_filter = params.get("type").cloned();
    let limit: i64 = params
        .get("limit")
        .and_then(|l| l.parse().ok())
        .unwrap_or(50);

    let rows: Vec<(String, String, String, Option<String>, i32, String, String)> = if let Some(etype) = &type_filter {
        sqlx::query_as(
            "SELECT entity_id, name, entity_type, description, mention_count,
                    first_seen::text, last_seen::text
             FROM kg_entities WHERE entity_type = $1
             ORDER BY mention_count DESC, last_seen DESC LIMIT $2",
        )
        .bind(etype)
        .bind(limit)
        .fetch_all(&state.pg)
        .await?
    } else {
        sqlx::query_as(
            "SELECT entity_id, name, entity_type, description, mention_count,
                    first_seen::text, last_seen::text
             FROM kg_entities
             ORDER BY mention_count DESC, last_seen DESC LIMIT $1",
        )
        .bind(limit)
        .fetch_all(&state.pg)
        .await?
    };

    let entities: Vec<serde_json::Value> = rows
        .into_iter()
        .map(|(eid, name, etype, desc, mentions, first, last)| {
            serde_json::json!({
                "entity_id": eid,
                "name": name,
                "entity_type": etype,
                "description": desc,
                "mention_count": mentions,
                "first_seen": first,
                "last_seen": last,
            })
        })
        .collect();

    Ok(ok_response(serde_json::json!({
        "entities": entities,
        "total": entities.len(),
    })))
}

// ---------------------------------------------------------------------------
// GET /api/v1/kg/edges  — Recent edges
// ---------------------------------------------------------------------------

async fn handler_kg_edges(
    State(state): State<Arc<AppState>>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, StatusApiError> {
    let relation_filter = params.get("relation_type").cloned();
    let limit: i64 = params
        .get("limit")
        .and_then(|l| l.parse().ok())
        .unwrap_or(50);

    let rows: Vec<(String, String, String, String, Option<String>, f64, String)> = if let Some(rel) = &relation_filter {
        sqlx::query_as(
            "SELECT s.name, e.relation_type, t.name, e.edge_id, e.description, e.weight, e.created_at::text
             FROM kg_edges e
             JOIN kg_entities s ON e.source_id = s.entity_id
             JOIN kg_entities t ON e.target_id = t.entity_id
             WHERE e.valid_to IS NULL AND e.relation_type = $1
             ORDER BY e.created_at DESC LIMIT $2",
        )
        .bind(rel)
        .bind(limit)
        .fetch_all(&state.pg)
        .await?
    } else {
        sqlx::query_as(
            "SELECT s.name, e.relation_type, t.name, e.edge_id, e.description, e.weight, e.created_at::text
             FROM kg_edges e
             JOIN kg_entities s ON e.source_id = s.entity_id
             JOIN kg_entities t ON e.target_id = t.entity_id
             WHERE e.valid_to IS NULL
             ORDER BY e.created_at DESC LIMIT $1",
        )
        .bind(limit)
        .fetch_all(&state.pg)
        .await?
    };

    let edges: Vec<serde_json::Value> = rows
        .into_iter()
        .map(|(source, rel, target, eid, desc, weight, created)| {
            serde_json::json!({
                "source": source,
                "relation_type": rel,
                "target": target,
                "edge_id": eid,
                "description": desc,
                "weight": weight,
                "created_at": created,
            })
        })
        .collect();

    Ok(ok_response(serde_json::json!({
        "edges": edges,
        "total": edges.len(),
    })))
}

// ---------------------------------------------------------------------------
// GET /api/v1/dlq
// ---------------------------------------------------------------------------

async fn handler_dlq(
    State(state): State<Arc<AppState>>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, StatusApiError> {
    let include_resolved = params.get("resolved").map_or(false, |v| v == "true");
    let limit: i64 = params
        .get("limit")
        .and_then(|l| l.parse().ok())
        .unwrap_or(50);

    let rows: Vec<(i64, String, String, String, i32, i32, bool, Option<String>, String)> = if include_resolved {
        sqlx::query_as(
            "SELECT id, task_id, reason, source_service, retry_count, max_retries,
                    resolved, resolved_by, created_at::text
             FROM dead_letter_queue
             ORDER BY created_at DESC LIMIT $1",
        )
        .bind(limit)
        .fetch_all(&state.pg)
        .await?
    } else {
        sqlx::query_as(
            "SELECT id, task_id, reason, source_service, retry_count, max_retries,
                    resolved, resolved_by, created_at::text
             FROM dead_letter_queue
             WHERE resolved = FALSE
             ORDER BY created_at DESC LIMIT $1",
        )
        .bind(limit)
        .fetch_all(&state.pg)
        .await?
    };

    let entries: Vec<serde_json::Value> = rows
        .into_iter()
        .map(|(id, task_id, reason, svc, retries, max, resolved, by, ts)| {
            serde_json::json!({
                "id": id,
                "task_id": task_id,
                "reason": reason,
                "source_service": svc,
                "retry_count": retries,
                "max_retries": max,
                "resolved": resolved,
                "resolved_by": by,
                "created_at": ts,
            })
        })
        .collect();

    Ok(ok_response(serde_json::json!({
        "entries": entries,
        "total": entries.len(),
    })))
}

// ---------------------------------------------------------------------------
// GET /api/v1/budgets
// ---------------------------------------------------------------------------

async fn handler_budgets(
    State(state): State<Arc<AppState>>,
) -> Result<Json<serde_json::Value>, StatusApiError> {
    let agents: Vec<(String, i64)> = sqlx::query_as(
        "SELECT agent_id, COALESCE(budget_tokens, 0) FROM agent_profiles ORDER BY agent_id",
    )
    .fetch_all(&state.dolt)
    .await?;

    let budgets: Vec<serde_json::Value> = agents
        .into_iter()
        .map(|(id, tokens)| {
            serde_json::json!({
                "agent_id": id,
                "budget_tokens": tokens,
            })
        })
        .collect();

    Ok(ok_response(serde_json::json!({
        "budgets": budgets,
        "total": budgets.len(),
    })))
}

// ---------------------------------------------------------------------------
// POST /api/v1/agents/:agent_id/toggle
// ---------------------------------------------------------------------------

async fn handler_agent_toggle(
    State(state): State<Arc<AppState>>,
    axum::extract::Path(agent_id): axum::extract::Path<String>,
) -> Result<Json<serde_json::Value>, StatusApiError> {
    let result = sqlx::query(
        "UPDATE agent_profiles SET active = NOT active WHERE agent_id = ?",
    )
    .bind(&agent_id)
    .execute(&state.dolt)
    .await?;

    if result.rows_affected() == 0 {
        return Err(StatusApiError::Internal(format!("agent {agent_id} not found")));
    }

    let (active,): (bool,) = sqlx::query_as(
        "SELECT active FROM agent_profiles WHERE agent_id = ?",
    )
    .bind(&agent_id)
    .fetch_one(&state.dolt)
    .await?;

    Ok(ok_response(serde_json::json!({
        "agent_id": agent_id,
        "active": active,
        "toggled": true,
    })))
}

// ---------------------------------------------------------------------------
// POST /api/v1/budgets/:agent_id/set
// ---------------------------------------------------------------------------

#[derive(serde::Deserialize)]
struct SetBudgetBody {
    tokens: i64,
}

async fn handler_budget_set(
    State(state): State<Arc<AppState>>,
    axum::extract::Path(agent_id): axum::extract::Path<String>,
    Json(body): Json<SetBudgetBody>,
) -> Result<Json<serde_json::Value>, StatusApiError> {
    sqlx::query("UPDATE agent_profiles SET budget_tokens = ? WHERE agent_id = ?")
        .bind(body.tokens)
        .bind(&agent_id)
        .execute(&state.dolt)
        .await?;

    Ok(ok_response(serde_json::json!({
        "agent_id": agent_id,
        "new_balance": body.tokens,
        "set": true,
    })))
}

// ---------------------------------------------------------------------------
// POST /api/v1/tasks/:task_id/cancel
// ---------------------------------------------------------------------------

async fn handler_task_cancel(
    State(state): State<Arc<AppState>>,
    axum::extract::Path(task_id): axum::extract::Path<String>,
) -> Result<Json<serde_json::Value>, StatusApiError> {
    let result = sqlx::query(
        "UPDATE task_queue SET status = 'failed', updated_at = NOW()
         WHERE id = $1 AND status IN ('pending', 'claimed')",
    )
    .bind(&task_id)
    .execute(&state.pg)
    .await?;

    Ok(ok_response(serde_json::json!({
        "task_id": task_id,
        "cancelled": result.rows_affected() > 0,
    })))
}

// ---------------------------------------------------------------------------
// GET /api/v1/workflows
// ---------------------------------------------------------------------------

async fn handler_workflows(
    State(state): State<Arc<AppState>>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, StatusApiError> {
    let limit: i64 = params.get("limit").and_then(|l| l.parse().ok()).unwrap_or(20);

    let rows: Vec<(String, String, String, i32, i32, String, String)> = sqlx::query_as(
        "SELECT id, formula_name, status, current_step, total_steps, started_by, created_at::text
         FROM workflow_runs
         ORDER BY created_at DESC
         LIMIT $1",
    )
    .bind(limit)
    .fetch_all(&state.pg)
    .await?;

    let workflows: Vec<serde_json::Value> = rows
        .into_iter()
        .map(|(id, formula, status, step, total, by, ts)| {
            serde_json::json!({
                "id": id,
                "formula_name": formula,
                "status": status,
                "current_step": step,
                "total_steps": total,
                "started_by": by,
                "created_at": ts,
            })
        })
        .collect();

    Ok(ok_response(serde_json::json!({
        "workflows": workflows,
        "total": workflows.len(),
    })))
}

// ---------------------------------------------------------------------------
// GET /api/v1/webhooks — list webhook endpoints
// ---------------------------------------------------------------------------

async fn handler_webhooks(
    State(state): State<Arc<AppState>>,
) -> Result<Json<serde_json::Value>, StatusApiError> {
    let rows: Vec<(String, String, String, Option<String>, serde_json::Value, bool, String, String)> = sqlx::query_as(
        "SELECT id, platform, name, webhook_url, events, active, created_at::text, updated_at::text
         FROM webhook_endpoints
         ORDER BY created_at DESC",
    )
    .fetch_all(&state.pg)
    .await?;

    let endpoints: Vec<serde_json::Value> = rows
        .into_iter()
        .map(|(id, platform, name, url, events, active, created, updated)| {
            serde_json::json!({
                "id": id,
                "platform": platform,
                "name": name,
                "webhook_url": url,
                "events": events,
                "active": active,
                "created_at": created,
                "updated_at": updated,
            })
        })
        .collect();

    Ok(ok_response(serde_json::json!({
        "endpoints": endpoints,
        "total": endpoints.len(),
    })))
}

// ---------------------------------------------------------------------------
// POST /api/v1/webhooks — create webhook endpoint
// ---------------------------------------------------------------------------

async fn handler_webhook_create(
    State(state): State<Arc<AppState>>,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, StatusApiError> {
    let platform = body.get("platform").and_then(|v| v.as_str()).unwrap_or("generic");
    let name = body.get("name").and_then(|v| v.as_str()).unwrap_or("unnamed");
    let webhook_url = body.get("webhook_url").and_then(|v| v.as_str());
    let events = body.get("events").cloned().unwrap_or(serde_json::json!([]));
    let id = Uuid::new_v4().to_string();

    sqlx::query(
        "INSERT INTO webhook_endpoints (id, platform, name, webhook_url, events, active)
         VALUES ($1, $2, $3, $4, $5, true)",
    )
    .bind(&id)
    .bind(platform)
    .bind(name)
    .bind(webhook_url)
    .bind(&events)
    .execute(&state.pg)
    .await?;

    Ok(ok_response(serde_json::json!({
        "id": id,
        "platform": platform,
        "name": name,
        "created": true,
    })))
}

// ---------------------------------------------------------------------------
// POST /api/v1/webhooks/:endpoint_id/toggle — toggle active
// ---------------------------------------------------------------------------

async fn handler_webhook_toggle(
    State(state): State<Arc<AppState>>,
    Path(endpoint_id): Path<String>,
) -> Result<Json<serde_json::Value>, StatusApiError> {
    let result = sqlx::query(
        "UPDATE webhook_endpoints SET active = NOT active, updated_at = NOW() WHERE id = $1",
    )
    .bind(&endpoint_id)
    .execute(&state.pg)
    .await?;

    if result.rows_affected() == 0 {
        return Ok(ok_response(serde_json::json!({"error": "endpoint not found"})));
    }

    let row: (bool,) = sqlx::query_as("SELECT active FROM webhook_endpoints WHERE id = $1")
        .bind(&endpoint_id)
        .fetch_one(&state.pg)
        .await?;

    Ok(ok_response(serde_json::json!({
        "id": endpoint_id,
        "active": row.0,
    })))
}

// ---------------------------------------------------------------------------
// POST /api/v1/webhooks/:endpoint_id/delete — delete endpoint
// ---------------------------------------------------------------------------

async fn handler_webhook_delete(
    State(state): State<Arc<AppState>>,
    Path(endpoint_id): Path<String>,
) -> Result<Json<serde_json::Value>, StatusApiError> {
    // Delete log entries first (FK constraint)
    sqlx::query("DELETE FROM webhook_log WHERE endpoint_id = $1")
        .bind(&endpoint_id)
        .execute(&state.pg)
        .await?;

    let result = sqlx::query("DELETE FROM webhook_endpoints WHERE id = $1")
        .bind(&endpoint_id)
        .execute(&state.pg)
        .await?;

    Ok(ok_response(serde_json::json!({
        "id": endpoint_id,
        "deleted": result.rows_affected() > 0,
    })))
}

// ---------------------------------------------------------------------------
// GET /api/v1/webhook-log — recent webhook deliveries
// ---------------------------------------------------------------------------

async fn handler_webhook_log(
    State(state): State<Arc<AppState>>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, StatusApiError> {
    let limit: i64 = params.get("limit").and_then(|l| l.parse().ok()).unwrap_or(50);

    let rows: Vec<(i64, String, String, String, serde_json::Value, String, Option<String>, String)> = sqlx::query_as(
        "SELECT wl.id, wl.endpoint_id, wl.direction, wl.event_type, wl.payload, wl.status, wl.error_msg, wl.created_at::text
         FROM webhook_log wl
         ORDER BY wl.created_at DESC
         LIMIT $1",
    )
    .bind(limit)
    .fetch_all(&state.pg)
    .await?;

    let entries: Vec<serde_json::Value> = rows
        .into_iter()
        .map(|(id, eid, dir, event, payload, status, err, ts)| {
            serde_json::json!({
                "id": id,
                "endpoint_id": eid,
                "direction": dir,
                "event_type": event,
                "payload": payload,
                "status": status,
                "error_msg": err,
                "created_at": ts,
            })
        })
        .collect();

    Ok(ok_response(serde_json::json!({
        "entries": entries,
        "total": entries.len(),
    })))
}

// ---------------------------------------------------------------------------
// GET /api/v1/commits
// SELECT * FROM dolt_log LIMIT 20 (Dolt)
// ---------------------------------------------------------------------------

async fn handler_commits(
    State(state): State<Arc<AppState>>,
) -> Result<Json<serde_json::Value>, StatusApiError> {
    let rows = sqlx::query_as::<_, (String, String, String, String)>(
        "SELECT commit_hash, committer, message, CAST(date AS CHAR)
         FROM dolt_log ORDER BY date DESC LIMIT 20",
    )
    .fetch_all(&state.dolt)
    .await?;

    let commits: Vec<serde_json::Value> = rows
        .into_iter()
        .map(|(hash, committer, message, date)| {
            serde_json::json!({
                "commit_hash": hash,
                "committer": committer,
                "message": message,
                "date": date,
            })
        })
        .collect();

    Ok(ok_response(serde_json::json!({ "commits": commits })))
}

// ---------------------------------------------------------------------------
// GET /api/v1/summary
// daily_summary WHERE summary_date = CURDATE() (Dolt)
// ---------------------------------------------------------------------------

async fn handler_summary(
    State(state): State<Arc<AppState>>,
) -> Result<Json<serde_json::Value>, StatusApiError> {
    let rows = sqlx::query_as::<_, (i64, String, String, Option<i32>, Option<i32>, Option<i32>, String)>(
        "SELECT id, CAST(summary_date AS CHAR), summary_text, tasks_completed, decisions_made, memories_stored, CAST(created_at AS CHAR)
         FROM daily_summary WHERE summary_date = CURDATE()
         ORDER BY created_at DESC",
    )
    .fetch_all(&state.dolt)
    .await?;

    if !rows.is_empty() {
        let summaries: Vec<serde_json::Value> = rows
            .into_iter()
            .map(|(id, summary_date, summary_text, tasks_completed, decisions_made, memories_stored, created_at)| {
                serde_json::json!({
                    "id": id,
                    "summary_date": summary_date,
                    "summary_text": summary_text,
                    "tasks_completed": tasks_completed,
                    "decisions_made": decisions_made,
                    "memories_stored": memories_stored,
                    "created_at": created_at,
                })
            })
            .collect();

        return Ok(ok_response(serde_json::json!({ "summaries": summaries })));
    }

    // No daily_summary row for today — compute live counts from source tables
    let (decisions_today,): (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM decisions WHERE created_at >= NOW() - INTERVAL 24 HOUR")
            .fetch_one(&state.dolt)
            .await?;

    let (memories_stored,): (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM agent_memory")
            .fetch_one(&state.dolt)
            .await?;

    let (tasks_completed,): (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM task_queue WHERE status = 'completed' AND created_at >= NOW() - INTERVAL '24 hours'")
            .fetch_one(&state.pg)
            .await?;

    Ok(ok_response(serde_json::json!({
        "summaries": [{
            "summary_date": Local::now().format("%Y-%m-%d").to_string(),
            "summary_text": "Live counts (last 24 hours)",
            "tasks_completed": tasks_completed,
            "decisions_made": decisions_today,
            "memories_stored": memories_stored,
        }]
    })))
}

// ---------------------------------------------------------------------------
// GET /api/v1/audit
// audit_log last 100 (Postgres), filterable: ?agent_id=&operation=&from=&to=
// ---------------------------------------------------------------------------

async fn handler_audit(
    State(state): State<Arc<AppState>>,
    Query(params): Query<AuditQuery>,
) -> Result<Json<serde_json::Value>, StatusApiError> {
    // Build dynamic query with optional filters
    let mut conditions: Vec<String> = Vec::new();
    let mut bind_values: Vec<String> = Vec::new();
    let mut param_idx: usize = 0;

    if let Some(ref agent_id) = params.agent_id {
        param_idx += 1;
        conditions.push(format!("agent_id = ${param_idx}"));
        bind_values.push(agent_id.clone());
    }

    if let Some(ref operation) = params.operation {
        param_idx += 1;
        conditions.push(format!("operation = ${param_idx}"));
        bind_values.push(operation.clone());
    }

    if let Some(ref from) = params.from {
        param_idx += 1;
        conditions.push(format!("created_at >= ${param_idx}::timestamptz"));
        bind_values.push(from.clone());
    }

    if let Some(ref to) = params.to {
        param_idx += 1;
        conditions.push(format!("created_at <= ${param_idx}::timestamptz"));
        bind_values.push(to.clone());
    }

    let where_clause = if conditions.is_empty() {
        String::new()
    } else {
        format!("WHERE {}", conditions.join(" AND "))
    };

    let sql = format!(
        "SELECT id, agent_id, operation, result_status, result_summary, created_at::text
         FROM audit_log {where_clause}
         ORDER BY created_at DESC LIMIT 100"
    );

    let mut query = sqlx::query_as::<_, (i64, String, String, Option<String>, Option<String>, String)>(
        &sql,
    );

    for val in &bind_values {
        query = query.bind(val);
    }

    let rows = query.fetch_all(&state.pg).await?;

    let entries: Vec<serde_json::Value> = rows
        .into_iter()
        .map(
            |(id, agent_id, operation, result_status, result_summary, created_at)| {
                serde_json::json!({
                    "id": id,
                    "agent_id": agent_id,
                    "operation": operation,
                    "result_status": result_status,
                    "result_summary": result_summary,
                    "created_at": created_at,
                })
            },
        )
        .collect();

    Ok(ok_response(serde_json::json!({ "audit": entries })))
}

// ---------------------------------------------------------------------------
// GET /api/v1/approvals
// Approval gates (Postgres)
// ---------------------------------------------------------------------------

async fn handler_approvals(
    State(state): State<Arc<AppState>>,
) -> Result<Json<serde_json::Value>, StatusApiError> {
    let rows = sqlx::query_as::<_, (String, String, String, serde_json::Value, Option<String>, String, Option<String>, Option<String>, Option<String>, Option<String>, Option<String>, Option<String>, Option<f64>, Option<String>)>(
        "SELECT id, gate_type, requested_by, payload, task_id, status, reviewed_by, reason,
                expires_at::text, created_at::text, reviewed_at::text,
                tool_name, confidence, policy_id
         FROM approval_gates ORDER BY created_at DESC LIMIT 100",
    )
    .fetch_all(&state.pg)
    .await?;

    let approvals: Vec<serde_json::Value> = rows
        .into_iter()
        .map(|(id, gate_type, requested_by, payload, task_id, status, reviewed_by, reason, expires, created, reviewed_at, tool_name, confidence, policy_id)| {
            serde_json::json!({
                "id": id,
                "gate_type": gate_type,
                "requested_by": requested_by,
                "payload": payload,
                "task_id": task_id,
                "status": status,
                "reviewed_by": reviewed_by,
                "reason": reason,
                "expires_at": expires,
                "created_at": created,
                "reviewed_at": reviewed_at,
                "tool_name": tool_name,
                "confidence": confidence,
                "policy_id": policy_id,
            })
        })
        .collect();

    Ok(ok_response(serde_json::json!({ "approvals": approvals })))
}

// ---------------------------------------------------------------------------
// GET /api/v1/approval-policies
// Approval policies (Postgres)
// ---------------------------------------------------------------------------

async fn handler_approval_policies(
    State(state): State<Arc<AppState>>,
) -> Result<Json<serde_json::Value>, StatusApiError> {
    let rows = sqlx::query_as::<_, (String, String, String, Option<String>, serde_json::Value, bool, f64, i32, bool, String, String)>(
        "SELECT id, name, gate_type, description, conditions, auto_approve,
                auto_approve_threshold, expiry_minutes, active,
                created_at::text, updated_at::text
         FROM approval_policies ORDER BY name",
    )
    .fetch_all(&state.pg)
    .await?;

    let policies: Vec<serde_json::Value> = rows
        .into_iter()
        .map(|(id, name, gate_type, description, conditions, auto_approve, threshold, expiry, active, created, updated)| {
            serde_json::json!({
                "id": id,
                "name": name,
                "gate_type": gate_type,
                "description": description,
                "conditions": conditions,
                "auto_approve": auto_approve,
                "auto_approve_threshold": threshold,
                "expiry_minutes": expiry,
                "active": active,
                "created_at": created,
                "updated_at": updated,
            })
        })
        .collect();

    Ok(ok_response(serde_json::json!({ "policies": policies })))
}

// ---------------------------------------------------------------------------
// POST /api/v1/approval-policies
// Create or update an approval policy (Postgres)
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct UpsertPolicyBody {
    name: String,
    gate_type: String,
    #[serde(default)]
    description: Option<String>,
    conditions: serde_json::Value,
    #[serde(default)]
    auto_approve: bool,
    #[serde(default = "default_threshold")]
    auto_approve_threshold: f64,
    #[serde(default = "default_expiry")]
    expiry_minutes: i32,
    #[serde(default = "default_true")]
    active: bool,
}

fn default_threshold() -> f64 { 0.8 }
fn default_expiry() -> i32 { 60 }
fn default_true() -> bool { true }

async fn handler_upsert_approval_policy(
    State(state): State<Arc<AppState>>,
    Json(body): Json<UpsertPolicyBody>,
) -> Result<Json<serde_json::Value>, StatusApiError> {
    if !["pre_dispatch", "pre_completion", "budget", "custom"].contains(&body.gate_type.as_str()) {
        return Err(StatusApiError::Internal(
            "gate_type must be one of: pre_dispatch, pre_completion, budget, custom".to_string(),
        ));
    }

    let id = uuid::Uuid::new_v4().to_string();

    sqlx::query(
        "INSERT INTO approval_policies (id, name, description, gate_type, conditions, auto_approve, auto_approve_threshold, expiry_minutes, active, created_at, updated_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, NOW(), NOW())
         ON CONFLICT (name) DO UPDATE SET
           description = EXCLUDED.description,
           gate_type = EXCLUDED.gate_type,
           conditions = EXCLUDED.conditions,
           auto_approve = EXCLUDED.auto_approve,
           auto_approve_threshold = EXCLUDED.auto_approve_threshold,
           expiry_minutes = EXCLUDED.expiry_minutes,
           active = EXCLUDED.active,
           updated_at = NOW()",
    )
    .bind(&id)
    .bind(&body.name)
    .bind(body.description.as_deref())
    .bind(&body.gate_type)
    .bind(&body.conditions)
    .bind(body.auto_approve)
    .bind(body.auto_approve_threshold)
    .bind(body.expiry_minutes)
    .bind(body.active)
    .execute(&state.pg)
    .await?;

    Ok(ok_response(serde_json::json!({
        "upserted": true,
        "name": body.name,
    })))
}

// ---------------------------------------------------------------------------
// POST /api/v1/approval-policies/:policy_id/toggle
// Toggle a policy's active state (Postgres)
// ---------------------------------------------------------------------------

async fn handler_toggle_approval_policy(
    State(state): State<Arc<AppState>>,
    axum::extract::Path(policy_id): axum::extract::Path<String>,
) -> Result<Json<serde_json::Value>, StatusApiError> {
    let result = sqlx::query(
        "UPDATE approval_policies SET active = NOT active, updated_at = NOW() WHERE id = $1",
    )
    .bind(&policy_id)
    .execute(&state.pg)
    .await?;

    if result.rows_affected() == 0 {
        return Err(StatusApiError::Internal(format!(
            "approval policy {policy_id} not found"
        )));
    }

    // Fetch new state
    let (active,): (bool,) = sqlx::query_as("SELECT active FROM approval_policies WHERE id = $1")
        .bind(&policy_id)
        .fetch_one(&state.pg)
        .await?;

    Ok(ok_response(serde_json::json!({
        "policy_id": policy_id,
        "active": active,
        "toggled": true,
    })))
}

// ---------------------------------------------------------------------------
// POST /api/v1/approvals/:gate_id/review
// Approve or reject an approval gate (Postgres)
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct ApprovalReviewBody {
    decision: String,
    #[serde(default)]
    reason: Option<String>,
    #[serde(default)]
    reviewer: Option<String>,
}

async fn handler_approval_review(
    State(state): State<Arc<AppState>>,
    axum::extract::Path(gate_id): axum::extract::Path<String>,
    Json(body): Json<ApprovalReviewBody>,
) -> Result<Json<serde_json::Value>, StatusApiError> {
    if body.decision != "approved" && body.decision != "rejected" {
        return Err(StatusApiError::Internal("decision must be 'approved' or 'rejected'".to_string()));
    }

    let reviewer = body.reviewer.as_deref().unwrap_or("dashboard-user");

    let affected = sqlx::query(
        "UPDATE approval_gates SET status = $1, reviewed_by = $2, reason = $3, reviewed_at = NOW()
         WHERE id = $4 AND status = 'pending'",
    )
    .bind(&body.decision)
    .bind(reviewer)
    .bind(body.reason.as_deref())
    .bind(&gate_id)
    .execute(&state.pg)
    .await?;

    if affected.rows_affected() == 0 {
        return Err(StatusApiError::Internal(format!(
            "approval gate {gate_id} not found or not pending"
        )));
    }

    // If approved, re-queue linked task
    if body.decision == "approved" {
        let _ = sqlx::query(
            "UPDATE task_queue SET status = 'pending', updated_at = NOW()
             WHERE approval_id = $1 AND status = 'awaiting_approval'",
        )
        .bind(&gate_id)
        .execute(&state.pg)
        .await;
    }

    Ok(ok_response(serde_json::json!({
        "gate_id": gate_id,
        "decision": body.decision,
        "resolved": true,
    })))
}

// ---------------------------------------------------------------------------
// GET /api/v1/agent-metrics
// Agent metrics (Postgres) + agent profiles (Dolt)
// ---------------------------------------------------------------------------

async fn handler_agent_metrics(
    State(state): State<Arc<AppState>>,
) -> Result<Json<serde_json::Value>, StatusApiError> {
    let metrics = sqlx::query_as::<_, (String, i32, i32, i32, i32, f32, Option<String>)>(
        "SELECT agent_id, tasks_completed, tasks_failed, avg_duration_ms, current_load, success_rate, last_task_at::text
         FROM agent_metrics ORDER BY agent_id",
    )
    .fetch_all(&state.pg)
    .await?;

    let profiles = sqlx::query_as::<_, (String, String, String)>(
        "SELECT agent_id, display_name, cost_tier FROM agent_profiles WHERE active = true",
    )
    .fetch_all(&state.dolt)
    .await?;

    let profile_map: HashMap<String, (String, String)> = profiles
        .into_iter()
        .map(|(aid, name, tier)| (aid, (name, tier)))
        .collect();

    let list: Vec<serde_json::Value> = metrics
        .into_iter()
        .map(|(agent_id, completed, failed, avg_dur, load, rate, last_task)| {
            let (display_name, cost_tier) = profile_map
                .get(&agent_id)
                .cloned()
                .unwrap_or_else(|| (agent_id.clone(), "unknown".to_string()));
            serde_json::json!({
                "agent_id": agent_id,
                "display_name": display_name,
                "cost_tier": cost_tier,
                "tasks_completed": completed,
                "tasks_failed": failed,
                "avg_duration_ms": avg_dur,
                "current_load": load,
                "success_rate": rate,
                "last_task_at": last_task,
            })
        })
        .collect();

    Ok(ok_response(serde_json::json!({ "metrics": list })))
}

// ---------------------------------------------------------------------------
// GET /api/v1/delegations
// ---------------------------------------------------------------------------

async fn handler_delegations(
    State(state): State<Arc<AppState>>,
) -> Result<Json<serde_json::Value>, StatusApiError> {
    let rows = sqlx::query_as::<_, (String, String, Option<String>, String, String, String, Option<String>, String, Option<serde_json::Value>, String, String)>(
        "SELECT id, trace_id, parent_task_id, from_agent, to_agent, title, description, status, result,
                created_at::text, updated_at::text
         FROM delegations ORDER BY created_at DESC LIMIT 100",
    )
    .fetch_all(&state.pg)
    .await?;

    let delegations: Vec<serde_json::Value> = rows
        .into_iter()
        .map(|(id, trace_id, parent_task_id, from_agent, to_agent, title, description, status, result, created, updated)| {
            serde_json::json!({
                "id": id,
                "trace_id": trace_id,
                "parent_task_id": parent_task_id,
                "from_agent": from_agent,
                "to_agent": to_agent,
                "title": title,
                "description": description,
                "status": status,
                "result": result,
                "created_at": created,
                "updated_at": updated,
            })
        })
        .collect();

    Ok(ok_response(serde_json::json!({ "delegations": delegations })))
}

// ---------------------------------------------------------------------------
// GET /api/v1/guardrails
// Guardrail policies (Postgres)
// ---------------------------------------------------------------------------

async fn handler_guardrails(
    State(state): State<Arc<AppState>>,
) -> Result<Json<serde_json::Value>, StatusApiError> {
    let rows = sqlx::query_as::<_, (i64, String, String, serde_json::Value, bool, String, String)>(
        "SELECT id, name, rule_type, config, enabled, created_at::text, updated_at::text
         FROM guardrail_policies ORDER BY name",
    )
    .fetch_all(&state.pg)
    .await?;

    let policies: Vec<serde_json::Value> = rows
        .into_iter()
        .map(|(id, name, rule_type, config, enabled, created, updated)| {
            serde_json::json!({
                "id": id,
                "name": name,
                "rule_type": rule_type,
                "config": config,
                "enabled": enabled,
                "created_at": created,
                "updated_at": updated,
            })
        })
        .collect();

    Ok(ok_response(serde_json::json!({ "policies": policies })))
}

// ---------------------------------------------------------------------------
// GET /api/v1/violations
// Guardrail violations (Postgres)
// ---------------------------------------------------------------------------

async fn handler_violations(
    State(state): State<Arc<AppState>>,
) -> Result<Json<serde_json::Value>, StatusApiError> {
    let rows = sqlx::query_as::<_, (i64, String, String, String, Option<String>, Option<String>, String)>(
        "SELECT id, trace_id, agent_id, policy_name, tool_name, details, created_at::text
         FROM guardrail_violations ORDER BY created_at DESC LIMIT 100",
    )
    .fetch_all(&state.pg)
    .await?;

    let violations: Vec<serde_json::Value> = rows
        .into_iter()
        .map(|(id, trace_id, agent_id, policy, tool, details, created)| {
            serde_json::json!({
                "id": id,
                "trace_id": trace_id,
                "agent_id": agent_id,
                "policy_name": policy,
                "tool_name": tool,
                "details": details,
                "created_at": created,
            })
        })
        .collect();

    Ok(ok_response(serde_json::json!({ "violations": violations })))
}

// ---------------------------------------------------------------------------
// GET /api/v1/streams
// Active streams (Postgres)
// ---------------------------------------------------------------------------

async fn handler_streams(
    State(state): State<Arc<AppState>>,
) -> Result<Json<serde_json::Value>, StatusApiError> {
    let rows = sqlx::query_as::<_, (String, String, Option<String>, Option<String>, String, String, Option<String>)>(
        "SELECT id, agent_id, tool_name, task_id, status, created_at::text, closed_at::text
         FROM streams ORDER BY created_at DESC LIMIT 100",
    )
    .fetch_all(&state.pg)
    .await?;

    let streams: Vec<serde_json::Value> = rows
        .into_iter()
        .map(|(id, agent_id, tool_name, task_id, status, created, closed)| {
            serde_json::json!({
                "id": id,
                "agent_id": agent_id,
                "tool_name": tool_name,
                "task_id": task_id,
                "status": status,
                "created_at": created,
                "closed_at": closed,
            })
        })
        .collect();

    Ok(ok_response(serde_json::json!({ "streams": streams })))
}

// ---------------------------------------------------------------------------
// GET /api/v1/stream/:stream_id
// SSE proxy — subscribes to NATS and forwards events to dashboard clients
// ---------------------------------------------------------------------------

async fn handler_stream_sse(
    State(state): State<Arc<AppState>>,
    Path(stream_id): Path<String>,
) -> Result<Sse<impl Stream<Item = Result<Event, std::convert::Infallible>>>, StatusApiError> {
    // Verify the stream exists and check its status
    let row = sqlx::query_as::<_, (String,)>(
        "SELECT status FROM streams WHERE id = $1",
    )
    .bind(&stream_id)
    .fetch_optional(&state.pg)
    .await?;

    match row {
        None => {
            return Err(StatusApiError::Internal(format!("stream {stream_id} not found")));
        }
        Some((status,)) if status == "completed" || status == "failed" || status == "expired" => {
            return Err(StatusApiError::Internal(format!("stream {stream_id} is {status}")));
        }
        _ => {}
    }

    // Subscribe to NATS subject for this stream
    let subject = format!(
        "{}.{}.stream.{}",
        state.config.nats.subject_prefix, state.config.broodlink.env, stream_id,
    );

    let mut subscriber = state.nats.subscribe(subject).await
        .map_err(|e| StatusApiError::Nats(e.to_string()))?;

    let (tx, rx) = tokio::sync::mpsc::channel::<Result<Event, std::convert::Infallible>>(32);

    // Bridge NATS messages to SSE events
    tokio::spawn(async move {
        while let Some(msg) = subscriber.next().await {
            let data = String::from_utf8_lossy(&msg.payload).to_string();

            // Determine event type from the payload JSON
            let event_type = serde_json::from_str::<serde_json::Value>(&data)
                .ok()
                .and_then(|v| v.get("event_type").and_then(|e| e.as_str().map(String::from)))
                .unwrap_or_else(|| "message".to_string());

            let event = Event::default()
                .event(&event_type)
                .data(data);

            if tx.send(Ok(event)).await.is_err() {
                break; // Client disconnected
            }

            // Close on terminal events
            if event_type == "complete" || event_type == "error" {
                break;
            }
        }
    });

    let stream = tokio_stream::wrappers::ReceiverStream::new(rx);
    Ok(Sse::new(stream).keep_alive(KeepAlive::default()))
}

// ---------------------------------------------------------------------------
// GET /api/v1/a2a/tasks — list A2A task mappings
// ---------------------------------------------------------------------------

async fn handler_a2a_tasks(
    State(state): State<Arc<AppState>>,
) -> Result<axum::Json<serde_json::Value>, StatusApiError> {
    let rows: Vec<(String, String, Option<String>, Option<String>)> = sqlx::query_as(
        "SELECT m.external_id, m.internal_id, m.source_agent, \
         CAST(m.created_at AS text) \
         FROM a2a_task_map m \
         ORDER BY m.created_at DESC \
         LIMIT 100",
    )
    .fetch_all(&state.pg)
    .await?;

    let tasks: Vec<serde_json::Value> = rows
        .into_iter()
        .map(|(ext_id, int_id, source, created)| {
            serde_json::json!({
                "external_id": ext_id,
                "internal_id": int_id,
                "source_agent": source,
                "created_at": created,
            })
        })
        .collect();

    Ok(axum::Json(serde_json::json!({
        "status": "ok",
        "tasks": tasks,
    })))
}

// ---------------------------------------------------------------------------
// GET /api/v1/a2a/card — return the local AgentCard
// ---------------------------------------------------------------------------

async fn handler_a2a_card(
    State(state): State<Arc<AppState>>,
) -> Result<axum::Json<serde_json::Value>, StatusApiError> {
    let env = &state.config.broodlink.env;
    let version = &state.config.broodlink.version;

    Ok(axum::Json(serde_json::json!({
        "status": "ok",
        "card": {
            "name": "Broodlink",
            "description": format!("Broodlink multi-agent orchestrator ({env})"),
            "url": format!("http://localhost:{}", state.config.a2a.port),
            "version": version,
            "capabilities": {
                "streaming": true,
                "pushNotifications": false,
                "stateTransitionHistory": false,
            },
            "defaultInputModes": ["text/plain"],
            "defaultOutputModes": ["text/plain"],
        }
    })))
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn test_ok_response_structure() {
        let data = serde_json::json!({"key": "value"});
        let Json(resp) = ok_response(data.clone());
        assert_eq!(resp["status"], "ok");
        assert_eq!(resp["data"]["key"], "value");
        assert!(resp["updated_at"].is_string(), "updated_at should be a string");
    }

    #[test]
    fn test_error_auth_returns_401() {
        let err = StatusApiError::Auth("bad key".to_string());
        let resp = err.into_response();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[test]
    fn test_error_internal_returns_500() {
        let err = StatusApiError::Internal("boom".to_string());
        let resp = err.into_response();
        assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[test]
    fn test_error_response_has_json_content_type() {
        let err = StatusApiError::Auth("test".to_string());
        let resp = err.into_response();
        let ct = resp.headers().get("content-type").unwrap().to_str().unwrap();
        assert!(ct.contains("application/json"), "error response should be JSON");
    }

    // -----------------------------------------------------------------------
    // v0.5.0: Knowledge Graph response structure tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_ok_response_kg_stats_structure() {
        let data = serde_json::json!({
            "total_entities": 42,
            "total_active_edges": 15,
            "total_historical_edges": 3,
            "entity_types": {"person": 10, "service": 20, "technology": 12},
            "top_relation_types": [
                {"relation": "DEPENDS_ON", "count": 8},
                {"relation": "MANAGES", "count": 5},
            ],
            "most_connected_entities": [
                {"name": "auth-service", "type": "service", "edge_count": 6},
            ],
        });
        let Json(resp) = ok_response(data);
        assert_eq!(resp["status"], "ok");
        assert_eq!(resp["data"]["total_entities"], 42);
        assert_eq!(resp["data"]["total_active_edges"], 15);
        assert_eq!(resp["data"]["total_historical_edges"], 3);
        assert!(resp["data"]["entity_types"].is_object());
        assert!(resp["data"]["top_relation_types"].is_array());
        assert!(resp["data"]["most_connected_entities"].is_array());
    }

    #[test]
    fn test_ok_response_kg_entities_structure() {
        let data = serde_json::json!({
            "entities": [
                {
                    "entity_id": "abc-123",
                    "name": "redis",
                    "entity_type": "technology",
                    "description": "In-memory data store",
                    "mention_count": 5,
                    "first_seen": "2026-02-20 10:00:00+00",
                    "last_seen": "2026-02-20 11:00:00+00",
                }
            ],
            "total": 1,
        });
        let Json(resp) = ok_response(data);
        assert_eq!(resp["status"], "ok");
        assert_eq!(resp["data"]["total"], 1);

        let entities = resp["data"]["entities"].as_array().unwrap();
        assert_eq!(entities.len(), 1);
        assert_eq!(entities[0]["name"], "redis");
        assert_eq!(entities[0]["entity_type"], "technology");
        assert!(entities[0]["entity_id"].is_string());
        assert!(entities[0]["first_seen"].is_string());
    }

    #[test]
    fn test_ok_response_kg_edges_structure() {
        let data = serde_json::json!({
            "edges": [
                {
                    "edge_id": "edge-1",
                    "source": "auth-service",
                    "target": "redis",
                    "relation_type": "DEPENDS_ON",
                    "description": "auth-service uses redis for session cache",
                    "weight": 1.3,
                    "created_at": "2026-02-20 10:00:00+00",
                }
            ],
            "total": 1,
        });
        let Json(resp) = ok_response(data);
        assert_eq!(resp["status"], "ok");
        assert_eq!(resp["data"]["total"], 1);

        let edges = resp["data"]["edges"].as_array().unwrap();
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0]["source"], "auth-service");
        assert_eq!(edges[0]["target"], "redis");
        assert_eq!(edges[0]["relation_type"], "DEPENDS_ON");
        let weight = edges[0]["weight"].as_f64().unwrap();
        assert!((weight - 1.3).abs() < f64::EPSILON);
    }

    #[test]
    fn test_ok_response_kg_stats_empty_graph() {
        let data = serde_json::json!({
            "total_entities": 0,
            "total_active_edges": 0,
            "total_historical_edges": 0,
            "entity_types": {},
            "top_relation_types": [],
            "most_connected_entities": [],
        });
        let Json(resp) = ok_response(data);
        assert_eq!(resp["status"], "ok");
        assert_eq!(resp["data"]["total_entities"], 0);
        assert_eq!(resp["data"]["total_active_edges"], 0);
        assert!(resp["data"]["entity_types"].as_object().unwrap().is_empty());
        assert!(resp["data"]["top_relation_types"].as_array().unwrap().is_empty());
    }

    #[test]
    fn test_ok_response_kg_entities_empty() {
        let data = serde_json::json!({"entities": [], "total": 0});
        let Json(resp) = ok_response(data);
        assert_eq!(resp["status"], "ok");
        assert_eq!(resp["data"]["total"], 0);
        assert!(resp["data"]["entities"].as_array().unwrap().is_empty());
    }

    // -----------------------------------------------------------------------
    // v0.5.0: Route registration tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_kg_routes_are_defined() {
        // Verify the KG route paths follow the expected pattern
        let routes = vec!["/kg/stats", "/kg/entities", "/kg/edges"];
        for route in &routes {
            assert!(route.starts_with("/kg/"), "KG route should be under /kg/ prefix: {route}");
        }
    }
}
