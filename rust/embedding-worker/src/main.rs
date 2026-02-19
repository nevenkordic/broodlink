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
use std::process;
use std::sync::Arc;
use std::time::Duration;

use broodlink_config::Config;
use broodlink_runtime::CircuitBreaker;
use broodlink_secrets::SecretsProvider;
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

const SERVICE_NAME: &str = "embedding-worker";
const SERVICE_VERSION: &str = env!("CARGO_PKG_VERSION");

const POLL_INTERVAL: Duration = Duration::from_secs(2);
const BATCH_SIZE: i64 = 10;

const CIRCUIT_FAILURE_THRESHOLD: u32 = 5;
const CIRCUIT_HALF_OPEN_SECS: u64 = 30;

const MAX_ATTEMPTS: i32 = 3;

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
    #[error("ollama error: {0}")]
    Ollama(String),
    #[error("qdrant error: {0}")]
    Qdrant(String),
    #[error("circuit open: {0}")]
    CircuitOpen(String),
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
// Ollama / Qdrant request/response types
// ---------------------------------------------------------------------------

#[derive(Serialize)]
struct OllamaEmbedRequest {
    model: String,
    prompt: String,
}

#[derive(Deserialize)]
struct OllamaEmbedResponse {
    embedding: Vec<f32>,
}

#[derive(Serialize)]
struct QdrantUpsertRequest {
    points: Vec<QdrantPoint>,
}

#[derive(Serialize)]
struct QdrantPoint {
    id: String,
    vector: HashMap<String, Vec<f32>>,
    payload: QdrantPayload,
}

#[derive(Serialize)]
struct QdrantPayload {
    topic: String,
    agent_id: String,
    memory_id: String,
    created_at: String,
}

// ---------------------------------------------------------------------------
// Outbox row
// ---------------------------------------------------------------------------

struct OutboxRow {
    id: i64,
    trace_id: Option<String>,
    payload: serde_json::Value,
    attempts: i32,
}

// ---------------------------------------------------------------------------
// Application state
// ---------------------------------------------------------------------------

pub struct AppState {
    pub dolt: MySqlPool,
    pub pg: PgPool,
    pub nats: async_nats::Client,
    pub config: Arc<Config>,
    pub http_client: reqwest::Client,
    pub ollama_breaker: CircuitBreaker,
    pub qdrant_breaker: CircuitBreaker,
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

    let state = Arc::new(state);

    // Graceful shutdown channel
    let (shutdown_tx, shutdown_rx) = watch::channel(false);

    // Spawn the poll loop
    let poll_handle = {
        let state = Arc::clone(&state);
        let rx = shutdown_rx.clone();
        tokio::spawn(async move {
            poll_loop(state, rx).await;
        })
    };

    // Wait for shutdown signal
    shutdown_signal().await;

    info!("shutdown signal received, stopping poll loop");
    let _ = shutdown_tx.send(true);

    // Give the poll loop time to finish current batch
    let timeout = tokio::time::timeout(Duration::from_secs(15), poll_handle).await;
    match timeout {
        Ok(Ok(())) => info!("poll loop stopped cleanly"),
        Ok(Err(e)) => warn!(error = %e, "poll loop task panicked"),
        Err(_) => warn!("poll loop did not stop within 15s, forcing shutdown"),
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
            None,
        )?;
        Arc::from(provider)
    };

    // Resolve database passwords
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

    // HTTP client for Ollama and Qdrant (rustls, no default features)
    let http_client = reqwest::Client::builder()
        .timeout(Duration::from_secs(config.ollama.timeout_seconds))
        .pool_max_idle_per_host(4)
        .build()
        .map_err(|e| BroodlinkError::Internal(format!("failed to create HTTP client: {e}")))?;

    // Circuit breakers
    let ollama_breaker = CircuitBreaker::new(
        "ollama",
        CIRCUIT_FAILURE_THRESHOLD,
        CIRCUIT_HALF_OPEN_SECS,
    );
    let qdrant_breaker = CircuitBreaker::new(
        "qdrant",
        CIRCUIT_FAILURE_THRESHOLD,
        CIRCUIT_HALF_OPEN_SECS,
    );

    Ok(AppState {
        dolt,
        pg,
        nats,
        config,
        http_client,
        ollama_breaker,
        qdrant_breaker,
    })
}

