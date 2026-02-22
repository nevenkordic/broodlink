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

use serde::Deserialize;
use std::collections::HashMap;

#[derive(Deserialize, Clone, Debug)]
pub struct Config {
    pub broodlink: BroodlinkConfig,
    pub profile: ProfileConfig,
    pub residency: ResidencyConfig,
    pub dolt: DoltConfig,
    pub postgres: PostgresConfig,
    pub nats: NatsConfig,
    pub qdrant: QdrantConfig,
    pub beads: BeadsConfig,
    pub ollama: OllamaConfig,
    pub secrets: SecretsConfig,
    pub status_api: StatusApiConfig,
    pub rate_limits: RateLimitsConfig,
    #[serde(default)]
    pub agents: HashMap<String, AgentConfig>,
    // --- v0.2.0 additions ---
    #[serde(default)]
    pub telemetry: TelemetryConfig,
    #[serde(default)]
    pub mcp_server: McpServerConfig,
    #[serde(default)]
    pub routing: RoutingConfig,
    #[serde(default)]
    pub a2a: A2aConfig,
    #[serde(default)]
    pub beads_bridge: BeadsBridgeConfig,
    #[serde(default)]
    pub tls: TlsConfig,
    #[serde(default)]
    pub postgres_read_replicas: ReadReplicaConfig,
    // --- v0.4.0 additions ---
    #[serde(default)]
    pub memory_search: MemorySearchConfig,
    // --- v0.6.0 additions ---
    #[serde(default)]
    pub jwt: JwtConfig,
    #[serde(default)]
    pub budget: BudgetConfig,
    #[serde(default)]
    pub dlq: DlqConfig,
    #[serde(default)]
    pub collaboration: CollaborationConfig,
    #[serde(default)]
    pub webhooks: WebhookConfig,
    // --- v0.7.0 additions ---
    #[serde(default)]
    pub chat: ChatConfig,
    #[serde(default)]
    pub dashboard_auth: DashboardAuthConfig,
    #[serde(default)]
    pub heartbeat: HeartbeatConfig,
}

#[derive(Deserialize, Clone, Debug)]
pub struct BroodlinkConfig {
    pub env: String,
    pub version: String,
}

#[derive(Deserialize, Clone, Debug)]
pub struct ProfileConfig {
    pub name: String,
    #[serde(default)]
    pub tls_interservice: bool,
    #[serde(default)]
    pub nats_cluster: bool,
    #[serde(default)]
    pub dolt_replication: bool,
    #[serde(default)]
    pub postgres_replicas: u32,
    #[serde(default)]
    pub qdrant_distributed: bool,
    #[serde(default = "default_secrets_provider")]
    pub secrets_provider: String,
    #[serde(default)]
    pub audit_residency: bool,
}

fn default_secrets_provider() -> String {
    "sops".to_string()
}

#[derive(Deserialize, Clone, Debug)]
pub struct ResidencyConfig {
    pub region: String,
    pub data_classification: String,
    #[serde(default)]
    pub allowed_egress: Vec<String>,
}

#[derive(Deserialize, Clone, Debug)]
pub struct DoltConfig {
    pub host: String,
    pub port: u16,
    pub database: String,
    pub user: String,
    pub password_key: String,
    #[serde(default = "default_dolt_min")]
    pub min_connections: u32,
    #[serde(default = "default_dolt_max")]
    pub max_connections: u32,
}

fn default_dolt_min() -> u32 {
    2
}
fn default_dolt_max() -> u32 {
    5
}

#[derive(Deserialize, Clone, Debug)]
pub struct PostgresConfig {
    pub host: String,
    pub port: u16,
    pub database: String,
    pub user: String,
    pub password_key: String,
    #[serde(default = "default_pg_min")]
    pub min_connections: u32,
    #[serde(default = "default_pg_max")]
    pub max_connections: u32,
}

fn default_pg_min() -> u32 {
    5
}
fn default_pg_max() -> u32 {
    20
}

#[derive(Deserialize, Clone, Debug)]
pub struct NatsConfig {
    pub url: String,
    pub subject_prefix: String,
    #[serde(default)]
    pub cluster_urls: Vec<String>,
}

