/*
 * Broodlink - Multi-agent AI orchestration system
 * Copyright (C) 2025–2026 Neven Kordic <neven@broodlink.ai>
 * SPDX-License-Identifier: AGPL-3.0-or-later
 */

//! Setup wizard: dependency detection, installation, DB creation, model pulling.
//! Everything exposed via JSON API — the web UI drives all interaction.

pub mod api;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SystemState {
    pub ollama: DepStatus,
    pub dolt: DepStatus,
    pub postgres: DepStatus,
    pub nats: DepStatus,
    pub qdrant: DepStatus,
    pub databases: DbStatus,
    pub models: Vec<ModelInfo>,
    pub config_exists: bool,
}

impl SystemState {
    pub fn is_ready(&self) -> bool {
        self.ollama.running
            && self.dolt.running
            && self.postgres.running
            && self.nats.running
            && self.databases.agent_ledger
            && self.databases.broodlink_hot
            && self.config_exists
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DepStatus {
    pub installed: bool,
    pub running: bool,
    pub version: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DbStatus {
    pub agent_ledger: bool,
    pub broodlink_hot: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelInfo {
    pub name: String,
    pub size: String,
    pub modified: String,
}

/// Check the full system state without modifying anything.
pub async fn check_system_state() -> SystemState {
    let (ollama, dolt, postgres, nats, qdrant) = tokio::join!(
        check_ollama(),
        check_dolt(),
        check_postgres(),
        check_nats(),
        check_qdrant(),
    );

    let databases = check_databases().await;
    let models = list_ollama_models().await;
    let config_exists = std::path::Path::new("config.toml").exists()
        || std::env::var("BROODLINK_CONFIG")
            .map(|p| std::path::Path::new(&p).exists())
            .unwrap_or(false);

    SystemState {
        ollama,
        dolt,
        postgres,
        nats,
        qdrant,
        databases,
        models,
        config_exists,
    }
}

async fn check_ollama() -> DepStatus {
    let version = run_cmd("ollama", &["--version"]).await;
    let running = reqwest::Client::new()
        .get("http://localhost:11434/api/tags")
        .timeout(std::time::Duration::from_secs(2))
        .send()
        .await
        .is_ok();

    DepStatus {
        installed: version.is_some(),
        running,
        version,
        error: None,
    }
}

async fn check_dolt() -> DepStatus {
    let version = run_cmd("dolt", &["version"]).await;
    let running = tokio::net::TcpStream::connect("127.0.0.1:3307")
        .await
        .is_ok();

    DepStatus {
        installed: version.is_some(),
        running,
        version,
        error: None,
    }
}

async fn check_postgres() -> DepStatus {
    // Try psql in PATH first, then common brew locations
    let version = run_cmd("psql", &["--version"])
        .await
        .or(run_cmd("/opt/homebrew/opt/postgresql/bin/psql", &["--version"]).await)
        .or(run_cmd("/usr/local/opt/postgresql/bin/psql", &["--version"]).await)
        .or(run_cmd(
            "C:\\Program Files\\PostgreSQL\\16\\bin\\psql",
            &["--version"],
        )
        .await)
        .or(run_cmd(
            "C:\\Program Files\\PostgreSQL\\15\\bin\\psql",
            &["--version"],
        )
        .await);
    let running = tokio::net::TcpStream::connect("127.0.0.1:5432")
        .await
        .is_ok();

    DepStatus {
        installed: version.is_some() || running,
        running,
        version,
        error: None,
    }
}

async fn check_nats() -> DepStatus {
    let version = run_cmd("nats-server", &["--version"]).await;
    let running = tokio::net::TcpStream::connect("127.0.0.1:4222")
        .await
        .is_ok();

    DepStatus {
        installed: version.is_some(),
        running,
        version,
        error: None,
    }
}

async fn check_qdrant() -> DepStatus {
    let running = reqwest::Client::new()
        .get("http://localhost:6333/healthz")
        .timeout(std::time::Duration::from_secs(2))
        .send()
        .await
        .is_ok();

    // Check PATH and ~/.local/bin
    let installed = which("qdrant")
        || dirs::home_dir()
            .map(|h| h.join(".local/bin/qdrant").exists())
            .unwrap_or(false);

    DepStatus {
        installed,
        running,
        version: None,
        error: None,
    }
}

async fn check_databases() -> DbStatus {
    // Try connecting to Dolt agent_ledger
    let agent_ledger = tokio::net::TcpStream::connect("127.0.0.1:3307")
        .await
        .is_ok(); // Simplified check — full check would query the DB

    // Try connecting to Postgres broodlink_hot
    let broodlink_hot = tokio::net::TcpStream::connect("127.0.0.1:5432")
        .await
        .is_ok();

    DbStatus {
        agent_ledger,
        broodlink_hot,
    }
}

async fn list_ollama_models() -> Vec<ModelInfo> {
    let resp = reqwest::Client::new()
        .get("http://localhost:11434/api/tags")
        .timeout(std::time::Duration::from_secs(5))
        .send()
        .await;

    let Ok(resp) = resp else { return vec![] };
    let Ok(body) = resp.json::<serde_json::Value>().await else {
        return vec![];
    };

    body.get("models")
        .and_then(|m| m.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|m| {
                    Some(ModelInfo {
                        name: m.get("name")?.as_str()?.to_string(),
                        size: format_bytes(m.get("size")?.as_u64()?),
                        modified: m.get("modified_at")?.as_str()?.to_string(),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

async fn run_cmd(cmd: &str, args: &[&str]) -> Option<String> {
    let output = tokio::process::Command::new(cmd)
        .args(args)
        .output()
        .await
        .ok()?;

    if output.status.success() {
        Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
    } else {
        None
    }
}

fn which(cmd: &str) -> bool {
    let which_cmd = if cfg!(windows) { "where" } else { "which" };
    std::process::Command::new(which_cmd)
        .arg(cmd)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn format_bytes(bytes: u64) -> String {
    const GB: u64 = 1_073_741_824;
    const MB: u64 = 1_048_576;
    if bytes >= GB {
        format!("{:.1} GB", bytes as f64 / GB as f64)
    } else {
        format!("{:.0} MB", bytes as f64 / MB as f64)
    }
}
