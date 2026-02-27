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

#![allow(clippy::module_name_repetitions)]

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
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
use broodlink_config::Config;
use broodlink_runtime::CircuitBreaker;
use broodlink_secrets::SecretsProvider;
use futures::stream::Stream;
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use sqlx::mysql::MySqlPoolOptions;
use sqlx::postgres::PgPoolOptions;
use sqlx::{MySqlPool, PgPool};
use tokio::sync::RwLock;
use tokio_stream::wrappers::ReceiverStream;
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
    #[error("budget exhausted: agent {agent_id} has {balance} tokens, needs {cost}")]
    BudgetExhausted {
        agent_id: String,
        balance: i64,
        cost: i64,
    },
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
        // Log full details server-side, return generic messages to clients
        let (status, body) = match &self {
            Self::Database(e) => {
                error!(error = %e, "database error");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal error".to_string(),
                )
            }
            Self::Nats(e) => {
                error!(error = %e, "nats error");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal error".to_string(),
                )
            }
            Self::Config(e) => {
                error!(error = %e, "config error");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal error".to_string(),
                )
            }
            Self::Secrets(e) => {
                error!(error = %e, "secrets error");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal error".to_string(),
                )
            }
            Self::Auth(msg) => {
                warn!(msg = %msg, "auth failure");
                (
                    StatusCode::UNAUTHORIZED,
                    "authentication required".to_string(),
                )
            }
            Self::Validation { field, message } => {
                warn!(field = %field, message = %message, "validation error");
                (StatusCode::BAD_REQUEST, self.to_string())
            }
            Self::NotFound(what) => {
                info!(entity = %what, "not found");
                (StatusCode::NOT_FOUND, "not found".to_string())
            }
            Self::RateLimited(agent) => {
                warn!(agent = %agent, "rate limited");
                (StatusCode::TOO_MANY_REQUESTS, "rate limited".to_string())
            }
            Self::CircuitOpen(svc) => {
                warn!(service = %svc, "circuit open");
                (
                    StatusCode::SERVICE_UNAVAILABLE,
                    "service temporarily unavailable".to_string(),
                )
            }
            Self::Guardrail {
                ref policy,
                ref message,
            } => {
                warn!(policy = %policy, message = %message, "guardrail blocked");
                (
                    StatusCode::FORBIDDEN,
                    "blocked by guardrail policy".to_string(),
                )
            }
            Self::BudgetExhausted {
                ref agent_id,
                balance,
                cost,
            } => {
                warn!(agent = %agent_id, balance = balance, cost = cost, "budget exhausted");
                (StatusCode::PAYMENT_REQUIRED, "budget exhausted".to_string())
            }
            Self::Internal(msg) => {
                error!(msg = %msg, "internal error");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal error".to_string(),
                )
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
    pub reranker_breaker: CircuitBreaker,
    pub start_time: Instant,
    pub jwt_decoding_keys: Vec<(String, jsonwebtoken::DecodingKey)>, // (kid, key) pairs
    pub jwt_validation: jsonwebtoken::Validation,
    pub sse_connections: RwLock<HashMap<String, usize>>, // agent_id → active SSE count
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
        if shared.config.broodlink.env != "dev" && shared.config.broodlink.env != "local" {
            warn!("TLS is disabled in non-dev environment — traffic is unencrypted");
        }
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
            sc.infisical_token.as_deref(),
        )?;
        Arc::from(provider)
    };

    // Resolve database passwords from secrets
    let dolt_password = secrets.get(&config.dolt.password_key).await?;
    let pg_password = secrets.get(&config.postgres.password_key).await?;

    // JWT RS256 decoding keys — load primary from secrets, then scan keys_dir for extras
    let mut jwt_decoding_keys: Vec<(String, jsonwebtoken::DecodingKey)> = Vec::new();

    // Primary key from secrets (always loaded)
    let jwt_public_pem = secrets.get("BROODLINK_JWT_PUBLIC_KEY").await?;
    let primary_kid = compute_key_kid(jwt_public_pem.as_bytes());
    let primary_key = jsonwebtoken::DecodingKey::from_rsa_pem(jwt_public_pem.as_bytes())
        .map_err(|e| BroodlinkError::Auth(format!("invalid JWT public key: {e}")))?;
    jwt_decoding_keys.push((primary_kid.clone(), primary_key));
    info!(kid = %primary_kid, "loaded primary JWT key");

    // Scan keys_dir for additional public keys (for rotation)
    if let Ok(entries) = std::fs::read_dir(&config.jwt.keys_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            let fname = entry.file_name().to_string_lossy().to_string();
            // Load jwt-public-*.pem files (but skip the primary jwt-public.pem)
            if fname.starts_with("jwt-public-") && fname.ends_with(".pem") {
                if let Ok(pem_bytes) = std::fs::read(&path) {
                    let kid = compute_key_kid(&pem_bytes);
                    if kid != primary_kid {
                        match jsonwebtoken::DecodingKey::from_rsa_pem(&pem_bytes) {
                            Ok(dk) => {
                                info!(kid = %kid, path = %path.display(), "loaded additional JWT key");
                                jwt_decoding_keys.push((kid, dk));
                            }
                            Err(e) => {
                                warn!(path = %path.display(), error = %e, "skipped invalid JWT key file");
                            }
                        }
                    }
                }
            }
        }
    }
    info!(count = jwt_decoding_keys.len(), "JWT decoding keys loaded");

    let mut jwt_validation = jsonwebtoken::Validation::new(jsonwebtoken::Algorithm::RS256);
    jwt_validation.validate_exp = true;
    jwt_validation.leeway = 60; // 60 seconds clock skew tolerance
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
        .idle_timeout(Duration::from_secs(300))
        .max_lifetime(Duration::from_secs(1800))
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
        .idle_timeout(Duration::from_secs(300))
        .max_lifetime(Duration::from_secs(1800))
        .connect(&pg_url)
        .await?;
    info!("postgres pool connected");

    // NATS client (cluster-aware via broodlink-runtime)
    let nats = broodlink_runtime::connect_nats(&config.nats).await?;

    // Rate limiter from config (validate bounds)
    let rpm = config.rate_limits.requests_per_minute_per_agent;
    let burst = config.rate_limits.burst;
    if rpm == 0 || rpm > 10_000 {
        error!("invalid rate_limits.requests_per_minute_per_agent: {rpm} (must be 1–10000)");
        process::exit(1);
    }
    if burst == 0 || burst > 1_000 {
        error!("invalid rate_limits.burst: {burst} (must be 1–1000)");
        process::exit(1);
    }
    let rate_limiter = RateLimiter::new(rpm, burst);

    // Read replica pool (optional)
    let pg_read = if !config.postgres_read_replicas.urls.is_empty() {
        let replica_url = &config.postgres_read_replicas.urls[0];
        match PgPoolOptions::new()
            .min_connections(1)
            .max_connections(5)
            .acquire_timeout(Duration::from_secs(3))
            .idle_timeout(Duration::from_secs(300))
            .max_lifetime(Duration::from_secs(1800))
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
    let qdrant_breaker =
        CircuitBreaker::new("qdrant", CIRCUIT_FAILURE_THRESHOLD, CIRCUIT_HALF_OPEN_SECS);
    let ollama_breaker =
        CircuitBreaker::new("ollama", CIRCUIT_FAILURE_THRESHOLD, CIRCUIT_HALF_OPEN_SECS);
    let reranker_breaker = CircuitBreaker::new(
        "reranker",
        CIRCUIT_FAILURE_THRESHOLD,
        CIRCUIT_HALF_OPEN_SECS,
    );

    // Pre-pull reranker model (best-effort, non-blocking)
    if config.memory_search.reranker_enabled {
        let reranker_model = config.memory_search.reranker_model.clone();
        let ollama_url = config.ollama.url.clone();
        tokio::spawn(async move {
            let client = reqwest::Client::new();
            match client
                .post(format!("{ollama_url}/api/pull"))
                .json(&serde_json::json!({ "name": reranker_model, "stream": false }))
                .timeout(Duration::from_secs(120))
                .send()
                .await
            {
                Ok(resp) if resp.status().is_success() => {
                    info!(model = %reranker_model, "reranker model ready");
                }
                Ok(resp) => {
                    warn!(model = %reranker_model, status = %resp.status(), "reranker model pull failed — reranking may not work");
                }
                Err(e) => {
                    warn!(model = %reranker_model, error = %e, "reranker model pull failed — reranking may not work");
                }
            }
        });
    }

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
        reranker_breaker,
        start_time: Instant::now(),
        jwt_decoding_keys,
        jwt_validation,
        sse_connections: RwLock::new(HashMap::new()),
    })
}

/// Returns the read replica pool if available, otherwise the primary.
#[allow(dead_code)]
fn pg_read_pool(state: &AppState) -> &PgPool {
    state.pg_read.as_ref().unwrap_or(&state.pg)
}

/// Compute a short key ID (kid) from PEM bytes using SHA-256 truncated to 16 hex chars.
fn compute_key_kid(pem_bytes: &[u8]) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut hasher = DefaultHasher::new();
    pem_bytes.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

/// JWKS endpoint — returns all loaded public keys with their kid identifiers (no auth required).
async fn jwks_handler(State(state): State<Arc<AppState>>) -> axum::Json<serde_json::Value> {
    let keys: Vec<serde_json::Value> = state
        .jwt_decoding_keys
        .iter()
        .map(|(kid, _)| {
            serde_json::json!({
                "kid": kid,
                "kty": "RSA",
                "use": "sig",
                "alg": "RS256",
            })
        })
        .collect();

    axum::Json(serde_json::json!({
        "keys": keys,
        "total": keys.len(),
    }))
}

async fn shutdown_signal() {
    broodlink_runtime::shutdown_signal().await;
}

// ---------------------------------------------------------------------------
// Tool metadata registry (OpenAI function-calling format)
// ---------------------------------------------------------------------------

fn tool_def(
    name: &str,
    desc: &str,
    props: serde_json::Value,
    required: &[&str],
) -> serde_json::Value {
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

fn p_number(desc: &str) -> serde_json::Value {
    serde_json::json!({"type": "number", "description": desc})
}

static TOOL_REGISTRY: std::sync::LazyLock<Vec<serde_json::Value>> = std::sync::LazyLock::new(
    || {
        vec![
            // --- Memory ---
            tool_def("store_memory", "Store or update a memory entry by topic. Upserts — updates if topic exists, inserts if new.", serde_json::json!({
                "topic": p_str("Short kebab-case topic key, e.g. 'user-preferences'"),
                "content": p_str("The content to store"),
                "tags": p_str("Optional comma-separated tags"),
            }), &["topic", "content"]),
            tool_def("recall_memory", "Search agent memory. If topic_search given, matches by LIKE pattern. Otherwise returns all.", serde_json::json!({
                "topic_search": p_str("Optional search string to filter memories"),
                "limit": { "type": "integer", "description": "Max results to return (default 200, max 1000)" },
            }), &[]),
            tool_def("delete_memory", "Delete a memory entry by exact topic name.", serde_json::json!({
                "topic": p_str("Exact topic name to delete"),
            }), &["topic"]),
            tool_def("semantic_search", "Search memory using semantic vector similarity via Qdrant.", serde_json::json!({
                "query": p_str("Natural language search query"),
                "limit": p_int("Number of results (default 5)"),
                "agent_id": p_str("Optional: filter by agent ID"),
                "date_from": p_str("Optional: ISO date, return results after this date"),
                "date_to": p_str("Optional: ISO date, return results before this date"),
            }), &["query"]),
            tool_def("hybrid_search", "Search memory using hybrid BM25 + semantic vector fusion with temporal decay and optional reranking.", serde_json::json!({
                "query": p_str("Natural language search query"),
                "limit": p_int("Max results to return (default 10)"),
                "agent_id": p_str("Optional filter by agent"),
                "semantic_weight": p_number("Weight for semantic vector scores (default 0.6)"),
                "keyword_weight": p_number("Weight for BM25 keyword scores (default 0.4)"),
                "decay": p_bool("Enable temporal decay (default true)"),
                "rerank": p_bool("Enable reranking via dedicated model (default false)"),
            }), &["query"]),

            // --- Knowledge Graph (Postgres) ---
            tool_def("graph_search", "Search for entities and their direct relationships in the knowledge graph.", serde_json::json!({
                "query": p_str("Entity name or search term"),
                "entity_type": p_str("Optional filter: person, service, concept, location, technology, organization, event, other"),
                "include_edges": p_bool("Include direct relationships in results (default true)"),
                "limit": p_int("Max entities returned (default 10)"),
            }), &["query"]),
            tool_def("graph_traverse", "Multi-hop graph traversal starting from an entity. Follows chains of relationships.", serde_json::json!({
                "start_entity": p_str("Entity name to start traversal from"),
                "max_hops": p_int("Maximum traversal depth (default 3, capped by config)"),
                "relation_types": p_str("Optional comma-separated filter, e.g. 'DEPENDS_ON,RUNS_ON'"),
                "direction": p_str("'outgoing', 'incoming', or 'both' (default 'both')"),
            }), &["start_entity"]),
            tool_def("graph_update_edge", "Manage temporal validity of relationships. Invalidate outdated edges or update descriptions.", serde_json::json!({
                "source_entity": p_str("Source entity name"),
                "target_entity": p_str("Target entity name"),
                "relation_type": p_str("Relationship type (UPPER_SNAKE_CASE)"),
                "action": p_str("'invalidate' (set valid_to=NOW) or 'update_description'"),
                "description": p_str("New description (required for update_description action)"),
            }), &["source_entity", "target_entity", "relation_type", "action"]),
            tool_def("graph_stats", "Overview of the knowledge graph state: entity counts, edge counts, type distribution, most connected entities.", serde_json::json!({}), &[]),
            tool_def("graph_prune", "Prune stale entities and decayed edges from the knowledge graph.", serde_json::json!({
                "dry_run": p_bool("Preview what would be pruned without deleting (default true)"),
                "max_age_days": p_int("Remove entities not seen in this many days (default from config)"),
                "min_weight": p_number("Expire edges below this weight (default 0.1)"),
            }), &[]),

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

            // --- Scheduled Tasks ---
            tool_def("schedule_task", "Schedule a task to run at a future time, optionally recurring.", serde_json::json!({
                "title": p_str("Task title"),
                "description": p_str("Task description"),
                "priority": p_int("Priority (higher = more urgent)"),
                "formula_name": p_str("Optional formula to execute when task fires"),
                "run_at": p_str("When to run (ISO 8601 datetime, e.g. 2026-02-25T10:00:00Z)"),
                "recurrence_secs": p_int("Repeat interval in seconds (omit for one-shot)"),
                "max_runs": p_int("Maximum number of runs (omit for unlimited)"),
            }), &["title", "run_at"]),
            tool_def("list_scheduled_tasks", "List active scheduled tasks.", serde_json::json!({
                "limit": p_int("Number of tasks (default 50)"),
            }), &[]),
            tool_def("cancel_scheduled_task", "Cancel (disable) a scheduled task.", serde_json::json!({
                "id": p_int("Scheduled task ID to cancel"),
            }), &["id"]),

            // --- Notifications ---
            tool_def("send_notification", "Send a notification to a channel immediately.", serde_json::json!({
                "channel": p_str("Delivery channel: telegram or slack"),
                "target": p_str("Chat ID (telegram) or webhook URL (slack)"),
                "message": p_str("Message text to send"),
            }), &["channel", "target", "message"]),
            tool_def("create_notification_rule", "Create an automated notification rule.", serde_json::json!({
                "name": p_str("Unique rule name"),
                "condition_type": p_str("Condition: service_event_error, dlq_spike, budget_low"),
                "condition_config": p_str("JSON config: {threshold, window_minutes, auto_postmortem}"),
                "channel": p_str("Delivery channel: telegram or slack"),
                "target": p_str("Chat ID (telegram) or webhook URL (slack)"),
                "template": p_str("Message template with {{count}}, {{agents}}, {{type}} placeholders"),
                "cooldown_minutes": p_int("Minimum minutes between alerts (default 30)"),
            }), &["name", "condition_type", "channel", "target"]),
            tool_def("list_notification_rules", "List notification rules.", serde_json::json!({
                "limit": p_int("Number of rules (default 50)"),
            }), &[]),

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
                "gate_type": p_str("Gate type this policy applies to: all, pre_dispatch, pre_completion, budget, custom"),
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

            // --- Budget ---
            tool_def("get_budget", "Get an agent's current budget balance and recent transactions.", serde_json::json!({
                "agent_id": p_str("Agent to query (defaults to calling agent)"),
                "limit": p_int("Number of recent transactions to include (default 10)"),
            }), &[]),
            tool_def("set_budget", "Set an agent's budget token balance. Requires strategist role.", serde_json::json!({
                "agent_id": p_str("Target agent ID"),
                "tokens": p_int("New budget token balance"),
            }), &["agent_id", "tokens"]),
            tool_def("get_cost_map", "List per-tool token costs.", serde_json::json!({}), &[]),

            // --- Dead-Letter Queue ---
            tool_def("inspect_dlq", "List dead-letter queue entries for failed tasks.", serde_json::json!({
                "resolved": p_bool("Include resolved entries (default false)"),
                "limit": p_int("Max entries to return (default 20)"),
            }), &[]),
            tool_def("retry_dlq_task", "Manually retry a dead-lettered task.", serde_json::json!({
                "dlq_id": p_int("DLQ entry ID to retry"),
            }), &["dlq_id"]),
            tool_def("purge_dlq", "Remove resolved entries from the dead-letter queue.", serde_json::json!({
                "older_than_days": p_int("Purge entries older than this many days (default 7)"),
            }), &[]),

            // --- Multi-Agent Collaboration ---
            tool_def("decompose_task", "Decompose a task into sub-tasks assigned to different agents.", serde_json::json!({
                "parent_task_id": p_str("Parent task ID to decompose"),
                "sub_tasks": p_str("JSON array of sub-task definitions: [{title, description, role, cost_tier}]"),
                "merge_strategy": p_str("How to combine results: concatenate (default), vote, best"),
            }), &["parent_task_id", "sub_tasks"]),
            tool_def("create_workspace", "Create a shared workspace for multi-agent collaboration.", serde_json::json!({
                "name": p_str("Workspace name"),
                "participants": p_str("JSON array of agent IDs"),
                "context": p_str("Optional initial context JSON"),
            }), &["name"]),
            tool_def("workspace_read", "Read from a shared workspace.", serde_json::json!({
                "workspace_id": p_str("Workspace ID"),
            }), &["workspace_id"]),
            tool_def("workspace_write", "Write data to a shared workspace.", serde_json::json!({
                "workspace_id": p_str("Workspace ID"),
                "key": p_str("Key to write"),
                "value": p_str("Value to store (string or JSON)"),
            }), &["workspace_id", "key", "value"]),
            tool_def("merge_results", "Merge results from completed sub-tasks of a decomposed task.", serde_json::json!({
                "parent_task_id": p_str("Parent task ID whose sub-tasks to merge"),
            }), &["parent_task_id"]),

            // --- Chat Sessions (v0.7.0) ---
            tool_def("list_chat_sessions", "List chat sessions from the conversational agent gateway.", serde_json::json!({
                "platform": p_str("Filter by platform: slack, teams, telegram (optional)"),
                "status": p_str("Filter by status: active, paused, closed (default active)"),
                "limit": p_int("Max sessions to return (default 20)"),
            }), &[]),
            tool_def("reply_to_chat", "Send a proactive message to a chat session. Queues reply for delivery to the originating platform.", serde_json::json!({
                "session_id": p_str("Chat session ID to reply to"),
                "content": p_str("Message content to send"),
            }), &["session_id", "content"]),

            // --- Formula Registry (v0.7.0) ---
            tool_def("list_formulas", "List workflow formulas from the registry.", serde_json::json!({
                "enabled_only": p_bool("Only return enabled formulas (default true)"),
                "tag": p_str("Filter by tag (optional)"),
            }), &[]),
            tool_def("get_formula", "Get a workflow formula definition from the registry.", serde_json::json!({
                "name": p_str("Formula name (e.g. 'research', 'build-feature')"),
            }), &["name"]),
            tool_def("create_formula", "Create a new workflow formula in the registry.", serde_json::json!({
                "name": p_str("Formula name slug (e.g. 'my-workflow')"),
                "display_name": p_str("Human-readable name"),
                "description": p_str("Description of what this formula does"),
                "definition": p_str("Formula definition as JSON string (steps, parameters, on_failure)"),
                "tags": p_str("Tags as JSON array string (optional)"),
            }), &["name", "display_name", "definition"]),
            tool_def("update_formula", "Update an existing workflow formula in the registry.", serde_json::json!({
                "name": p_str("Formula name to update"),
                "definition": p_str("New formula definition as JSON string (optional, triggers version bump)"),
                "display_name": p_str("New display name (optional)"),
                "description": p_str("New description (optional)"),
                "enabled": p_bool("Enable/disable formula (optional)"),
                "tags": p_str("New tags as JSON array string (optional)"),
            }), &["name"]),
            // --- File I/O (v0.10.0) ---
            tool_def("read_file", "Read a text file from disk within allowed directories.", serde_json::json!({
                "path": p_str("Absolute path to the file"),
            }), &["path"]),
            tool_def("write_file", "Write content to a file within allowed directories.", serde_json::json!({
                "path": p_str("Absolute path to the file"),
                "content": p_str("Content to write"),
            }), &["path", "content"]),
            tool_def("read_pdf", "Extract text from a PDF file within allowed directories.", serde_json::json!({
                "path": p_str("Absolute path to the PDF file"),
            }), &["path"]),
            tool_def("read_docx", "Extract text from a Word (.docx) document within allowed directories.", serde_json::json!({
                "path": p_str("Absolute path to the .docx file"),
            }), &["path"]),
            // --- Negotiation (v0.11.0) ---
            tool_def("decline_task", "Decline a claimed task. Optionally suggest another agent.", serde_json::json!({
                "task_id": p_str("The task ID to decline"),
                "reason": p_str("Why the agent is declining"),
                "suggested_agent": p_str("Optional: agent ID better suited for this task"),
            }), &["task_id"]),
            tool_def("request_task_context", "Request additional context before working on a claimed task.", serde_json::json!({
                "task_id": p_str("The task ID to request context for"),
                "questions": {"type": "array", "items": {"type": "string"}, "description": "List of questions needing answers"},
            }), &["task_id", "questions"]),
        ]
    },
);

/// Returns all tool definitions in OpenAI function-calling format (public, no auth).
async fn tools_metadata_handler() -> axum::Json<serde_json::Value> {
    axum::Json(serde_json::json!({
        "tools": *TOOL_REGISTRY,
        "version": SERVICE_VERSION,
        "total": TOOL_REGISTRY.len(),
    }))
}

// ---------------------------------------------------------------------------
// Security headers middleware (OWASP A05)
// ---------------------------------------------------------------------------

async fn security_headers_middleware(
    req: Request<axum::body::Body>,
    next: Next,
) -> axum::response::Response {
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
// Router construction
// ---------------------------------------------------------------------------

fn build_router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/health", get(health_handler))
        .route("/api/v1/tools", get(tools_metadata_handler))
        .route("/api/v1/.well-known/jwks.json", get(jwks_handler))
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
        .layer(axum::extract::DefaultBodyLimit::max(10_485_760)) // 10 MiB
        .layer(middleware::from_fn(security_headers_middleware))
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

    // Multi-key validation: try kid-matched key first, then all keys
    let token_data = {
        let header_kid = jsonwebtoken::decode_header(token).ok().and_then(|h| h.kid);

        if let Some(ref kid) = header_kid {
            // Try the specific key matching the kid
            if let Some((_, key)) = state.jwt_decoding_keys.iter().find(|(k, _)| k == kid) {
                match jsonwebtoken::decode::<Claims>(token, key, &state.jwt_validation) {
                    Ok(data) => data,
                    Err(e) => {
                        let msg = format!("invalid token (kid={kid}): {e}");
                        log_auth_failure(&state.pg, &msg).await;
                        return Err(BroodlinkError::Auth(msg));
                    }
                }
            } else {
                let msg = format!("unknown key id: {kid}");
                log_auth_failure(&state.pg, &msg).await;
                return Err(BroodlinkError::Auth(msg));
            }
        } else {
            // No kid in header — try all keys (backward compatibility)
            let mut last_err = String::new();
            let mut result = None;
            for (_, key) in &state.jwt_decoding_keys {
                match jsonwebtoken::decode::<Claims>(token, key, &state.jwt_validation) {
                    Ok(data) => {
                        result = Some(data);
                        break;
                    }
                    Err(e) => last_err = e.to_string(),
                }
            }
            match result {
                Some(data) => data,
                None => {
                    let msg = format!("invalid token: {last_err}");
                    log_auth_failure(&state.pg, &msg).await;
                    return Err(BroodlinkError::Auth(msg));
                }
            }
        }
    };

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

#[tracing::instrument(skip_all, fields(tool, agent, otel.kind = "server"))]
async fn tool_dispatch(
    State(state): State<Arc<AppState>>,
    Path(tool_name): Path<String>,
    axum::Extension(claims): axum::Extension<Claims>,
    Json(body): Json<ToolRequest>,
) -> Result<Json<ToolResponse>, BroodlinkError> {
    let agent_id = &claims.agent_id;
    tracing::Span::current().record("tool", tracing::field::display(&tool_name));
    tracing::Span::current().record("agent", tracing::field::display(agent_id));
    // Prefer OTLP trace ID for cross-service correlation; fall back to UUID
    let request_id =
        broodlink_telemetry::current_trace_id().unwrap_or_else(|| Uuid::new_v4().to_string());
    let params = &body.params;

    info!(
        tool = %tool_name,
        agent = %agent_id,
        request_id = %request_id,
        "tool invoked"
    );

    // Guardrail check — runs after auth+rate-limit, before tool execution
    check_guardrails(&state, agent_id, &tool_name, params).await?;

    // Budget check — runs after guardrails, before tool execution
    // Exempt read-only utility and budget-management tools to prevent lockout
    let budget_exempt = matches!(
        tool_name.as_str(),
        "health_check" | "ping" | "get_budget" | "set_budget" | "get_cost_map" | "get_config_info"
    );
    let tool_cost = if state.config.budget.enabled && !budget_exempt {
        check_budget(&state, agent_id, &tool_name).await?
    } else {
        0
    };

    let result = match tool_name.as_str() {
        // --- Memory (Dolt) ---
        "store_memory" => tool_store_memory(&state, agent_id, params).await,
        "recall_memory" => tool_recall_memory(&state, agent_id, params).await,
        "delete_memory" => tool_delete_memory(&state, agent_id, params).await,
        "semantic_search" => tool_semantic_search(&state, agent_id, params).await,
        "hybrid_search" => tool_hybrid_search(&state, agent_id, params).await,

        // --- Knowledge Graph (Postgres) ---
        "graph_search" => tool_graph_search(&state, agent_id, params).await,
        "graph_traverse" => tool_graph_traverse(&state, agent_id, params).await,
        "graph_update_edge" => tool_graph_update_edge(&state, agent_id, params).await,
        "graph_stats" => tool_graph_stats(&state, agent_id, params).await,
        "graph_prune" => tool_graph_prune(&state, agent_id, params).await,

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
        "decline_task" => tool_decline_task(&state, agent_id, params).await,
        "request_task_context" => tool_request_task_context(&state, agent_id, params).await,

        // --- Scheduled Tasks (Postgres) ---
        "schedule_task" => tool_schedule_task(&state, agent_id, params).await,
        "list_scheduled_tasks" => tool_list_scheduled_tasks(&state, params).await,
        "cancel_scheduled_task" => tool_cancel_scheduled_task(&state, params).await,

        // --- Notifications (Postgres + NATS) ---
        "send_notification" => tool_send_notification(&state, params).await,
        "create_notification_rule" => tool_create_notification_rule(&state, params).await,
        "list_notification_rules" => tool_list_notification_rules(&state, params).await,

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

        // --- Budget ---
        "get_budget" => tool_get_budget(&state, agent_id, params).await,
        "set_budget" => tool_set_budget(&state, agent_id, params).await,
        "get_cost_map" => tool_get_cost_map(&state, params).await,

        // --- Dead-Letter Queue ---
        "inspect_dlq" => tool_inspect_dlq(&state, params).await,
        "retry_dlq_task" => tool_retry_dlq_task(&state, params).await,
        "purge_dlq" => tool_purge_dlq(&state, params).await,

        // --- Multi-Agent Collaboration ---
        "decompose_task" => tool_decompose_task(&state, agent_id, params).await,
        "create_workspace" => tool_create_workspace(&state, agent_id, params).await,
        "workspace_read" => tool_workspace_read(&state, params).await,
        "workspace_write" => tool_workspace_write(&state, agent_id, params).await,
        "merge_results" => tool_merge_results(&state, params).await,

        // --- Chat Sessions (v0.7.0) ---
        "list_chat_sessions" => tool_list_chat_sessions(&state, params).await,
        "reply_to_chat" => tool_reply_to_chat(&state, params).await,

        // --- Formula Registry (v0.7.0) ---
        "list_formulas" => tool_list_formulas(&state, params).await,
        "get_formula" => tool_get_formula(&state, params).await,
        "create_formula" => tool_create_formula(&state, params).await,
        "update_formula" => tool_update_formula(&state, params).await,

        // --- File I/O (v0.10.0) ---
        "read_file" => tool_read_file(&state, params).await,
        "write_file" => tool_write_file(&state, params).await,
        "read_pdf" => tool_read_pdf(&state, params).await,
        "read_docx" => tool_read_docx(&state, params).await,

        "ping" => Ok(serde_json::json!({ "pong": true })),

        _ => Err(BroodlinkError::NotFound(format!(
            "unknown tool: {tool_name}"
        ))),
    };

    // Deduct budget on success (best-effort, don't fail the request)
    let success = result.is_ok();
    if success && tool_cost > 0 {
        if let Err(e) = deduct_budget(&state, agent_id, &tool_name, tool_cost, &request_id).await {
            warn!(error = %e, agent = %agent_id, tool = %tool_name, "budget deduction failed");
        }
    }

    // Audit log every tool invocation (best-effort, don't fail the request)
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

/// Fire-and-forget audit entry for authentication failures.
async fn log_auth_failure(pg: &PgPool, reason: &str) {
    let trace_id = Uuid::new_v4().to_string();
    let _ = sqlx::query(
        "INSERT INTO audit_log (trace_id, agent_id, service, operation, result_status, result_summary, created_at)
         VALUES ($1, 'unknown', 'beads-bridge', 'auth', 'error', $2, NOW())",
    )
    .bind(&trace_id)
    .bind(reason)
    .execute(pg)
    .await;
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

/// Maximum byte length for short text fields (topic, title, agent_id, etc.)
const MAX_SHORT_TEXT: usize = 512;
/// Maximum byte length for long text fields (content, description, reasoning, etc.)
const MAX_LONG_TEXT: usize = 100_000;

/// Valid entity types for the knowledge graph.
const VALID_ENTITY_TYPES: &[&str] = &[
    "person",
    "service",
    "concept",
    "location",
    "technology",
    "organization",
    "event",
    "other",
];

/// Reject URLs pointing to internal/private networks (SSRF protection).
fn validate_outbound_url(url: &str) -> Result<(), BroodlinkError> {
    let parsed = url::Url::parse(url).map_err(|_| BroodlinkError::Validation {
        field: "url".to_string(),
        message: "invalid URL".to_string(),
    })?;

    match parsed.scheme() {
        "http" | "https" => {}
        s => {
            return Err(BroodlinkError::Validation {
                field: "url".to_string(),
                message: format!("unsupported scheme '{s}'; must be http or https"),
            });
        }
    }

    let host = parsed
        .host_str()
        .ok_or_else(|| BroodlinkError::Validation {
            field: "url".to_string(),
            message: "URL must contain a host".to_string(),
        })?;

    // Block known internal hostnames
    let lower = host.to_ascii_lowercase();
    if lower == "localhost"
        || lower == "metadata.google.internal"
        || lower.ends_with(".internal")
        || lower.ends_with(".local")
    {
        return Err(BroodlinkError::Validation {
            field: "url".to_string(),
            message: "URL must not target internal hosts".to_string(),
        });
    }

    // Block private/loopback/link-local IPs
    if let Ok(ip) = host.parse::<IpAddr>() {
        let blocked = match ip {
            IpAddr::V4(v4) => {
                v4.is_loopback()
                    || v4.is_private()
                    || v4.is_link_local()
                    || v4.is_broadcast()
                    || v4.is_unspecified()
                    || v4.octets()[0] == 169 && v4.octets()[1] == 254 // link-local
                    || v4.octets()[0] == 100 && (v4.octets()[1] & 0xC0) == 64 // CGNAT 100.64/10
            }
            IpAddr::V6(v6) => v6.is_loopback() || v6.is_unspecified(),
        };
        if blocked {
            return Err(BroodlinkError::Validation {
                field: "url".to_string(),
                message: "URL must not target private/loopback addresses".to_string(),
            });
        }
    }

    Ok(())
}

/// Validate optional entity_type parameter.
fn validate_entity_type(entity_type: Option<&str>) -> Result<Option<&str>, BroodlinkError> {
    match entity_type {
        None => Ok(None),
        Some(et) => {
            if VALID_ENTITY_TYPES.contains(&et) {
                Ok(Some(et))
            } else {
                Err(BroodlinkError::Validation {
                    field: "entity_type".to_string(),
                    message: format!(
                        "invalid entity_type '{et}'; must be one of: {}",
                        VALID_ENTITY_TYPES.join(", ")
                    ),
                })
            }
        }
    }
}

fn param_str<'a>(params: &'a serde_json::Value, field: &str) -> Result<&'a str, BroodlinkError> {
    let val = params
        .get(field)
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| BroodlinkError::Validation {
            field: field.to_string(),
            message: "required string field".to_string(),
        })?;
    if val.len() > MAX_LONG_TEXT {
        return Err(BroodlinkError::Validation {
            field: field.to_string(),
            message: format!("exceeds maximum length of {MAX_LONG_TEXT} bytes"),
        });
    }
    Ok(val)
}

fn param_str_opt<'a>(params: &'a serde_json::Value, field: &str) -> Option<&'a str> {
    let val = params.get(field).and_then(serde_json::Value::as_str)?;
    if val.len() > MAX_LONG_TEXT {
        return None;
    }
    Some(val)
}