#[derive(Deserialize, Clone, Debug)]
pub struct QdrantConfig {
    pub url: String,
    pub collection: String,
    pub api_key: String,
}

#[derive(Deserialize, Clone, Debug)]
pub struct BeadsConfig {
    pub workspace: String,
    pub bd_binary: String,
    pub formulas_dir: String,
}

#[derive(Deserialize, Clone, Debug)]
pub struct OllamaConfig {
    pub url: String,
    pub embedding_model: String,
    #[serde(default = "default_ollama_timeout")]
    pub timeout_seconds: u64,
}

fn default_ollama_timeout() -> u64 {
    30
}

#[derive(Deserialize, Clone, Debug)]
pub struct SecretsConfig {
    pub provider: String,
    #[serde(default)]
    pub sops_file: Option<String>,
    #[serde(default)]
    pub age_identity: Option<String>,
    #[serde(default)]
    pub infisical_url: Option<String>,
    #[serde(default)]
    pub infisical_token: Option<String>,
}

#[derive(Deserialize, Clone, Debug)]
pub struct StatusApiConfig {
    pub port: u16,
    #[serde(default)]
    pub cors_origins: Vec<String>,
    pub api_key_name: String,
}

#[derive(Deserialize, Clone, Debug)]
pub struct RateLimitsConfig {
    #[serde(default = "default_rpm")]
    pub requests_per_minute_per_agent: u32,
    #[serde(default = "default_burst")]
    pub burst: u32,
}

fn default_rpm() -> u32 {
    60
}
fn default_burst() -> u32 {
    10
}

#[derive(Deserialize, Clone, Debug)]
pub struct AgentConfig {
    pub agent_id: String,
    pub role: String,
    pub transport: String,
    pub cost_tier: String,
    #[serde(default)]
    pub preferred_formulas: Vec<String>,
}

// --- v0.2.0 config structs ---

#[derive(Deserialize, Clone, Debug)]
pub struct TelemetryConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_otlp_endpoint")]
    pub otlp_endpoint: String,
    #[serde(default = "default_sample_rate")]
    pub sample_rate: f64,
}

impl Default for TelemetryConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            otlp_endpoint: default_otlp_endpoint(),
            sample_rate: default_sample_rate(),
        }
    }
}

fn default_otlp_endpoint() -> String {
    "http://localhost:4317".to_string()
}

fn default_sample_rate() -> f64 {
    1.0
}

#[derive(Deserialize, Clone, Debug)]
pub struct McpServerConfig {
    #[serde(default = "default_mcp_enabled")]
    pub enabled: bool,
    #[serde(default = "default_mcp_transport")]
    pub transport: String,
    #[serde(default = "default_mcp_port")]
    pub port: u16,
    #[serde(default = "default_mcp_bridge_url")]
    pub bridge_url: String,
    #[serde(default = "default_mcp_agent_id")]
    pub agent_id: String,
    #[serde(default)]
    pub jwt_token_path: Option<String>,
    #[serde(default = "default_session_timeout")]
    pub session_timeout_minutes: u32,
    #[serde(default)]
    pub cors_origins: Vec<String>,
}

impl Default for McpServerConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            transport: default_mcp_transport(),
            port: default_mcp_port(),
            bridge_url: default_mcp_bridge_url(),
            agent_id: default_mcp_agent_id(),
            jwt_token_path: None,
            session_timeout_minutes: default_session_timeout(),
            cors_origins: Vec::new(),
        }
    }
}

fn default_session_timeout() -> u32 {
    30
}

fn default_mcp_transport() -> String {
    "http".to_string()
}

fn default_mcp_enabled() -> bool {
    true
}

fn default_mcp_port() -> u16 {
    3311
}

fn default_mcp_bridge_url() -> String {
    "http://localhost:3310".to_string()
}

fn default_mcp_agent_id() -> String {
    "mcp-server".to_string()
}

#[derive(Deserialize, Clone, Debug)]
pub struct RoutingConfig {
    #[serde(default)]
    pub weights: RoutingWeights,
    #[serde(default = "default_max_concurrent")]
    pub max_concurrent_default: u32,
    #[serde(default = "default_new_agent_bonus")]
    pub new_agent_bonus: f64,
}