async fn shutdown_signal() {
    broodlink_runtime::shutdown_signal().await;
}

// ---------------------------------------------------------------------------
// Poll loop: outbox -> embed -> qdrant -> update
// ---------------------------------------------------------------------------

async fn poll_loop(state: Arc<AppState>, mut shutdown: watch::Receiver<bool>) {
    info!(
        interval_secs = POLL_INTERVAL.as_secs(),
        batch_size = BATCH_SIZE,
        "outbox poll loop started"
    );

    loop {
        // Check for shutdown before each cycle
        if *shutdown.borrow() {
            info!("poll loop received shutdown");
            break;
        }

        match fetch_pending_batch(&state).await {
            Ok(rows) => {
                if rows.is_empty() {
                    // Nothing to process, wait and retry
                } else {
                    info!(count = rows.len(), "processing outbox batch");
                    for row in rows {
                        if let Err(e) = process_outbox_row(&state, &row).await {
                            warn!(
                                outbox_id = %row.id,
                                error = %e,
                                "failed to process outbox row"
                            );
                        }
                    }
                }
            }
            Err(e) => {
                error!(error = %e, "failed to fetch outbox batch");
            }
        }

        // Wait for the poll interval, but break early on shutdown
        tokio::select! {
            () = tokio::time::sleep(POLL_INTERVAL) => {}
            () = async { let _ = shutdown.changed().await; } => {
                info!("poll loop received shutdown during sleep");
                break;
            }
        }
    }

    info!("poll loop exited");
}

// ---------------------------------------------------------------------------
// Fetch pending outbox rows (Postgres) with FOR UPDATE SKIP LOCKED
// ---------------------------------------------------------------------------

async fn fetch_pending_batch(state: &AppState) -> Result<Vec<OutboxRow>, BroodlinkError> {
    let rows = sqlx::query(
        "SELECT id, trace_id, payload, attempts
         FROM outbox
         WHERE status = 'pending'
         ORDER BY created_at ASC
         LIMIT $1
         FOR UPDATE SKIP LOCKED",
    )
    .bind(BATCH_SIZE)
    .fetch_all(&state.pg)
    .await?;

    let mut result = Vec::with_capacity(rows.len());
    for row in rows {
        let id: i64 = row.try_get("id")?;
        let trace_id: Option<String> = row.try_get("trace_id").ok();
        let payload: serde_json::Value = row.try_get("payload")?;
        let attempts: i32 = row.try_get::<i32, _>("attempts").unwrap_or(0);

        result.push(OutboxRow {
            id,
            trace_id,
            payload,
            attempts,
        });
    }

    Ok(result)
}

// ---------------------------------------------------------------------------
// Process a single outbox row end-to-end
// ---------------------------------------------------------------------------

async fn process_outbox_row(state: &AppState, row: &OutboxRow) -> Result<(), BroodlinkError> {
    let trace_id = row
        .trace_id
        .as_deref()
        .unwrap_or("no-trace");

    info!(
        outbox_id = %row.id,
        trace_id = %trace_id,
        "processing outbox row"
    );

    // Mark as processing
    mark_processing(&state.pg, row.id).await?;

    // Extract payload fields
    let content = row
        .payload
        .get("content")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");

    let topic = row
        .payload
        .get("topic")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");

    let agent_name = row
        .payload
        .get("agent_name")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");

    let memory_id = row
        .payload
        .get("memory_id")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");

    if content.is_empty() {
        warn!(outbox_id = %row.id, "empty content, marking as done");
        mark_done(&state.pg, row.id).await?;
        return Ok(());
    }

    // Step 1: Get embedding from Ollama (with circuit breaker)
    let embedding = match get_embedding(state, content).await {
        Ok(vec) => vec,
        Err(e) => {
            return handle_failure(state, row, &e.to_string()).await;
        }
    };

    // Step 2: Upsert into Qdrant (with circuit breaker)
    let qdrant_id = Uuid::new_v4().to_string();
    let now_str = chrono::Utc::now().to_rfc3339();

    if let Err(e) = upsert_qdrant(
        state,
        &qdrant_id,
        &embedding,
        topic,
        agent_name,
        memory_id,
        &now_str,
    )
    .await
    {
        return handle_failure(state, row, &e.to_string()).await;
    }

    // Step 3: Update agent_memory in Dolt with embedding reference
    if !memory_id.is_empty() {
        if let Err(e) = update_memory_ref(&state.dolt, memory_id, &qdrant_id).await {
            warn!(
                outbox_id = %row.id,
                memory_id = %memory_id,
                error = %e,
                "failed to update embedding_ref in dolt (non-fatal)"
            );
        }
    }

    // Step 4: Mark outbox row as done
    mark_done(&state.pg, row.id).await?;

    info!(
        outbox_id = %row.id,
        qdrant_id = %qdrant_id,
        memory_id = %memory_id,
        "embedding pipeline complete"
    );

    Ok(())
}

