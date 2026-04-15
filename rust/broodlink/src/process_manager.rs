/*
 * Broodlink - Multi-agent AI orchestration system
 * Copyright (C) 2025–2026 Neven Kordic <neven@broodlink.ai>
 * SPDX-License-Identifier: AGPL-3.0-or-later
 */

//! Manages external services and Broodlink backend processes.
//! Phase 1: launches each service as a child process.
//! Phase 2 (future): migrates to in-process tokio tasks.

use anyhow::{Context, Result};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use tokio::process::{Child, Command};
use tokio::sync::Mutex;

pub struct ProcessManager {
    children: Mutex<HashMap<String, Child>>,
}

impl ProcessManager {
    pub fn new() -> Self {
        Self {
            children: Mutex::new(HashMap::new()),
        }
    }

    /// Start all external dependencies and Broodlink services.
    pub async fn start_all(&self, broodlink_dir: &Path) -> Result<()> {
        // 1. Ensure external deps are running
        self.ensure_ollama().await?;
        self.ensure_dolt(broodlink_dir).await?;
        self.ensure_postgres().await?;
        self.ensure_nats().await?;
        self.ensure_qdrant().await?;

        // 2. Evict any stale launchd-managed Broodlink services that would
        //    conflict with the child processes we're about to spawn.
        let launchd_services = [
            ("beads-bridge", 3310u16),
            ("coordinator", 0),
            ("heartbeat", 0),
            ("embedding-worker", 0),
            ("status-api", 3312),
            ("a2a-gateway", 3313),
        ];
        for (name, port) in &launchd_services {
            evict_stale_launchd_service(name, *port).await;
        }

        // 3. Start Broodlink services (as child processes for now)
        let target_dir = find_binary_dir(broodlink_dir);

        let services = [
            ("beads-bridge", "beads-bridge"),
            ("coordinator", "coordinator"),
            ("heartbeat", "heartbeat"),
            ("embedding-worker", "embedding-worker"),
            ("status-api", "status-api"),
            ("a2a-gateway", "a2a-gateway"),
        ];

        for (name, binary) in &services {
            match self
                .start_service(name, &target_dir, binary, broodlink_dir)
                .await
            {
                Ok(()) => tracing::info!(service = name, "started"),
                Err(e) => tracing::warn!(service = name, error = %e, "failed to start"),
            }
        }

        Ok(())
    }

    async fn start_service(
        &self,
        name: &str,
        target_dir: &Path,
        binary: &str,
        work_dir: &Path,
    ) -> Result<()> {
        let bin_path = target_dir.join(binary);
        if !bin_path.exists() {
            anyhow::bail!("binary not found: {}", bin_path.display());
        }

        let child = Command::new(&bin_path)
            .current_dir(work_dir)
            .env("BROODLINK_CONFIG", work_dir.join("config.toml"))
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .with_context(|| format!("failed to spawn {name}"))?;

        self.children.lock().await.insert(name.to_string(), child);
        Ok(())
    }

    /// Stop all managed processes.
    pub async fn stop_all(&self) {
        let mut children = self.children.lock().await;
        for (name, child) in children.iter_mut() {
            tracing::info!(service = name.as_str(), "stopping");
            let _ = child.kill().await;
        }
        children.clear();
    }

    // -- External dependency management --

    async fn ensure_ollama(&self) -> Result<()> {
        if check_http("http://localhost:11434/api/tags").await {
            return Ok(());
        }
        tracing::info!("Starting Ollama...");
        // macOS: try opening the app
        #[cfg(target_os = "macos")]
        {
            let _ = std::process::Command::new("open")
                .arg("-a")
                .arg("Ollama")
                .spawn();
        }
        // Linux / Windows: try ollama serve
        #[cfg(target_os = "linux")]
        {
            let _ = Command::new("ollama")
                .arg("serve")
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn();
        }
        #[cfg(target_os = "windows")]
        {
            let _ = Command::new("ollama")
                .arg("serve")
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn();
        }
        wait_for_http("http://localhost:11434/api/tags", 30)
            .await
            .context("Ollama failed to start")
    }

    async fn ensure_dolt(&self, broodlink_dir: &Path) -> Result<()> {
        if check_tcp("127.0.0.1", 3307).await {
            return Ok(());
        }
        tracing::info!("Starting Dolt SQL server...");
        let dolt_dir = broodlink_dir.join(".dolt");
        let _ = std::fs::create_dir_all(&dolt_dir);

        let child = Command::new("dolt")
            .args(["sql-server", "--host", "127.0.0.1", "--port", "3307"])
            .current_dir(&dolt_dir)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .context("failed to start dolt sql-server")?;

        self.children.lock().await.insert("dolt".to_string(), child);
        wait_for_tcp("127.0.0.1", 3307, 15)
            .await
            .context("Dolt failed to start")
    }

    async fn ensure_postgres(&self) -> Result<()> {
        if check_tcp("127.0.0.1", 5432).await {
            return Ok(());
        }
        tracing::info!("Starting PostgreSQL...");
        #[cfg(target_os = "macos")]
        {
            let _ = std::process::Command::new("brew")
                .args(["services", "start", "postgresql"])
                .output();
        }
        #[cfg(target_os = "linux")]
        {
            let _ = std::process::Command::new("sudo")
                .args(["systemctl", "start", "postgresql"])
                .output();
        }
        #[cfg(target_os = "windows")]
        {
            let _ = std::process::Command::new("pg_ctl")
                .args(["start", "-D", &std::env::var("PGDATA").unwrap_or_default()])
                .output();
        }
        wait_for_tcp("127.0.0.1", 5432, 15)
            .await
            .context("PostgreSQL failed to start")
    }

