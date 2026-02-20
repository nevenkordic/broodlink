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

use std::process;
use std::sync::Arc;
use std::time::{Duration, Instant};

use broodlink_config::Config;
use broodlink_secrets::SecretsProvider;
use chrono::Utc;
use serde::Serialize;
use sqlx::mysql::MySqlPoolOptions;
use sqlx::postgres::PgPoolOptions;
use sqlx::{MySqlPool, PgPool, Row};
use tracing::{error, info, warn};
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

const SERVICE_NAME: &str = "heartbeat";
const SERVICE_VERSION: &str = env!("CARGO_PKG_VERSION");
const HEARTBEAT_INTERVAL_SECS: u64 = 300; // 5 minutes

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
    #[error("command error: {0}")]
    Command(String),
    #[error("parse error: {0}")]
    Parse(String),
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

// ---------------------------------------------------------------------------
// NATS payload types
// ---------------------------------------------------------------------------

#[derive(Serialize)]
struct HealthPayload {
    service: &'static str,
    version: &'static str,
    trace_id: String,
    uptime_seconds: u64,
    dolt_ok: bool,
    postgres_ok: bool,
    nats_ok: bool,
    timestamp: String,
}

#[derive(Serialize)]
struct StatusPayload {
    service: &'static str,
    trace_id: String,
    cycle_duration_ms: u128,
    dolt_writes: i64,
    beads_synced: usize,
    agents_activated: u64,
    agents_deactivated: u64,
    daily_summary_updated: bool,
    timestamp: String,
}

// ---------------------------------------------------------------------------
// Beads issue (parsed from `bd issue list --json`)
// ---------------------------------------------------------------------------

#[derive(serde::Deserialize, Debug)]
struct BeadsIssue {
    #[serde(alias = "bead_id", alias = "id")]
    bead_id: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    assignee: Option<String>,
    #[serde(default)]
    convoy_id: Option<String>,
    #[serde(default)]
    dependencies: Option<serde_json::Value>,
    #[serde(default)]
    formula: Option<String>,
    #[serde(default)]
    created_at: Option<String>,
    #[serde(default)]
    updated_at: Option<String>,
}

// ---------------------------------------------------------------------------
// Application state
// ---------------------------------------------------------------------------

struct AppState {
    dolt: MySqlPool,
    pg: PgPool,
    nats: async_nats::Client,
    config: Arc<Config>,
    start_time: Instant,
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

    let shared = Arc::new(state);

    info!(
        interval_secs = HEARTBEAT_INTERVAL_SECS,
        "entering heartbeat loop"
    );

    run_loop(shared).await;

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
            None,
        )?;
        Arc::from(provider)
    };

    // Resolve database passwords from secrets
    let dolt_password = secrets.get(&config.dolt.password_key).await?;
    let pg_password = secrets.get(&config.postgres.password_key).await?;

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

    Ok(AppState {
        dolt,
        pg,
        nats,
        config,
        start_time: Instant::now(),
    })
}

// ---------------------------------------------------------------------------
// Main loop: tokio::select! on interval + shutdown signal
// ---------------------------------------------------------------------------

async fn run_loop(state: Arc<AppState>) {
    let mut interval = tokio::time::interval(Duration::from_secs(HEARTBEAT_INTERVAL_SECS));
    // The first tick fires immediately; consume it and run a cycle right away.
    interval.tick().await;

    // Run the first cycle immediately on startup
    if let Err(e) = run_cycle(&state).await {
        error!(error = %e, "heartbeat cycle failed");
    }

    loop {
        tokio::select! {
            _ = interval.tick() => {
                if let Err(e) = run_cycle(&state).await {
                    error!(error = %e, "heartbeat cycle failed");
                }
            }
            () = shutdown_signal() => {
                info!("received shutdown signal, draining");
                break;
            }
        }
    }
}

async fn shutdown_signal() {
    broodlink_runtime::shutdown_signal().await;
}

// ---------------------------------------------------------------------------
// Single heartbeat cycle
// ---------------------------------------------------------------------------

