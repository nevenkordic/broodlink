/*
 * Broodlink - Multi-agent AI orchestration system
 * Copyright (C) 2025–2026 Neven Kordic <neven@broodlink.ai>
 * SPDX-License-Identifier: AGPL-3.0-or-later
 */

//! Shared runtime utilities for Broodlink services.
//!
//! Provides common building blocks that are duplicated across services:
//! - [`CircuitBreaker`]: failure tracking with half-open recovery
//! - [`shutdown_signal`]: graceful SIGINT/SIGTERM handler
//! - [`connect_nats`]: cluster-aware NATS connection

use broodlink_config::RuntimeConfig;
use std::collections::{BTreeMap, HashMap};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::process::{Child, Command};
use tracing::{error, info};

// ---------------------------------------------------------------------------
// Circuit breaker
// ---------------------------------------------------------------------------

/// Thread-safe circuit breaker with three states: CLOSED → OPEN → HALF-OPEN.
///
/// After `threshold` consecutive failures, the circuit opens and rejects all
/// calls for `half_open_secs`. After that window, one probe attempt is allowed
/// through (half-open). A success resets the breaker; another failure re-opens.
pub struct CircuitBreaker {
    name: String,
    failure_count: AtomicU32,
    last_failure_epoch_ms: AtomicU64,
    half_open_probe_in_flight: AtomicBool,
    threshold: u32,
    half_open_secs: u64,
}

impl CircuitBreaker {
    /// Create a new circuit breaker.
    ///
    /// - `name`: used in error messages
    /// - `threshold`: number of failures before opening
    /// - `half_open_secs`: seconds before allowing a probe
    #[must_use]
    pub fn new(name: &str, threshold: u32, half_open_secs: u64) -> Self {
        Self {
            name: name.to_string(),
            failure_count: AtomicU32::new(0),
            last_failure_epoch_ms: AtomicU64::new(0),
            half_open_probe_in_flight: AtomicBool::new(false),
            threshold,
            half_open_secs,
        }
    }

    /// Returns `true` if the circuit is open (all calls should be rejected).
    ///
    /// When the half-open window is reached, exactly one probe is allowed
    /// through via an atomic compare-exchange. All other concurrent callers
    /// still see the circuit as open until the probe succeeds or fails.
    #[must_use]
    pub fn is_open(&self) -> bool {
        let failures = self.failure_count.load(Ordering::Relaxed);
        if failures < self.threshold {
            return false;
        }
        let last_ms = self.last_failure_epoch_ms.load(Ordering::Relaxed);
        let now_ms = now_epoch_ms();
        let elapsed_secs = (now_ms.saturating_sub(last_ms)) / 1000;
        if elapsed_secs >= self.half_open_secs {
            // Half-open window: allow exactly one probe through
            if self
                .half_open_probe_in_flight
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Relaxed)
                .is_ok()
            {
                return false; // this caller is the probe
            }
            // Another probe is already in flight — remain open
        }
        true
    }

    /// Record a successful operation — resets the failure counter and probe flag.
    pub fn record_success(&self) {
        self.failure_count.store(0, Ordering::Relaxed);
        self.half_open_probe_in_flight
            .store(false, Ordering::Release);
    }

    /// Record a failed operation — increments counter, updates timestamp,
    /// and resets the probe flag so the next half-open window can try again.
    pub fn record_failure(&self) {
        self.failure_count.fetch_add(1, Ordering::Relaxed);
        self.last_failure_epoch_ms
            .store(now_epoch_ms(), Ordering::Relaxed);
        self.half_open_probe_in_flight
            .store(false, Ordering::Release);
    }

    /// Returns `Ok(())` if the circuit is closed/half-open, or `Err(name)` if open.
    ///
    /// # Errors
    ///
    /// Returns the breaker name as a `String` when the circuit is open.
    pub fn check(&self) -> Result<(), String> {
        if self.is_open() {
            return Err(self.name.clone());
        }
        Ok(())
    }

    /// Name of this circuit breaker (for error messages).
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }
}

fn now_epoch_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

// ---------------------------------------------------------------------------
// Shutdown signal
// ---------------------------------------------------------------------------