// ---------------------------------------------------------------------------
// Ollama embedding
// ---------------------------------------------------------------------------

async fn get_embedding(state: &AppState, content: &str) -> Result<Vec<f32>, BroodlinkError> {
    state.ollama_breaker.check().map_err(BroodlinkError::CircuitOpen)?;

    let url = format!("{}/api/embeddings", state.config.ollama.url);
    let body = OllamaEmbedRequest {
        model: state.config.ollama.embedding_model.clone(),
        prompt: content.to_string(),
    };

    let resp = state
        .http_client
        .post(&url)
        .json(&body)
        .timeout(Duration::from_secs(state.config.ollama.timeout_seconds))
        .send()
        .await
        .map_err(|e| {
            state.ollama_breaker.record_failure();
            BroodlinkError::Ollama(format!("request failed: {e}"))
        })?;

    if !resp.status().is_success() {
        state.ollama_breaker.record_failure();
        let status = resp.status();
        let body_text = resp
            .text()
            .await
            .unwrap_or_else(|_| String::from("<unreadable>"));
        return Err(BroodlinkError::Ollama(format!(
            "returned status {status}: {body_text}"
        )));
    }

    let embed_resp: OllamaEmbedResponse = resp.json().await.map_err(|e| {
        state.ollama_breaker.record_failure();
        BroodlinkError::Ollama(format!("failed to parse response: {e}"))
    })?;

    state.ollama_breaker.record_success();
    Ok(embed_resp.embedding)
}

// ---------------------------------------------------------------------------
// Qdrant upsert
// ---------------------------------------------------------------------------

async fn upsert_qdrant(
    state: &AppState,
    qdrant_id: &str,
    embedding: &[f32],
    topic: &str,
    agent_id: &str,
    memory_id: &str,
    created_at: &str,
) -> Result<(), BroodlinkError> {
    state.qdrant_breaker.check().map_err(BroodlinkError::CircuitOpen)?;

    let url = format!(
        "{}/collections/{}/points",
        state.config.qdrant.url, state.config.qdrant.collection,
    );

    let mut vector_map = HashMap::new();
    vector_map.insert("default".to_string(), embedding.to_vec());

    let body = QdrantUpsertRequest {
        points: vec![QdrantPoint {
            id: qdrant_id.to_string(),
            vector: vector_map,
            payload: QdrantPayload {
                topic: topic.to_string(),
                agent_id: agent_id.to_string(),
                memory_id: memory_id.to_string(),
                created_at: created_at.to_string(),
            },
        }],
    };

    let resp = state
        .http_client
        .put(&url)
        .json(&body)
        .timeout(Duration::from_secs(10))
        .send()
        .await
        .map_err(|e| {
            state.qdrant_breaker.record_failure();
            BroodlinkError::Qdrant(format!("request failed: {e}"))
        })?;

    if !resp.status().is_success() {
        state.qdrant_breaker.record_failure();
        let status = resp.status();
        let body_text = resp
            .text()
            .await
            .unwrap_or_else(|_| String::from("<unreadable>"));
        return Err(BroodlinkError::Qdrant(format!(
            "upsert returned status {status}: {body_text}"
        )));
    }

    state.qdrant_breaker.record_success();
    Ok(())
}

