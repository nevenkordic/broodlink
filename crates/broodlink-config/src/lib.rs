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
}

impl Default for A2aConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            port: default_a2a_port(),
            api_key_name: default_a2a_api_key_name(),
            bridge_url: default_a2a_bridge_url(),
            bridge_jwt_path: None,
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
}

impl Default for MemorySearchConfig {
    fn default() -> Self {
        Self {
            decay_lambda: default_decay_lambda(),
            reranker_model: default_reranker_model(),
            reranker_enabled: default_reranker_enabled(),
            max_content_length: default_max_content_length(),
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

        settings.try_deserialize()
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

        std::env::remove_var("BROODLINK_CONFIG");
    }
}