/// Wait for SIGINT (ctrl-c) or SIGTERM, then return.
///
/// Use with `tokio::select!` or `axum::serve(...).with_graceful_shutdown(...)`.
pub async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .unwrap_or_else(|e| error!(error = %e, "ctrl-c handler failed"));
    };

    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut sig) => {
                sig.recv().await;
            }
            Err(e) => {
                error!(error = %e, "SIGTERM handler unavailable, relying on ctrl-c");
                std::future::pending::<()>().await;
            }
        }
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = ctrl_c => info!("received ctrl-c"),
        () = terminate => info!("received SIGTERM"),
    }
}

// ---------------------------------------------------------------------------
// NATS connection (cluster-aware)
// ---------------------------------------------------------------------------

/// Connect to NATS, using cluster URLs and token auth if configured.
///
/// When `nats_token` is `Some`, the connection uses token authentication.
/// Pass the resolved secret value; leave as `None` for unauthenticated
/// connections (dev only).
///
/// # Errors
///
/// Returns `async_nats::ConnectError` if the connection fails.
pub async fn connect_nats(
    config: &broodlink_config::NatsConfig,
    nats_token: Option<&str>,
) -> Result<async_nats::Client, async_nats::ConnectError> {
    let mut addrs: Vec<String> = vec![config.url.clone()];
    addrs.extend(config.cluster_urls.clone());

    let opts = if let Some(token) = nats_token {
        async_nats::ConnectOptions::with_token(token.to_string())
    } else {
        async_nats::ConnectOptions::new()
    };

    let client = if addrs.len() == 1 {
        opts.connect(&config.url).await?
    } else {
        opts.connect(addrs.as_slice()).await?
    };

    info!(
        url = %config.url,
        cluster_size = config.cluster_urls.len(),
        auth = nats_token.is_some(),
        "nats connected"
    );
    Ok(client)
}

// ---------------------------------------------------------------------------
// Isolated workers / pluggable runtimes
// ---------------------------------------------------------------------------

/// Isolation backend named in `[runtimes.<name>].backend`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IsolationBackend {
    Local,
    Docker,
    Ssh,
    RemoteIdle,
}

impl IsolationBackend {
    /// Parse a backend name from config or a `spawn_worker` argument.
    ///
    /// # Errors
    ///
    /// Returns [`RuntimeError::UnknownBackend`] when `name` is not one of
    /// `local`, `docker`, `ssh`, or `remote-idle`.
    pub fn parse(name: &str) -> Result<Self, RuntimeError> {
        match name {
            "local" => Ok(Self::Local),
            "docker" => Ok(Self::Docker),
            "ssh" => Ok(Self::Ssh),
            "remote-idle" => Ok(Self::RemoteIdle),
            other => Err(RuntimeError::UnknownBackend(other.to_string())),
        }
    }

    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::Docker => "docker",
            Self::Ssh => "ssh",
            Self::RemoteIdle => "remote-idle",
        }
    }
}

/// Errors from runtime selection, planning, or spawn.
#[derive(Debug, thiserror::Error)]
pub enum RuntimeError {
    #[error("unknown isolation backend: {0}")]
    UnknownBackend(String),
    #[error("unknown runtime name: {0}")]
    UnknownRuntime(String),
    #[error("runtime {0} is missing required field {1}")]
    MissingField(String, String),
    #[error("failed to spawn worker: {0}")]
    Spawn(String),
}

/// Worker job handed to a runtime backend.
#[derive(Debug, Clone)]
pub struct WorkerSpec {
    pub worker_id: String,
    pub parent_agent_id: String,
    pub child_agent_id: String,
    pub goal: String,
    pub allowed_tools: Vec<String>,
    pub timeout_secs: u64,
    pub jwt: String,
    pub bridge_url: String,
}

/// Planned argv/env for one backend. Audit payload is backend-agnostic.
#[derive(Debug, Clone)]
pub struct PlannedInvocation {
    pub runtime_name: String,
    pub backend: IsolationBackend,
    pub argv: Vec<String>,
    pub env: BTreeMap<String, String>,
    /// Task identity written to `audit_log` — identical across backends.
    pub audit: serde_json::Value,
}

