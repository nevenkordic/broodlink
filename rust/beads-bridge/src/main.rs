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

use axum::extract::{Json, Path, State};
use axum::http::{header, Request, StatusCode};
use axum::middleware::{self, Next};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::Router;
use futures::stream::Stream;
use futures::StreamExt;
use tokio_stream::wrappers::ReceiverStream;
use broodlink_config::Config;
use broodlink_runtime::CircuitBreaker;
use broodlink_secrets::SecretsProvider;
use serde::{Deserialize, Serialize};
use sqlx::mysql::MySqlPoolOptions;
use sqlx::postgres::PgPoolOptions;
use sqlx::{MySqlPool, PgPool};
use tokio::sync::RwLock;
use tracing::{error, info, warn};
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

const SERVICE_NAME: &str = "beads-bridge";
const SERVICE_VERSION: &str = env!("CARGO_PKG_VERSION");

const CIRCUIT_FAILURE_THRESHOLD: u32 = 5;
const CIRCUIT_HALF_OPEN_SECS: u64 = 30;

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

#[derive(thiserror::Error, Debug)]
pub enum BroodlinkError {
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
    #[error("validation error: {field} — {message}")]
    Validation { field: String, message: String },
    #[error("not found: {0}")]
    NotFound(String),
    #[error("rate limited: agent {0}")]
    RateLimited(String),
    #[error("circuit open: {0}")]
    CircuitOpen(String),
    #[error("guardrail blocked: {policy} — {message}")]
    Guardrail { policy: String, message: String },
    #[error("internal error: {0}")]
    Internal(String),
}

impl From<async_nats::ConnectError> for BroodlinkError {
    fn from(e: async_nats::ConnectError) -> Self {
        Self::Nats(e.to_string())
    }
}

impl From<async_nats::PublishError> for BroodlinkError {
    fn from(e: async_nats::PublishError) -> Self {
        Self::Nats(e.to_string())
    }
}

impl IntoResponse for BroodlinkError {
    fn into_response(self) -> Response {
        let (status, body) = match &self {
            Self::Database(e) => {
                error!(error = %e, "database error");
                (StatusCode::INTERNAL_SERVER_ERROR, self.to_string())
            }
            Self::Nats(e) => {
                error!(error = %e, "nats error");
                (StatusCode::INTERNAL_SERVER_ERROR, self.to_string())
            }
            Self::Config(e) => {
                error!(error = %e, "config error");
                (StatusCode::INTERNAL_SERVER_ERROR, self.to_string())
            }
            Self::Secrets(e) => {
                error!(error = %e, "secrets error");
                (StatusCode::INTERNAL_SERVER_ERROR, self.to_string())
            }
            Self::Auth(msg) => {
                warn!(msg = %msg, "auth failure");
                (StatusCode::UNAUTHORIZED, self.to_string())
            }
            Self::Validation { field, message } => {
                warn!(field = %field, message = %message, "validation error");
                (StatusCode::BAD_REQUEST, self.to_string())
            }
            Self::NotFound(what) => {
                info!(entity = %what, "not found");
                (StatusCode::NOT_FOUND, self.to_string())
            }
            Self::RateLimited(agent) => {
                warn!(agent = %agent, "rate limited");
                (StatusCode::TOO_MANY_REQUESTS, self.to_string())
            }
            Self::CircuitOpen(svc) => {
                warn!(service = %svc, "circuit open");
                (StatusCode::SERVICE_UNAVAILABLE, self.to_string())
            }
            Self::Guardrail { ref policy, ref message } => {
                warn!(policy = %policy, message = %message, "guardrail blocked");
                (StatusCode::FORBIDDEN, self.to_string())
            }
            Self::Internal(msg) => {
                error!(msg = %msg, "internal error");
                (StatusCode::INTERNAL_SERVER_ERROR, self.to_string())
            }
        };
        let json = serde_json::json!({ "error": body });
        (status, axum::Json(json)).into_response()
    }
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
    pub dependencies: HashMap<String, String>,
}

// ---------------------------------------------------------------------------
// Tool request / response envelope
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct ToolRequest {
    #[serde(default)]
    pub params: serde_json::Value,
}

#[derive(Serialize)]
pub struct ToolResponse {
    pub tool: String,
    pub success: bool,
    pub data: serde_json::Value,
}

// ---------------------------------------------------------------------------
// JWT claims
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Claims {
    pub sub: String,
    pub agent_id: String,
    pub exp: u64,
    pub iat: u64,
}

// ---------------------------------------------------------------------------
// Rate limiter (token bucket per agent)
// ---------------------------------------------------------------------------

pub struct TokenBucket {
    tokens: f64,
    last_refill: Instant,
    max_tokens: f64,
    refill_rate: f64, // tokens per second
}

pub struct RateLimiter {
    buckets: RwLock<HashMap<String, TokenBucket>>,
    max_tokens: f64,
    refill_rate: f64,
}

impl RateLimiter {
    fn new(requests_per_minute: u32, burst: u32) -> Self {
        let refill_rate = f64::from(requests_per_minute) / 60.0;
        let max_tokens = f64::from(burst);
        Self {
            buckets: RwLock::new(HashMap::new()),
            max_tokens,
            refill_rate,
        }
    }

    async fn check(&self, agent_id: &str) -> Result<(), BroodlinkError> {
        let mut buckets = self.buckets.write().await;
        let bucket = buckets.entry(agent_id.to_string()).or_insert(TokenBucket {
            tokens: self.max_tokens,
            last_refill: Instant::now(),
            max_tokens: self.max_tokens,
            refill_rate: self.refill_rate,
        });

        let now = Instant::now();
        let elapsed = now.duration_since(bucket.last_refill).as_secs_f64();
        bucket.tokens = (bucket.tokens + elapsed * bucket.refill_rate).min(bucket.max_tokens);
        bucket.last_refill = now;

        if bucket.tokens >= 1.0 {
            bucket.tokens -= 1.0;
            Ok(())
        } else {
            Err(BroodlinkError::RateLimited(agent_id.to_string()))
        }
    }
}

// ---------------------------------------------------------------------------
// Application state
// ---------------------------------------------------------------------------

pub struct AppState {
    pub dolt: MySqlPool,
    pub pg: PgPool,
    pub pg_read: Option<PgPool>,
    pub nats: async_nats::Client,
    pub config: Arc<Config>,
    pub secrets: Arc<dyn SecretsProvider>,
    pub rate_limiter: RateLimiter,
    pub rate_override_buckets: RwLock<HashMap<String, TokenBucket>>,
    pub qdrant_breaker: CircuitBreaker,
    pub ollama_breaker: CircuitBreaker,
    pub start_time: Instant,
    pub jwt_decoding_key: jsonwebtoken::DecodingKey,
    pub jwt_validation: jsonwebtoken::Validation,
}

// ---------------------------------------------------------------------------
// Startup
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() {
    // Load config early so telemetry can use it
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

    let shared = Arc::new(state);

    let app = build_router(Arc::clone(&shared));

    let addr = SocketAddr::from(([0, 0, 0, 0], shared.config.beads_bridge.port));

    // Conditional TLS binding
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

        axum_server::bind_rustls(addr, tls_config)
            .serve(app.into_make_service())
            .await
            .unwrap_or_else(|e| {
                error!(error = %e, "TLS server error");
                process::exit(1);
            });
    } else {
        info!(addr = %addr, "listening (plaintext)");

        let listener = match tokio::net::TcpListener::bind(addr).await {
            Ok(l) => l,
            Err(e) => {
                error!(error = %e, "failed to bind");
                process::exit(1);
            }
        };

        axum::serve(listener, app.into_make_service())
            .with_graceful_shutdown(shutdown_signal())
            .await
            .unwrap_or_else(|e| {
                error!(error = %e, "server error");
                process::exit(1);
            });
    }

    info!("shutdown complete");
}

async fn init_state() -> Result<AppState, BroodlinkError> {
    let config = Config::load().map_err(|e| BroodlinkError::Config(e.to_string()))?;
    let config = Arc::new(config);

    // Secrets provider
    let secrets: Arc<dyn SecretsProvider> = {
        let sc = &config.secrets;
        let provider = broodlink_secrets::create_provider(
            &sc.provider,
            sc.sops_file.as_deref(),
            sc.age_identity.as_deref(),
            sc.infisical_url.as_deref(),
            None, // infisical token resolved from env at provider level
        )?;
        Arc::from(provider)
    };

    // Resolve database passwords from secrets
    let dolt_password = secrets.get(&config.dolt.password_key).await?;
    let pg_password = secrets.get(&config.postgres.password_key).await?;

    // JWT RS256 decoding key from secrets
    let jwt_public_pem = secrets.get("BROODLINK_JWT_PUBLIC_KEY").await?;
    let jwt_decoding_key = jsonwebtoken::DecodingKey::from_rsa_pem(jwt_public_pem.as_bytes())
        .map_err(|e| BroodlinkError::Auth(format!("invalid JWT public key: {e}")))?;
    let mut jwt_validation = jsonwebtoken::Validation::new(jsonwebtoken::Algorithm::RS256);
    jwt_validation.validate_exp = true;
    jwt_validation.set_required_spec_claims(&["sub", "agent_id", "exp", "iat"]);

    // Dolt (MySQL) pool
    let dolt_url = format!(
        "mysql://{}:{}@{}:{}/{}",
        config.dolt.user, dolt_password, config.dolt.host, config.dolt.port, config.dolt.database,
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

    // NATS client (cluster-aware via broodlink-runtime)
    let nats = broodlink_runtime::connect_nats(&config.nats).await?;

    // Rate limiter from config
    let rate_limiter = RateLimiter::new(
        config.rate_limits.requests_per_minute_per_agent,
        config.rate_limits.burst,
    );

    // Read replica pool (optional)
    let pg_read = if !config.postgres_read_replicas.urls.is_empty() {
        let replica_url = &config.postgres_read_replicas.urls[0];
        match PgPoolOptions::new()
            .min_connections(1)
            .max_connections(5)
            .acquire_timeout(Duration::from_secs(3))
            .connect(replica_url)
            .await
        {
            Ok(pool) => {
                info!(url = %replica_url, "postgres read replica connected");
                Some(pool)
            }
            Err(e) => {
                warn!(error = %e, "failed to connect read replica — falling back to primary");
                None
            }
        }
    } else {
        None
    };

    // Circuit breakers
    let qdrant_breaker = CircuitBreaker::new(
        "qdrant",
        CIRCUIT_FAILURE_THRESHOLD,
        CIRCUIT_HALF_OPEN_SECS,
    );
    let ollama_breaker = CircuitBreaker::new(
        "ollama",
        CIRCUIT_FAILURE_THRESHOLD,
        CIRCUIT_HALF_OPEN_SECS,
    );

    Ok(AppState {
        dolt,
        pg,
        pg_read,
        nats,
        config,
        secrets,
        rate_limiter,
        rate_override_buckets: RwLock::new(HashMap::new()),
        qdrant_breaker,
        ollama_breaker,
        start_time: Instant::now(),
        jwt_decoding_key,
        jwt_validation,
    })
}

/// Returns the read replica pool if available, otherwise the primary.
#[allow(dead_code)]
fn pg_read_pool(state: &AppState) -> &PgPool {
    state.pg_read.as_ref().unwrap_or(&state.pg)
}

async fn shutdown_signal() {
    broodlink_runtime::shutdown_signal().await;
}

// ---------------------------------------------------------------------------
// Tool metadata registry (OpenAI function-calling format)
// ---------------------------------------------------------------------------

fn tool_def(name: &str, desc: &str, props: serde_json::Value, required: &[&str]) -> serde_json::Value {
    serde_json::json!({
        "type": "function",
        "function": {
            "name": name,
            "description": desc,
            "parameters": {
                "type": "object",
                "properties": props,
                "required": required,
            }
        }
    })
}

fn p_str(desc: &str) -> serde_json::Value {
    serde_json::json!({"type": "string", "description": desc})
}

fn p_int(desc: &str) -> serde_json::Value {
    serde_json::json!({"type": "integer", "description": desc})
}

fn p_bool(desc: &str) -> serde_json::Value {
    serde_json::json!({"type": "boolean", "description": desc})
}

static TOOL_REGISTRY: std::sync::LazyLock<Vec<serde_json::Value>> =
    std::sync::LazyLock::new(|| {
        vec![
            // --- Memory ---
            tool_def("store_memory", "Store or update a memory entry by topic. Upserts — updates if topic exists, inserts if new.", serde_json::json!({
                "topic": p_str("Short kebab-case topic key, e.g. 'user-preferences'"),
                "content": p_str("The content to store"),
                "tags": p_str("Optional comma-separated tags"),
            }), &["topic", "content"]),
            tool_def("recall_memory", "Search agent memory. If topic_search given, matches by LIKE pattern. Otherwise returns all.", serde_json::json!({
                "topic_search": p_str("Optional search string to filter memories"),
            }), &[]),
            tool_def("delete_memory", "Delete a memory entry by exact topic name.", serde_json::json!({
                "topic": p_str("Exact topic name to delete"),
            }), &["topic"]),
            tool_def("semantic_search", "Search memory using semantic vector similarity via Qdrant.", serde_json::json!({
                "query": p_str("Natural language search query"),
                "limit": p_int("Number of results (default 5)"),
            }), &["query"]),

            // --- Work ---
            tool_def("log_work", "Log a work entry. Actions: created, modified, fixed, deployed, conversation.", serde_json::json!({
                "action": p_str("Action type, e.g. 'fixed', 'deployed', 'conversation'"),
                "details": p_str("Description of what happened"),
                "files_changed": p_str("Optional JSON array of file paths"),
            }), &["action", "details"]),
            tool_def("get_work_log", "Get recent work log entries.", serde_json::json!({
                "limit": p_int("Number of entries (default 20)"),
                "agent_id": p_str("Optional filter by agent"),
            }), &[]),

            // --- Projects ---
            tool_def("list_projects", "List all projects, optionally filtered by status.", serde_json::json!({
                "status": p_str("Filter: planning, in_progress, completed, on_hold, failed"),
            }), &[]),
            tool_def("add_project", "Create a new project.", serde_json::json!({
                "name": p_str("Project name"),
                "description": p_str("Project description"),
                "status": p_str("Initial status (default: planning)"),
                "tags": p_str("Optional comma-separated tags"),
            }), &["name", "description"]),
            tool_def("update_project", "Update an existing project.", serde_json::json!({
                "project_id": p_int("Project ID to update"),
                "status": p_str("New status"),
                "description": p_str("New description"),
                "tags": p_str("New tags"),
            }), &["project_id"]),

            // --- Skills ---
            tool_def("list_skills", "List registered skills, optionally filtered by agent.", serde_json::json!({
                "agent_name": p_str("Optional filter by agent name"),
            }), &[]),
            tool_def("add_skill", "Register or update a skill.", serde_json::json!({
                "name": p_str("Skill name"),
                "description": p_str("Skill description"),
                "agent_name": p_str("Agent name (default: caller)"),
                "examples": p_str("Optional JSON array of usage examples"),
            }), &["name", "description"]),

            // --- Conversations ---
            tool_def("log_conversation", "Log a conversation message.", serde_json::json!({
                "agent_name": p_str("Agent name"),
                "role": p_str("Role: user, assistant, or system"),
                "content": p_str("Message content"),
                "user_id": p_str("Optional user identifier"),
                "session_id": p_str("Optional session identifier"),
                "metadata": p_str("Optional JSON metadata"),
            }), &["agent_name", "role", "content"]),

            // --- Beads ---
            tool_def("beads_list_issues", "List Beads issues, optionally filtered by status.", serde_json::json!({
                "status": p_str("Filter by status"),
                "limit": p_int("Number of issues (default 50)"),
            }), &[]),
            tool_def("beads_get_issue", "Get details of a specific Beads issue.", serde_json::json!({
                "issue_id": p_str("Bead ID to look up"),
            }), &["issue_id"]),
            tool_def("beads_update_status", "Update the status of a Beads issue.", serde_json::json!({
                "issue_id": p_str("Bead ID to update"),
                "status": p_str("New status"),
            }), &["issue_id", "status"]),
            tool_def("beads_create_issue", "Create a new Beads issue.", serde_json::json!({
                "title": p_str("Issue title"),
                "description": p_str("Issue description"),
                "assignee": p_str("Agent ID to assign"),
                "convoy_id": p_str("Parent convoy ID"),
                "formula": p_str("Workflow formula name"),
            }), &["title"]),
            tool_def("beads_list_formulas", "List available Beads workflow formulas.", serde_json::json!({}), &[]),
            tool_def("beads_run_formula", "Run a Beads workflow formula.", serde_json::json!({
                "formula": p_str("Formula name to run"),
                "parameters": p_str("Optional JSON parameters"),
            }), &["formula"]),
            tool_def("beads_get_convoy", "Get details of a Beads convoy (issue group).", serde_json::json!({
                "convoy_id": p_str("Convoy ID to look up"),
            }), &[]),

            // --- Messaging ---
            tool_def("send_message", "Send a message to another agent.", serde_json::json!({
                "to": p_str("Recipient agent ID"),
                "content": p_str("Message content"),
                "subject": p_str("Subject (default: direct)"),
            }), &["to", "content"]),
            tool_def("read_messages", "Read messages from the message queue.", serde_json::json!({
                "limit": p_int("Number of messages (default 20)"),
                "unread_only": p_bool("Only return unread messages"),
            }), &[]),

            // --- Decisions ---
            tool_def("log_decision", "Log a decision with reasoning for audit trail.", serde_json::json!({
                "decision": p_str("The decision made"),
                "reasoning": p_str("Why this decision was made"),
                "outcome": p_str("Expected or actual outcome"),
                "alternatives": p_str("Alternatives considered"),
            }), &["decision"]),
            tool_def("get_decisions", "Get recent decisions.", serde_json::json!({
                "limit": p_int("Number of decisions (default 20)"),
                "agent_id": p_str("Optional filter by agent"),
            }), &[]),

            // --- Queue ---
            tool_def("create_task", "Create a new task in the queue.", serde_json::json!({
                "title": p_str("Task title"),
                "description": p_str("Task description"),
                "priority": p_int("Priority (higher = more urgent)"),
                "role": p_str("Required agent role for routing (default: worker)"),
                "cost_tier": p_str("Preferred cost tier: low, medium, high"),
            }), &["title"]),
            tool_def("get_task", "Get details of a specific task.", serde_json::json!({
                "task_id": p_str("Task ID"),
            }), &["task_id"]),
            tool_def("list_tasks", "List tasks, optionally filtered by status.", serde_json::json!({
                "status": p_str("Filter: pending, claimed, completed, failed"),
                "limit": p_int("Number of tasks (default 50)"),
            }), &[]),
            tool_def("claim_task", "Atomically claim a pending task.", serde_json::json!({
                "task_id": p_str("Task ID to claim (optional — claims next by priority if omitted)"),
            }), &[]),
            tool_def("complete_task", "Mark a claimed task as completed.", serde_json::json!({
                "task_id": p_str("Task ID to complete"),
            }), &["task_id"]),
            tool_def("fail_task", "Mark a claimed task as failed.", serde_json::json!({
                "task_id": p_str("Task ID to mark failed"),
            }), &["task_id"]),

            // --- Agent ---
            tool_def("agent_upsert", "Register or update an agent profile.", serde_json::json!({
                "agent_id": p_str("Unique agent identifier"),
                "display_name": p_str("Human-readable name"),
                "role": p_str("Agent role (e.g. strategist, worker)"),
                "transport": p_str("Transport type (default: mcp)"),
                "cost_tier": p_str("Cost tier: low, medium, high (default: medium)"),
                "capabilities": p_str("Comma-separated capabilities"),
                "active": p_bool("Whether agent is active (default: true)"),
            }), &["agent_id", "display_name", "role"]),

            // --- Utility ---
            tool_def("health_check", "Check health of all services and dependencies.", serde_json::json!({}), &[]),
            tool_def("get_config_info", "Get current Broodlink configuration info.", serde_json::json!({}), &[]),
            tool_def("list_agents", "List all registered agent profiles.", serde_json::json!({}), &[]),
            tool_def("get_agent", "Get a specific agent's profile.", serde_json::json!({
                "agent_id": p_str("Agent ID to look up"),
            }), &["agent_id"]),
            tool_def("get_audit_log", "Get recent audit log entries.", serde_json::json!({
                "limit": p_int("Number of entries (default 50)"),
                "agent_id": p_str("Optional filter by agent"),
            }), &[]),
            tool_def("get_daily_summary", "Get daily summary. Defaults to last 7 days.", serde_json::json!({
                "date": p_str("Specific date (YYYY-MM-DD)"),
                "limit": p_int("Number of days (default 7)"),
            }), &[]),
            tool_def("get_commits", "Get recent Dolt commits.", serde_json::json!({
                "limit": p_int("Number of commits (default 20)"),
            }), &[]),
            tool_def("get_memory_stats", "Get memory statistics: total count, recent updates, top topics.", serde_json::json!({}), &[]),
            tool_def("get_convoy_status", "Get status of a Beads convoy.", serde_json::json!({
                "convoy_id": p_str("Convoy ID"),
            }), &[]),
            tool_def("ping", "Ping beads-bridge to check connectivity.", serde_json::json!({}), &[]),

            // --- Guardrails (Postgres) ---
            tool_def("set_guardrail", "Create or update a guardrail policy.", serde_json::json!({
                "name": p_str("Unique policy name"),
                "rule_type": p_str("Rule type: tool_block, rate_override, content_filter, scope_limit"),
                "config": p_str("Policy config as JSON string"),
                "enabled": p_bool("Whether policy is active (default: true)"),
            }), &["name", "rule_type", "config"]),
            tool_def("list_guardrails", "List guardrail policies.", serde_json::json!({
                "enabled_only": p_bool("Only return enabled policies (default: false)"),
            }), &[]),
            tool_def("get_guardrail_violations", "Get guardrail violation log entries.", serde_json::json!({
                "agent_id": p_str("Optional filter by agent"),
                "limit": p_int("Number of entries (default 50)"),
            }), &[]),

            // --- Approval Gates (Postgres) ---
            tool_def("create_approval_gate", "Create an approval gate. Auto-approved if a matching policy allows it.", serde_json::json!({
                "gate_type": p_str("Gate type: pre_dispatch, pre_completion, budget, custom"),
                "payload": p_str("Details as JSON object or string"),
                "severity": p_str("Severity: low, medium, high, critical (default: medium)"),
                "task_id": p_str("Optional linked task ID"),
                "tool_name": p_str("Optional tool that triggered this gate"),
                "confidence": p_str("Optional confidence score 0.0-1.0"),
                "expires_minutes": p_int("Minutes until auto-expiry (default: 60, overridden by policy)"),
            }), &["gate_type", "payload"]),
            tool_def("resolve_approval", "Approve or reject a pending approval gate.", serde_json::json!({
                "approval_id": p_str("Approval gate ID"),
                "decision": p_str("Decision: approved or rejected"),
                "reason": p_str("Optional reason for decision"),
            }), &["approval_id", "decision"]),
            tool_def("list_approvals", "List approval gates.", serde_json::json!({
                "status": p_str("Filter by status: pending, approved, rejected, expired, auto_approved"),
                "limit": p_int("Number of entries (default 50)"),
            }), &[]),
            tool_def("get_approval", "Get details of a specific approval gate.", serde_json::json!({
                "approval_id": p_str("Approval gate ID"),
            }), &["approval_id"]),
            tool_def("set_approval_policy", "Create or update an approval policy for auto-approve rules.", serde_json::json!({
                "name": p_str("Unique policy name"),
                "gate_type": p_str("Gate type this policy applies to"),
                "description": p_str("Optional description"),
                "conditions": p_str("JSON conditions: {severity: [...], agent_ids: [...], tool_names: [...]}"),
                "auto_approve": p_bool("Whether matching gates are auto-approved"),
                "auto_approve_threshold": p_str("Confidence threshold for auto-approval (0.0-1.0, default 0.8)"),
                "expiry_minutes": p_int("Minutes until expiry (default 60)"),
                "active": p_bool("Whether policy is active (default true)"),
            }), &["name", "gate_type", "conditions"]),
            tool_def("list_approval_policies", "List approval policies.", serde_json::json!({
                "active_only": p_bool("Only return active policies (default false)"),
            }), &[]),

            // --- Routing (Postgres + Dolt) ---
            tool_def("get_routing_scores", "Get agent routing scores with breakdown.", serde_json::json!({
                "role": p_str("Optional filter by agent role"),
            }), &[]),

            // --- Delegation (Postgres) ---
            tool_def("delegate_task", "Delegate a task to another agent.", serde_json::json!({
                "to_agent": p_str("Target agent ID"),
                "title": p_str("Delegation title"),
                "description": p_str("Optional description"),
                "parent_task_id": p_str("Optional parent task ID for hierarchy"),
            }), &["to_agent", "title"]),
            tool_def("accept_delegation", "Accept an incoming delegation.", serde_json::json!({
                "delegation_id": p_str("Delegation ID to accept"),
            }), &["delegation_id"]),
            tool_def("reject_delegation", "Reject an incoming delegation.", serde_json::json!({
                "delegation_id": p_str("Delegation ID to reject"),
                "reason": p_str("Optional reason for rejection"),
            }), &["delegation_id"]),
            tool_def("complete_delegation", "Mark a delegation as completed with results.", serde_json::json!({
                "delegation_id": p_str("Delegation ID to complete"),
                "result": p_str("Result data as JSON string"),
            }), &["delegation_id", "result"]),
            tool_def("get_delegation", "Get a delegation by ID including status and result.", serde_json::json!({
                "delegation_id": p_str("Delegation ID"),
            }), &["delegation_id"]),
            tool_def("list_delegations", "List delegations with optional filters.", serde_json::json!({
                "from_agent": p_str("Optional filter by delegating agent"),
                "to_agent": p_str("Optional filter by target agent"),
                "status": p_str("Optional filter by status"),
                "limit": p_str("Max results (default 50)"),
            }), &[]),

            // --- Streaming (Postgres + NATS) ---
            tool_def("start_stream", "Start a real-time event stream.", serde_json::json!({
                "task_id": p_str("Optional linked task ID"),
                "tool_name": p_str("Optional tool name context"),
            }), &[]),
            tool_def("emit_stream_event", "Emit an event to an active stream.", serde_json::json!({
                "stream_id": p_str("Stream ID to emit to"),
                "event_type": p_str("Event type: progress, chunk, complete, error"),
                "data": p_str("Event data as JSON string"),
            }), &["stream_id", "event_type", "data"]),

            // --- A2A (Agent-to-Agent) ---
            tool_def("a2a_discover", "Fetch a remote agent's A2A AgentCard.", serde_json::json!({
                "url": p_str("Base URL of the remote A2A agent (e.g. http://remote:3313)"),
            }), &["url"]),
            tool_def("a2a_delegate", "Send a task to a remote A2A agent.", serde_json::json!({
                "url": p_str("Base URL of the remote A2A agent"),
                "message": p_str("Task message text to send"),
                "api_key": p_str("Optional Bearer token for auth"),
            }), &["url", "message"]),

            // --- Workflow Orchestration ---
            tool_def("start_workflow", "Start a multi-step workflow from a formula. The coordinator chains steps automatically.", serde_json::json!({
                "formula_name": p_str("Formula name (e.g. 'research', 'build-feature')"),
                "params": p_str("Optional JSON parameters for the formula template"),
            }), &["formula_name"]),
        ]
    });

/// Returns all tool definitions in OpenAI function-calling format (public, no auth).
async fn tools_metadata_handler() -> axum::Json<serde_json::Value> {
    axum::Json(serde_json::json!({
        "tools": *TOOL_REGISTRY,
        "version": SERVICE_VERSION,
        "total": TOOL_REGISTRY.len(),
    }))
}

// ---------------------------------------------------------------------------
// Router construction
// ---------------------------------------------------------------------------

fn build_router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/health", get(health_handler))
        .route("/api/v1/tools", get(tools_metadata_handler))
        .route(
            "/api/v1/tool/:tool_name",
            post(tool_dispatch).layer(middleware::from_fn_with_state(
                Arc::clone(&state),
                auth_middleware,
            )),
        )
        .route(
            "/api/v1/stream/:stream_id",
            get(sse_stream_handler).layer(middleware::from_fn_with_state(
                Arc::clone(&state),
                auth_middleware,
            )),
        )
        .layer(tower_http::trace::TraceLayer::new_for_http())
        .with_state(state)
}