async fn run_cycle(state: &AppState) -> Result<(), BroodlinkError> {
    let cycle_start = Instant::now();
    let trace_id = Uuid::new_v4().to_string();
    let env = &state.config.broodlink.env;
    let prefix = &state.config.nats.subject_prefix;

    info!(trace_id = %trace_id, "heartbeat cycle start");

    // -----------------------------------------------------------------------
    // 1. Count Dolt writes since last commit (used in commit message)
    // -----------------------------------------------------------------------
    let dolt_writes = count_dolt_writes(&state.dolt).await;

    // -----------------------------------------------------------------------
    // 2. Dolt commit
    // -----------------------------------------------------------------------
    let commit_msg = format!(
        "heartbeat(sync): {} writes [trace:{}]",
        dolt_writes, trace_id
    );
    if let Err(e) = dolt_commit(&state.dolt, &commit_msg).await {
        warn!(error = %e, "dolt commit failed (may be empty)");
    }

    // -----------------------------------------------------------------------
    // 3. Sync Beads issues
    // -----------------------------------------------------------------------
    let beads_synced = match sync_beads(&state.dolt, &state.config.beads.bd_binary).await {
        Ok(n) => n,
        Err(e) => {
            warn!(error = %e, "beads sync failed");
            0
        }
    };

    // -----------------------------------------------------------------------
    // 4. Update daily summary
    // -----------------------------------------------------------------------
    let daily_ok = match update_daily_summary(&state.dolt, &state.pg).await {
        Ok(()) => true,
        Err(e) => {
            warn!(error = %e, "daily summary update failed");
            false
        }
    };

    // -----------------------------------------------------------------------
    // 5a. Activate agents with recent activity (catches pymysql agents)
    // -----------------------------------------------------------------------
    let agents_activated = match activate_recent_agents(&state.dolt, &state.pg).await {
        Ok(n) => n,
        Err(e) => {
            warn!(error = %e, "agent activation check failed");
            0
        }
    };

    // -----------------------------------------------------------------------
    // 5b. Mark stale agents inactive
    // -----------------------------------------------------------------------
    let agents_deactivated = match deactivate_stale_agents(&state.dolt).await {
        Ok(n) => n,
        Err(e) => {
            warn!(error = %e, "agent deactivation failed");
            0
        }
    };

    // -----------------------------------------------------------------------
    // 5c. Expire overdue approval gates
    // -----------------------------------------------------------------------
    let gates_expired = match expire_approval_gates(&state.pg).await {
        Ok(n) => {
            if n > 0 {
                info!(expired = n, "expired overdue approval gates");
                let subj = format!("{prefix}.{env}.approvals.expired");
                publish_nats(&state.nats, &subj, &serde_json::json!({
                    "service": SERVICE_NAME,
                    "expired_count": n,
                    "timestamp": Utc::now().to_rfc3339(),
                })).await;
            }
            n
        }
        Err(e) => {
            warn!(error = %e, "approval gate expiry check failed");
            0
        }
    };
    let _ = gates_expired; // used for logging only

    // -----------------------------------------------------------------------
    // 5d. Compute and upsert agent_metrics (Postgres)
    // -----------------------------------------------------------------------
    let metrics_updated = match compute_agent_metrics(&state.dolt, &state.pg).await {
        Ok(n) => {
            if n > 0 {
                info!(agents = n, "agent_metrics updated");
            }
            n
        }
        Err(e) => {
            warn!(error = %e, "agent_metrics computation failed");
            0
        }
    };
    let _ = metrics_updated; // used for logging only

    // -----------------------------------------------------------------------
    // 5e. Sync memory_search_index from Dolt-direct writes (Postgres)
    // -----------------------------------------------------------------------
    if let Err(e) = sync_memory_search_index(&state.dolt, &state.pg).await {
        warn!(error = %e, "memory search index sync failed");
    }

    // -----------------------------------------------------------------------
    // 5f. Sync unprocessed memories for KG extraction
    // -----------------------------------------------------------------------
    if let Err(e) = sync_kg_unprocessed_memories(&state.dolt, &state.pg).await {
        warn!(error = %e, "kg unprocessed memory sync failed");
    }

    // -----------------------------------------------------------------------
    // 6. Publish broodlink.<env>.health (NATS)
    // -----------------------------------------------------------------------
    let dolt_ok = sqlx::query("SELECT 1").execute(&state.dolt).await.is_ok();
    let pg_ok = sqlx::query("SELECT 1").execute(&state.pg).await.is_ok();
    let nats_ok = state.nats.flush().await.is_ok();

    let health = HealthPayload {
        service: SERVICE_NAME,
        version: SERVICE_VERSION,
        trace_id: trace_id.clone(),
        uptime_seconds: state.start_time.elapsed().as_secs(),
        dolt_ok,
        postgres_ok: pg_ok,
        nats_ok,
        timestamp: Utc::now().to_rfc3339(),
    };

    let health_subject = format!("{prefix}.{env}.health");
    publish_nats(&state.nats, &health_subject, &health).await;

    // -----------------------------------------------------------------------
    // 7. Publish broodlink.<env>.status (NATS)
    // -----------------------------------------------------------------------
    let cycle_duration_ms = cycle_start.elapsed().as_millis();

    let status = StatusPayload {
        service: SERVICE_NAME,
        trace_id: trace_id.clone(),
        cycle_duration_ms,
        dolt_writes,
        beads_synced,
        agents_activated,
        agents_deactivated,
        daily_summary_updated: daily_ok,
        timestamp: Utc::now().to_rfc3339(),
    };

    let status_subject = format!("{prefix}.{env}.status");
    publish_nats(&state.nats, &status_subject, &status).await;

    // -----------------------------------------------------------------------
    // 8. Write audit_log row (Postgres)
    // -----------------------------------------------------------------------
    if let Err(e) = write_audit_log(
        &state.pg,
        &trace_id,
        SERVICE_NAME,
        cycle_duration_ms,
    )
    .await
    {
        warn!(error = %e, "failed to write audit log");
    }

    // -----------------------------------------------------------------------
    // 9. If prod profile: write residency_log row (Postgres)
    // -----------------------------------------------------------------------
    if state.config.profile.audit_residency {
        if let Err(e) = write_residency_log(
            &state.pg,
            &trace_id,
            &state.config.residency.region,
            &state.config.profile.name,
        )
        .await
        {
            warn!(error = %e, "failed to write residency log");
        }
    }

    info!(
        trace_id = %trace_id,
        dolt_writes = dolt_writes,
        beads_synced = beads_synced,
        agents_activated = agents_activated,
        agents_deactivated = agents_deactivated,
        daily_summary_updated = daily_ok,
        cycle_ms = cycle_duration_ms,
        "heartbeat cycle complete"
    );

    Ok(())
}

