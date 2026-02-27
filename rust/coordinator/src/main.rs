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
use std::process;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use broodlink_config::Config;
use broodlink_formulas::{FormulaFile, FormulaInput};
use broodlink_secrets::SecretsProvider;
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use sqlx::mysql::MySqlPoolOptions;
use sqlx::postgres::PgPoolOptions;
use sqlx::{MySqlPool, PgPool, Row};
use tokio::sync::watch;
use tracing::{error, info, warn};
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

const SERVICE_NAME: &str = "coordinator";
const SERVICE_VERSION: &str = env!("CARGO_PKG_VERSION");
const MAX_CLAIM_RETRIES: u32 = 5;
const BACKOFF_BASE_MS: u64 = 1000;

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
    #[error("no eligible agents for task {task_id}: role={role}")]
    NoEligibleAgent { task_id: String, role: String },
    #[error("claim contention exhausted for task {0} after {1} retries")]
    ClaimExhausted(String, u32),
    #[error("deserialization error: {0}")]
    Deserialize(String),
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

impl From<async_nats::SubscribeError> for BroodlinkError {
    fn from(e: async_nats::SubscribeError) -> Self {
        Self::Nats(e.to_string())
    }
}

// ---------------------------------------------------------------------------
// NATS message payloads
// ---------------------------------------------------------------------------

/// Payload received on the `task_available` subject.
#[derive(Deserialize, Debug)]
struct TaskAvailablePayload {
    task_id: String,
    #[serde(default)]
    task_type: Option<String>,
    #[serde(default)]
    role: Option<String>,
    #[serde(default)]
    cost_tier: Option<String>,
}

/// Payload dispatched to an agent on the `agent.<id>.task` subject.
#[derive(Serialize)]
struct TaskDispatchPayload {
    task_id: String,
    title: String,
    description: Option<String>,
    priority: i32,
    formula_name: Option<String>,
    convoy_id: Option<String>,
    assigned_by: String,
    // v0.9.0: Model domain hint for smart routing
    #[serde(skip_serializing_if = "Option::is_none")]
    model_hint: Option<String>,
}

/// Payload received on the `workflow_start` subject.
#[derive(Deserialize, Debug)]
struct WorkflowStartPayload {
    workflow_id: String,
    convoy_id: String,
    formula_name: String,
    #[serde(default)]
    params: serde_json::Value,
    #[allow(dead_code)]
    started_by: String,
}

/// Payload received on the `task_completed` subject.
#[derive(Deserialize, Debug)]
struct TaskCompletedPayload {
    task_id: String,
    #[allow(dead_code)]
    agent_id: String,
    #[serde(default)]
    result_data: Option<serde_json::Value>,
}

/// Payload received on the `task_failed` subject.
#[derive(Deserialize, Debug)]
struct TaskFailedPayload {
    task_id: String,
    #[allow(dead_code)]
    agent_id: String,
    #[serde(default)]
    error: Option<String>,
}

/// Payload received on the `task_declined` subject.
#[derive(Deserialize, Debug)]
struct TaskDeclinedPayload {
    task_id: String,
    agent_id: String,
    #[serde(default)]
    reason: Option<String>,
    #[serde(default)]
    suggested_agent: Option<String>,
}

/// Payload received on the `task_context_request` subject.
#[derive(Deserialize, Debug)]
struct TaskContextRequestPayload {
    task_id: String,
    agent_id: String,
    questions: Vec<String>,
}

/// Metrics payload published to `broodlink.metrics.coordinator`.
#[derive(Serialize)]
struct MetricsPayload {
    service: String,
    tasks_claimed: u64,
    tasks_dead_lettered: u64,
    claim_retries_total: u64,
    uptime_seconds: u64,
}

// ---------------------------------------------------------------------------
// Application state
// ---------------------------------------------------------------------------

struct AppState {
    dolt: MySqlPool,
    pg: PgPool,
    nats: async_nats::Client,
    config: Arc<Config>,
    start_time: std::time::Instant,
    // Atomic counters for metrics
    tasks_claimed: AtomicU64,
    tasks_dead_lettered: AtomicU64,
    claim_retries_total: AtomicU64,
}

// ---------------------------------------------------------------------------
// Agent selection from Dolt agent_profiles
// ---------------------------------------------------------------------------

/// An eligible agent row from agent_profiles (Dolt).
#[derive(Debug, Clone)]
struct EligibleAgent {
    agent_id: String,
    cost_tier: String,
    capabilities: Option<serde_json::Value>,
    max_concurrent: i32,
    last_seen: Option<String>,
}

/// Per-agent metrics from the Postgres `agent_metrics` table.
#[derive(Debug, Default)]
struct AgentMetrics {
    success_rate: f64,
    current_load: i32,
}

/// Scored agent candidate ready for selection.
#[derive(Debug)]
struct ScoredAgent {
    agent: EligibleAgent,
    score: f64,
    breakdown: ScoreBreakdown,
}

/// Breakdown of how the score was computed.
#[derive(Debug, Clone, Serialize)]
struct ScoreBreakdown {
    capability: f64,
    success_rate: f64,
    availability: f64,
    cost: f64,
    recency: f64,
    domain: f64,
}

/// Payload published when a routing decision is made.
#[derive(Serialize)]
struct RoutingDecisionPayload {
    task_id: String,
    agent_id: String,
    score: f64,
    breakdown: ScoreBreakdown,
    candidates_evaluated: usize,
    attempt: u32,
}

/// Query Dolt `agent_profiles` for active agents matching the requested role.
async fn find_eligible_agents(
    dolt: &MySqlPool,
    role: &str,
    cost_tier: Option<&str>,
) -> Result<Vec<EligibleAgent>, BroodlinkError> {
    let rows = if let Some(tier) = cost_tier {
        sqlx::query(
            "SELECT agent_id, cost_tier, capabilities, max_concurrent,
                    CAST(last_seen AS CHAR) AS last_seen_str
             FROM agent_profiles
             WHERE role = ? AND cost_tier = ? AND active = true",
        )
        .bind(role)
        .bind(tier)
        .fetch_all(dolt)
        .await?
    } else {
        sqlx::query(
            "SELECT agent_id, cost_tier, capabilities, max_concurrent,
                    CAST(last_seen AS CHAR) AS last_seen_str
             FROM agent_profiles
             WHERE role = ? AND active = true",
        )
        .bind(role)
        .fetch_all(dolt)
        .await?
    };

    let agents = rows
        .iter()
        .map(|row| {
            let agent_id: String = row.get("agent_id");
            let cost_tier: String = row.get("cost_tier");
            let capabilities: Option<serde_json::Value> = row.get("capabilities");
            let max_concurrent: i32 = row.get("max_concurrent");
            let last_seen: Option<String> = row.get("last_seen_str");
            EligibleAgent {
                agent_id,
                cost_tier,
                capabilities,
                max_concurrent,
                last_seen,
            }
        })
        .collect();

    Ok(agents)
}

/// Fetch metrics for a batch of agent IDs from Postgres `agent_metrics`.
async fn fetch_agent_metrics(
    pg: &PgPool,
    agent_ids: &[String],
) -> Result<HashMap<String, AgentMetrics>, BroodlinkError> {
    if agent_ids.is_empty() {
        return Ok(HashMap::new());
    }

    // Build a parameterized query for the list of agent_ids
    let placeholders: Vec<String> = (1..=agent_ids.len()).map(|i| format!("${i}")).collect();
    let query_str = format!(
        "SELECT agent_id, success_rate, current_load
         FROM agent_metrics WHERE agent_id IN ({})",
        placeholders.join(", ")
    );

    let mut query = sqlx::query(&query_str);
    for id in agent_ids {
        query = query.bind(id);
    }

    let rows = query.fetch_all(pg).await?;

    let mut metrics = HashMap::new();
    for row in &rows {
        let agent_id: String = row.get("agent_id");
        let success_rate: f32 = row.get("success_rate");
        let current_load: i32 = row.get("current_load");
        metrics.insert(
            agent_id,
            AgentMetrics {
                success_rate: f64::from(success_rate),
                current_load,
            },
        );
    }

    Ok(metrics)
}

/// Map cost_tier string to efficiency score: low=1.0, medium=0.6, high=0.3.
fn cost_tier_score(tier: &str) -> f64 {
    match tier {
        "low" => 1.0,
        "medium" => 0.6,
        "high" => 0.3,
        _ => 0.3,
    }
}

/// Compute the recency bonus based on last_seen timestamp.
/// 1.0 if < 60s ago, 0.0 if > 30min, linear interpolation between.
fn recency_score(last_seen: Option<&str>) -> f64 {
    let Some(ts_str) = last_seen else {
        return 0.0;
    };

    let Ok(last) = chrono::NaiveDateTime::parse_from_str(ts_str, "%Y-%m-%d %H:%M:%S") else {
        return 0.0;
    };

    let now = chrono::Utc::now().naive_utc();
    let elapsed_secs = (now - last).num_seconds().max(0) as f64;

    if elapsed_secs < 60.0 {
        1.0
    } else if elapsed_secs > 1800.0 {
        0.0
    } else {
        // Linear decay from 1.0 at 60s to 0.0 at 1800s
        1.0 - (elapsed_secs - 60.0) / (1800.0 - 60.0)
    }
}

/// Compute agent score using the weighted multi-factor algorithm.
fn compute_score(
    agent: &EligibleAgent,
    metrics: Option<&AgentMetrics>,
    formula_name: Option<&str>,
    weights: &broodlink_config::RoutingWeights,
    new_agent_bonus: f64,
    task_domain: Option<&str>,
    agent_domains: &[String],
) -> (f64, ScoreBreakdown) {
    // Capability match: when a formula is specified, agents declaring that
    // capability in their JSON get 1.0; otherwise 0.5. When no formula is
    // specified we give 1.0 (role match is all that matters).
    let capability = match formula_name {
        Some(formula) => match &agent.capabilities {
            Some(caps) if caps.get(formula).is_some() => 1.0,
            _ => 0.5,
        },
        None => 1.0,
    };

    // Success rate from metrics, default to new_agent_bonus if no metrics
    let success_rate = metrics.map_or(new_agent_bonus, |m| m.success_rate);

    // Availability: 1.0 - (current_load / max_concurrent), clamped to [0.0, 1.0]
    let availability = if let Some(m) = metrics {
        let max = if agent.max_concurrent > 0 {
            agent.max_concurrent
        } else {
            5
        };
        (1.0 - f64::from(m.current_load) / f64::from(max)).clamp(0.0, 1.0)
    } else {
        1.0 // No metrics = assume available
    };

    let cost = cost_tier_score(&agent.cost_tier);
    let recency = recency_score(agent.last_seen.as_deref());

    // v0.9.0: Domain match score
    let domain = match task_domain {
        Some(td) => {
            if agent_domains.is_empty() {
                if td == "general" {
                    1.0
                } else {
                    0.2
                }
            } else if agent_domains.iter().any(|d| d == td) {
                1.0
            } else {
                0.2
            }
        }
        None => 1.0,
    };

    let score = capability * weights.capability
        + success_rate * weights.success_rate
        + availability * weights.availability
        + cost * weights.cost
        + recency * weights.recency
        + domain * weights.domain;

    let breakdown = ScoreBreakdown {
        capability,
        success_rate,
        availability,
        cost,
        recency,
        domain,
    };

    (score, breakdown)
}

/// Score and sort agents by the multi-factor routing algorithm.
fn rank_agents(
    agents: Vec<EligibleAgent>,
    metrics: &HashMap<String, AgentMetrics>,
    formula_name: Option<&str>,
    weights: &broodlink_config::RoutingWeights,
    new_agent_bonus: f64,
    task_domain: Option<&str>,
    agent_domain_map: &HashMap<String, Vec<String>>,
) -> Vec<ScoredAgent> {
    let mut scored: Vec<ScoredAgent> = agents
        .into_iter()
        .map(|agent| {
            let m = metrics.get(&agent.agent_id);
            let domains = agent_domain_map
                .get(&agent.agent_id)
                .map(|v| v.as_slice())
                .unwrap_or(&[]);
            let (score, breakdown) = compute_score(
                &agent,
                m,
                formula_name,
                weights,
                new_agent_bonus,
                task_domain,
                domains,
            );
            ScoredAgent {
                agent,
                score,
                breakdown,
            }
        })
        .collect();

    // Sort descending by score
    scored.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    scored
}

// ---------------------------------------------------------------------------
// v0.9.0: Model domain classification
// ---------------------------------------------------------------------------

/// Determine the model domain hint for a task dispatch.
/// Checks formula name first, then falls back to keyword classification.
fn determine_model_hint(
    formula_name: Option<&str>,
    description: Option<&str>,
    config: &Config,
) -> Option<String> {
    // 1. Check formula metadata model_domain (from TOML file)
    if let Some(name) = formula_name {
        let path = format!("{}/{}.formula.toml", config.beads.formulas_dir, name);
        if let Ok(contents) = std::fs::read_to_string(&path) {
            if let Ok(ff) = toml::from_str::<FormulaFile>(&contents) {
                if let Some(domain) = &ff.formula.model_domain {
                    return Some(domain.clone());
                }
            }
        }
    }

    // 2. If code model isn't configured, no hint needed
    if config.chat.chat_code_model.is_empty() {
        return None;
    }

    // 3. Fall back to keyword classification of description
    if let Some(desc) = description {
        if classify_task_domain(desc) == "code" {
            return Some("code".to_string());
        }
    }

    None
}

/// Lightweight task description classifier (mirrors a2a-gateway's classify_model_domain).
fn classify_task_domain(text: &str) -> &'static str {
    let lower = text.to_lowercase();

    if lower.contains("```") || lower.contains("~~~") {
        return "code";
    }

    const CODE_SIGNALS: &[&str] = &[
        ".rs",
        ".py",
        ".ts",
        ".js",
        ".go",
        ".java",
        "implement",
        "refactor",
        "debug",
        "compile",
        "unit test",
        "integration test",
        "code review",
        "function",
        "struct ",
        "class ",
        "migration",
        "api endpoint",
        "pull request",
        "codebase",
    ];
    let hits = CODE_SIGNALS.iter().filter(|s| lower.contains(*s)).count();
    if hits >= 2 {
        return "code";
    }

    "general"
}