impl Default for RoutingConfig {
    fn default() -> Self {
        Self {
            weights: RoutingWeights::default(),
            max_concurrent_default: default_max_concurrent(),
            new_agent_bonus: default_new_agent_bonus(),
        }
    }
}

fn default_new_agent_bonus() -> f64 {
    1.0
}

fn default_max_concurrent() -> u32 {
    5
}

#[derive(Deserialize, Clone, Debug)]
pub struct RoutingWeights {
    #[serde(default = "default_capability_weight")]
    pub capability: f64,
    #[serde(default = "default_success_rate_weight")]
    pub success_rate: f64,
    #[serde(default = "default_availability_weight")]
    pub availability: f64,
    #[serde(default = "default_cost_weight")]
    pub cost: f64,
    #[serde(default = "default_recency_weight")]
    pub recency: f64,
}

impl Default for RoutingWeights {
    fn default() -> Self {
        Self {
            capability: default_capability_weight(),
            success_rate: default_success_rate_weight(),
            availability: default_availability_weight(),
            cost: default_cost_weight(),
            recency: default_recency_weight(),
        }
    }
}

fn default_capability_weight() -> f64 {
    0.35
}

fn default_success_rate_weight() -> f64 {
    0.25
}

fn default_availability_weight() -> f64 {
    0.20
}

fn default_cost_weight() -> f64 {
    0.15
}

fn default_recency_weight() -> f64 {
    0.05
}

#[derive(Deserialize, Clone, Debug)]
pub struct A2aConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_a2a_port")]
    pub port: u16,
    #[serde(default = "default_a2a_api_key_name")]
    pub api_key_name: String,
    #[serde(default = "default_a2a_bridge_url")]
    pub bridge_url: String,
    #[serde(default)]
    pub bridge_jwt_path: Option<String>,
    #[serde(default)]
    pub cors_origins: Vec<String>,
}

impl Default for A2aConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            port: default_a2a_port(),
            api_key_name: default_a2a_api_key_name(),
            bridge_url: default_a2a_bridge_url(),
            bridge_jwt_path: None,
            cors_origins: Vec::new(),
        }
    }
}

fn default_a2a_port() -> u16 {
    3313
}

fn default_a2a_api_key_name() -> String {
    "BROODLINK_A2A_API_KEY".to_string()
}

fn default_a2a_bridge_url() -> String {
    "http://localhost:3310".to_string()
}

#[derive(Deserialize, Clone, Debug)]
pub struct BeadsBridgeConfig {
    #[serde(default = "default_beads_bridge_port")]
    pub port: u16,
}

impl Default for BeadsBridgeConfig {
    fn default() -> Self {
        Self {
            port: default_beads_bridge_port(),
        }
    }
}

fn default_beads_bridge_port() -> u16 {
    3310
}

#[derive(Deserialize, Clone, Debug, Default)]
pub struct TlsConfig {
    #[serde(default)]
    pub cert_path: Option<String>,
    #[serde(default)]
    pub key_path: Option<String>,
    #[serde(default)]
    pub ca_path: Option<String>,
}

#[derive(Deserialize, Clone, Debug, Default)]
pub struct ReadReplicaConfig {
    #[serde(default)]
    pub urls: Vec<String>,
}

// --- v0.4.0: Hybrid memory search ---

#[derive(Deserialize, Clone, Debug)]
pub struct MemorySearchConfig {
    #[serde(default = "default_decay_lambda")]
    pub decay_lambda: f64,
    #[serde(default = "default_reranker_model")]
    pub reranker_model: String,
    #[serde(default = "default_reranker_enabled")]
    pub reranker_enabled: bool,
    #[serde(default = "default_max_content_length")]
    pub max_content_length: usize,
    // v0.5.0: Knowledge graph
    #[serde(default = "default_kg_enabled")]
    pub kg_enabled: bool,
    #[serde(default = "default_kg_extraction_model")]
    pub kg_extraction_model: String,
    #[serde(default = "default_kg_entity_similarity_threshold")]
    pub kg_entity_similarity_threshold: f64,
    #[serde(default = "default_kg_max_hops")]
    pub kg_max_hops: u32,
    #[serde(default = "default_kg_extraction_timeout_seconds")]
    pub kg_extraction_timeout_seconds: u64,
    // v0.6.0: Knowledge graph expiry
    #[serde(default = "default_kg_entity_ttl_days")]
    pub kg_entity_ttl_days: u32,
    #[serde(default = "default_kg_edge_decay_rate")]
    pub kg_edge_decay_rate: f64,
    #[serde(default = "default_kg_cleanup_interval_hours")]
    pub kg_cleanup_interval_hours: u32,
    #[serde(default = "default_kg_min_mention_count")]
    pub kg_min_mention_count: u32,
}