// ---------------------------------------------------------------------------
// Step helpers
// ---------------------------------------------------------------------------

/// Count uncommitted Dolt writes using `DOLT_STATUS`.
async fn count_dolt_writes(dolt: &MySqlPool) -> i64 {
    let row = sqlx::query("SELECT COUNT(*) AS cnt FROM dolt_status")
        .fetch_one(dolt)
        .await;

    match row {
        Ok(r) => r.try_get::<i64, _>("cnt").unwrap_or(0),
        Err(_) => 0,
    }
}

/// Commit all staged Dolt changes. This calls `DOLT_ADD` then `DOLT_COMMIT`.
/// If there are no changes, the stored procedure may error -- we treat that
/// as a non-fatal warning.
async fn dolt_commit(dolt: &MySqlPool, message: &str) -> Result<(), BroodlinkError> {
    // Stage all tables
    sqlx::query("CALL DOLT_ADD('-A')")
        .execute(dolt)
        .await?;

    // Commit with message. Using `-m` flag.
    sqlx::query("CALL DOLT_COMMIT('-m', ?)")
        .bind(message)
        .execute(dolt)
        .await?;

    info!(message = %message, "dolt commit succeeded");
    Ok(())
}

/// Run `bd issue list --json`, parse the output, and upsert each issue
/// into the `beads_issues` table in Dolt.
async fn sync_beads(dolt: &MySqlPool, bd_binary: &str) -> Result<usize, BroodlinkError> {
    let output = tokio::process::Command::new(bd_binary)
        .arg("issue")
        .arg("list")
        .arg("--json")
        .output()
        .await
        .map_err(|e| BroodlinkError::Command(format!("failed to run bd: {e}")))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(BroodlinkError::Command(format!(
            "bd issue list exited with {}: {stderr}",
            output.status.code().unwrap_or(-1)
        )));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    if stdout.trim().is_empty() {
        return Ok(0);
    }

    let issues: Vec<BeadsIssue> = serde_json::from_str(&stdout)
        .map_err(|e| BroodlinkError::Parse(format!("failed to parse bd output: {e}")))?;

    let count = issues.len();

    for issue in &issues {
        let deps_json = issue
            .dependencies
            .as_ref()
            .map(serde_json::Value::to_string);

        sqlx::query(
            "INSERT INTO beads_issues
                (bead_id, title, description, status, assignee,
                 convoy_id, dependencies, formula, created_at, updated_at, synced_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, NOW())
             ON DUPLICATE KEY UPDATE
                title       = VALUES(title),
                description = VALUES(description),
                status      = VALUES(status),
                assignee    = VALUES(assignee),
                convoy_id   = VALUES(convoy_id),
                dependencies= VALUES(dependencies),
                formula     = VALUES(formula),
                updated_at  = VALUES(updated_at),
                synced_at   = NOW()",
        )
        .bind(&issue.bead_id)
        .bind(&issue.title)
        .bind(&issue.description)
        .bind(&issue.status)
        .bind(&issue.assignee)
        .bind(&issue.convoy_id)
        .bind(&deps_json)
        .bind(&issue.formula)
        .bind(&issue.created_at)
        .bind(&issue.updated_at)
        .execute(dolt)
        .await?;
    }

    info!(count = count, "beads issues synced");
    Ok(count)
}