// ---------------------------------------------------------------------------
// Dolt: update agent_memory.embedding_ref
// ---------------------------------------------------------------------------

async fn update_memory_ref(
    dolt: &MySqlPool,
    memory_id: &str,
    qdrant_id: &str,
) -> Result<(), BroodlinkError> {
    sqlx::query("UPDATE agent_memory SET embedding_ref = ? WHERE id = ?")
        .bind(qdrant_id)
        .bind(memory_id)
        .execute(dolt)
        .await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Outbox status transitions (Postgres)
// ---------------------------------------------------------------------------

async fn mark_processing(pg: &PgPool, outbox_id: i64) -> Result<(), BroodlinkError> {
    sqlx::query("UPDATE outbox SET status = 'processing' WHERE id = $1")
        .bind(outbox_id)
        .execute(pg)
        .await?;
    Ok(())
}

async fn mark_done(pg: &PgPool, outbox_id: i64) -> Result<(), BroodlinkError> {
    sqlx::query(
        "UPDATE outbox SET status = 'done', processed_at = NOW() WHERE id = $1",
    )
    .bind(outbox_id)
    .execute(pg)
    .await?;
    Ok(())
}

async fn increment_attempts(pg: &PgPool, outbox_id: i64) -> Result<(), BroodlinkError> {
    sqlx::query(
        "UPDATE outbox SET status = 'pending', attempts = attempts + 1 WHERE id = $1",
    )
    .bind(outbox_id)
    .execute(pg)
    .await?;
    Ok(())
}

async fn mark_failed(pg: &PgPool, outbox_id: i64) -> Result<(), BroodlinkError> {
    sqlx::query(
        "UPDATE outbox SET status = 'failed', processed_at = NOW() WHERE id = $1",
    )
    .bind(outbox_id)
    .execute(pg)
    .await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Failure handling: retry or dead-letter
// ---------------------------------------------------------------------------

async fn handle_failure(
    state: &AppState,
    row: &OutboxRow,
    error_msg: &str,
) -> Result<(), BroodlinkError> {
    let new_attempts = row.attempts + 1;

    if new_attempts >= MAX_ATTEMPTS {
        warn!(
            outbox_id = %row.id,
            attempts = new_attempts,
            error = %error_msg,
            "max attempts reached, marking as failed and publishing dead letter"
        );

        mark_failed(&state.pg, row.id).await?;

        // Publish dead letter to NATS
        let subject = format!(
            "{}.{}.embed.dead_letter",
            state.config.nats.subject_prefix, state.config.broodlink.env,
        );
        let dead_letter = serde_json::json!({
            "outbox_id": row.id,
            "trace_id": row.trace_id,
            "payload": row.payload,
            "attempts": new_attempts,
            "last_error": error_msg,
            "failed_at": chrono::Utc::now().to_rfc3339(),
        });
        if let Ok(bytes) = serde_json::to_vec(&dead_letter) {
            if let Err(e) = state.nats.publish(subject.clone(), bytes.into()).await {
                error!(
                    error = %e,
                    subject = %subject,
                    "failed to publish dead letter to NATS"
                );
            } else {
                info!(outbox_id = %row.id, subject = %subject, "dead letter published");
            }
        }
    } else {
        warn!(
            outbox_id = %row.id,
            attempts = new_attempts,
            error = %error_msg,
            "transient failure, will retry"
        );

        increment_attempts(&state.pg, row.id).await?;
    }

    Ok(())
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // QdrantPoint serialization
    // -----------------------------------------------------------------------

    #[test]
    fn test_qdrant_point_named_vector_serialization() {
        let mut vector_map = HashMap::new();
        vector_map.insert("default".to_string(), vec![1.0_f32, 2.0, 3.0]);

        let point = QdrantPoint {
            id: "test-id-1".to_string(),
            vector: vector_map,
            payload: QdrantPayload {
                topic: "test-topic".to_string(),
                agent_id: "agent-1".to_string(),
                memory_id: "mem-1".to_string(),
                created_at: "2026-01-01T00:00:00Z".to_string(),
            },
        };

        let json = serde_json::to_value(&point).unwrap();

        // Verify the vector field is a named-vector map: {"default": [1.0, 2.0, 3.0]}
        let vector_obj = json.get("vector").unwrap();
        assert!(vector_obj.is_object(), "vector should be a JSON object");

        let default_vec = vector_obj.get("default").unwrap();
        assert!(default_vec.is_array(), "default should be an array");

        let values: Vec<f64> = default_vec
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_f64().unwrap())
            .collect();
        assert_eq!(values, vec![1.0, 2.0, 3.0]);

        // Verify payload fields are present
        let payload_obj = json.get("payload").unwrap();
        assert_eq!(payload_obj.get("topic").unwrap().as_str().unwrap(), "test-topic");
        assert_eq!(payload_obj.get("agent_id").unwrap().as_str().unwrap(), "agent-1");
        assert_eq!(payload_obj.get("memory_id").unwrap().as_str().unwrap(), "mem-1");
    }

    #[test]
    fn test_qdrant_upsert_request_serialization() {
        let mut vector_map = HashMap::new();
        vector_map.insert("default".to_string(), vec![0.5_f32, 0.6]);

        let req = QdrantUpsertRequest {
            points: vec![QdrantPoint {
                id: "pt-1".to_string(),
                vector: vector_map,
                payload: QdrantPayload {
                    topic: "t".to_string(),
                    agent_id: "a".to_string(),
                    memory_id: "m".to_string(),
                    created_at: "now".to_string(),
                },
            }],
        };

        let json = serde_json::to_value(&req).unwrap();
        let points = json.get("points").unwrap().as_array().unwrap();
        assert_eq!(points.len(), 1);
        assert_eq!(points[0].get("id").unwrap().as_str().unwrap(), "pt-1");
    }

    // -----------------------------------------------------------------------
    // Circuit breaker tests (same pattern as beads-bridge)
    // -----------------------------------------------------------------------

    #[test]
    fn test_circuit_breaker_starts_closed() {
        let cb = CircuitBreaker::new("test", 5, 30);
        assert!(!cb.is_open(), "a new circuit breaker should be closed");
        assert!(cb.check().is_ok());
    }

    #[test]
    fn test_circuit_breaker_opens_after_threshold() {
        let cb = CircuitBreaker::new("test", 5, 30);

        for _ in 0..5 {
            cb.record_failure();
        }

        assert!(cb.is_open(), "breaker should be open after 5 failures");
        assert!(cb.check().is_err());
    }

    #[test]
    fn test_circuit_breaker_closes_on_success() {
        let cb = CircuitBreaker::new("test", 5, 30);

        for _ in 0..5 {
            cb.record_failure();
        }
        assert!(cb.is_open());

        cb.record_success();
        assert!(!cb.is_open(), "breaker should close after record_success()");
    }

    #[test]
    fn test_circuit_breaker_below_threshold_stays_closed() {
        let cb = CircuitBreaker::new("test", 5, 30);

        // Record 4 failures (below the threshold of 5)
        for _ in 0..4 {
            cb.record_failure();
        }

        assert!(!cb.is_open(), "4 failures < threshold of 5, breaker should remain closed");
    }

    #[test]
    fn test_circuit_breaker_check_returns_circuit_open_error() {
        let cb = CircuitBreaker::new("qdrant", 5, 30);

        for _ in 0..5 {
            cb.record_failure();
        }

        let name = cb.check().unwrap_err();
        assert_eq!(name, "qdrant", "check() should return the breaker name");

        // Verify mapping to BroodlinkError works as expected
        let err = BroodlinkError::CircuitOpen(name);
        let msg = err.to_string();
        assert!(
            msg.contains("circuit open"),
            "BroodlinkError should contain 'circuit open', got: {msg}"
        );
        assert!(
            msg.contains("qdrant"),
            "BroodlinkError should contain the breaker name, got: {msg}"
        );
    }
}