impl Default for MemorySearchConfig {
    fn default() -> Self {
        Self {
            decay_lambda: default_decay_lambda(),
            reranker_model: default_reranker_model(),
            reranker_enabled: default_reranker_enabled(),
            max_content_length: default_max_content_length(),
            kg_enabled: default_kg_enabled(),
            kg_extraction_model: default_kg_extraction_model(),
            kg_entity_similarity_threshold: default_kg_entity_similarity_threshold(),
            kg_max_hops: default_kg_max_hops(),
            kg_extraction_timeout_seconds: default_kg_extraction_timeout_seconds(),
            kg_entity_ttl_days: default_kg_entity_ttl_days(),
            kg_edge_decay_rate: default_kg_edge_decay_rate(),
            kg_cleanup_interval_hours: default_kg_cleanup_interval_hours(),
            kg_min_mention_count: default_kg_min_mention_count(),
        }
    }
}

fn default_decay_lambda() -> f64 {
    0.01
}

fn default_reranker_model() -> String {
    "snowflake-arctic-embed2:137m".to_string()
}

fn default_reranker_enabled() -> bool {
    true
}

fn default_max_content_length() -> usize {
    2000
}

fn default_kg_enabled() -> bool {
    true
}

fn default_kg_extraction_model() -> String {
    "qwen3:1.7b".to_string()
}

fn default_kg_entity_similarity_threshold() -> f64 {
    0.85
}

fn default_kg_max_hops() -> u32 {
    3
}

fn default_kg_extraction_timeout_seconds() -> u64 {
    120
}

// --- v0.6.0: JWT key rotation ---

#[derive(Deserialize, Clone, Debug)]
pub struct JwtConfig {
    #[serde(default = "default_jwt_keys_dir")]
    pub keys_dir: String,
    #[serde(default = "default_jwt_grace_period_hours")]
    pub grace_period_hours: u32,
}

impl Default for JwtConfig {
    fn default() -> Self {
        Self {
            keys_dir: default_jwt_keys_dir(),
            grace_period_hours: default_jwt_grace_period_hours(),
        }
    }
}

fn default_jwt_keys_dir() -> String {
    "~/.broodlink".to_string()
}

fn default_jwt_grace_period_hours() -> u32 {
    24
}

// --- v0.6.0: Agent budget enforcement ---

#[derive(Deserialize, Clone, Debug)]
pub struct BudgetConfig {
    #[serde(default = "default_budget_enabled")]
    pub enabled: bool,
    #[serde(default = "default_daily_replenishment")]
    pub daily_replenishment: i64,
    #[serde(default = "default_replenish_hour_utc")]
    pub replenish_hour_utc: u32,
    #[serde(default = "default_low_budget_threshold")]
    pub low_budget_threshold: i64,
    #[serde(default = "default_tool_cost")]
    pub default_tool_cost: i64,
}

impl Default for BudgetConfig {
    fn default() -> Self {
        Self {
            enabled: default_budget_enabled(),
            daily_replenishment: default_daily_replenishment(),
            replenish_hour_utc: default_replenish_hour_utc(),
            low_budget_threshold: default_low_budget_threshold(),
            default_tool_cost: default_tool_cost(),
        }
    }
}

fn default_budget_enabled() -> bool {
    true
}

fn default_daily_replenishment() -> i64 {
    100_000
}

fn default_replenish_hour_utc() -> u32 {
    0
}

fn default_low_budget_threshold() -> i64 {
    1000
}

fn default_tool_cost() -> i64 {
    1
}

// --- v0.6.0: Dead-letter queue ---