// ---------------------------------------------------------------------------
// Auth middleware — extracts JWT from Authorization header
// ---------------------------------------------------------------------------

async fn auth_middleware(
    State(state): State<Arc<AppState>>,
    mut req: Request<axum::body::Body>,
    next: Next,
) -> Result<Response, BroodlinkError> {
    let auth_header = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| BroodlinkError::Auth("missing Authorization header".to_string()))?;

    let token = auth_header
        .strip_prefix("Bearer ")
        .ok_or_else(|| BroodlinkError::Auth("expected Bearer token".to_string()))?;

    let token_data = jsonwebtoken::decode::<Claims>(
        token,
        &state.jwt_decoding_key,
        &state.jwt_validation,
    )
    .map_err(|e| BroodlinkError::Auth(format!("invalid token: {e}")))?;

    // Rate limit check per agent
    state
        .rate_limiter
        .check(&token_data.claims.agent_id)
        .await?;

    // Touch last_seen / active so the dashboard reflects live status.
    // Fire-and-forget — auth should not fail if Dolt is momentarily slow.
    let dolt = state.dolt.clone();
    let aid = token_data.claims.agent_id.clone();
    tokio::spawn(async move {
        let _ = sqlx::query(
            "UPDATE agent_profiles SET last_seen = NOW(), active = true WHERE agent_id = ?",
        )
        .bind(&aid)
        .execute(&dolt)
        .await;
    });

    // Stash agent_id in request extensions for downstream handlers
    req.extensions_mut().insert(token_data.claims);
    Ok(next.run(req).await)
}

// ---------------------------------------------------------------------------
// Health endpoint (no auth)
// ---------------------------------------------------------------------------

async fn health_handler(
    State(state): State<Arc<AppState>>,
) -> Result<Json<HealthResponse>, BroodlinkError> {
    let mut deps = HashMap::new();

    // Dolt health
    match sqlx::query("SELECT 1").execute(&state.dolt).await {
        Ok(_) => deps.insert("dolt".to_string(), "ok".to_string()),
        Err(e) => deps.insert("dolt".to_string(), format!("err: {e}")),
    };

    // Postgres health
    match sqlx::query("SELECT 1").execute(&state.pg).await {
        Ok(_) => deps.insert("postgres".to_string(), "ok".to_string()),
        Err(e) => deps.insert("postgres".to_string(), format!("err: {e}")),
    };

    // NATS health
    match state.nats.flush().await {
        Ok(()) => deps.insert("nats".to_string(), "ok".to_string()),
        Err(e) => deps.insert("nats".to_string(), format!("err: {e}")),
    };

    // Circuit breaker states
    deps.insert(
        "qdrant_circuit".to_string(),
        if state.qdrant_breaker.is_open() {
            "open".to_string()
        } else {
            "closed".to_string()
        },
    );
    deps.insert(
        "ollama_circuit".to_string(),
        if state.ollama_breaker.is_open() {
            "open".to_string()
        } else {
            "closed".to_string()
        },
    );

    let uptime = state.start_time.elapsed().as_secs();

    Ok(Json(HealthResponse {
        status: "ok".to_string(),
        service: SERVICE_NAME.to_string(),
        version: SERVICE_VERSION.to_string(),
        uptime_seconds: uptime,
        dependencies: deps,
    }))
}

// ---------------------------------------------------------------------------
// Tool dispatch — routes the 35 tools via path param
// ---------------------------------------------------------------------------

async fn tool_dispatch(
    State(state): State<Arc<AppState>>,
    Path(tool_name): Path<String>,
    axum::Extension(claims): axum::Extension<Claims>,
    Json(body): Json<ToolRequest>,
) -> Result<Json<ToolResponse>, BroodlinkError> {
    let agent_id = &claims.agent_id;
    let request_id = Uuid::new_v4().to_string();
    let params = &body.params;

    info!(
        tool = %tool_name,
        agent = %agent_id,
        request_id = %request_id,
        "tool invoked"
    );

    // Guardrail check — runs after auth+rate-limit, before tool execution
    check_guardrails(&state, agent_id, &tool_name, params).await?;

    let result = match tool_name.as_str() {
        // --- Memory (Dolt) ---
        "store_memory" => tool_store_memory(&state, agent_id, params).await,
        "recall_memory" => tool_recall_memory(&state, agent_id, params).await,
        "delete_memory" => tool_delete_memory(&state, params).await,
        "semantic_search" => tool_semantic_search(&state, agent_id, params).await,

        // --- Work (Postgres) ---
        "log_work" => tool_log_work(&state, agent_id, params).await,
        "get_work_log" => tool_get_work_log(&state, agent_id, params).await,

        // --- Projects (Dolt) ---
        "list_projects" => tool_list_projects(&state, agent_id, params).await,
        "add_project" => tool_add_project(&state, agent_id, params).await,
        "update_project" => tool_update_project(&state, agent_id, params).await,

        // --- Skills (Dolt) ---
        "list_skills" => tool_list_skills(&state, params).await,
        "add_skill" => tool_add_skill(&state, params).await,

        // --- Conversations (Dolt) ---
        "log_conversation" => tool_log_conversation(&state, params).await,

        // --- Beads (Dolt + external) ---
        "beads_list_issues" => tool_beads_list_issues(&state, agent_id, params).await,
        "beads_get_issue" => tool_beads_get_issue(&state, agent_id, params).await,
        "beads_update_status" => tool_beads_update_status(&state, agent_id, params).await,
        "beads_create_issue" => tool_beads_create_issue(&state, agent_id, params).await,
        "beads_list_formulas" => tool_beads_list_formulas(&state, agent_id, params).await,
        "beads_run_formula" => tool_beads_run_formula(&state, agent_id, params).await,
        "beads_get_convoy" => tool_beads_get_convoy(&state, agent_id, params).await,

        // --- Messaging (Postgres) ---
        "send_message" => tool_send_message(&state, agent_id, params).await,
        "read_messages" => tool_read_messages(&state, agent_id, params).await,

        // --- Decisions (Dolt) ---
        "log_decision" => tool_log_decision(&state, agent_id, params).await,
        "get_decisions" => tool_get_decisions(&state, agent_id, params).await,

        // --- Queue (Postgres) ---
        "claim_task" => tool_claim_task(&state, agent_id, params).await,
        "complete_task" => tool_complete_task(&state, agent_id, params).await,
        "fail_task" => tool_fail_task(&state, agent_id, params).await,

        // --- Agent (Dolt) ---
        "agent_upsert" => tool_agent_upsert(&state, agent_id, params).await,

        // --- Utility ---
        "health_check" => tool_health_check(&state).await,
        "get_config_info" => tool_get_config_info(&state).await,
        "list_agents" => tool_list_agents(&state).await,
        "get_agent" => tool_get_agent(&state, params).await,
        "get_audit_log" => tool_get_audit_log(&state, params).await,
        "get_task" => tool_get_task(&state, params).await,
        "list_tasks" => tool_list_tasks(&state, params).await,
        "create_task" => tool_create_task(&state, agent_id, params).await,
        "get_daily_summary" => tool_get_daily_summary(&state, params).await,
        "get_commits" => tool_get_commits(&state, params).await,
        "get_memory_stats" => tool_get_memory_stats(&state).await,
        "get_convoy_status" => tool_get_convoy_status(&state, params).await,

        // --- Guardrails (Postgres) ---
        "set_guardrail" => tool_set_guardrail(&state, agent_id, params).await,
        "list_guardrails" => tool_list_guardrails(&state, params).await,
        "get_guardrail_violations" => tool_get_guardrail_violations(&state, params).await,

        // --- Approval Gates (Postgres) ---
        "create_approval_gate" => tool_create_approval_gate(&state, agent_id, params).await,
        "resolve_approval" => tool_resolve_approval(&state, agent_id, params).await,
        "list_approvals" => tool_list_approvals(&state, params).await,
        "get_approval" => tool_get_approval(&state, params).await,
        "set_approval_policy" => tool_set_approval_policy(&state, agent_id, params).await,
        "list_approval_policies" => tool_list_approval_policies(&state, params).await,

        // --- Routing (Postgres + Dolt) ---
        "get_routing_scores" => tool_get_routing_scores(&state, params).await,

        // --- Delegation (Postgres) ---
        "delegate_task" => tool_delegate_task(&state, agent_id, params).await,
        "accept_delegation" => tool_accept_delegation(&state, agent_id, params).await,
        "reject_delegation" => tool_reject_delegation(&state, agent_id, params).await,
        "complete_delegation" => tool_complete_delegation(&state, agent_id, params).await,
        "get_delegation" => tool_get_delegation(&state, params).await,
        "list_delegations" => tool_list_delegations(&state, params).await,

        // --- Streaming (Postgres + NATS) ---
        "start_stream" => tool_start_stream(&state, agent_id, params).await,
        "emit_stream_event" => tool_emit_stream_event(&state, agent_id, params).await,

        // --- A2A (Agent-to-Agent) ---
        "a2a_discover" => tool_a2a_discover(params).await,
        "a2a_delegate" => tool_a2a_delegate(params).await,

        // --- Workflow Orchestration ---
        "start_workflow" => tool_start_workflow(&state, agent_id, params).await,

        "ping" => Ok(serde_json::json!({ "pong": true })),

        _ => Err(BroodlinkError::NotFound(format!(
            "unknown tool: {tool_name}"
        ))),
    };

    // Audit log every tool invocation (best-effort, don't fail the request)
    let success = result.is_ok();
    let audit_error = result.as_ref().err().map(ToString::to_string);
    if let Err(e) = write_audit_log(
        &state.pg,
        &request_id,
        agent_id,
        SERVICE_NAME,
        &tool_name,
        success,
        audit_error.as_deref(),
    )
    .await
    {
        warn!(error = %e, "failed to write audit log");
    }

    // Publish NATS event (best-effort)
    let subject = format!(
        "{}.{}.beads-bridge.tool_call",
        state.config.nats.subject_prefix, state.config.broodlink.env,
    );
    let nats_payload = serde_json::json!({
        "tool": tool_name,
        "agent_id": agent_id,
        "request_id": request_id,
        "success": success,
    });
    if let Ok(bytes) = serde_json::to_vec(&nats_payload) {
        if let Err(e) = state.nats.publish(subject, bytes.into()).await {
            warn!(error = %e, "failed to publish nats event");
        }
    }

    let data = result?;

    Ok(Json(ToolResponse {
        tool: tool_name,
        success: true,
        data,
    }))
}