/// Gather today's counts and upsert a `daily_summary` row.
async fn update_daily_summary(dolt: &MySqlPool, pg: &PgPool) -> Result<(), BroodlinkError> {
    let today = Utc::now().format("%Y-%m-%d").to_string();

    // Tasks completed today (Postgres)
    let tasks_completed: i64 = sqlx::query(
        "SELECT COUNT(*) AS cnt FROM task_queue
         WHERE status = 'completed'
           AND completed_at::date = CURRENT_DATE",
    )
    .fetch_one(pg)
    .await
    .and_then(|r| r.try_get::<i64, _>("cnt"))
    .unwrap_or(0);

    // Decisions made today (Dolt)
    let decisions_made: i64 = sqlx::query(
        "SELECT COUNT(*) AS cnt FROM decisions
         WHERE DATE(created_at) = CURDATE()",
    )
    .fetch_one(dolt)
    .await
    .and_then(|r| r.try_get::<i64, _>("cnt"))
    .unwrap_or(0);

    // Memories stored today (Dolt)
    let memories_stored: i64 = sqlx::query(
        "SELECT COUNT(*) AS cnt FROM agent_memory
         WHERE DATE(created_at) = CURDATE()",
    )
    .fetch_one(dolt)
    .await
    .and_then(|r| r.try_get::<i64, _>("cnt"))
    .unwrap_or(0);

    // Build summary text
    let summary_text = format!(
        "Tasks completed: {tasks_completed}, Decisions made: {decisions_made}, Memories stored: {memories_stored}"
    );

    // Upsert into daily_summary (Dolt)
    sqlx::query(
        "INSERT INTO daily_summary
            (summary_date, summary_text, tasks_completed, decisions_made, memories_stored, created_at)
         VALUES (?, ?, ?, ?, ?, NOW())
         ON DUPLICATE KEY UPDATE
            summary_text    = VALUES(summary_text),
            tasks_completed = VALUES(tasks_completed),
            decisions_made  = VALUES(decisions_made),
            memories_stored = VALUES(memories_stored)",
    )
    .bind(&today)
    .bind(&summary_text)
    .bind(tasks_completed)
    .bind(decisions_made)
    .bind(memories_stored)
    .execute(dolt)
    .await?;

    info!(
        date = %today,
        tasks = tasks_completed,
        decisions = decisions_made,
        memories = memories_stored,
        "daily summary updated"
    );
    Ok(())
}