#[derive(Deserialize, Clone, Debug)]
pub struct DlqConfig {
    #[serde(default = "default_dlq_auto_retry")]
    pub auto_retry_enabled: bool,
    #[serde(default = "default_dlq_max_retries")]
    pub max_retries: u32,
    #[serde(default = "default_dlq_backoff_base_ms")]
    pub backoff_base_ms: u64,
    #[serde(default = "default_dlq_check_interval_secs")]
    pub check_interval_secs: u64,
}

impl Default for DlqConfig {
    fn default() -> Self {
        Self {
            auto_retry_enabled: default_dlq_auto_retry(),
            max_retries: default_dlq_max_retries(),
            backoff_base_ms: default_dlq_backoff_base_ms(),
            check_interval_secs: default_dlq_check_interval_secs(),
        }
    }
}

fn default_dlq_auto_retry() -> bool {
    true
}

fn default_dlq_max_retries() -> u32 {
    3
}

fn default_dlq_backoff_base_ms() -> u64 {
    1000
}

fn default_dlq_check_interval_secs() -> u64 {
    60
}

// --- v0.6.0: Multi-agent collaboration ---

#[derive(Deserialize, Clone, Debug)]
pub struct CollaborationConfig {
    #[serde(default = "default_max_sub_tasks")]
    pub max_sub_tasks: u32,
    #[serde(default = "default_workspace_ttl_hours")]
    pub workspace_ttl_hours: u32,
}

impl Default for CollaborationConfig {
    fn default() -> Self {
        Self {
            max_sub_tasks: default_max_sub_tasks(),
            workspace_ttl_hours: default_workspace_ttl_hours(),
        }
    }
}

fn default_max_sub_tasks() -> u32 {
    10
}

fn default_workspace_ttl_hours() -> u32 {
    24
}

// --- v0.6.0: Webhook gateway ---

#[derive(Deserialize, Clone, Debug)]
pub struct WebhookConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub slack_signing_secret: Option<String>,
    #[serde(default)]
    pub slack_bot_token: Option<String>,
    #[serde(default)]
    pub teams_app_id: Option<String>,
    #[serde(default)]
    pub teams_shared_secret: Option<String>,
    #[serde(default)]
    pub telegram_bot_token: Option<String>,
    #[serde(default)]
    pub telegram_secret_token: Option<String>,
    #[serde(default = "default_webhook_delivery_timeout_secs")]
    pub delivery_timeout_secs: u64,
    #[serde(default = "default_webhook_max_retries")]
    pub max_retries: u32,
}

impl Default for WebhookConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            slack_signing_secret: None,
            slack_bot_token: None,
            teams_app_id: None,
            teams_shared_secret: None,
            telegram_bot_token: None,
            telegram_secret_token: None,
            delivery_timeout_secs: default_webhook_delivery_timeout_secs(),
            max_retries: default_webhook_max_retries(),
        }
    }
}

fn default_webhook_delivery_timeout_secs() -> u64 {
    10
}

fn default_webhook_max_retries() -> u32 {
    3
}

// --- v0.7.0: Conversational agent gateway ---

#[derive(Deserialize, Clone, Debug)]
pub struct ChatConfig {
    #[serde(default = "default_chat_enabled")]
    pub enabled: bool,
    #[serde(default = "default_chat_max_context_messages")]
    pub max_context_messages: u32,
    #[serde(default = "default_chat_session_timeout_hours")]
    pub session_timeout_hours: u32,
    #[serde(default = "default_chat_reply_timeout_seconds")]
    pub reply_timeout_seconds: u64,
    #[serde(default = "default_chat_reply_retry_attempts")]
    pub reply_retry_attempts: u32,
    #[serde(default = "default_chat_default_agent_role")]
    pub default_agent_role: String,
    #[serde(default = "default_chat_greeting_enabled")]
    pub greeting_enabled: bool,
    #[serde(default = "default_chat_greeting_message")]
    pub greeting_message: String,
}