/// Extract a required string field with a short-text length limit (512 bytes).
fn param_str_short<'a>(
    params: &'a serde_json::Value,
    field: &str,
) -> Result<&'a str, BroodlinkError> {
    let val = params
        .get(field)
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| BroodlinkError::Validation {
            field: field.to_string(),
            message: "required string field".to_string(),
        })?;
    if val.len() > MAX_SHORT_TEXT {
        return Err(BroodlinkError::Validation {
            field: field.to_string(),
            message: format!("exceeds maximum length of {MAX_SHORT_TEXT} bytes"),
        });
    }
    Ok(val)
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
    let val = params
        .get(field)
        .ok_or_else(|| BroodlinkError::Validation {
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

/// Clamp a user-supplied limit to a safe range [1, 1000].
fn clamp_limit(limit: i64) -> i64 {
    limit.clamp(1, 1000)
}

fn param_f64_opt(params: &serde_json::Value, field: &str) -> Option<f64> {
    let val = params.get(field)?;
    if let Some(n) = val.as_f64() {
        return Some(n);
    }
    val.as_str().and_then(|s| s.parse::<f64>().ok())
}

fn param_bool_opt(params: &serde_json::Value, field: &str) -> Option<bool> {
    let val = params.get(field)?;
    if let Some(b) = val.as_bool() {
        return Some(b);
    }
    match val.as_str() {
        Some("true" | "1") => Some(true),
        Some("false" | "0") => Some(false),
        _ => None,
    }
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
    let topic = param_str_short(params, "topic")?;
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

    // Fetch memory_id for the Postgres search index (LAST_INSERT_ID returns 0 on UPDATE)
    let memory_id: i64 = sqlx::query_as::<_, (i64,)>(
        "SELECT id FROM agent_memory WHERE topic = ? AND agent_name = ?",
    )
    .bind(topic)
    .bind(agent_id)
    .fetch_one(&state.dolt)
    .await
    .map(|r| r.0)
    .unwrap_or(0);

    // Outbox write for embedding pipeline (includes memory_id + tags for KG extraction)
    let outbox_payload = serde_json::json!({
        "topic": topic,
        "content": content,
        "agent_name": agent_id,
        "memory_id": memory_id.to_string(),
        "tags": tags_str.unwrap_or(""),
    });
    write_outbox(&state.pg, "embed", &outbox_payload).await?;

    // UPSERT into Postgres full-text search index (non-fatal)
    if let Err(e) = sqlx::query(
        "INSERT INTO memory_search_index (memory_id, agent_id, topic, content, tags, updated_at)
         VALUES ($1, $2, $3, $4, $5, NOW())
         ON CONFLICT (agent_id, topic) DO UPDATE SET
             content = EXCLUDED.content,
             tags = EXCLUDED.tags,
             memory_id = EXCLUDED.memory_id,
             updated_at = NOW()",
    )
    .bind(memory_id)
    .bind(agent_id)
    .bind(topic)
    .bind(content)
    .bind(tags_str)
    .execute(&state.pg)
    .await
    {
        warn!(error = %e, topic = %topic, "failed to upsert memory_search_index (non-fatal)");
    }

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
    let limit = clamp_limit(param_i64_opt(params, "limit").unwrap_or(200));

    let rows = if let Some(search) = topic_search {
        let pattern = format!("%{search}%");
        sqlx::query_as::<_, (i64, String, String, String, Option<serde_json::Value>, String, String)>(
            "SELECT id, topic, content, agent_name, tags, CAST(created_at AS CHAR), CAST(updated_at AS CHAR)
             FROM agent_memory WHERE topic LIKE ? ORDER BY updated_at DESC LIMIT ?",
        )
        .bind(&pattern)
        .bind(limit)
        .fetch_all(&state.dolt)
        .await?
    } else {
        sqlx::query_as::<_, (i64, String, String, String, Option<serde_json::Value>, String, String)>(
            "SELECT id, topic, content, agent_name, tags, CAST(created_at AS CHAR), CAST(updated_at AS CHAR)
             FROM agent_memory ORDER BY updated_at DESC LIMIT ?",
        )
        .bind(limit)
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
    agent_id: &str,
    params: &serde_json::Value,
) -> Result<serde_json::Value, BroodlinkError> {
    let topic = param_str_short(params, "topic")?;

    let result = sqlx::query("DELETE FROM agent_memory WHERE topic = ?")
        .bind(topic)
        .execute(&state.dolt)
        .await?;

    // Also delete from Postgres full-text search index (non-fatal)
    if let Err(e) =
        sqlx::query("DELETE FROM memory_search_index WHERE agent_id = $1 AND topic = $2")
            .bind(agent_id)
            .bind(topic)
            .execute(&state.pg)
            .await
    {
        warn!(error = %e, topic = %topic, "failed to delete from memory_search_index (non-fatal)");
    }

    // Also delete matching vectors from Qdrant (non-fatal)
    if state.qdrant_breaker.check().is_ok() {
        let qdrant_url = format!(
            "{}/collections/{}/points/delete",
            state.config.qdrant.url, state.config.qdrant.collection,
        );
        let filter_body = serde_json::json!({
            "filter": {
                "must": [
                    { "key": "topic", "match": { "value": topic } },
                    { "key": "agent_id", "match": { "value": agent_id } }
                ]
            }
        });
        let client = reqwest::Client::new();
        match client
            .post(&qdrant_url)
            .json(&filter_body)
            .timeout(Duration::from_secs(10))
            .send()
            .await
        {
            Ok(resp) if resp.status().is_success() => {
                state.qdrant_breaker.record_success();
            }
            Ok(resp) => {
                state.qdrant_breaker.record_failure();
                warn!(status = %resp.status(), topic = %topic, "qdrant delete returned non-success (non-fatal)");
            }
            Err(e) => {
                state.qdrant_breaker.record_failure();
                warn!(error = %e, topic = %topic, "failed to delete from qdrant (non-fatal)");
            }
        }
    }

    Ok(serde_json::json!({
        "topic": topic,
        "deleted": result.rows_affected() > 0,
    }))
}

// ---------------------------------------------------------------------------
// Query expansion — generate variant phrasings before searching
// ---------------------------------------------------------------------------

/// Remove `<think>...</think>` blocks from model output (for expansion parsing).
fn strip_think_tags_bridge(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let mut rest = s;
    // Handle case where text starts inside a think block (no opening tag before closing tag)
    if let Some(close_pos) = rest.find("</think>") {
        let open_before = rest.find("<think>").map(|p| p < close_pos).unwrap_or(false);
        if !open_before {
            // Started mid-think — skip everything up to </think>
            rest = &rest[close_pos + 8..];
        }
    }
    while let Some(start) = rest.find("<think>") {
        result.push_str(&rest[..start]);
        rest = &rest[start + 7..];
        if let Some(end) = rest.find("</think>") {
            rest = &rest[end + 8..];
        } else {
            return result;
        }
    }
    result.push_str(rest);
    result
}

/// Generate 2-3 variant phrasings of a search query using a fast LLM.
/// Returns `vec![original_query]` on any error or when expansion is disabled.
async fn expand_query(
    client: &reqwest::Client,
    ollama_url: &str,
    model: &str,
    timeout_secs: u64,
    query: &str,
) -> Vec<String> {
    // Skip expansion for very short queries
    if query.split_whitespace().count() < 3 {
        return vec![query.to_string()];
    }

    let prompt = format!(
        "Generate 2-3 alternative search phrasings for the following query. \
         Return one phrasing per line, nothing else. No numbering, no explanation.\n\n\
         Query: {query}"
    );

    let url = format!("{ollama_url}/api/generate");
    let payload = serde_json::json!({
        "model": model,
        "prompt": prompt,
        "stream": false,
        "options": {
            "temperature": 0.7,
            "num_predict": 100,
        }
    });

    let resp = match client
        .post(&url)
        .json(&payload)
        .timeout(Duration::from_secs(timeout_secs))
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(error = %e, "query expansion request failed");
            return vec![query.to_string()];
        }
    };

    let body: serde_json::Value = match resp.json().await {
        Ok(b) => b,
        Err(_) => return vec![query.to_string()],
    };

    let raw = body.get("response").and_then(|r| r.as_str()).unwrap_or("");

    let cleaned = strip_think_tags_bridge(raw);

    let mut variants: Vec<String> = vec![query.to_string()];
    for line in cleaned.lines() {
        let trimmed = line.trim().trim_start_matches(|c: char| {
            c == '-' || c == '*' || c.is_ascii_digit() || c == '.' || c == ')'
        });
        let trimmed = trimmed.trim();
        if !trimmed.is_empty() && trimmed.len() > 5 && trimmed != query {
            variants.push(trimmed.to_string());
        }
    }

    // Cap at 4 total (original + 3 expansions)
    variants.truncate(4);
    variants
}

async fn tool_semantic_search(
    state: &AppState,
    _agent_id: &str,
    params: &serde_json::Value,
) -> Result<serde_json::Value, BroodlinkError> {
    state
        .qdrant_breaker
        .check()
        .map_err(BroodlinkError::CircuitOpen)?;
    state
        .ollama_breaker
        .check()
        .map_err(BroodlinkError::CircuitOpen)?;

    let query = param_str(params, "query")?;
    let limit = clamp_limit(param_i64_opt(params, "limit").unwrap_or(5));
    let agent_filter = param_str_opt(params, "agent_id");
    let date_from = param_str_opt(params, "date_from");
    let date_to = param_str_opt(params, "date_to");
    let mem_cfg = &state.config.memory_search;

    // Step 0: query expansion
    let client = reqwest::Client::new();
    let queries = if mem_cfg.query_expansion_enabled {
        expand_query(
            &client,
            &state.config.ollama.url,
            &mem_cfg.query_expansion_model,
            mem_cfg.query_expansion_timeout_seconds,
            query,
        )
        .await
    } else {
        vec![query.to_string()]
    };

    if queries.len() > 1 {
        tracing::info!(
            original = %query,
            expansions = ?&queries[1..],
            "query expansion generated {} variants",
            queries.len()
        );
    }

    // Step 1: embed all variants in parallel
    let ollama_url = format!("{}/api/embeddings", state.config.ollama.url);
    let embed_futures: Vec<_> = queries
        .iter()
        .map(|q| {
            let client = &client;
            let ollama_url = &ollama_url;
            let model = &state.config.ollama.embedding_model;
            let timeout = state.config.ollama.timeout_seconds;
            async move {
                let embed_body = serde_json::json!({
                    "model": model,
                    "prompt": q,
                });
                let resp = client
                    .post(ollama_url.as_str())
                    .json(&embed_body)
                    .timeout(Duration::from_secs(timeout))
                    .send()
                    .await
                    .ok()?;
                if !resp.status().is_success() {
                    return None;
                }
                let json: serde_json::Value = resp.json().await.ok()?;
                json.get("embedding").cloned()
            }
        })
        .collect();

    let embeddings: Vec<Option<serde_json::Value>> = futures::future::join_all(embed_futures).await;

    // Require at least the original query's embedding
    if embeddings.is_empty() || embeddings[0].is_none() {
        state.ollama_breaker.record_failure();
        return Err(BroodlinkError::Internal(
            "failed to embed original query".to_string(),
        ));
    }
    state.ollama_breaker.record_success();

    // Step 2: search Qdrant with each embedding in parallel
    let qdrant_url = format!(
        "{}/collections/{}/points/search",
        state.config.qdrant.url, state.config.qdrant.collection,
    );

    let mut must_conditions: Vec<serde_json::Value> = Vec::new();
    if let Some(ref aid) = agent_filter {
        must_conditions.push(serde_json::json!({
            "key": "agent_id",
            "match": {"value": aid}
        }));
    }
    if let Some(ref from) = date_from {
        must_conditions.push(serde_json::json!({
            "key": "created_at",
            "range": {"gte": from}
        }));
    }
    if let Some(ref to) = date_to {
        must_conditions.push(serde_json::json!({
            "key": "created_at",
            "range": {"lte": to}
        }));
    }

    let search_futures: Vec<_> = embeddings
        .iter()
        .filter_map(|e| e.as_ref())
        .map(|embedding| {
            let client = &client;
            let qdrant_url = &qdrant_url;
            let must_conditions = &must_conditions;
            async move {
                let mut qdrant_body = serde_json::json!({
                    "vector": {
                        "name": "default",
                        "vector": embedding,
                    },
                    "limit": limit,
                    "with_payload": true,
                });
                if !must_conditions.is_empty() {
                    qdrant_body["filter"] = serde_json::json!({"must": must_conditions.clone()});
                }
                let resp = client
                    .post(qdrant_url.as_str())
                    .json(&qdrant_body)
                    .timeout(Duration::from_secs(10))
                    .send()
                    .await
                    .ok()?;
                if !resp.status().is_success() {
                    return None;
                }
                let json: serde_json::Value = resp.json().await.ok()?;
                json.get("result").cloned()
            }
        })
        .collect();

    let search_results: Vec<Option<serde_json::Value>> =
        futures::future::join_all(search_futures).await;

    // Step 3: merge results by topic, taking max score per candidate
    let mut merged: std::collections::HashMap<String, serde_json::Value> =
        std::collections::HashMap::new();

    for result_opt in &search_results {
        if let Some(serde_json::Value::Array(results)) = result_opt {
            for item in results {
                let topic = item
                    .get("payload")
                    .and_then(|p| p.get("topic"))
                    .and_then(|t| t.as_str())
                    .unwrap_or("")
                    .to_string();
                let score = item.get("score").and_then(|s| s.as_f64()).unwrap_or(0.0);

                let existing_score = merged
                    .get(&topic)
                    .and_then(|v| v.get("score"))
                    .and_then(|s| s.as_f64())
                    .unwrap_or(0.0);

                if score > existing_score {
                    merged.insert(topic, item.clone());
                }
            }
        }
    }
    state.qdrant_breaker.record_success();

    // Sort by score descending and take top `limit`
    let mut sorted: Vec<serde_json::Value> = merged.into_values().collect();
    sorted.sort_by(|a, b| {
        let sa = a.get("score").and_then(|s| s.as_f64()).unwrap_or(0.0);
        let sb = b.get("score").and_then(|s| s.as_f64()).unwrap_or(0.0);
        sb.partial_cmp(&sa).unwrap_or(std::cmp::Ordering::Equal)
    });
    sorted.truncate(limit as usize);

    Ok(serde_json::json!({
        "query": query,
        "expanded_queries": queries,
        "results": sorted,
    }))
}

// ---------------------------------------------------------------------------
// Hybrid search helpers
// ---------------------------------------------------------------------------

/// Get embedding vector from Ollama. Uses the default embedding model.
async fn get_ollama_embedding(state: &AppState, text: &str) -> Result<Vec<f64>, BroodlinkError> {
    state
        .ollama_breaker
        .check()
        .map_err(BroodlinkError::CircuitOpen)?;

    let url = format!("{}/api/embeddings", state.config.ollama.url);
    let body = serde_json::json!({
        "model": state.config.ollama.embedding_model,
        "prompt": text,
    });

    let client = reqwest::Client::new();
    let resp = client
        .post(&url)
        .json(&body)
        .timeout(Duration::from_secs(state.config.ollama.timeout_seconds))
        .send()
        .await
        .map_err(|e| {
            state.ollama_breaker.record_failure();
            BroodlinkError::Internal(format!("ollama request failed: {e}"))
        })?;

    if !resp.status().is_success() {
        state.ollama_breaker.record_failure();
        return Err(BroodlinkError::Internal(format!(
            "ollama returned status {}",
            resp.status()
        )));
    }
    state.ollama_breaker.record_success();

    let json: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| BroodlinkError::Internal(format!("ollama parse: {e}")))?;

    let arr = json
        .get("embedding")
        .and_then(|v| v.as_array())
        .ok_or_else(|| {
            BroodlinkError::Internal("no embedding array in ollama response".to_string())
        })?;

    Ok(arr.iter().filter_map(|v| v.as_f64()).collect())
}