/// Pick a named runtime, or the implicit `local` default.
///
/// `requested` may be a runtime name (`docker`) or a backend (`ssh`).
///
/// # Errors
///
/// Returns [`RuntimeError::UnknownRuntime`] or [`RuntimeError::UnknownBackend`].
pub fn select_runtime(
    runtimes: &HashMap<String, RuntimeConfig>,
    requested: Option<&str>,
) -> Result<(String, IsolationBackend, RuntimeConfig), RuntimeError> {
    match requested {
        None | Some("") | Some("local") => {
            if let Some((name, cfg)) = runtimes.iter().find(|(_, c)| c.backend == "local") {
                Ok((name.clone(), IsolationBackend::Local, cfg.clone()))
            } else {
                Ok((
                    "local".to_string(),
                    IsolationBackend::Local,
                    RuntimeConfig {
                        backend: "local".to_string(),
                        ..RuntimeConfig::default()
                    },
                ))
            }
        }
        Some(name) => {
            if let Some(cfg) = runtimes.get(name) {
                let backend = IsolationBackend::parse(&cfg.backend)?;
                Ok((name.to_string(), backend, cfg.clone()))
            } else {
                let backend = IsolationBackend::parse(name)?;
                let cfg = runtimes
                    .values()
                    .find(|c| c.backend == backend.as_str())
                    .cloned()
                    .unwrap_or(RuntimeConfig {
                        backend: backend.as_str().to_string(),
                        ..RuntimeConfig::default()
                    });
                Ok((name.to_string(), backend, cfg))
            }
        }
    }
}

/// Build the command a backend will run. Does not start a process.
///
/// # Errors
///
/// Returns [`RuntimeError::MissingField`] when an `ssh` runtime has no host.
pub fn plan_invocation(
    spec: &WorkerSpec,
    runtime_name: &str,
    backend: IsolationBackend,
    runtime: &RuntimeConfig,
) -> Result<PlannedInvocation, RuntimeError> {
    let mut env = BTreeMap::new();
    env.insert("BROODLINK_WORKER_ID".into(), spec.worker_id.clone());
    env.insert(
        "BROODLINK_WORKER_PARENT".into(),
        spec.parent_agent_id.clone(),
    );
    env.insert("BROODLINK_WORKER_AGENT".into(), spec.child_agent_id.clone());
    env.insert("BROODLINK_WORKER_GOAL".into(), spec.goal.clone());
    env.insert(
        "BROODLINK_WORKER_TOOLS".into(),
        spec.allowed_tools.join(","),
    );
    env.insert(
        "BROODLINK_WORKER_TIMEOUT".into(),
        spec.timeout_secs.to_string(),
    );
    env.insert("BROODLINK_WORKER_JWT".into(), spec.jwt.clone());
    env.insert("BROODLINK_BRIDGE_URL".into(), spec.bridge_url.clone());

    // Task identity only — backend is stored on the worker row, not here,
    // so local and docker produce identical audit rows for the same task.
    let audit = serde_json::json!({
        "event": "worker_spawn",
        "worker_id": spec.worker_id,
        "parent_agent_id": spec.parent_agent_id,
        "child_agent_id": spec.child_agent_id,
        "goal": spec.goal,
        "allowed_tools": spec.allowed_tools,
        "timeout_secs": spec.timeout_secs,
    });

    let argv = match backend {
        IsolationBackend::Local | IsolationBackend::RemoteIdle => {
            vec!["/bin/bash".into(), worker_script_path()]
        }
        IsolationBackend::Docker => {
            let image = runtime
                .image
                .clone()
                .unwrap_or_else(|| "curlimages/curl:8.13.0".to_string());
            let argv = vec![
                "docker".into(),
                "run".into(),
                "--rm".into(),
                "-e".into(),
                "BROODLINK_WORKER_JWT".into(),
                "-e".into(),
                "BROODLINK_BRIDGE_URL".into(),
                "-e".into(),
                "BROODLINK_WORKER_ID".into(),
                "-e".into(),
                "BROODLINK_WORKER_GOAL".into(),
                "-e".into(),
                "BROODLINK_WORKER_TOOLS".into(),
                image,
            ];
            let mut argv = argv;
            argv.push("sh".into());
            argv.push("-c".into());
            argv.push(docker_inner_script());
            argv
        }
        IsolationBackend::Ssh => {
            let host = runtime.host.as_deref().filter(|h| !h.is_empty()).ok_or(
                RuntimeError::MissingField(runtime_name.to_string(), "host".to_string()),
            )?;
            let target = match runtime.user.as_deref().filter(|u| !u.is_empty()) {
                Some(user) => format!("{user}@{host}"),
                None => host.to_string(),
            };
            vec![
                "ssh".into(),
                "-o".into(),
                "BatchMode=yes".into(),
                "-o".into(),
                "StrictHostKeyChecking=accept-new".into(),
                target,
                worker_script_path(),
            ]
        }
    };

    Ok(PlannedInvocation {
        runtime_name: runtime_name.to_string(),
        backend,
        argv,
        env,
        audit,
    })
}