impl Default for ChatConfig {
    fn default() -> Self {
        Self {
            enabled: default_chat_enabled(),
            max_context_messages: default_chat_max_context_messages(),
            session_timeout_hours: default_chat_session_timeout_hours(),
            reply_timeout_seconds: default_chat_reply_timeout_seconds(),
            reply_retry_attempts: default_chat_reply_retry_attempts(),
            default_agent_role: default_chat_default_agent_role(),
            greeting_enabled: default_chat_greeting_enabled(),
            greeting_message: default_chat_greeting_message(),
        }
    }
}

fn default_chat_enabled() -> bool {
    true
}

fn default_chat_max_context_messages() -> u32 {
    10
}

fn default_chat_session_timeout_hours() -> u32 {
    24
}

fn default_chat_reply_timeout_seconds() -> u64 {
    300
}

fn default_chat_reply_retry_attempts() -> u32 {
    3
}

fn default_chat_default_agent_role() -> String {
    "worker".to_string()
}

fn default_chat_greeting_enabled() -> bool {
    true
}

fn default_chat_greeting_message() -> String {
    "Hello! I'm a Broodlink agent. How can I help?".to_string()
}

// ---------------------------------------------------------------------------
// Dashboard Auth (v0.7.0)
// ---------------------------------------------------------------------------

#[derive(Deserialize, Clone, Debug)]
pub struct DashboardAuthConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_session_ttl_hours")]
    pub session_ttl_hours: u32,
    #[serde(default = "default_bcrypt_cost")]
    pub bcrypt_cost: u32,
    #[serde(default = "default_max_sessions_per_user")]
    pub max_sessions_per_user: u32,
}

impl Default for DashboardAuthConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            session_ttl_hours: default_session_ttl_hours(),
            bcrypt_cost: default_bcrypt_cost(),
            max_sessions_per_user: default_max_sessions_per_user(),
        }
    }
}

fn default_session_ttl_hours() -> u32 {
    8
}

fn default_bcrypt_cost() -> u32 {
    12
}

fn default_max_sessions_per_user() -> u32 {
    5
}

fn default_kg_entity_ttl_days() -> u32 {
    90
}

fn default_kg_edge_decay_rate() -> f64 {
    0.005
}

fn default_kg_cleanup_interval_hours() -> u32 {
    24
}

fn default_kg_min_mention_count() -> u32 {
    3
}

// --- Heartbeat config ---

#[derive(Deserialize, Clone, Debug)]
pub struct HeartbeatConfig {
    #[serde(default = "default_heartbeat_cycle_timeout_secs")]
    pub cycle_timeout_secs: u64,
    #[serde(default = "default_heartbeat_stale_agent_minutes")]
    pub stale_agent_minutes: u32,
}

impl Default for HeartbeatConfig {
    fn default() -> Self {
        Self {
            cycle_timeout_secs: default_heartbeat_cycle_timeout_secs(),
            stale_agent_minutes: default_heartbeat_stale_agent_minutes(),
        }
    }
}

fn default_heartbeat_cycle_timeout_secs() -> u64 {
    120
}

fn default_heartbeat_stale_agent_minutes() -> u32 {
    60
}

/// Expand a leading `~` to the user's home directory.
fn expand_tilde(path: &str) -> String {
    if let Some(rest) = path.strip_prefix("~/") {
        if let Ok(home) = std::env::var("HOME") {
            return format!("{home}/{rest}");
        }
    }
    path.to_string()
}