// ---------------------------------------------------------------------------
// Audit log (Postgres)
// ---------------------------------------------------------------------------

async fn write_audit_log(
    pg: &PgPool,
    trace_id: &str,
    agent_id: &str,
    service: &str,
    operation: &str,
    success: bool,
    error_msg: Option<&str>,
) -> Result<(), BroodlinkError> {
    let result_status = if success { "ok" } else { "error" };
    sqlx::query(
        "INSERT INTO audit_log (trace_id, agent_id, service, operation, result_status, result_summary, created_at)
         VALUES ($1, $2, $3, $4, $5, $6, NOW())",
    )
    .bind(trace_id)
    .bind(agent_id)
    .bind(service)
    .bind(operation)
    .bind(result_status)
    .bind(error_msg)
    .execute(pg)
    .await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Outbox helper (Postgres) — for memory operations → embedding pipeline
// ---------------------------------------------------------------------------

async fn write_outbox(
    pg: &PgPool,
    operation: &str,
    payload: &serde_json::Value,
) -> Result<(), BroodlinkError> {
    let trace_id = Uuid::new_v4().to_string();
    sqlx::query(
        "INSERT INTO outbox (trace_id, operation, payload, status, created_at)
         VALUES ($1, $2, $3, 'pending', NOW())",
    )
    .bind(&trace_id)
    .bind(operation)
    .bind(payload)
    .execute(pg)
    .await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Param extraction helpers
// ---------------------------------------------------------------------------

fn param_str<'a>(
    params: &'a serde_json::Value,
    field: &str,
) -> Result<&'a str, BroodlinkError> {
    params
        .get(field)
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| BroodlinkError::Validation {
            field: field.to_string(),
            message: "required string field".to_string(),
        })
}

fn param_str_opt<'a>(params: &'a serde_json::Value, field: &str) -> Option<&'a str> {
    params.get(field).and_then(serde_json::Value::as_str)
}

/// Extract a JSON value from params: accepts either a JSON object/array directly
/// or a JSON-encoded string.
fn param_json(
    params: &serde_json::Value,
    field: &str,
) -> Result<serde_json::Value, BroodlinkError> {
    match params.get(field) {
        Some(v) if v.is_object() || v.is_array() => Ok(v.clone()),
        Some(v) if v.is_string() => {
            serde_json::from_str(v.as_str().unwrap()).map_err(|e| BroodlinkError::Validation {
                field: field.to_string(),
                message: format!("invalid JSON: {e}"),
            })
        }
        _ => Err(BroodlinkError::Validation {
            field: field.to_string(),
            message: "required JSON field".to_string(),
        }),
    }
}

fn param_i64(params: &serde_json::Value, field: &str) -> Result<i64, BroodlinkError> {
    let val = params.get(field).ok_or_else(|| BroodlinkError::Validation {
        field: field.to_string(),
        message: "required integer field".to_string(),
    })?;

    // Try direct i64, then fall back to parsing a string representation
    if let Some(n) = val.as_i64() {
        return Ok(n);
    }
    if let Some(s) = val.as_str() {
        if let Ok(n) = s.parse::<i64>() {
            return Ok(n);
        }
    }

    Err(BroodlinkError::Validation {
        field: field.to_string(),
        message: "required integer field".to_string(),
    })
}

fn param_i64_opt(params: &serde_json::Value, field: &str) -> Option<i64> {
    params.get(field).and_then(serde_json::Value::as_i64)
}

// ===========================================================================
// Tool implementations
// ===========================================================================

// ---------------------------------------------------------------------------
// Memory tools (Dolt: agent_memory)
// ---------------------------------------------------------------------------

async fn tool_store_memory(
    state: &AppState,
    agent_id: &str,
    params: &serde_json::Value,
) -> Result<serde_json::Value, BroodlinkError> {
    let topic = param_str(params, "topic")?;
    let content = param_str(params, "content")?;
    let tags_str = param_str_opt(params, "tags");
    // Convert comma-separated tags string to JSON array for Dolt JSON column
    let tags_json: Option<String> = tags_str.map(|t| {
        let arr: Vec<&str> = t.split(',').map(str::trim).collect();
        serde_json::to_string(&arr).unwrap_or_else(|_| "[]".to_string())
    });

    sqlx::query(
        "INSERT INTO agent_memory (topic, content, agent_name, tags, created_at, updated_at)
         VALUES (?, ?, ?, ?, NOW(), NOW())
         ON DUPLICATE KEY UPDATE content = VALUES(content), tags = VALUES(tags), updated_at = NOW()",
    )
    .bind(topic)
    .bind(content)
    .bind(agent_id)
    .bind(&tags_json)
    .execute(&state.dolt)
    .await?;

    // Outbox write for embedding pipeline
    let outbox_payload = serde_json::json!({
        "topic": topic,
        "content": content,
        "agent_name": agent_id,
    });
    write_outbox(&state.pg, "embed", &outbox_payload).await?;

    info!(topic = %topic, agent = %agent_id, "memory stored + outbox queued");

    Ok(serde_json::json!({
        "topic": topic,
        "stored": true,
        "outbox_queued": true,
    }))
}

async fn tool_recall_memory(
    state: &AppState,
    _agent_id: &str,
    params: &serde_json::Value,
) -> Result<serde_json::Value, BroodlinkError> {
    let topic_search = param_str_opt(params, "topic_search");

    let rows = if let Some(search) = topic_search {
        let pattern = format!("%{search}%");
        sqlx::query_as::<_, (i64, String, String, String, Option<serde_json::Value>, String, String)>(
            "SELECT id, topic, content, agent_name, tags, CAST(created_at AS CHAR), CAST(updated_at AS CHAR)
             FROM agent_memory WHERE topic LIKE ? ORDER BY updated_at DESC LIMIT 50",
        )
        .bind(&pattern)
        .fetch_all(&state.dolt)
        .await?
    } else {
        sqlx::query_as::<_, (i64, String, String, String, Option<serde_json::Value>, String, String)>(
            "SELECT id, topic, content, agent_name, tags, CAST(created_at AS CHAR), CAST(updated_at AS CHAR)
             FROM agent_memory ORDER BY updated_at DESC LIMIT 50",
        )
        .fetch_all(&state.dolt)
        .await?
    };

    let results: Vec<serde_json::Value> = rows
        .into_iter()
        .map(|(id, topic, content, agent_name, tags, created, updated)| {
            serde_json::json!({
                "id": id,
                "topic": topic,
                "content": content,
                "agent_name": agent_name,
                "tags": tags,
                "created_at": created,
                "updated_at": updated,
            })
        })
        .collect();

    Ok(serde_json::json!({ "memories": results }))
}

async fn tool_delete_memory(
    state: &AppState,
    params: &serde_json::Value,
) -> Result<serde_json::Value, BroodlinkError> {
    let topic = param_str(params, "topic")?;

    let result = sqlx::query("DELETE FROM agent_memory WHERE topic = ?")
        .bind(topic)
        .execute(&state.dolt)
        .await?;

    Ok(serde_json::json!({
        "topic": topic,
        "deleted": result.rows_affected() > 0,
    }))
}

async fn tool_semantic_search(
    state: &AppState,
    _agent_id: &str,
    params: &serde_json::Value,
) -> Result<serde_json::Value, BroodlinkError> {
    state.qdrant_breaker.check().map_err(BroodlinkError::CircuitOpen)?;
    state.ollama_breaker.check().map_err(BroodlinkError::CircuitOpen)?;

    let query = param_str(params, "query")?;
    let limit = param_i64_opt(params, "limit").unwrap_or(5);

    // Step 1: get embedding from Ollama
    let ollama_url = format!("{}/api/embeddings", state.config.ollama.url);
    let embed_body = serde_json::json!({
        "model": state.config.ollama.embedding_model,
        "prompt": query,
    });

    let client = reqwest::Client::new();
    let embed_resp = client
        .post(&ollama_url)
        .json(&embed_body)
        .timeout(Duration::from_secs(state.config.ollama.timeout_seconds))
        .send()
        .await
        .map_err(|e| {
            state.ollama_breaker.record_failure();
            BroodlinkError::Internal(format!("ollama request failed: {e}"))
        })?;

    if !embed_resp.status().is_success() {
        state.ollama_breaker.record_failure();
        return Err(BroodlinkError::Internal(format!(
            "ollama returned status {}",
            embed_resp.status()
        )));
    }
    state.ollama_breaker.record_success();

    let embed_json: serde_json::Value = embed_resp
        .json()
        .await
        .map_err(|e| BroodlinkError::Internal(format!("ollama parse error: {e}")))?;

    let embedding = embed_json
        .get("embedding")
        .ok_or_else(|| BroodlinkError::Internal("no embedding in ollama response".to_string()))?;

    // Step 2: query Qdrant
    let qdrant_url = format!(
        "{}/collections/{}/points/search",
        state.config.qdrant.url, state.config.qdrant.collection,
    );
    let qdrant_body = serde_json::json!({
        "vector": {
            "name": "default",
            "vector": embedding,
        },
        "limit": limit,
        "with_payload": true,
    });

    let qdrant_resp = client
        .post(&qdrant_url)
        .json(&qdrant_body)
        .timeout(Duration::from_secs(10))
        .send()
        .await
        .map_err(|e| {
            state.qdrant_breaker.record_failure();
            BroodlinkError::Internal(format!("qdrant request failed: {e}"))
        })?;

    if !qdrant_resp.status().is_success() {
        state.qdrant_breaker.record_failure();
        return Err(BroodlinkError::Internal(format!(
            "qdrant returned status {}",
            qdrant_resp.status()
        )));
    }
    state.qdrant_breaker.record_success();

    let qdrant_json: serde_json::Value = qdrant_resp
        .json()
        .await
        .map_err(|e| BroodlinkError::Internal(format!("qdrant parse error: {e}")))?;

    Ok(serde_json::json!({
        "query": query,
        "results": qdrant_json.get("result"),
    }))
}

// ---------------------------------------------------------------------------
// Work tools (Postgres: work_log)
// ---------------------------------------------------------------------------

async fn tool_log_work(
    state: &AppState,
    agent_id: &str,
    params: &serde_json::Value,
) -> Result<serde_json::Value, BroodlinkError> {
    let action = param_str(params, "action")?;
    let details = param_str(params, "details")?;
    let files_changed = params.get("files_changed");
    let trace_id = Uuid::new_v4().to_string();

    // files_changed is JSONB — bind as serde_json::Value directly
    let files_json: Option<serde_json::Value> = files_changed.cloned();
    sqlx::query(
        "INSERT INTO work_log (trace_id, agent_id, action, details, files_changed, created_at)
         VALUES ($1, $2, $3, $4, $5, NOW())",
    )
    .bind(&trace_id)
    .bind(agent_id)
    .bind(action)
    .bind(details)
    .bind(&files_json)
    .execute(&state.pg)
    .await?;

    Ok(serde_json::json!({ "trace_id": trace_id, "logged": true }))
}

async fn tool_get_work_log(
    state: &AppState,
    _agent_id: &str,
    params: &serde_json::Value,
) -> Result<serde_json::Value, BroodlinkError> {
    let limit = param_i64_opt(params, "limit").unwrap_or(20);
    let agent_filter = param_str_opt(params, "agent_id");

    let rows = if let Some(agent) = agent_filter {
        sqlx::query_as::<_, (i64, String, String, String, Option<serde_json::Value>, String)>(
            "SELECT id, agent_id, action, details, files_changed, created_at::text
             FROM work_log WHERE agent_id = $1 ORDER BY created_at DESC LIMIT $2",
        )
        .bind(agent)
        .bind(limit)
        .fetch_all(&state.pg)
        .await?
    } else {
        sqlx::query_as::<_, (i64, String, String, String, Option<serde_json::Value>, String)>(
            "SELECT id, agent_id, action, details, files_changed, created_at::text
             FROM work_log ORDER BY created_at DESC LIMIT $1",
        )
        .bind(limit)
        .fetch_all(&state.pg)
        .await?
    };

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

    Ok(serde_json::json!({ "entries": entries }))
}

// ---------------------------------------------------------------------------
// Project tools (Dolt: projects)
// ---------------------------------------------------------------------------

async fn tool_list_projects(
    state: &AppState,
    _agent_id: &str,
    params: &serde_json::Value,
) -> Result<serde_json::Value, BroodlinkError> {
    let status_filter = param_str_opt(params, "status");

    let rows = if let Some(status) = status_filter {
        sqlx::query_as::<_, (i64, String, String, String, String, Option<String>, String, String)>(
            "SELECT id, name, status, description, agent_name, tags, CAST(created_at AS CHAR), CAST(updated_at AS CHAR)
             FROM projects WHERE status = ? ORDER BY updated_at DESC",
        )
        .bind(status)
        .fetch_all(&state.dolt)
        .await?
    } else {
        sqlx::query_as::<_, (i64, String, String, String, String, Option<String>, String, String)>(
            "SELECT id, name, status, description, agent_name, tags, CAST(created_at AS CHAR), CAST(updated_at AS CHAR)
             FROM projects ORDER BY updated_at DESC",
        )
        .fetch_all(&state.dolt)
        .await?
    };

    let projects: Vec<serde_json::Value> = rows
        .into_iter()
        .map(
            |(id, name, status, description, agent_name, tags, created_at, updated_at)| {
                serde_json::json!({
                    "id": id,
                    "name": name,
                    "status": status,
                    "description": description,
                    "agent_name": agent_name,
                    "tags": tags,
                    "created_at": created_at,
                    "updated_at": updated_at,
                })
            },
        )
        .collect();

    Ok(serde_json::json!({ "projects": projects }))
}

async fn tool_update_project(
    state: &AppState,
    _agent_id: &str,
    params: &serde_json::Value,
) -> Result<serde_json::Value, BroodlinkError> {
    let project_id = param_i64(params, "project_id")?;
    let status = param_str_opt(params, "status");
    let description = param_str_opt(params, "description");
    let tags = param_str_opt(params, "tags");

    if status.is_none() && description.is_none() && tags.is_none() {
        return Err(BroodlinkError::Validation {
            field: "params".to_string(),
            message: "at least one of status, description, or tags required".to_string(),
        });
    }

    // Build dynamic SET clause
    let mut sets = Vec::new();
    let mut binds: Vec<String> = Vec::new();

    if let Some(s) = status {
        sets.push("status = ?");
        binds.push(s.to_string());
    }
    if let Some(d) = description {
        sets.push("description = ?");
        binds.push(d.to_string());
    }
    if let Some(t) = tags {
        sets.push("tags = ?");
        binds.push(t.to_string());
    }
    sets.push("updated_at = NOW()");

    let sql = format!("UPDATE projects SET {} WHERE id = ?", sets.join(", "));
    let mut query = sqlx::query(&sql);
    for b in &binds {
        query = query.bind(b);
    }
    query = query.bind(project_id);
    query.execute(&state.dolt).await?;

    Ok(serde_json::json!({ "project_id": project_id, "updated": true }))
}

async fn tool_add_project(
    state: &AppState,
    agent_id: &str,
    params: &serde_json::Value,
) -> Result<serde_json::Value, BroodlinkError> {
    let name = param_str(params, "name")?;
    let description = param_str(params, "description")?;
    let status = param_str_opt(params, "status").unwrap_or("planning");
    let tags_str = param_str_opt(params, "tags");
    let tags_json: Option<String> = tags_str.map(|t| {
        let arr: Vec<&str> = t.split(',').map(str::trim).collect();
        serde_json::to_string(&arr).unwrap_or_else(|_| "[]".to_string())
    });

    sqlx::query(
        "INSERT INTO projects (name, description, status, agent_name, tags, created_at, updated_at)
         VALUES (?, ?, ?, ?, ?, NOW(), NOW())",
    )
    .bind(name)
    .bind(description)
    .bind(status)
    .bind(agent_id)
    .bind(&tags_json)
    .execute(&state.dolt)
    .await?;

    Ok(serde_json::json!({ "name": name, "status": status, "created": true }))
}

// ---------------------------------------------------------------------------
// Skills (Dolt)
// ---------------------------------------------------------------------------

async fn tool_list_skills(
    state: &AppState,
    params: &serde_json::Value,
) -> Result<serde_json::Value, BroodlinkError> {
    let agent_filter = param_str_opt(params, "agent_name");

    let rows = if let Some(agent) = agent_filter {
        sqlx::query_as::<_, (i64, String, String, Option<String>, Option<serde_json::Value>, String)>(
            "SELECT id, name, description, agent_name, examples, CAST(created_at AS CHAR)
             FROM skills WHERE agent_name = ? ORDER BY name",
        )
        .bind(agent)
        .fetch_all(&state.dolt)
        .await?
    } else {
        sqlx::query_as::<_, (i64, String, String, Option<String>, Option<serde_json::Value>, String)>(
            "SELECT id, name, description, agent_name, examples, CAST(created_at AS CHAR)
             FROM skills ORDER BY name",
        )
        .fetch_all(&state.dolt)
        .await?
    };

    let skills: Vec<serde_json::Value> = rows
        .into_iter()
        .map(|(id, name, description, agent_name, examples, created_at)| {
            serde_json::json!({
                "id": id,
                "name": name,
                "description": description,
                "agent_name": agent_name,
                "examples": examples,
                "created_at": created_at,
            })
        })
        .collect();

    Ok(serde_json::json!({ "skills": skills }))
}

async fn tool_add_skill(
    state: &AppState,
    params: &serde_json::Value,
) -> Result<serde_json::Value, BroodlinkError> {
    let name = param_str(params, "name")?;
    let description = param_str(params, "description")?;
    let agent_name = param_str_opt(params, "agent_name").unwrap_or("claude-code");
    let examples = params.get("examples");
    let examples_json: Option<String> = examples.map(|v| {
        if v.is_array() || v.is_object() {
            v.to_string()
        } else if let Some(s) = v.as_str() {
            serde_json::to_string(&[s]).unwrap_or_else(|_| "[]".to_string())
        } else {
            "[]".to_string()
        }
    });

    sqlx::query(
        "INSERT INTO skills (name, description, agent_name, examples, created_at, updated_at)
         VALUES (?, ?, ?, ?, NOW(), NOW())
         ON DUPLICATE KEY UPDATE description = VALUES(description), examples = VALUES(examples), updated_at = NOW()",
    )
    .bind(name)
    .bind(description)
    .bind(agent_name)
    .bind(&examples_json)
    .execute(&state.dolt)
    .await?;

    Ok(serde_json::json!({ "name": name, "agent_name": agent_name, "upserted": true }))
}

// ---------------------------------------------------------------------------
// Conversations (Dolt)
// ---------------------------------------------------------------------------