fn worker_script_path() -> String {
    std::env::var("BROODLINK_WORKER_SCRIPT").unwrap_or_else(|_| {
        let here = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        here.join("../../scripts/broodlink-worker.sh")
            .canonicalize()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|_| "scripts/broodlink-worker.sh".to_string())
    })
}

fn docker_inner_script() -> String {
    // Reads JWT from the environment — never interpolated into the plan string.
    "curl -sS -X POST -H \"Authorization: Bearer ${BROODLINK_WORKER_JWT}\" -H \"Content-Type: application/json\" -d '{\"params\":{}}' \"${BROODLINK_BRIDGE_URL%/}/api/v1/tool/ping\"".to_string()
}

/// Whether the `docker` CLI is on PATH.
#[must_use]
pub fn docker_available() -> bool {
    std::process::Command::new("docker")
        .arg("version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// Spawn a planned invocation. Caller must `wait` or `wait_child` the handle;
/// dropping it kills the process.
///
/// # Errors
///
/// Returns [`RuntimeError::Spawn`] when the process cannot be started.
pub async fn spawn_planned(plan: &PlannedInvocation) -> Result<Child, RuntimeError> {
    if plan.argv.is_empty() {
        return Err(RuntimeError::Spawn("empty argv".to_string()));
    }
    if plan.backend == IsolationBackend::Docker && !docker_available() {
        return Err(RuntimeError::Spawn(
            "docker CLI is not available".to_string(),
        ));
    }
    let mut cmd = Command::new(&plan.argv[0]);
    cmd.args(&plan.argv[1..])
        .envs(&plan.env)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    cmd.spawn().map_err(|e| RuntimeError::Spawn(e.to_string()))
}

/// Wait for a child, treating overrun as a timeout error.
///
/// # Errors
///
/// Returns [`RuntimeError::Spawn`] on I/O failure or when the timeout is hit.
pub async fn wait_child(child: &mut Child, timeout_secs: u64) -> Result<i32, RuntimeError> {
    match tokio::time::timeout(Duration::from_secs(timeout_secs.max(1)), child.wait()).await {
        Ok(Ok(status)) => Ok(status.code().unwrap_or(1)),
        Ok(Err(e)) => Err(RuntimeError::Spawn(e.to_string())),
        Err(_) => {
            let _ = child.kill().await;
            Err(RuntimeError::Spawn("worker timed out".to_string()))
        }
    }
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn test_circuit_breaker_starts_closed() {
        let cb = CircuitBreaker::new("test", 3, 30);
        assert!(!cb.is_open());
        assert!(cb.check().is_ok());
    }

    #[test]
    fn test_circuit_breaker_opens_after_threshold() {
        let cb = CircuitBreaker::new("test", 3, 30);
        cb.record_failure();
        cb.record_failure();
        assert!(!cb.is_open(), "should still be closed at 2 failures");
        cb.record_failure();
        assert!(cb.is_open(), "should be open at 3 failures");
        assert!(cb.check().is_err());
    }

    #[test]
    fn test_circuit_breaker_resets_on_success() {
        let cb = CircuitBreaker::new("test", 2, 30);
        cb.record_failure();
        cb.record_failure();
        assert!(cb.is_open());
        cb.record_success();
        assert!(!cb.is_open());
        assert!(cb.check().is_ok());
    }

    #[test]
    fn test_circuit_breaker_half_open_after_timeout() {
        let cb = CircuitBreaker::new("test", 1, 0); // 0-second recovery
        cb.record_failure();
        // With 0-second recovery, should immediately be half-open
        assert!(!cb.is_open(), "should be half-open after recovery period");
    }

    #[test]
    fn test_circuit_breaker_check_returns_name() {
        let cb = CircuitBreaker::new("qdrant", 1, 9999);
        cb.record_failure();
        let err = cb.check().unwrap_err();
        assert_eq!(err, "qdrant");
    }

    fn sample_spec() -> WorkerSpec {
        WorkerSpec {
            worker_id: "w-1".into(),
            parent_agent_id: "claude".into(),
            child_agent_id: "worker-w-1".into(),
            goal: "summarise the inbox".into(),
            allowed_tools: vec!["ping".into(), "list_formulas".into()],
            timeout_secs: 60,
            jwt: "test-jwt".into(),
            bridge_url: "http://127.0.0.1:3310".into(),
        }
    }

    #[test]
    fn test_select_runtime_defaults_local() {
        let (name, backend, cfg) = select_runtime(&HashMap::new(), None).unwrap();
        assert_eq!(name, "local");
        assert_eq!(backend, IsolationBackend::Local);
        assert_eq!(cfg.backend, "local");
    }

    #[test]
    fn test_select_runtime_by_backend_name() {
        let (name, backend, _) = select_runtime(&HashMap::new(), Some("docker")).unwrap();
        assert_eq!(name, "docker");
        assert_eq!(backend, IsolationBackend::Docker);
    }

    #[test]
    fn test_select_runtime_unknown() {
        let err = select_runtime(&HashMap::new(), Some("kubernetes")).unwrap_err();
        assert!(matches!(err, RuntimeError::UnknownBackend(_)));
    }

    #[test]
    fn test_plan_local_and_docker_share_audit() {
        let spec = sample_spec();
        let local_cfg = RuntimeConfig {
            backend: "local".into(),
            ..RuntimeConfig::default()
        };
        let docker_cfg = RuntimeConfig {
            backend: "docker".into(),
            image: Some("broodlink/worker:test".into()),
            ..RuntimeConfig::default()
        };
        let local = plan_invocation(&spec, "local", IsolationBackend::Local, &local_cfg).unwrap();
        let docker =
            plan_invocation(&spec, "docker", IsolationBackend::Docker, &docker_cfg).unwrap();
        assert_eq!(local.audit, docker.audit);
        assert_eq!(local.audit["event"], "worker_spawn");
        assert_eq!(local.audit["goal"], "summarise the inbox");
        assert!(local.audit.get("backend").is_none());
        assert_eq!(docker.argv[0], "docker");
        assert!(docker.argv.contains(&"broodlink/worker:test".into()));
        assert_eq!(local.argv[0], "/bin/bash");
    }

    #[test]
    fn test_plan_ssh_requires_host() {
        let spec = sample_spec();
        let cfg = RuntimeConfig {
            backend: "ssh".into(),
            ..RuntimeConfig::default()
        };
        let err = plan_invocation(&spec, "lab", IsolationBackend::Ssh, &cfg).unwrap_err();
        assert!(matches!(err, RuntimeError::MissingField(_, _)));
    }

    #[test]
    fn test_plan_ssh_target() {
        let spec = sample_spec();
        let cfg = RuntimeConfig {
            backend: "ssh".into(),
            host: Some("lab.example".into()),
            user: Some("broodlink".into()),
            ..RuntimeConfig::default()
        };
        let plan = plan_invocation(&spec, "lab", IsolationBackend::Ssh, &cfg).unwrap();
        assert!(plan.argv.contains(&"broodlink@lab.example".into()));
        assert_eq!(plan.audit, {
            let local_cfg = RuntimeConfig {
                backend: "local".into(),
                ..RuntimeConfig::default()
            };
            plan_invocation(&spec, "local", IsolationBackend::Local, &local_cfg)
                .unwrap()
                .audit
        });
    }
}