impl Config {
    /// Load configuration from file path specified by `BROODLINK_CONFIG` env var,
    /// with environment variable overrides.
    ///
    /// # Errors
    ///
    /// Returns `config::ConfigError` if the config file is missing, malformed,
    /// or required fields are absent.
    pub fn load() -> Result<Self, config::ConfigError> {
        let config_path = std::env::var("BROODLINK_CONFIG")
            .unwrap_or_else(|_| "config.toml".to_string());

        let settings = config::Config::builder()
            .add_source(config::File::with_name(&config_path))
            .add_source(
                config::Environment::with_prefix("BROODLINK")
                    .separator("_")
                    .try_parsing(true),
            )
            .build()?;

        let mut cfg: Self = settings.try_deserialize()?;

        // Expand tilde in paths that may contain ~/
        cfg.jwt.keys_dir = expand_tilde(&cfg.jwt.keys_dir);
        if let Some(ref p) = cfg.secrets.age_identity {
            cfg.secrets.age_identity = Some(expand_tilde(p));
        }
        if let Some(ref p) = cfg.secrets.sops_file {
            cfg.secrets.sops_file = Some(expand_tilde(p));
        }
        cfg.beads.formulas_dir = expand_tilde(&cfg.beads.formulas_dir);
        if let Some(ref p) = cfg.tls.cert_path {
            cfg.tls.cert_path = Some(expand_tilde(p));
        }
        if let Some(ref p) = cfg.tls.key_path {
            cfg.tls.key_path = Some(expand_tilde(p));
        }
        if let Some(ref p) = cfg.tls.ca_path {
            cfg.tls.ca_path = Some(expand_tilde(p));
        }

        Ok(cfg)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    /// Helper: returns a valid TOML config string that satisfies all required fields.
    fn valid_toml() -> String {
        r#"
[broodlink]
env = "test"
version = "0.1.0"

[profile]
name = "dev"

[residency]
region = "us-west-2"
data_classification = "internal"

[dolt]
host = "127.0.0.1"
port = 3307
database = "agent_ledger"
user = "root"
password_key = "DOLT_PASSWORD"

[postgres]
host = "127.0.0.1"
port = 5432
database = "broodlink_hot"
user = "broodlink"
password_key = "PG_PASSWORD"

[nats]
url = "nats://127.0.0.1:4222"
subject_prefix = "broodlink"

[qdrant]
url = "http://127.0.0.1:6333"
collection = "broodlink_memory"
api_key = "test-key"

[beads]
workspace = "/tmp/beads"
bd_binary = "/usr/local/bin/bd"
formulas_dir = "/tmp/formulas"

[ollama]
url = "http://127.0.0.1:11434"
embedding_model = "nomic-embed-text"

[secrets]
provider = "sops"

[status_api]
port = 3312
api_key_name = "STATUS_API_KEY"

[rate_limits]
"#
        .to_string()
    }

    #[test]
    fn test_load_valid_config() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.toml");
        std::fs::write(&config_path, valid_toml()).unwrap();

        // Point Config::load() at our temp file
        std::env::set_var("BROODLINK_CONFIG", config_path.to_str().unwrap());

        let cfg = Config::load().unwrap();

        assert_eq!(cfg.broodlink.env, "test");
        assert_eq!(cfg.broodlink.version, "0.1.0");
        assert_eq!(cfg.profile.name, "dev");
        assert_eq!(cfg.dolt.host, "127.0.0.1");
        assert_eq!(cfg.dolt.port, 3307);
        assert_eq!(cfg.postgres.port, 5432);
        assert_eq!(cfg.nats.url, "nats://127.0.0.1:4222");
        assert_eq!(cfg.qdrant.collection, "broodlink_memory");
        assert_eq!(cfg.ollama.embedding_model, "nomic-embed-text");
        assert_eq!(cfg.status_api.port, 3312);
        assert_eq!(cfg.residency.region, "us-west-2");

        // Clean up env var
        std::env::remove_var("BROODLINK_CONFIG");
    }

    #[test]
    fn test_load_missing_file() {
        std::env::set_var("BROODLINK_CONFIG", "/tmp/broodlink_nonexistent_config_12345.toml");

        let result = Config::load();
        assert!(result.is_err(), "loading a nonexistent file should return an error");

        std::env::remove_var("BROODLINK_CONFIG");
    }

    #[test]
    fn test_default_pool_sizes() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.toml");
        std::fs::write(&config_path, valid_toml()).unwrap();

        std::env::set_var("BROODLINK_CONFIG", config_path.to_str().unwrap());

        let cfg = Config::load().unwrap();

        // Dolt defaults
        assert_eq!(cfg.dolt.min_connections, 2, "dolt min_connections default should be 2");
        assert_eq!(cfg.dolt.max_connections, 5, "dolt max_connections default should be 5");

        // Postgres defaults
        assert_eq!(cfg.postgres.min_connections, 5, "pg min_connections default should be 5");
        assert_eq!(cfg.postgres.max_connections, 20, "pg max_connections default should be 20");

        // Rate limits defaults
        assert_eq!(cfg.rate_limits.requests_per_minute_per_agent, 60, "default rpm should be 60");
        assert_eq!(cfg.rate_limits.burst, 10, "default burst should be 10");