async fn tool_log_conversation(
    state: &AppState,
    params: &serde_json::Value,
) -> Result<serde_json::Value, BroodlinkError> {
    let agent_name = param_str(params, "agent_name")?;
    let role = param_str(params, "role")?;
    let content = param_str(params, "content")?;
    let user_id = param_str_opt(params, "user_id");
    let session_id = param_str_opt(params, "session_id");
    let metadata = params.get("metadata");
    let metadata_json: Option<String> = metadata.map(|v| v.to_string());

    sqlx::query(
        "INSERT INTO conversations (agent_name, user_id, session_id, role, content, metadata, created_at)
         VALUES (?, ?, ?, ?, ?, ?, NOW())",
    )
    .bind(agent_name)
    .bind(user_id)
    .bind(session_id)
    .bind(role)
    .bind(content)
    .bind(&metadata_json)
    .execute(&state.dolt)
    .await?;

    Ok(serde_json::json!({ "logged": true, "agent_name": agent_name, "role": role }))
}

// ---------------------------------------------------------------------------
// Beads tools (Dolt: beads_issues + external bd binary)
// ---------------------------------------------------------------------------

async fn tool_beads_list_issues(
    state: &AppState,
    _agent_id: &str,
    params: &serde_json::Value,
) -> Result<serde_json::Value, BroodlinkError> {
    let status_filter = param_str_opt(params, "status");
    let limit = param_i64_opt(params, "limit").unwrap_or(50);

    let rows = if let Some(status) = status_filter {
        sqlx::query_as::<_, (String, String, Option<String>, Option<String>, Option<String>, String, String)>(
            "SELECT bead_id, title, status, assignee, convoy_id, CAST(created_at AS CHAR), CAST(updated_at AS CHAR)
             FROM beads_issues WHERE status = ? ORDER BY updated_at DESC LIMIT ?",
        )
        .bind(status)
        .bind(limit)
        .fetch_all(&state.dolt)
        .await?
    } else {
        sqlx::query_as::<_, (String, String, Option<String>, Option<String>, Option<String>, String, String)>(
            "SELECT bead_id, title, status, assignee, convoy_id, CAST(created_at AS CHAR), CAST(updated_at AS CHAR)
             FROM beads_issues ORDER BY updated_at DESC LIMIT ?",
        )
        .bind(limit)
        .fetch_all(&state.dolt)
        .await?
    };

    let issues: Vec<serde_json::Value> = rows
        .into_iter()
        .map(
            |(bead_id, title, status, assignee, convoy_id, created_at, updated_at)| {
                serde_json::json!({
                    "bead_id": bead_id,
                    "title": title,
                    "status": status,
                    "assignee": assignee,
                    "convoy_id": convoy_id,
                    "created_at": created_at,
                    "updated_at": updated_at,
                })
            },
        )
        .collect();

    Ok(serde_json::json!({ "issues": issues }))
}

async fn tool_beads_get_issue(
    state: &AppState,
    _agent_id: &str,
    params: &serde_json::Value,
) -> Result<serde_json::Value, BroodlinkError> {
    let issue_id = param_str(params, "issue_id")?;

    let row = sqlx::query_as::<_, (String, String, Option<String>, Option<String>, Option<String>, Option<String>, Option<String>, String, String)>(
        "SELECT bead_id, title, status, description, assignee, convoy_id, formula, CAST(created_at AS CHAR), CAST(updated_at AS CHAR)
         FROM beads_issues WHERE bead_id = ?",
    )
    .bind(issue_id)
    .fetch_optional(&state.dolt)
    .await?
    .ok_or_else(|| BroodlinkError::NotFound(format!("issue {issue_id}")))?;

    Ok(serde_json::json!({
        "bead_id": row.0,
        "title": row.1,
        "status": row.2,
        "description": row.3,
        "assignee": row.4,
        "convoy_id": row.5,
        "formula": row.6,
        "created_at": row.7,
        "updated_at": row.8,
    }))
}

async fn tool_beads_update_status(
    state: &AppState,
    _agent_id: &str,
    params: &serde_json::Value,
) -> Result<serde_json::Value, BroodlinkError> {
    let issue_id = param_str(params, "issue_id")?;
    let status = param_str(params, "status")?;

    let result = sqlx::query(
        "UPDATE beads_issues SET status = ?, updated_at = NOW() WHERE bead_id = ?",
    )
    .bind(status)
    .bind(issue_id)
    .execute(&state.dolt)
    .await?;

    if result.rows_affected() == 0 {
        return Err(BroodlinkError::NotFound(format!("issue {issue_id}")));
    }

    Ok(serde_json::json!({ "issue_id": issue_id, "status": status, "updated": true }))
}

async fn tool_beads_create_issue(
    state: &AppState,
    _agent_id: &str,
    params: &serde_json::Value,
) -> Result<serde_json::Value, BroodlinkError> {
    let title = param_str(params, "title")?;
    let description = param_str_opt(params, "description").unwrap_or("");
    let assignee = param_str_opt(params, "assignee");
    let convoy_id = param_str_opt(params, "convoy_id");
    let formula = param_str_opt(params, "formula");
    // Generate a short bead_id (e.g. "BD-a1b2c3") instead of a full UUID
    let short_id = format!("BD-{}", &Uuid::new_v4().to_string()[..8]);

    sqlx::query(
        "INSERT INTO beads_issues (bead_id, title, description, status, assignee, convoy_id, formula, created_at, updated_at)
         VALUES (?, ?, ?, 'open', ?, ?, ?, NOW(), NOW())",
    )
    .bind(&short_id)
    .bind(title)
    .bind(description)
    .bind(assignee)
    .bind(convoy_id)
    .bind(formula)
    .execute(&state.dolt)
    .await?;

    Ok(serde_json::json!({ "bead_id": short_id, "title": title, "created": true }))
}

async fn tool_beads_list_formulas(
    state: &AppState,
    _agent_id: &str,
    _params: &serde_json::Value,
) -> Result<serde_json::Value, BroodlinkError> {
    let formulas_dir = &state.config.beads.formulas_dir;

    let output = tokio::process::Command::new("ls")
        .arg("-1")
        .arg(formulas_dir)
        .output()
        .await
        .map_err(|e| BroodlinkError::Internal(format!("failed to list formulas: {e}")))?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let formulas: Vec<&str> = stdout.lines().filter(|l| !l.is_empty()).collect();

    Ok(serde_json::json!({ "formulas": formulas, "directory": formulas_dir }))
}

async fn tool_beads_run_formula(
    state: &AppState,
    agent_id: &str,
    params: &serde_json::Value,
) -> Result<serde_json::Value, BroodlinkError> {
    let formula = param_str(params, "formula")?;
    let bd_binary = &state.config.beads.bd_binary;
    let workspace = &state.config.beads.workspace;

    info!(formula = %formula, agent = %agent_id, "running formula");

    let output = tokio::process::Command::new(bd_binary)
        .arg("run")
        .arg(formula)
        .current_dir(workspace)
        .output()
        .await
        .map_err(|e| BroodlinkError::Internal(format!("failed to run bd: {e}")))?;

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    let exit_code = output.status.code().unwrap_or(-1);

    Ok(serde_json::json!({
        "formula": formula,
        "exit_code": exit_code,
        "stdout": stdout,
        "stderr": stderr,
    }))
}

async fn tool_beads_get_convoy(
    state: &AppState,
    _agent_id: &str,
    params: &serde_json::Value,
) -> Result<serde_json::Value, BroodlinkError> {
    let convoy_id = param_str_opt(params, "convoy_id");
    let bd_binary = &state.config.beads.bd_binary;
    let workspace = &state.config.beads.workspace;

    let mut cmd = tokio::process::Command::new(bd_binary);
    cmd.arg("convoy").arg("status").current_dir(workspace);

    if let Some(cid) = convoy_id {
        cmd.arg(cid);
    }

    let output = cmd
        .output()
        .await
        .map_err(|e| BroodlinkError::Internal(format!("failed to run bd convoy: {e}")))?;

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();

    Ok(serde_json::json!({
        "convoy_id": convoy_id,
        "output": stdout,
    }))
}

// ---------------------------------------------------------------------------
// Messaging tools (Postgres: messages)
// ---------------------------------------------------------------------------

async fn tool_send_message(
    state: &AppState,
    agent_id: &str,
    params: &serde_json::Value,
) -> Result<serde_json::Value, BroodlinkError> {
    let recipient = param_str(params, "to")?;
    let body = param_str(params, "content")?;
    let subject_field = param_str_opt(params, "subject").unwrap_or("direct");
    let id = Uuid::new_v4().to_string();
    let trace_id = Uuid::new_v4().to_string();

    sqlx::query(
        "INSERT INTO messages (id, trace_id, sender, recipient, subject, body, status, created_at)
         VALUES ($1, $2, $3, $4, $5, $6, 'unread', NOW())",
    )
    .bind(&id)
    .bind(&trace_id)
    .bind(agent_id)
    .bind(recipient)
    .bind(subject_field)
    .bind(body)
    .execute(&state.pg)
    .await?;

    // Notify via NATS
    let nats_subject = format!(
        "{}.{}.messaging.new",
        state.config.nats.subject_prefix, state.config.broodlink.env,
    );
    let nats_payload = serde_json::json!({
        "message_id": id,
        "from": agent_id,
        "to": recipient,
        "subject": subject_field,
    });
    if let Ok(bytes) = serde_json::to_vec(&nats_payload) {
        if let Err(e) = state.nats.publish(nats_subject, bytes.into()).await {
            warn!(error = %e, "failed to publish message notification");
        }
    }

    Ok(serde_json::json!({ "id": id, "sent": true }))
}

async fn tool_read_messages(
    state: &AppState,
    agent_id: &str,
    params: &serde_json::Value,
) -> Result<serde_json::Value, BroodlinkError> {
    let limit = param_i64_opt(params, "limit").unwrap_or(20);
    let unread_only = params
        .get("unread_only")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);

    let rows = if unread_only {
        sqlx::query_as::<_, (String, String, String, Option<String>, String, String, String)>(
            "SELECT id, sender, recipient, subject, body, status, created_at::text
             FROM messages WHERE recipient = $1 AND status = 'unread'
             ORDER BY created_at DESC LIMIT $2",
        )
        .bind(agent_id)
        .bind(limit)
        .fetch_all(&state.pg)
        .await?
    } else {
        sqlx::query_as::<_, (String, String, String, Option<String>, String, String, String)>(
            "SELECT id, sender, recipient, subject, body, status, created_at::text
             FROM messages WHERE recipient = $1
             ORDER BY created_at DESC LIMIT $2",
        )
        .bind(agent_id)
        .bind(limit)
        .fetch_all(&state.pg)
        .await?
    };

    // Mark retrieved messages as read
    let ids: Vec<String> = rows.iter().map(|(id, ..)| id.clone()).collect();
    if !ids.is_empty() {
        let placeholders: Vec<String> = (1..=ids.len()).map(|i| format!("${i}")).collect();
        let sql = format!(
            "UPDATE messages SET status = 'read' WHERE id IN ({})",
            placeholders.join(", ")
        );
        let mut query = sqlx::query(&sql);
        for id in &ids {
            query = query.bind(id);
        }
        if let Err(e) = query.execute(&state.pg).await {
            warn!(error = %e, "failed to mark messages as read");
        }
    }

    let messages: Vec<serde_json::Value> = rows
        .into_iter()
        .map(
            |(id, sender, recipient, subject, body, status, created_at)| {
                serde_json::json!({
                    "id": id,
                    "sender": sender,
                    "recipient": recipient,
                    "subject": subject,
                    "body": body,
                    "status": status,
                    "created_at": created_at,
                })
            },
        )
        .collect();

    Ok(serde_json::json!({ "messages": messages }))
}

// ---------------------------------------------------------------------------
// Decision tools (Dolt: decisions)
// ---------------------------------------------------------------------------

async fn tool_log_decision(
    state: &AppState,
    agent_id: &str,
    params: &serde_json::Value,
) -> Result<serde_json::Value, BroodlinkError> {
    let decision = param_str(params, "decision")?;
    let reasoning = param_str_opt(params, "reasoning").unwrap_or("");
    let outcome = param_str_opt(params, "outcome");
    let alternatives = params.get("alternatives");
    let trace_id = Uuid::new_v4().to_string();

    sqlx::query(
        "INSERT INTO decisions (trace_id, agent_id, decision, reasoning, alternatives, outcome, created_at)
         VALUES (?, ?, ?, ?, ?, ?, NOW())",
    )
    .bind(&trace_id)
    .bind(agent_id)
    .bind(decision)
    .bind(reasoning)
    .bind(alternatives.map(serde_json::Value::to_string))
    .bind(outcome)
    .execute(&state.dolt)
    .await?;

    Ok(serde_json::json!({ "trace_id": trace_id, "logged": true }))
}

async fn tool_get_decisions(
    state: &AppState,
    _agent_id: &str,
    params: &serde_json::Value,
) -> Result<serde_json::Value, BroodlinkError> {
    let limit = param_i64_opt(params, "limit").unwrap_or(20);
    let agent_filter = param_str_opt(params, "agent_id");

    let rows = if let Some(agent) = agent_filter {
        sqlx::query_as::<_, (i64, String, String, String, Option<String>, String)>(
            "SELECT id, agent_id, decision, reasoning, outcome, CAST(created_at AS CHAR)
             FROM decisions WHERE agent_id = ? ORDER BY created_at DESC LIMIT ?",
        )
        .bind(agent)
        .bind(limit)
        .fetch_all(&state.dolt)
        .await?
    } else {
        sqlx::query_as::<_, (i64, String, String, String, Option<String>, String)>(
            "SELECT id, agent_id, decision, reasoning, outcome, CAST(created_at AS CHAR)
             FROM decisions ORDER BY created_at DESC LIMIT ?",
        )
        .bind(limit)
        .fetch_all(&state.dolt)
        .await?
    };

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

    Ok(serde_json::json!({ "decisions": decisions }))
}

// ---------------------------------------------------------------------------
// Queue tools (Postgres: task_queue)
// ---------------------------------------------------------------------------

async fn tool_claim_task(
    state: &AppState,
    agent_id: &str,
    params: &serde_json::Value,
) -> Result<serde_json::Value, BroodlinkError> {
    let task_id = param_str_opt(params, "task_id");

    // Atomic claim: use UPDATE ... WHERE to avoid races
    let row = if let Some(tid) = task_id {
        sqlx::query_as::<_, (String, String, Option<String>, Option<i32>, String)>(
            "UPDATE task_queue SET status = 'claimed', assigned_agent = $1, claimed_at = NOW(), updated_at = NOW()
             WHERE id = $2 AND status = 'pending'
             RETURNING id, title, description, priority, created_at::text",
        )
        .bind(agent_id)
        .bind(tid)
        .fetch_optional(&state.pg)
        .await?
    } else {
        // Claim next available task by priority
        sqlx::query_as::<_, (String, String, Option<String>, Option<i32>, String)>(
            "UPDATE task_queue SET status = 'claimed', assigned_agent = $1, claimed_at = NOW(), updated_at = NOW()
             WHERE id = (
                 SELECT id FROM task_queue
                 WHERE status = 'pending'
                 ORDER BY priority DESC NULLS LAST, created_at ASC
                 LIMIT 1
                 FOR UPDATE SKIP LOCKED
             )
             RETURNING id, title, description, priority, created_at::text",
        )
        .bind(agent_id)
        .fetch_optional(&state.pg)
        .await?
    };

    match row {
        Some((id, title, description, priority, created_at)) => {
            // Publish NATS notification
            let subject = format!(
                "{}.{}.coordinator.task_claimed",
                state.config.nats.subject_prefix, state.config.broodlink.env,
            );
            let nats_payload = serde_json::json!({
                "task_id": id,
                "agent_id": agent_id,
            });
            if let Ok(bytes) = serde_json::to_vec(&nats_payload) {
                if let Err(e) = state.nats.publish(subject, bytes.into()).await {
                    warn!(error = %e, "failed to publish task claimed event");
                }
            }

            Ok(serde_json::json!({
                "id": id,
                "title": title,
                "description": description,
                "priority": priority,
                "created_at": created_at,
                "claimed": true,
            }))
        }
        None => Ok(serde_json::json!({ "claimed": false, "message": "no tasks available" })),
    }
}

async fn tool_complete_task(
    state: &AppState,
    agent_id: &str,
    params: &serde_json::Value,
) -> Result<serde_json::Value, BroodlinkError> {
    let task_id = param_str(params, "task_id")?;
    let result_data: Option<serde_json::Value> = params.get("result").cloned();

    let affected = sqlx::query(
        "UPDATE task_queue SET status = 'completed', result_data = $3, completed_at = NOW(), updated_at = NOW()
         WHERE id = $1 AND assigned_agent = $2 AND status IN ('claimed', 'in_progress')",
    )
    .bind(task_id)
    .bind(agent_id)
    .bind(&result_data)
    .execute(&state.pg)
    .await?;

    if affected.rows_affected() == 0 {
        return Err(BroodlinkError::NotFound(format!(
            "task {task_id} not claimed/in_progress for agent {agent_id}"
        )));
    }

    // Update agent_metrics (best-effort)
    update_agent_metrics(&state.pg, agent_id, true).await;

    // Notify coordinator of completion (for workflow step chaining)
    let subject = format!(
        "{}.{}.coordinator.task_completed",
        state.config.nats.subject_prefix, state.config.broodlink.env,
    );
    let nats_payload = serde_json::json!({
        "task_id": task_id,
        "agent_id": agent_id,
        "result_data": result_data,
    });
    if let Ok(bytes) = serde_json::to_vec(&nats_payload) {
        if let Err(e) = state.nats.publish(subject, bytes.into()).await {
            warn!(error = %e, "failed to publish task_completed event");
        }
    }

    Ok(serde_json::json!({ "task_id": task_id, "completed": true }))
}

async fn tool_fail_task(
    state: &AppState,
    agent_id: &str,
    params: &serde_json::Value,
) -> Result<serde_json::Value, BroodlinkError> {
    let task_id = param_str(params, "task_id")?;

    let affected = sqlx::query(
        "UPDATE task_queue SET status = 'failed', completed_at = NOW(), updated_at = NOW()
         WHERE id = $1 AND assigned_agent = $2 AND status IN ('claimed', 'in_progress')",
    )
    .bind(task_id)
    .bind(agent_id)
    .execute(&state.pg)
    .await?;

    if affected.rows_affected() == 0 {
        return Err(BroodlinkError::NotFound(format!(
            "task {task_id} not claimed/in_progress for agent {agent_id}"
        )));
    }

    // Update agent_metrics (best-effort)
    update_agent_metrics(&state.pg, agent_id, false).await;

    // Notify coordinator of failure (for workflow step chaining)
    let subject = format!(
        "{}.{}.coordinator.task_failed",
        state.config.nats.subject_prefix, state.config.broodlink.env,
    );
    let nats_payload = serde_json::json!({
        "task_id": task_id,
        "agent_id": agent_id,
    });
    if let Ok(bytes) = serde_json::to_vec(&nats_payload) {
        if let Err(e) = state.nats.publish(subject, bytes.into()).await {
            warn!(error = %e, "failed to publish task_failed event");
        }
    }

    Ok(serde_json::json!({ "task_id": task_id, "failed": true }))
}

// ---------------------------------------------------------------------------
// Agent tools (Dolt: agent_profiles)
// ---------------------------------------------------------------------------