/// Activate agents that have recent activity in either Dolt or Postgres,
/// even if they bypass beads-bridge (e.g. pymysql agents writing directly
/// to Dolt). Checks audit_log (Postgres) and agent_memory (Dolt) for
/// activity within the last 15 minutes.
async fn activate_recent_agents(dolt: &MySqlPool, pg: &PgPool) -> Result<u64, BroodlinkError> {
    // Agents with recent audit_log entries in Postgres (beads-bridge users)
    let pg_active: Vec<(String,)> = sqlx::query_as(
        "SELECT DISTINCT agent_id FROM audit_log
         WHERE created_at > NOW() - INTERVAL '15 minutes'",
    )
    .fetch_all(pg)
    .await
    .unwrap_or_default();

    // Agents with recent memory writes in Dolt (pymysql users)
    let dolt_active: Vec<(String,)> = sqlx::query_as(
        "SELECT DISTINCT agent_name FROM agent_memory
         WHERE updated_at > NOW() - INTERVAL 15 MINUTE",
    )
    .fetch_all(dolt)
    .await
    .unwrap_or_default();

    let mut activated = 0u64;
    let mut seen = std::collections::HashSet::new();

    for (agent_id,) in pg_active.into_iter().chain(dolt_active.into_iter()) {
        if !seen.insert(agent_id.clone()) {
            continue;
        }
        let result = sqlx::query(
            "UPDATE agent_profiles
             SET active = true, last_seen = NOW()
             WHERE agent_id = ? AND active = false",
        )
        .bind(&agent_id)
        .execute(dolt)
        .await?;
        activated += result.rows_affected();
    }

    if activated > 0 {
        info!(count = activated, "activated agents with recent activity");
    }

    Ok(activated)
}

/// Mark agents as inactive if their `last_seen` timestamp is older than
/// one hour.
async fn deactivate_stale_agents(dolt: &MySqlPool) -> Result<u64, BroodlinkError> {
    let result = sqlx::query(
        "UPDATE agent_profiles
         SET active = false
         WHERE active = true
           AND last_seen IS NOT NULL
           AND last_seen < NOW() - INTERVAL 1 HOUR",
    )
    .execute(dolt)
    .await?;

    let affected = result.rows_affected();
    if affected > 0 {
        warn!(count = affected, "deactivated stale agents");
    }

    Ok(affected)
}

/// Expire pending approval gates past their expires_at, and update linked tasks.
async fn expire_approval_gates(pg: &PgPool) -> Result<u64, BroodlinkError> {
    // Expire gates
    let result = sqlx::query(
        "UPDATE approval_gates SET status = 'expired'
         WHERE status = 'pending' AND expires_at IS NOT NULL AND expires_at < NOW()",
    )
    .execute(pg)
    .await?;

    let affected = result.rows_affected();

    if affected > 0 {
        // Also expire linked tasks stuck in awaiting_approval
        sqlx::query(
            "UPDATE task_queue SET status = 'expired'
             WHERE status = 'awaiting_approval'
               AND approval_id IN (
                 SELECT id FROM approval_gates WHERE status = 'expired'
               )",
        )
        .execute(pg)
        .await
        .ok();
    }

    Ok(affected)
}