/// Run vector similarity search against Qdrant, returning (topic, agent_id, score, created_at).
async fn semantic_search_raw(
    state: &AppState,
    query_embedding: &[f64],
    limit: i64,
) -> Result<Vec<(String, String, f64, String)>, BroodlinkError> {
    state
        .qdrant_breaker
        .check()
        .map_err(BroodlinkError::CircuitOpen)?;

    let qdrant_url = format!(
        "{}/collections/{}/points/search",
        state.config.qdrant.url, state.config.qdrant.collection,
    );
    let qdrant_body = serde_json::json!({
        "vector": { "name": "default", "vector": query_embedding },
        "limit": limit,
        "with_payload": true,
    });

    let client = reqwest::Client::new();
    let resp = client
        .post(&qdrant_url)
        .json(&qdrant_body)
        .timeout(Duration::from_secs(10))
        .send()
        .await
        .map_err(|e| {
            state.qdrant_breaker.record_failure();
            BroodlinkError::Internal(format!("qdrant request failed: {e}"))
        })?;

    if !resp.status().is_success() {
        state.qdrant_breaker.record_failure();
        return Err(BroodlinkError::Internal(format!(
            "qdrant returned status {}",
            resp.status()
        )));
    }
    state.qdrant_breaker.record_success();

    let json: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| BroodlinkError::Internal(format!("qdrant parse: {e}")))?;

    let mut results = Vec::new();
    if let Some(points) = json.get("result").and_then(|r| r.as_array()) {
        for point in points {
            let score = point.get("score").and_then(|s| s.as_f64()).unwrap_or(0.0);
            let payload = point
                .get("payload")
                .cloned()
                .unwrap_or(serde_json::json!({}));
            let topic = payload
                .get("topic")
                .and_then(|t| t.as_str())
                .unwrap_or("")
                .to_string();
            let agent_id = payload
                .get("agent_id")
                .and_then(|a| a.as_str())
                .unwrap_or("")
                .to_string();
            let created_at = payload
                .get("created_at")
                .and_then(|c| c.as_str())
                .unwrap_or("")
                .to_string();

            if !topic.is_empty() {
                results.push((topic, agent_id, score, created_at));
            }
        }
    }

    Ok(results)
}

/// Run BM25 full-text search on Postgres memory_search_index.
/// Returns (memory_id, agent_id, topic, content, tags, bm25_score, created_at_text, updated_at_text).
async fn bm25_search(
    pg: &PgPool,
    query: &str,
    agent_filter: Option<&str>,
    limit: i64,
) -> Result<
    Vec<(
        i64,
        String,
        String,
        String,
        Option<String>,
        f64,
        String,
        String,
    )>,
    BroodlinkError,
> {
    let rows = sqlx::query_as::<
        _,
        (
            i64,
            String,
            String,
            String,
            Option<String>,
            f64,
            String,
            String,
        ),
    >(
        r#"SELECT memory_id, agent_id, topic, content, tags,
                  ts_rank_cd(tsv, plainto_tsquery('english', $1), 32)::float8 AS bm25_score,
                  created_at::text, updated_at::text
           FROM memory_search_index
           WHERE tsv @@ plainto_tsquery('english', $1)
             AND ($2::text IS NULL OR agent_id = $2)
           ORDER BY bm25_score DESC
           LIMIT $3"#,
    )
    .bind(query)
    .bind(agent_filter)
    .bind(limit)
    .fetch_all(pg)
    .await?;

    Ok(rows)
}

/// Min-max normalise a slice of scores to [0, 1].
fn min_max_normalize(scores: &[f64]) -> Vec<f64> {
    if scores.is_empty() {
        return vec![];
    }
    let min = scores.iter().copied().fold(f64::INFINITY, f64::min);
    let max = scores.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let range = max - min;
    if range < f64::EPSILON {
        return vec![1.0; scores.len()];
    }
    scores.iter().map(|s| (s - min) / range).collect()
}

/// Exponential temporal decay: e^(-lambda * age_days).
fn temporal_decay(updated_at: &str, lambda: f64) -> f64 {
    if lambda < f64::EPSILON {
        return 1.0; // decay disabled
    }

    let now = chrono::Utc::now();
    // Try parsing Postgres text formats
    let parsed = chrono::DateTime::parse_from_rfc3339(updated_at)
        .or_else(|_| chrono::DateTime::parse_from_str(updated_at, "%Y-%m-%d %H:%M:%S%.f%:z"))
        .or_else(|_| chrono::DateTime::parse_from_str(updated_at, "%Y-%m-%d %H:%M:%S%.f+00"))
        .map(|dt| dt.with_timezone(&chrono::Utc));

    let age_days = match parsed {
        Ok(dt) => now.signed_duration_since(dt).num_seconds() as f64 / 86400.0,
        Err(_) => 0.0, // unparseable → no decay
    };

    (-lambda * age_days.max(0.0)).exp()
}

/// Truncate content to max_len chars, appending "…" if truncated.
fn truncate_content(content: &str, max_len: usize) -> String {
    if max_len == 0 || content.len() <= max_len {
        return content.to_string();
    }
    let mut truncated = content.chars().take(max_len).collect::<String>();
    truncated.push('\u{2026}'); // …
    truncated
}

/// Cosine similarity between two vectors.
#[cfg(test)]
fn cosine_similarity(a: &[f64], b: &[f64]) -> f64 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let dot: f64 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
    let norm_a: f64 = a.iter().map(|x| x * x).sum::<f64>().sqrt();
    let norm_b: f64 = b.iter().map(|x| x * x).sum::<f64>().sqrt();
    if norm_a < f64::EPSILON || norm_b < f64::EPSILON {
        return 0.0;
    }
    dot / (norm_a * norm_b)
}

// ---------------------------------------------------------------------------
// Hybrid search tool
// ---------------------------------------------------------------------------

/// Candidate record for hybrid search fusion.
struct HybridCandidate {
    memory_id: i64,
    topic: String,
    content: String,
    agent_id: String,
    tags: Option<String>,
    created_at: String,
    updated_at: String,
    bm25_score: f64,
    vector_score: f64,
    fused_score: f64,
}