async fn tool_agent_upsert(
    state: &AppState,
    _agent_id: &str,
    params: &serde_json::Value,
) -> Result<serde_json::Value, BroodlinkError> {
    let target_agent_id = param_str(params, "agent_id")?;
    let display_name = param_str(params, "display_name")?;
    let role = param_str(params, "role")?;
    let transport = param_str_opt(params, "transport").unwrap_or("mcp");
    let cost_tier = param_str_opt(params, "cost_tier").unwrap_or("medium");
    // capabilities is a JSON column — accept array or string and serialize to JSON string
    let capabilities_json: Option<String> = params.get("capabilities").map(|v| {
        if v.is_array() || v.is_object() {
            v.to_string()
        } else if let Some(s) = v.as_str() {
            // Treat as comma-separated list
            let arr: Vec<&str> = s.split(',').map(str::trim).collect();
            serde_json::to_string(&arr).unwrap_or_else(|_| "[]".to_string())
        } else {
            "[]".to_string()
        }
    });
    let active = params
        .get("active")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(true);

    sqlx::query(
        "INSERT INTO agent_profiles (agent_id, display_name, role, transport, cost_tier, capabilities, active, last_seen, created_at, updated_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, NOW(), NOW(), NOW())
         ON DUPLICATE KEY UPDATE display_name = VALUES(display_name), role = VALUES(role), transport = VALUES(transport),
            cost_tier = VALUES(cost_tier), capabilities = VALUES(capabilities),
            active = VALUES(active), last_seen = NOW(), updated_at = NOW()",
    )
    .bind(target_agent_id)
    .bind(display_name)
    .bind(role)
    .bind(transport)
    .bind(cost_tier)
    .bind(&capabilities_json)
    .bind(active)
    .execute(&state.dolt)
    .await?;

    Ok(serde_json::json!({ "agent_id": target_agent_id, "upserted": true }))
}

// ---------------------------------------------------------------------------
// Utility tools
// ---------------------------------------------------------------------------

async fn tool_health_check(
    state: &AppState,
) -> Result<serde_json::Value, BroodlinkError> {
    let dolt_ok = sqlx::query("SELECT 1").execute(&state.dolt).await.is_ok();
    let pg_ok = sqlx::query("SELECT 1").execute(&state.pg).await.is_ok();
    let nats_ok = state.nats.flush().await.is_ok();

    Ok(serde_json::json!({
        "dolt": dolt_ok,
        "postgres": pg_ok,
        "nats": nats_ok,
        "qdrant_circuit": if state.qdrant_breaker.is_open() { "open" } else { "closed" },
        "ollama_circuit": if state.ollama_breaker.is_open() { "open" } else { "closed" },
        "uptime_seconds": state.start_time.elapsed().as_secs(),
    }))
}

#[allow(clippy::unused_async)]
async fn tool_get_config_info(
    state: &AppState,
) -> Result<serde_json::Value, BroodlinkError> {
    Ok(serde_json::json!({
        "env": state.config.broodlink.env,
        "version": state.config.broodlink.version,
        "profile": state.config.profile.name,
        "residency_region": state.config.residency.region,
        "data_classification": state.config.residency.data_classification,
        "dolt_host": state.config.dolt.host,
        "dolt_port": state.config.dolt.port,
        "postgres_host": state.config.postgres.host,
        "postgres_port": state.config.postgres.port,
        "nats_url": state.config.nats.url,
        "qdrant_url": state.config.qdrant.url,
        "ollama_url": state.config.ollama.url,
        "rate_limits": {
            "rpm": state.config.rate_limits.requests_per_minute_per_agent,
            "burst": state.config.rate_limits.burst,
        },
    }))
}

async fn tool_list_agents(
    state: &AppState,
) -> Result<serde_json::Value, BroodlinkError> {
    let rows = sqlx::query_as::<_, (String, String, String, String, String, Option<serde_json::Value>, bool, String, String)>(
        "SELECT agent_id, display_name, role, transport, cost_tier, capabilities, active, CAST(created_at AS CHAR), CAST(updated_at AS CHAR)
         FROM agent_profiles ORDER BY agent_id",
    )
    .fetch_all(&state.dolt)
    .await?;

    let agents: Vec<serde_json::Value> = rows
        .into_iter()
        .map(
            |(agent_id, display_name, role, transport, cost_tier, capabilities, active, created_at, updated_at)| {
                serde_json::json!({
                    "agent_id": agent_id,
                    "display_name": display_name,
                    "role": role,
                    "transport": transport,
                    "cost_tier": cost_tier,
                    "capabilities": capabilities,
                    "active": active,
                    "created_at": created_at,
                    "updated_at": updated_at,
                })
            },
        )
        .collect();

    Ok(serde_json::json!({ "agents": agents }))
}

async fn tool_get_agent(
    state: &AppState,
    params: &serde_json::Value,
) -> Result<serde_json::Value, BroodlinkError> {
    let agent_id = param_str(params, "agent_id")?;

    let row = sqlx::query_as::<_, (String, String, String, String, String, Option<serde_json::Value>, bool, String, String)>(
        "SELECT agent_id, display_name, role, transport, cost_tier, capabilities, active, CAST(created_at AS CHAR), CAST(updated_at AS CHAR)
         FROM agent_profiles WHERE agent_id = ?",
    )
    .bind(agent_id)
    .fetch_optional(&state.dolt)
    .await?
    .ok_or_else(|| BroodlinkError::NotFound(format!("agent {agent_id}")))?;

    Ok(serde_json::json!({
        "agent_id": row.0,
        "display_name": row.1,
        "role": row.2,
        "transport": row.3,
        "cost_tier": row.4,
        "capabilities": row.5,
        "active": row.6,
        "created_at": row.7,
        "updated_at": row.8,
    }))
}

async fn tool_get_audit_log(
    state: &AppState,
    params: &serde_json::Value,
) -> Result<serde_json::Value, BroodlinkError> {
    let limit = param_i64_opt(params, "limit").unwrap_or(50);
    let agent_filter = param_str_opt(params, "agent_id");

    let rows = if let Some(agent) = agent_filter {
        sqlx::query_as::<_, (i64, String, String, String, String, Option<String>, String)>(
            "SELECT id, agent_id, service, operation, result_status, result_summary, created_at::text
             FROM audit_log WHERE agent_id = $1 ORDER BY created_at DESC LIMIT $2",
        )
        .bind(agent)
        .bind(limit)
        .fetch_all(&state.pg)
        .await?
    } else {
        sqlx::query_as::<_, (i64, String, String, String, String, Option<String>, String)>(
            "SELECT id, agent_id, service, operation, result_status, result_summary, created_at::text
             FROM audit_log ORDER BY created_at DESC LIMIT $1",
        )
        .bind(limit)
        .fetch_all(&state.pg)
        .await?
    };

    let entries: Vec<serde_json::Value> = rows
        .into_iter()
        .map(
            |(id, agent_id, service, operation, result_status, result_summary, created_at)| {
                serde_json::json!({
                    "id": id,
                    "agent_id": agent_id,
                    "service": service,
                    "operation": operation,
                    "result_status": result_status,
                    "result_summary": result_summary,
                    "created_at": created_at,
                })
            },
        )
        .collect();

    Ok(serde_json::json!({ "entries": entries }))
}

async fn tool_get_task(
    state: &AppState,
    params: &serde_json::Value,
) -> Result<serde_json::Value, BroodlinkError> {
    let task_id = param_str(params, "task_id")?;

    let row = sqlx::query_as::<_, (String, String, Option<String>, String, Option<i32>, Option<String>, String, Option<serde_json::Value>)>(
        "SELECT id, title, description, status, priority, assigned_agent, created_at::text, result_data
         FROM task_queue WHERE id = $1",
    )
    .bind(task_id)
    .fetch_optional(&state.pg)
    .await?
    .ok_or_else(|| BroodlinkError::NotFound(format!("task {task_id}")))?;

    Ok(serde_json::json!({
        "id": row.0,
        "title": row.1,
        "description": row.2,
        "status": row.3,
        "priority": row.4,
        "assigned_agent": row.5,
        "created_at": row.6,
        "result_data": row.7,
    }))
}

async fn tool_list_tasks(
    state: &AppState,
    params: &serde_json::Value,
) -> Result<serde_json::Value, BroodlinkError> {
    let limit = param_i64_opt(params, "limit").unwrap_or(50);
    let status_filter = param_str_opt(params, "status");

    let rows = if let Some(status) = status_filter {
        sqlx::query_as::<_, (String, String, String, Option<i32>, Option<String>, String)>(
            "SELECT id, title, status, priority, assigned_agent, created_at::text
             FROM task_queue WHERE status = $1 ORDER BY created_at DESC LIMIT $2",
        )
        .bind(status)
        .bind(limit)
        .fetch_all(&state.pg)
        .await?
    } else {
        sqlx::query_as::<_, (String, String, String, Option<i32>, Option<String>, String)>(
            "SELECT id, title, status, priority, assigned_agent, created_at::text
             FROM task_queue ORDER BY created_at DESC LIMIT $1",
        )
        .bind(limit)
        .fetch_all(&state.pg)
        .await?
    };

    let tasks: Vec<serde_json::Value> = rows
        .into_iter()
        .map(
            |(id, title, status, priority, assigned_agent, created_at)| {
                serde_json::json!({
                    "id": id,
                    "title": title,
                    "status": status,
                    "priority": priority,
                    "assigned_agent": assigned_agent,
                    "created_at": created_at,
                })
            },
        )
        .collect();

    Ok(serde_json::json!({ "tasks": tasks }))
}

async fn tool_create_task(
    state: &AppState,
    _agent_id: &str,
    params: &serde_json::Value,
) -> Result<serde_json::Value, BroodlinkError> {
    let title = param_str(params, "title")?;
    let description = param_str_opt(params, "description").unwrap_or("");
    let priority = param_i64_opt(params, "priority");
    let role = param_str_opt(params, "role");
    let cost_tier = param_str_opt(params, "cost_tier");
    let formula_name = param_str_opt(params, "formula_name");
    let convoy_id = param_str_opt(params, "convoy_id");
    let workflow_run_id = param_str_opt(params, "workflow_run_id");
    let step_index = param_i64_opt(params, "step_index");
    let step_name = param_str_opt(params, "step_name");
    let id = Uuid::new_v4().to_string();
    let trace_id = Uuid::new_v4().to_string();

    sqlx::query(
        "INSERT INTO task_queue (id, trace_id, title, description, status, priority, formula_name, convoy_id, workflow_run_id, step_index, step_name, created_at, updated_at)
         VALUES ($1, $2, $3, $4, 'pending', $5, $6, $7, $8, $9, $10, NOW(), NOW())",
    )
    .bind(&id)
    .bind(&trace_id)
    .bind(title)
    .bind(description)
    .bind(priority.map(|p| p as i32))
    .bind(formula_name)
    .bind(convoy_id)
    .bind(workflow_run_id)
    .bind(step_index.map(|i| i as i32))
    .bind(step_name)
    .execute(&state.pg)
    .await?;

    // Notify coordinator via NATS
    let subject = format!(
        "{}.{}.coordinator.task_available",
        state.config.nats.subject_prefix, state.config.broodlink.env,
    );
    let nats_payload = serde_json::json!({
        "task_id": id,
        "role": role,
        "cost_tier": cost_tier,
    });
    if let Ok(bytes) = serde_json::to_vec(&nats_payload) {
        if let Err(e) = state.nats.publish(subject, bytes.into()).await {
            warn!(error = %e, "failed to publish task available event");
        }
    }

    Ok(serde_json::json!({ "id": id, "title": title, "created": true }))
}

async fn tool_get_daily_summary(
    state: &AppState,
    params: &serde_json::Value,
) -> Result<serde_json::Value, BroodlinkError> {
    let date = param_str_opt(params, "date");
    let limit = param_i64_opt(params, "limit").unwrap_or(7);

    let rows = if let Some(d) = date {
        sqlx::query_as::<_, (i64, String, Option<String>, Option<i32>, Option<i32>, Option<i32>, String)>(
            "SELECT id, CAST(summary_date AS CHAR), summary_text, tasks_completed, decisions_made, memories_stored, CAST(created_at AS CHAR)
             FROM daily_summary WHERE summary_date = ? ORDER BY created_at DESC LIMIT ?",
        )
        .bind(d)
        .bind(limit)
        .fetch_all(&state.dolt)
        .await?
    } else {
        sqlx::query_as::<_, (i64, String, Option<String>, Option<i32>, Option<i32>, Option<i32>, String)>(
            "SELECT id, CAST(summary_date AS CHAR), summary_text, tasks_completed, decisions_made, memories_stored, CAST(created_at AS CHAR)
             FROM daily_summary ORDER BY summary_date DESC LIMIT ?",
        )
        .bind(limit)
        .fetch_all(&state.dolt)
        .await?
    };

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

    Ok(serde_json::json!({ "summaries": summaries }))
}

async fn tool_get_commits(
    state: &AppState,
    params: &serde_json::Value,
) -> Result<serde_json::Value, BroodlinkError> {
    let limit = param_i64_opt(params, "limit").unwrap_or(20);

    let rows = sqlx::query_as::<_, (String, String, String, String)>(
        "SELECT commit_hash, committer, message, CAST(date AS CHAR)
         FROM dolt_log ORDER BY date DESC LIMIT ?",
    )
    .bind(limit)
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

    Ok(serde_json::json!({ "commits": commits }))
}

async fn tool_get_memory_stats(
    state: &AppState,
) -> Result<serde_json::Value, BroodlinkError> {
    let memory_count = sqlx::query_as::<_, (i64,)>(
        "SELECT COUNT(*) FROM agent_memory",
    )
    .fetch_one(&state.dolt)
    .await?
    .0;

    let decision_count = sqlx::query_as::<_, (i64,)>(
        "SELECT COUNT(*) FROM decisions",
    )
    .fetch_one(&state.dolt)
    .await?
    .0;

    let agent_count = sqlx::query_as::<_, (i64,)>(
        "SELECT COUNT(*) FROM agent_profiles",
    )
    .fetch_one(&state.dolt)
    .await?
    .0;

    let outbox_pending = sqlx::query_as::<_, (i64,)>(
        "SELECT COUNT(*) FROM outbox WHERE status = 'pending'",
    )
    .fetch_one(&state.pg)
    .await?
    .0;

    Ok(serde_json::json!({
        "memory_entries": memory_count,
        "decisions": decision_count,
        "agent_profiles": agent_count,
        "outbox_pending": outbox_pending,
    }))
}

async fn tool_get_convoy_status(
    state: &AppState,
    params: &serde_json::Value,
) -> Result<serde_json::Value, BroodlinkError> {
    let convoy_id = param_str_opt(params, "convoy_id");
    let bd_binary = &state.config.beads.bd_binary;
    let workspace = &state.config.beads.workspace;

    let mut cmd = tokio::process::Command::new(bd_binary);
    cmd.arg("convoy").arg("list").current_dir(workspace);

    if let Some(cid) = convoy_id {
        cmd.arg("--id").arg(cid);
    }

    let output = cmd
        .output()
        .await
        .map_err(|e| BroodlinkError::Internal(format!("failed to get convoy status: {e}")))?;

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let exit_code = output.status.code().unwrap_or(-1);

    Ok(serde_json::json!({
        "convoy_id": convoy_id,
        "output": stdout,
        "exit_code": exit_code,
    }))
}

// ---------------------------------------------------------------------------
// Guardrail middleware
// ---------------------------------------------------------------------------

fn is_readonly_tool(tool: &str) -> bool {
    matches!(
        tool,
        "recall_memory" | "semantic_search" | "list_agents" | "get_agent"
            | "get_task" | "list_tasks" | "get_audit_log" | "health_check"
            | "get_config_info" | "get_daily_summary" | "get_commits"
            | "get_memory_stats" | "list_guardrails" | "get_guardrail_violations"
            | "list_approvals" | "get_approval" | "list_approval_policies"
            | "get_routing_scores" | "get_delegation" | "list_delegations"
            | "get_work_log" | "get_decisions" | "get_convoy_status"
            | "beads_list_issues" | "beads_get_issue" | "beads_list_formulas"
            | "beads_get_convoy" | "read_messages" | "list_projects"
            | "list_skills" | "ping"
    )
}