/// Compute performance metrics for all active agents and upsert into
/// `agent_metrics` (Postgres). Returns the number of agents updated.
async fn compute_agent_metrics(dolt: &MySqlPool, pg: &PgPool) -> Result<usize, BroodlinkError> {
    // Get all active agents from Dolt
    let active_agents: Vec<(String,)> = sqlx::query_as(
        "SELECT agent_id FROM agent_profiles WHERE active = true",
    )
    .fetch_all(dolt)
    .await?;

    if active_agents.is_empty() {
        return Ok(0);
    }

    let mut updated = 0usize;

    for (agent_id,) in &active_agents {
        // Completed tasks count
        let completed: i64 = sqlx::query(
            "SELECT COUNT(*) AS cnt FROM task_queue
             WHERE assigned_agent = $1 AND status = 'completed'",
        )
        .bind(agent_id)
        .fetch_one(pg)
        .await
        .and_then(|r| r.try_get::<i64, _>("cnt"))
        .unwrap_or(0);

        // Failed tasks count
        let failed: i64 = sqlx::query(
            "SELECT COUNT(*) AS cnt FROM task_queue
             WHERE assigned_agent = $1 AND status = 'failed'",
        )
        .bind(agent_id)
        .fetch_one(pg)
        .await
        .and_then(|r| r.try_get::<i64, _>("cnt"))
        .unwrap_or(0);

        // Average duration of completed tasks (ms)
        let avg_duration: i32 = sqlx::query(
            "SELECT COALESCE(AVG(EXTRACT(EPOCH FROM (completed_at - claimed_at)) * 1000), 0)::int
             AS avg_ms FROM task_queue
             WHERE assigned_agent = $1 AND status = 'completed'
               AND completed_at IS NOT NULL AND claimed_at IS NOT NULL",
        )
        .bind(agent_id)
        .fetch_one(pg)
        .await
        .and_then(|r| r.try_get::<i32, _>("avg_ms"))
        .unwrap_or(0);

        // Current load (claimed but not yet completed/failed)
        let current_load: i64 = sqlx::query(
            "SELECT COUNT(*) AS cnt FROM task_queue
             WHERE assigned_agent = $1 AND status = 'claimed'",
        )
        .bind(agent_id)
        .fetch_one(pg)
        .await
        .and_then(|r| r.try_get::<i64, _>("cnt"))
        .unwrap_or(0);

        // Compute success rate
        let total = completed + failed;
        let success_rate: f32 = if total > 0 {
            completed as f32 / total as f32
        } else {
            1.0 // No history = assume good
        };

        let completed_i32 = i32::try_from(completed).unwrap_or(i32::MAX);
        let failed_i32 = i32::try_from(failed).unwrap_or(i32::MAX);
        let load_i32 = i32::try_from(current_load).unwrap_or(i32::MAX);

        // Upsert into agent_metrics
        sqlx::query(
            "INSERT INTO agent_metrics
                (agent_id, tasks_completed, tasks_failed, avg_duration_ms, current_load, success_rate, updated_at)
             VALUES ($1, $2, $3, $4, $5, $6, NOW())
             ON CONFLICT (agent_id) DO UPDATE SET
                tasks_completed = EXCLUDED.tasks_completed,
                tasks_failed    = EXCLUDED.tasks_failed,
                avg_duration_ms = EXCLUDED.avg_duration_ms,
                current_load    = EXCLUDED.current_load,
                success_rate    = EXCLUDED.success_rate,
                updated_at      = NOW()",
        )
        .bind(agent_id)
        .bind(completed_i32)
        .bind(failed_i32)
        .bind(avg_duration)
        .bind(load_i32)
        .bind(success_rate)
        .execute(pg)
        .await?;

        updated += 1;
    }

    Ok(updated)
}

/// Write an audit_log row to Postgres.
async fn write_audit_log(
    pg: &PgPool,
    trace_id: &str,
    service: &str,
    duration_ms: u128,
) -> Result<(), BroodlinkError> {
    let duration_i32: i32 = i32::try_from(duration_ms).unwrap_or(i32::MAX);

    sqlx::query(
        "INSERT INTO audit_log
            (trace_id, agent_id, service, operation, result_status, result_summary, duration_ms, created_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7, NOW())",
    )
    .bind(trace_id)
    .bind(SERVICE_NAME)
    .bind(service)
    .bind("heartbeat_cycle")
    .bind("ok")
    .bind("periodic sync cycle completed")
    .bind(duration_i32)
    .execute(pg)
    .await?;

    Ok(())
}

/// Write a residency_log row (prod profile only) to Postgres.
async fn write_residency_log(
    pg: &PgPool,
    trace_id: &str,
    region: &str,
    profile: &str,
) -> Result<(), BroodlinkError> {
    let services = serde_json::json!({
        "heartbeat": SERVICE_VERSION,
        "dolt": "connected",
        "postgres": "connected",
        "nats": "connected",
    });

    sqlx::query(
        "INSERT INTO residency_log (trace_id, region, profile, services, checked_at)
         VALUES ($1, $2, $3, $4, NOW())",
    )
    .bind(trace_id)
    .bind(region)
    .bind(profile)
    .bind(&services)
    .execute(pg)
    .await?;

    Ok(())
}