// ---------------------------------------------------------------------------
// Task row from Postgres
// ---------------------------------------------------------------------------

struct TaskRow {
    id: String,
    title: String,
    description: Option<String>,
    priority: i32,
    formula_name: Option<String>,
    convoy_id: Option<String>,
    declined_agents: Vec<String>,
}

async fn fetch_task(pg: &PgPool, task_id: &str) -> Result<Option<TaskRow>, BroodlinkError> {
    let row = sqlx::query(
        "SELECT id, title, description, priority, formula_name, convoy_id, declined_agents
         FROM task_queue WHERE id = $1 AND status = 'pending'",
    )
    .bind(task_id)
    .fetch_optional(pg)
    .await?;

    Ok(row.map(|r| {
        let priority: Option<i32> = r.get("priority");
        let declined_json: Option<serde_json::Value> = r.get("declined_agents");
        let declined_agents: Vec<String> = declined_json
            .and_then(|v| serde_json::from_value(v).ok())
            .unwrap_or_default();
        TaskRow {
            id: r.get("id"),
            title: r.get("title"),
            description: r.get("description"),
            priority: priority.unwrap_or(0),
            formula_name: r.get("formula_name"),
            convoy_id: r.get("convoy_id"),
            declined_agents,
        }
    }))
}

// ---------------------------------------------------------------------------
// Atomic claim with exponential backoff
// ---------------------------------------------------------------------------

/// Attempt to atomically claim a task in Postgres.
/// Returns `true` if the claim succeeded (rows_affected == 1).
async fn try_claim_task(
    pg: &PgPool,
    task_id: &str,
    agent_id: &str,
) -> Result<bool, BroodlinkError> {
    let result = sqlx::query(
        "UPDATE task_queue
         SET status = 'claimed', assigned_agent = $1, claimed_at = NOW(), updated_at = NOW()
         WHERE id = $2 AND status = 'pending'",
    )
    .bind(agent_id)
    .bind(task_id)
    .execute(pg)
    .await?;

    Ok(result.rows_affected() == 1)
}

// ---------------------------------------------------------------------------
// Audit log (Postgres)
// ---------------------------------------------------------------------------