        // Ollama timeout default
        assert_eq!(cfg.ollama.timeout_seconds, 30, "default ollama timeout should be 30");

        // Profile defaults
        assert!(!cfg.profile.tls_interservice, "tls_interservice should default to false");
        assert!(!cfg.profile.nats_cluster, "nats_cluster should default to false");
        assert_eq!(cfg.profile.secrets_provider, "sops", "secrets_provider should default to sops");

        // Agents defaults to empty
        assert!(cfg.agents.is_empty(), "agents should default to empty HashMap");

        std::env::remove_var("BROODLINK_CONFIG");
    }

    #[test]
    fn test_v020_config_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.toml");
        std::fs::write(&config_path, valid_toml()).unwrap();

        std::env::set_var("BROODLINK_CONFIG", config_path.to_str().unwrap());

        let cfg = Config::load().unwrap();

        // Telemetry defaults
        assert!(!cfg.telemetry.enabled, "telemetry should default to disabled");
        assert_eq!(cfg.telemetry.otlp_endpoint, "http://localhost:4317");
        assert!((cfg.telemetry.sample_rate - 1.0).abs() < f64::EPSILON);

        // MCP server defaults
        assert!(cfg.mcp_server.enabled, "mcp_server should default to enabled");
        assert_eq!(cfg.mcp_server.port, 3311);
        assert_eq!(cfg.mcp_server.bridge_url, "http://localhost:3310");
        assert_eq!(cfg.mcp_server.agent_id, "mcp-server");
        assert!(cfg.mcp_server.jwt_token_path.is_none());
        assert_eq!(cfg.mcp_server.session_timeout_minutes, 30);
        assert!(cfg.mcp_server.cors_origins.is_empty());

        // Routing defaults
        assert!((cfg.routing.weights.capability - 0.35).abs() < f64::EPSILON);
        assert!((cfg.routing.weights.success_rate - 0.25).abs() < f64::EPSILON);
        assert!((cfg.routing.weights.availability - 0.20).abs() < f64::EPSILON);
        assert!((cfg.routing.weights.cost - 0.15).abs() < f64::EPSILON);
        assert!((cfg.routing.weights.recency - 0.05).abs() < f64::EPSILON);
        assert_eq!(cfg.routing.max_concurrent_default, 5);
        assert!((cfg.routing.new_agent_bonus - 1.0).abs() < f64::EPSILON);

        // A2A defaults
        assert!(!cfg.a2a.enabled, "a2a should default to disabled");
        assert_eq!(cfg.a2a.port, 3313);
        assert_eq!(cfg.a2a.api_key_name, "BROODLINK_A2A_API_KEY");
        assert_eq!(cfg.a2a.bridge_url, "http://localhost:3310");
        assert!(cfg.a2a.bridge_jwt_path.is_none());

        // TLS defaults
        assert!(cfg.tls.cert_path.is_none());
        assert!(cfg.tls.key_path.is_none());
        assert!(cfg.tls.ca_path.is_none());

        // Read replicas defaults
        assert!(cfg.postgres_read_replicas.urls.is_empty());

        // NATS cluster URLs default
        assert!(cfg.nats.cluster_urls.is_empty());

        // v0.4.0: MemorySearch defaults
        assert!((cfg.memory_search.decay_lambda - 0.01).abs() < f64::EPSILON);
        assert_eq!(cfg.memory_search.reranker_model, "snowflake-arctic-embed2:137m");
        assert!(cfg.memory_search.reranker_enabled);
        assert_eq!(cfg.memory_search.max_content_length, 2000);

        // v0.5.0: Knowledge graph defaults
        assert!(cfg.memory_search.kg_enabled);
        assert_eq!(cfg.memory_search.kg_extraction_model, "qwen3:1.7b");
        assert!((cfg.memory_search.kg_entity_similarity_threshold - 0.85).abs() < f64::EPSILON);
        assert_eq!(cfg.memory_search.kg_max_hops, 3);
        assert_eq!(cfg.memory_search.kg_extraction_timeout_seconds, 120);

        std::env::remove_var("BROODLINK_CONFIG");
    }
}