/// Sync Dolt agent_memory → Postgres memory_search_index for direct-write catch-up.
async fn sync_memory_search_index(
    dolt: &MySqlPool,
    pg: &PgPool,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // Find the latest updated_at in Postgres search index
    let latest: Option<String> = sqlx::query_as::<_, (Option<String>,)>(
        "SELECT MAX(updated_at::text) FROM memory_search_index",
    )
    .fetch_one(pg)
    .await
    .ok()
    .and_then(|r| r.0);

    let since = latest.unwrap_or_else(|| "1970-01-01 00:00:00".to_string());

    // Fetch recent Dolt memories newer than our latest sync point
    let rows = sqlx::query_as::<_, (i64, String, String, String, Option<serde_json::Value>, String, String)>(
        "SELECT id, agent_name, topic, content, tags, CAST(created_at AS CHAR), CAST(updated_at AS CHAR)
         FROM agent_memory
         WHERE updated_at > ?
         ORDER BY updated_at ASC
         LIMIT 100",
    )
    .bind(&since)
    .fetch_all(dolt)
    .await?;

    if rows.is_empty() {
        return Ok(());
    }

    info!(count = rows.len(), "syncing memory_search_index from Dolt");

    for (memory_id, agent_id, topic, content, tags, created_at, updated_at) in &rows {
        // Convert JSON array tags to comma-separated string for Postgres
        let tags_str: Option<String> = tags.as_ref().and_then(|v| {
            v.as_array().map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            })
        });

        if let Err(e) = sqlx::query(
            "INSERT INTO memory_search_index (memory_id, agent_id, topic, content, tags, created_at, updated_at)
             VALUES ($1, $2, $3, $4, $5, $6::timestamptz, $7::timestamptz)
             ON CONFLICT (agent_id, topic) DO UPDATE SET
                 content = EXCLUDED.content,
                 tags = EXCLUDED.tags,
                 memory_id = EXCLUDED.memory_id,
                 created_at = EXCLUDED.created_at,
                 updated_at = EXCLUDED.updated_at",
        )
        .bind(memory_id)
        .bind(agent_id)
        .bind(topic)
        .bind(content)
        .bind(&tags_str)
        .bind(created_at)
        .bind(updated_at)
        .execute(pg)
        .await
        {
            warn!(error = %e, topic = %topic, "sync: failed to upsert memory_search_index");
        }
    }

    Ok(())
}

/// Sync unprocessed Dolt memories for knowledge graph extraction via outbox.
async fn sync_kg_unprocessed_memories(
    dolt: &MySqlPool,
    pg: &PgPool,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // Find max memory_id already linked in kg_entity_memories
    let max_processed: Option<i64> = sqlx::query_as::<_, (Option<i64>,)>(
        "SELECT MAX(memory_id) FROM kg_entity_memories",
    )
    .fetch_one(pg)
    .await
    .ok()
    .and_then(|r| r.0);

    let since_id = max_processed.unwrap_or(0);

    // Fetch Dolt memories with id > max processed
    let rows = sqlx::query_as::<_, (i64, String, String, String, Option<serde_json::Value>)>(
        "SELECT id, agent_name, topic, content, tags
         FROM agent_memory
         WHERE id > ?
         ORDER BY id ASC
         LIMIT 50",
    )
    .bind(since_id)
    .fetch_all(dolt)
    .await?;

    if rows.is_empty() {
        return Ok(());
    }

    let mut queued = 0u64;
    for (id, agent_name, topic, content, tags) in &rows {
        let tags_str: String = tags
            .as_ref()
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .unwrap_or_default();

        let payload = serde_json::json!({
            "topic": topic,
            "content": content,
            "agent_name": agent_name,
            "memory_id": id.to_string(),
            "tags": tags_str,
        });

        let trace_id = Uuid::new_v4().to_string();
        if let Err(e) = sqlx::query(
            "INSERT INTO outbox (trace_id, operation, payload, status, created_at)
             VALUES ($1, 'kg_extract', $2, 'pending', NOW())",
        )
        .bind(&trace_id)
        .bind(&payload)
        .execute(pg)
        .await
        {
            warn!(error = %e, memory_id = id, "failed to queue kg_extract outbox row");
        } else {
            queued += 1;
        }
    }

    if queued > 0 {
        info!(queued = queued, "queued unprocessed memories for kg extraction");
    }

    Ok(())
}

/// Publish a JSON payload to NATS (best-effort, logs warning on failure).
async fn publish_nats<T: Serialize>(
    nats: &async_nats::Client,
    subject: &str,
    payload: &T,
) {
    match serde_json::to_vec(payload) {
        Ok(bytes) => {
            if let Err(e) = nats.publish(subject.to_string(), bytes.into()).await {
                warn!(error = %e, subject = %subject, "failed to publish nats message");
            }
        }
        Err(e) => {
            warn!(error = %e, subject = %subject, "failed to serialize nats payload");
        }
    }
}