async fn write_audit_log(
    pg: &PgPool,
    trace_id: &str,
    agent_id: &str,
    operation: &str,
    result_status: &str,
    result_summary: Option<&str>,
    duration_ms: Option<i32>,
) -> Result<(), BroodlinkError> {
    sqlx::query(
        "INSERT INTO audit_log (trace_id, agent_id, service, operation, result_status, result_summary, duration_ms, created_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7, NOW())",
    )
    .bind(trace_id)
    .bind(agent_id)
    .bind(SERVICE_NAME)
    .bind(operation)
    .bind(result_status)
    .bind(result_summary)
    .bind(duration_ms)
    .execute(pg)
    .await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Dead-letter publishing
// ---------------------------------------------------------------------------

async fn publish_dead_letter(
    state: &AppState,
    task_id: &str,
    reason: &str,
) -> Result<(), BroodlinkError> {
    let subject = format!(
        "{}.{}.coordinator.dead_letter",
        state.config.nats.subject_prefix, state.config.broodlink.env,
    );
    let payload = serde_json::json!({
        "task_id": task_id,
        "reason": reason,
        "service": SERVICE_NAME,
        "timestamp": chrono::Utc::now().to_rfc3339(),
    });

    if let Ok(bytes) = serde_json::to_vec(&payload) {
        state.nats.publish(subject, bytes.into()).await?;
    }

    state.tasks_dead_lettered.fetch_add(1, Ordering::Relaxed);

    // Also mark the task as 'failed' in Postgres so it is not picked up again
    if let Err(e) = sqlx::query(
        "UPDATE task_queue SET status = 'failed', updated_at = NOW()
         WHERE id = $1 AND status = 'pending'",
    )
    .bind(task_id)
    .execute(&state.pg)
    .await
    {
        warn!(task_id = %task_id, error = %e, "failed to mark task as failed in DB");
    }

    // Persist to dead_letter_queue for inspection and auto-retry
    let max_retries = state.config.dlq.max_retries as i32;
    let backoff_ms = state.config.dlq.backoff_base_ms as i64;
    let next_retry = if state.config.dlq.auto_retry_enabled {
        // First retry after backoff_base_ms
        Some(chrono::Utc::now() + chrono::Duration::milliseconds(backoff_ms))
    } else {
        None
    };

    if let Err(e) = sqlx::query(
        "INSERT INTO dead_letter_queue (task_id, reason, source_service, max_retries, next_retry_at)
         VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(task_id)
    .bind(reason)
    .bind(SERVICE_NAME)
    .bind(max_retries)
    .bind(next_retry)
    .execute(&state.pg)
    .await
    {
        warn!(task_id = %task_id, error = %e, "failed to insert into dead_letter_queue");
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Formula loading (types imported from broodlink_formulas)
// ---------------------------------------------------------------------------

/// Load a formula from Postgres registry first, falling back to TOML files.
async fn load_formula_from_registry(
    pg: &sqlx::PgPool,
    config: &Config,
    formula_name: &str,
) -> Result<FormulaFile, BroodlinkError> {
    // Try Postgres first
    let row: Option<(serde_json::Value,)> = sqlx::query_as(
        "SELECT definition FROM formula_registry WHERE name = $1 AND enabled = true",
    )
    .bind(formula_name)
    .fetch_optional(pg)
    .await?;

    if let Some((definition,)) = row {
        // Increment usage_count and update last_used_at
        if let Err(e) = sqlx::query(
            "UPDATE formula_registry SET usage_count = usage_count + 1, last_used_at = NOW(), updated_at = NOW()
             WHERE name = $1",
        )
        .bind(formula_name)
        .execute(pg)
        .await
        {
            warn!(formula = %formula_name, error = %e, "failed to update formula usage count");
        }

        // Parse JSONB definition into FormulaFile
        let formula: FormulaFile = serde_json::from_value(definition).map_err(|e| {
            BroodlinkError::Internal(format!(
                "failed to parse registry formula {formula_name}: {e}"
            ))
        })?;

        if formula.steps.is_empty() {
            return Err(BroodlinkError::Internal(format!(
                "registry formula {formula_name} has no steps"
            )));
        }

        info!(
            formula = formula_name,
            source = "registry",
            "loaded formula from Postgres registry"
        );
        return Ok(formula);
    }

    // Fall back to TOML file
    info!(
        formula = formula_name,
        source = "toml",
        "formula not in registry, falling back to TOML"
    );
    load_formula(config, formula_name)
}

fn load_formula(config: &Config, formula_name: &str) -> Result<FormulaFile, BroodlinkError> {
    if !broodlink_formulas::validate_formula_name(formula_name) {
        return Err(BroodlinkError::Internal(format!(
            "invalid formula name: {formula_name}"
        )));
    }

    let path = std::path::Path::new(&config.beads.workspace)
        .join(&config.beads.formulas_dir)
        .join(format!("{formula_name}.formula.toml"));

    let content = std::fs::read_to_string(&path).map_err(|e| {
        BroodlinkError::Internal(format!("failed to read formula {formula_name}: {e}"))
    })?;

    let formula: FormulaFile = toml::from_str(&content).map_err(|e| {
        BroodlinkError::Internal(format!("failed to parse formula {formula_name}: {e}"))
    })?;

    if formula.steps.is_empty() {
        return Err(BroodlinkError::Internal(format!(
            "formula {formula_name} has no steps"
        )));
    }

    Ok(formula)
}

/// Replace `{{key}}` placeholders in a prompt template with values from a params map.
fn render_prompt(template: &str, params: &serde_json::Value) -> String {
    let mut result = template.to_string();
    if let Some(obj) = params.as_object() {
        for (key, value) in obj {
            let placeholder = format!("{{{{{key}}}}}");
            let replacement = match value.as_str() {
                Some(s) => s.to_string(),
                None => value.to_string(),
            };
            result = result.replace(&placeholder, &replacement);
        }
    }
    result
}

/// Evaluate a simple condition expression against step_results.
/// Supports: `key.count > N`, `key.count < N`, `key == "value"`, `key.exists`
fn evaluate_condition(expr: &str, step_results: &serde_json::Value) -> bool {
    let expr = expr.trim();

    // key.exists
    if let Some(key) = expr.strip_suffix(".exists") {
        return step_results.get(key).is_some();
    }

    // key.count > N  or  key.count < N
    if let Some((left, right)) = expr.split_once('>') {
        let left = left.trim();
        if let Some(key) = left.strip_suffix(".count") {
            let key = key.trim();
            if let Ok(n) = right.trim().parse::<usize>() {
                return step_results
                    .get(key)
                    .and_then(|v| v.as_array())
                    .is_some_and(|arr| arr.len() > n);
            }
        }
    }

    if let Some((left, right)) = expr.split_once('<') {
        let left = left.trim();
        if let Some(key) = left.strip_suffix(".count") {
            let key = key.trim();
            if let Ok(n) = right.trim().parse::<usize>() {
                return step_results
                    .get(key)
                    .and_then(|v| v.as_array())
                    .is_some_and(|arr| arr.len() < n);
            }
        }
    }

    // key == "value"
    if let Some((left, right)) = expr.split_once("==") {
        let key = left.trim();
        let val = right.trim().trim_matches('"');
        return step_results
            .get(key)
            .and_then(|v| v.as_str())
            .is_some_and(|s| s == val);
    }

    // Default: false (fail-closed — unknown conditions block execution)
    warn!(condition = %expr, "unknown condition expression, defaulting to false");
    false
}

// ---------------------------------------------------------------------------
// Workflow handlers
// ---------------------------------------------------------------------------

async fn handle_workflow_start(
    state: &Arc<AppState>,
    payload: &WorkflowStartPayload,
) -> Result<(), BroodlinkError> {
    info!(
        workflow_id = %payload.workflow_id,
        formula = %payload.formula_name,
        "starting workflow"
    );

    let formula =
        load_formula_from_registry(&state.pg, &state.config, &payload.formula_name).await?;
    let total_steps = formula.steps.len() as i32;

    // Update workflow_runs with total_steps and set status to running
    sqlx::query(
        "UPDATE workflow_runs SET total_steps = $1, status = 'running', updated_at = NOW()
         WHERE id = $2",
    )
    .bind(total_steps)
    .bind(&payload.workflow_id)
    .execute(&state.pg)
    .await?;

    // Create the first step task
    create_step_task(
        state,
        &payload.workflow_id,
        &payload.convoy_id,
        &payload.formula_name,
        &formula,
        0,
        &payload.params,
        &serde_json::json!({}),
    )
    .await?;

    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn create_step_task(
    state: &Arc<AppState>,
    workflow_id: &str,
    convoy_id: &str,
    formula_name: &str,
    formula: &FormulaFile,
    step_index: usize,
    params: &serde_json::Value,
    step_results: &serde_json::Value,
) -> Result<String, BroodlinkError> {
    let step = &formula.steps[step_index];
    let total = formula.steps.len();

    // Render prompt with formula params
    let mut rendered = render_prompt(&step.prompt, params);

    // Prepend step-level system prompt if present
    if let Some(ref sp) = step.system_prompt {
        rendered = format!("{sp}\n\n{rendered}");
    }

    // Append previous step outputs as context
    if let Some(ref input) = step.input {
        let keys = match input {
            FormulaInput::Single(k) => vec![k.clone()],
            FormulaInput::Multiple(ks) => ks.clone(),
        };
        for key in &keys {
            if let Some(val) = step_results.get(key) {
                rendered.push_str(&format!("\n\n--- Previous output ({key}) ---\n{val}"));
            }
        }
    }

    // Append few-shot examples if present
    if let Some(ref examples) = step.examples {
        rendered.push_str("\n\n## Examples:\n");
        rendered.push_str(examples);
    }

    // Append output schema guidance if present
    if let Some(ref schema) = step.output_schema {
        if let Ok(schema_str) = serde_json::to_string_pretty(schema) {
            rendered.push_str(&format!(
                "\n\n## Required output format:\nYour response must conform to this schema:\n```json\n{schema_str}\n```"
            ));
        }
    }

    let task_id = Uuid::new_v4().to_string();
    let trace_id = Uuid::new_v4().to_string();
    let title = format!(
        "[{}/{}] {} — {}",
        step_index + 1,
        total,
        formula_name,
        step.name,
    );

    // Calculate timeout_at if step has timeout_seconds
    let timeout_at: Option<chrono::DateTime<chrono::Utc>> = step
        .timeout_seconds
        .map(|secs| chrono::Utc::now() + chrono::Duration::seconds(secs as i64));

    sqlx::query(
        "INSERT INTO task_queue (id, trace_id, title, description, status, priority, formula_name, convoy_id, workflow_run_id, step_index, step_name, timeout_at, created_at, updated_at)
         VALUES ($1, $2, $3, $4, 'pending', 5, $5, $6, $7, $8, $9, $10, NOW(), NOW())",
    )
    .bind(&task_id)
    .bind(&trace_id)
    .bind(&title)
    .bind(&rendered)
    .bind(formula_name)
    .bind(convoy_id)
    .bind(workflow_id)
    .bind(step_index as i32)
    .bind(&step.name)
    .bind(timeout_at)
    .execute(&state.pg)
    .await?;

    // Update workflow_runs to track current task
    sqlx::query(
        "UPDATE workflow_runs SET current_task_id = $1, current_step = $2, updated_at = NOW()
         WHERE id = $3",
    )
    .bind(&task_id)
    .bind(step_index as i32)
    .bind(workflow_id)
    .execute(&state.pg)
    .await?;

    // Publish task_available to trigger routing
    let subject = format!(
        "{}.{}.coordinator.task_available",
        state.config.nats.subject_prefix, state.config.broodlink.env,
    );
    let nats_payload = serde_json::json!({
        "task_id": task_id,
        "role": step.agent_role,
    });
    if let Ok(bytes) = serde_json::to_vec(&nats_payload) {
        if let Err(e) = state.nats.publish(subject, bytes.into()).await {
            warn!(error = %e, "failed to publish task_available for workflow step");
        }
    }

    info!(
        workflow_id = %workflow_id,
        task_id = %task_id,
        step = step_index,
        step_name = %step.name,
        role = %step.agent_role,
        "created workflow step task"
    );

    Ok(task_id)
}

async fn handle_task_completed(
    state: &Arc<AppState>,
    payload: &TaskCompletedPayload,
) -> Result<(), BroodlinkError> {
    // Check if this task belongs to a workflow
    let row = sqlx::query(
        "SELECT workflow_run_id, step_index, step_name
         FROM task_queue WHERE id = $1",
    )
    .bind(&payload.task_id)
    .fetch_optional(&state.pg)
    .await?;

    let row = match row {
        Some(r) => r,
        None => return Ok(()), // task not found, ignore
    };

    let workflow_run_id: Option<String> = row.get("workflow_run_id");
    let workflow_run_id = match workflow_run_id {
        Some(id) => id,
        None => return Ok(()), // not a workflow task, ignore
    };

    let step_index: Option<i32> = row.get("step_index");
    let step_index = step_index.unwrap_or(0) as usize;

    info!(
        task_id = %payload.task_id,
        workflow_id = %workflow_run_id,
        step = step_index,
        "workflow step completed"
    );

    // Fetch workflow_runs row
    let wf_row = sqlx::query(
        "SELECT formula_name, params, step_results, total_steps, convoy_id
         FROM workflow_runs WHERE id = $1",
    )
    .bind(&workflow_run_id)
    .fetch_optional(&state.pg)
    .await?;

    let wf_row = match wf_row {
        Some(r) => r,
        None => {
            warn!(workflow_id = %workflow_run_id, "workflow_runs row not found");
            return Ok(());
        }
    };

    let formula_name: String = wf_row.get("formula_name");
    let params: serde_json::Value = wf_row.get("params");
    let mut step_results: serde_json::Value = wf_row.get("step_results");
    let total_steps: i32 = wf_row.get("total_steps");
    let convoy_id: String = wf_row.get("convoy_id");

    // Load formula to get step's output name
    let formula = load_formula(&state.config, &formula_name)?;
    let step_def = formula.steps.get(step_index).ok_or_else(|| {
        BroodlinkError::Internal(format!(
            "step index {step_index} out of bounds (formula has {} steps)",
            formula.steps.len()
        ))
    })?;

    // Accumulate result into step_results
    if let Some(ref result_data) = payload.result_data {
        if let Some(obj) = step_results.as_object_mut() {
            obj.insert(step_def.output.clone(), result_data.clone());
        }
    }

    // Validate step output against schema if defined
    if let Some(ref schema) = step_def.output_schema {
        if let Some(ref result_data) = payload.result_data {
            if let Some(required) = schema.get("required").and_then(|r| r.as_array()) {
                let valid = if let Some(obj) = result_data.as_object() {
                    required
                        .iter()
                        .all(|f| f.as_str().map_or(true, |name| obj.contains_key(name)))
                } else if let Ok(parsed) =
                    serde_json::from_str::<serde_json::Value>(result_data.as_str().unwrap_or(""))
                {
                    if let Some(obj) = parsed.as_object() {
                        required
                            .iter()
                            .all(|f| f.as_str().map_or(true, |name| obj.contains_key(name)))
                    } else {
                        true // Not an object, skip validation
                    }
                } else {
                    true // Can't parse, skip validation
                };

                if !valid {
                    warn!(
                        step = %step_def.name,
                        workflow_run_id = %workflow_run_id,
                        "step output failed schema validation"
                    );
                    // Could retry here, but for now just log the warning
                }
            }
        }
    }

    // Check if this step was part of a parallel group
    let current_group = step_def.group;
    if current_group.is_some() {
        // Decrement parallel_pending counter
        sqlx::query(
            "UPDATE workflow_runs SET parallel_pending = parallel_pending - 1, step_results = $1, updated_at = NOW()
             WHERE id = $2",
        )
        .bind(&step_results)
        .bind(&workflow_run_id)
        .execute(&state.pg)
        .await?;

        // Check if more parallel tasks pending
        let pending: i32 =
            sqlx::query_as::<_, (i32,)>("SELECT parallel_pending FROM workflow_runs WHERE id = $1")
                .bind(&workflow_run_id)
                .fetch_one(&state.pg)
                .await?
                .0;

        if pending > 0 {
            info!(
                workflow_id = %workflow_run_id,
                remaining = pending,
                "parallel step completed, waiting for others"
            );
            return Ok(());
        }
        // All parallel steps done — fall through to advance
    } else {
        // Sequential step — just update step_results
        sqlx::query(
            "UPDATE workflow_runs SET step_results = $1, updated_at = NOW()
             WHERE id = $2",
        )
        .bind(&step_results)
        .bind(&workflow_run_id)
        .execute(&state.pg)
        .await?;
    }

    // Find the next step(s) to execute
    let next_step = step_index + 1;

    // Skip ahead past any steps in the same group (they ran in parallel)
    let mut advance_to = next_step;
    if let Some(grp) = current_group {
        while advance_to < formula.steps.len() {
            if formula.steps.get(advance_to).and_then(|s| s.group) == Some(grp) {
                advance_to += 1;
            } else {
                break;
            }
        }
    }

    if advance_to < total_steps as usize {
        // Check if next step(s) form a parallel group
        let next_def = formula.steps.get(advance_to).ok_or_else(|| {
            BroodlinkError::Internal(format!(
                "advance_to {advance_to} out of bounds (formula has {} steps)",
                formula.steps.len()
            ))
        })?;
        let next_group = next_def.group;

        if let Some(grp) = next_group {
            // Collect all steps in this parallel group
            let group_steps: Vec<usize> = (advance_to..formula.steps.len())
                .take_while(|&i| formula.steps.get(i).and_then(|s| s.group) == Some(grp))
                .collect();

            let group_size = group_steps.len() as i32;

            // Set parallel_pending counter
            sqlx::query(
                "UPDATE workflow_runs SET parallel_pending = $1, updated_at = NOW()
                 WHERE id = $2",
            )
            .bind(group_size)
            .bind(&workflow_run_id)
            .execute(&state.pg)
            .await?;

            // Create all parallel tasks
            for &si in &group_steps {
                let step = &formula.steps[si];
                // Evaluate conditional
                if let Some(ref cond) = step.when {
                    if !evaluate_condition(cond, &step_results) {
                        info!(step = %step.name, condition = %cond, "skipping step (condition false)");
                        // Decrement pending since we're skipping
                        sqlx::query(
                            "UPDATE workflow_runs SET parallel_pending = parallel_pending - 1 WHERE id = $1",
                        )
                        .bind(&workflow_run_id)
                        .execute(&state.pg)
                        .await?;
                        continue;
                    }
                }
                create_step_task(
                    state,
                    &workflow_run_id,
                    &convoy_id,
                    &formula_name,
                    &formula,
                    si,
                    &params,
                    &step_results,
                )
                .await?;
            }
        } else {
            // Sequential step — evaluate condition
            if let Some(ref cond) = next_def.when {
                if !evaluate_condition(cond, &step_results) {
                    info!(step = %next_def.name, condition = %cond, "skipping step (condition false)");
                    // Skip to step after this one by recursively handling as if this step completed
                    // We do this by advancing step_results and trying the next step
                    let skip_step = advance_to + 1;
                    if skip_step < total_steps as usize {
                        create_step_task(
                            state,
                            &workflow_run_id,
                            &convoy_id,
                            &formula_name,
                            &formula,
                            skip_step,
                            &params,
                            &step_results,
                        )
                        .await?;
                    } else {
                        // All remaining steps skipped — complete workflow
                        sqlx::query(
                            "UPDATE workflow_runs SET status = 'completed', step_results = $1, completed_at = NOW(), updated_at = NOW()
                             WHERE id = $2",
                        )
                        .bind(&step_results)
                        .bind(&workflow_run_id)
                        .execute(&state.pg)
                        .await?;
                    }
                    return Ok(());
                }
            }
            create_step_task(
                state,
                &workflow_run_id,
                &convoy_id,
                &formula_name,
                &formula,
                advance_to,
                &params,
                &step_results,
            )
            .await?;
        }
    } else {
        // All steps done — mark workflow as completed
        sqlx::query(
            "UPDATE workflow_runs SET status = 'completed', step_results = $1, completed_at = NOW(), updated_at = NOW()
             WHERE id = $2",
        )
        .bind(&step_results)
        .bind(&workflow_run_id)
        .execute(&state.pg)
        .await?;

        // Publish workflow_completed event
        let subject = format!(
            "{}.{}.coordinator.workflow_completed",
            state.config.nats.subject_prefix, state.config.broodlink.env,
        );
        let nats_payload = serde_json::json!({
            "workflow_id": workflow_run_id,
            "formula_name": formula_name,
            "step_results": step_results,
        });
        if let Ok(bytes) = serde_json::to_vec(&nats_payload) {
            if let Err(e) = state.nats.publish(subject, bytes.into()).await {
                warn!(error = %e, "failed to publish workflow_completed");
            }
        }

        info!(
            workflow_id = %workflow_run_id,
            formula = %formula_name,
            "workflow completed"
        );
    }

    Ok(())
}

async fn handle_task_failed(
    state: &Arc<AppState>,
    payload: &TaskFailedPayload,
) -> Result<(), BroodlinkError> {
    // Check if this task belongs to a workflow
    let row = sqlx::query(
        "SELECT workflow_run_id, step_index, step_name, retry_count
         FROM task_queue WHERE id = $1",
    )
    .bind(&payload.task_id)
    .fetch_optional(&state.pg)
    .await?;

    let row = match row {
        Some(r) => r,
        None => return Ok(()),
    };

    let workflow_run_id: Option<String> = row.get("workflow_run_id");
    let workflow_run_id = match workflow_run_id {
        Some(id) => id,
        None => return Ok(()), // not a workflow task, ignore
    };

    let step_index: i32 = row.get::<Option<i32>, _>("step_index").unwrap_or(0);
    let retry_count: i32 = row.get::<Option<i32>, _>("retry_count").unwrap_or(0);

    // Load formula to check step-level retry config
    let wf_row = sqlx::query(
        "SELECT formula_name, convoy_id, params, step_results
         FROM workflow_runs WHERE id = $1",
    )
    .bind(&workflow_run_id)
    .fetch_optional(&state.pg)
    .await?;

    let wf_row = match wf_row {
        Some(r) => r,
        None => return Ok(()),
    };

    let formula_name: String = wf_row.get("formula_name");
    let convoy_id: String = wf_row.get("convoy_id");
    let params: serde_json::Value = wf_row.get("params");
    let step_results: serde_json::Value = wf_row.get("step_results");

    let formula = load_formula(&state.config, &formula_name)?;
    let step_def = formula.steps.get(step_index as usize);

    // Check if step has retries configured
    if let Some(step) = step_def {
        let max_retries = step.retries.unwrap_or(0) as i32;
        if retry_count < max_retries {
            let new_count = retry_count + 1;
            let backoff_ms = match step.backoff.as_deref() {
                Some("exponential") => (1000i64 * 2i64.pow(new_count as u32)).min(60_000),
                _ => 1000,
            };

            info!(
                task_id = %payload.task_id,
                workflow_id = %workflow_run_id,
                step = %step.name,
                retry = new_count,
                max = max_retries,
                "retrying workflow step"
            );

            // Reset task to pending with incremented retry_count
            sqlx::query(
                "UPDATE task_queue SET status = 'pending', retry_count = $1, updated_at = NOW()
                 WHERE id = $2",
            )
            .bind(new_count)
            .bind(&payload.task_id)
            .execute(&state.pg)
            .await?;

            // Schedule retry after backoff
            tokio::time::sleep(Duration::from_millis(backoff_ms as u64)).await;

            // Re-publish task_available
            let subject = format!(
                "{}.{}.coordinator.task_available",
                state.config.nats.subject_prefix, state.config.broodlink.env,
            );
            let nats_payload = serde_json::json!({
                "task_id": payload.task_id,
                "role": step.agent_role,
                "retry": true,
                "retry_count": new_count,
            });
            if let Ok(bytes) = serde_json::to_vec(&nats_payload) {
                if let Err(e) = state.nats.publish(subject, bytes.into()).await {
                    warn!(task_id = %payload.task_id, error = %e, "failed to re-publish task_available after retry");
                }
            }

            return Ok(());
        }
    }

    // Retries exhausted — check for on_failure handler
    if let Some(ref error_handler) = formula.on_failure {
        warn!(
            task_id = %payload.task_id,
            workflow_id = %workflow_run_id,
            "step retries exhausted, invoking on_failure handler"
        );

        // Create the error handler task
        let handler_task_id = Uuid::new_v4().to_string();
        let handler_trace_id = Uuid::new_v4().to_string();
        let error_context = serde_json::json!({
            "failed_task_id": payload.task_id,
            "failed_step": step_def.map(|s| s.name.as_str()).unwrap_or("unknown"),
            "error": payload.error.as_deref().unwrap_or("unknown"),
            "step_results": step_results,
        });

        let mut rendered = render_prompt(&error_handler.prompt, &params);
        rendered.push_str(&format!("\n\n--- Error context ---\n{error_context}"));

        sqlx::query(
            "INSERT INTO task_queue (id, trace_id, title, description, status, priority, formula_name, convoy_id, workflow_run_id, step_name, created_at, updated_at)
             VALUES ($1, $2, $3, $4, 'pending', 10, $5, $6, $7, $8, NOW(), NOW())",
        )
        .bind(&handler_task_id)
        .bind(&handler_trace_id)
        .bind(format!("[error-handler] {} — {}", formula_name, error_handler.name))
        .bind(&rendered)
        .bind(&formula_name)
        .bind(&convoy_id)
        .bind(&workflow_run_id)
        .bind(&error_handler.name)
        .execute(&state.pg)
        .await?;

        // Track error handler in workflow_runs
        sqlx::query(
            "UPDATE workflow_runs SET error_handler_task_id = $1, updated_at = NOW()
             WHERE id = $2",
        )
        .bind(&handler_task_id)
        .bind(&workflow_run_id)
        .execute(&state.pg)
        .await?;

        // Publish for routing
        let subject = format!(
            "{}.{}.coordinator.task_available",
            state.config.nats.subject_prefix, state.config.broodlink.env,
        );
        let nats_payload = serde_json::json!({
            "task_id": handler_task_id,
            "role": error_handler.agent_role,
        });
        if let Ok(bytes) = serde_json::to_vec(&nats_payload) {
            if let Err(e) = state.nats.publish(subject, bytes.into()).await {
                warn!(task_id = %handler_task_id, error = %e, "failed to publish error handler task_available");
            }
        }

        return Ok(());
    }

    // No retries, no error handler — mark workflow as failed
    warn!(
        task_id = %payload.task_id,
        workflow_id = %workflow_run_id,
        "workflow step failed, marking workflow as failed"
    );

    sqlx::query(
        "UPDATE workflow_runs SET status = 'failed', updated_at = NOW()
         WHERE id = $1",
    )
    .bind(&workflow_run_id)
    .execute(&state.pg)
    .await?;

    Ok(())
}

// ---------------------------------------------------------------------------
// Workflow NATS subscription loops
// ---------------------------------------------------------------------------

async fn run_workflow_start_subscription(
    state: Arc<AppState>,
    mut shutdown_rx: watch::Receiver<bool>,
) -> Result<(), BroodlinkError> {
    let subject = format!(
        "{}.{}.coordinator.workflow_start",
        state.config.nats.subject_prefix, state.config.broodlink.env,
    );

    info!(subject = %subject, "subscribing to workflow_start");

    let mut subscriber = state.nats.subscribe(subject.clone()).await?;

    loop {
        tokio::select! {
            msg = subscriber.next() => {
                match msg {
                    Some(nats_msg) => {
                        let payload_result: Result<WorkflowStartPayload, _> =
                            serde_json::from_slice(&nats_msg.payload);

                        match payload_result {
                            Ok(payload) => {
                                let state_clone = Arc::clone(&state);
                                tokio::spawn(async move {
                                    if let Err(e) = handle_workflow_start(&state_clone, &payload).await {
                                        warn!(
                                            error = %e,
                                            workflow_id = %payload.workflow_id,
                                            "workflow start failed"
                                        );
                                    }
                                });
                            }
                            Err(e) => {
                                warn!(error = %e, "failed to deserialize workflow_start payload");
                            }
                        }
                    }
                    None => {
                        warn!("workflow_start subscription stream ended");
                        break;
                    }
                }
            }
            _ = shutdown_rx.changed() => {
                info!("shutdown signal, stopping workflow_start subscription");
                break;
            }
        }
    }

    if let Err(e) = subscriber.unsubscribe().await {
        warn!(error = %e, "failed to unsubscribe from workflow_start");
    }

    Ok(())
}

async fn run_task_completed_subscription(
    state: Arc<AppState>,
    mut shutdown_rx: watch::Receiver<bool>,
) -> Result<(), BroodlinkError> {
    let subject = format!(
        "{}.{}.coordinator.task_completed",
        state.config.nats.subject_prefix, state.config.broodlink.env,
    );

    info!(subject = %subject, "subscribing to task_completed");

    let mut subscriber = state.nats.subscribe(subject.clone()).await?;

    loop {
        tokio::select! {
            msg = subscriber.next() => {
                match msg {
                    Some(nats_msg) => {
                        let payload_result: Result<TaskCompletedPayload, _> =
                            serde_json::from_slice(&nats_msg.payload);

                        match payload_result {
                            Ok(payload) => {
                                let state_clone = Arc::clone(&state);
                                tokio::spawn(async move {
                                    if let Err(e) = handle_task_completed(&state_clone, &payload).await {
                                        warn!(
                                            error = %e,
                                            task_id = %payload.task_id,
                                            "task_completed handling failed"
                                        );
                                    }
                                });
                            }
                            Err(e) => {
                                warn!(error = %e, "failed to deserialize task_completed payload");
                            }
                        }
                    }
                    None => {
                        warn!("task_completed subscription stream ended");
                        break;
                    }
                }
            }
            _ = shutdown_rx.changed() => {
                info!("shutdown signal, stopping task_completed subscription");
                break;
            }
        }
    }

    if let Err(e) = subscriber.unsubscribe().await {
        warn!(error = %e, "failed to unsubscribe from task_completed");
    }

    Ok(())
}

async fn run_task_failed_subscription(
    state: Arc<AppState>,
    mut shutdown_rx: watch::Receiver<bool>,
) -> Result<(), BroodlinkError> {
    let subject = format!(
        "{}.{}.coordinator.task_failed",
        state.config.nats.subject_prefix, state.config.broodlink.env,
    );

    info!(subject = %subject, "subscribing to task_failed");

    let mut subscriber = state.nats.subscribe(subject.clone()).await?;

    loop {
        tokio::select! {
            msg = subscriber.next() => {
                match msg {
                    Some(nats_msg) => {
                        let payload_result: Result<TaskFailedPayload, _> =
                            serde_json::from_slice(&nats_msg.payload);

                        match payload_result {
                            Ok(payload) => {
                                let state_clone = Arc::clone(&state);
                                tokio::spawn(async move {
                                    if let Err(e) = handle_task_failed(&state_clone, &payload).await {
                                        warn!(
                                            error = %e,
                                            task_id = %payload.task_id,
                                            "task_failed handling failed"
                                        );
                                    }
                                });
                            }
                            Err(e) => {
                                warn!(error = %e, "failed to deserialize task_failed payload");
                            }
                        }
                    }
                    None => {
                        warn!("task_failed subscription stream ended");
                        break;
                    }
                }
            }
            _ = shutdown_rx.changed() => {
                info!("shutdown signal, stopping task_failed subscription");
                break;
            }
        }
    }

    if let Err(e) = subscriber.unsubscribe().await {
        warn!(error = %e, "failed to unsubscribe from task_failed");
    }

    Ok(())
}

// v0.11.0: Negotiation subscription loops

async fn run_task_declined_subscription(
    state: Arc<AppState>,
    mut shutdown_rx: watch::Receiver<bool>,
) -> Result<(), BroodlinkError> {
    let subject = format!(
        "{}.{}.coordinator.task_declined",
        state.config.nats.subject_prefix, state.config.broodlink.env,
    );

    info!(subject = %subject, "subscribing to task_declined");
    let mut subscriber = state.nats.subscribe(subject.clone()).await?;

    loop {
        tokio::select! {
            msg = subscriber.next() => {
                match msg {
                    Some(nats_msg) => {
                        match serde_json::from_slice::<TaskDeclinedPayload>(&nats_msg.payload) {
                            Ok(payload) => {
                                let state_clone = Arc::clone(&state);
                                tokio::spawn(async move {
                                    if let Err(e) = handle_task_declined(&state_clone, &payload).await {
                                        warn!(
                                            error = %e,
                                            task_id = %payload.task_id,
                                            "task_declined handling failed"
                                        );
                                    }
                                });
                            }
                            Err(e) => {
                                warn!(error = %e, "failed to deserialize task_declined payload");
                            }
                        }
                    }
                    None => {
                        warn!("task_declined subscription stream ended");
                        break;
                    }
                }
            }
            _ = shutdown_rx.changed() => {
                info!("shutdown signal, stopping task_declined subscription");
                break;
            }
        }
    }

    if let Err(e) = subscriber.unsubscribe().await {
        warn!(error = %e, "failed to unsubscribe from task_declined");
    }
    Ok(())
}

async fn run_task_context_request_subscription(
    state: Arc<AppState>,
    mut shutdown_rx: watch::Receiver<bool>,
) -> Result<(), BroodlinkError> {
    let subject = format!(
        "{}.{}.coordinator.task_context_request",
        state.config.nats.subject_prefix, state.config.broodlink.env,
    );

    info!(subject = %subject, "subscribing to task_context_request");
    let mut subscriber = state.nats.subscribe(subject.clone()).await?;

    loop {
        tokio::select! {
            msg = subscriber.next() => {
                match msg {
                    Some(nats_msg) => {
                        match serde_json::from_slice::<TaskContextRequestPayload>(&nats_msg.payload) {
                            Ok(payload) => {
                                let state_clone = Arc::clone(&state);
                                tokio::spawn(async move {
                                    if let Err(e) = handle_task_context_request(&state_clone, &payload).await {
                                        warn!(
                                            error = %e,
                                            task_id = %payload.task_id,
                                            "task_context_request handling failed"
                                        );
                                    }
                                });
                            }
                            Err(e) => {
                                warn!(error = %e, "failed to deserialize task_context_request payload");
                            }
                        }
                    }
                    None => {
                        warn!("task_context_request subscription stream ended");
                        break;
                    }
                }
            }
            _ = shutdown_rx.changed() => {
                info!("shutdown signal, stopping task_context_request subscription");
                break;
            }
        }
    }

    if let Err(e) = subscriber.unsubscribe().await {
        warn!(error = %e, "failed to unsubscribe from task_context_request");
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Metrics publishing
// ---------------------------------------------------------------------------

async fn publish_metrics(state: &AppState) -> Result<(), BroodlinkError> {
    let payload = MetricsPayload {
        service: SERVICE_NAME.to_string(),
        tasks_claimed: state.tasks_claimed.load(Ordering::Relaxed),
        tasks_dead_lettered: state.tasks_dead_lettered.load(Ordering::Relaxed),
        claim_retries_total: state.claim_retries_total.load(Ordering::Relaxed),
        uptime_seconds: state.start_time.elapsed().as_secs(),
    };

    if let Ok(bytes) = serde_json::to_vec(&payload) {
        let subject = "broodlink.metrics.coordinator".to_string();
        state.nats.publish(subject, bytes.into()).await?;
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Core task routing logic
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_lines)]
async fn handle_task_available(
    state: &Arc<AppState>,
    payload: &TaskAvailablePayload,
) -> Result<(), BroodlinkError> {
    let trace_id = Uuid::new_v4().to_string();
    let started = std::time::Instant::now();

    info!(
        trace_id = %trace_id,
        task_id = %payload.task_id,
        task_type = ?payload.task_type,
        role = ?payload.role,
        "received task_available"
    );

    // 1. Fetch the task from Postgres to verify it exists and is pending
    let task = match fetch_task(&state.pg, &payload.task_id).await? {
        Some(t) => t,
        None => {
            info!(
                task_id = %payload.task_id,
                "task not found or already claimed, skipping"
            );
            return Ok(());
        }
    };

    // 2. Determine the required role.  The NATS payload may specify it,
    //    otherwise fall back to "worker" as the default role.
    let role = payload.role.as_deref().unwrap_or("worker");
    let cost_tier = payload.cost_tier.as_deref();
    let formula_name = task.formula_name.as_deref();
    let routing_config = &state.config.routing;

    // v0.9.0: Determine model domain for smart routing
    let task_domain =
        determine_model_hint(formula_name, task.description.as_deref(), &state.config);
    let agent_domain_map: HashMap<String, Vec<String>> = state
        .config
        .agents
        .iter()
        .map(|(id, cfg)| (id.clone(), cfg.model_domains.clone()))
        .collect();

    // 3. Outer retry loop: re-query agents on full exhaustion
    let mut claimed = false;
    let mut claiming_agent = String::new();
    let mut claiming_tier = String::new();
    let mut winning_score = 0.0_f64;
    let mut winning_breakdown: Option<ScoreBreakdown> = None;
    let mut candidates_evaluated = 0_usize;
    let mut outer_attempt = 0_u32;

    for backoff_round in 0..MAX_CLAIM_RETRIES {
        outer_attempt = backoff_round;

        // 3a. Query Dolt agent_profiles for eligible agents
        let mut agents = find_eligible_agents(&state.dolt, role, cost_tier).await?;

        // v0.11.0: Filter out agents that have already declined this task
        if !task.declined_agents.is_empty() {
            agents.retain(|a| !task.declined_agents.contains(&a.agent_id));
        }

        if agents.is_empty() {
            warn!(
                task_id = %payload.task_id,
                role = %role,
                cost_tier = ?cost_tier,
                "no eligible agents found"
            );

            let reason = format!("no eligible agents for role={role}, cost_tier={cost_tier:?}");
            publish_dead_letter(state, &payload.task_id, &reason).await?;

            let duration_ms = i32::try_from(started.elapsed().as_millis()).unwrap_or(i32::MAX);
            if let Err(e) = write_audit_log(
                &state.pg,
                &trace_id,
                "coordinator",
                "route_task",
                "error",
                Some(&reason),
                Some(duration_ms),
            )
            .await
            {
                warn!(error = %e, "failed to write audit log");
            }

            return Err(BroodlinkError::NoEligibleAgent {
                task_id: payload.task_id.clone(),
                role: role.to_string(),
            });
        }

        // 3b. Fetch metrics from Postgres and score all candidates
        let agent_ids: Vec<String> = agents.iter().map(|a| a.agent_id.clone()).collect();
        let metrics = fetch_agent_metrics(&state.pg, &agent_ids)
            .await
            .unwrap_or_default();

        let scored = rank_agents(
            agents,
            &metrics,
            formula_name,
            &routing_config.weights,
            routing_config.new_agent_bonus,
            task_domain.as_deref(),
            &agent_domain_map,
        );
        candidates_evaluated = scored.len();

        // 3c. Try each candidate once (highest score first)
        for candidate in &scored {
            info!(
                task_id = %payload.task_id,
                agent_id = %candidate.agent.agent_id,
                score = candidate.score,
                round = backoff_round,
                "attempting claim on scored candidate"
            );

            match try_claim_task(&state.pg, &payload.task_id, &candidate.agent.agent_id).await {
                Ok(true) => {
                    claimed = true;
                    claiming_agent.clone_from(&candidate.agent.agent_id);
                    claiming_tier.clone_from(&candidate.agent.cost_tier);
                    winning_score = candidate.score;
                    winning_breakdown = Some(candidate.breakdown.clone());
                    break;
                }
                Ok(false) => {
                    state.claim_retries_total.fetch_add(1, Ordering::Relaxed);
                    info!(
                        task_id = %payload.task_id,
                        agent_id = %candidate.agent.agent_id,
                        "claim contention, trying next candidate"
                    );
                }
                Err(e) => {
                    warn!(
                        task_id = %payload.task_id,
                        agent_id = %candidate.agent.agent_id,
                        error = %e,
                        "database error during claim"
                    );
                    return Err(e);
                }
            }
        }

        if claimed {
            break;
        }

        // 3d. ALL candidates failed in this round — exponential backoff then re-query
        let backoff_ms = BACKOFF_BASE_MS * 2u64.pow(backoff_round);
        info!(
            task_id = %payload.task_id,
            round = backoff_round,
            backoff_ms = backoff_ms,
            candidates = candidates_evaluated,
            "all candidates exhausted, backing off before re-query"
        );
        tokio::time::sleep(Duration::from_millis(backoff_ms)).await;
    }

    let duration_ms = i32::try_from(started.elapsed().as_millis()).unwrap_or(i32::MAX);

    if !claimed {
        let reason = format!(
            "claim contention exhausted for all candidates after {} rounds",
            MAX_CLAIM_RETRIES,
        );
        warn!(
            task_id = %payload.task_id,
            reason = %reason,
            "dead-lettering task"
        );

        publish_dead_letter(state, &payload.task_id, &reason).await?;

        if let Err(e) = write_audit_log(
            &state.pg,
            &trace_id,
            "coordinator",
            "route_task",
            "error",
            Some(&reason),
            Some(duration_ms),
        )
        .await
        {
            warn!(error = %e, "failed to write audit log");
        }

        return Err(BroodlinkError::ClaimExhausted(
            payload.task_id.clone(),
            MAX_CLAIM_RETRIES,
        ));
    }

    // 4. Claimed successfully — publish routing decision to NATS
    let routing_decision = RoutingDecisionPayload {
        task_id: payload.task_id.clone(),
        agent_id: claiming_agent.clone(),
        score: winning_score,
        breakdown: winning_breakdown.unwrap_or(ScoreBreakdown {
            capability: 0.0,
            success_rate: 0.0,
            availability: 0.0,
            cost: 0.0,
            recency: 0.0,
            domain: 0.0,
        }),
        candidates_evaluated,
        attempt: outer_attempt + 1,
    };

    let routing_subject = format!(
        "{}.{}.coordinator.routing_decision",
        state.config.nats.subject_prefix, state.config.broodlink.env,
    );
    if let Ok(bytes) = serde_json::to_vec(&routing_decision) {
        if let Err(e) = state
            .nats
            .publish(routing_subject.clone(), bytes.into())
            .await
        {
            warn!(error = %e, subject = %routing_subject, "failed to publish routing decision");
        }
    }

    // 5. Dispatch to the agent's NATS subject
    info!(
        task_id = %payload.task_id,
        agent_id = %claiming_agent,
        cost_tier = %claiming_tier,
        score = winning_score,
        duration_ms = %duration_ms,
        "task claimed and dispatching"
    );

    state.tasks_claimed.fetch_add(1, Ordering::Relaxed);

    let dispatch_subject = format!(
        "{}.{}.agent.{}.task",
        state.config.nats.subject_prefix, state.config.broodlink.env, claiming_agent,
    );

    let dispatch_payload = TaskDispatchPayload {
        task_id: task.id.clone(),
        title: task.title,
        description: task.description,
        priority: task.priority,
        formula_name: task.formula_name,
        convoy_id: task.convoy_id,
        assigned_by: SERVICE_NAME.to_string(),
        model_hint: task_domain.clone(),
    };

    if let Ok(bytes) = serde_json::to_vec(&dispatch_payload) {
        if let Err(e) = state
            .nats
            .publish(dispatch_subject.clone(), bytes.into())
            .await
        {
            warn!(
                error = %e,
                subject = %dispatch_subject,
                "failed to publish task dispatch"
            );
        }
    }

    // 6. Write audit log
    let summary = format!(
        "task {} claimed by {} (score={winning_score:.3}, cost_tier={claiming_tier})",
        payload.task_id, claiming_agent,
    );
    if let Err(e) = write_audit_log(
        &state.pg,
        &trace_id,
        &claiming_agent,
        "route_task",
        "ok",
        Some(&summary),
        Some(duration_ms),
    )
    .await
    {
        warn!(error = %e, "failed to write audit log");
    }

    // 7. Publish metrics (best-effort)
    if let Err(e) = publish_metrics(state).await {
        warn!(error = %e, "failed to publish metrics");
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// v0.11.0: Agent negotiation protocol — decline and context request handlers
// ---------------------------------------------------------------------------

async fn handle_task_declined(
    state: &Arc<AppState>,
    payload: &TaskDeclinedPayload,
) -> Result<(), BroodlinkError> {
    let max_declines = state.config.collaboration.max_declines_per_task;

    info!(
        task_id = %payload.task_id,
        agent_id = %payload.agent_id,
        reason = ?payload.reason,
        suggested_agent = ?payload.suggested_agent,
        "agent declined task"
    );

    // 1. Log to task_negotiations table
    sqlx::query(
        "INSERT INTO task_negotiations (task_id, agent_id, action, reason, suggested_agent, created_at)
         VALUES ($1, $2, 'declined', $3, $4, NOW())",
    )
    .bind(&payload.task_id)
    .bind(&payload.agent_id)
    .bind(&payload.reason)
    .bind(&payload.suggested_agent)
    .execute(&state.pg)
    .await?;

    // 2. Update task_queue: increment decline_count, append to declined_agents
    let row = sqlx::query(
        "UPDATE task_queue
         SET decline_count = decline_count + 1,
             declined_agents = COALESCE(declined_agents, '[]'::jsonb) || to_jsonb($2::text),
             assigned_agent = NULL,
             status = 'pending',
             updated_at = NOW()
         WHERE id = $1 AND status IN ('claimed', 'in_progress')
         RETURNING decline_count",
    )
    .bind(&payload.task_id)
    .bind(&payload.agent_id)
    .fetch_optional(&state.pg)
    .await?;

    let decline_count: i32 = match row {
        Some(r) => r.get("decline_count"),
        None => {
            warn!(task_id = %payload.task_id, "task not found or not in claimable state for decline");
            return Ok(());
        }
    };

    // 3. Log service event
    let _ = sqlx::query(
        "INSERT INTO service_events (service, event_type, severity, details, created_at)
         VALUES ('coordinator', 'task_declined', 'info', $1, NOW())",
    )
    .bind(serde_json::json!({
        "task_id": payload.task_id,
        "agent_id": payload.agent_id,
        "reason": payload.reason,
        "suggested_agent": payload.suggested_agent,
        "decline_count": decline_count,
    }))
    .execute(&state.pg)
    .await;

    // 4. If max declines reached, dead-letter the task
    if decline_count as u32 >= max_declines {
        let reason = format!(
            "task declined {} times (max={}), agents: declined by {}",
            decline_count, max_declines, payload.agent_id,
        );
        warn!(task_id = %payload.task_id, %reason, "dead-lettering over-declined task");
        publish_dead_letter(state, &payload.task_id, &reason).await?;
        return Ok(());
    }

    // 5. Re-publish task_available so it gets re-routed (excluding declined agents)
    let subject = format!(
        "{}.{}.coordinator.task_available",
        state.config.nats.subject_prefix, state.config.broodlink.env,
    );
    let nats_payload = serde_json::json!({ "task_id": payload.task_id });
    if let Ok(bytes) = serde_json::to_vec(&nats_payload) {
        let _ = state.nats.publish(subject, bytes.into()).await;
    }

    Ok(())
}

async fn handle_task_context_request(
    state: &Arc<AppState>,
    payload: &TaskContextRequestPayload,
) -> Result<(), BroodlinkError> {
    info!(
        task_id = %payload.task_id,
        agent_id = %payload.agent_id,
        questions = ?payload.questions,
        "agent requested context for task"
    );

    // 1. Log to task_negotiations table
    let questions_json = serde_json::to_value(&payload.questions).unwrap_or_default();
    sqlx::query(
        "INSERT INTO task_negotiations (task_id, agent_id, action, questions, created_at)
         VALUES ($1, $2, 'context_requested', $3, NOW())",
    )
    .bind(&payload.task_id)
    .bind(&payload.agent_id)
    .bind(&questions_json)
    .execute(&state.pg)
    .await?;

    // 2. Update task_queue with context request info
    sqlx::query(
        "UPDATE task_queue
         SET status = 'context_requested',
             context_questions = $2,
             context_requested_by = $3,
             context_requested_at = NOW(),
             updated_at = NOW()
         WHERE id = $1 AND status IN ('claimed', 'in_progress')",
    )
    .bind(&payload.task_id)
    .bind(&questions_json)
    .bind(&payload.agent_id)
    .execute(&state.pg)
    .await?;

    // 3. Log service event
    let _ = sqlx::query(
        "INSERT INTO service_events (service, event_type, severity, details, created_at)
         VALUES ('coordinator', 'task_context_requested', 'info', $1, NOW())",
    )
    .bind(serde_json::json!({
        "task_id": payload.task_id,
        "agent_id": payload.agent_id,
        "questions": payload.questions,
    }))
    .execute(&state.pg)
    .await;

    // 4. Publish questions to the parent/originator agent via NATS
    //    (the agent that created the task can listen on this subject)
    let subject = format!(
        "{}.{}.coordinator.task_context_needed",
        state.config.nats.subject_prefix, state.config.broodlink.env,
    );
    let nats_payload = serde_json::json!({
        "task_id": payload.task_id,
        "requesting_agent": payload.agent_id,
        "questions": payload.questions,
    });
    if let Ok(bytes) = serde_json::to_vec(&nats_payload) {
        let _ = state.nats.publish(subject, bytes.into()).await;
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// NATS subscription loop
// ---------------------------------------------------------------------------

async fn run_subscription(
    state: Arc<AppState>,
    mut shutdown_rx: watch::Receiver<bool>,
) -> Result<(), BroodlinkError> {
    let subject = format!(
        "{}.{}.coordinator.task_available",
        state.config.nats.subject_prefix, state.config.broodlink.env,
    );

    info!(subject = %subject, "subscribing to task_available");

    let mut subscriber = state.nats.subscribe(subject.clone()).await?;

    loop {
        tokio::select! {
            msg = subscriber.next() => {
                match msg {
                    Some(nats_msg) => {
                        let payload_result: Result<TaskAvailablePayload, _> =
                            serde_json::from_slice(&nats_msg.payload);

                        match payload_result {
                            Ok(payload) => {
                                let state_clone = Arc::clone(&state);
                                // Spawn each task handling in its own task to avoid
                                // blocking the subscription loop
                                tokio::spawn(async move {
                                    if let Err(e) = handle_task_available(&state_clone, &payload).await {
                                        warn!(
                                            error = %e,
                                            task_id = %payload.task_id,
                                            "task routing failed"
                                        );
                                    }
                                });
                            }
                            Err(e) => {
                                warn!(
                                    error = %e,
                                    payload_bytes = nats_msg.payload.len(),
                                    "failed to deserialize task_available payload"
                                );
                            }
                        }
                    }
                    None => {
                        warn!("NATS subscription stream ended");
                        break;
                    }
                }
            }
            _ = shutdown_rx.changed() => {
                info!("shutdown signal received, stopping subscription");
                break;
            }
        }
    }

    // Unsubscribe gracefully
    if let Err(e) = subscriber.unsubscribe().await {
        warn!(error = %e, "failed to unsubscribe from NATS");
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Periodic metrics publisher
// ---------------------------------------------------------------------------

async fn metrics_loop(state: Arc<AppState>, mut shutdown_rx: watch::Receiver<bool>) {
    let mut interval = tokio::time::interval(Duration::from_secs(60));

    loop {
        tokio::select! {
            _ = interval.tick() => {
                if let Err(e) = publish_metrics(&state).await {
                    warn!(error = %e, "periodic metrics publish failed");
                }
            }
            _ = shutdown_rx.changed() => {
                info!("shutdown signal, stopping metrics loop");
                break;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// DLQ auto-retry loop
// ---------------------------------------------------------------------------

async fn dlq_retry_loop(state: Arc<AppState>, mut shutdown_rx: watch::Receiver<bool>) {
    if !state.config.dlq.auto_retry_enabled {
        info!("DLQ auto-retry disabled");
        return;
    }

    let check_secs = state.config.dlq.check_interval_secs;
    let mut interval = tokio::time::interval(Duration::from_secs(check_secs));

    info!(interval_secs = check_secs, "DLQ retry loop started");

    loop {
        tokio::select! {
            _ = interval.tick() => {
                match check_dlq_retries(&state).await {
                    Ok(retried) => {
                        if retried > 0 {
                            info!(retried = retried, "DLQ entries retried");
                        }
                    }
                    Err(e) => {
                        warn!(error = %e, "DLQ retry check failed");
                    }
                }
            }
            _ = shutdown_rx.changed() => {
                info!("shutdown signal, stopping DLQ retry loop");
                break;
            }
        }
    }
}

async fn check_dlq_retries(state: &AppState) -> Result<u64, BroodlinkError> {
    // Find DLQ entries eligible for retry: unresolved, retry_count < max_retries, next_retry_at <= now
    let entries: Vec<(i64, String, i32, i32)> = sqlx::query_as(
        "SELECT id, task_id, retry_count, max_retries
         FROM dead_letter_queue
         WHERE resolved = FALSE
           AND retry_count < max_retries
           AND next_retry_at <= NOW()
         ORDER BY next_retry_at ASC
         LIMIT 10",
    )
    .fetch_all(&state.pg)
    .await?;

    let mut retried = 0u64;

    for (dlq_id, task_id, retry_count, max_retries) in &entries {
        let new_count = retry_count + 1;

        // Reset task to 'pending' for re-routing
        let updated = sqlx::query(
            "UPDATE task_queue SET status = 'pending', retry_count = $1, updated_at = NOW()
             WHERE id = $2 AND status = 'failed'",
        )
        .bind(new_count)
        .bind(task_id)
        .execute(&state.pg)
        .await?;

        if updated.rows_affected() == 0 {
            // Task no longer in 'failed' state — mark DLQ entry resolved
            if let Err(e) = sqlx::query(
                "UPDATE dead_letter_queue SET resolved = TRUE, resolved_by = 'auto-skip', updated_at = NOW()
                 WHERE id = $1",
            )
            .bind(dlq_id)
            .execute(&state.pg)
            .await
            {
                warn!(dlq_id = %dlq_id, error = %e, "failed to mark DLQ entry as auto-skipped");
            }
            continue;
        }

        // Calculate next retry with exponential backoff
        let backoff_ms = state.config.dlq.backoff_base_ms as i64 * 2i64.pow(new_count as u32);
        let next_retry = if new_count < *max_retries {
            Some(chrono::Utc::now() + chrono::Duration::milliseconds(backoff_ms))
        } else {
            None // No more retries
        };

        // Update DLQ entry
        if let Err(e) = sqlx::query(
            "UPDATE dead_letter_queue SET retry_count = $1, next_retry_at = $2, updated_at = NOW()
             WHERE id = $3",
        )
        .bind(new_count)
        .bind(next_retry)
        .bind(dlq_id)
        .execute(&state.pg)
        .await
        {
            warn!(dlq_id = %dlq_id, error = %e, "failed to update DLQ retry state");
        }

        // Publish task_available event for re-routing
        let subject = format!(
            "{}.{}.coordinator.task_available",
            state.config.nats.subject_prefix, state.config.broodlink.env,
        );
        let nats_payload = serde_json::json!({
            "task_id": task_id,
            "retry": true,
            "retry_count": new_count,
        });
        if let Ok(bytes) = serde_json::to_vec(&nats_payload) {
            if let Err(e) = state.nats.publish(subject, bytes.into()).await {
                warn!(error = %e, task_id = %task_id, "failed to publish retry event");
            }
        }

        info!(task_id = %task_id, retry = new_count, max = max_retries, "DLQ task retried");
        retried += 1;
    }

    Ok(retried)
}

// ---------------------------------------------------------------------------
// Scheduled task promotion loop
// ---------------------------------------------------------------------------

async fn scheduled_task_loop(state: Arc<AppState>, mut shutdown_rx: watch::Receiver<bool>) {
    let mut interval = tokio::time::interval(Duration::from_secs(60));

    info!("scheduled task loop started");

    loop {
        tokio::select! {
            _ = interval.tick() => {
                match check_scheduled_tasks(&state).await {
                    Ok(promoted) => {
                        if promoted > 0 {
                            info!(promoted = promoted, "scheduled tasks promoted");
                        }
                    }
                    Err(e) => {
                        warn!(error = %e, "scheduled task check failed");
                    }
                }
            }
            _ = shutdown_rx.changed() => {
                info!("shutdown signal, stopping scheduled task loop");
                break;
            }
        }
    }
}

async fn check_scheduled_tasks(state: &AppState) -> Result<u64, BroodlinkError> {
    let entries: Vec<(
        i64,
        String,
        String,
        i32,
        Option<String>,
        Option<serde_json::Value>,
        Option<i64>,
        Option<i32>,
        i32,
    )> = sqlx::query_as(
        "SELECT id, title, COALESCE(description, ''), priority, formula_name, params,
                recurrence_secs, max_runs, run_count
         FROM scheduled_tasks
         WHERE enabled = TRUE AND next_run_at <= NOW()
         ORDER BY next_run_at ASC
         LIMIT 10",
    )
    .fetch_all(&state.pg)
    .await?;

    let mut promoted = 0u64;

    for (
        sched_id,
        title,
        description,
        priority,
        formula_name,
        _params,
        recurrence_secs,
        max_runs,
        run_count,
    ) in &entries
    {
        // Create task in task_queue
        let task_id = Uuid::new_v4().to_string();
        let trace_id = Uuid::new_v4().to_string();
        sqlx::query(
            "INSERT INTO task_queue (id, trace_id, title, description, priority, formula_name, status, created_at, updated_at)
             VALUES ($1, $2, $3, $4, $5, $6, 'pending', NOW(), NOW())",
        )
        .bind(&task_id)
        .bind(&trace_id)
        .bind(title)
        .bind(description)
        .bind(priority)
        .bind(formula_name)
        .execute(&state.pg)
        .await?;

        // Publish task_available NATS event
        let subject = format!(
            "{}.{}.coordinator.task_available",
            state.config.nats.subject_prefix, state.config.broodlink.env,
        );
        let nats_payload = serde_json::json!({
            "task_id": task_id,
            "scheduled_task_id": sched_id,
        });
        if let Ok(bytes) = serde_json::to_vec(&nats_payload) {
            if let Err(e) = state.nats.publish(subject, bytes.into()).await {
                warn!(error = %e, task_id = %task_id, "failed to publish scheduled task event");
            }
        }

        // Update scheduled_task
        let new_count = run_count + 1;
        match recurrence_secs {
            Some(interval_secs) if *interval_secs > 0 => {
                // Recurring: advance next_run_at, check max_runs
                let max_reached = max_runs.map_or(false, |m| new_count >= m);
                if max_reached {
                    sqlx::query(
                        "UPDATE scheduled_tasks SET run_count = $1, last_run_at = NOW(), enabled = FALSE
                         WHERE id = $2",
                    )
                    .bind(new_count)
                    .bind(sched_id)
                    .execute(&state.pg)
                    .await?;
                } else {
                    sqlx::query(
                        "UPDATE scheduled_tasks SET run_count = $1, last_run_at = NOW(),
                                next_run_at = next_run_at + make_interval(secs => $2::double precision)
                         WHERE id = $3",
                    )
                    .bind(new_count)
                    .bind(*interval_secs as f64)
                    .bind(sched_id)
                    .execute(&state.pg)
                    .await?;
                }
            }
            _ => {
                // One-shot: disable
                sqlx::query(
                    "UPDATE scheduled_tasks SET run_count = $1, last_run_at = NOW(), enabled = FALSE
                     WHERE id = $2",
                )
                .bind(new_count)
                .bind(sched_id)
                .execute(&state.pg)
                .await?;
            }
        }

        info!(task_id = %task_id, scheduled_id = %sched_id, title = %title, "scheduled task promoted");
        promoted += 1;
    }

    Ok(promoted)
}

// ---------------------------------------------------------------------------
// Task timeout / deadlock detection loop
// ---------------------------------------------------------------------------

/// Background loop that detects timed-out tasks and resets them for re-assignment.
async fn task_timeout_loop(state: Arc<AppState>, mut shutdown_rx: watch::Receiver<bool>) {
    let timeout_minutes = state.config.collaboration.task_claim_timeout_minutes;
    let ctx_timeout_minutes = state.config.collaboration.context_request_timeout_minutes;
    info!(
        timeout_minutes,
        ctx_timeout_minutes, "task timeout detection loop started"
    );

    loop {
        if *shutdown_rx.borrow() {
            break;
        }

        match detect_timed_out_tasks(&state, timeout_minutes).await {
            Ok(count) if count > 0 => {
                info!(count, "reassigned timed-out tasks");
            }
            Ok(_) => {}
            Err(e) => warn!(error = %e, "task timeout check failed"),
        }

        // v0.11.0: Check for timed-out context requests
        match detect_timed_out_context_requests(&state, ctx_timeout_minutes).await {
            Ok(count) if count > 0 => {
                info!(count, "reset timed-out context requests to pending");
            }
            Ok(_) => {}
            Err(e) => warn!(error = %e, "context request timeout check failed"),
        }

        tokio::select! {
            _ = tokio::time::sleep(Duration::from_secs(60)) => {}
            _ = shutdown_rx.changed() => break,
        }
    }
    info!("task timeout loop stopped");
}

async fn detect_timed_out_tasks(
    state: &AppState,
    timeout_minutes: u32,
) -> Result<u32, BroodlinkError> {
    let rows: Vec<(String,)> = sqlx::query_as(
        "UPDATE task_queue SET status = 'pending', assigned_agent = NULL, updated_at = NOW()
         WHERE status IN ('claimed', 'in_progress')
         AND claimed_at IS NOT NULL
         AND claimed_at < NOW() - make_interval(mins => $1::int)
         RETURNING id",
    )
    .bind(timeout_minutes as i32)
    .fetch_all(&state.pg)
    .await?;

    let mut count = 0u32;
    for (task_id,) in &rows {
        // Log timeout event
        let _ = sqlx::query(
            "INSERT INTO service_events (service, event_type, severity, details, created_at)
             VALUES ('coordinator', 'task_timeout', 'warning', $1, NOW())",
        )
        .bind(serde_json::json!({"task_id": task_id}))
        .execute(&state.pg)
        .await;

        // Re-publish task_available
        let subject = format!(
            "{}.{}.coordinator.task_available",
            state.config.nats.subject_prefix, state.config.broodlink.env,
        );
        let payload = serde_json::json!({"task_id": task_id});
        if let Ok(bytes) = serde_json::to_vec(&payload) {
            let _ = state.nats.publish(subject, bytes.into()).await;
        }

        info!(task_id = %task_id, "task timed out, reset to pending");
        count += 1;
    }

    Ok(count)
}

/// v0.11.0: Reset tasks stuck in `context_requested` status back to `pending`
/// after the configured timeout, so they can be re-routed.
async fn detect_timed_out_context_requests(
    state: &AppState,
    timeout_minutes: u32,
) -> Result<u32, BroodlinkError> {
    let rows: Vec<(String,)> = sqlx::query_as(
        "UPDATE task_queue
         SET status = 'pending',
             context_questions = NULL,
             context_requested_by = NULL,
             context_requested_at = NULL,
             updated_at = NOW()
         WHERE status = 'context_requested'
         AND context_requested_at IS NOT NULL
         AND context_requested_at < NOW() - make_interval(mins => $1::int)
         RETURNING id",
    )
    .bind(timeout_minutes as i32)
    .fetch_all(&state.pg)
    .await?;

    let mut count = 0u32;
    for (task_id,) in &rows {
        let _ = sqlx::query(
            "INSERT INTO service_events (service, event_type, severity, details, created_at)
             VALUES ('coordinator', 'context_request_timeout', 'warning', $1, NOW())",
        )
        .bind(serde_json::json!({"task_id": task_id}))
        .execute(&state.pg)
        .await;

        // Re-publish task_available
        let subject = format!(
            "{}.{}.coordinator.task_available",
            state.config.nats.subject_prefix, state.config.broodlink.env,
        );
        let payload = serde_json::json!({"task_id": task_id});
        if let Ok(bytes) = serde_json::to_vec(&payload) {
            let _ = state.nats.publish(subject, bytes.into()).await;
        }

        info!(task_id = %task_id, "context request timed out, reset to pending");
        count += 1;
    }

    Ok(count)
}

// ---------------------------------------------------------------------------
// Initialization
// ---------------------------------------------------------------------------

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

    // Dolt (MySQL) pool — read-only for agent_profiles lookup
    let dolt_url = format!(
        "mysql://{}:{}@{}:{}/{}",
        config.dolt.user, dolt_password, config.dolt.host, config.dolt.port, config.dolt.database,
    );
    let dolt = MySqlPoolOptions::new()
        .min_connections(1)
        .max_connections(config.dolt.max_connections)
        .acquire_timeout(Duration::from_secs(5))
        .connect(&dolt_url)
        .await?;
    info!("dolt pool connected (read-only for agent_profiles)");

    // Postgres pool — for task_queue + audit_log
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
        start_time: std::time::Instant::now(),
        tasks_claimed: AtomicU64::new(0),
        tasks_dead_lettered: AtomicU64::new(0),
        claim_retries_total: AtomicU64::new(0),
    })
}

async fn shutdown_signal() {
    broodlink_runtime::shutdown_signal().await;
}

// ---------------------------------------------------------------------------
// Main
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
        "starting coordinator (NATS-only task router)"
    );

    let state = match init_state().await {
        Ok(s) => Arc::new(s),
        Err(e) => {
            error!(error = %e, "fatal: failed to initialise coordinator");
            process::exit(1);
        }
    };

    // Shutdown coordination via watch channel
    let (shutdown_tx, shutdown_rx) = watch::channel(false);

    // Spawn the NATS subscription loop
    let sub_state = Arc::clone(&state);
    let sub_shutdown = shutdown_rx.clone();
    let sub_handle = tokio::spawn(async move {
        if let Err(e) = run_subscription(sub_state, sub_shutdown).await {
            error!(error = %e, "subscription loop failed");
        }
    });

    // Spawn workflow subscription loops
    let wf_start_state = Arc::clone(&state);
    let wf_start_shutdown = shutdown_rx.clone();
    let wf_start_handle = tokio::spawn(async move {
        if let Err(e) = run_workflow_start_subscription(wf_start_state, wf_start_shutdown).await {
            error!(error = %e, "workflow_start subscription failed");
        }
    });

    let task_completed_state = Arc::clone(&state);
    let task_completed_shutdown = shutdown_rx.clone();
    let task_completed_handle = tokio::spawn(async move {
        if let Err(e) =
            run_task_completed_subscription(task_completed_state, task_completed_shutdown).await
        {
            error!(error = %e, "task_completed subscription failed");
        }
    });

    let task_failed_state = Arc::clone(&state);
    let task_failed_shutdown = shutdown_rx.clone();
    let task_failed_handle = tokio::spawn(async move {
        if let Err(e) = run_task_failed_subscription(task_failed_state, task_failed_shutdown).await
        {
            error!(error = %e, "task_failed subscription failed");
        }
    });

    // v0.11.0: Spawn negotiation subscription loops
    let declined_state = Arc::clone(&state);
    let declined_shutdown = shutdown_rx.clone();
    let declined_handle = tokio::spawn(async move {
        if let Err(e) = run_task_declined_subscription(declined_state, declined_shutdown).await {
            error!(error = %e, "task_declined subscription failed");
        }
    });

    let ctx_req_state = Arc::clone(&state);
    let ctx_req_shutdown = shutdown_rx.clone();
    let ctx_req_handle = tokio::spawn(async move {
        if let Err(e) = run_task_context_request_subscription(ctx_req_state, ctx_req_shutdown).await
        {
            error!(error = %e, "task_context_request subscription failed");
        }
    });

    // Spawn the periodic metrics publisher
    let metrics_state = Arc::clone(&state);
    let metrics_shutdown = shutdown_rx.clone();
    let metrics_handle = tokio::spawn(async move {
        metrics_loop(metrics_state, metrics_shutdown).await;
    });

    // Spawn the DLQ auto-retry loop
    let dlq_state = Arc::clone(&state);
    let dlq_shutdown = shutdown_rx.clone();
    let dlq_handle = tokio::spawn(async move {
        dlq_retry_loop(dlq_state, dlq_shutdown).await;
    });

    // Spawn the scheduled task promotion loop
    let sched_state = Arc::clone(&state);
    let sched_shutdown = shutdown_rx.clone();
    let sched_handle = tokio::spawn(async move {
        scheduled_task_loop(sched_state, sched_shutdown).await;
    });

    // Spawn the task timeout / deadlock detection loop
    let timeout_state = Arc::clone(&state);
    let timeout_shutdown = shutdown_rx.clone();
    let timeout_handle = tokio::spawn(async move {
        task_timeout_loop(timeout_state, timeout_shutdown).await;
    });

    // Wait for shutdown signal
    shutdown_signal().await;

    info!("initiating graceful shutdown");

    // Signal all spawned tasks to stop
    let _ = shutdown_tx.send(true);

    // Wait for tasks to complete with a timeout
    let shutdown_timeout = Duration::from_secs(10);
    match tokio::time::timeout(shutdown_timeout, async {
        if let Err(e) = sub_handle.await {
            warn!(error = %e, "subscription task panicked");
        }
        if let Err(e) = wf_start_handle.await {
            warn!(error = %e, "workflow start task panicked");
        }
        if let Err(e) = task_completed_handle.await {
            warn!(error = %e, "task completed handler panicked");
        }
        if let Err(e) = task_failed_handle.await {
            warn!(error = %e, "task failed handler panicked");
        }
        if let Err(e) = metrics_handle.await {
            warn!(error = %e, "metrics task panicked");
        }
        if let Err(e) = dlq_handle.await {
            warn!(error = %e, "DLQ retry task panicked");
        }
        if let Err(e) = sched_handle.await {
            warn!(error = %e, "scheduled task loop panicked");
        }
        if let Err(e) = timeout_handle.await {
            warn!(error = %e, "task timeout loop panicked");
        }
        if let Err(e) = declined_handle.await {
            warn!(error = %e, "task declined handler panicked");
        }
        if let Err(e) = ctx_req_handle.await {
            warn!(error = %e, "task context request handler panicked");
        }
    })
    .await
    {
        Ok(()) => info!("all background tasks stopped"),
        Err(_) => warn!("shutdown timed out after {shutdown_timeout:?}, forcing exit"),
    }

    // Close database pools
    state.pg.close().await;
    state.dolt.close().await;

    // Final metrics publish (best-effort)
    if let Err(e) = publish_metrics(&state).await {
        warn!(error = %e, "final metrics publish failed");
    }

    info!(
        tasks_claimed = state.tasks_claimed.load(Ordering::Relaxed),
        tasks_dead_lettered = state.tasks_dead_lettered.load(Ordering::Relaxed),
        uptime_seconds = state.start_time.elapsed().as_secs(),
        "coordinator shutdown complete"
    );
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    /// Helper that mirrors the MySQL `FIELD(cost_tier, 'low', 'medium', 'high')`
    /// ordering used in `find_eligible_agents`.  Returns a sort key for a cost tier
    /// string, where lower values are preferred (cheaper agents first).
    fn cost_tier_sort_key(tier: &str) -> u32 {
        match tier {
            "low" => 0,
            "medium" => 1,
            "high" => 2,
            _ => u32::MAX,
        }
    }

    fn make_agent(id: &str, tier: &str) -> EligibleAgent {
        EligibleAgent {
            agent_id: id.to_string(),
            cost_tier: tier.to_string(),
            capabilities: None,
            max_concurrent: 5,
            last_seen: None,
        }
    }

    fn default_weights() -> broodlink_config::RoutingWeights {
        broodlink_config::RoutingWeights {
            capability: 0.35,
            success_rate: 0.25,
            availability: 0.20,
            cost: 0.15,
            recency: 0.05,
            domain: 0.0,
        }
    }

    #[test]
    fn test_cost_tier_ordering() {
        let mut agents = [
            make_agent("agent-high", "high"),
            make_agent("agent-low", "low"),
            make_agent("agent-medium", "medium"),
        ];

        agents.sort_by_key(|a| cost_tier_sort_key(&a.cost_tier));

        assert_eq!(agents[0].cost_tier, "low", "low should sort first");
        assert_eq!(agents[1].cost_tier, "medium", "medium should sort second");
        assert_eq!(agents[2].cost_tier, "high", "high should sort last");
    }

    #[test]
    fn test_backoff_calculation() {
        // The coordinator uses exponential backoff: BACKOFF_BASE_MS * 2^attempt
        // Attempt 0 => 1000ms, Attempt 1 => 2000ms, Attempt 2 => 4000ms, etc.
        for attempt in 0..MAX_CLAIM_RETRIES {
            let backoff_ms = BACKOFF_BASE_MS * 2u64.pow(attempt);
            let expected = match attempt {
                0 => 1000,
                1 => 2000,
                2 => 4000,
                3 => 8000,
                4 => 16000,
                _ => unreachable!(),
            };
            assert_eq!(
                backoff_ms, expected,
                "backoff for attempt {attempt} should be {expected}ms"
            );
        }
    }

    #[test]
    fn test_task_available_payload_deserialization() {
        let json_str = r#"{"task_id":"task-1","role":"worker","cost_tier":"low"}"#;
        let payload: TaskAvailablePayload = serde_json::from_str(json_str).unwrap();

        assert_eq!(payload.task_id, "task-1");
        assert_eq!(payload.role.as_deref(), Some("worker"));
        assert_eq!(payload.cost_tier.as_deref(), Some("low"));
        assert!(payload.task_type.is_none());
    }

    #[test]
    fn test_task_available_payload_minimal() {
        // Only task_id is required; role, cost_tier, task_type default to None
        let json_str = r#"{"task_id":"task-2"}"#;
        let payload: TaskAvailablePayload = serde_json::from_str(json_str).unwrap();

        assert_eq!(payload.task_id, "task-2");
        assert!(payload.role.is_none());
        assert!(payload.cost_tier.is_none());
        assert!(payload.task_type.is_none());
    }

    #[test]
    fn test_task_available_payload_ignores_unknown_fields() {
        let json_str = r#"{"task_id":"task-3","unknown_field":"ignored","role":"worker"}"#;
        let result: Result<TaskAvailablePayload, _> = serde_json::from_str(json_str);
        // serde default behavior: unknown fields are ignored (no deny_unknown_fields)
        assert!(result.is_ok(), "unknown fields should be silently ignored");
        let payload = result.unwrap();
        assert_eq!(payload.task_id, "task-3");
        assert_eq!(payload.role.as_deref(), Some("worker"));
    }

    #[test]
    fn test_task_dispatch_payload_serialization() {
        let payload = TaskDispatchPayload {
            task_id: "task-42".to_string(),
            title: "Test task".to_string(),
            description: None,
            priority: 5,
            formula_name: None,
            convoy_id: None,
            assigned_by: "coordinator".to_string(),
            model_hint: None,
        };
        let json = serde_json::to_value(&payload).unwrap();
        assert_eq!(json["task_id"], "task-42");
        assert_eq!(json["title"], "Test task");
        assert_eq!(json["priority"], 5);
        assert_eq!(json["assigned_by"], "coordinator");
        assert!(
            json["description"].is_null(),
            "None fields should serialize as null"
        );
    }

    // ----- Scoring algorithm tests -----

    #[test]
    fn test_cost_tier_score_values() {
        assert!((cost_tier_score("low") - 1.0).abs() < f64::EPSILON);
        assert!((cost_tier_score("medium") - 0.6).abs() < f64::EPSILON);
        assert!((cost_tier_score("high") - 0.3).abs() < f64::EPSILON);
        assert!((cost_tier_score("unknown") - 0.3).abs() < f64::EPSILON);
    }

    #[test]
    fn test_recency_score_none_returns_zero() {
        assert!((recency_score(None) - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_recency_score_recent_returns_one() {
        let now = chrono::Utc::now().naive_utc();
        let ts = now.format("%Y-%m-%d %H:%M:%S").to_string();
        let score = recency_score(Some(&ts));
        assert!(
            score > 0.95,
            "just-now timestamp should score ~1.0, got {score}"
        );
    }

    #[test]
    fn test_recency_score_old_returns_zero() {
        let old = chrono::Utc::now().naive_utc() - chrono::Duration::hours(1);
        let ts = old.format("%Y-%m-%d %H:%M:%S").to_string();
        let score = recency_score(Some(&ts));
        assert!(
            score < f64::EPSILON,
            "1-hour-old timestamp should score 0.0, got {score}"
        );
    }

    #[test]
    fn test_recency_score_midpoint() {
        // ~15 minutes ago should give ~0.5
        let mid = chrono::Utc::now().naive_utc() - chrono::Duration::minutes(15);
        let ts = mid.format("%Y-%m-%d %H:%M:%S").to_string();
        let score = recency_score(Some(&ts));
        assert!(
            score > 0.2 && score < 0.8,
            "15-min-old should be mid-range, got {score}"
        );
    }

    #[test]
    fn test_recency_score_invalid_format() {
        assert!((recency_score(Some("not-a-date")) - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_compute_score_no_metrics_uses_bonus() {
        let agent = make_agent("a1", "low");
        let weights = default_weights();
        let (score, breakdown) = compute_score(&agent, None, None, &weights, 0.8, None, &[]);

        // success_rate should use new_agent_bonus = 0.8
        assert!((breakdown.success_rate - 0.8).abs() < f64::EPSILON);
        // availability should be 1.0 (no metrics = assume available)
        assert!((breakdown.availability - 1.0).abs() < f64::EPSILON);
        // cost should be 1.0 (low tier)
        assert!((breakdown.cost - 1.0).abs() < f64::EPSILON);
        assert!(score > 0.0, "score should be positive");
    }

    #[test]
    fn test_compute_score_with_metrics() {
        let agent = make_agent("a1", "medium");
        let metrics = AgentMetrics {
            success_rate: 0.9,
            current_load: 2,
        };
        let weights = default_weights();
        let (score, breakdown) =
            compute_score(&agent, Some(&metrics), None, &weights, 1.0, None, &[]);

        assert!((breakdown.success_rate - 0.9).abs() < f64::EPSILON);
        // availability: 1.0 - (2/5) = 0.6
        assert!((breakdown.availability - 0.6).abs() < f64::EPSILON);
        assert!((breakdown.cost - 0.6).abs() < f64::EPSILON);
        assert!(score > 0.0);
    }

    #[test]
    fn test_compute_score_overloaded_agent_low_availability() {
        let mut agent = make_agent("a1", "low");
        agent.max_concurrent = 3;
        let metrics = AgentMetrics {
            success_rate: 0.95,
            current_load: 3,
        };
        let weights = default_weights();
        let (_, breakdown) = compute_score(&agent, Some(&metrics), None, &weights, 1.0, None, &[]);

        // availability: 1.0 - (3/3) = 0.0
        assert!(
            breakdown.availability < f64::EPSILON,
            "fully loaded agent should have 0 availability"
        );
    }

    #[test]
    fn test_rank_agents_sorts_descending() {
        let agents = vec![
            make_agent("low-success", "high"),
            make_agent("high-success", "low"),
        ];
        let mut metrics = HashMap::new();
        metrics.insert(
            "low-success".to_string(),
            AgentMetrics {
                success_rate: 0.3,
                current_load: 0,
            },
        );
        metrics.insert(
            "high-success".to_string(),
            AgentMetrics {
                success_rate: 0.95,
                current_load: 0,
            },
        );

        let weights = default_weights();
        let ranked = rank_agents(agents, &metrics, None, &weights, 1.0, None, &HashMap::new());

        assert_eq!(ranked.len(), 2);
        assert_eq!(
            ranked[0].agent.agent_id, "high-success",
            "higher score first"
        );
        assert!(
            ranked[0].score >= ranked[1].score,
            "should be sorted descending"
        );
    }

    #[test]
    fn test_rank_agents_capability_bonus() {
        let mut agent_with_cap = make_agent("a-cap", "low");
        agent_with_cap.capabilities = Some(serde_json::json!({"review_code": true}));
        let agent_no_cap = make_agent("a-nocap", "low");

        let agents = vec![agent_no_cap, agent_with_cap];
        let metrics = HashMap::new(); // no metrics = new_agent_bonus

        let weights = default_weights();
        let ranked = rank_agents(
            agents,
            &metrics,
            Some("review_code"),
            &weights,
            1.0,
            None,
            &HashMap::new(),
        );

        // Agent with matching capability should rank higher
        assert_eq!(ranked[0].agent.agent_id, "a-cap");
        assert!((ranked[0].breakdown.capability - 1.0).abs() < f64::EPSILON);
        // Agent without capability gets 0.5 when formula specified
        assert!((ranked[1].breakdown.capability - 0.5).abs() < f64::EPSILON);
    }

    #[test]
    fn test_routing_decision_payload_serialization() {
        let payload = RoutingDecisionPayload {
            task_id: "task-1".to_string(),
            agent_id: "agent-1".to_string(),
            score: 0.85,
            breakdown: ScoreBreakdown {
                capability: 1.0,
                success_rate: 0.9,
                availability: 0.8,
                cost: 0.6,
                recency: 0.5,
                domain: 1.0,
            },
            candidates_evaluated: 3,
            attempt: 1,
        };
        let json = serde_json::to_value(&payload).unwrap();
        assert_eq!(json["task_id"], "task-1");
        assert_eq!(json["agent_id"], "agent-1");
        assert_eq!(json["candidates_evaluated"], 3);
        assert!(json["breakdown"]["capability"].is_number());
    }

    // ----- v0.9.0: Domain routing tests -----

    #[test]
    fn test_compute_score_domain_match() {
        let agent = make_agent("a1", "low");
        let weights = default_weights();
        // Override domain weight for testing
        let mut w = weights;
        w.domain = 0.15;
        let agent_domains = vec!["code".to_string(), "general".to_string()];
        let (_, bd) = compute_score(&agent, None, None, &w, 1.0, Some("code"), &agent_domains);
        assert!(
            (bd.domain - 1.0).abs() < f64::EPSILON,
            "matching domain should score 1.0"
        );
    }

    #[test]
    fn test_compute_score_domain_no_match() {
        let agent = make_agent("a1", "low");
        let mut w = default_weights();
        w.domain = 0.15;
        let (_, bd) = compute_score(&agent, None, None, &w, 1.0, Some("code"), &[]);
        assert!(
            bd.domain < 0.5,
            "no declared domains + code task should score low"
        );
    }

    #[test]
    fn test_compute_score_no_domain_constraint() {
        let agent = make_agent("a1", "low");
        let w = default_weights();
        let (_, bd) = compute_score(&agent, None, None, &w, 1.0, None, &[]);
        assert!(
            (bd.domain - 1.0).abs() < f64::EPSILON,
            "no domain constraint = 1.0 for all"
        );
    }

    #[test]
    fn test_classify_task_domain_code() {
        assert_eq!(
            classify_task_domain("implement a struct with unit tests"),
            "code"
        );
    }

    #[test]
    fn test_classify_task_domain_general() {
        assert_eq!(
            classify_task_domain("summarize the quarterly report"),
            "general"
        );
    }

    // ----- Workflow orchestration tests -----

    #[test]
    fn test_render_prompt_replaces_placeholders() {
        let template = "Research {{topic}} and report on {{format}}";
        let params = serde_json::json!({"topic": "quantum computing", "format": "PDF"});
        let result = render_prompt(template, &params);
        assert_eq!(result, "Research quantum computing and report on PDF");
    }

    #[test]
    fn test_render_prompt_no_params() {
        let template = "Do something with no placeholders";
        let params = serde_json::json!({});
        let result = render_prompt(template, &params);
        assert_eq!(result, "Do something with no placeholders");
    }

    #[test]
    fn test_render_prompt_missing_key() {
        let template = "Research {{topic}} with {{missing}}";
        let params = serde_json::json!({"topic": "AI"});
        let result = render_prompt(template, &params);
        assert_eq!(result, "Research AI with {{missing}}");
    }

    #[test]
    fn test_render_prompt_numeric_value() {
        let template = "Process {{count}} items";
        let params = serde_json::json!({"count": 42});
        let result = render_prompt(template, &params);
        assert_eq!(result, "Process 42 items");
    }

    #[test]
    fn test_workflow_start_payload_deserialization() {
        let json_str = r#"{"workflow_id":"wf-1","convoy_id":"c-1","formula_name":"research","params":{"topic":"AI"},"started_by":"claude"}"#;
        let payload: WorkflowStartPayload = serde_json::from_str(json_str).unwrap();
        assert_eq!(payload.workflow_id, "wf-1");
        assert_eq!(payload.formula_name, "research");
        assert_eq!(payload.params["topic"], "AI");
        assert_eq!(payload.started_by, "claude");
    }

    #[test]
    fn test_task_completed_payload_deserialization() {
        let json_str =
            r#"{"task_id":"t-1","agent_id":"claude","result_data":{"sources":["a","b"]}}"#;
        let payload: TaskCompletedPayload = serde_json::from_str(json_str).unwrap();
        assert_eq!(payload.task_id, "t-1");
        assert_eq!(payload.agent_id, "claude");
        assert!(payload.result_data.is_some());
    }

    #[test]
    fn test_task_completed_payload_no_result() {
        let json_str = r#"{"task_id":"t-2","agent_id":"qwen3"}"#;
        let payload: TaskCompletedPayload = serde_json::from_str(json_str).unwrap();
        assert_eq!(payload.task_id, "t-2");
        assert!(payload.result_data.is_none());
    }

    #[test]
    fn test_task_failed_payload_deserialization() {
        let json_str = r#"{"task_id":"t-3","agent_id":"claude"}"#;
        let payload: TaskFailedPayload = serde_json::from_str(json_str).unwrap();
        assert_eq!(payload.task_id, "t-3");
        assert_eq!(payload.agent_id, "claude");
    }

    #[test]
    fn test_load_formula_research() {
        // Parse the formula TOML directly from the workspace
        let content =
            std::fs::read_to_string("../../.beads/formulas/research.formula.toml").unwrap();
        let formula: FormulaFile = toml::from_str(&content).unwrap();
        assert_eq!(formula.formula.name, "research");
        assert_eq!(formula.steps.len(), 3);
        assert_eq!(formula.steps[0].name, "gather_sources");
        assert_eq!(formula.steps[0].agent_role, "researcher");
        assert_eq!(formula.steps[0].output, "sources");
        assert!(formula.steps[0].input.is_none());
        assert!(formula.steps[1].input.is_some());
    }

    #[test]
    fn test_load_formula_build_feature() {
        let content =
            std::fs::read_to_string("../../.beads/formulas/build-feature.formula.toml").unwrap();
        let formula: FormulaFile = toml::from_str(&content).unwrap();
        assert_eq!(formula.formula.name, "build-feature");
        assert_eq!(formula.steps.len(), 4);
        assert_eq!(formula.steps[0].name, "plan");
        assert_eq!(formula.steps[3].name, "document");
    }

    #[test]
    fn test_load_formula_not_found_file() {
        let result = std::fs::read_to_string("../../.beads/formulas/nonexistent.formula.toml");
        assert!(result.is_err(), "nonexistent formula file should not exist");
    }

    #[test]
    fn test_formula_input_single_deserialization() {
        let toml_str = r#"
[formula]
name = "test"

[[steps]]
name = "s1"
agent_role = "worker"
prompt = "do it"
input = "previous"
output = "result"
"#;
        let formula: FormulaFile = toml::from_str(toml_str).unwrap();
        match &formula.steps[0].input {
            Some(FormulaInput::Single(s)) => assert_eq!(s, "previous"),
            other => panic!("expected Single input, got {other:?}"),
        }
    }

    #[test]
    fn test_formula_input_multiple_deserialization() {
        let toml_str = r#"
[formula]
name = "test"

[[steps]]
name = "s1"
agent_role = "worker"
prompt = "do it"
input = ["a", "b"]
output = "result"
"#;
        let formula: FormulaFile = toml::from_str(toml_str).unwrap();
        match &formula.steps[0].input {
            Some(FormulaInput::Multiple(v)) => assert_eq!(v, &["a", "b"]),
            other => panic!("expected Multiple input, got {other:?}"),
        }
    }

    // -----------------------------------------------------------------------
    // v0.7.0: JSONB definition → FormulaFile deserialization
    // -----------------------------------------------------------------------

    #[test]
    fn test_formula_jsonb_deserialization() {
        // This is the JSONB format stored in formula_registry.definition
        let definition = serde_json::json!({
            "formula": {
                "name": "test-jsonb",
                "description": "A test formula from JSONB",
                "version": "2"
            },
            "parameters": [
                {"name": "topic", "type": "string", "required": true}
            ],
            "steps": [
                {
                    "name": "step_1",
                    "agent_role": "researcher",
                    "tools": ["semantic_search"],
                    "prompt": "Research {{topic}}",
                    "output": "result",
                    "when": null,
                    "retries": 0,
                    "timeout_seconds": null,
                    "group": null
                }
            ],
            "on_failure": null
        });

        let formula: FormulaFile = serde_json::from_value(definition).unwrap();
        assert_eq!(formula.formula.name, "test-jsonb");
        assert_eq!(
            formula.formula.description.as_deref(),
            Some("A test formula from JSONB")
        );
        assert_eq!(formula.steps.len(), 1);
        assert_eq!(formula.steps[0].name, "step_1");
        assert_eq!(formula.steps[0].agent_role, "researcher");
        assert_eq!(formula.steps[0].prompt, "Research {{topic}}");
        assert_eq!(formula.steps[0].output, "result");
        assert!(formula.on_failure.is_none());
    }

    #[test]
    fn test_formula_jsonb_multi_step_deserialization() {
        let definition = serde_json::json!({
            "formula": {"name": "multi", "description": "Multi-step"},
            "steps": [
                {"name": "a", "agent_role": "worker", "prompt": "do A", "output": "ra"},
                {"name": "b", "agent_role": "worker", "prompt": "do B", "input": "ra", "output": "rb"},
                {"name": "c", "agent_role": "writer", "prompt": "do C", "input": ["ra", "rb"], "output": "rc"}
            ]
        });

        let formula: FormulaFile = serde_json::from_value(definition).unwrap();
        assert_eq!(formula.steps.len(), 3);
        assert!(formula.steps[0].input.is_none());
        match &formula.steps[1].input {
            Some(FormulaInput::Single(s)) => assert_eq!(s, "ra"),
            other => panic!("expected Single input for step b, got {other:?}"),
        }
        match &formula.steps[2].input {
            Some(FormulaInput::Multiple(v)) => assert_eq!(v, &["ra", "rb"]),
            other => panic!("expected Multiple input for step c, got {other:?}"),
        }
    }

    #[test]
    fn test_formula_toml_fallback_still_works() {
        // Verify the TOML format is still parsed correctly (backward compat)
        let toml_str = r#"
[formula]
name = "toml-test"
version = "1.0.0"

[[steps]]
name = "s1"
agent_role = "worker"
prompt = "test"
output = "out"
"#;
        let formula: FormulaFile = toml::from_str(toml_str).unwrap();
        assert_eq!(formula.formula.name, "toml-test");
        assert_eq!(formula.steps.len(), 1);
    }

    #[test]
    fn test_formula_registry_loading_logic() {
        // Simulate the loading priority: registry → TOML
        // If registry returns a valid FormulaFile, that should be used
        let registry_def = serde_json::json!({
            "formula": {"name": "from-registry"},
            "steps": [{"name": "s1", "agent_role": "w", "prompt": "p", "output": "o"}]
        });
        let formula: FormulaFile = serde_json::from_value(registry_def).unwrap();
        assert_eq!(formula.formula.name, "from-registry");
        assert!(!formula.steps.is_empty());
    }

    // --- v0.11.0: Negotiation protocol tests ---

    #[test]
    fn test_task_declined_payload_deserialization() {
        let json = serde_json::json!({
            "task_id": "t-1",
            "agent_id": "agent-a",
            "reason": "I lack capability",
            "suggested_agent": "agent-b"
        });
        let payload: TaskDeclinedPayload = serde_json::from_value(json).unwrap();
        assert_eq!(payload.task_id, "t-1");
        assert_eq!(payload.agent_id, "agent-a");
        assert_eq!(payload.reason.as_deref(), Some("I lack capability"));
        assert_eq!(payload.suggested_agent.as_deref(), Some("agent-b"));
    }

    #[test]
    fn test_task_declined_payload_minimal() {
        let json = serde_json::json!({
            "task_id": "t-2",
            "agent_id": "agent-x"
        });
        let payload: TaskDeclinedPayload = serde_json::from_value(json).unwrap();
        assert_eq!(payload.task_id, "t-2");
        assert!(payload.reason.is_none());
        assert!(payload.suggested_agent.is_none());
    }

    #[test]
    fn test_task_context_request_payload_deserialization() {
        let json = serde_json::json!({
            "task_id": "t-3",
            "agent_id": "agent-c",
            "questions": ["What format?", "Which database?"]
        });
        let payload: TaskContextRequestPayload = serde_json::from_value(json).unwrap();
        assert_eq!(payload.task_id, "t-3");
        assert_eq!(payload.agent_id, "agent-c");
        assert_eq!(payload.questions.len(), 2);
        assert_eq!(payload.questions[0], "What format?");
    }

    #[test]
    fn test_declined_agents_filter_excludes_declined() {
        let agents = vec![
            make_agent("agent-a", "low"),
            make_agent("agent-b", "low"),
            make_agent("agent-c", "medium"),
        ];
        let declined = vec!["agent-a".to_string(), "agent-c".to_string()];
        let filtered: Vec<_> = agents
            .into_iter()
            .filter(|a| !declined.contains(&a.agent_id))
            .collect();
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].agent_id, "agent-b");
    }

    #[test]
    fn test_declined_agents_empty_filter_keeps_all() {
        let agents = vec![
            make_agent("agent-a", "low"),
            make_agent("agent-b", "medium"),
        ];
        let declined: Vec<String> = vec![];
        let filtered: Vec<_> = agents
            .into_iter()
            .filter(|a| !declined.contains(&a.agent_id))
            .collect();
        assert_eq!(filtered.len(), 2);
    }
}