async fn check_guardrails(
    state: &AppState,
    agent_id: &str,
    tool_name: &str,
    params: &serde_json::Value,
) -> Result<(), BroodlinkError> {
    // Fetch all enabled policies from Postgres
    let policies: Vec<(String, String, serde_json::Value)> = sqlx::query_as(
        "SELECT name, rule_type, config FROM guardrail_policies WHERE enabled = true",
    )
    .fetch_all(&state.pg)
    .await
    .unwrap_or_default();

    for (policy_name, rule_type, config) in &policies {
        match rule_type.as_str() {
            "tool_block" => {
                // config: {"blocked_tools": ["tool1"], "agents": ["agent1"] or "*"}
                let blocked_tools = config["blocked_tools"]
                    .as_array()
                    .map(|a| a.iter().filter_map(|v| v.as_str()).collect::<Vec<_>>())
                    .unwrap_or_default();
                let agents = config["agents"]
                    .as_array()
                    .map(|a| a.iter().filter_map(|v| v.as_str()).collect::<Vec<_>>())
                    .unwrap_or_default();
                let applies_to_agent = agents.iter().any(|a| *a == "*" || *a == agent_id);

                if applies_to_agent && blocked_tools.iter().any(|t| *t == tool_name) {
                    log_guardrail_violation(state, agent_id, policy_name, tool_name, "tool blocked by policy").await;
                    return Err(BroodlinkError::Guardrail {
                        policy: policy_name.clone(),
                        message: format!("tool '{tool_name}' is blocked for agent '{agent_id}'"),
                    });
                }
            }
            "scope_limit" => {
                // config: {"agents": {"agent_id": ["allowed_tool1", ...]}}
                if let Some(allowed) = config["agents"].get(agent_id) {
                    let allowed_tools = allowed
                        .as_array()
                        .map(|a| a.iter().filter_map(|v| v.as_str()).collect::<Vec<_>>())
                        .unwrap_or_default();
                    if !allowed_tools.is_empty() && !allowed_tools.iter().any(|t| *t == tool_name) {
                        log_guardrail_violation(state, agent_id, policy_name, tool_name, "tool not in scope").await;
                        return Err(BroodlinkError::Guardrail {
                            policy: policy_name.clone(),
                            message: format!("tool '{tool_name}' not in allowed scope for agent '{agent_id}'"),
                        });
                    }
                }
            }
            "content_filter" => {
                // config: {"patterns": ["regex1"], "tools": ["tool1"], "agents": ["*"]}
                let patterns = config["patterns"]
                    .as_array()
                    .map(|a| a.iter().filter_map(|v| v.as_str()).collect::<Vec<_>>())
                    .unwrap_or_default();
                let tools = config["tools"]
                    .as_array()
                    .map(|a| a.iter().filter_map(|v| v.as_str()).collect::<Vec<_>>())
                    .unwrap_or_default();
                let agents = config["agents"]
                    .as_array()
                    .map(|a| a.iter().filter_map(|v| v.as_str()).collect::<Vec<_>>())
                    .unwrap_or_default();

                let applies_to_agent =
                    agents.is_empty() || agents.iter().any(|a| *a == "*" || *a == agent_id);
                let applies_to_tool =
                    tools.is_empty() || tools.iter().any(|t| *t == "*" || *t == tool_name);

                if applies_to_agent && applies_to_tool {
                    let params_str = params.to_string();
                    for pat in &patterns {
                        if let Ok(re) = regex::Regex::new(pat) {
                            if re.is_match(&params_str) {
                                log_guardrail_violation(
                                    state, agent_id, policy_name, tool_name,
                                    &format!("content matched filter pattern: {pat}"),
                                ).await;
                                return Err(BroodlinkError::Guardrail {
                                    policy: policy_name.clone(),
                                    message: format!(
                                        "content blocked by filter policy '{policy_name}'"
                                    ),
                                });
                            }
                        }
                    }
                }
            }
            "rate_override" => {
                // config: {"agents": {"claude": 30, "qwen3": 120}}
                if let Some(rpm) = config["agents"].get(agent_id).and_then(|v| v.as_f64()) {
                    let mut buckets = state.rate_override_buckets.write().await;
                    let refill_rate = rpm / 60.0;
                    let max_tokens = (rpm / 10.0).max(1.0); // burst = rpm/10, min 1
                    let bucket = buckets
                        .entry(format!("{policy_name}:{agent_id}"))
                        .or_insert(TokenBucket {
                            tokens: max_tokens,
                            last_refill: std::time::Instant::now(),
                            max_tokens,
                            refill_rate,
                        });

                    let now = std::time::Instant::now();
                    let elapsed = now.duration_since(bucket.last_refill).as_secs_f64();
                    bucket.tokens = (bucket.tokens + elapsed * bucket.refill_rate).min(bucket.max_tokens);
                    bucket.last_refill = now;

                    if bucket.tokens >= 1.0 {
                        bucket.tokens -= 1.0;
                    } else {
                        log_guardrail_violation(
                            state, agent_id, policy_name, tool_name,
                            &format!("rate override exceeded: {rpm} rpm"),
                        ).await;
                        return Err(BroodlinkError::RateLimited(agent_id.to_string()));
                    }
                }
            }
            _ => {}
        }
    }

    // --- Approval gate enforcement ---
    // Skip read-only tools that never need approval
    if !is_readonly_tool(tool_name) {
        let approval_policies: Vec<(String, String, serde_json::Value)> = sqlx::query_as(
            "SELECT id, name, conditions FROM approval_policies
             WHERE gate_type = 'pre_dispatch' AND active = true",
        )
        .fetch_all(&state.pg)
        .await
        .unwrap_or_default();

        for (_policy_id, policy_name, conditions) in &approval_policies {
            // Check if this policy applies to this agent+tool (ignore severity —
            // severity is a property of the gate request, not the tool call)
            if !policy_matches_agent_tool(conditions, agent_id, tool_name) {
                continue;
            }
            // Check if an approved gate already exists for this agent+tool
            let approved: Option<(String,)> = sqlx::query_as(
                "SELECT id FROM approval_gates
                 WHERE agent_id = $1 AND tool_name = $2
                   AND status IN ('approved', 'auto_approved')
                   AND created_at > NOW() - INTERVAL '1 hour'
                 LIMIT 1",
            )
            .bind(agent_id)
            .bind(tool_name)
            .fetch_optional(&state.pg)
            .await
            .unwrap_or(None);

            if approved.is_none() {
                log_guardrail_violation(
                    state, agent_id, policy_name, tool_name,
                    "tool requires approval gate",
                ).await;
                return Err(BroodlinkError::Guardrail {
                    policy: policy_name.clone(),
                    message: format!(
                        "tool '{}' requires approval for agent '{}'. Call create_approval_gate first.",
                        tool_name, agent_id
                    ),
                });
            }
        }
    }

    Ok(())
}

async fn log_guardrail_violation(
    state: &AppState,
    agent_id: &str,
    policy_name: &str,
    tool_name: &str,
    details: &str,
) {
    let trace_id = Uuid::new_v4().to_string();
    let _ = sqlx::query(
        "INSERT INTO guardrail_violations (trace_id, agent_id, policy_name, tool_name, details, created_at)
         VALUES ($1, $2, $3, $4, $5, NOW())",
    )
    .bind(&trace_id)
    .bind(agent_id)
    .bind(policy_name)
    .bind(tool_name)
    .bind(details)
    .execute(&state.pg)
    .await;
}

// ---------------------------------------------------------------------------
// Agent metrics helper
// ---------------------------------------------------------------------------

async fn update_agent_metrics(pg: &PgPool, agent_id: &str, success: bool) {
    let query = if success {
        "INSERT INTO agent_metrics (agent_id, tasks_completed, tasks_failed, current_load, success_rate, last_task_at)
         VALUES ($1, 1, 0, 0, 1.0, NOW())
         ON CONFLICT (agent_id) DO UPDATE SET
           tasks_completed = agent_metrics.tasks_completed + 1,
           current_load = GREATEST(agent_metrics.current_load - 1, 0),
           success_rate = (agent_metrics.tasks_completed + 1)::FLOAT
             / GREATEST((agent_metrics.tasks_completed + agent_metrics.tasks_failed + 1)::FLOAT, 1.0),
           last_task_at = NOW()"
    } else {
        "INSERT INTO agent_metrics (agent_id, tasks_completed, tasks_failed, current_load, success_rate, last_task_at)
         VALUES ($1, 0, 1, 0, 0.0, NOW())
         ON CONFLICT (agent_id) DO UPDATE SET
           tasks_failed = agent_metrics.tasks_failed + 1,
           current_load = GREATEST(agent_metrics.current_load - 1, 0),
           success_rate = agent_metrics.tasks_completed::FLOAT
             / GREATEST((agent_metrics.tasks_completed + agent_metrics.tasks_failed + 1)::FLOAT, 1.0),
           last_task_at = NOW()"
    };
    if let Err(e) = sqlx::query(query).bind(agent_id).execute(pg).await {
        warn!(error = %e, agent = %agent_id, "failed to update agent_metrics");
    }
}

// ---------------------------------------------------------------------------
// Guardrail tools
// ---------------------------------------------------------------------------

async fn tool_set_guardrail(
    state: &AppState,
    agent_id: &str,
    params: &serde_json::Value,
) -> Result<serde_json::Value, BroodlinkError> {
    let name = param_str(params, "name")?;
    let rule_type = param_str(params, "rule_type")?;
    let config_json = param_json(params, "config")?;

    // Validate rule_type
    if !["tool_block", "rate_override", "content_filter", "scope_limit"].contains(&rule_type) {
        return Err(BroodlinkError::Validation {
            field: "rule_type".to_string(),
            message: "must be one of: tool_block, rate_override, content_filter, scope_limit".to_string(),
        });
    }

    let enabled = params.get("enabled").and_then(|v| v.as_bool()).unwrap_or(true);

    sqlx::query(
        "INSERT INTO guardrail_policies (name, rule_type, config, enabled, created_at, updated_at)
         VALUES ($1, $2, $3, $4, NOW(), NOW())
         ON CONFLICT (name) DO UPDATE SET
           rule_type = EXCLUDED.rule_type,
           config = EXCLUDED.config,
           enabled = EXCLUDED.enabled,
           updated_at = NOW()",
    )
    .bind(name)
    .bind(rule_type)
    .bind(&config_json)
    .bind(enabled)
    .execute(&state.pg)
    .await?;

    write_audit_log(&state.pg, &Uuid::new_v4().to_string(), agent_id, SERVICE_NAME, "set_guardrail", true, None).await.ok();

    Ok(serde_json::json!({ "upserted": true, "name": name }))
}

async fn tool_list_guardrails(
    state: &AppState,
    params: &serde_json::Value,
) -> Result<serde_json::Value, BroodlinkError> {
    let enabled_only = params.get("enabled_only").and_then(|v| v.as_bool()).unwrap_or(false);

    let policies: Vec<(i64, String, String, serde_json::Value, bool, String, String)> = if enabled_only {
        sqlx::query_as(
            "SELECT id, name, rule_type, config, enabled,
                    created_at::text, updated_at::text
             FROM guardrail_policies WHERE enabled = true ORDER BY name",
        )
        .fetch_all(&state.pg)
        .await?
    } else {
        sqlx::query_as(
            "SELECT id, name, rule_type, config, enabled,
                    created_at::text, updated_at::text
             FROM guardrail_policies ORDER BY name",
        )
        .fetch_all(&state.pg)
        .await?
    };

    let list: Vec<serde_json::Value> = policies
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

    Ok(serde_json::json!({ "policies": list }))
}

async fn tool_get_guardrail_violations(
    state: &AppState,
    params: &serde_json::Value,
) -> Result<serde_json::Value, BroodlinkError> {
    let limit = param_i64_opt(params, "limit").unwrap_or(50);
    let agent_filter = param_str_opt(params, "agent_id");

    let violations: Vec<(i64, String, String, String, Option<String>, Option<String>, String)> = if let Some(aid) = agent_filter {
        sqlx::query_as(
            "SELECT id, trace_id, agent_id, policy_name, tool_name, details, created_at::text
             FROM guardrail_violations
             WHERE agent_id = $1
             ORDER BY created_at DESC LIMIT $2",
        )
        .bind(aid)
        .bind(limit)
        .fetch_all(&state.pg)
        .await?
    } else {
        sqlx::query_as(
            "SELECT id, trace_id, agent_id, policy_name, tool_name, details, created_at::text
             FROM guardrail_violations
             ORDER BY created_at DESC LIMIT $1",
        )
        .bind(limit)
        .fetch_all(&state.pg)
        .await?
    };

    let list: Vec<serde_json::Value> = violations
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

    Ok(serde_json::json!({ "violations": list }))
}

// ---------------------------------------------------------------------------
// Approval gate tools
// ---------------------------------------------------------------------------

async fn tool_set_approval_policy(
    state: &AppState,
    agent_id: &str,
    params: &serde_json::Value,
) -> Result<serde_json::Value, BroodlinkError> {
    let name = param_str(params, "name")?;
    let gate_type = param_str(params, "gate_type")?;

    // Validate gate_type
    if !["pre_dispatch", "pre_completion", "budget", "custom"].contains(&gate_type) {
        return Err(BroodlinkError::Validation {
            field: "gate_type".to_string(),
            message: "must be one of: pre_dispatch, pre_completion, budget, custom".to_string(),
        });
    }

    let conditions = param_json(params, "conditions")?;
    let description = param_str_opt(params, "description");
    let auto_approve = params.get("auto_approve").and_then(|v| v.as_bool()).unwrap_or(false);
    let threshold = params
        .get("auto_approve_threshold")
        .and_then(|v| v.as_f64().or_else(|| v.as_str().and_then(|s| s.parse().ok())))
        .unwrap_or(0.8);
    let expiry = params
        .get("expiry_minutes")
        .and_then(|v| v.as_i64().or_else(|| v.as_str().and_then(|s| s.parse().ok())))
        .unwrap_or(60) as i32;
    let active = params.get("active").and_then(|v| v.as_bool()).unwrap_or(true);

    let id = Uuid::new_v4().to_string();

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
    .bind(name)
    .bind(description)
    .bind(gate_type)
    .bind(&conditions)
    .bind(auto_approve)
    .bind(threshold)
    .bind(expiry)
    .bind(active)
    .execute(&state.pg)
    .await?;

    write_audit_log(&state.pg, &Uuid::new_v4().to_string(), agent_id, SERVICE_NAME, "set_approval_policy", true, None).await.ok();

    Ok(serde_json::json!({ "upserted": true, "name": name }))
}