async fn tool_hybrid_search(
    state: &AppState,
    _caller_agent_id: &str,
    params: &serde_json::Value,
) -> Result<serde_json::Value, BroodlinkError> {
    let query = param_str(params, "query")?;
    let limit = clamp_limit(param_i64_opt(params, "limit").unwrap_or(10));
    let agent_filter = param_str_opt(params, "agent_id");
    let semantic_weight = param_f64_opt(params, "semantic_weight").unwrap_or(0.6);
    let keyword_weight = param_f64_opt(params, "keyword_weight").unwrap_or(0.4);
    let decay_enabled = param_bool_opt(params, "decay").unwrap_or(true);
    let rerank =
        param_bool_opt(params, "rerank").unwrap_or(state.config.memory_search.reranker_enabled);

    let mem_cfg = &state.config.memory_search;
    let lambda = mem_cfg.decay_lambda;
    let max_content_len = mem_cfg.max_content_length;
    let fetch_limit = limit * 2; // over-fetch for fusion

    // --- Step 0: query expansion ---
    let client = reqwest::Client::new();
    let queries = if mem_cfg.query_expansion_enabled {
        expand_query(
            &client,
            &state.config.ollama.url,
            &mem_cfg.query_expansion_model,
            mem_cfg.query_expansion_timeout_seconds,
            query,
        )
        .await
    } else {
        vec![query.to_string()]
    };

    if queries.len() > 1 {
        tracing::info!(
            original = %query,
            expansions = ?&queries[1..],
            "hybrid search query expansion generated {} variants",
            queries.len()
        );
    }

    // --- Run BM25 (all variants) + semantic (all variants) in parallel ---
    let qdrant_available =
        state.qdrant_breaker.check().is_ok() && state.ollama_breaker.check().is_ok();

    let bm25_fut = async {
        let futs: Vec<_> = queries
            .iter()
            .map(|q| bm25_search(&state.pg, q.as_str(), agent_filter, fetch_limit))
            .collect();
        futures::future::join_all(futs).await
    };

    let semantic_fut = async {
        if !qdrant_available {
            return vec![Ok(vec![])];
        }
        // Embed all variants in parallel
        let embed_futs: Vec<_> = queries
            .iter()
            .map(|q| get_ollama_embedding(state, q.as_str()))
            .collect();
        let embeddings = futures::future::join_all(embed_futs).await;
        // Search with each successful embedding in parallel
        let search_futs: Vec<_> = embeddings
            .into_iter()
            .filter_map(|e| e.ok())
            .map(|emb| async move { semantic_search_raw(state, &emb, fetch_limit).await })
            .collect();
        futures::future::join_all(search_futs).await
    };

    let (bm25_results, semantic_results) = tokio::join!(bm25_fut, semantic_fut);

    // Flatten BM25 results, dedup by (agent_id, topic) keeping max score
    let mut bm25_map: HashMap<(String, String), _> = HashMap::new();
    for result in bm25_results {
        for row in result.unwrap_or_else(|e| {
            warn!(error = %e, "BM25 search variant failed");
            vec![]
        }) {
            let key = (row.1.clone(), row.2.clone());
            let existing_score = bm25_map
                .get(&key)
                .map(
                    |r: &(
                        i64,
                        String,
                        String,
                        String,
                        Option<String>,
                        f64,
                        String,
                        String,
                    )| r.5,
                )
                .unwrap_or(f64::NEG_INFINITY);
            if row.5 > existing_score {
                bm25_map.insert(key, row);
            }
        }
    }
    let bm25_rows: Vec<_> = bm25_map.into_values().collect();

    // Flatten semantic results, dedup by (agent_id, topic) keeping max score
    let mut sem_map: HashMap<(String, String), _> = HashMap::new();
    for result in semantic_results {
        for (topic, aid, score, created_at) in result.unwrap_or_else(|e| {
            warn!(error = %e, "semantic search variant failed");
            vec![]
        }) {
            let key = (aid.clone(), topic.clone());
            let existing_score = sem_map
                .get(&key)
                .map(|r: &(String, String, f64, String)| r.2)
                .unwrap_or(f64::NEG_INFINITY);
            if score > existing_score {
                sem_map.insert(key, (topic, aid, score, created_at));
            }
        }
    }
    let semantic_rows: Vec<_> = sem_map.into_values().collect();

    let bm25_empty = bm25_rows.is_empty();
    let semantic_empty = semantic_rows.is_empty();

    // --- Build candidate map keyed by (agent_id, topic) ---
    let mut candidates: HashMap<(String, String), HybridCandidate> = HashMap::new();

    // Normalise BM25 scores
    let bm25_raw: Vec<f64> = bm25_rows.iter().map(|r| r.5).collect();
    let bm25_norm = min_max_normalize(&bm25_raw);

    for (i, (memory_id, aid, topic, content, tags, _raw, created_at, updated_at)) in
        bm25_rows.into_iter().enumerate()
    {
        let key = (aid.clone(), topic.clone());
        let norm_score = bm25_norm[i];
        let entry = candidates.entry(key).or_insert(HybridCandidate {
            memory_id,
            topic,
            content,
            agent_id: aid,
            tags,
            created_at,
            updated_at,
            bm25_score: 0.0,
            vector_score: 0.0,
            fused_score: 0.0,
        });
        entry.bm25_score = norm_score;
        entry.fused_score += norm_score * keyword_weight;
    }

    // Normalise semantic scores (Qdrant cosine similarity is already ~[0,1] but normalise anyway)
    let sem_raw: Vec<f64> = semantic_rows.iter().map(|r| r.2).collect();
    let sem_norm = min_max_normalize(&sem_raw);

    for (i, (topic, aid, _raw_score, created_at)) in semantic_rows.into_iter().enumerate() {
        let key = (aid.clone(), topic.clone());
        let norm_score = sem_norm[i];

        let entry = candidates.entry(key).or_insert_with(|| {
            // Content not in BM25 results — look it up later
            HybridCandidate {
                memory_id: 0,
                topic: topic.clone(),
                content: String::new(),
                agent_id: aid.clone(),
                tags: None,
                created_at: created_at.clone(),
                updated_at: created_at, // use created_at as fallback
                bm25_score: 0.0,
                vector_score: 0.0,
                fused_score: 0.0,
            }
        });
        entry.vector_score = norm_score;
        entry.fused_score += norm_score * semantic_weight;
    }

    // --- Fill missing content from Postgres search index (for semantic-only results) ---
    for candidate in candidates.values_mut() {
        if candidate.content.is_empty() {
            if let Ok(Some(row)) =
                sqlx::query_as::<_, (i64, String, Option<String>, String, String)>(
                    "SELECT memory_id, content, tags, created_at::text, updated_at::text
                 FROM memory_search_index WHERE agent_id = $1 AND topic = $2",
                )
                .bind(&candidate.agent_id)
                .bind(&candidate.topic)
                .fetch_optional(&state.pg)
                .await
            {
                candidate.memory_id = row.0;
                candidate.content = row.1;
                candidate.tags = row.2;
                candidate.created_at = row.3;
                candidate.updated_at = row.4;
            } else if let Ok(Some(row)) =
                sqlx::query_as::<_, (i64, String, Option<serde_json::Value>, String, String)>(
                    "SELECT id, content, tags, CAST(created_at AS CHAR), CAST(updated_at AS CHAR)
                 FROM agent_memory WHERE agent_name = ? AND topic = ?",
                )
                .bind(&candidate.agent_id)
                .bind(&candidate.topic)
                .fetch_optional(&state.dolt)
                .await
            {
                candidate.memory_id = row.0;
                candidate.content = row.1;
                candidate.tags = row.2.and_then(|v| v.as_str().map(String::from));
                candidate.created_at = row.3;
                candidate.updated_at = row.4;
            }
        }
    }

    // --- Apply temporal decay ---
    if decay_enabled {
        for candidate in candidates.values_mut() {
            let decay = temporal_decay(&candidate.updated_at, lambda);
            candidate.fused_score *= decay;
        }
    }

    // --- Sort by fused_score descending ---
    let mut ranked: Vec<HybridCandidate> = candidates.into_values().collect();
    ranked.sort_by(|a, b| {
        b.fused_score
            .partial_cmp(&a.fused_score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    // --- Determine method ---
    let mut method = if bm25_empty && semantic_empty {
        "none"
    } else if bm25_empty {
        "semantic_fallback"
    } else if semantic_empty {
        "bm25_fallback"
    } else {
        "hybrid"
    };

    // --- Optional reranking ---
    if rerank
        && state.config.memory_search.reranker_enabled
        && state.reranker_breaker.check().is_ok()
        && ranked.len() > 1
    {
        let client = reqwest::Client::new();
        let reranker_model = &state.config.memory_search.reranker_model;
        let ollama_url = format!("{}/api/embeddings", state.config.ollama.url);
        let mut rerank_failed = false;

        for candidate in &mut ranked {
            let input = format!(
                "query: {} document: {}: {}",
                query, candidate.topic, candidate.content
            );
            let body = serde_json::json!({
                "model": reranker_model,
                "prompt": input,
            });

            match client
                .post(&ollama_url)
                .json(&body)
                .timeout(Duration::from_secs(state.config.ollama.timeout_seconds))
                .send()
                .await
            {
                Ok(resp) if resp.status().is_success() => {
                    state.reranker_breaker.record_success();
                    if let Ok(json) = resp.json::<serde_json::Value>().await {
                        if let Some(emb) = json.get("embedding").and_then(|v| v.as_array()) {
                            // Use the magnitude of the embedding as a relevance proxy
                            let score: f64 = emb
                                .iter()
                                .filter_map(|v| v.as_f64())
                                .map(|v| v * v)
                                .sum::<f64>()
                                .sqrt();
                            candidate.fused_score = score;
                        }
                    }
                }
                Ok(_) | Err(_) => {
                    state.reranker_breaker.record_failure();
                    rerank_failed = true;
                    break;
                }
            }
        }

        if !rerank_failed {
            ranked.sort_by(|a, b| {
                b.fused_score
                    .partial_cmp(&a.fused_score)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
            method = "hybrid_reranked";
        }
    }

    // --- Truncate to limit ---
    ranked.truncate(limit as usize);

    // --- Format response ---
    let results: Vec<serde_json::Value> = ranked
        .iter()
        .map(|c| {
            let content = truncate_content(&c.content, max_content_len);
            serde_json::json!({
                "memory_id": c.memory_id,
                "agent_id": c.agent_id,
                "topic": c.topic,
                "content": content,
                "tags": c.tags,
                "score": (c.fused_score * 1000.0).round() / 1000.0,
                "bm25_score": (c.bm25_score * 1000.0).round() / 1000.0,
                "vector_score": (c.vector_score * 1000.0).round() / 1000.0,
                "created_at": c.created_at,
                "updated_at": c.updated_at,
            })
        })
        .collect();

    let total = results.len();

    Ok(serde_json::json!({
        "results": results,
        "query": query,
        "expanded_queries": queries,
        "total": total,
        "method": method,
    }))
}

// ---------------------------------------------------------------------------
// Knowledge Graph tools (Postgres: kg_entities, kg_edges)
// ---------------------------------------------------------------------------

/// Search Qdrant broodlink_kg_entities collection for entity name similarity.
/// Compute a freshness score for a KG entity based on mention count and recency.
/// Score in [0.0, 1.0]: higher = more fresh and well-referenced.
fn compute_freshness(mention_count: i32, last_seen_str: &str, decay_rate: f64) -> f64 {
    let mention_factor = (f64::from(mention_count) / 10.0).min(1.0);
    let days_since = chrono::NaiveDateTime::parse_from_str(last_seen_str, "%Y-%m-%d %H:%M:%S%.f")
        .or_else(|_| chrono::NaiveDateTime::parse_from_str(last_seen_str, "%Y-%m-%d %H:%M:%S"))
        .map(|dt| {
            let now = chrono::Utc::now().naive_utc();
            (now - dt).num_days().max(0) as f64
        })
        .unwrap_or(365.0);
    let time_factor = (-decay_rate * days_since).exp();
    // Weighted combination: 40% mention strength, 60% recency
    let score = 0.4 * mention_factor + 0.6 * time_factor;
    (score * 1000.0).round() / 1000.0 // Round to 3 decimal places
}

async fn search_kg_entities_qdrant(
    state: &AppState,
    query_embedding: &[f64],
    limit: i64,
) -> Result<Vec<(String, String, String, f64)>, BroodlinkError> {
    state
        .qdrant_breaker
        .check()
        .map_err(BroodlinkError::CircuitOpen)?;

    let qdrant_url = format!(
        "{}/collections/broodlink_kg_entities/points/search",
        state.config.qdrant.url,
    );
    let qdrant_body = serde_json::json!({
        "vector": { "name": "default", "vector": query_embedding },
        "limit": limit,
        "with_payload": true,
    });

    let client = reqwest::Client::new();
    let resp = client
        .post(&qdrant_url)
        .json(&qdrant_body)
        .timeout(Duration::from_secs(10))
        .send()
        .await
        .map_err(|e| {
            state.qdrant_breaker.record_failure();
            BroodlinkError::Internal(format!("qdrant kg request failed: {e}"))
        })?;

    if !resp.status().is_success() {
        state.qdrant_breaker.record_failure();
        return Err(BroodlinkError::Internal(format!(
            "qdrant kg returned status {}",
            resp.status()
        )));
    }
    state.qdrant_breaker.record_success();

    let json: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| BroodlinkError::Internal(format!("qdrant kg parse: {e}")))?;

    let mut results = Vec::new();
    if let Some(points) = json.get("result").and_then(|r| r.as_array()) {
        for point in points {
            let score = point.get("score").and_then(|s| s.as_f64()).unwrap_or(0.0);
            let payload = point
                .get("payload")
                .cloned()
                .unwrap_or(serde_json::json!({}));
            let entity_id = payload
                .get("entity_id")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let name = payload
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let entity_type = payload
                .get("entity_type")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            if !entity_id.is_empty() {
                results.push((entity_id, name, entity_type, score));
            }
        }
    }
    Ok(results)
}

async fn tool_graph_search(
    state: &AppState,
    _agent_id: &str,
    params: &serde_json::Value,
) -> Result<serde_json::Value, BroodlinkError> {
    let query = param_str(params, "query")?;
    let entity_type = validate_entity_type(param_str_opt(params, "entity_type"))?;
    let include_edges = param_bool_opt(params, "include_edges").unwrap_or(true);
    let limit = clamp_limit(param_i64_opt(params, "limit").unwrap_or(10));

    // 1. Text search via ILIKE
    let text_rows: Vec<(
        String,
        String,
        String,
        Option<String>,
        serde_json::Value,
        i32,
        String,
        String,
    )> = if let Some(etype) = entity_type {
        sqlx::query_as(
            "SELECT entity_id, name, entity_type, description, properties, mention_count,
                        first_seen::text, last_seen::text
                 FROM kg_entities
                 WHERE lower(name) LIKE '%' || lower($1) || '%' AND entity_type = $2
                 ORDER BY mention_count DESC LIMIT $3",
        )
        .bind(query)
        .bind(etype)
        .bind(limit)
        .fetch_all(&state.pg)
        .await?
    } else {
        sqlx::query_as(
            "SELECT entity_id, name, entity_type, description, properties, mention_count,
                        first_seen::text, last_seen::text
                 FROM kg_entities
                 WHERE lower(name) LIKE '%' || lower($1) || '%'
                 ORDER BY mention_count DESC LIMIT $2",
        )
        .bind(query)
        .bind(limit)
        .fetch_all(&state.pg)
        .await?
    };

    // 2. Also search by embedding similarity (best-effort)
    let mut seen_ids: std::collections::HashSet<String> =
        text_rows.iter().map(|r| r.0.clone()).collect();

    let decay_rate = state.config.memory_search.kg_edge_decay_rate;

    let mut entities: Vec<serde_json::Value> = text_rows
        .iter()
        .map(|(eid, name, etype, desc, props, mentions, first, last)| {
            let freshness = compute_freshness(*mentions, last, decay_rate);
            serde_json::json!({
                "entity_id": eid,
                "name": name,
                "entity_type": etype,
                "description": desc,
                "properties": props,
                "mention_count": mentions,
                "freshness_score": freshness,
                "first_seen": first,
                "last_seen": last,
            })
        })
        .collect();

    // Embedding similarity search (non-fatal if circuit open)
    if (entities.len() as i64) < limit {
        if let Ok(emb) = get_ollama_embedding(state, query).await {
            if let Ok(qdrant_results) = search_kg_entities_qdrant(state, &emb, limit).await {
                for (eid, _name, _etype, _score) in qdrant_results {
                    if seen_ids.contains(&eid) {
                        continue;
                    }
                    seen_ids.insert(eid.clone());
                    // Fetch full entity from Postgres
                    let row: Option<(String, String, String, Option<String>, serde_json::Value, i32, String, String)> =
                        sqlx::query_as(
                            "SELECT entity_id, name, entity_type, description, properties, mention_count,
                                    first_seen::text, last_seen::text
                             FROM kg_entities WHERE entity_id = $1",
                        )
                        .bind(&eid)
                        .fetch_optional(&state.pg)
                        .await?;
                    if let Some((eid2, name, etype, desc, props, mentions, first, last)) = row {
                        if entity_type.is_none() || entity_type == Some(&etype) {
                            let freshness = compute_freshness(mentions, &last, decay_rate);
                            entities.push(serde_json::json!({
                                "entity_id": eid2,
                                "name": name,
                                "entity_type": etype,
                                "description": desc,
                                "properties": props,
                                "mention_count": mentions,
                                "freshness_score": freshness,
                                "first_seen": first,
                                "last_seen": last,
                            }));
                        }
                    }
                    if entities.len() as i64 >= limit {
                        break;
                    }
                }
            }
        }
    }

    // 3. Optionally fetch edges for each entity
    if include_edges {
        for entity in entities.iter_mut() {
            let eid = entity["entity_id"].as_str().unwrap_or("");
            let edge_rows: Vec<(
                String,
                String,
                Option<String>,
                f64,
                String,
                String,
                String,
                String,
                String,
            )> = sqlx::query_as(
                "SELECT e.edge_id, e.relation_type, e.description, e.weight, e.valid_from::text,
                            CASE WHEN e.source_id = $1 THEN 'outgoing' ELSE 'incoming' END,
                            CASE WHEN e.source_id = $1 THEN t.name ELSE s.name END,
                            CASE WHEN e.source_id = $1 THEN t.entity_type ELSE s.entity_type END,
                            CASE WHEN e.source_id = $1 THEN t.entity_id ELSE s.entity_id END
                     FROM kg_edges e
                     JOIN kg_entities s ON e.source_id = s.entity_id
                     JOIN kg_entities t ON e.target_id = t.entity_id
                     WHERE (e.source_id = $1 OR e.target_id = $1) AND e.valid_to IS NULL",
            )
            .bind(eid)
            .fetch_all(&state.pg)
            .await?;

            let edges: Vec<serde_json::Value> = edge_rows
                .iter()
                .map(
                    |(
                        edge_id,
                        rel,
                        desc,
                        weight,
                        valid_from,
                        direction,
                        related_name,
                        related_type,
                        _related_id,
                    )| {
                        serde_json::json!({
                            "edge_id": edge_id,
                            "direction": direction,
                            "relation_type": rel,
                            "related_entity": related_name,
                            "related_entity_type": related_type,
                            "description": desc,
                            "weight": weight,
                            "valid_from": valid_from,
                        })
                    },
                )
                .collect();
            entity["edges"] = serde_json::json!(edges);
        }
    }

    let total = entities.len();
    Ok(serde_json::json!({
        "entities": entities,
        "query": query,
        "total": total,
    }))
}

async fn tool_graph_traverse(
    state: &AppState,
    _agent_id: &str,
    params: &serde_json::Value,
) -> Result<serde_json::Value, BroodlinkError> {
    let start_entity = param_str(params, "start_entity")?;
    let config_max = state.config.memory_search.kg_max_hops as i64;
    let max_hops = param_i64_opt(params, "max_hops")
        .unwrap_or(config_max)
        .min(config_max);
    let relation_types_str = param_str_opt(params, "relation_types");
    let direction = param_str_opt(params, "direction").unwrap_or("both");

    // Build the direction filter for the recursive CTE
    let direction_join = match direction {
        "outgoing" => "edge.source_id = t.entity_id",
        "incoming" => "edge.target_id = t.entity_id",
        _ => "(edge.source_id = t.entity_id OR edge.target_id = t.entity_id)",
    };

    let next_entity_expr = match direction {
        "outgoing" => "edge.target_id",
        "incoming" => "edge.source_id",
        _ => "CASE WHEN edge.source_id = t.entity_id THEN edge.target_id ELSE edge.source_id END",
    };

    let direction_label = match direction {
        "outgoing" => "'outgoing'",
        "incoming" => "'incoming'",
        _ => "CASE WHEN edge.source_id = t.entity_id THEN 'outgoing' ELSE 'incoming' END",
    };

    // Build optional relation_type filter (parameterized to prevent injection)
    let relation_types: Option<Vec<String>> = relation_types_str.map(|rt_str| {
        rt_str
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(String::from)
            .collect()
    });
    let has_relation_filter = relation_types.as_ref().is_some_and(|v| !v.is_empty());
    let relation_filter = if has_relation_filter {
        " AND edge.relation_type = ANY($3)"
    } else {
        ""
    };

    let sql = format!(
        "WITH RECURSIVE traversal AS (
            SELECT
                e.entity_id, e.name, e.entity_type,
                NULL::varchar AS from_entity,
                NULL::varchar AS relation_type,
                NULL::varchar AS edge_direction,
                0 AS depth,
                ARRAY[e.entity_id]::varchar[] AS path
            FROM kg_entities e
            WHERE lower(e.name) = lower($1)

            UNION ALL

            SELECT
                next_e.entity_id, next_e.name, next_e.entity_type,
                t.name AS from_entity,
                edge.relation_type,
                {direction_label}::varchar,
                t.depth + 1,
                t.path || next_e.entity_id
            FROM traversal t
            JOIN kg_edges edge ON (
                {direction_join}
                AND edge.valid_to IS NULL
                {relation_filter}
            )
            JOIN kg_entities next_e ON (
                next_e.entity_id = {next_entity_expr}
            )
            WHERE t.depth < $2
            AND NOT next_e.entity_id = ANY(t.path)
        )
        SELECT entity_id, name, entity_type, from_entity, relation_type, edge_direction, depth
        FROM traversal ORDER BY depth, name"
    );

    let rows: Vec<(
        String,
        String,
        String,
        Option<String>,
        Option<String>,
        Option<String>,
        i32,
    )> = {
        let q = sqlx::query_as(&sql).bind(start_entity).bind(max_hops);
        // Bind relation_types as $3 only when filter is active
        if has_relation_filter {
            q.bind(relation_types.unwrap_or_default())
                .fetch_all(&state.pg)
                .await?
        } else {
            q.fetch_all(&state.pg).await?
        }
    };

    let nodes: Vec<serde_json::Value> = rows
        .iter()
        .map(|(eid, name, etype, from_ent, rel_type, _dir, depth)| {
            let mut node = serde_json::json!({
                "entity_id": eid,
                "name": name,
                "type": etype,
                "depth": depth,
            });
            if let Some(from) = from_ent {
                node["from"] = serde_json::json!(from);
            }
            if let Some(rel) = rel_type {
                node["via_relation"] = serde_json::json!(rel);
            }
            node
        })
        .collect();

    let max_depth = rows.iter().map(|r| r.6).max().unwrap_or(0);

    Ok(serde_json::json!({
        "start": start_entity,
        "nodes": nodes,
        "total_nodes": nodes.len(),
        "max_depth_reached": max_depth,
    }))
}

async fn tool_graph_update_edge(
    state: &AppState,
    agent_id: &str,
    params: &serde_json::Value,
) -> Result<serde_json::Value, BroodlinkError> {
    let source_entity = param_str(params, "source_entity")?;
    let target_entity = param_str(params, "target_entity")?;
    let relation_type = param_str(params, "relation_type")?;
    let action = param_str(params, "action")?;
    let description = param_str_opt(params, "description");

    // Look up entity IDs
    let source_id: String = sqlx::query_as::<_, (String,)>(
        "SELECT entity_id FROM kg_entities WHERE lower(name) = lower($1)",
    )
    .bind(source_entity)
    .fetch_optional(&state.pg)
    .await?
    .map(|r| r.0)
    .ok_or_else(|| BroodlinkError::NotFound(format!("entity not found: {source_entity}")))?;

    let target_id: String = sqlx::query_as::<_, (String,)>(
        "SELECT entity_id FROM kg_entities WHERE lower(name) = lower($1)",
    )
    .bind(target_entity)
    .fetch_optional(&state.pg)
    .await?
    .map(|r| r.0)
    .ok_or_else(|| BroodlinkError::NotFound(format!("entity not found: {target_entity}")))?;

    // Find active edge
    let edge_id: String = sqlx::query_as::<_, (String,)>(
        "SELECT edge_id FROM kg_edges
         WHERE source_id = $1 AND target_id = $2 AND relation_type = $3 AND valid_to IS NULL",
    )
    .bind(&source_id)
    .bind(&target_id)
    .bind(relation_type)
    .fetch_optional(&state.pg)
    .await?
    .map(|r| r.0)
    .ok_or_else(|| {
        BroodlinkError::NotFound(format!(
            "no active edge: {source_entity} --{relation_type}--> {target_entity}"
        ))
    })?;

    match action {
        "invalidate" => {
            sqlx::query(
                "UPDATE kg_edges SET valid_to = NOW(), updated_at = NOW() WHERE edge_id = $1",
            )
            .bind(&edge_id)
            .execute(&state.pg)
            .await?;
        }
        "update_description" => {
            let desc = description.ok_or_else(|| BroodlinkError::Validation {
                field: "description".to_string(),
                message: "required for update_description action".to_string(),
            })?;
            sqlx::query(
                "UPDATE kg_edges SET description = $1, updated_at = NOW() WHERE edge_id = $2",
            )
            .bind(desc)
            .bind(&edge_id)
            .execute(&state.pg)
            .await?;
        }
        _ => {
            return Err(BroodlinkError::Validation {
                field: "action".to_string(),
                message: "must be 'invalidate' or 'update_description'".to_string(),
            });
        }
    }

    // Audit log
    let trace_id = Uuid::new_v4().to_string();
    let _ = write_audit_log(
        &state.pg,
        &trace_id,
        agent_id,
        SERVICE_NAME,
        &format!("graph_update_edge:{action}"),
        true,
        None,
    )
    .await;

    Ok(serde_json::json!({
        "edge_id": edge_id,
        "action": action,
        "source": source_entity,
        "target": target_entity,
        "relation_type": relation_type,
    }))
}

async fn tool_graph_stats(
    state: &AppState,
    _agent_id: &str,
    _params: &serde_json::Value,
) -> Result<serde_json::Value, BroodlinkError> {
    // Parallel queries
    let (total_entities, active_edges, historical_edges, type_rows, relation_rows, connected_rows, last_updated) = tokio::join!(
        sqlx::query_as::<_, (i64,)>("SELECT COUNT(*) FROM kg_entities")
            .fetch_one(&state.pg),
        sqlx::query_as::<_, (i64,)>("SELECT COUNT(*) FROM kg_edges WHERE valid_to IS NULL")
            .fetch_one(&state.pg),
        sqlx::query_as::<_, (i64,)>("SELECT COUNT(*) FROM kg_edges WHERE valid_to IS NOT NULL")
            .fetch_one(&state.pg),
        sqlx::query_as::<_, (String, i64)>(
            "SELECT entity_type, COUNT(*) FROM kg_entities GROUP BY entity_type ORDER BY COUNT(*) DESC"
        ).fetch_all(&state.pg),
        sqlx::query_as::<_, (String, i64)>(
            "SELECT relation_type, COUNT(*) as cnt FROM kg_edges WHERE valid_to IS NULL
             GROUP BY relation_type ORDER BY cnt DESC LIMIT 10"
        ).fetch_all(&state.pg),
        sqlx::query_as::<_, (String, String, i64)>(
            "SELECT e.name, e.entity_type, COUNT(ed.id) as edge_count
             FROM kg_entities e
             LEFT JOIN kg_edges ed ON (ed.source_id = e.entity_id OR ed.target_id = e.entity_id) AND ed.valid_to IS NULL
             GROUP BY e.entity_id, e.name, e.entity_type
             ORDER BY edge_count DESC LIMIT 10"
        ).fetch_all(&state.pg),
        sqlx::query_as::<_, (Option<String>,)>("SELECT MAX(updated_at)::text FROM kg_edges")
            .fetch_one(&state.pg),
    );

    let total_entities = total_entities.map(|r| r.0).unwrap_or(0);
    let active_edges = active_edges.map(|r| r.0).unwrap_or(0);
    let historical_edges = historical_edges.map(|r| r.0).unwrap_or(0);

    let entity_types: serde_json::Map<String, serde_json::Value> = type_rows
        .unwrap_or_default()
        .into_iter()
        .map(|(t, c)| (t, serde_json::Value::from(c)))
        .collect();

    let top_relation_types: Vec<serde_json::Value> = relation_rows
        .unwrap_or_default()
        .into_iter()
        .map(|(r, c)| serde_json::json!({"relation": r, "count": c}))
        .collect();

    let most_connected: Vec<serde_json::Value> = connected_rows
        .unwrap_or_default()
        .into_iter()
        .map(|(n, t, c)| serde_json::json!({"name": n, "type": t, "edge_count": c}))
        .collect();

    let last = last_updated.ok().and_then(|r| r.0).unwrap_or_default();

    Ok(serde_json::json!({
        "total_entities": total_entities,
        "total_active_edges": active_edges,
        "total_historical_edges": historical_edges,
        "entity_types": entity_types,
        "top_relation_types": top_relation_types,
        "most_connected_entities": most_connected,
        "last_updated": last,
    }))
}

async fn tool_graph_prune(
    state: &AppState,
    _agent_id: &str,
    params: &serde_json::Value,
) -> Result<serde_json::Value, BroodlinkError> {
    let dry_run = param_bool_opt(params, "dry_run").unwrap_or(true);
    let max_age_days = param_i64_opt(params, "max_age_days")
        .unwrap_or(i64::from(state.config.memory_search.kg_entity_ttl_days));
    let min_weight = param_f64_opt(params, "min_weight").unwrap_or(0.1);
    let min_mentions = i64::from(state.config.memory_search.kg_min_mention_count);

    let ttl_interval = format!("{max_age_days} days");

    // Preview: count what would be affected
    let stale_entities: (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM kg_entities
         WHERE last_seen < NOW() - $1::interval AND mention_count < $2",
    )
    .bind(&ttl_interval)
    .bind(min_mentions as i32)
    .fetch_one(&state.pg)
    .await?;

    let low_weight_edges: (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM kg_edges WHERE valid_to IS NULL AND weight < $1")
            .bind(min_weight)
            .fetch_one(&state.pg)
            .await?;

    let orphan_entities: (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM kg_entities e
         LEFT JOIN kg_edges ea ON (ea.source_id = e.entity_id OR ea.target_id = e.entity_id) AND ea.valid_to IS NULL
         LEFT JOIN kg_entity_memories em ON em.entity_id = e.entity_id
         WHERE ea.edge_id IS NULL AND em.entity_id IS NULL",
    )
    .fetch_one(&state.pg)
    .await?;

    if dry_run {
        return Ok(serde_json::json!({
            "dry_run": true,
            "would_prune_entities": stale_entities.0,
            "would_expire_edges": low_weight_edges.0,
            "would_remove_orphans": orphan_entities.0,
            "params": {
                "max_age_days": max_age_days,
                "min_weight": min_weight,
                "min_mentions": min_mentions,
            },
        }));
    }

    // Execute: expire low-weight edges
    let expired = sqlx::query(
        "UPDATE kg_edges SET valid_to = NOW(), updated_at = NOW()
         WHERE valid_to IS NULL AND weight < $1",
    )
    .bind(min_weight)
    .execute(&state.pg)
    .await?
    .rows_affected();

    // Delete stale entity_memories links
    sqlx::query(
        "DELETE FROM kg_entity_memories WHERE entity_id IN (
            SELECT entity_id FROM kg_entities
            WHERE last_seen < NOW() - $1::interval AND mention_count < $2
        )",
    )
    .bind(&ttl_interval)
    .bind(min_mentions as i32)
    .execute(&state.pg)
    .await?;

    // Invalidate edges to stale entities
    sqlx::query(
        "UPDATE kg_edges SET valid_to = NOW(), updated_at = NOW()
         WHERE valid_to IS NULL AND (
            source_id IN (SELECT entity_id FROM kg_entities WHERE last_seen < NOW() - $1::interval AND mention_count < $2)
            OR target_id IN (SELECT entity_id FROM kg_entities WHERE last_seen < NOW() - $1::interval AND mention_count < $2)
         )",
    )
    .bind(&ttl_interval)
    .bind(min_mentions as i32)
    .execute(&state.pg)
    .await?;

    // Delete stale entities
    let pruned = sqlx::query(
        "DELETE FROM kg_entities
         WHERE last_seen < NOW() - $1::interval AND mention_count < $2",
    )
    .bind(&ttl_interval)
    .bind(min_mentions as i32)
    .execute(&state.pg)
    .await?
    .rows_affected();

    // Remove orphans
    let orphans = sqlx::query(
        "DELETE FROM kg_entities WHERE entity_id IN (
            SELECT e.entity_id FROM kg_entities e
            LEFT JOIN kg_edges ea ON (ea.source_id = e.entity_id OR ea.target_id = e.entity_id) AND ea.valid_to IS NULL
            LEFT JOIN kg_entity_memories em ON em.entity_id = e.entity_id
            WHERE ea.edge_id IS NULL AND em.entity_id IS NULL
        )",
    )
    .execute(&state.pg)
    .await?
    .rows_affected();

    Ok(serde_json::json!({
        "dry_run": false,
        "entities_pruned": pruned,
        "edges_expired": expired,
        "orphans_removed": orphans,
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
    let limit = clamp_limit(param_i64_opt(params, "limit").unwrap_or(20));
    let agent_filter = param_str_opt(params, "agent_id");

    let rows = if let Some(agent) = agent_filter {
        sqlx::query_as::<
            _,
            (
                i64,
                String,
                String,
                String,
                Option<serde_json::Value>,
                String,
            ),
        >(
            "SELECT id, agent_id, action, details, files_changed, created_at::text
             FROM work_log WHERE agent_id = $1 ORDER BY created_at DESC LIMIT $2",
        )
        .bind(agent)
        .bind(limit)
        .fetch_all(&state.pg)
        .await?
    } else {
        sqlx::query_as::<
            _,
            (
                i64,
                String,
                String,
                String,
                Option<serde_json::Value>,
                String,
            ),
        >(
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
        sqlx::query_as::<
            _,
            (
                i64,
                String,
                String,
                Option<String>,
                Option<serde_json::Value>,
                String,
            ),
        >(
            "SELECT id, name, description, agent_name, examples, CAST(created_at AS CHAR)
             FROM skills WHERE agent_name = ? ORDER BY name",
        )
        .bind(agent)
        .fetch_all(&state.dolt)
        .await?
    } else {
        sqlx::query_as::<
            _,
            (
                i64,
                String,
                String,
                Option<String>,
                Option<serde_json::Value>,
                String,
            ),
        >(
            "SELECT id, name, description, agent_name, examples, CAST(created_at AS CHAR)
             FROM skills ORDER BY name",
        )
        .fetch_all(&state.dolt)
        .await?
    };

    let skills: Vec<serde_json::Value> = rows
        .into_iter()
        .map(
            |(id, name, description, agent_name, examples, created_at)| {
                serde_json::json!({
                    "id": id,
                    "name": name,
                    "description": description,
                    "agent_name": agent_name,
                    "examples": examples,
                    "created_at": created_at,
                })
            },
        )
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
    let agent_name = param_str_short(params, "agent_name")?;
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
    let limit = clamp_limit(param_i64_opt(params, "limit").unwrap_or(50));

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

    let result =
        sqlx::query("UPDATE beads_issues SET status = ?, updated_at = NOW() WHERE bead_id = ?")
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
    let title = param_str_short(params, "title")?;
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
    let recipient = param_str_short(params, "to")?;
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
    let limit = clamp_limit(param_i64_opt(params, "limit").unwrap_or(20));
    let unread_only = params
        .get("unread_only")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);

    let rows = if unread_only {
        sqlx::query_as::<
            _,
            (
                String,
                String,
                String,
                Option<String>,
                String,
                String,
                String,
            ),
        >(
            "SELECT id, sender, recipient, subject, body, status, created_at::text
             FROM messages WHERE recipient = $1 AND status = 'unread'
             ORDER BY created_at DESC LIMIT $2",
        )
        .bind(agent_id)
        .bind(limit)
        .fetch_all(&state.pg)
        .await?
    } else {
        sqlx::query_as::<
            _,
            (
                String,
                String,
                String,
                Option<String>,
                String,
                String,
                String,
            ),
        >(
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
    let limit = clamp_limit(param_i64_opt(params, "limit").unwrap_or(20));
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
// v0.11.0: Negotiation tools (Postgres + NATS)
// ---------------------------------------------------------------------------

async fn tool_decline_task(
    state: &AppState,
    agent_id: &str,
    params: &serde_json::Value,
) -> Result<serde_json::Value, BroodlinkError> {
    let task_id = param_str(params, "task_id")?;
    let reason = param_str_opt(params, "reason");
    let suggested_agent = param_str_opt(params, "suggested_agent");

    // Verify the task is assigned to this agent
    let affected = sqlx::query(
        "SELECT id FROM task_queue
         WHERE id = $1 AND assigned_agent = $2 AND status IN ('claimed', 'in_progress')",
    )
    .bind(task_id)
    .bind(agent_id)
    .fetch_optional(&state.pg)
    .await?;

    if affected.is_none() {
        return Err(BroodlinkError::NotFound(format!(
            "task {task_id} not claimed/in_progress for agent {agent_id}"
        )));
    }

    // Publish decline event to coordinator via NATS
    let subject = format!(
        "{}.{}.coordinator.task_declined",
        state.config.nats.subject_prefix, state.config.broodlink.env,
    );
    let nats_payload = serde_json::json!({
        "task_id": task_id,
        "agent_id": agent_id,
        "reason": reason,
        "suggested_agent": suggested_agent,
    });
    if let Ok(bytes) = serde_json::to_vec(&nats_payload) {
        if let Err(e) = state.nats.publish(subject, bytes.into()).await {
            warn!(error = %e, "failed to publish task_declined event");
        }
    }

    Ok(serde_json::json!({
        "task_id": task_id,
        "declined": true,
        "suggested_agent": suggested_agent,
    }))
}

async fn tool_request_task_context(
    state: &AppState,
    agent_id: &str,
    params: &serde_json::Value,
) -> Result<serde_json::Value, BroodlinkError> {
    let task_id = param_str(params, "task_id")?;
    let questions: Vec<String> = params
        .get("questions")
        .and_then(|v| serde_json::from_value(v.clone()).ok())
        .ok_or_else(|| BroodlinkError::Validation {
            field: "questions".to_string(),
            message: "missing or invalid 'questions' array".to_string(),
        })?;

    if questions.is_empty() {
        return Err(BroodlinkError::Validation {
            field: "questions".to_string(),
            message: "questions array must not be empty".to_string(),
        });
    }

    // Verify the task is assigned to this agent
    let row = sqlx::query(
        "SELECT id FROM task_queue
         WHERE id = $1 AND assigned_agent = $2 AND status IN ('claimed', 'in_progress')",
    )
    .bind(task_id)
    .bind(agent_id)
    .fetch_optional(&state.pg)
    .await?;

    if row.is_none() {
        return Err(BroodlinkError::NotFound(format!(
            "task {task_id} not claimed/in_progress for agent {agent_id}"
        )));
    }

    // Publish context request to coordinator via NATS
    let subject = format!(
        "{}.{}.coordinator.task_context_request",
        state.config.nats.subject_prefix, state.config.broodlink.env,
    );
    let nats_payload = serde_json::json!({
        "task_id": task_id,
        "agent_id": agent_id,
        "questions": questions,
    });
    if let Ok(bytes) = serde_json::to_vec(&nats_payload) {
        if let Err(e) = state.nats.publish(subject, bytes.into()).await {
            warn!(error = %e, "failed to publish task_context_request event");
        }
    }

    Ok(serde_json::json!({
        "task_id": task_id,
        "context_requested": true,
        "questions": questions,
    }))
}

// ---------------------------------------------------------------------------
// Scheduled task tools (Postgres: scheduled_tasks)
// ---------------------------------------------------------------------------

async fn tool_schedule_task(
    state: &AppState,
    agent_id: &str,
    params: &serde_json::Value,
) -> Result<serde_json::Value, BroodlinkError> {
    let title = param_str(params, "title")?;
    let description = param_str_opt(params, "description").unwrap_or_default();
    let priority = param_i64_opt(params, "priority").unwrap_or(0);
    let formula_name = param_str_opt(params, "formula_name");
    let run_at_str = param_str(params, "run_at")?;
    let recurrence_secs = param_i64_opt(params, "recurrence_secs");
    let max_runs = param_i64_opt(params, "max_runs");

    // Parse ISO 8601 datetime
    let run_at = chrono::DateTime::parse_from_rfc3339(run_at_str).map_err(|e| {
        BroodlinkError::Validation {
            field: "run_at".into(),
            message: format!("invalid ISO 8601 datetime: {e}"),
        }
    })?;

    let row: (i64,) = sqlx::query_as(
        "INSERT INTO scheduled_tasks (title, description, priority, formula_name, next_run_at,
                recurrence_secs, max_runs, created_by, created_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, NOW())
         RETURNING id",
    )
    .bind(title)
    .bind(description)
    .bind(priority as i32)
    .bind(formula_name)
    .bind(run_at)
    .bind(recurrence_secs)
    .bind(max_runs.map(|v| v as i32))
    .bind(agent_id)
    .fetch_one(&state.pg)
    .await?;

    Ok(serde_json::json!({
        "id": row.0,
        "title": title,
        "next_run_at": run_at.to_rfc3339(),
        "recurring": recurrence_secs.is_some(),
    }))
}

async fn tool_list_scheduled_tasks(
    state: &AppState,
    params: &serde_json::Value,
) -> Result<serde_json::Value, BroodlinkError> {
    let limit = clamp_limit(param_i64_opt(params, "limit").unwrap_or(50));

    let rows: Vec<(
        i64,
        String,
        Option<String>,
        i32,
        Option<String>,
        String,
        Option<i64>,
        i32,
        Option<i32>,
        String,
    )> = sqlx::query_as(
        "SELECT id, title, description, priority, formula_name, next_run_at::text,
                    recurrence_secs, run_count, max_runs, created_at::text
             FROM scheduled_tasks
             WHERE enabled = TRUE
             ORDER BY next_run_at ASC
             LIMIT $1",
    )
    .bind(limit)
    .fetch_all(&state.pg)
    .await?;

    let tasks: Vec<serde_json::Value> = rows
        .into_iter()
        .map(
            |(id, title, desc, priority, formula, next_run, recurrence, runs, max, created)| {
                serde_json::json!({
                    "id": id,
                    "title": title,
                    "description": desc.unwrap_or_default(),
                    "priority": priority,
                    "formula_name": formula,
                    "next_run_at": next_run,
                    "recurrence_secs": recurrence,
                    "run_count": runs,
                    "max_runs": max,
                    "created_at": created,
                })
            },
        )
        .collect();

    Ok(serde_json::json!({ "scheduled_tasks": tasks }))
}

async fn tool_cancel_scheduled_task(
    state: &AppState,
    params: &serde_json::Value,
) -> Result<serde_json::Value, BroodlinkError> {
    let id = param_i64(params, "id")?;

    let affected =
        sqlx::query("UPDATE scheduled_tasks SET enabled = FALSE WHERE id = $1 AND enabled = TRUE")
            .bind(id)
            .execute(&state.pg)
            .await?;

    if affected.rows_affected() == 0 {
        return Err(BroodlinkError::NotFound(format!(
            "scheduled task {id} not found or already disabled"
        )));
    }

    Ok(serde_json::json!({ "id": id, "cancelled": true }))
}

// ---------------------------------------------------------------------------
// Notification tools (Postgres: notification_rules, notification_log)
// ---------------------------------------------------------------------------

async fn tool_send_notification(
    state: &AppState,
    params: &serde_json::Value,
) -> Result<serde_json::Value, BroodlinkError> {
    let channel = param_str(params, "channel")?;
    let target = param_str(params, "target")?;
    let message = param_str(params, "message")?;

    if !matches!(channel, "telegram" | "slack") {
        return Err(BroodlinkError::Validation {
            field: "channel".into(),
            message: "channel must be 'telegram' or 'slack'".into(),
        });
    }

    // Insert log entry
    let row: (i64,) = sqlx::query_as(
        "INSERT INTO notification_log (channel, target, message, status, created_at)
         VALUES ($1, $2, $3, 'pending', NOW())
         RETURNING id",
    )
    .bind(channel)
    .bind(target)
    .bind(message)
    .fetch_one(&state.pg)
    .await?;

    // Publish NATS event for a2a-gateway delivery
    let subject = format!(
        "{}.{}.notification.send",
        state.config.nats.subject_prefix, state.config.broodlink.env,
    );
    let payload = serde_json::json!({
        "log_id": row.0,
        "channel": channel,
        "target": target,
        "message": message,
    });
    if let Ok(bytes) = serde_json::to_vec(&payload) {
        if let Err(e) = state.nats.publish(subject, bytes.into()).await {
            warn!(error = %e, "failed to publish notification event");
        }
    }

    Ok(serde_json::json!({
        "id": row.0,
        "channel": channel,
        "status": "pending",
    }))
}

async fn tool_create_notification_rule(
    state: &AppState,
    params: &serde_json::Value,
) -> Result<serde_json::Value, BroodlinkError> {
    let name = param_str(params, "name")?;
    let condition_type = param_str(params, "condition_type")?;
    let channel = param_str(params, "channel")?;
    let target = param_str(params, "target")?;
    let template = param_str_opt(params, "template");
    let cooldown = param_i64_opt(params, "cooldown_minutes")
        .unwrap_or(state.config.notifications.default_cooldown_minutes as i64);

    if !matches!(
        condition_type,
        "service_event_error" | "dlq_spike" | "budget_low"
    ) {
        return Err(BroodlinkError::Validation {
            field: "condition_type".into(),
            message: "must be: service_event_error, dlq_spike, or budget_low".into(),
        });
    }

    // Parse condition_config from string or use as-is
    let condition_config = params
        .get("condition_config")
        .and_then(|v| {
            if let Some(s) = v.as_str() {
                serde_json::from_str::<serde_json::Value>(s).ok()
            } else if v.is_object() {
                Some(v.clone())
            } else {
                None
            }
        })
        .unwrap_or_else(|| serde_json::json!({}));

    let row: (i64,) = sqlx::query_as(
        "INSERT INTO notification_rules (name, condition_type, condition_config, channel, target,
                template, cooldown_minutes, created_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7, NOW())
         RETURNING id",
    )
    .bind(name)
    .bind(condition_type)
    .bind(&condition_config)
    .bind(channel)
    .bind(target)
    .bind(template)
    .bind(cooldown as i32)
    .fetch_one(&state.pg)
    .await?;

    Ok(serde_json::json!({
        "id": row.0,
        "name": name,
        "created": true,
    }))
}

async fn tool_list_notification_rules(
    state: &AppState,
    params: &serde_json::Value,
) -> Result<serde_json::Value, BroodlinkError> {
    let limit = clamp_limit(param_i64_opt(params, "limit").unwrap_or(50));

    let rows: Vec<(
        i64,
        String,
        String,
        serde_json::Value,
        String,
        String,
        Option<String>,
        i32,
        bool,
        Option<String>,
        String,
    )> = sqlx::query_as(
        "SELECT id, name, condition_type, condition_config, channel, target,
                    template, cooldown_minutes, enabled, last_triggered_at::text, created_at::text
             FROM notification_rules
             ORDER BY created_at DESC
             LIMIT $1",
    )
    .bind(limit)
    .fetch_all(&state.pg)
    .await?;

    let rules: Vec<serde_json::Value> = rows
        .into_iter()
        .map(
            |(
                id,
                name,
                ctype,
                cconfig,
                channel,
                target,
                template,
                cooldown,
                enabled,
                last_triggered,
                created,
            )| {
                serde_json::json!({
                    "id": id,
                    "name": name,
                    "condition_type": ctype,
                    "condition_config": cconfig,
                    "channel": channel,
                    "target": target,
                    "template": template,
                    "cooldown_minutes": cooldown,
                    "enabled": enabled,
                    "last_triggered_at": last_triggered,
                    "created_at": created,
                })
            },
        )
        .collect();

    Ok(serde_json::json!({ "notification_rules": rules }))
}

// ---------------------------------------------------------------------------
// Agent tools (Dolt: agent_profiles)
// ---------------------------------------------------------------------------

async fn tool_agent_upsert(
    state: &AppState,
    _agent_id: &str,
    params: &serde_json::Value,
) -> Result<serde_json::Value, BroodlinkError> {
    let target_agent_id = param_str_short(params, "agent_id")?;
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

async fn tool_health_check(state: &AppState) -> Result<serde_json::Value, BroodlinkError> {
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
async fn tool_get_config_info(state: &AppState) -> Result<serde_json::Value, BroodlinkError> {
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

async fn tool_list_agents(state: &AppState) -> Result<serde_json::Value, BroodlinkError> {
    let rows = sqlx::query_as::<_, (String, String, String, String, String, Option<serde_json::Value>, bool, String, String)>(
        "SELECT agent_id, display_name, role, transport, cost_tier, capabilities, active, CAST(created_at AS CHAR), CAST(updated_at AS CHAR)
         FROM agent_profiles ORDER BY agent_id",
    )
    .fetch_all(&state.dolt)
    .await?;

    let agents: Vec<serde_json::Value> = rows
        .into_iter()
        .map(
            |(
                agent_id,
                display_name,
                role,
                transport,
                cost_tier,
                capabilities,
                active,
                created_at,
                updated_at,
            )| {
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
    let agent_id = param_str_short(params, "agent_id")?;

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
    let limit = clamp_limit(param_i64_opt(params, "limit").unwrap_or(50));
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
    let limit = clamp_limit(param_i64_opt(params, "limit").unwrap_or(50));
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
    let title = param_str_short(params, "title")?;
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
    let limit = clamp_limit(param_i64_opt(params, "limit").unwrap_or(7));

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
        .map(
            |(
                id,
                summary_date,
                summary_text,
                tasks_completed,
                decisions_made,
                memories_stored,
                created_at,
            )| {
                serde_json::json!({
                    "id": id,
                    "summary_date": summary_date,
                    "summary_text": summary_text,
                    "tasks_completed": tasks_completed,
                    "decisions_made": decisions_made,
                    "memories_stored": memories_stored,
                    "created_at": created_at,
                })
            },
        )
        .collect();

    Ok(serde_json::json!({ "summaries": summaries }))
}

async fn tool_get_commits(
    state: &AppState,
    params: &serde_json::Value,
) -> Result<serde_json::Value, BroodlinkError> {
    let limit = clamp_limit(param_i64_opt(params, "limit").unwrap_or(20));

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

async fn tool_get_memory_stats(state: &AppState) -> Result<serde_json::Value, BroodlinkError> {
    let memory_count = sqlx::query_as::<_, (i64,)>("SELECT COUNT(*) FROM agent_memory")
        .fetch_one(&state.dolt)
        .await?
        .0;

    let decision_count = sqlx::query_as::<_, (i64,)>("SELECT COUNT(*) FROM decisions")
        .fetch_one(&state.dolt)
        .await?
        .0;

    let agent_count = sqlx::query_as::<_, (i64,)>("SELECT COUNT(*) FROM agent_profiles")
        .fetch_one(&state.dolt)
        .await?
        .0;

    let outbox_pending =
        sqlx::query_as::<_, (i64,)>("SELECT COUNT(*) FROM outbox WHERE status = 'pending'")
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
        "recall_memory"
            | "semantic_search"
            | "list_agents"
            | "get_agent"
            | "get_task"
            | "list_tasks"
            | "get_audit_log"
            | "health_check"
            | "get_config_info"
            | "get_daily_summary"
            | "get_commits"
            | "get_memory_stats"
            | "list_guardrails"
            | "get_guardrail_violations"
            | "list_approvals"
            | "get_approval"
            | "list_approval_policies"
            | "get_routing_scores"
            | "get_delegation"
            | "list_delegations"
            | "get_work_log"
            | "get_decisions"
            | "get_convoy_status"
            | "beads_list_issues"
            | "beads_get_issue"
            | "beads_list_formulas"
            | "beads_get_convoy"
            | "read_messages"
            | "list_projects"
            | "list_skills"
            | "ping"
            | "graph_search"
            | "graph_traverse"
            | "graph_stats"
            | "list_chat_sessions"
            | "list_formulas"
            | "get_formula"
            | "list_scheduled_tasks"
            | "list_notification_rules"
    )
}

// ---------------------------------------------------------------------------
// Budget enforcement
// ---------------------------------------------------------------------------

/// Look up the cost for a tool call and verify the agent has sufficient budget.
/// Returns the token cost to deduct after successful execution.
async fn check_budget(
    state: &AppState,
    agent_id: &str,
    tool_name: &str,
) -> Result<i64, BroodlinkError> {
    // Look up per-tool cost, fall back to config default
    let cost: i64 =
        sqlx::query_as::<_, (i64,)>("SELECT cost_tokens FROM tool_cost_map WHERE tool_name = $1")
            .bind(tool_name)
            .fetch_optional(&state.pg)
            .await?
            .map_or(state.config.budget.default_tool_cost, |r| r.0);

    if cost == 0 {
        return Ok(0);
    }

    // Read current budget from Dolt agent_profiles
    let balance: i64 = sqlx::query_as::<_, (i64,)>(
        "SELECT COALESCE(budget_tokens, 0) FROM agent_profiles WHERE agent_id = ?",
    )
    .bind(agent_id)
    .fetch_optional(&state.dolt)
    .await?
    .map_or(0, |r| r.0);

    if balance < cost {
        return Err(BroodlinkError::BudgetExhausted {
            agent_id: agent_id.to_string(),
            balance,
            cost,
        });
    }

    Ok(cost)
}

/// Deduct tokens from agent budget after successful tool execution.
async fn deduct_budget(
    state: &AppState,
    agent_id: &str,
    tool_name: &str,
    cost: i64,
    trace_id: &str,
) -> Result<(), BroodlinkError> {
    // Deduct from Dolt agent_profiles
    sqlx::query("UPDATE agent_profiles SET budget_tokens = budget_tokens - ? WHERE agent_id = ?")
        .bind(cost)
        .bind(agent_id)
        .execute(&state.dolt)
        .await?;

    // Read new balance
    let new_balance: i64 = sqlx::query_as::<_, (i64,)>(
        "SELECT COALESCE(budget_tokens, 0) FROM agent_profiles WHERE agent_id = ?",
    )
    .bind(agent_id)
    .fetch_optional(&state.dolt)
    .await?
    .map_or(0, |r| r.0);

    // Record transaction in Postgres
    sqlx::query(
        "INSERT INTO budget_transactions (agent_id, tool_name, cost_tokens, balance_after, trace_id)
         VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(agent_id)
    .bind(tool_name)
    .bind(-cost)
    .bind(new_balance)
    .bind(trace_id)
    .execute(&state.pg)
    .await?;

    // Warn if budget is running low
    if new_balance < state.config.budget.low_budget_threshold && new_balance >= 0 {
        warn!(
            agent = %agent_id,
            balance = new_balance,
            threshold = state.config.budget.low_budget_threshold,
            "agent budget running low"
        );
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Budget tools
// ---------------------------------------------------------------------------

async fn tool_get_budget(
    state: &AppState,
    agent_id: &str,
    params: &serde_json::Value,
) -> Result<serde_json::Value, BroodlinkError> {
    let target = param_str_opt(params, "agent_id").unwrap_or(agent_id);
    let limit = clamp_limit(param_i64_opt(params, "limit").unwrap_or(10));

    // Current balance from Dolt
    let balance: i64 = sqlx::query_as::<_, (i64,)>(
        "SELECT COALESCE(budget_tokens, 0) FROM agent_profiles WHERE agent_id = ?",
    )
    .bind(target)
    .fetch_optional(&state.dolt)
    .await?
    .map_or(0, |r| r.0);

    // Recent transactions from Postgres
    let rows = sqlx::query_as::<_, (String, i64, i64, String, String)>(
        "SELECT tool_name, cost_tokens, balance_after, trace_id, created_at::text
         FROM budget_transactions
         WHERE agent_id = $1
         ORDER BY created_at DESC
         LIMIT $2",
    )
    .bind(target)
    .bind(limit)
    .fetch_all(&state.pg)
    .await?;

    let transactions: Vec<serde_json::Value> = rows
        .iter()
        .map(|(tool, cost, bal, tid, ts)| {
            serde_json::json!({
                "tool_name": tool,
                "cost_tokens": cost,
                "balance_after": bal,
                "trace_id": tid,
                "created_at": ts,
            })
        })
        .collect();

    Ok(serde_json::json!({
        "agent_id": target,
        "balance": balance,
        "budget_enabled": state.config.budget.enabled,
        "daily_replenishment": state.config.budget.daily_replenishment,
        "low_threshold": state.config.budget.low_budget_threshold,
        "transactions": transactions,
    }))
}

async fn tool_set_budget(
    state: &AppState,
    agent_id: &str,
    params: &serde_json::Value,
) -> Result<serde_json::Value, BroodlinkError> {
    let target = param_str_short(params, "agent_id")?;
    let tokens = param_i64(params, "tokens")?;

    // Read old balance
    let old_balance: i64 = sqlx::query_as::<_, (i64,)>(
        "SELECT COALESCE(budget_tokens, 0) FROM agent_profiles WHERE agent_id = ?",
    )
    .bind(target)
    .fetch_optional(&state.dolt)
    .await?
    .map_or(0, |r| r.0);

    // Update in Dolt
    sqlx::query("UPDATE agent_profiles SET budget_tokens = ? WHERE agent_id = ?")
        .bind(tokens)
        .bind(target)
        .execute(&state.dolt)
        .await?;

    // Record transaction
    let diff = tokens - old_balance;
    sqlx::query(
        "INSERT INTO budget_transactions (agent_id, tool_name, cost_tokens, balance_after, trace_id)
         VALUES ($1, 'set_budget', $2, $3, $4)",
    )
    .bind(target)
    .bind(diff)
    .bind(tokens)
    .bind(format!("set-by-{agent_id}"))
    .execute(&state.pg)
    .await?;

    Ok(serde_json::json!({
        "agent_id": target,
        "old_balance": old_balance,
        "new_balance": tokens,
        "set_by": agent_id,
    }))
}

async fn tool_get_cost_map(
    state: &AppState,
    _params: &serde_json::Value,
) -> Result<serde_json::Value, BroodlinkError> {
    let rows = sqlx::query_as::<_, (String, i64, String)>(
        "SELECT tool_name, cost_tokens, COALESCE(description, '') FROM tool_cost_map ORDER BY cost_tokens DESC",
    )
    .fetch_all(&state.pg)
    .await?;

    let costs: Vec<serde_json::Value> = rows
        .iter()
        .map(|(name, cost, desc)| {
            serde_json::json!({
                "tool_name": name,
                "cost_tokens": cost,
                "description": desc,
            })
        })
        .collect();

    Ok(serde_json::json!({
        "default_cost": state.config.budget.default_tool_cost,
        "tools": costs,
        "total": costs.len(),
    }))
}

// ---------------------------------------------------------------------------
// DLQ tools
// ---------------------------------------------------------------------------

async fn tool_inspect_dlq(
    state: &AppState,
    params: &serde_json::Value,
) -> Result<serde_json::Value, BroodlinkError> {
    let include_resolved = param_bool_opt(params, "resolved").unwrap_or(false);
    let limit = clamp_limit(param_i64_opt(params, "limit").unwrap_or(20));

    let rows = if include_resolved {
        sqlx::query_as::<_, (i64, String, String, String, i32, i32, bool, String, String)>(
            "SELECT d.id, d.task_id, d.reason, d.source_service,
                    d.retry_count, d.max_retries, d.resolved,
                    COALESCE(d.resolved_by, ''), d.created_at::text
             FROM dead_letter_queue d
             ORDER BY d.created_at DESC
             LIMIT $1",
        )
        .bind(limit)
        .fetch_all(&state.pg)
        .await?
    } else {
        sqlx::query_as::<_, (i64, String, String, String, i32, i32, bool, String, String)>(
            "SELECT d.id, d.task_id, d.reason, d.source_service,
                    d.retry_count, d.max_retries, d.resolved,
                    COALESCE(d.resolved_by, ''), d.created_at::text
             FROM dead_letter_queue d
             WHERE d.resolved = FALSE
             ORDER BY d.created_at DESC
             LIMIT $1",
        )
        .bind(limit)
        .fetch_all(&state.pg)
        .await?
    };

    let entries: Vec<serde_json::Value> = rows
        .iter()
        .map(
            |(id, task_id, reason, svc, retries, max, resolved, by, ts)| {
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
            },
        )
        .collect();

    Ok(serde_json::json!({
        "entries": entries,
        "total": entries.len(),
    }))
}

async fn tool_retry_dlq_task(
    state: &AppState,
    params: &serde_json::Value,
) -> Result<serde_json::Value, BroodlinkError> {
    let dlq_id = param_i64(params, "dlq_id")?;

    // Get the DLQ entry
    let entry = sqlx::query_as::<_, (String, i32, i32, bool)>(
        "SELECT task_id, retry_count, max_retries, resolved
         FROM dead_letter_queue WHERE id = $1",
    )
    .bind(dlq_id)
    .fetch_optional(&state.pg)
    .await?;

    let (task_id, retry_count, _max_retries, resolved) = match entry {
        Some(e) => e,
        None => return Err(BroodlinkError::NotFound(format!("DLQ entry {dlq_id}"))),
    };

    if resolved {
        return Err(BroodlinkError::Validation {
            field: "dlq_id".to_string(),
            message: "entry already resolved".to_string(),
        });
    }

    // Reset task to pending
    sqlx::query(
        "UPDATE task_queue SET status = 'pending', retry_count = $1, updated_at = NOW()
         WHERE id = $2",
    )
    .bind(retry_count + 1)
    .bind(&task_id)
    .execute(&state.pg)
    .await?;

    // Mark DLQ entry as resolved
    sqlx::query(
        "UPDATE dead_letter_queue SET resolved = TRUE, resolved_by = 'manual', updated_at = NOW()
         WHERE id = $1",
    )
    .bind(dlq_id)
    .execute(&state.pg)
    .await?;

    // Publish task_available to re-route
    let subject = format!(
        "{}.{}.coordinator.task_available",
        state.config.nats.subject_prefix, state.config.broodlink.env,
    );
    let nats_payload = serde_json::json!({
        "task_id": task_id,
        "retry": true,
        "manual": true,
    });
    if let Ok(bytes) = serde_json::to_vec(&nats_payload) {
        if let Err(e) = state.nats.publish(subject, bytes.into()).await {
            warn!(error = %e, "failed to publish manual retry event");
        }
    }

    Ok(serde_json::json!({
        "dlq_id": dlq_id,
        "task_id": task_id,
        "retried": true,
    }))
}

async fn tool_purge_dlq(
    state: &AppState,
    params: &serde_json::Value,
) -> Result<serde_json::Value, BroodlinkError> {
    let older_than_days = param_i64_opt(params, "older_than_days").unwrap_or(7);

    let result = sqlx::query(
        "DELETE FROM dead_letter_queue
         WHERE resolved = TRUE
           AND created_at < NOW() - ($1 || ' days')::interval",
    )
    .bind(format!("{older_than_days}"))
    .execute(&state.pg)
    .await?;

    Ok(serde_json::json!({
        "purged": result.rows_affected(),
        "older_than_days": older_than_days,
    }))
}

// ---------------------------------------------------------------------------
// Multi-agent collaboration tools
// ---------------------------------------------------------------------------

async fn tool_decompose_task(
    state: &AppState,
    agent_id: &str,
    params: &serde_json::Value,
) -> Result<serde_json::Value, BroodlinkError> {
    let parent_task_id = param_str(params, "parent_task_id")?;
    let sub_tasks_raw = param_str(params, "sub_tasks")?;
    let merge_strategy = param_str_opt(params, "merge_strategy").unwrap_or("concatenate");

    let sub_tasks: Vec<serde_json::Value> =
        serde_json::from_str(sub_tasks_raw).map_err(|e| BroodlinkError::Validation {
            field: "sub_tasks".to_string(),
            message: format!("invalid JSON array: {e}"),
        })?;

    let max = state.config.collaboration.max_sub_tasks as usize;
    if sub_tasks.len() > max {
        return Err(BroodlinkError::Validation {
            field: "sub_tasks".to_string(),
            message: format!("too many sub-tasks ({} > max {})", sub_tasks.len(), max),
        });
    }

    let mut child_ids = Vec::new();

    for st in &sub_tasks {
        let title = st
            .get("title")
            .and_then(|v| v.as_str())
            .unwrap_or("sub-task");
        let desc = st.get("description").and_then(|v| v.as_str()).unwrap_or("");
        let role = st.get("role").and_then(|v| v.as_str()).unwrap_or("worker");
        let cost_tier = st
            .get("cost_tier")
            .and_then(|v| v.as_str())
            .unwrap_or("low");

        let child_id = Uuid::new_v4().to_string();
        let trace_id = Uuid::new_v4().to_string();

        sqlx::query(
            "INSERT INTO task_queue (id, trace_id, title, description, status, priority, parent_task_id, created_at, updated_at)
             VALUES ($1, $2, $3, $4, 'pending', 5, $5, NOW(), NOW())",
        )
        .bind(&child_id)
        .bind(&trace_id)
        .bind(title)
        .bind(desc)
        .bind(parent_task_id)
        .execute(&state.pg)
        .await?;

        // Record decomposition
        sqlx::query(
            "INSERT INTO task_decompositions (parent_task_id, child_task_id, merge_strategy)
             VALUES ($1, $2, $3)",
        )
        .bind(parent_task_id)
        .bind(&child_id)
        .bind(merge_strategy)
        .execute(&state.pg)
        .await?;

        // Publish for routing
        let subject = format!(
            "{}.{}.coordinator.task_available",
            state.config.nats.subject_prefix, state.config.broodlink.env,
        );
        let nats_payload = serde_json::json!({
            "task_id": child_id,
            "role": role,
            "cost_tier": cost_tier,
            "parent_task_id": parent_task_id,
        });
        if let Ok(bytes) = serde_json::to_vec(&nats_payload) {
            let _ = state.nats.publish(subject, bytes.into()).await;
        }

        child_ids.push(child_id);
    }

    Ok(serde_json::json!({
        "parent_task_id": parent_task_id,
        "child_task_ids": child_ids,
        "merge_strategy": merge_strategy,
        "decomposed_by": agent_id,
        "count": child_ids.len(),
    }))
}

async fn tool_create_workspace(
    state: &AppState,
    agent_id: &str,
    params: &serde_json::Value,
) -> Result<serde_json::Value, BroodlinkError> {
    let name = param_str(params, "name")?;
    let participants_raw = param_str_opt(params, "participants").unwrap_or("[]");
    let context_raw = param_str_opt(params, "context").unwrap_or("{}");

    let participants: serde_json::Value =
        serde_json::from_str(participants_raw).unwrap_or(serde_json::json!([]));
    let context: serde_json::Value =
        serde_json::from_str(context_raw).unwrap_or(serde_json::json!({}));

    let id = Uuid::new_v4().to_string();

    sqlx::query(
        "INSERT INTO shared_workspaces (id, name, owner_agent, participants, context)
         VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(&id)
    .bind(name)
    .bind(agent_id)
    .bind(&participants)
    .bind(&context)
    .execute(&state.pg)
    .await?;

    Ok(serde_json::json!({
        "workspace_id": id,
        "name": name,
        "owner": agent_id,
        "created": true,
    }))
}

async fn tool_workspace_read(
    state: &AppState,
    params: &serde_json::Value,
) -> Result<serde_json::Value, BroodlinkError> {
    let workspace_id = param_str(params, "workspace_id")?;

    let row = sqlx::query_as::<
        _,
        (
            String,
            String,
            String,
            serde_json::Value,
            serde_json::Value,
            String,
            String,
        ),
    >(
        "SELECT id, name, owner_agent, participants, context, status, created_at::text
         FROM shared_workspaces WHERE id = $1",
    )
    .bind(workspace_id)
    .fetch_optional(&state.pg)
    .await?;

    match row {
        Some((id, name, owner, participants, context, status, created)) => Ok(serde_json::json!({
            "workspace_id": id,
            "name": name,
            "owner_agent": owner,
            "participants": participants,
            "context": context,
            "status": status,
            "created_at": created,
        })),
        None => Err(BroodlinkError::NotFound(format!(
            "workspace {workspace_id}"
        ))),
    }
}

async fn tool_workspace_write(
    state: &AppState,
    agent_id: &str,
    params: &serde_json::Value,
) -> Result<serde_json::Value, BroodlinkError> {
    let workspace_id = param_str(params, "workspace_id")?;
    let key = param_str(params, "key")?;
    let value = param_str(params, "value")?;

    // Parse value as JSON if possible, otherwise store as string
    let json_val: serde_json::Value =
        serde_json::from_str(value).unwrap_or(serde_json::Value::String(value.to_string()));

    // Update context JSONB by merging the key
    sqlx::query(
        "UPDATE shared_workspaces SET context = context || $1, updated_at = NOW()
         WHERE id = $2 AND status = 'active'",
    )
    .bind(serde_json::json!({ key: json_val }))
    .bind(workspace_id)
    .execute(&state.pg)
    .await?;

    Ok(serde_json::json!({
        "workspace_id": workspace_id,
        "key": key,
        "written_by": agent_id,
        "success": true,
    }))
}

async fn tool_merge_results(
    state: &AppState,
    params: &serde_json::Value,
) -> Result<serde_json::Value, BroodlinkError> {
    let parent_task_id = param_str(params, "parent_task_id")?;

    // Get decomposition info
    let decomps: Vec<(String, String)> = sqlx::query_as(
        "SELECT child_task_id, merge_strategy
         FROM task_decompositions
         WHERE parent_task_id = $1
         ORDER BY id",
    )
    .bind(parent_task_id)
    .fetch_all(&state.pg)
    .await?;

    if decomps.is_empty() {
        return Err(BroodlinkError::NotFound(format!(
            "no decompositions for task {parent_task_id}"
        )));
    }

    let strategy = decomps
        .first()
        .map(|(_, s)| s.as_str())
        .unwrap_or("concatenate");

    // Get child task results
    let child_ids: Vec<&str> = decomps.iter().map(|(id, _)| id.as_str()).collect();
    let mut results = Vec::new();
    let mut all_completed = true;

    for child_id in &child_ids {
        let row: Option<(String, Option<String>)> = sqlx::query_as(
            "SELECT status, COALESCE(description, '') FROM task_queue WHERE id = $1",
        )
        .bind(child_id)
        .fetch_optional(&state.pg)
        .await?;

        match row {
            Some((status, desc)) => {
                if status != "completed" {
                    all_completed = false;
                }
                results.push(serde_json::json!({
                    "task_id": child_id,
                    "status": status,
                    "result": desc,
                }));
            }
            None => {
                all_completed = false;
                results.push(serde_json::json!({
                    "task_id": child_id,
                    "status": "not_found",
                }));
            }
        }
    }

    Ok(serde_json::json!({
        "parent_task_id": parent_task_id,
        "merge_strategy": strategy,
        "all_completed": all_completed,
        "child_count": results.len(),
        "children": results,
    }))
}

// ---------------------------------------------------------------------------
// Chat session tools (v0.7.0)
// ---------------------------------------------------------------------------

async fn tool_list_chat_sessions(
    state: &AppState,
    params: &serde_json::Value,
) -> Result<serde_json::Value, BroodlinkError> {
    let platform = params.get("platform").and_then(|v| v.as_str());
    let status = params
        .get("status")
        .and_then(|v| v.as_str())
        .unwrap_or("active");
    let limit = clamp_limit(params.get("limit").and_then(|v| v.as_i64()).unwrap_or(20));

    let rows: Vec<(
        String,
        String,
        String,
        String,
        Option<String>,
        Option<String>,
        i32,
        Option<String>,
        String,
    )> = if let Some(plat) = platform {
        sqlx::query_as(
            "SELECT id, platform, channel_id, user_id, user_display_name, assigned_agent,
                        message_count, last_message_at::text, status
                 FROM chat_sessions
                 WHERE platform = $1 AND status = $2
                 ORDER BY last_message_at DESC NULLS LAST
                 LIMIT $3",
        )
        .bind(plat)
        .bind(status)
        .bind(limit)
        .fetch_all(&state.pg)
        .await?
    } else {
        sqlx::query_as(
            "SELECT id, platform, channel_id, user_id, user_display_name, assigned_agent,
                        message_count, last_message_at::text, status
                 FROM chat_sessions
                 WHERE status = $1
                 ORDER BY last_message_at DESC NULLS LAST
                 LIMIT $2",
        )
        .bind(status)
        .bind(limit)
        .fetch_all(&state.pg)
        .await?
    };

    let sessions: Vec<serde_json::Value> = rows
        .iter()
        .map(|(id, plat, ch, uid, name, agent, cnt, last, st)| {
            serde_json::json!({
                "id": id,
                "platform": plat,
                "channel_id": ch,
                "user_id": uid,
                "user_display_name": name,
                "assigned_agent": agent,
                "message_count": cnt,
                "last_message_at": last,
                "status": st,
            })
        })
        .collect();

    Ok(serde_json::json!({
        "sessions": sessions,
        "total": sessions.len(),
    }))
}

async fn tool_reply_to_chat(
    state: &AppState,
    params: &serde_json::Value,
) -> Result<serde_json::Value, BroodlinkError> {
    let session_id = param_str(params, "session_id")?;
    let content = param_str(params, "content")?;

    // Look up session to get platform and channel info
    let row: Option<(String, String, Option<String>, String)> = sqlx::query_as(
        "SELECT platform, channel_id, thread_id, status FROM chat_sessions WHERE id = $1",
    )
    .bind(session_id)
    .fetch_optional(&state.pg)
    .await?;

    let (platform, channel_id, thread_id, status) = row
        .ok_or_else(|| BroodlinkError::NotFound(format!("chat session not found: {session_id}")))?;

    if status != "active" {
        return Err(BroodlinkError::Validation {
            field: "session_id".to_string(),
            message: format!("chat session {session_id} is {status}, not active"),
        });
    }

    // Insert into reply queue
    sqlx::query(
        "INSERT INTO chat_reply_queue (session_id, task_id, content, platform, channel_id, thread_id)
         VALUES ($1, NULL, $2, $3, $4, $5)",
    )
    .bind(session_id)
    .bind(content)
    .bind(&platform)
    .bind(&channel_id)
    .bind(&thread_id)
    .execute(&state.pg)
    .await?;

    // Store as outbound message
    sqlx::query(
        "INSERT INTO chat_messages (session_id, direction, content) VALUES ($1, 'outbound', $2)",
    )
    .bind(session_id)
    .bind(content)
    .execute(&state.pg)
    .await?;

    Ok(serde_json::json!({
        "session_id": session_id,
        "status": "queued",
        "platform": platform,
    }))
}

// ---------------------------------------------------------------------------
// Formula registry tools (v0.7.0)
// ---------------------------------------------------------------------------

async fn tool_list_formulas(
    state: &AppState,
    params: &serde_json::Value,
) -> Result<serde_json::Value, BroodlinkError> {
    let enabled_only = params
        .get("enabled_only")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    let tag = params.get("tag").and_then(|v| v.as_str());

    let rows: Vec<(
        String,
        String,
        String,
        Option<String>,
        i32,
        bool,
        bool,
        i32,
        Option<String>,
        Option<String>,
    )> = if enabled_only {
        sqlx::query_as(
            "SELECT id, name, display_name, description, version, is_system, enabled,
                        usage_count, last_used_at::text, created_at::text
                 FROM formula_registry
                 WHERE enabled = true
                 ORDER BY name",
        )
        .fetch_all(&state.pg)
        .await?
    } else {
        sqlx::query_as(
            "SELECT id, name, display_name, description, version, is_system, enabled,
                        usage_count, last_used_at::text, created_at::text
                 FROM formula_registry
                 ORDER BY name",
        )
        .fetch_all(&state.pg)
        .await?
    };

    let mut formulas: Vec<serde_json::Value> = rows
        .iter()
        .map(
            |(id, name, display_name, desc, ver, system, enabled, usage, last_used, created)| {
                serde_json::json!({
                    "id": id,
                    "name": name,
                    "display_name": display_name,
                    "description": desc,
                    "version": ver,
                    "is_system": system,
                    "enabled": enabled,
                    "usage_count": usage,
                    "last_used_at": last_used,
                    "created_at": created,
                })
            },
        )
        .collect();

    // Filter by tag if provided
    if let Some(tag_filter) = tag {
        // Need to fetch tags for filtering
        let tag_rows: Vec<(String, serde_json::Value)> =
            sqlx::query_as("SELECT name, tags FROM formula_registry")
                .fetch_all(&state.pg)
                .await?;

        let names_with_tag: std::collections::HashSet<String> = tag_rows
            .iter()
            .filter(|(_, tags)| {
                tags.as_array()
                    .is_some_and(|arr| arr.iter().any(|t| t.as_str() == Some(tag_filter)))
            })
            .map(|(name, _)| name.clone())
            .collect();

        formulas.retain(|f| {
            f.get("name")
                .and_then(|n| n.as_str())
                .is_some_and(|n| names_with_tag.contains(n))
        });
    }

    Ok(serde_json::json!({
        "formulas": formulas,
        "total": formulas.len(),
    }))
}

async fn tool_get_formula(
    state: &AppState,
    params: &serde_json::Value,
) -> Result<serde_json::Value, BroodlinkError> {
    let name = param_str(params, "name")?;

    let row: Option<(
        String,
        String,
        String,
        Option<String>,
        i32,
        serde_json::Value,
        Option<String>,
        serde_json::Value,
        bool,
        bool,
        i32,
        Option<String>,
        Option<String>,
        Option<String>,
    )> = sqlx::query_as(
        "SELECT id, name, display_name, description, version, definition, author,
                    tags, is_system, enabled, usage_count, last_used_at::text,
                    created_at::text, updated_at::text
             FROM formula_registry
             WHERE name = $1",
    )
    .bind(name)
    .fetch_optional(&state.pg)
    .await?;

    let (
        id,
        nm,
        display,
        desc,
        ver,
        def,
        author,
        tags,
        system,
        enabled,
        usage,
        last_used,
        created,
        updated,
    ) = row.ok_or_else(|| BroodlinkError::NotFound(format!("formula not found: {name}")))?;

    Ok(serde_json::json!({
        "id": id,
        "name": nm,
        "display_name": display,
        "description": desc,
        "version": ver,
        "definition": def,
        "author": author,
        "tags": tags,
        "is_system": system,
        "enabled": enabled,
        "usage_count": usage,
        "last_used_at": last_used,
        "created_at": created,
        "updated_at": updated,
    }))
}

/// Validate a formula definition JSON structure.
fn validate_formula_definition(def: &serde_json::Value) -> Result<(), String> {
    let steps = def
        .get("steps")
        .and_then(|s| s.as_array())
        .ok_or("definition must have a 'steps' array")?;

    if steps.is_empty() {
        return Err("formula must have at least one step".to_string());
    }

    let mut step_names = Vec::new();
    for (i, step) in steps.iter().enumerate() {
        let name = step
            .get("name")
            .and_then(|n| n.as_str())
            .ok_or(format!("step {i} missing 'name'"))?;
        step_names.push(name.to_string());

        if step.get("agent_role").and_then(|r| r.as_str()).is_none() {
            return Err(format!("step '{name}' missing 'agent_role'"));
        }
        if step.get("prompt").and_then(|p| p.as_str()).is_none() {
            return Err(format!("step '{name}' missing 'prompt'"));
        }
    }

    // Validate parameters
    if let Some(params) = def.get("parameters").and_then(|p| p.as_array()) {
        for param in params {
            let ptype = param
                .get("type")
                .and_then(|t| t.as_str())
                .unwrap_or("string");
            if !matches!(ptype, "string" | "integer" | "boolean") {
                let pname = param.get("name").and_then(|n| n.as_str()).unwrap_or("?");
                return Err(format!(
                    "parameter '{pname}' has invalid type '{ptype}' (must be string, integer, or boolean)"
                ));
            }
        }
    }

    // Validate on_failure references an existing step name
    if let Some(on_failure) = def.get("on_failure") {
        if !on_failure.is_null() {
            if let Some(step_name) = on_failure.get("step_name").and_then(|s| s.as_str()) {
                if !step_names.iter().any(|n| n == step_name) {
                    return Err(format!("on_failure references unknown step '{step_name}'"));
                }
            }
        }
    }

    Ok(())
}

async fn tool_create_formula(
    state: &AppState,
    params: &serde_json::Value,
) -> Result<serde_json::Value, BroodlinkError> {
    let name = param_str(params, "name")?;
    let display_name = param_str(params, "display_name")?;
    let description = param_str_opt(params, "description");
    let definition_str = param_str(params, "definition")?;
    let tags_str = param_str_opt(params, "tags");

    // Guard: reject names that collide with system formulas
    let system_exists: Option<(String,)> =
        sqlx::query_as("SELECT id FROM formula_registry WHERE name = $1 AND is_system = true")
            .bind(name)
            .fetch_optional(&state.pg)
            .await?;
    if system_exists.is_some() {
        return Err(BroodlinkError::Validation {
            field: "name".to_string(),
            message: format!("name '{name}' is reserved for a system formula"),
        });
    }

    // Parse and validate definition
    let definition: serde_json::Value =
        serde_json::from_str(definition_str).map_err(|e| BroodlinkError::Validation {
            field: "definition".to_string(),
            message: format!("invalid JSON: {e}"),
        })?;

    validate_formula_definition(&definition).map_err(|msg| BroodlinkError::Validation {
        field: "definition".to_string(),
        message: msg,
    })?;

    // Parse tags
    let tags: serde_json::Value = if let Some(ts) = tags_str {
        serde_json::from_str(ts).unwrap_or(serde_json::json!([]))
    } else {
        serde_json::json!([])
    };

    let id = uuid::Uuid::new_v4().to_string();
    let def_hash = broodlink_formulas::definition_hash(&definition);

    sqlx::query(
        "INSERT INTO formula_registry (id, name, display_name, description, definition, definition_hash, tags, author)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
    )
    .bind(&id)
    .bind(name)
    .bind(display_name)
    .bind(description)
    .bind(&definition)
    .bind(&def_hash)
    .bind(&tags)
    .bind("agent")
    .execute(&state.pg)
    .await
    .map_err(|e| {
        if e.to_string().contains("duplicate key") || e.to_string().contains("unique") {
            BroodlinkError::Validation {
                field: "name".to_string(),
                message: format!("formula '{name}' already exists"),
            }
        } else {
            BroodlinkError::Database(e)
        }
    })?;

    // Write-through: persist to custom TOML (best-effort)
    let meta = broodlink_formulas::FormulaTomlMeta {
        display_name,
        description: description.unwrap_or(""),
    };
    if let Err(e) = broodlink_formulas::persist_formula_toml(
        &state.config.beads.formulas_custom_dir,
        name,
        &definition,
        &meta,
    ) {
        warn!(formula = %name, error = %e, "write-through to disk failed (create)");
    }

    Ok(serde_json::json!({
        "id": id,
        "name": name,
        "status": "created",
    }))
}

async fn tool_update_formula(
    state: &AppState,
    params: &serde_json::Value,
) -> Result<serde_json::Value, BroodlinkError> {
    let name = param_str(params, "name")?;
    let definition_str = param_str_opt(params, "definition");
    let display_name = param_str_opt(params, "display_name");
    let description = param_str_opt(params, "description");
    let enabled = params.get("enabled").and_then(|v| v.as_bool());
    let tags_str = param_str_opt(params, "tags");

    // Verify formula exists and check system flag
    let exists: Option<(String, bool)> =
        sqlx::query_as("SELECT id, is_system FROM formula_registry WHERE name = $1")
            .bind(name)
            .fetch_optional(&state.pg)
            .await?;

    let (_id, is_system) =
        exists.ok_or_else(|| BroodlinkError::NotFound(format!("formula not found: {name}")))?;

    // Guard: block modifications to system formulas (except toggling enabled)
    if is_system
        && (definition_str.is_some()
            || display_name.is_some()
            || description.is_some()
            || tags_str.is_some())
    {
        return Err(BroodlinkError::Validation {
            field: "name".to_string(),
            message: "cannot modify system formula — create a copy with a new name".to_string(),
        });
    }

    // Build dynamic update
    let mut updates = Vec::new();
    let mut bump_version = false;

    if let Some(def_str) = definition_str {
        let definition: serde_json::Value =
            serde_json::from_str(def_str).map_err(|e| BroodlinkError::Validation {
                field: "definition".to_string(),
                message: format!("invalid JSON: {e}"),
            })?;

        validate_formula_definition(&definition).map_err(|msg| BroodlinkError::Validation {
            field: "definition".to_string(),
            message: msg,
        })?;

        let def_hash = broodlink_formulas::definition_hash(&definition);
        sqlx::query(
            "UPDATE formula_registry SET definition = $1, definition_hash = $2 WHERE name = $3",
        )
        .bind(&definition)
        .bind(&def_hash)
        .bind(name)
        .execute(&state.pg)
        .await?;
        updates.push("definition");
        bump_version = true;
    }

    if let Some(dn) = display_name {
        sqlx::query("UPDATE formula_registry SET display_name = $1 WHERE name = $2")
            .bind(dn)
            .bind(name)
            .execute(&state.pg)
            .await?;
        updates.push("display_name");
    }

    if let Some(desc) = description {
        sqlx::query("UPDATE formula_registry SET description = $1 WHERE name = $2")
            .bind(desc)
            .bind(name)
            .execute(&state.pg)
            .await?;
        updates.push("description");
    }

    if let Some(en) = enabled {
        sqlx::query("UPDATE formula_registry SET enabled = $1 WHERE name = $2")
            .bind(en)
            .bind(name)
            .execute(&state.pg)
            .await?;
        updates.push("enabled");
    }

    if let Some(ts) = tags_str {
        let tags: serde_json::Value = serde_json::from_str(ts).unwrap_or(serde_json::json!([]));
        sqlx::query("UPDATE formula_registry SET tags = $1 WHERE name = $2")
            .bind(&tags)
            .bind(name)
            .execute(&state.pg)
            .await?;
        updates.push("tags");
    }

    // Bump version if definition changed
    if bump_version {
        sqlx::query("UPDATE formula_registry SET version = version + 1 WHERE name = $1")
            .bind(name)
            .execute(&state.pg)
            .await?;
    }

    // Always update timestamp
    sqlx::query("UPDATE formula_registry SET updated_at = NOW() WHERE name = $1")
        .bind(name)
        .execute(&state.pg)
        .await?;

    // Write-through: persist user formula to disk (best-effort, skip system formulas)
    if !is_system {
        let row: Option<(serde_json::Value, String, Option<String>)> = sqlx::query_as(
            "SELECT definition, display_name, description FROM formula_registry WHERE name = $1",
        )
        .bind(name)
        .fetch_optional(&state.pg)
        .await?;
        if let Some((def, dn, desc)) = row {
            let meta = broodlink_formulas::FormulaTomlMeta {
                display_name: &dn,
                description: desc.as_deref().unwrap_or(""),
            };
            if let Err(e) = broodlink_formulas::persist_formula_toml(
                &state.config.beads.formulas_custom_dir,
                name,
                &def,
                &meta,
            ) {
                warn!(formula = %name, error = %e, "write-through to disk failed (update)");
            }
        }
    }

    Ok(serde_json::json!({
        "name": name,
        "status": "updated",
        "updated_fields": updates,
        "version_bumped": bump_version,
    }))
}

// ---------------------------------------------------------------------------
// File I/O tools (v0.10.0)
// ---------------------------------------------------------------------------

async fn tool_read_file(
    state: &AppState,
    params: &serde_json::Value,
) -> Result<serde_json::Value, BroodlinkError> {
    let path = param_str(params, "path")?;
    let tool_cfg = &state.config.chat.tools;

    if !tool_cfg.file_tools_enabled {
        return Err(BroodlinkError::Validation {
            field: "path".to_string(),
            message: "file tools are disabled".to_string(),
        });
    }

    let canonical = broodlink_fs::validate_read_path(
        path,
        &tool_cfg.allowed_read_dirs,
        tool_cfg.max_read_size_bytes,
    )
    .map_err(|e| BroodlinkError::Validation {
        field: "path".to_string(),
        message: e,
    })?;

    let content = broodlink_fs::read_file_safe(&canonical, tool_cfg.max_read_size_bytes)
        .map_err(BroodlinkError::Internal)?;

    Ok(serde_json::json!({
        "path": canonical.display().to_string(),
        "content": content,
        "size_bytes": content.len(),
    }))
}

async fn tool_write_file(
    state: &AppState,
    params: &serde_json::Value,
) -> Result<serde_json::Value, BroodlinkError> {
    let path = param_str(params, "path")?;
    let content = param_str(params, "content")?;
    let tool_cfg = &state.config.chat.tools;

    if !tool_cfg.file_tools_enabled {
        return Err(BroodlinkError::Validation {
            field: "path".to_string(),
            message: "file tools are disabled".to_string(),
        });
    }

    let canonical = broodlink_fs::validate_write_path(
        path,
        &tool_cfg.allowed_write_dirs,
        content.len() as u64,
        tool_cfg.max_write_size_bytes,
    )
    .map_err(|e| BroodlinkError::Validation {
        field: "path".to_string(),
        message: e,
    })?;

    broodlink_fs::write_file_safe(
        &canonical,
        content.as_bytes(),
        tool_cfg.max_write_size_bytes,
    )
    .map_err(BroodlinkError::Internal)?;

    Ok(serde_json::json!({
        "path": canonical.display().to_string(),
        "status": "written",
        "size_bytes": content.len(),
    }))
}

async fn tool_read_pdf(
    state: &AppState,
    params: &serde_json::Value,
) -> Result<serde_json::Value, BroodlinkError> {
    let path = param_str(params, "path")?;
    let tool_cfg = &state.config.chat.tools;

    if !tool_cfg.pdf_tools_enabled {
        return Err(BroodlinkError::Validation {
            field: "path".to_string(),
            message: "PDF tools are disabled".to_string(),
        });
    }

    let canonical = broodlink_fs::validate_read_path(
        path,
        &tool_cfg.allowed_read_dirs,
        tool_cfg.max_read_size_bytes,
    )
    .map_err(|e| BroodlinkError::Validation {
        field: "path".to_string(),
        message: e,
    })?;

    let content = broodlink_fs::read_pdf_safe(&canonical, tool_cfg.max_pdf_pages)
        .map_err(BroodlinkError::Internal)?;

    Ok(serde_json::json!({
        "path": canonical.display().to_string(),
        "content": content,
        "size_bytes": content.len(),
    }))
}

async fn tool_read_docx(
    state: &AppState,
    params: &serde_json::Value,
) -> Result<serde_json::Value, BroodlinkError> {
    let path = param_str(params, "path")?;
    let tool_cfg = &state.config.chat.tools;

    if !tool_cfg.pdf_tools_enabled {
        return Err(BroodlinkError::Validation {
            field: "path".to_string(),
            message: "Document tools are disabled".to_string(),
        });
    }

    let canonical = broodlink_fs::validate_read_path(
        path,
        &tool_cfg.allowed_read_dirs,
        tool_cfg.max_read_size_bytes,
    )
    .map_err(|e| BroodlinkError::Validation {
        field: "path".to_string(),
        message: e,
    })?;

    let max_chars = tool_cfg.max_pdf_pages as usize * 3000;
    let content =
        broodlink_fs::read_docx_safe(&canonical, max_chars).map_err(BroodlinkError::Internal)?;

    Ok(serde_json::json!({
        "path": canonical.display().to_string(),
        "content": content,
        "size_bytes": content.len(),
    }))
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
    .map_err(|e| {
        error!(error = %e, "failed to load guardrail policies — denying request");
        BroodlinkError::Internal("guardrail check unavailable".to_string())
    })?;

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

                if applies_to_agent && blocked_tools.contains(&tool_name) {
                    log_guardrail_violation(
                        state,
                        agent_id,
                        policy_name,
                        tool_name,
                        "tool blocked by policy",
                    )
                    .await;
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
                    if !allowed_tools.is_empty() && !allowed_tools.contains(&tool_name) {
                        log_guardrail_violation(
                            state,
                            agent_id,
                            policy_name,
                            tool_name,
                            "tool not in scope",
                        )
                        .await;
                        return Err(BroodlinkError::Guardrail {
                            policy: policy_name.clone(),
                            message: format!(
                                "tool '{tool_name}' not in allowed scope for agent '{agent_id}'"
                            ),
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
                        // Size-limit regex to prevent ReDoS from malicious patterns
                        let re = regex::RegexBuilder::new(pat)
                            .size_limit(1 << 20) // 1 MiB compiled size limit
                            .dfa_size_limit(1 << 20)
                            .build();
                        if let Ok(re) = re {
                            if re.is_match(&params_str) {
                                log_guardrail_violation(
                                    state,
                                    agent_id,
                                    policy_name,
                                    tool_name,
                                    &format!("content matched filter pattern: {pat}"),
                                )
                                .await;
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
                    bucket.tokens =
                        (bucket.tokens + elapsed * bucket.refill_rate).min(bucket.max_tokens);
                    bucket.last_refill = now;

                    if bucket.tokens >= 1.0 {
                        bucket.tokens -= 1.0;
                    } else {
                        log_guardrail_violation(
                            state,
                            agent_id,
                            policy_name,
                            tool_name,
                            &format!("rate override exceeded: {rpm} rpm"),
                        )
                        .await;
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
             WHERE gate_type IN ('pre_dispatch', 'all') AND active = true",
        )
        .fetch_all(&state.pg)
        .await
        .map_err(|e| {
            error!(error = %e, "failed to load approval policies — denying request");
            BroodlinkError::Internal("approval policy check unavailable".to_string())
        })?;

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
                    state,
                    agent_id,
                    policy_name,
                    tool_name,
                    "tool requires approval gate",
                )
                .await;
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
    if ![
        "tool_block",
        "rate_override",
        "content_filter",
        "scope_limit",
    ]
    .contains(&rule_type)
    {
        return Err(BroodlinkError::Validation {
            field: "rule_type".to_string(),
            message: "must be one of: tool_block, rate_override, content_filter, scope_limit"
                .to_string(),
        });
    }

    let enabled = params
        .get("enabled")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);

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

    write_audit_log(
        &state.pg,
        &Uuid::new_v4().to_string(),
        agent_id,
        SERVICE_NAME,
        "set_guardrail",
        true,
        None,
    )
    .await
    .ok();

    Ok(serde_json::json!({ "upserted": true, "name": name }))
}

async fn tool_list_guardrails(
    state: &AppState,
    params: &serde_json::Value,
) -> Result<serde_json::Value, BroodlinkError> {
    let enabled_only = params
        .get("enabled_only")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let policies: Vec<(i64, String, String, serde_json::Value, bool, String, String)> =
        if enabled_only {
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
    let limit = clamp_limit(param_i64_opt(params, "limit").unwrap_or(50));
    let agent_filter = param_str_opt(params, "agent_id");

    let violations: Vec<(
        i64,
        String,
        String,
        String,
        Option<String>,
        Option<String>,
        String,
    )> = if let Some(aid) = agent_filter {
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
    if !["all", "pre_dispatch", "pre_completion", "budget", "custom"].contains(&gate_type) {
        return Err(BroodlinkError::Validation {
            field: "gate_type".to_string(),
            message: "must be one of: all, pre_dispatch, pre_completion, budget, custom".to_string(),
        });
    }

    let conditions = param_json(params, "conditions")?;
    let description = param_str_opt(params, "description");
    let auto_approve = params
        .get("auto_approve")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let threshold = params
        .get("auto_approve_threshold")
        .and_then(|v| {
            v.as_f64()
                .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
        })
        .unwrap_or(0.8);
    let expiry = params
        .get("expiry_minutes")
        .and_then(|v| {
            v.as_i64()
                .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
        })
        .unwrap_or(60) as i32;
    let active = params
        .get("active")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);

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

    write_audit_log(
        &state.pg,
        &Uuid::new_v4().to_string(),
        agent_id,
        SERVICE_NAME,
        "set_approval_policy",
        true,
        None,
    )
    .await
    .ok();

    Ok(serde_json::json!({ "upserted": true, "name": name }))
}

async fn tool_list_approval_policies(
    state: &AppState,
    params: &serde_json::Value,
) -> Result<serde_json::Value, BroodlinkError> {
    let active_only = params
        .get("active_only")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let policies: Vec<(
        String,
        String,
        Option<String>,
        String,
        serde_json::Value,
        bool,
        f64,
        i32,
        bool,
        String,
        String,
    )> = if active_only {
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
        .map(
            |(
                id,
                name,
                desc,
                gate_type,
                conditions,
                auto_approve,
                threshold,
                expiry,
                active,
                created,
                updated,
            )| {
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
            },
        )
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
    // Match policies with the exact gate_type OR with gate_type = 'all' (wildcard)
    let policies: Vec<(String, serde_json::Value, bool, f64, i32)> = sqlx::query_as(
        "SELECT id, conditions, auto_approve, auto_approve_threshold, expiry_minutes
         FROM approval_policies WHERE (gate_type = $1 OR gate_type = 'all') AND active = true ORDER BY name",
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
    let confidence_val = params.get("confidence").and_then(|v| {
        v.as_f64()
            .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
    });
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
        &state.pg,
        gate_type,
        severity,
        agent_id,
        tool_name_val,
        confidence_val,
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
        obj.insert(
            "severity".to_string(),
            serde_json::Value::String(severity.to_string()),
        );
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
        let _ = state
            .nats
            .publish(subject, b"re-queued after approval".to_vec().into())
            .await;
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
    let limit = clamp_limit(param_i64_opt(params, "limit").unwrap_or(50));
    let status_filter = param_str_opt(params, "status");

    let approvals: Vec<(
        String,
        String,
        String,
        serde_json::Value,
        Option<String>,
        String,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
    )> = if let Some(status) = status_filter {
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
        .map(
            |(
                id,
                gate_type,
                requested_by,
                payload,
                task_id,
                status,
                reviewed_by,
                reason,
                expires,
                created,
                reviewed_at,
            )| {
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
            },
        )
        .collect();

    Ok(serde_json::json!({ "approvals": list }))
}

async fn tool_get_approval(
    state: &AppState,
    params: &serde_json::Value,
) -> Result<serde_json::Value, BroodlinkError> {
    let approval_id = param_str(params, "approval_id")?;

    let row: Option<(
        String,
        String,
        String,
        serde_json::Value,
        Option<String>,
        String,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
    )> = sqlx::query_as(
        "SELECT id, gate_type, requested_by, payload, task_id, status, reviewed_by, reason,
                    expires_at::text, created_at::text, reviewed_at::text
             FROM approval_gates WHERE id = $1",
    )
    .bind(approval_id)
    .fetch_optional(&state.pg)
    .await?;

    match row {
        Some((
            id,
            gate_type,
            requested_by,
            payload,
            task_id,
            status,
            reviewed_by,
            reason,
            expires,
            created,
            reviewed_at,
        )) => Ok(serde_json::json!({
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
        })),
        None => Err(BroodlinkError::NotFound(format!(
            "approval gate {approval_id}"
        ))),
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

    // Fetch metrics from Postgres (INT→i32, REAL→f32, cast to f64 for scoring)
    let metrics: Vec<(String, i32, i32, i32, f32)> = sqlx::query_as(
        "SELECT agent_id, tasks_completed, tasks_failed, current_load, success_rate FROM agent_metrics",
    )
    .fetch_all(&state.pg)
    .await
    .map_err(|e| {
        error!(error = %e, "failed to load agent metrics");
        BroodlinkError::Database(e)
    })?;

    let metrics_map: HashMap<String, (i32, i32, i32, f64)> = metrics
        .into_iter()
        .map(|(aid, completed, failed, load, rate)| {
            (aid, (completed, failed, load, f64::from(rate)))
        })
        .collect();

    let weights = &state.config.routing.weights;
    let max_concurrent = state.config.routing.max_concurrent_default;

    let mut scored_agents: Vec<serde_json::Value> = agents
        .into_iter()
        .map(|(agent_id, role, cost_tier, _last_seen)| {
            let (completed, _failed, current_load, success_rate) = metrics_map
                .get(&agent_id)
                .copied()
                .unwrap_or((0, 0, 0, 1.0));

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
    let title = param_str_short(params, "title")?;
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
    let row: Option<(String,)> = sqlx::query_as("SELECT from_agent FROM delegations WHERE id = $1")
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
    let row: Option<(String,)> = sqlx::query_as("SELECT from_agent FROM delegations WHERE id = $1")
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
    let row: Option<(String,)> = sqlx::query_as("SELECT from_agent FROM delegations WHERE id = $1")
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
    let limit = clamp_limit(param_i64_opt(params, "limit").unwrap_or(50));
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

    let mut query = sqlx::query_as::<
        _,
        (
            String,
            Option<String>,
            String,
            String,
            String,
            String,
            Option<serde_json::Value>,
            String,
            String,
        ),
    >(&sql);
    for b in &binds {
        query = query.bind(b);
    }
    query = query.bind(limit);

    let rows = query.fetch_all(&state.pg).await?;

    let delegations: Vec<serde_json::Value> = rows
        .into_iter()
        .map(
            |(
                id,
                parent_task_id,
                from_agent,
                to_agent,
                title,
                status,
                result,
                created_at,
                updated_at,
            )| {
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
            },
        )
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
    let row: Option<(String,)> =
        sqlx::query_as("SELECT status FROM streams WHERE id = $1 AND agent_id = $2")
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
        state
            .nats
            .publish(subject, bytes.into())
            .await
            .map_err(BroodlinkError::from)?;
    }

    // Close stream on terminal events
    if event_type == "complete" || event_type == "error" {
        let _ = sqlx::query("UPDATE streams SET status = $1, closed_at = NOW() WHERE id = $2")
            .bind(if event_type == "complete" {
                "completed"
            } else {
                "failed"
            })
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
    validate_outbound_url(base_url)?;
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
    validate_outbound_url(base_url)?;
    let message_text = param_str(params, "message")?;
    let api_key = params.get("api_key").and_then(|v| v.as_str()).unwrap_or("");

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

const MAX_SSE_PER_AGENT: usize = 10;

async fn sse_stream_handler(
    State(state): State<Arc<AppState>>,
    Path(stream_id): Path<String>,
    axum::Extension(claims): axum::Extension<Claims>,
) -> Result<Sse<impl Stream<Item = Result<Event, std::convert::Infallible>>>, BroodlinkError> {
    // Enforce per-agent SSE connection limit
    {
        let conns = state.sse_connections.read().await;
        let count = conns.get(&claims.agent_id).copied().unwrap_or(0);
        if count >= MAX_SSE_PER_AGENT {
            return Err(BroodlinkError::RateLimited(claims.agent_id.clone()));
        }
    }
    {
        let mut conns = state.sse_connections.write().await;
        *conns.entry(claims.agent_id.clone()).or_insert(0) += 1;
    }

    // Verify the stream exists
    let row: Option<(String,)> = sqlx::query_as("SELECT status FROM streams WHERE id = $1")
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

            let event = Event::default().event(&event_type).data(&data);

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

    // Decrement SSE counter when stream/client disconnects
    let agent_id_cleanup = claims.agent_id.clone();
    let state_cleanup = Arc::clone(&state);
    let cleanup_stream = stream.chain(futures::stream::once(async move {
        // This runs when the upstream stream ends; decrement counter
        {
            let mut conns = state_cleanup.sse_connections.write().await;
            if let Some(count) = conns.get_mut(&agent_id_cleanup) {
                *count = count.saturating_sub(1);
                if *count == 0 {
                    conns.remove(&agent_id_cleanup);
                }
            }
        }
        // Yield a no-op event that won't actually be sent (stream is ending)
        Ok(Event::default().comment(""))
    }));

    Ok(Sse::new(cleanup_stream).keep_alive(KeepAlive::default()))
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
        assert!(
            cb.check().is_ok(),
            "check() should succeed on a closed breaker"
        );
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
        assert!(
            result.is_err(),
            "should be rate limited after exhausting burst"
        );
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
        assert_eq!(
            TOOL_REGISTRY.len(),
            96,
            "tool registry should have 96 tools"
        );
    }

    #[test]
    fn test_tool_registry_openai_format() {
        for tool in TOOL_REGISTRY.iter() {
            assert_eq!(tool["type"], "function", "tool type must be 'function'");
            let func = &tool["function"];
            assert!(func["name"].is_string(), "tool must have a name");
            assert!(
                func["description"].is_string(),
                "tool must have a description"
            );
            assert_eq!(
                func["parameters"]["type"], "object",
                "params must be object type"
            );
        }
    }

    #[test]
    fn test_tool_registry_has_known_tools() {
        let names: Vec<&str> = TOOL_REGISTRY
            .iter()
            .map(|t| t["function"]["name"].as_str().unwrap_or(""))
            .collect();
        for expected in &[
            "store_memory",
            "recall_memory",
            "semantic_search",
            "hybrid_search",
            "log_work",
            "list_projects",
            "send_message",
            "log_decision",
            "claim_task",
            "agent_upsert",
            "health_check",
            "ping",
            // v0.2.0 tools
            "set_guardrail",
            "list_guardrails",
            "get_guardrail_violations",
            "create_approval_gate",
            "resolve_approval",
            "list_approvals",
            "get_approval",
            "set_approval_policy",
            "list_approval_policies",
            "get_routing_scores",
            "delegate_task",
            "accept_delegation",
            "reject_delegation",
            "complete_delegation",
            "start_stream",
            "emit_stream_event",
            "a2a_discover",
            "a2a_delegate",
            // v0.5.0 knowledge graph tools
            "graph_search",
            "graph_traverse",
            "graph_update_edge",
            "graph_stats",
        ] {
            assert!(
                names.contains(expected),
                "registry missing tool: {expected}"
            );
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
        let ct = resp
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        assert!(
            ct.contains("application/json"),
            "error response should be JSON, got: {ct}"
        );
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
        assert!(
            req.params.is_null(),
            "missing params should default to null"
        );
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
        assert!(
            result.is_err(),
            "missing agent_id should fail deserialization"
        );
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
        assert!(policy_matches(
            &cond,
            "critical",
            "any-agent",
            Some("deploy")
        ));
    }

    #[test]
    fn test_policy_matches_tool_filter() {
        let cond = json!({"tool_names": ["store_memory", "log_work"]});
        assert!(policy_matches(&cond, "low", "claude", Some("store_memory")));
        assert!(policy_matches(&cond, "low", "claude", Some("log_work")));
        assert!(!policy_matches(&cond, "low", "claude", Some("deploy")));
        assert!(!policy_matches(&cond, "low", "claude", None));
    }

    // -----------------------------------------------------------------------
    // v0.4.0: param_f64_opt / param_bool_opt tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_param_f64_opt_from_number() {
        let params = json!({"w": 0.7});
        let val = param_f64_opt(&params, "w").unwrap();
        assert!((val - 0.7).abs() < f64::EPSILON);
    }

    #[test]
    fn test_param_f64_opt_from_string() {
        let params = json!({"w": "0.7"});
        let val = param_f64_opt(&params, "w").unwrap();
        assert!((val - 0.7).abs() < f64::EPSILON);
    }

    #[test]
    fn test_param_f64_opt_absent() {
        let params = json!({});
        assert_eq!(param_f64_opt(&params, "w"), None);
    }

    #[test]
    fn test_param_bool_opt_from_bool() {
        let params = json!({"flag": true});
        assert_eq!(param_bool_opt(&params, "flag"), Some(true));
    }

    #[test]
    fn test_param_bool_opt_from_string() {
        let params = json!({"flag": "false"});
        assert_eq!(param_bool_opt(&params, "flag"), Some(false));
    }

    #[test]
    fn test_param_bool_opt_absent() {
        let params = json!({});
        assert_eq!(param_bool_opt(&params, "flag"), None);
    }

    // -----------------------------------------------------------------------
    // v0.4.0: min_max_normalize tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_min_max_normalize_basic() {
        let scores = vec![1.0, 2.0, 3.0];
        let norm = min_max_normalize(&scores);
        assert!((norm[0] - 0.0).abs() < f64::EPSILON);
        assert!((norm[1] - 0.5).abs() < f64::EPSILON);
        assert!((norm[2] - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_min_max_normalize_single() {
        let norm = min_max_normalize(&[5.0]);
        assert_eq!(norm.len(), 1);
        assert!((norm[0] - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_min_max_normalize_equal() {
        let norm = min_max_normalize(&[3.0, 3.0, 3.0]);
        assert!(norm.iter().all(|&s| (s - 1.0).abs() < f64::EPSILON));
    }

    #[test]
    fn test_min_max_normalize_empty() {
        let norm = min_max_normalize(&[]);
        assert!(norm.is_empty());
    }

    // -----------------------------------------------------------------------
    // v0.4.0: temporal_decay tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_temporal_decay_zero_age() {
        let now = chrono::Utc::now().to_rfc3339();
        let decay = temporal_decay(&now, 0.01);
        assert!(
            (decay - 1.0).abs() < 0.02,
            "decay at zero age should be ~1.0, got {decay}"
        );
    }

    #[test]
    fn test_temporal_decay_30_days() {
        let thirty_days_ago = (chrono::Utc::now() - chrono::Duration::days(30)).to_rfc3339();
        let decay = temporal_decay(&thirty_days_ago, 0.01);
        let expected = (-0.01_f64 * 30.0).exp(); // ~0.7408
        assert!(
            (decay - expected).abs() < 0.02,
            "decay at 30 days should be ~{expected}, got {decay}"
        );
    }

    #[test]
    fn test_temporal_decay_lambda_zero() {
        let old = (chrono::Utc::now() - chrono::Duration::days(365)).to_rfc3339();
        let decay = temporal_decay(&old, 0.0);
        assert!(
            (decay - 1.0).abs() < f64::EPSILON,
            "lambda=0 should disable decay"
        );
    }

    // -----------------------------------------------------------------------
    // v0.4.0: truncate_content tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_truncate_content_under_limit() {
        let result = truncate_content("short text", 100);
        assert_eq!(result, "short text");
    }

    #[test]
    fn test_truncate_content_over_limit() {
        let result = truncate_content("hello world", 5);
        assert_eq!(result, "hello\u{2026}");
    }

    #[test]
    fn test_truncate_content_zero_limit() {
        let result = truncate_content("no truncation", 0);
        assert_eq!(result, "no truncation");
    }

    // -----------------------------------------------------------------------
    // v0.4.0: cosine_similarity tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_cosine_similarity_identical() {
        let a = vec![1.0, 0.0, 1.0];
        let sim = cosine_similarity(&a, &a);
        assert!(
            (sim - 1.0).abs() < 1e-10,
            "identical vectors should have similarity 1.0"
        );
    }

    #[test]
    fn test_cosine_similarity_orthogonal() {
        let a = vec![1.0, 0.0];
        let b = vec![0.0, 1.0];
        let sim = cosine_similarity(&a, &b);
        assert!(
            sim.abs() < 1e-10,
            "orthogonal vectors should have similarity 0.0"
        );
    }

    #[test]
    fn test_cosine_similarity_opposite() {
        let a = vec![1.0, 0.0];
        let b = vec![-1.0, 0.0];
        let sim = cosine_similarity(&a, &b);
        assert!(
            (sim - (-1.0)).abs() < 1e-10,
            "opposite vectors should have similarity -1.0"
        );
    }

    // -----------------------------------------------------------------------
    // v0.5.0: Knowledge Graph tool registry schema tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_graph_search_tool_schema() {
        let tool = TOOL_REGISTRY
            .iter()
            .find(|t| t["function"]["name"] == "graph_search")
            .expect("graph_search must exist in registry");

        let params = &tool["function"]["parameters"];
        assert_eq!(params["type"], "object");

        // Required params
        let required = params["required"].as_array().unwrap();
        assert!(required.contains(&json!("query")), "query must be required");
        assert!(
            !required.contains(&json!("entity_type")),
            "entity_type must be optional"
        );
        assert!(
            !required.contains(&json!("include_edges")),
            "include_edges must be optional"
        );
        assert!(
            !required.contains(&json!("limit")),
            "limit must be optional"
        );

        // Properties exist
        let props = params["properties"].as_object().unwrap();
        assert!(props.contains_key("query"));
        assert!(props.contains_key("entity_type"));
        assert!(props.contains_key("include_edges"));
        assert!(props.contains_key("limit"));
    }

    #[test]
    fn test_graph_traverse_tool_schema() {
        let tool = TOOL_REGISTRY
            .iter()
            .find(|t| t["function"]["name"] == "graph_traverse")
            .expect("graph_traverse must exist in registry");

        let params = &tool["function"]["parameters"];
        let required = params["required"].as_array().unwrap();
        assert!(
            required.contains(&json!("start_entity")),
            "start_entity must be required"
        );
        assert_eq!(required.len(), 1, "only start_entity should be required");

        let props = params["properties"].as_object().unwrap();
        assert!(props.contains_key("start_entity"));
        assert!(props.contains_key("max_hops"));
        assert!(props.contains_key("relation_types"));
        assert!(props.contains_key("direction"));
    }

    #[test]
    fn test_graph_update_edge_tool_schema() {
        let tool = TOOL_REGISTRY
            .iter()
            .find(|t| t["function"]["name"] == "graph_update_edge")
            .expect("graph_update_edge must exist in registry");

        let params = &tool["function"]["parameters"];
        let required: Vec<&str> = params["required"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();

        assert!(required.contains(&"source_entity"));
        assert!(required.contains(&"target_entity"));
        assert!(required.contains(&"relation_type"));
        assert!(required.contains(&"action"));
        assert!(
            !required.contains(&"description"),
            "description must be optional"
        );

        let props = params["properties"].as_object().unwrap();
        assert!(
            props.contains_key("description"),
            "description property must exist"
        );
    }

    #[test]
    fn test_graph_stats_tool_schema() {
        let tool = TOOL_REGISTRY
            .iter()
            .find(|t| t["function"]["name"] == "graph_stats")
            .expect("graph_stats must exist in registry");

        let params = &tool["function"]["parameters"];
        assert_eq!(params["type"], "object");

        // No required params
        let required = params.get("required").and_then(|v| v.as_array());
        assert!(
            required.is_none_or(|r| r.is_empty()),
            "graph_stats should have no required params"
        );
    }

    // -----------------------------------------------------------------------
    // v0.5.0: is_readonly_tool coverage for KG tools
    // -----------------------------------------------------------------------

    #[test]
    fn test_kg_readonly_tools() {
        assert!(
            is_readonly_tool("graph_search"),
            "graph_search should be readonly"
        );
        assert!(
            is_readonly_tool("graph_traverse"),
            "graph_traverse should be readonly"
        );
        assert!(
            is_readonly_tool("graph_stats"),
            "graph_stats should be readonly"
        );
    }

    #[test]
    fn test_kg_write_tools_not_readonly() {
        assert!(
            !is_readonly_tool("graph_update_edge"),
            "graph_update_edge should NOT be readonly"
        );
    }

    // -----------------------------------------------------------------------
    // v0.5.0: KG tool parameter validation tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_graph_search_requires_query() {
        let params = json!({});
        let result = param_str(&params, "query");
        assert!(result.is_err(), "graph_search should reject missing query");
    }

    #[test]
    fn test_graph_search_optional_defaults() {
        let params = json!({"query": "test"});
        // include_edges defaults to true
        assert_eq!(param_bool_opt(&params, "include_edges"), None);
        // limit defaults via unwrap_or(10)
        assert_eq!(param_i64_opt(&params, "limit"), None);
        // entity_type is optional
        assert_eq!(param_str_opt(&params, "entity_type"), None);
    }

    #[test]
    fn test_graph_traverse_requires_start_entity() {
        let params = json!({});
        let result = param_str(&params, "start_entity");
        assert!(
            result.is_err(),
            "graph_traverse should reject missing start_entity"
        );
    }

    #[test]
    fn test_graph_traverse_direction_defaults() {
        let params = json!({"start_entity": "alice"});
        let direction = param_str_opt(&params, "direction").unwrap_or("both");
        assert_eq!(direction, "both", "direction should default to 'both'");
    }

    #[test]
    fn test_graph_traverse_max_hops_capping() {
        // Simulates the capping logic from tool_graph_traverse
        let config_max: i64 = 3;
        let user_hops: i64 = 10;
        let effective = user_hops.min(config_max);
        assert_eq!(effective, 3, "max_hops should be capped by config");

        let user_hops: i64 = 2;
        let effective = user_hops.min(config_max);
        assert_eq!(effective, 2, "max_hops below config should be preserved");
    }

    #[test]
    fn test_graph_traverse_relation_filter_parsing() {
        // Simulates the relation_types parsing logic from tool_graph_traverse
        let rt_str = "DEPENDS_ON, RUNS_ON, MANAGES";
        let types: Vec<&str> = rt_str
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .collect();
        assert_eq!(types, vec!["DEPENDS_ON", "RUNS_ON", "MANAGES"]);

        let quoted: Vec<String> = types
            .iter()
            .map(|t| format!("'{}'", t.replace('\'', "''")))
            .collect();
        let filter = format!(" AND edge.relation_type IN ({})", quoted.join(", "));
        assert_eq!(
            filter,
            " AND edge.relation_type IN ('DEPENDS_ON', 'RUNS_ON', 'MANAGES')"
        );
    }

    #[test]
    fn test_graph_traverse_empty_relation_filter() {
        let rt_str = "";
        let types: Vec<&str> = rt_str
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .collect();
        assert!(
            types.is_empty(),
            "empty string should produce no relation types"
        );
    }

    #[test]
    fn test_graph_traverse_sql_injection_in_relation_types() {
        // Verify the quoting logic escapes single quotes
        let rt_str = "DEPENDS_ON, O'REILLY";
        let types: Vec<&str> = rt_str
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .collect();
        let quoted: Vec<String> = types
            .iter()
            .map(|t| format!("'{}'", t.replace('\'', "''")))
            .collect();
        assert_eq!(quoted[1], "'O''REILLY'", "single quotes should be escaped");
    }

    #[test]
    fn test_graph_update_edge_requires_all_four_params() {
        let params = json!({"source_entity": "a", "target_entity": "b", "relation_type": "R"});
        assert!(
            param_str(&params, "action").is_err(),
            "missing action should fail"
        );

        let params = json!({"target_entity": "b", "relation_type": "R", "action": "invalidate"});
        assert!(
            param_str(&params, "source_entity").is_err(),
            "missing source_entity should fail"
        );
    }

    #[test]
    fn test_graph_update_edge_invalid_action_type() {
        // The handler validates action is "invalidate" or "update_description"
        // This test verifies the validation logic pattern
        let action = "delete";
        let valid = matches!(action, "invalidate" | "update_description");
        assert!(!valid, "'delete' should not be a valid action");

        let action = "invalidate";
        let valid = matches!(action, "invalidate" | "update_description");
        assert!(valid, "'invalidate' should be valid");

        let action = "update_description";
        let valid = matches!(action, "invalidate" | "update_description");
        assert!(valid, "'update_description' should be valid");
    }

    #[test]
    fn test_graph_traverse_direction_sql_mapping() {
        // Verifies the direction → SQL clause mapping logic
        for (direction, expected_join) in &[
            ("outgoing", "edge.source_id = t.entity_id"),
            ("incoming", "edge.target_id = t.entity_id"),
            (
                "both",
                "(edge.source_id = t.entity_id OR edge.target_id = t.entity_id)",
            ),
            (
                "unknown",
                "(edge.source_id = t.entity_id OR edge.target_id = t.entity_id)",
            ),
        ] {
            let join = match *direction {
                "outgoing" => "edge.source_id = t.entity_id",
                "incoming" => "edge.target_id = t.entity_id",
                _ => "(edge.source_id = t.entity_id OR edge.target_id = t.entity_id)",
            };
            assert_eq!(
                join, *expected_join,
                "direction '{direction}' should map correctly"
            );
        }
    }

    #[test]
    fn test_graph_traverse_next_entity_expr_mapping() {
        for (direction, expected) in &[
            ("outgoing", "edge.target_id"),
            ("incoming", "edge.source_id"),
            ("both", "CASE WHEN edge.source_id = t.entity_id THEN edge.target_id ELSE edge.source_id END"),
        ] {
            let expr = match *direction {
                "outgoing" => "edge.target_id",
                "incoming" => "edge.source_id",
                _ => "CASE WHEN edge.source_id = t.entity_id THEN edge.target_id ELSE edge.source_id END",
            };
            assert_eq!(expr, *expected, "direction '{direction}' next_entity_expr mismatch");
        }
    }

    // -----------------------------------------------------------------------
    // v0.7.0: Chat tool parameter extraction tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_list_chat_sessions_param_extraction() {
        // list_chat_sessions uses optional params: platform, status (default "active"), limit (default 20)
        let params = json!({"platform": "slack", "status": "closed", "limit": 5});
        assert_eq!(
            params.get("platform").and_then(|v| v.as_str()),
            Some("slack")
        );
        assert_eq!(
            params
                .get("status")
                .and_then(|v| v.as_str())
                .unwrap_or("active"),
            "closed"
        );
        assert_eq!(
            params.get("limit").and_then(|v| v.as_i64()).unwrap_or(20),
            5
        );

        // With no params, defaults should apply
        let empty = json!({});
        assert_eq!(empty.get("platform").and_then(|v| v.as_str()), None);
        assert_eq!(
            empty
                .get("status")
                .and_then(|v| v.as_str())
                .unwrap_or("active"),
            "active"
        );
        assert_eq!(
            empty.get("limit").and_then(|v| v.as_i64()).unwrap_or(20),
            20
        );
    }

    #[test]
    fn test_reply_to_chat_param_extraction() {
        // reply_to_chat requires session_id and content
        let params = json!({"session_id": "abc-123", "content": "Hello from agent"});
        let session_id = param_str(&params, "session_id").unwrap();
        let content = param_str(&params, "content").unwrap();
        assert_eq!(session_id, "abc-123");
        assert_eq!(content, "Hello from agent");

        // Missing session_id should fail
        let bad_params = json!({"content": "hello"});
        assert!(
            param_str(&bad_params, "session_id").is_err(),
            "missing session_id should error"
        );

        // Missing content should fail
        let bad_params = json!({"session_id": "abc-123"});
        assert!(
            param_str(&bad_params, "content").is_err(),
            "missing content should error"
        );
    }

    #[test]
    fn test_chat_tool_schemas_in_registry() {
        let names: Vec<&str> = TOOL_REGISTRY
            .iter()
            .map(|t| t["function"]["name"].as_str().unwrap_or(""))
            .collect();
        assert!(
            names.contains(&"list_chat_sessions"),
            "registry missing list_chat_sessions"
        );
        assert!(
            names.contains(&"reply_to_chat"),
            "registry missing reply_to_chat"
        );
    }

    #[test]
    fn test_list_chat_sessions_is_readonly() {
        assert!(
            is_readonly_tool("list_chat_sessions"),
            "list_chat_sessions should be readonly"
        );
    }

    #[test]
    fn test_reply_to_chat_is_not_readonly() {
        assert!(
            !is_readonly_tool("reply_to_chat"),
            "reply_to_chat should NOT be readonly"
        );
    }

    // -----------------------------------------------------------------------
    // v0.7.0: Formula definition validation tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_validate_formula_valid() {
        let def = json!({
            "formula": {"name": "test", "description": "test"},
            "steps": [
                {"name": "s1", "agent_role": "worker", "prompt": "do stuff", "output": "out"}
            ],
            "parameters": [
                {"name": "topic", "type": "string", "required": true}
            ]
        });
        assert!(validate_formula_definition(&def).is_ok());
    }

    #[test]
    fn test_validate_formula_missing_step_name() {
        let def = json!({
            "steps": [
                {"agent_role": "worker", "prompt": "do stuff", "output": "out"}
            ]
        });
        let err = validate_formula_definition(&def).unwrap_err();
        assert!(
            err.contains("missing 'name'"),
            "error should mention missing name: {err}"
        );
    }

    #[test]
    fn test_validate_formula_invalid_param_type() {
        let def = json!({
            "steps": [
                {"name": "s1", "agent_role": "worker", "prompt": "do", "output": "o"}
            ],
            "parameters": [
                {"name": "x", "type": "float"}
            ]
        });
        let err = validate_formula_definition(&def).unwrap_err();
        assert!(
            err.contains("invalid type 'float'"),
            "error should mention invalid type: {err}"
        );
    }

    #[test]
    fn test_validate_formula_bad_on_failure_ref() {
        let def = json!({
            "steps": [
                {"name": "s1", "agent_role": "worker", "prompt": "do", "output": "o"}
            ],
            "on_failure": {
                "step_name": "nonexistent_step"
            }
        });
        let err = validate_formula_definition(&def).unwrap_err();
        assert!(
            err.contains("nonexistent_step"),
            "error should mention bad step ref: {err}"
        );
    }

    #[test]
    fn test_validate_formula_empty_steps() {
        let def = json!({"steps": []});
        let err = validate_formula_definition(&def).unwrap_err();
        assert!(
            err.contains("at least one step"),
            "error should mention empty steps: {err}"
        );
    }

    #[test]
    fn test_formula_tools_in_registry() {
        let names: Vec<&str> = TOOL_REGISTRY
            .iter()
            .map(|t| t["function"]["name"].as_str().unwrap_or(""))
            .collect();
        assert!(
            names.contains(&"list_formulas"),
            "registry missing list_formulas"
        );
        assert!(
            names.contains(&"get_formula"),
            "registry missing get_formula"
        );
        assert!(
            names.contains(&"create_formula"),
            "registry missing create_formula"
        );
        assert!(
            names.contains(&"update_formula"),
            "registry missing update_formula"
        );
        // v0.10.0 file tools
        assert!(names.contains(&"read_file"), "registry missing read_file");
        assert!(names.contains(&"write_file"), "registry missing write_file");
        assert!(names.contains(&"read_pdf"), "registry missing read_pdf");
        assert!(names.contains(&"read_docx"), "registry missing read_docx");
    }

    #[test]
    fn test_file_tool_schemas() {
        let names: Vec<&str> = TOOL_REGISTRY
            .iter()
            .filter_map(|t| t["function"]["name"].as_str())
            .collect();

        // read_file: requires path
        let idx = names.iter().position(|n| *n == "read_file").unwrap();
        let rf = &TOOL_REGISTRY[idx]["function"];
        let required = rf["parameters"]["required"].as_array().unwrap();
        assert!(required.iter().any(|v| v == "path"));

        // write_file: requires path + content
        let idx = names.iter().position(|n| *n == "write_file").unwrap();
        let wf = &TOOL_REGISTRY[idx]["function"];
        let required = wf["parameters"]["required"].as_array().unwrap();
        assert!(required.iter().any(|v| v == "path"));
        assert!(required.iter().any(|v| v == "content"));

        // read_pdf: requires path
        let idx = names.iter().position(|n| *n == "read_pdf").unwrap();
        let rp = &TOOL_REGISTRY[idx]["function"];
        let required = rp["parameters"]["required"].as_array().unwrap();
        assert!(required.iter().any(|v| v == "path"));
    }

    #[test]
    fn test_file_tools_not_readonly() {
        assert!(
            !is_readonly_tool("read_file"),
            "read_file should NOT be readonly"
        );
        assert!(
            !is_readonly_tool("write_file"),
            "write_file should NOT be readonly"
        );
        assert!(
            !is_readonly_tool("read_pdf"),
            "read_pdf should NOT be readonly"
        );
    }

    #[test]
    fn test_formula_readonly_tools() {
        assert!(
            is_readonly_tool("list_formulas"),
            "list_formulas should be readonly"
        );
        assert!(
            is_readonly_tool("get_formula"),
            "get_formula should be readonly"
        );
        assert!(
            !is_readonly_tool("create_formula"),
            "create_formula should NOT be readonly"
        );
        assert!(
            !is_readonly_tool("update_formula"),
            "update_formula should NOT be readonly"
        );
    }

    // -----------------------------------------------------------------------
    // strip_think_tags_bridge tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_strip_think_tags_basic() {
        let input = "hello <think>internal reasoning</think> world";
        assert_eq!(strip_think_tags_bridge(input), "hello  world");
    }

    #[test]
    fn test_strip_think_tags_leading_think() {
        let input = "<think>reasoning at start</think>actual content";
        assert_eq!(strip_think_tags_bridge(input), "actual content");
    }

    #[test]
    fn test_strip_think_tags_no_tags() {
        let input = "plain text with no tags";
        assert_eq!(strip_think_tags_bridge(input), "plain text with no tags");
    }

    #[test]
    fn test_strip_think_tags_multiple() {
        let input = "<think>first</think>A<think>second</think>B";
        assert_eq!(strip_think_tags_bridge(input), "AB");
    }

    #[test]
    fn test_strip_think_tags_unclosed() {
        let input = "start <think>unclosed tag never ends";
        assert_eq!(strip_think_tags_bridge(input), "start ");
    }

    // -----------------------------------------------------------------------
    // expand_query helpers test
    // -----------------------------------------------------------------------

    #[test]
    fn test_expand_query_short_query_skips() {
        // expand_query skips queries with fewer than 3 words
        // We test the skip logic indirectly: short queries should come back as-is
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let result = rt.block_on(expand_query(
            &reqwest::Client::new(),
            "http://localhost:99999", // unreachable, shouldn't be called
            "test-model",
            1,
            "two words",
        ));
        assert_eq!(result, vec!["two words".to_string()]);
    }
}
