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
            sc.infisical_token.as_deref(),
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

    let cycle_timeout = Duration::from_secs(state.config.heartbeat.cycle_timeout_secs);

    // Run the first cycle immediately on startup
    match tokio::time::timeout(cycle_timeout, run_cycle(&state)).await {
        Ok(Err(e)) => error!(error = %e, "heartbeat cycle failed"),
        Err(_) => error!("heartbeat cycle timed out"),
        Ok(Ok(())) => {}
    }

    loop {
        tokio::select! {
            _ = interval.tick() => {
                match tokio::time::timeout(cycle_timeout, run_cycle(&state)).await {
                    Ok(Err(e)) => error!(error = %e, "heartbeat cycle failed"),
                    Err(_) => error!("heartbeat cycle timed out"),
                    Ok(Ok(())) => {}
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
    let agents_deactivated = match deactivate_stale_agents(
        &state.dolt,
        state.config.heartbeat.stale_agent_minutes,
    )
    .await
    {
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
                publish_nats(
                    &state.nats,
                    &subj,
                    &serde_json::json!({
                        "service": SERVICE_NAME,
                        "expired_count": n,
                        "timestamp": Utc::now().to_rfc3339(),
                    }),
                )
                .await;
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
    // 5g. Knowledge graph cleanup (edge decay, stale entity pruning)
    // -----------------------------------------------------------------------
    match kg_cleanup_cycle(&state.pg, &state.config).await {
        Ok(stats) => {
            if stats.edges_decayed > 0
                || stats.edges_expired > 0
                || stats.entities_pruned > 0
                || stats.orphans_removed > 0
            {
                info!(
                    edges_decayed = stats.edges_decayed,
                    edges_expired = stats.edges_expired,
                    entities_pruned = stats.entities_pruned,
                    orphans_removed = stats.orphans_removed,
                    "kg cleanup completed"
                );
                let subj = format!("{prefix}.{env}.kg.cleanup");
                publish_nats(
                    &state.nats,
                    &subj,
                    &serde_json::json!({
                        "service": SERVICE_NAME,
                        "edges_decayed": stats.edges_decayed,
                        "edges_expired": stats.edges_expired,
                        "entities_pruned": stats.entities_pruned,
                        "orphans_removed": stats.orphans_removed,
                        "timestamp": Utc::now().to_rfc3339(),
                    }),
                )
                .await;
            }
        }
        Err(e) => {
            warn!(error = %e, "kg cleanup failed");
        }
    }

    // -----------------------------------------------------------------------
    // 5h. Daily budget replenishment (Dolt + Postgres)
    // -----------------------------------------------------------------------
    if state.config.budget.enabled {
        match replenish_budgets(&state.dolt, &state.pg, &state.config).await {
            Ok(n) => {
                if n > 0 {
                    info!(agents = n, "budget replenishment completed");
                    let subj = format!("{prefix}.{env}.budget.replenished");
                    publish_nats(
                        &state.nats,
                        &subj,
                        &serde_json::json!({
                            "service": SERVICE_NAME,
                            "agents_replenished": n,
                            "amount": state.config.budget.daily_replenishment,
                            "timestamp": Utc::now().to_rfc3339(),
                        }),
                    )
                    .await;
                }
            }
            Err(e) => {
                warn!(error = %e, "budget replenishment failed");
            }
        }
    }

    // -----------------------------------------------------------------------
    // 5i. Outbound webhook notifications
    // -----------------------------------------------------------------------
    if state.config.webhooks.enabled {
        if let Err(e) = deliver_outbound_notifications(&state.dolt, &state.pg, &state.config).await
        {
            warn!(error = %e, "outbound webhook delivery failed");
        }
    }

    // -----------------------------------------------------------------------
    // 5j. Chat session expiry + reply queue cleanup (v0.7.0)
    // -----------------------------------------------------------------------
    if state.config.chat.enabled {
        match chat_cleanup(&state.pg, &state.config).await {
            Ok((sessions_closed, replies_failed)) => {
                if sessions_closed > 0 || replies_failed > 0 {
                    info!(sessions_closed, replies_failed, "chat cleanup completed");
                }
            }
            Err(e) => {
                warn!(error = %e, "chat cleanup failed");
            }
        }
    }

    // -----------------------------------------------------------------------
    // 5k. Dashboard session cleanup (v0.7.0)
    // -----------------------------------------------------------------------
    if state.config.dashboard_auth.enabled {
        match dashboard_session_cleanup(&state.pg, &state.config).await {
            Ok(deleted) => {
                if deleted > 0 {
                    info!(
                        expired_sessions = deleted,
                        "dashboard session cleanup completed"
                    );
                }
            }
            Err(e) => {
                warn!(error = %e, "dashboard session cleanup failed");
            }
        }
    }

    // -----------------------------------------------------------------------
    // 5l. Formula registry sync (bidirectional TOML ↔ Postgres)
    // -----------------------------------------------------------------------
    match sync_formula_registry(&state.pg, &state.config).await {
        Ok(stats) => {
            if stats.system_synced > 0
                || stats.custom_backfilled > 0
                || stats.hashes_filled > 0
                || stats.disk_written > 0
            {
                info!(
                    system_synced = stats.system_synced,
                    custom_backfilled = stats.custom_backfilled,
                    hashes_filled = stats.hashes_filled,
                    disk_written = stats.disk_written,
                    "formula registry synced"
                );
            }
        }
        Err(e) => {
            warn!(error = %e, "formula registry sync failed");
        }
    }

    // -----------------------------------------------------------------------
    // 5m. Skill registry sync (config → Dolt)
    // -----------------------------------------------------------------------
    match sync_skill_registry(&state.dolt, &state.config).await {
        Ok(synced) => {
            if synced > 0 {
                info!(skills_synced = synced, "skill registry synced");
            }
        }
        Err(e) => {
            warn!(error = %e, "skill registry sync failed");
        }
    }

    // -----------------------------------------------------------------------
    // 5n. Incident detection + notification dispatch
    // -----------------------------------------------------------------------
    if state.config.notifications.enabled {
        match check_notification_rules(&state.pg, &state.nats, &state.config).await {
            Ok(fired) => {
                if fired > 0 {
                    info!(notifications_fired = fired, "notification rules evaluated");
                }
            }
            Err(e) => {
                warn!(error = %e, "notification rule check failed");
            }
        }
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
    if let Err(e) = write_audit_log(&state.pg, &trace_id, SERVICE_NAME, cycle_duration_ms).await {
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
    sqlx::query("CALL DOLT_ADD('-A')").execute(dolt).await?;

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
async fn deactivate_stale_agents(
    dolt: &MySqlPool,
    stale_minutes: u32,
) -> Result<u64, BroodlinkError> {
    let result = sqlx::query(&format!(
        "UPDATE agent_profiles
             SET active = false
             WHERE active = true
               AND last_seen IS NOT NULL
               AND last_seen < NOW() - INTERVAL {stale_minutes} MINUTE"
    ))
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
    let active_agents: Vec<(String,)> =
        sqlx::query_as("SELECT agent_id FROM agent_profiles WHERE active = true")
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
    let max_processed: Option<i64> =
        sqlx::query_as::<_, (Option<i64>,)>("SELECT MAX(memory_id) FROM kg_entity_memories")
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
        info!(
            queued = queued,
            "queued unprocessed memories for kg extraction"
        );
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Knowledge graph cleanup (v0.6.0)
// ---------------------------------------------------------------------------

struct KgCleanupStats {
    edges_decayed: u64,
    edges_expired: u64,
    entities_pruned: u64,
    orphans_removed: u64,
}

/// Replenish agent budgets. Runs every cycle but only actually tops up
/// agents whose balance is below the daily_replenishment threshold.
/// This is idempotent — running multiple times per day just sets the
/// budget to the replenishment amount if it's below that.
async fn replenish_budgets(
    dolt: &MySqlPool,
    pg: &PgPool,
    config: &Config,
) -> Result<u64, BroodlinkError> {
    let amount = config.budget.daily_replenishment;

    // Only replenish agents whose budget is below the daily amount
    let result = sqlx::query(
        "UPDATE agent_profiles SET budget_tokens = ? WHERE active = TRUE AND budget_tokens < ?",
    )
    .bind(amount)
    .bind(amount)
    .execute(dolt)
    .await?;

    let count = result.rows_affected();

    if count > 0 {
        // Record replenishment transactions for auditing
        // Fetch agents that were replenished (balance now equals amount)
        let agents: Vec<(String, i64)> = sqlx::query_as::<_, (String, i64)>(
            "SELECT agent_id, COALESCE(budget_tokens, 0) FROM agent_profiles WHERE active = TRUE AND budget_tokens = ?",
        )
        .bind(amount)
        .fetch_all(dolt)
        .await?;

        for (agent_id, balance) in &agents {
            let _ = sqlx::query(
                "INSERT INTO budget_transactions (agent_id, tool_name, cost_tokens, balance_after, trace_id)
                 VALUES ($1, 'replenishment', $2, $3, 'heartbeat')",
            )
            .bind(agent_id)
            .bind(amount)
            .bind(balance)
            .execute(pg)
            .await;
        }
    }

    Ok(count)
}

async fn kg_cleanup_cycle(pg: &PgPool, config: &Config) -> Result<KgCleanupStats, BroodlinkError> {
    let ms = &config.memory_search;
    let decay_rate = ms.kg_edge_decay_rate;
    let ttl_days = ms.kg_entity_ttl_days;
    let min_mentions = ms.kg_min_mention_count;

    // 1. Edge weight decay: reduce weights by decay_rate for edges not updated today
    let edges_decayed = if decay_rate > 0.0 {
        let factor = 1.0 - decay_rate;
        let result = sqlx::query(
            "UPDATE kg_edges SET weight = weight * $1, updated_at = NOW()
             WHERE valid_to IS NULL AND updated_at < NOW() - INTERVAL '1 day'",
        )
        .bind(factor)
        .execute(pg)
        .await?;
        result.rows_affected()
    } else {
        0
    };

    // 2. Expire edges with weight below threshold
    let result = sqlx::query(
        "UPDATE kg_edges SET valid_to = NOW(), updated_at = NOW()
         WHERE valid_to IS NULL AND weight < 0.1",
    )
    .execute(pg)
    .await?;
    let edges_expired = result.rows_affected();

    // 3. Prune stale entities (not seen within TTL, low mention count)
    let entities_pruned = if ttl_days > 0 {
        let ttl_interval = format!("{ttl_days} days");
        // Delete entity_memories links first
        sqlx::query(
            "DELETE FROM kg_entity_memories WHERE entity_id IN (
                SELECT entity_id FROM kg_entities
                WHERE last_seen < NOW() - $1::interval AND mention_count < $2
            )",
        )
        .bind(&ttl_interval)
        .bind(min_mentions as i32)
        .execute(pg)
        .await?;

        // Delete edges referencing stale entities
        sqlx::query(
            "UPDATE kg_edges SET valid_to = NOW(), updated_at = NOW()
             WHERE valid_to IS NULL AND (
                source_id IN (SELECT entity_id FROM kg_entities WHERE last_seen < NOW() - $1::interval AND mention_count < $2)
                OR target_id IN (SELECT entity_id FROM kg_entities WHERE last_seen < NOW() - $1::interval AND mention_count < $2)
             )",
        )
        .bind(&ttl_interval)
        .bind(min_mentions as i32)
        .execute(pg)
        .await?;

        let result = sqlx::query(
            "DELETE FROM kg_entities
             WHERE last_seen < NOW() - $1::interval AND mention_count < $2",
        )
        .bind(&ttl_interval)
        .bind(min_mentions as i32)
        .execute(pg)
        .await?;
        result.rows_affected()
    } else {
        0
    };

    // 4. Remove orphan entities (no active edges, no entity_memories links)
    let result = sqlx::query(
        "DELETE FROM kg_entities WHERE entity_id IN (
            SELECT e.entity_id FROM kg_entities e
            LEFT JOIN kg_edges ea ON (ea.source_id = e.entity_id OR ea.target_id = e.entity_id) AND ea.valid_to IS NULL
            LEFT JOIN kg_entity_memories em ON em.entity_id = e.entity_id
            WHERE ea.edge_id IS NULL AND em.entity_id IS NULL
        )",
    )
    .execute(pg)
    .await?;
    let orphans_removed = result.rows_affected();

    Ok(KgCleanupStats {
        edges_decayed,
        edges_expired,
        entities_pruned,
        orphans_removed,
    })
}

// ---------------------------------------------------------------------------
// Outbound webhook notifications (v0.6.0)
// ---------------------------------------------------------------------------

/// Check for notification-worthy conditions and deliver to registered webhook endpoints.
async fn deliver_outbound_notifications(
    dolt: &MySqlPool,
    pg: &PgPool,
    config: &Config,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // Fetch active webhook endpoints with their subscribed events
    let endpoints: Vec<(String, String, Option<String>, serde_json::Value)> = sqlx::query_as(
        "SELECT id, platform, webhook_url, events FROM webhook_endpoints WHERE active = true AND webhook_url IS NOT NULL",
    )
    .fetch_all(pg)
    .await?;

    if endpoints.is_empty() {
        return Ok(());
    }

    let mut notifications: Vec<(String, serde_json::Value)> = Vec::new();

    // Check: agent.offline — agents that became inactive this cycle
    let offline_agents: Vec<(String,)> = sqlx::query_as(
        "SELECT agent_id FROM agent_profiles
         WHERE active = false
           AND last_seen IS NOT NULL
           AND last_seen > NOW() - INTERVAL 10 MINUTE",
    )
    .fetch_all(dolt)
    .await
    .unwrap_or_default();

    for (agent_id,) in &offline_agents {
        notifications.push((
            "agent.offline".to_string(),
            serde_json::json!({
                "event": "agent.offline",
                "agent_id": agent_id,
                "message": format!("Agent {agent_id} went offline"),
            }),
        ));
    }

    // Check: budget.low — agents with budget below threshold
    if config.budget.enabled {
        let threshold = config.budget.low_budget_threshold;
        let low_budget: Vec<(String, i64)> = sqlx::query_as(
            "SELECT agent_id, COALESCE(budget_tokens, 0) FROM agent_profiles
             WHERE active = true AND budget_tokens < ? AND budget_tokens >= 0",
        )
        .bind(threshold)
        .fetch_all(dolt)
        .await
        .unwrap_or_default();

        for (agent_id, balance) in &low_budget {
            notifications.push((
                "budget.low".to_string(),
                serde_json::json!({
                    "event": "budget.low",
                    "agent_id": agent_id,
                    "balance": balance,
                    "threshold": threshold,
                    "message": format!("Agent {agent_id} budget low: {balance} tokens"),
                }),
            ));
        }
    }

    // Check: task.failed — recent DLQ entries (last 5 minutes)
    let dlq_entries: Vec<(i64, String, String)> = sqlx::query_as(
        "SELECT id, task_id, reason FROM dead_letter_queue
         WHERE resolved = false AND created_at > NOW() - INTERVAL '5 minutes'",
    )
    .fetch_all(pg)
    .await
    .unwrap_or_default();

    for (id, task_id, reason) in &dlq_entries {
        notifications.push((
            "task.failed".to_string(),
            serde_json::json!({
                "event": "task.failed",
                "dlq_id": id,
                "task_id": task_id,
                "reason": reason,
                "message": format!("Task {task_id} failed: {reason}"),
            }),
        ));
    }

    // Check: workflow.completed / workflow.failed — recent workflow state changes
    let wf_events: Vec<(String, String, String)> = sqlx::query_as(
        "SELECT id, formula_name, status FROM workflow_runs
         WHERE status IN ('completed', 'failed')
           AND updated_at > NOW() - INTERVAL '5 minutes'",
    )
    .fetch_all(pg)
    .await
    .unwrap_or_default();

    for (id, formula, status) in &wf_events {
        let event_type = format!("workflow.{status}");
        notifications.push((
            event_type.clone(),
            serde_json::json!({
                "event": event_type,
                "workflow_id": id,
                "formula": formula,
                "status": status,
                "message": format!("Workflow '{formula}' {status}"),
            }),
        ));
    }

    // Check: approval.pending — approval gates waiting for review
    let pending_approvals: Vec<(String, String)> = sqlx::query_as(
        "SELECT id, task_id FROM approval_gates
         WHERE status = 'pending'
           AND created_at > NOW() - INTERVAL '5 minutes'",
    )
    .fetch_all(pg)
    .await
    .unwrap_or_default();

    for (gate_id, task_id) in &pending_approvals {
        notifications.push((
            "approval.pending".to_string(),
            serde_json::json!({
                "event": "approval.pending",
                "gate_id": gate_id,
                "task_id": task_id,
                "message": format!("Approval needed for task {task_id}"),
            }),
        ));
    }

    if notifications.is_empty() {
        return Ok(());
    }

    // Deliver each notification to matching endpoints
    let http_client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(
            config.webhooks.delivery_timeout_secs,
        ))
        .build()
        .unwrap_or_else(|e| {
            error!(error = %e, "failed to build webhook HTTP client");
            std::process::exit(1);
        });

    let mut delivered = 0u64;
    for (event_type, payload) in &notifications {
        for (eid, platform, webhook_url, events) in &endpoints {
            // Check if endpoint subscribes to this event type
            let subscribed = events
                .as_array()
                .map(|arr| {
                    arr.is_empty()
                        || arr.iter().any(|e| {
                            e.as_str()
                                .map(|s| s == event_type || event_type.starts_with(s))
                                .unwrap_or(false)
                        })
                })
                .unwrap_or(true); // empty = subscribe to all

            if !subscribed {
                continue;
            }

            let url = match webhook_url {
                Some(u) => u,
                None => continue,
            };

            let result = http_client
                .post(url)
                .header("Content-Type", "application/json")
                .json(payload)
                .send()
                .await;

            let (status, error_msg) = match result {
                Ok(resp) if resp.status().is_success() => ("delivered", None),
                Ok(resp) => ("failed", Some(format!("HTTP {}", resp.status()))),
                Err(e) => ("failed", Some(e.to_string())),
            };

            // Log delivery attempt
            let _ = sqlx::query(
                "INSERT INTO webhook_log (endpoint_id, direction, event_type, payload, status, error_msg)
                 VALUES ($1, 'outbound', $2, $3, $4, $5)",
            )
            .bind(eid)
            .bind(event_type)
            .bind(payload)
            .bind(status)
            .bind(&error_msg)
            .execute(pg)
            .await;

            if status == "delivered" {
                delivered += 1;
            } else if let Some(msg) = &error_msg {
                warn!(endpoint = %eid, platform = %platform, event = %event_type, error = %msg, "webhook delivery failed");
            }
        }
    }

    if delivered > 0 {
        info!(
            delivered = delivered,
            total_events = notifications.len(),
            "outbound webhooks delivered"
        );
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// v0.7.0: Chat session cleanup
// ---------------------------------------------------------------------------

/// Close expired sessions and fail timed-out replies.
async fn chat_cleanup(pg: &PgPool, config: &Config) -> Result<(u64, u64), BroodlinkError> {
    let timeout_hours = i64::from(config.chat.session_timeout_hours);
    let reply_timeout_secs = config.chat.reply_timeout_seconds as i64;

    // Close sessions where last_message_at is older than session_timeout_hours
    let sessions_result = sqlx::query(
        "UPDATE chat_sessions
         SET status = 'closed', updated_at = NOW()
         WHERE status = 'active'
           AND last_message_at < NOW() - ($1 || ' hours')::interval",
    )
    .bind(timeout_hours.to_string())
    .execute(pg)
    .await?;
    let sessions_closed = sessions_result.rows_affected();

    // Mark pending replies older than reply_timeout_seconds as 'failed'
    let replies_result = sqlx::query(
        "UPDATE chat_reply_queue
         SET status = 'failed', error_msg = 'reply timeout'
         WHERE status = 'pending'
           AND created_at < NOW() - ($1 || ' seconds')::interval",
    )
    .bind(reply_timeout_secs.to_string())
    .execute(pg)
    .await?;
    let replies_failed = replies_result.rows_affected();

    Ok((sessions_closed, replies_failed))
}

// ---------------------------------------------------------------------------
// v0.7.0: Dashboard session cleanup
// ---------------------------------------------------------------------------

/// Delete expired dashboard sessions and enforce max sessions per user.
async fn dashboard_session_cleanup(pg: &PgPool, config: &Config) -> Result<u64, BroodlinkError> {
    // Delete all expired sessions
    let expired = sqlx::query("DELETE FROM dashboard_sessions WHERE expires_at < NOW()")
        .execute(pg)
        .await?;
    let mut total_deleted = expired.rows_affected();

    // Enforce max sessions per user
    let max = i64::from(config.dashboard_auth.max_sessions_per_user);
    let over_limit: Vec<(String, i64)> = sqlx::query_as(
        "SELECT user_id, COUNT(*) as cnt
         FROM dashboard_sessions
         GROUP BY user_id
         HAVING COUNT(*) > $1",
    )
    .bind(max)
    .fetch_all(pg)
    .await?;

    for (user_id, count) in &over_limit {
        let excess = count - max;
        if excess > 0 {
            let result = sqlx::query(
                "DELETE FROM dashboard_sessions
                 WHERE id IN (
                     SELECT id FROM dashboard_sessions
                     WHERE user_id = $1
                     ORDER BY created_at ASC
                     LIMIT $2
                 )",
            )
            .bind(user_id)
            .bind(excess)
            .execute(pg)
            .await?;
            total_deleted += result.rows_affected();
        }
    }

    Ok(total_deleted)
}

// ---------------------------------------------------------------------------
// Formula registry sync (bidirectional TOML ↔ Postgres)
// ---------------------------------------------------------------------------

struct FormulaSyncStats {
    system_synced: u32,
    custom_backfilled: u32,
    hashes_filled: u32,
    disk_written: u32,
}

async fn sync_formula_registry(
    pg: &PgPool,
    config: &Arc<Config>,
) -> Result<FormulaSyncStats, BroodlinkError> {
    let mut stats = FormulaSyncStats {
        system_synced: 0,
        custom_backfilled: 0,
        hashes_filled: 0,
        disk_written: 0,
    };

    // A. System sync: TOML → Postgres (system formulas only)
    let system_formulas = broodlink_formulas::load_toml_formulas(&config.beads.formulas_dir);
    for lf in &system_formulas {
        let row: Option<(String, Option<String>)> = sqlx::query_as(
            "SELECT id, definition_hash FROM formula_registry WHERE name = $1 AND is_system = true",
        )
        .bind(&lf.name)
        .fetch_optional(pg)
        .await?;

        match row {
            Some((id, existing_hash)) => {
                if existing_hash.as_deref() == Some(&lf.hash) {
                    continue; // no change
                }
                let display_name = broodlink_formulas::title_case(&lf.name);
                let description = lf.formula.formula.description.as_deref().unwrap_or("");
                sqlx::query(
                    "UPDATE formula_registry
                     SET definition = $1, definition_hash = $2,
                         display_name = $3, description = $4,
                         version = version + 1, updated_at = NOW()
                     WHERE id = $5 AND is_system = true",
                )
                .bind(&lf.definition)
                .bind(&lf.hash)
                .bind(&display_name)
                .bind(description)
                .bind(&id)
                .execute(pg)
                .await?;
                info!(formula = %lf.name, "system formula updated from TOML");
                stats.system_synced += 1;
            }
            None => {
                // Insert only if no row with this name exists (don't overwrite user formulas)
                let id = Uuid::new_v4().to_string();
                let display_name = broodlink_formulas::title_case(&lf.name);
                let description = lf.formula.formula.description.as_deref().unwrap_or("");
                let result = sqlx::query(
                    "INSERT INTO formula_registry (id, name, display_name, description, definition, definition_hash, author, is_system, enabled)
                     VALUES ($1, $2, $3, $4, $5, $6, 'system', true, true)
                     ON CONFLICT (name) DO UPDATE SET
                         definition = EXCLUDED.definition,
                         definition_hash = EXCLUDED.definition_hash,
                         display_name = EXCLUDED.display_name,
                         description = EXCLUDED.description,
                         version = formula_registry.version + 1,
                         updated_at = NOW()
                     WHERE formula_registry.is_system = true",
                )
                .bind(&id)
                .bind(&lf.name)
                .bind(&display_name)
                .bind(description)
                .bind(&lf.definition)
                .bind(&lf.hash)
                .execute(pg)
                .await?;
                if result.rows_affected() > 0 {
                    info!(formula = %lf.name, "system formula synced to registry");
                    stats.system_synced += 1;
                }
            }
        }
    }

    // B. Custom backfill: custom TOML → Postgres (insert only, never overwrite)
    let custom_formulas =
        broodlink_formulas::load_custom_formulas(&config.beads.formulas_custom_dir);
    for lf in &custom_formulas {
        let exists: Option<(String,)> =
            sqlx::query_as("SELECT id FROM formula_registry WHERE name = $1")
                .bind(&lf.name)
                .fetch_optional(pg)
                .await?;
        if exists.is_some() {
            continue; // never overwrite existing
        }
        let id = Uuid::new_v4().to_string();
        let display_name = broodlink_formulas::title_case(&lf.name);
        let description = lf.formula.formula.description.as_deref().unwrap_or("");
        sqlx::query(
            "INSERT INTO formula_registry (id, name, display_name, description, definition, definition_hash, author, is_system, enabled)
             VALUES ($1, $2, $3, $4, $5, $6, 'disk-import', false, true)
             ON CONFLICT (name) DO NOTHING",
        )
        .bind(&id)
        .bind(&lf.name)
        .bind(&display_name)
        .bind(description)
        .bind(&lf.definition)
        .bind(&lf.hash)
        .execute(pg)
        .await?;
        info!(formula = %lf.name, "custom formula backfilled from disk");
        stats.custom_backfilled += 1;
    }

    // C. Hash backfill: compute hashes for rows missing definition_hash
    let rows: Vec<(String, serde_json::Value)> =
        sqlx::query_as("SELECT id, definition FROM formula_registry WHERE definition_hash IS NULL")
            .fetch_all(pg)
            .await?;
    for (id, def) in &rows {
        let hash = broodlink_formulas::definition_hash(def);
        sqlx::query("UPDATE formula_registry SET definition_hash = $1 WHERE id = $2")
            .bind(&hash)
            .bind(id)
            .execute(pg)
            .await?;
        stats.hashes_filled += 1;
    }

    // D. Disk safety net: write user formulas to disk that are missing on disk
    let custom_dir = &config.beads.formulas_custom_dir;
    let user_rows: Vec<(String, String, serde_json::Value, Option<String>)> = sqlx::query_as(
        "SELECT name, display_name, definition, description
         FROM formula_registry WHERE is_system = false",
    )
    .fetch_all(pg)
    .await?;

    for (name, display_name, definition, description) in &user_rows {
        let toml_path = std::path::Path::new(custom_dir).join(format!("{name}.formula.toml"));
        if toml_path.exists() {
            continue; // already on disk
        }
        let meta = broodlink_formulas::FormulaTomlMeta {
            display_name,
            description: description.as_deref().unwrap_or(""),
        };
        if let Err(e) =
            broodlink_formulas::persist_formula_toml(custom_dir, name, definition, &meta)
        {
            warn!(formula = %name, error = %e, "failed to write user formula to disk (safety net)");
        } else {
            info!(formula = %name, "user formula written to disk (safety net)");
            stats.disk_written += 1;
        }
    }

    Ok(stats)
}

// ---------------------------------------------------------------------------
// Skill registry sync: config.toml → Dolt skills table
// ---------------------------------------------------------------------------
async fn sync_skill_registry(
    dolt: &MySqlPool,
    config: &Arc<Config>,
) -> Result<u32, BroodlinkError> {
    let mut synced: u32 = 0;

    for agent_cfg in config.agents.values() {
        for skill_name in &agent_cfg.skills {
            let description = match config.skill_definitions.get(skill_name) {
                Some(d) => d.as_str(),
                None => {
                    warn!(
                        skill = %skill_name,
                        agent = %agent_cfg.agent_id,
                        "skill listed in agent config but missing from [skill_definitions], skipping"
                    );
                    continue;
                }
            };

            sqlx::query(
                "INSERT INTO skills (name, description, agent_name, created_at, updated_at)
                 VALUES (?, ?, ?, NOW(), NOW())
                 ON DUPLICATE KEY UPDATE description = VALUES(description), updated_at = NOW()",
            )
            .bind(skill_name)
            .bind(description)
            .bind(&agent_cfg.agent_id)
            .execute(dolt)
            .await?;

            synced += 1;
        }
    }

    Ok(synced)
}

/// Publish a JSON payload to NATS (best-effort, logs warning on failure).
async fn publish_nats<T: Serialize>(nats: &async_nats::Client, subject: &str, payload: &T) {
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

// ---------------------------------------------------------------------------
// Notification rule evaluation (incident detection + alerting)
// ---------------------------------------------------------------------------

async fn check_notification_rules(
    pg: &PgPool,
    nats: &async_nats::Client,
    config: &Config,
) -> Result<u64, BroodlinkError> {
    let rules: Vec<(i64, String, String, serde_json::Value, String, String, Option<String>, i32, Option<String>)> =
        sqlx::query_as(
            "SELECT id, name, condition_type, condition_config, channel, target, template,
                    cooldown_minutes, last_triggered_at::text
             FROM notification_rules
             WHERE enabled = TRUE",
        )
        .fetch_all(pg)
        .await?;

    if rules.is_empty() {
        return Ok(0);
    }

    let mut fired = 0u64;
    let prefix = &config.nats.subject_prefix;
    let env = &config.broodlink.env;

    for (rule_id, name, condition_type, condition_config, channel, target, template, cooldown_minutes, last_triggered) in &rules {
        // Check cooldown
        if let Some(last) = last_triggered {
            if let Ok(ts) = chrono::NaiveDateTime::parse_from_str(last, "%Y-%m-%d %H:%M:%S%.f%z")
                .or_else(|_| chrono::NaiveDateTime::parse_from_str(last, "%Y-%m-%d %H:%M:%S%.f"))
            {
                let elapsed = (Utc::now().naive_utc() - ts).num_minutes();
                if elapsed < *cooldown_minutes as i64 {
                    continue;
                }
            }
        }

        let window_minutes = condition_config
            .get("window_minutes")
            .and_then(|v| v.as_i64())
            .unwrap_or(15);
        let threshold = condition_config
            .get("threshold")
            .and_then(|v| v.as_i64())
            .unwrap_or(1);

        let (should_fire, message_vars) = match condition_type.as_str() {
            "service_event_error" => {
                let count: (i64,) = sqlx::query_as(
                    "SELECT COUNT(*) FROM service_events
                     WHERE severity = 'error'
                       AND created_at > NOW() - make_interval(mins => $1::double precision)",
                )
                .bind(window_minutes as f64)
                .fetch_one(pg)
                .await
                .unwrap_or((0,));

                (count.0 >= threshold, serde_json::json!({"count": count.0, "type": "service_event_error"}))
            }
            "dlq_spike" => {
                let count: (i64,) = sqlx::query_as(
                    "SELECT COUNT(*) FROM dead_letter_queue
                     WHERE resolved = FALSE
                       AND created_at > NOW() - make_interval(mins => $1::double precision)",
                )
                .bind(window_minutes as f64)
                .fetch_one(pg)
                .await
                .unwrap_or((0,));

                (count.0 >= threshold, serde_json::json!({"count": count.0, "type": "dlq_spike"}))
            }
            "budget_low" => {
                let budget_threshold = condition_config
                    .get("budget_threshold")
                    .and_then(|v| v.as_i64())
                    .unwrap_or(config.budget.low_budget_threshold);

                let agents: Vec<(String, i64)> = sqlx::query_as(
                    "SELECT agent_id, budget_tokens FROM agent_profiles
                     WHERE active = true AND budget_tokens < $1",
                )
                .bind(budget_threshold)
                .fetch_all(pg)
                .await
                .unwrap_or_default();

                let agent_names: Vec<&str> = agents.iter().map(|(a, _)| a.as_str()).collect();
                (!agents.is_empty(), serde_json::json!({"agents": agent_names, "count": agents.len(), "type": "budget_low"}))
            }
            _ => {
                warn!(condition_type = %condition_type, rule = %name, "unknown notification condition type");
                continue;
            }
        };

        if !should_fire {
            continue;
        }

        // Render message from template or generate default
        let message = if let Some(tmpl) = template {
            let mut msg = tmpl.clone();
            if let Some(obj) = message_vars.as_object() {
                for (key, val) in obj {
                    let replacement = match val {
                        serde_json::Value::String(s) => s.clone(),
                        other => other.to_string(),
                    };
                    msg = msg.replace(&format!("{{{{{key}}}}}"), &replacement);
                }
            }
            msg
        } else {
            format!(
                "[Broodlink Alert] Rule '{}' triggered: {}",
                name,
                message_vars
            )
        };

        // Insert notification_log entry
        let log_id: (i64,) = sqlx::query_as(
            "INSERT INTO notification_log (rule_id, rule_name, channel, target, message, status, created_at)
             VALUES ($1, $2, $3, $4, $5, 'pending', NOW())
             RETURNING id",
        )
        .bind(rule_id)
        .bind(name)
        .bind(channel)
        .bind(target)
        .bind(&message)
        .fetch_one(pg)
        .await?;

        // Publish NATS event for a2a-gateway to deliver
        let notify_subject = format!("{prefix}.{env}.notification.send");
        let notify_payload = serde_json::json!({
            "log_id": log_id.0,
            "rule_id": rule_id,
            "rule_name": name,
            "channel": channel,
            "target": target,
            "message": message,
        });
        if let Ok(bytes) = serde_json::to_vec(&notify_payload) {
            if let Err(e) = nats.publish(notify_subject, bytes.into()).await {
                warn!(error = %e, rule = %name, "failed to publish notification event");
            }
        }

        // Update last_triggered_at
        let _ = sqlx::query(
            "UPDATE notification_rules SET last_triggered_at = NOW() WHERE id = $1",
        )
        .bind(rule_id)
        .execute(pg)
        .await;

        // Auto-postmortem for service_event_error
        if condition_type == "service_event_error"
            && condition_config
                .get("auto_postmortem")
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
        {
            let workflow_id = Uuid::new_v4().to_string();
            let convoy_id = Uuid::new_v4().to_string();
            let wf_subject = format!("{prefix}.{env}.coordinator.workflow_start");
            let wf_payload = serde_json::json!({
                "workflow_id": workflow_id,
                "convoy_id": convoy_id,
                "formula_name": "incident-postmortem",
                "params": {
                    "incident_title": format!("Auto-detected: {} error events in {}min", message_vars["count"], window_minutes),
                    "severity": "high",
                    "affected_services": "auto-detected",
                },
                "started_by": "heartbeat",
            });

            // Create workflow_runs entry first
            let _ = sqlx::query(
                "INSERT INTO workflow_runs (id, convoy_id, formula_name, status, started_by, created_at, updated_at)
                 VALUES ($1, $2, 'incident-postmortem', 'pending', 'heartbeat', NOW(), NOW())",
            )
            .bind(&workflow_id)
            .bind(&convoy_id)
            .execute(pg)
            .await;

            if let Ok(bytes) = serde_json::to_vec(&wf_payload) {
                if let Err(e) = nats.publish(wf_subject, bytes.into()).await {
                    warn!(error = %e, "failed to publish auto-postmortem workflow");
                } else {
                    info!(workflow_id = %workflow_id, "auto-postmortem triggered by incident detection");
                }
            }
        }

        info!(rule = %name, channel = %channel, "notification rule fired");
        fired += 1;
    }

    Ok(fired)
}