async fn tool_list_approval_policies(
    state: &AppState,
    params: &serde_json::Value,
) -> Result<serde_json::Value, BroodlinkError> {
    let active_only = params.get("active_only").and_then(|v| v.as_bool()).unwrap_or(false);

    let policies: Vec<(String, String, Option<String>, String, serde_json::Value, bool, f64, i32, bool, String, String)> = if active_only {
        sqlx::query_as(
            "SELECT id, name, description, gate_type, conditions, auto_approve,
                    auto_approve_threshold, expiry_minutes, active,
                    created_at::text, updated_at::text
             FROM approval_policies WHERE active = true ORDER BY name",
        )
        .fetch_all(&state.pg)
        .await?
    } else {
        sqlx::query_as(
            "SELECT id, name, description, gate_type, conditions, auto_approve,
                    auto_approve_threshold, expiry_minutes, active,
                    created_at::text, updated_at::text
             FROM approval_policies ORDER BY name",
        )
        .fetch_all(&state.pg)
        .await?
    };

    let list: Vec<serde_json::Value> = policies
        .into_iter()
        .map(|(id, name, desc, gate_type, conditions, auto_approve, threshold, expiry, active, created, updated)| {
            serde_json::json!({
                "id": id,
                "name": name,
                "description": desc,
                "gate_type": gate_type,
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

    Ok(serde_json::json!({ "policies": list }))
}

// ---------------------------------------------------------------------------
// Approval policy matching
// ---------------------------------------------------------------------------

/// Check if an approval policy should be enforced for a specific agent+tool call.
/// Only enforces when the policy explicitly lists tool_names in its conditions
/// (policies without tool_names are default approval behaviour, not enforcement gates).
/// Ignores severity (severity is a gate-creation concept, not a tool-call concept).
fn policy_matches_agent_tool(
    conditions: &serde_json::Value,
    agent_id: &str,
    tool_name: &str,
) -> bool {
    // Policy must explicitly list tool_names to trigger enforcement.
    // Policies without tool_names are just auto-approve defaults, not enforcement gates.
    match conditions.get("tool_names").and_then(|v| v.as_array()) {
        Some(arr) => {
            let strs: Vec<&str> = arr.iter().filter_map(|s| s.as_str()).collect();
            if strs.is_empty() || (!strs.contains(&tool_name) && !strs.contains(&"*")) {
                return false;
            }
        }
        None => return false,
    }
    // Check agent_ids match (["*"] means all, absent means all)
    if let Some(arr) = conditions.get("agent_ids").and_then(|v| v.as_array()) {
        let strs: Vec<&str> = arr.iter().filter_map(|s| s.as_str()).collect();
        if !strs.is_empty() && !strs.contains(&"*") && !strs.contains(&agent_id) {
            return false;
        }
    }
    true
}

fn policy_matches(
    conditions: &serde_json::Value,
    severity: &str,
    agent_id: &str,
    tool_name: Option<&str>,
) -> bool {
    // Check severity match
    if let Some(arr) = conditions.get("severity").and_then(|v| v.as_array()) {
        let strs: Vec<&str> = arr.iter().filter_map(|s| s.as_str()).collect();
        if !strs.is_empty() && !strs.contains(&severity) {
            return false;
        }
    }
    // Check agent_ids match (["*"] means all)
    if let Some(arr) = conditions.get("agent_ids").and_then(|v| v.as_array()) {
        let strs: Vec<&str> = arr.iter().filter_map(|s| s.as_str()).collect();
        if !strs.is_empty() && !strs.contains(&"*") && !strs.contains(&agent_id) {
            return false;
        }
    }
    // Check tool_names match
    if let Some(arr) = conditions.get("tool_names").and_then(|v| v.as_array()) {
        let strs: Vec<&str> = arr.iter().filter_map(|s| s.as_str()).collect();
        if !strs.is_empty() {
            match tool_name {
                Some(tn) if strs.contains(&tn) || strs.contains(&"*") => {}
                _ => return false,
            }
        }
    }
    true
}

async fn evaluate_approval_policy(
    pg: &sqlx::PgPool,
    gate_type: &str,
    severity: &str,
    agent_id: &str,
    tool_name: Option<&str>,
    confidence: Option<f64>,
) -> Result<Option<(String, bool, i32)>, BroodlinkError> {
    // Returns: Option<(policy_id, should_auto_approve, expiry_minutes)>
    let policies: Vec<(String, serde_json::Value, bool, f64, i32)> = sqlx::query_as(
        "SELECT id, conditions, auto_approve, auto_approve_threshold, expiry_minutes
         FROM approval_policies WHERE gate_type = $1 AND active = true ORDER BY name",
    )
    .bind(gate_type)
    .fetch_all(pg)
    .await?;

    for (policy_id, conditions, auto_approve, threshold, expiry) in &policies {
        if !policy_matches(conditions, severity, agent_id, tool_name) {
            continue;
        }
        let should_auto = *auto_approve
            && match confidence {
                Some(c) => c >= *threshold,
                None => *threshold == 0.0, // no confidence only passes if threshold is 0
            };
        return Ok(Some((policy_id.clone(), should_auto, *expiry)));
    }
    Ok(None)
}

async fn tool_create_approval_gate(
    state: &AppState,
    agent_id: &str,
    params: &serde_json::Value,
) -> Result<serde_json::Value, BroodlinkError> {
    let gate_type = param_str(params, "gate_type")?;
    let payload = param_json(params, "payload")?;
    let severity = param_str_opt(params, "severity").unwrap_or("medium");
    let task_id = param_str_opt(params, "task_id");
    let tool_name_val = param_str_opt(params, "tool_name");
    let confidence_val = params
        .get("confidence")
        .and_then(|v| v.as_f64().or_else(|| v.as_str().and_then(|s| s.parse().ok())));
    let caller_expiry = param_i64_opt(params, "expires_minutes").unwrap_or(60);

    if !["pre_dispatch", "pre_completion", "budget", "custom"].contains(&gate_type) {
        return Err(BroodlinkError::Validation {
            field: "gate_type".to_string(),
            message: "must be one of: pre_dispatch, pre_completion, budget, custom".to_string(),
        });
    }

    if !["low", "medium", "high", "critical"].contains(&severity) {
        return Err(BroodlinkError::Validation {
            field: "severity".to_string(),
            message: "must be one of: low, medium, high, critical".to_string(),
        });
    }

    // Evaluate policies
    let policy_result = evaluate_approval_policy(
        &state.pg, gate_type, severity, agent_id, tool_name_val, confidence_val,
    )
    .await?;

    let (status, policy_id, effective_expiry) = match &policy_result {
        Some((pid, true, expiry)) => ("auto_approved", Some(pid.as_str()), *expiry as i64),
        Some((pid, false, expiry)) => ("pending", Some(pid.as_str()), *expiry as i64),
        None => ("pending", None::<&str>, caller_expiry),
    };

    // Merge severity into payload
    let mut enriched_payload = payload.clone();
    if let Some(obj) = enriched_payload.as_object_mut() {
        obj.insert("severity".to_string(), serde_json::Value::String(severity.to_string()));
    }

    let gate_id = Uuid::new_v4().to_string();
    let trace_id = Uuid::new_v4().to_string();

    sqlx::query(
        "INSERT INTO approval_gates
         (id, trace_id, gate_type, agent_id, requested_by, payload, task_id,
          tool_name, confidence, policy_id, status, expires_at, created_at)
         VALUES ($1, $2, $3, $4, $4, $5, $6, $7, $8, $9, $10,
                 NOW() + ($11 || ' minutes')::INTERVAL, NOW())",
    )
    .bind(&gate_id)
    .bind(&trace_id)
    .bind(gate_type)
    .bind(agent_id)
    .bind(&enriched_payload)
    .bind(task_id)
    .bind(tool_name_val)
    .bind(confidence_val)
    .bind(policy_id)
    .bind(status)
    .bind(format!("{effective_expiry}"))
    .execute(&state.pg)
    .await?;

    if status == "auto_approved" {
        // Auto-approved: immediately re-queue linked task
        if let Some(tid) = task_id {
            let _ = sqlx::query(
                "UPDATE task_queue SET status = 'pending', updated_at = NOW() WHERE id = $1",
            )
            .bind(tid)
            .execute(&state.pg)
            .await;
        }

        let subject = format!(
            "{}.{}.approvals.auto_approved",
            state.config.nats.subject_prefix, state.config.broodlink.env,
        );
        let nats_payload = serde_json::json!({
            "approval_id": gate_id, "gate_type": gate_type, "severity": severity,
            "policy_id": policy_id, "agent_id": agent_id,
        });
        if let Ok(bytes) = serde_json::to_vec(&nats_payload) {
            let _ = state.nats.publish(subject, bytes.into()).await;
        }
    } else {
        // Pending: mark task as awaiting approval
        if let Some(tid) = task_id {
            let _ = sqlx::query(
                "UPDATE task_queue SET requires_approval = true, approval_id = $1, status = 'awaiting_approval', updated_at = NOW()
                 WHERE id = $2",
            )
            .bind(&gate_id)
            .bind(tid)
            .execute(&state.pg)
            .await;
        }

        let subject = format!(
            "{}.{}.approvals.pending",
            state.config.nats.subject_prefix, state.config.broodlink.env,
        );
        let nats_payload = serde_json::json!({
            "approval_id": gate_id, "gate_type": gate_type, "severity": severity,
            "requested_by": agent_id, "task_id": task_id,
        });
        if let Ok(bytes) = serde_json::to_vec(&nats_payload) {
            let _ = state.nats.publish(subject, bytes.into()).await;
        }
    }

    Ok(serde_json::json!({
        "approval_id": gate_id,
        "status": status,
        "policy_id": policy_id,
        "severity": severity,
    }))
}

async fn tool_resolve_approval(
    state: &AppState,
    agent_id: &str,
    params: &serde_json::Value,
) -> Result<serde_json::Value, BroodlinkError> {
    let approval_id = param_str(params, "approval_id")?;
    let decision = param_str(params, "decision")?;
    let reason = param_str_opt(params, "reason");

    if !["approved", "rejected"].contains(&decision) {
        return Err(BroodlinkError::Validation {
            field: "decision".to_string(),
            message: "must be 'approved' or 'rejected'".to_string(),
        });
    }

    let affected = sqlx::query(
        "UPDATE approval_gates SET status = $1, reviewed_by = $2, reason = $3, reviewed_at = NOW()
         WHERE id = $4 AND status = 'pending'",
    )
    .bind(decision)
    .bind(agent_id)
    .bind(reason)
    .bind(approval_id)
    .execute(&state.pg)
    .await?;

    if affected.rows_affected() == 0 {
        return Err(BroodlinkError::NotFound(format!(
            "approval gate {approval_id} not found or not pending"
        )));
    }

    // If approved and linked to a task, update task status back
    if decision == "approved" {
        let _ = sqlx::query(
            "UPDATE task_queue SET status = 'pending', updated_at = NOW() WHERE approval_id = $1 AND status = 'awaiting_approval'",
        )
        .bind(approval_id)
        .execute(&state.pg)
        .await;

        // Re-publish task_available so coordinator picks it up
        let subject = format!(
            "{}.{}.tasks.task_available",
            state.config.nats.subject_prefix, state.config.broodlink.env,
        );
        let _ = state.nats.publish(subject, b"re-queued after approval".to_vec().into()).await;
    }

    // Publish resolution event
    let subject = format!(
        "{}.{}.approvals.resolved",
        state.config.nats.subject_prefix, state.config.broodlink.env,
    );
    let nats_payload = serde_json::json!({
        "approval_id": approval_id,
        "decision": decision,
        "resolved_by": agent_id,
    });
    if let Ok(bytes) = serde_json::to_vec(&nats_payload) {
        let _ = state.nats.publish(subject, bytes.into()).await;
    }

    Ok(serde_json::json!({ "resolved": true, "approval_id": approval_id, "decision": decision }))
}

async fn tool_list_approvals(
    state: &AppState,
    params: &serde_json::Value,
) -> Result<serde_json::Value, BroodlinkError> {
    let limit = param_i64_opt(params, "limit").unwrap_or(50);
    let status_filter = param_str_opt(params, "status");

    let approvals: Vec<(String, String, String, serde_json::Value, Option<String>, String, Option<String>, Option<String>, Option<String>, Option<String>, Option<String>)> =
        if let Some(status) = status_filter {
            sqlx::query_as(
                "SELECT id, gate_type, requested_by, payload, task_id, status, reviewed_by, reason,
                        expires_at::text, created_at::text, reviewed_at::text
                 FROM approval_gates WHERE status = $1
                 ORDER BY created_at DESC LIMIT $2",
            )
            .bind(status)
            .bind(limit)
            .fetch_all(&state.pg)
            .await?
        } else {
            sqlx::query_as(
                "SELECT id, gate_type, requested_by, payload, task_id, status, reviewed_by, reason,
                        expires_at::text, created_at::text, reviewed_at::text
                 FROM approval_gates ORDER BY created_at DESC LIMIT $1",
            )
            .bind(limit)
            .fetch_all(&state.pg)
            .await?
        };

    let list: Vec<serde_json::Value> = approvals
        .into_iter()
        .map(|(id, gate_type, requested_by, payload, task_id, status, reviewed_by, reason, expires, created, reviewed_at)| {
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
            })
        })
        .collect();

    Ok(serde_json::json!({ "approvals": list }))
}

async fn tool_get_approval(
    state: &AppState,
    params: &serde_json::Value,
) -> Result<serde_json::Value, BroodlinkError> {
    let approval_id = param_str(params, "approval_id")?;

    let row: Option<(String, String, String, serde_json::Value, Option<String>, String, Option<String>, Option<String>, Option<String>, Option<String>, Option<String>)> =
        sqlx::query_as(
            "SELECT id, gate_type, requested_by, payload, task_id, status, reviewed_by, reason,
                    expires_at::text, created_at::text, reviewed_at::text
             FROM approval_gates WHERE id = $1",
        )
        .bind(approval_id)
        .fetch_optional(&state.pg)
        .await?;

    match row {
        Some((id, gate_type, requested_by, payload, task_id, status, reviewed_by, reason, expires, created, reviewed_at)) => {
            Ok(serde_json::json!({
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
            }))
        }
        None => Err(BroodlinkError::NotFound(format!("approval gate {approval_id}"))),
    }
}

// ---------------------------------------------------------------------------
// Routing tools
// ---------------------------------------------------------------------------

async fn tool_get_routing_scores(
    state: &AppState,
    params: &serde_json::Value,
) -> Result<serde_json::Value, BroodlinkError> {
    let role_filter = param_str_opt(params, "role");

    // Fetch eligible agents from Dolt
    let agents: Vec<(String, String, String, Option<String>)> = if let Some(role) = role_filter {
        sqlx::query_as(
            "SELECT agent_id, role, cost_tier, CAST(last_seen AS CHAR) FROM agent_profiles WHERE active = true AND role = ?",
        )
        .bind(role)
        .fetch_all(&state.dolt)
        .await?
    } else {
        sqlx::query_as(
            "SELECT agent_id, role, cost_tier, CAST(last_seen AS CHAR) FROM agent_profiles WHERE active = true",
        )
        .fetch_all(&state.dolt)
        .await?
    };

    // Fetch metrics from Postgres
    let metrics: Vec<(String, i64, i64, i32, f64)> = sqlx::query_as(
        "SELECT agent_id, tasks_completed, tasks_failed, current_load, success_rate FROM agent_metrics",
    )
    .fetch_all(&state.pg)
    .await
    .unwrap_or_default();

    let metrics_map: HashMap<String, (i64, i64, i32, f64)> = metrics
        .into_iter()
        .map(|(aid, completed, failed, load, rate)| (aid, (completed, failed, load, rate)))
        .collect();

    let weights = &state.config.routing.weights;
    let max_concurrent = state.config.routing.max_concurrent_default;

    let mut scored_agents: Vec<serde_json::Value> = agents
        .into_iter()
        .map(|(agent_id, role, cost_tier, _last_seen)| {
            let (completed, _failed, current_load, success_rate) =
                metrics_map.get(&agent_id).copied().unwrap_or((0, 0, 0, 1.0));

            let cost_score = match cost_tier.as_str() {
                "low" => 1.0,
                "medium" => 0.6,
                "high" => 0.3,
                _ => 0.5,
            };

            let availability = 1.0 - (current_load as f64 / max_concurrent as f64).min(1.0);
            let maturity = (completed as f64 / 100.0).min(1.0); // proxy for capability

            let score = cost_score * f64::from(weights.cost)
                + success_rate * f64::from(weights.success_rate)
                + availability * f64::from(weights.availability)
                + maturity * f64::from(weights.capability)
                + availability * f64::from(weights.recency); // use availability as recency proxy

            serde_json::json!({
                "agent_id": agent_id,
                "role": role,
                "score": (score * 1000.0).round() / 1000.0,
                "breakdown": {
                    "cost": cost_score,
                    "success_rate": success_rate,
                    "availability": availability,
                    "capability": maturity,
                    "cost_tier": cost_tier,
                    "current_load": current_load,
                    "tasks_completed": completed,
                }
            })
        })
        .collect();

    scored_agents.sort_by(|a, b| {
        b["score"]
            .as_f64()
            .unwrap_or(0.0)
            .partial_cmp(&a["score"].as_f64().unwrap_or(0.0))
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    Ok(serde_json::json!({ "agents": scored_agents }))
}

// ---------------------------------------------------------------------------
// Delegation tools
// ---------------------------------------------------------------------------

async fn tool_delegate_task(
    state: &AppState,
    agent_id: &str,
    params: &serde_json::Value,
) -> Result<serde_json::Value, BroodlinkError> {
    let to_agent = param_str(params, "to_agent")?;
    let title = param_str(params, "title")?;
    let description = param_str_opt(params, "description");
    let parent_task_id = param_str_opt(params, "parent_task_id");

    let delegation_id = Uuid::new_v4().to_string();
    let trace_id = Uuid::new_v4().to_string();

    sqlx::query(
        "INSERT INTO delegations (id, trace_id, parent_task_id, from_agent, to_agent, title, description, status, created_at, updated_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7, 'pending', NOW(), NOW())",
    )
    .bind(&delegation_id)
    .bind(&trace_id)
    .bind(parent_task_id)
    .bind(agent_id)
    .bind(to_agent)
    .bind(title)
    .bind(description)
    .execute(&state.pg)
    .await?;

    // Send a message to the target agent's inbox
    let _ = sqlx::query(
        "INSERT INTO messages (from_agent, to_agent, subject, content, created_at)
         VALUES ($1, $2, 'delegation', $3, NOW())",
    )
    .bind(agent_id)
    .bind(to_agent)
    .bind(format!("Delegation: {title} (id: {delegation_id})"))
    .execute(&state.pg)
    .await;

    // Publish NATS notification to target agent
    let subject = format!(
        "{}.{}.agent.{}.delegation",
        state.config.nats.subject_prefix, state.config.broodlink.env, to_agent,
    );
    let nats_payload = serde_json::json!({
        "delegation_id": delegation_id,
        "from_agent": agent_id,
        "title": title,
    });
    if let Ok(bytes) = serde_json::to_vec(&nats_payload) {
        let _ = state.nats.publish(subject, bytes.into()).await;
    }

    Ok(serde_json::json!({ "delegation_id": delegation_id, "status": "pending" }))
}

async fn tool_accept_delegation(
    state: &AppState,
    agent_id: &str,
    params: &serde_json::Value,
) -> Result<serde_json::Value, BroodlinkError> {
    let delegation_id = param_str(params, "delegation_id")?;

    let affected = sqlx::query(
        "UPDATE delegations SET status = 'accepted', updated_at = NOW()
         WHERE id = $1 AND to_agent = $2 AND status = 'pending'",
    )
    .bind(delegation_id)
    .bind(agent_id)
    .execute(&state.pg)
    .await?;

    if affected.rows_affected() == 0 {
        return Err(BroodlinkError::NotFound(format!(
            "delegation {delegation_id} not found or not pending for agent {agent_id}"
        )));
    }

    // Notify the delegator
    let row: Option<(String,)> = sqlx::query_as(
        "SELECT from_agent FROM delegations WHERE id = $1",
    )
    .bind(delegation_id)
    .fetch_optional(&state.pg)
    .await?;

    if let Some((from_agent,)) = row {
        let subject = format!(
            "{}.{}.agent.{}.delegation_response",
            state.config.nats.subject_prefix, state.config.broodlink.env, from_agent,
        );
        let nats_payload = serde_json::json!({
            "delegation_id": delegation_id,
            "status": "accepted",
            "by": agent_id,
        });
        if let Ok(bytes) = serde_json::to_vec(&nats_payload) {
            let _ = state.nats.publish(subject, bytes.into()).await;
        }
    }

    Ok(serde_json::json!({ "delegation_id": delegation_id, "status": "accepted" }))
}

async fn tool_reject_delegation(
    state: &AppState,
    agent_id: &str,
    params: &serde_json::Value,
) -> Result<serde_json::Value, BroodlinkError> {
    let delegation_id = param_str(params, "delegation_id")?;
    let reason = param_str_opt(params, "reason");

    let affected = sqlx::query(
        "UPDATE delegations SET status = 'rejected', result = $1, updated_at = NOW()
         WHERE id = $2 AND to_agent = $3 AND status = 'pending'",
    )
    .bind(reason.map(|r| serde_json::json!({"reason": r})))
    .bind(delegation_id)
    .bind(agent_id)
    .execute(&state.pg)
    .await?;

    if affected.rows_affected() == 0 {
        return Err(BroodlinkError::NotFound(format!(
            "delegation {delegation_id} not found or not pending for agent {agent_id}"
        )));
    }

    // Notify the delegator
    let row: Option<(String,)> = sqlx::query_as(
        "SELECT from_agent FROM delegations WHERE id = $1",
    )
    .bind(delegation_id)
    .fetch_optional(&state.pg)
    .await?;

    if let Some((from_agent,)) = row {
        let subject = format!(
            "{}.{}.agent.{}.delegation_response",
            state.config.nats.subject_prefix, state.config.broodlink.env, from_agent,
        );
        let nats_payload = serde_json::json!({
            "delegation_id": delegation_id,
            "status": "rejected",
            "by": agent_id,
            "reason": reason,
        });
        if let Ok(bytes) = serde_json::to_vec(&nats_payload) {
            let _ = state.nats.publish(subject, bytes.into()).await;
        }
    }

    Ok(serde_json::json!({ "delegation_id": delegation_id, "status": "rejected" }))
}

async fn tool_complete_delegation(
    state: &AppState,
    agent_id: &str,
    params: &serde_json::Value,
) -> Result<serde_json::Value, BroodlinkError> {
    let delegation_id = param_str(params, "delegation_id")?;
    let result_json = param_json(params, "result")?;

    let affected = sqlx::query(
        "UPDATE delegations SET status = 'completed', result = $1, updated_at = NOW()
         WHERE id = $2 AND to_agent = $3 AND status IN ('accepted', 'in_progress')",
    )
    .bind(&result_json)
    .bind(delegation_id)
    .bind(agent_id)
    .execute(&state.pg)
    .await?;

    if affected.rows_affected() == 0 {
        return Err(BroodlinkError::NotFound(format!(
            "delegation {delegation_id} not found or not accepted/in_progress for agent {agent_id}"
        )));
    }

    // Notify the delegator
    let row: Option<(String,)> = sqlx::query_as(
        "SELECT from_agent FROM delegations WHERE id = $1",
    )
    .bind(delegation_id)
    .fetch_optional(&state.pg)
    .await?;

    if let Some((from_agent,)) = row {
        let subject = format!(
            "{}.{}.agent.{}.delegation_response",
            state.config.nats.subject_prefix, state.config.broodlink.env, from_agent,
        );
        let nats_payload = serde_json::json!({
            "delegation_id": delegation_id,
            "status": "completed",
            "by": agent_id,
            "result": result_json,
        });
        if let Ok(bytes) = serde_json::to_vec(&nats_payload) {
            let _ = state.nats.publish(subject, bytes.into()).await;
        }
    }

    Ok(serde_json::json!({ "delegation_id": delegation_id, "status": "completed" }))
}

async fn tool_get_delegation(
    state: &AppState,
    params: &serde_json::Value,
) -> Result<serde_json::Value, BroodlinkError> {
    let delegation_id = param_str(params, "delegation_id")?;

    let row = sqlx::query_as::<_, (String, String, Option<String>, String, String, String, Option<String>, String, Option<serde_json::Value>, String, String)>(
        "SELECT id, trace_id, parent_task_id, from_agent, to_agent, title, description, status, result, created_at::text, updated_at::text
         FROM delegations WHERE id = $1",
    )
    .bind(delegation_id)
    .fetch_optional(&state.pg)
    .await?
    .ok_or_else(|| BroodlinkError::NotFound(format!("delegation {delegation_id}")))?;

    Ok(serde_json::json!({
        "id": row.0,
        "trace_id": row.1,
        "parent_task_id": row.2,
        "from_agent": row.3,
        "to_agent": row.4,
        "title": row.5,
        "description": row.6,
        "status": row.7,
        "result": row.8,
        "created_at": row.9,
        "updated_at": row.10,
    }))
}

async fn tool_list_delegations(
    state: &AppState,
    params: &serde_json::Value,
) -> Result<serde_json::Value, BroodlinkError> {
    let limit = param_i64_opt(params, "limit").unwrap_or(50);
    let from_agent = param_str_opt(params, "from_agent");
    let to_agent = param_str_opt(params, "to_agent");
    let status_filter = param_str_opt(params, "status");

    // Build dynamic query
    let mut conditions = vec!["TRUE".to_string()];
    let mut bind_idx = 0u32;
    let mut binds: Vec<String> = Vec::new();

    if let Some(f) = from_agent {
        bind_idx += 1;
        conditions.push(format!("from_agent = ${bind_idx}"));
        binds.push(f.to_string());
    }
    if let Some(t) = to_agent {
        bind_idx += 1;
        conditions.push(format!("to_agent = ${bind_idx}"));
        binds.push(t.to_string());
    }
    if let Some(s) = status_filter {
        bind_idx += 1;
        conditions.push(format!("status = ${bind_idx}"));
        binds.push(s.to_string());
    }
    bind_idx += 1;
    let limit_placeholder = format!("${bind_idx}");

    let sql = format!(
        "SELECT id, parent_task_id, from_agent, to_agent, title, status, result, created_at::text, updated_at::text
         FROM delegations WHERE {} ORDER BY created_at DESC LIMIT {}",
        conditions.join(" AND "),
        limit_placeholder,
    );

    let mut query = sqlx::query_as::<_, (String, Option<String>, String, String, String, String, Option<serde_json::Value>, String, String)>(&sql);
    for b in &binds {
        query = query.bind(b);
    }
    query = query.bind(limit);

    let rows = query.fetch_all(&state.pg).await?;

    let delegations: Vec<serde_json::Value> = rows
        .into_iter()
        .map(|(id, parent_task_id, from_agent, to_agent, title, status, result, created_at, updated_at)| {
            serde_json::json!({
                "id": id,
                "parent_task_id": parent_task_id,
                "from_agent": from_agent,
                "to_agent": to_agent,
                "title": title,
                "status": status,
                "result": result,
                "created_at": created_at,
                "updated_at": updated_at,
            })
        })
        .collect();

    Ok(serde_json::json!({ "delegations": delegations }))
}

// ---------------------------------------------------------------------------
// Streaming tools
// ---------------------------------------------------------------------------

async fn tool_start_stream(
    state: &AppState,
    agent_id: &str,
    params: &serde_json::Value,
) -> Result<serde_json::Value, BroodlinkError> {
    let task_id = param_str_opt(params, "task_id");
    let tool_name = param_str_opt(params, "tool_name");

    let stream_id = Uuid::new_v4().to_string();

    sqlx::query(
        "INSERT INTO streams (id, agent_id, tool_name, task_id, status, created_at)
         VALUES ($1, $2, $3, $4, 'active', NOW())",
    )
    .bind(&stream_id)
    .bind(agent_id)
    .bind(tool_name)
    .bind(task_id)
    .execute(&state.pg)
    .await?;

    let sse_url = format!("/api/v1/stream/{stream_id}");

    Ok(serde_json::json!({
        "stream_id": stream_id,
        "sse_url": sse_url,
        "status": "active",
    }))
}

async fn tool_emit_stream_event(
    state: &AppState,
    agent_id: &str,
    params: &serde_json::Value,
) -> Result<serde_json::Value, BroodlinkError> {
    let stream_id = param_str(params, "stream_id")?;
    let event_type = param_str(params, "event_type")?;
    let data_json = param_json(params, "data")?;

    if !["progress", "chunk", "complete", "error"].contains(&event_type) {
        return Err(BroodlinkError::Validation {
            field: "event_type".to_string(),
            message: "must be one of: progress, chunk, complete, error".to_string(),
        });
    }

    // Verify the stream exists and is active
    let row: Option<(String,)> = sqlx::query_as(
        "SELECT status FROM streams WHERE id = $1 AND agent_id = $2",
    )
    .bind(stream_id)
    .bind(agent_id)
    .fetch_optional(&state.pg)
    .await?;

    match row {
        Some((status,)) if status != "active" => {
            return Err(BroodlinkError::Validation {
                field: "stream_id".to_string(),
                message: format!("stream is {status}, not active"),
            });
        }
        None => {
            return Err(BroodlinkError::NotFound(format!(
                "stream {stream_id} not found for agent {agent_id}"
            )));
        }
        _ => {}
    }

    // Publish event to NATS
    let subject = format!(
        "{}.{}.stream.{}",
        state.config.nats.subject_prefix, state.config.broodlink.env, stream_id,
    );
    let nats_payload = serde_json::json!({
        "event_type": event_type,
        "data": data_json,
        "agent_id": agent_id,
        "timestamp": chrono::Utc::now().to_rfc3339(),
    });
    if let Ok(bytes) = serde_json::to_vec(&nats_payload) {
        state.nats.publish(subject, bytes.into()).await.map_err(BroodlinkError::from)?;
    }

    // Close stream on terminal events
    if event_type == "complete" || event_type == "error" {
        let _ = sqlx::query(
            "UPDATE streams SET status = $1, closed_at = NOW() WHERE id = $2",
        )
        .bind(if event_type == "complete" { "completed" } else { "failed" })
        .bind(stream_id)
        .execute(&state.pg)
        .await;
    }

    Ok(serde_json::json!({ "emitted": true, "stream_id": stream_id, "event_type": event_type }))
}

// ---------------------------------------------------------------------------
// A2A (Agent-to-Agent) tools
// ---------------------------------------------------------------------------

/// Fetch a remote agent's A2A AgentCard.
async fn tool_a2a_discover(
    params: &serde_json::Value,
) -> Result<serde_json::Value, BroodlinkError> {
    let base_url = param_str(params, "url")?;
    let url = format!("{}/.well-known/agent.json", base_url.trim_end_matches('/'));

    let client = reqwest::Client::new();
    let resp = client
        .get(&url)
        .timeout(std::time::Duration::from_secs(10))
        .send()
        .await
        .map_err(|e| BroodlinkError::Internal(format!("a2a discover failed: {e}")))?;

    if !resp.status().is_success() {
        let status = resp.status();
        return Err(BroodlinkError::Internal(format!(
            "AgentCard fetch returned {status}"
        )));
    }

    let card: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| BroodlinkError::Internal(format!("invalid AgentCard JSON: {e}")))?;

    Ok(card)
}

/// Send a task to a remote A2A agent.
async fn tool_a2a_delegate(
    params: &serde_json::Value,
) -> Result<serde_json::Value, BroodlinkError> {
    let base_url = param_str(params, "url")?;
    let message_text = param_str(params, "message")?;
    let api_key = params
        .get("api_key")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    let url = format!("{}/a2a/tasks/send", base_url.trim_end_matches('/'));
    let task_id = uuid::Uuid::new_v4().to_string();

    let body = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tasks/send",
        "params": {
            "id": task_id,
            "message": {
                "role": "user",
                "parts": [{ "type": "text", "text": message_text }],
            },
        },
    });

    let client = reqwest::Client::new();
    let mut req = client
        .post(&url)
        .header("Content-Type", "application/json")
        .timeout(std::time::Duration::from_secs(30));

    if !api_key.is_empty() {
        req = req.header("Authorization", format!("Bearer {api_key}"));
    }

    let resp = req
        .json(&body)
        .send()
        .await
        .map_err(|e| BroodlinkError::Internal(format!("a2a delegate failed: {e}")))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        return Err(BroodlinkError::Internal(format!(
            "a2a_delegate returned {status}: {text}"
        )));
    }

    let result: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| BroodlinkError::Internal(format!("invalid a2a response JSON: {e}")))?;

    Ok(serde_json::json!({
        "task_id": task_id,
        "response": result,
    }))
}