    async fn ensure_nats(&self) -> Result<()> {
        if check_tcp("127.0.0.1", 4222).await {
            return Ok(());
        }
        tracing::info!("Starting NATS...");
        let child = Command::new("nats-server")
            .args(["--js", "-p", "4222"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .context("failed to start nats-server")?;

        self.children.lock().await.insert("nats".to_string(), child);
        wait_for_tcp("127.0.0.1", 4222, 10)
            .await
            .context("NATS failed to start")
    }

    async fn ensure_qdrant(&self) -> Result<()> {
        if check_http("http://localhost:6333/healthz").await {
            return Ok(());
        }
        tracing::info!("Starting Qdrant...");
        let child = Command::new("qdrant")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .context("failed to start qdrant")?;

        self.children
            .lock()
            .await
            .insert("qdrant".to_string(), child);
        wait_for_http("http://localhost:6333/healthz", 15)
            .await
            .context("Qdrant failed to start")
    }
}

/// Stop a launchd-managed Broodlink service if it is running, so the process
/// manager can spawn its own child process on the same port without conflict.
///
/// Strategy:
///  1. Try `launchctl bootout gui/<uid> com.broodlink.<name>`.
///  2. If bootout fails (permission error, not loaded, etc.), fall back to
///     killing whatever process is listening on `port` (when port > 0).
///  3. Wait briefly for the port to actually free up.
async fn evict_stale_launchd_service(name: &str, port: u16) {
    let label = format!("com.broodlink.{name}");

    // Check if the launchd service is loaded
    let loaded = std::process::Command::new("launchctl")
        .args(["print", &format!("gui/{}", get_uid())])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).contains(&label))
        .unwrap_or(false);

    if !loaded && (port == 0 || !check_tcp("127.0.0.1", port).await) {
        return; // Nothing to evict
    }

    if loaded {
        tracing::info!(service = name, "evicting stale launchd service {label}");
        let bootout = std::process::Command::new("launchctl")
            .args(["bootout", &format!("gui/{}", get_uid()), &label])
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .output();

        match bootout {
            Ok(output) if output.status.success() => {
                tracing::info!(service = name, "launchd service stopped via bootout");
            }
            Ok(output) => {
                let stderr = String::from_utf8_lossy(&output.stderr);
                tracing::warn!(
                    service = name,
                    stderr = %stderr.trim(),
                    "launchctl bootout failed — will try kill fallback"
                );
            }
            Err(e) => {
                tracing::warn!(service = name, error = %e, "launchctl command failed");
            }
        }
    }

    // Fallback: if the port is still occupied, kill the process holding it
    if port > 0 && check_tcp("127.0.0.1", port).await {
        tracing::info!(service = name, port, "port still occupied — killing process");
        let _ = std::process::Command::new("lsof")
            .args(["-ti", &format!(":{port}")])
            .output()
            .ok()
            .and_then(|o| {
                let pids = String::from_utf8_lossy(&o.stdout).trim().to_string();
                if pids.is_empty() {
                    return None;
                }
                for pid in pids.lines() {
                    let _ = std::process::Command::new("kill")
                        .args(["-9", pid.trim()])
                        .output();
                }
                Some(())
            });

        // Wait for port to free (up to 3 seconds)
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(3);
        while tokio::time::Instant::now() < deadline {
            if !check_tcp("127.0.0.1", port).await {
                tracing::info!(service = name, port, "port freed");
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        }
        tracing::warn!(service = name, port, "port still occupied after eviction attempt");
    }
}

/// Get the current user's UID for launchctl gui/<uid> domain.
fn get_uid() -> u32 {
    #[cfg(unix)]
    {
        // Safety: getuid() is always safe to call
        unsafe { libc::getuid() }
    }
    #[cfg(not(unix))]
    {
        0
    }
}

fn find_binary_dir(broodlink_dir: &Path) -> PathBuf {
    // Check release first, then debug
    let release = broodlink_dir.join("target/release");
    let probe = if cfg!(windows) {
        "beads-bridge.exe"
    } else {
        "beads-bridge"
    };
    if release.join(probe).exists() {
        return release;
    }
    broodlink_dir.join("target/debug")
}

async fn check_http(url: &str) -> bool {
    reqwest::Client::new()
        .get(url)
        .timeout(std::time::Duration::from_secs(2))
        .send()
        .await
        .is_ok()
}

async fn check_tcp(host: &str, port: u16) -> bool {
    tokio::net::TcpStream::connect((host, port)).await.is_ok()
}

async fn wait_for_http(url: &str, timeout_secs: u64) -> Result<()> {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(timeout_secs);
    while tokio::time::Instant::now() < deadline {
        if check_http(url).await {
            return Ok(());
        }
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }
    anyhow::bail!("timed out waiting for {url}")
}

async fn wait_for_tcp(host: &str, port: u16, timeout_secs: u64) -> Result<()> {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(timeout_secs);
    while tokio::time::Instant::now() < deadline {
        if check_tcp(host, port).await {
            return Ok(());
        }
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }
    anyhow::bail!("timed out waiting for {host}:{port}")
}