// ---------------------------------------------------------------------------
// Workflow orchestration
// ---------------------------------------------------------------------------

async fn tool_start_workflow(
    state: &AppState,
    agent_id: &str,
    params: &serde_json::Value,
) -> Result<serde_json::Value, BroodlinkError> {
    let formula_name = param_str(params, "formula_name")?;
    let workflow_params: serde_json::Value = params
        .get("params")
        .cloned()
        .unwrap_or_else(|| serde_json::json!({}));

    let workflow_id = Uuid::new_v4().to_string();
    let convoy_id = format!("wf-{formula_name}-{}", &workflow_id[..8]);

    sqlx::query(
        "INSERT INTO workflow_runs (id, convoy_id, formula_name, status, current_step, total_steps, params, step_results, started_by, created_at, updated_at)
         VALUES ($1, $2, $3, 'pending', 0, 0, $4, '{}', $5, NOW(), NOW())",
    )
    .bind(&workflow_id)
    .bind(&convoy_id)
    .bind(formula_name)
    .bind(&workflow_params)
    .bind(agent_id)
    .execute(&state.pg)
    .await?;

    // Notify coordinator to start workflow
    let subject = format!(
        "{}.{}.coordinator.workflow_start",
        state.config.nats.subject_prefix, state.config.broodlink.env,
    );
    let nats_payload = serde_json::json!({
        "workflow_id": workflow_id,
        "convoy_id": convoy_id,
        "formula_name": formula_name,
        "params": workflow_params,
        "started_by": agent_id,
    });
    if let Ok(bytes) = serde_json::to_vec(&nats_payload) {
        if let Err(e) = state.nats.publish(subject, bytes.into()).await {
            warn!(error = %e, "failed to publish workflow_start event");
        }
    }

    Ok(serde_json::json!({
        "workflow_id": workflow_id,
        "convoy_id": convoy_id,
        "formula_name": formula_name,
        "started": true,
    }))
}

// ---------------------------------------------------------------------------
// SSE stream handler
// ---------------------------------------------------------------------------

async fn sse_stream_handler(
    State(state): State<Arc<AppState>>,
    Path(stream_id): Path<String>,
    axum::Extension(_claims): axum::Extension<Claims>,
) -> Result<Sse<impl Stream<Item = Result<Event, std::convert::Infallible>>>, BroodlinkError> {
    // Verify the stream exists
    let row: Option<(String,)> = sqlx::query_as(
        "SELECT status FROM streams WHERE id = $1",
    )
    .bind(&stream_id)
    .fetch_optional(&state.pg)
    .await?;

    if row.is_none() {
        return Err(BroodlinkError::NotFound(format!("stream {stream_id}")));
    }

    // Subscribe to NATS subject for this stream
    let subject = format!(
        "{}.{}.stream.{}",
        state.config.nats.subject_prefix, state.config.broodlink.env, stream_id,
    );

    let mut subscriber = state
        .nats
        .subscribe(subject)
        .await
        .map_err(|e| BroodlinkError::Nats(e.to_string()))?;

    // Bridge NATS messages → SSE events via mpsc channel
    let (tx, rx) = tokio::sync::mpsc::channel::<Result<Event, std::convert::Infallible>>(64);

    tokio::spawn(async move {
        while let Some(msg) = subscriber.next().await {
            let data = String::from_utf8_lossy(&msg.payload).to_string();

            // Parse event_type from payload if possible
            let event_type = serde_json::from_str::<serde_json::Value>(&data)
                .ok()
                .and_then(|v| v["event_type"].as_str().map(String::from))
                .unwrap_or_else(|| "message".to_string());

            let event = Event::default()
                .event(&event_type)
                .data(&data);

            if tx.send(Ok(event)).await.is_err() {
                break; // Client disconnected
            }

            // Stop on terminal events
            if event_type == "complete" || event_type == "error" {
                break;
            }
        }
    });

    let stream = ReceiverStream::new(rx);

    Ok(Sse::new(stream).keep_alive(KeepAlive::default()))
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use serde_json::json;

    // -----------------------------------------------------------------------
    // Circuit breaker tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_circuit_breaker_starts_closed() {
        let cb = CircuitBreaker::new("test", 5, 30);
        assert!(!cb.is_open(), "a new circuit breaker should be closed");
        assert!(cb.check().is_ok(), "check() should succeed on a closed breaker");
    }

    #[test]
    fn test_circuit_breaker_opens_after_threshold() {
        let cb = CircuitBreaker::new("test", 5, 30);

        for _ in 0..5 {
            cb.record_failure();
        }

        assert!(cb.is_open(), "breaker should be open after 5 failures");
        assert!(cb.check().is_err(), "check() should return Err when open");
    }

    #[test]
    fn test_circuit_breaker_closes_on_success() {
        let cb = CircuitBreaker::new("test", 5, 30);

        // Open it
        for _ in 0..5 {
            cb.record_failure();
        }
        assert!(cb.is_open());

        // Record success resets the failure count
        cb.record_success();
        assert!(!cb.is_open(), "breaker should close after record_success()");
        assert!(cb.check().is_ok());
    }

    // -----------------------------------------------------------------------
    // Rate limiter tests
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn test_rate_limiter_allows_within_limit() {
        // 60 rpm, burst of 10 => 10 tokens initially
        let rl = RateLimiter::new(60, 10);

        // First 10 calls should succeed (burst window)
        for i in 0..10 {
            assert!(
                rl.check("agent-1").await.is_ok(),
                "call {i} should be within burst"
            );
        }
    }

    #[tokio::test]
    async fn test_rate_limiter_rejects_over_burst() {
        let rl = RateLimiter::new(60, 5);

        // Exhaust all 5 burst tokens
        for _ in 0..5 {
            rl.check("agent-1").await.unwrap();
        }

        // Next check should fail (no time for refill)
        let result = rl.check("agent-1").await;
        assert!(result.is_err(), "should be rate limited after exhausting burst");
    }

    // -----------------------------------------------------------------------
    // Param extraction tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_param_str_present() {
        let params = json!({"key": "value"});
        let result = param_str(&params, "key").unwrap();
        assert_eq!(result, "value");
    }

    #[test]
    fn test_param_str_missing() {
        let params = json!({});
        let result = param_str(&params, "key");
        assert!(result.is_err(), "missing field should return Err");
    }

    #[test]
    fn test_param_str_opt_present() {
        let params = json!({"key": "value"});
        assert_eq!(param_str_opt(&params, "key"), Some("value"));
    }

    #[test]
    fn test_param_str_opt_absent() {
        let params = json!({});
        assert_eq!(param_str_opt(&params, "key"), None);
    }

    #[test]
    fn test_param_i64_from_number() {
        let params = json!({"n": 42});
        let result = param_i64(&params, "n").unwrap();
        assert_eq!(result, 42);
    }

    #[test]
    fn test_param_i64_from_string() {
        let params = json!({"n": "42"});
        let result = param_i64(&params, "n").unwrap();
        assert_eq!(result, 42);
    }

    #[test]
    fn test_param_i64_opt_present() {
        let params = json!({"n": 99});
        assert_eq!(param_i64_opt(&params, "n"), Some(99));
    }

    #[test]
    fn test_param_i64_opt_absent() {
        let params = json!({});
        assert_eq!(param_i64_opt(&params, "n"), None);
    }

    // -----------------------------------------------------------------------
    // Tool registry tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_tool_registry_count() {
        // Must match the number of match arms in tool_dispatch (excluding the _ fallback)
        assert_eq!(TOOL_REGISTRY.len(), 61, "tool registry should have 61 tools");
    }

    #[test]
    fn test_tool_registry_openai_format() {
        for tool in TOOL_REGISTRY.iter() {
            assert_eq!(tool["type"], "function", "tool type must be 'function'");
            let func = &tool["function"];
            assert!(func["name"].is_string(), "tool must have a name");
            assert!(func["description"].is_string(), "tool must have a description");
            assert_eq!(func["parameters"]["type"], "object", "params must be object type");
        }
    }

    #[test]
    fn test_tool_registry_has_known_tools() {
        let names: Vec<&str> = TOOL_REGISTRY
            .iter()
            .map(|t| t["function"]["name"].as_str().unwrap_or(""))
            .collect();
        for expected in &[
            "store_memory", "recall_memory", "semantic_search",
            "log_work", "list_projects", "send_message",
            "log_decision", "claim_task", "agent_upsert",
            "health_check", "ping",
            // v0.2.0 tools
            "set_guardrail", "list_guardrails", "get_guardrail_violations",
            "create_approval_gate", "resolve_approval", "list_approvals", "get_approval",
            "set_approval_policy", "list_approval_policies",
            "get_routing_scores",
            "delegate_task", "accept_delegation", "reject_delegation", "complete_delegation",
            "start_stream", "emit_stream_event",
            "a2a_discover", "a2a_delegate",
        ] {
            assert!(names.contains(expected), "registry missing tool: {expected}");
        }
    }

    // -----------------------------------------------------------------------
    // Error response code tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_error_auth_returns_401() {
        let err = BroodlinkError::Auth("bad token".to_string());
        let resp = err.into_response();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[test]
    fn test_error_validation_returns_400() {
        let err = BroodlinkError::Validation {
            field: "topic".to_string(),
            message: "required".to_string(),
        };
        let resp = err.into_response();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[test]
    fn test_error_not_found_returns_404() {
        let err = BroodlinkError::NotFound("task xyz".to_string());
        let resp = err.into_response();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[test]
    fn test_error_rate_limited_returns_429() {
        let err = BroodlinkError::RateLimited("agent-1".to_string());
        let resp = err.into_response();
        assert_eq!(resp.status(), StatusCode::TOO_MANY_REQUESTS);
    }

    #[test]
    fn test_error_circuit_open_returns_503() {
        let err = BroodlinkError::CircuitOpen("qdrant".to_string());
        let resp = err.into_response();
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[test]
    fn test_error_guardrail_returns_403() {
        let err = BroodlinkError::Guardrail {
            policy: "block-deploy".to_string(),
            message: "tool blocked".to_string(),
        };
        let resp = err.into_response();
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    }

    #[test]
    fn test_error_internal_returns_500() {
        let err = BroodlinkError::Internal("unexpected".to_string());
        let resp = err.into_response();
        assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[test]
    fn test_error_response_is_json() {
        let err = BroodlinkError::NotFound("test".to_string());
        let resp = err.into_response();
        let ct = resp.headers().get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        assert!(ct.contains("application/json"), "error response should be JSON, got: {ct}");
    }

    // -----------------------------------------------------------------------
    // ToolRequest / ToolResponse / Claims serde tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_tool_request_with_params() {
        let input = json!({"params": {"topic": "test", "content": "hello"}});
        let req: ToolRequest = serde_json::from_value(input).unwrap();
        assert_eq!(req.params["topic"], "test");
        assert_eq!(req.params["content"], "hello");
    }

    #[test]
    fn test_tool_request_empty_body() {
        let input = json!({});
        let req: ToolRequest = serde_json::from_value(input).unwrap();
        assert!(req.params.is_null(), "missing params should default to null");
    }

    #[test]
    fn test_tool_response_serialization() {
        let resp = ToolResponse {
            tool: "ping".to_string(),
            success: true,
            data: json!({"pong": true}),
        };
        let j = serde_json::to_value(&resp).unwrap();
        assert_eq!(j["tool"], "ping");
        assert_eq!(j["success"], true);
        assert_eq!(j["data"]["pong"], true);
    }

    #[test]
    fn test_claims_deserialization() {
        let input = json!({
            "sub": "claude",
            "agent_id": "claude",
            "exp": 9999999999_u64,
            "iat": 1700000000_u64,
        });
        let claims: Claims = serde_json::from_value(input).unwrap();
        assert_eq!(claims.agent_id, "claude");
        assert_eq!(claims.sub, "claude");
    }

    #[test]
    fn test_claims_missing_agent_id_fails() {
        let input = json!({"sub": "claude", "exp": 9999999999_u64, "iat": 1700000000_u64});
        let result = serde_json::from_value::<Claims>(input);
        assert!(result.is_err(), "missing agent_id should fail deserialization");
    }

    // -----------------------------------------------------------------------
    // policy_matches tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_policy_matches_severity() {
        let cond = json!({"severity": ["low", "medium"]});
        assert!(policy_matches(&cond, "low", "claude", None));
        assert!(policy_matches(&cond, "medium", "claude", None));
        assert!(!policy_matches(&cond, "high", "claude", None));
        assert!(!policy_matches(&cond, "critical", "claude", None));
    }

    #[test]
    fn test_policy_matches_agent_wildcard() {
        let cond = json!({"severity": ["low"], "agent_ids": ["*"]});
        assert!(policy_matches(&cond, "low", "claude", None));
        assert!(policy_matches(&cond, "low", "qwen3", None));
        assert!(policy_matches(&cond, "low", "any-agent", None));
    }

    #[test]
    fn test_policy_matches_specific_agent() {
        let cond = json!({"severity": ["low"], "agent_ids": ["claude"]});
        assert!(policy_matches(&cond, "low", "claude", None));
        assert!(!policy_matches(&cond, "low", "qwen3", None));
    }

    #[test]
    fn test_policy_matches_empty_conditions() {
        let cond = json!({});
        assert!(policy_matches(&cond, "low", "claude", None));
        assert!(policy_matches(&cond, "critical", "any-agent", Some("deploy")));
    }

    #[test]
    fn test_policy_matches_tool_filter() {
        let cond = json!({"tool_names": ["store_memory", "log_work"]});
        assert!(policy_matches(&cond, "low", "claude", Some("store_memory")));
        assert!(policy_matches(&cond, "low", "claude", Some("log_work")));
        assert!(!policy_matches(&cond, "low", "claude", Some("deploy")));
        assert!(!policy_matches(&cond, "low", "claude", None));
    }
}
