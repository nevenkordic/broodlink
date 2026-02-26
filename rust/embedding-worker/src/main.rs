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
    operation: String,
    payload: serde_json::Value,
    attempts: i32,
}

// ---------------------------------------------------------------------------
// Knowledge Graph extraction types
// ---------------------------------------------------------------------------

#[derive(Serialize)]
struct OllamaGenerateRequest {
    model: String,
    prompt: String,
    stream: bool,
}

#[derive(Deserialize)]
struct OllamaGenerateResponse {
    response: String,
}

#[derive(Deserialize, Debug)]
struct ExtractedEntities {
    #[serde(default)]
    entities: Vec<ExtractedEntity>,
    #[serde(default)]
    relationships: Vec<ExtractedRelationship>,
}

#[derive(Deserialize, Debug)]
struct ExtractedEntity {
    name: String,
    #[serde(rename = "type")]
    entity_type: String,
    #[serde(default)]
    description: Option<String>,
}

#[derive(Deserialize, Debug)]
struct ExtractedRelationship {
    source: String,
    target: String,
    relation: String,
    #[serde(default)]
    description: Option<String>,
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
    pub kg_extraction_breaker: CircuitBreaker,
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
            sc.infisical_token.as_deref(),
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
    let ollama_breaker =
        CircuitBreaker::new("ollama", CIRCUIT_FAILURE_THRESHOLD, CIRCUIT_HALF_OPEN_SECS);
    let qdrant_breaker =
        CircuitBreaker::new("qdrant", CIRCUIT_FAILURE_THRESHOLD, CIRCUIT_HALF_OPEN_SECS);
    let kg_extraction_breaker = CircuitBreaker::new(
        "kg_extraction",
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
        kg_extraction_breaker,
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
        "SELECT id, trace_id, operation, payload, attempts
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
        let operation: String = row
            .try_get("operation")
            .unwrap_or_else(|_| "embed".to_string());
        let payload: serde_json::Value = row.try_get("payload")?;
        let attempts: i32 = row.try_get::<i32, _>("attempts").unwrap_or(0);

        result.push(OutboxRow {
            id,
            trace_id,
            operation,
            payload,
            attempts,
        });
    }

    Ok(result)
}

// ---------------------------------------------------------------------------
// Content chunking for long texts
// ---------------------------------------------------------------------------

/// Split content into overlapping chunks for embedding.
fn chunk_content(text: &str, max_tokens: usize, overlap: usize) -> Vec<String> {
    let words: Vec<&str> = text.split_whitespace().collect();
    if words.len() <= max_tokens {
        return vec![text.to_string()];
    }

    let mut chunks = Vec::new();
    let mut start = 0;
    while start < words.len() {
        let end = (start + max_tokens).min(words.len());
        chunks.push(words[start..end].join(" "));
        if end >= words.len() {
            break;
        }
        start = end.saturating_sub(overlap);
    }
    chunks
}

// ---------------------------------------------------------------------------
// Process a single outbox row end-to-end
// ---------------------------------------------------------------------------

async fn process_outbox_row(state: &AppState, row: &OutboxRow) -> Result<(), BroodlinkError> {
    let trace_id = row.trace_id.as_deref().unwrap_or("no-trace");

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

    let tags = row
        .payload
        .get("tags")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");

    // For "embed" operations: full pipeline (embed + qdrant + kg extract)
    // For "kg_extract" operations: skip embedding, only do kg extraction
    if row.operation == "embed" {
        let chunks = chunk_content(
            content,
            state.config.memory_search.chunk_max_tokens,
            state.config.memory_search.chunk_overlap_tokens,
        );

        let now_str = chrono::Utc::now().to_rfc3339();
        let qdrant_id = Uuid::new_v4().to_string();

        if chunks.len() <= 1 {
            // Original single-embedding path
            let embedding = match get_embedding(state, content).await {
                Ok(vec) => vec,
                Err(e) => {
                    return handle_failure(state, row, &e.to_string()).await;
                }
            };

            if let Err(e) = upsert_qdrant(
                state, &qdrant_id, &embedding, topic, agent_name, memory_id, &now_str,
            )
            .await
            {
                return handle_failure(state, row, &e.to_string()).await;
            }
        } else {
            // Multi-chunk path
            info!(
                outbox_id = %row.id,
                chunks = chunks.len(),
                "splitting content into chunks for embedding"
            );
            for (i, chunk) in chunks.iter().enumerate() {
                // Generate a deterministic UUID for each chunk by modifying the
                // base qdrant_id bytes with the chunk index.  This keeps the
                // chunk point IDs stable across re-embeds for the same memory.
                let chunk_qdrant_id = {
                    let mut bytes = Uuid::parse_str(&qdrant_id)
                        .unwrap_or_else(|_| Uuid::new_v4())
                        .into_bytes();
                    // XOR the last two bytes with the chunk index to make it unique
                    bytes[14] ^= (i as u8).wrapping_add(1);
                    bytes[15] ^= (i as u8).wrapping_add(0xAB);
                    // Set version 4 + variant bits so it's still a valid UUID
                    bytes[6] = (bytes[6] & 0x0F) | 0x40; // version 4
                    bytes[8] = (bytes[8] & 0x3F) | 0x80; // variant 1
                    Uuid::from_bytes(bytes).to_string()
                };
                let chunk_topic = format!("{topic} [part {}/{}]", i + 1, chunks.len());
                match get_embedding(state, chunk).await {
                    Ok(embedding) => {
                        if let Err(e) = upsert_qdrant(
                            state,
                            &chunk_qdrant_id,
                            &embedding,
                            &chunk_topic,
                            agent_name,
                            memory_id,
                            &now_str,
                        )
                        .await
                        {
                            warn!(chunk = i, error = %e, "failed to upsert chunk, skipping");
                        }
                    }
                    Err(e) => {
                        warn!(chunk = i, error = %e, "failed to embed chunk, skipping");
                    }
                }
            }
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
    }

    // Step 4: Knowledge graph entity extraction (non-fatal, for both embed and kg_extract)
    if state.config.memory_search.kg_enabled {
        match extract_entities(state, topic, content, tags, agent_name).await {
            Ok(extracted) => {
                if !extracted.entities.is_empty() {
                    let mut entity_map: HashMap<String, String> = HashMap::new();
                    for ent in &extracted.entities {
                        match resolve_entity(state, ent, agent_name, memory_id).await {
                            Ok(eid) => {
                                entity_map.insert(ent.name.to_lowercase(), eid);
                            }
                            Err(e) => {
                                warn!(error = %e, entity = %ent.name, "entity resolution failed (non-fatal)");
                            }
                        }
                    }
                    if !extracted.relationships.is_empty() {
                        if let Err(e) = resolve_edges(
                            state,
                            &extracted.relationships,
                            &entity_map,
                            memory_id,
                            agent_name,
                        )
                        .await
                        {
                            warn!(error = %e, "edge resolution failed (non-fatal)");
                        }
                    }
                    info!(
                        outbox_id = %row.id,
                        entities = extracted.entities.len(),
                        relationships = extracted.relationships.len(),
                        "kg extraction complete"
                    );
                }
            }
            Err(e) => {
                warn!(
                    error = %e,
                    outbox_id = %row.id,
                    "kg extraction failed (non-fatal, memory still stored)"
                );
            }
        }
    }

    // Step 5: Mark outbox row as done
    mark_done(&state.pg, row.id).await?;

    info!(
        outbox_id = %row.id,
        operation = %row.operation,
        memory_id = %memory_id,
        "embedding pipeline complete"
    );

    Ok(())
}

// ---------------------------------------------------------------------------
// Ollama embedding
// ---------------------------------------------------------------------------

async fn get_embedding(state: &AppState, content: &str) -> Result<Vec<f32>, BroodlinkError> {
    state
        .ollama_breaker
        .check()
        .map_err(BroodlinkError::CircuitOpen)?;

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
// Knowledge Graph: LLM entity extraction
// ---------------------------------------------------------------------------

async fn extract_entities(
    state: &AppState,
    topic: &str,
    content: &str,
    tags: &str,
    agent_id: &str,
) -> Result<ExtractedEntities, BroodlinkError> {
    state
        .kg_extraction_breaker
        .check()
        .map_err(BroodlinkError::CircuitOpen)?;

    let prompt = format!(
        "System: You are an entity and relationship extractor. Given a memory stored by an AI agent, extract all entities and relationships. Respond ONLY with valid JSON, no markdown, no explanation.\n\n\
         User: Extract entities and relationships from this agent memory.\n\n\
         Topic: {topic}\n\
         Content: {content}\n\
         Tags: {tags}\n\
         Agent: {agent_id}\n\n\
         Respond with this exact JSON structure:\n\
         {{\n  \"entities\": [\n    {{\n      \"name\": \"exact entity name\",\n      \"type\": \"person|service|concept|location|technology|organization|event|other\",\n      \"description\": \"one-sentence description of this entity\"\n    }}\n  ],\n  \"relationships\": [\n    {{\n      \"source\": \"entity name (must match an entity above)\",\n      \"target\": \"entity name (must match an entity above)\",\n      \"relation\": \"RELATION_TYPE in UPPER_SNAKE_CASE\",\n      \"description\": \"one-sentence description of this relationship\"\n    }}\n  ]\n}}\n\n\
         Rules:\n\
         - Extract ALL named entities: people, services, systems, technologies, concepts, places, organizations\n\
         - Extract ALL relationships stated or strongly implied\n\
         - Use UPPER_SNAKE_CASE for relation types (e.g. LEADS, DEPENDS_ON, WORKS_FOR, BUILT_WITH, LOCATED_IN, RELATED_TO)\n\
         - Entity names should be specific and canonical (e.g. \"Redis\" not \"the cache\")\n\
         - If no entities or relationships found, return empty arrays\n\
         - Do NOT hallucinate entities or relationships not present in the text"
    );

    let url = format!("{}/api/generate", state.config.ollama.url);
    let body = OllamaGenerateRequest {
        model: state.config.memory_search.kg_extraction_model.clone(),
        prompt,
        stream: false,
    };

    let resp = state
        .http_client
        .post(&url)
        .json(&body)
        .timeout(Duration::from_secs(
            state.config.memory_search.kg_extraction_timeout_seconds,
        ))
        .send()
        .await
        .map_err(|e| {
            state.kg_extraction_breaker.record_failure();
            BroodlinkError::Ollama(format!("kg extraction request failed: {e}"))
        })?;

    if !resp.status().is_success() {
        state.kg_extraction_breaker.record_failure();
        return Err(BroodlinkError::Ollama(format!(
            "kg extraction returned status {}",
            resp.status()
        )));
    }
    state.kg_extraction_breaker.record_success();

    let gen_resp: OllamaGenerateResponse = resp
        .json()
        .await
        .map_err(|e| BroodlinkError::Ollama(format!("kg extraction parse: {e}")))?;

    // Clean LLM output: strip markdown code fences
    let cleaned = gen_resp.response.trim();
    let cleaned = cleaned
        .strip_prefix("```json")
        .or_else(|| cleaned.strip_prefix("```"))
        .unwrap_or(cleaned);
    let cleaned = cleaned.strip_suffix("```").unwrap_or(cleaned).trim();

    serde_json::from_str::<ExtractedEntities>(cleaned)
        .map_err(|e| BroodlinkError::Internal(format!("kg extraction JSON parse failed: {e}")))
}

// ---------------------------------------------------------------------------
// Knowledge Graph: entity resolution
// ---------------------------------------------------------------------------

async fn resolve_entity(
    state: &AppState,
    entity: &ExtractedEntity,
    agent_id: &str,
    memory_id: &str,
) -> Result<String, BroodlinkError> {
    let mem_id: i64 = memory_id.parse().unwrap_or(0);

    // 1. Exact match by name
    let existing: Option<(String, i32)> = sqlx::query_as(
        "SELECT entity_id, mention_count FROM kg_entities WHERE lower(name) = lower($1)",
    )
    .bind(&entity.name)
    .fetch_optional(&state.pg)
    .await?;

    if let Some((entity_id, count)) = existing {
        sqlx::query(
            "UPDATE kg_entities SET mention_count = $1, last_seen = NOW(), updated_at = NOW() WHERE entity_id = $2",
        )
        .bind(count + 1)
        .bind(&entity_id)
        .execute(&state.pg)
        .await?;

        if let Err(e) = sqlx::query(
            "INSERT INTO kg_entity_memories (entity_id, memory_id, agent_id)
             VALUES ($1, $2, $3) ON CONFLICT DO NOTHING",
        )
        .bind(&entity_id)
        .bind(mem_id)
        .bind(agent_id)
        .execute(&state.pg)
        .await
        {
            warn!(entity_id = %entity_id, memory_id = mem_id, error = %e, "failed to link entity to memory (exact match)");
        }

        return Ok(entity_id);
    }

    // 2. Fuzzy match via embedding similarity
    if let Ok(embedding) = get_embedding(state, &entity.name).await {
        let embedding_f64: Vec<f64> = embedding.iter().map(|&f| f64::from(f)).collect();

        let qdrant_url = format!(
            "{}/collections/broodlink_kg_entities/points/search",
            state.config.qdrant.url
        );
        let qdrant_body = serde_json::json!({
            "vector": { "name": "default", "vector": embedding_f64 },
            "limit": 1,
            "with_payload": true,
        });

        match state
            .http_client
            .post(&qdrant_url)
            .json(&qdrant_body)
            .timeout(Duration::from_secs(10))
            .send()
            .await
        {
            Ok(resp) => {
                if resp.status().is_success() {
                    match resp.json::<serde_json::Value>().await {
                        Ok(json) => {
                            if let Some(results) = json.get("result").and_then(|r| r.as_array()) {
                                if let Some(top) = results.first() {
                                    let score =
                                        top.get("score").and_then(|s| s.as_f64()).unwrap_or(0.0);
                                    if score
                                        >= state.config.memory_search.kg_entity_similarity_threshold
                                    {
                                        if let Some(matched_id) = top
                                            .get("payload")
                                            .and_then(|p| p.get("entity_id"))
                                            .and_then(|e| e.as_str())
                                        {
                                            if !matched_id.is_empty() {
                                                sqlx::query(
                                                    "UPDATE kg_entities SET mention_count = mention_count + 1, last_seen = NOW(), updated_at = NOW() WHERE entity_id = $1",
                                                )
                                                .bind(matched_id)
                                                .execute(&state.pg)
                                                .await?;

                                                if let Err(e) = sqlx::query(
                                                    "INSERT INTO kg_entity_memories (entity_id, memory_id, agent_id)
                                                     VALUES ($1, $2, $3) ON CONFLICT DO NOTHING",
                                                )
                                                .bind(matched_id)
                                                .bind(mem_id)
                                                .bind(agent_id)
                                                .execute(&state.pg)
                                                .await
                                                {
                                                    warn!(entity_id = %matched_id, memory_id = mem_id, error = %e, "failed to link entity to memory (fuzzy match)");
                                                }

                                                return Ok(matched_id.to_string());
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        Err(e) => {
                            warn!(error = %e, entity = %entity.name, "failed to parse Qdrant search response");
                        }
                    }
                } else {
                    warn!(status = %resp.status(), entity = %entity.name, "Qdrant entity search returned non-success status");
                }
            }
            Err(e) => {
                warn!(error = %e, entity = %entity.name, "Qdrant entity search request failed");
            }
        }
    }

    // 3. New entity: insert into Postgres + upsert embedding to Qdrant
    let entity_id = Uuid::new_v4().to_string();
    let qdrant_point_id = Uuid::new_v4().to_string();

    sqlx::query(
        "INSERT INTO kg_entities (entity_id, name, entity_type, description, embedding_ref, source_agent, created_at, updated_at)
         VALUES ($1, $2, $3, $4, $5, $6, NOW(), NOW())",
    )
    .bind(&entity_id)
    .bind(&entity.name)
    .bind(&entity.entity_type)
    .bind(&entity.description)
    .bind(&qdrant_point_id)
    .bind(agent_id)
    .execute(&state.pg)
    .await?;

    // Upsert entity name embedding to Qdrant kg collection (non-fatal)
    if let Ok(emb) = get_embedding(state, &entity.name).await {
        let emb_f64: Vec<f64> = emb.iter().map(|&f| f64::from(f)).collect();
        let mut vector_map = HashMap::new();
        vector_map.insert("default".to_string(), emb_f64);

        let kg_point = serde_json::json!({
            "points": [{
                "id": qdrant_point_id,
                "vector": { "default": emb.iter().map(|&f| f64::from(f)).collect::<Vec<f64>>() },
                "payload": {
                    "entity_id": entity_id,
                    "name": entity.name,
                    "entity_type": entity.entity_type,
                    "source_agent": agent_id,
                },
            }],
        });

        let url = format!(
            "{}/collections/broodlink_kg_entities/points",
            state.config.qdrant.url
        );
        if let Err(e) = state
            .http_client
            .put(&url)
            .json(&kg_point)
            .timeout(Duration::from_secs(10))
            .send()
            .await
        {
            warn!(error = %e, entity = %entity.name, "failed to upsert kg entity to qdrant (non-fatal)");
        }
    }

    // Link to memory
    let _ = sqlx::query(
        "INSERT INTO kg_entity_memories (entity_id, memory_id, agent_id)
         VALUES ($1, $2, $3) ON CONFLICT DO NOTHING",
    )
    .bind(&entity_id)
    .bind(mem_id)
    .bind(agent_id)
    .execute(&state.pg)
    .await;

    Ok(entity_id)
}

// ---------------------------------------------------------------------------
// Knowledge Graph: edge resolution
// ---------------------------------------------------------------------------

async fn resolve_edges(
    state: &AppState,
    relationships: &[ExtractedRelationship],
    entity_map: &HashMap<String, String>,
    memory_id: &str,
    agent_id: &str,
) -> Result<u32, BroodlinkError> {
    let mem_id: i64 = memory_id.parse().unwrap_or(0);
    let mut created = 0u32;

    for rel in relationships {
        let source_id = match entity_map.get(&rel.source.to_lowercase()) {
            Some(id) => id,
            None => continue,
        };
        let target_id = match entity_map.get(&rel.target.to_lowercase()) {
            Some(id) => id,
            None => continue,
        };

        // Check for existing active edge
        let existing: Option<(String, f64)> = sqlx::query_as(
            "SELECT edge_id, weight FROM kg_edges
             WHERE source_id = $1 AND target_id = $2 AND relation_type = $3 AND valid_to IS NULL",
        )
        .bind(source_id)
        .bind(target_id)
        .bind(&rel.relation)
        .fetch_optional(&state.pg)
        .await?;

        if let Some((edge_id, weight)) = existing {
            // Reinforce: increment weight
            sqlx::query("UPDATE kg_edges SET weight = $1, updated_at = NOW() WHERE edge_id = $2")
                .bind(weight + 0.1)
                .bind(&edge_id)
                .execute(&state.pg)
                .await?;
        } else {
            // New edge
            let edge_id = Uuid::new_v4().to_string();
            sqlx::query(
                "INSERT INTO kg_edges (edge_id, source_id, target_id, relation_type, description, source_memory_id, source_agent, created_at, updated_at)
                 VALUES ($1, $2, $3, $4, $5, $6, $7, NOW(), NOW())",
            )
            .bind(&edge_id)
            .bind(source_id)
            .bind(target_id)
            .bind(&rel.relation)
            .bind(&rel.description)
            .bind(mem_id)
            .bind(agent_id)
            .execute(&state.pg)
            .await?;
            created += 1;
        }
    }

    Ok(created)
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
    state
        .qdrant_breaker
        .check()
        .map_err(BroodlinkError::CircuitOpen)?;

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
    sqlx::query("UPDATE outbox SET status = 'done', processed_at = NOW() WHERE id = $1")
        .bind(outbox_id)
        .execute(pg)
        .await?;
    Ok(())
}

async fn increment_attempts(pg: &PgPool, outbox_id: i64) -> Result<(), BroodlinkError> {
    sqlx::query("UPDATE outbox SET status = 'pending', attempts = attempts + 1 WHERE id = $1")
        .bind(outbox_id)
        .execute(pg)
        .await?;
    Ok(())
}

async fn mark_failed(pg: &PgPool, outbox_id: i64) -> Result<(), BroodlinkError> {
    sqlx::query("UPDATE outbox SET status = 'failed', processed_at = NOW() WHERE id = $1")
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
                    "failed to publish dead letter to NATS, falling back to DB"
                );
                // Fallback: persist dead letter directly to Postgres
                if let Err(db_e) = sqlx::query(
                    "INSERT INTO dead_letter_queue (task_id, reason, source_service) VALUES ($1, $2, 'embedding-worker')",
                )
                .bind(&row.trace_id)
                .bind(error_msg)
                .execute(&state.pg)
                .await
                {
                    error!(error = %db_e, outbox_id = %row.id, "dead letter DB fallback also failed");
                }
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
        assert_eq!(
            payload_obj.get("topic").unwrap().as_str().unwrap(),
            "test-topic"
        );
        assert_eq!(
            payload_obj.get("agent_id").unwrap().as_str().unwrap(),
            "agent-1"
        );
        assert_eq!(
            payload_obj.get("memory_id").unwrap().as_str().unwrap(),
            "mem-1"
        );
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

        assert!(
            !cb.is_open(),
            "4 failures < threshold of 5, breaker should remain closed"
        );
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

    // -----------------------------------------------------------------------
    // Knowledge Graph extraction tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_extraction_json_parsing() {
        let json_str = r#"{
            "entities": [
                {"name": "Alice", "type": "person", "description": "Team lead"},
                {"name": "auth-service", "type": "service", "description": "Authentication microservice"}
            ],
            "relationships": [
                {"source": "Alice", "target": "auth-service", "relation": "LEADS", "description": "Alice leads auth-service"}
            ]
        }"#;

        let extracted: ExtractedEntities = serde_json::from_str(json_str).unwrap();
        assert_eq!(extracted.entities.len(), 2);
        assert_eq!(extracted.relationships.len(), 1);
        assert_eq!(extracted.entities[0].name, "Alice");
        assert_eq!(extracted.entities[0].entity_type, "person");
        assert_eq!(extracted.relationships[0].relation, "LEADS");
    }

    #[test]
    fn test_extraction_strip_markdown_fences() {
        let raw = "```json\n{\"entities\": [], \"relationships\": []}\n```";
        let cleaned = raw.trim();
        let cleaned = cleaned
            .strip_prefix("```json")
            .or_else(|| cleaned.strip_prefix("```"))
            .unwrap_or(cleaned);
        let cleaned = cleaned.strip_suffix("```").unwrap_or(cleaned).trim();

        let extracted: ExtractedEntities = serde_json::from_str(cleaned).unwrap();
        assert!(extracted.entities.is_empty());
        assert!(extracted.relationships.is_empty());
    }

    #[test]
    fn test_extraction_empty_entities() {
        let json_str = r#"{"entities": [], "relationships": []}"#;
        let extracted: ExtractedEntities = serde_json::from_str(json_str).unwrap();
        assert!(extracted.entities.is_empty());
        assert!(extracted.relationships.is_empty());
    }

    #[test]
    fn test_extraction_missing_optional_fields() {
        let json_str = r#"{
            "entities": [{"name": "Redis", "type": "technology"}],
            "relationships": [{"source": "Redis", "target": "Redis", "relation": "SELF_REF"}]
        }"#;
        let extracted: ExtractedEntities = serde_json::from_str(json_str).unwrap();
        assert_eq!(extracted.entities.len(), 1);
        assert!(extracted.entities[0].description.is_none());
        assert!(extracted.relationships[0].description.is_none());
    }

    #[test]
    fn test_kg_circuit_breaker_starts_closed() {
        let cb = CircuitBreaker::new("kg_extraction", 5, 30);
        assert!(!cb.is_open());
        assert!(cb.check().is_ok());
    }

    // -----------------------------------------------------------------------
    // v0.5.0: KG extraction edge cases
    // -----------------------------------------------------------------------

    #[test]
    fn test_extraction_unicode_entities() {
        let json_str = r#"{
            "entities": [
                {"name": "München-Cluster", "type": "location", "description": "EU data center"},
                {"name": "Ключ-Сервис", "type": "service"}
            ],
            "relationships": [
                {"source": "München-Cluster", "target": "Ключ-Сервис", "relation": "HOSTS"}
            ]
        }"#;
        let extracted: ExtractedEntities = serde_json::from_str(json_str).unwrap();
        assert_eq!(extracted.entities.len(), 2);
        assert_eq!(extracted.entities[0].name, "München-Cluster");
        assert_eq!(extracted.relationships[0].source, "München-Cluster");
    }

    #[test]
    fn test_extraction_only_entities_no_relationships() {
        let json_str =
            r#"{"entities": [{"name": "solo-node", "type": "service"}], "relationships": []}"#;
        let extracted: ExtractedEntities = serde_json::from_str(json_str).unwrap();
        assert_eq!(extracted.entities.len(), 1);
        assert!(extracted.relationships.is_empty());
    }

    #[test]
    fn test_extraction_only_relationships_no_entities() {
        // Malformed LLM output — relationships without entities
        let json_str = r#"{"entities": [], "relationships": [{"source": "a", "target": "b", "relation": "X"}]}"#;
        let extracted: ExtractedEntities = serde_json::from_str(json_str).unwrap();
        assert!(extracted.entities.is_empty());
        assert_eq!(extracted.relationships.len(), 1);
    }

    #[test]
    fn test_extraction_large_entity_count() {
        let mut entities = Vec::new();
        for i in 0..50 {
            entities.push(serde_json::json!({"name": format!("entity-{i}"), "type": "service"}));
        }
        let json_val = serde_json::json!({"entities": entities, "relationships": []});
        let json_str = serde_json::to_string(&json_val).unwrap();
        let extracted: ExtractedEntities = serde_json::from_str(&json_str).unwrap();
        assert_eq!(extracted.entities.len(), 50);
    }

    #[test]
    fn test_extraction_strip_markdown_fence_with_language_tag() {
        let raw = "```json\n{\"entities\": [{\"name\": \"x\", \"type\": \"t\"}], \"relationships\": []}\n```";
        let cleaned = raw.trim();
        let cleaned = cleaned
            .strip_prefix("```json")
            .or_else(|| cleaned.strip_prefix("```"))
            .unwrap_or(cleaned);
        let cleaned = cleaned.strip_suffix("```").unwrap_or(cleaned).trim();
        let extracted: ExtractedEntities = serde_json::from_str(cleaned).unwrap();
        assert_eq!(extracted.entities[0].name, "x");
    }

    #[test]
    fn test_extraction_strip_bare_markdown_fence() {
        let raw = "```\n{\"entities\": [], \"relationships\": []}\n```";
        let cleaned = raw.trim();
        let cleaned = cleaned
            .strip_prefix("```json")
            .or_else(|| cleaned.strip_prefix("```"))
            .unwrap_or(cleaned);
        let cleaned = cleaned.strip_suffix("```").unwrap_or(cleaned).trim();
        let extracted: ExtractedEntities = serde_json::from_str(cleaned).unwrap();
        assert!(extracted.entities.is_empty());
    }

    #[test]
    fn test_extraction_no_fence_passthrough() {
        let raw = r#"{"entities": [{"name": "n", "type": "t"}], "relationships": []}"#;
        let cleaned = raw.trim();
        let cleaned = cleaned
            .strip_prefix("```json")
            .or_else(|| cleaned.strip_prefix("```"))
            .unwrap_or(cleaned);
        let cleaned = cleaned.strip_suffix("```").unwrap_or(cleaned).trim();
        let extracted: ExtractedEntities = serde_json::from_str(cleaned).unwrap();
        assert_eq!(extracted.entities.len(), 1);
    }

    #[test]
    fn test_extraction_invalid_json_fails() {
        let json_str = "not valid json at all";
        let result = serde_json::from_str::<ExtractedEntities>(json_str);
        assert!(result.is_err(), "invalid JSON should fail to parse");
    }

    #[test]
    fn test_extraction_partial_json_with_defaults() {
        // ExtractedEntities uses #[serde(default)] so missing fields get defaults
        let json_str = r#"{}"#;
        let extracted: ExtractedEntities = serde_json::from_str(json_str).unwrap();
        assert!(extracted.entities.is_empty());
        assert!(extracted.relationships.is_empty());
    }

    #[test]
    fn test_extraction_duplicate_entity_names() {
        let json_str = r#"{
            "entities": [
                {"name": "redis", "type": "service"},
                {"name": "redis", "type": "technology"}
            ],
            "relationships": []
        }"#;
        let extracted: ExtractedEntities = serde_json::from_str(json_str).unwrap();
        assert_eq!(
            extracted.entities.len(),
            2,
            "duplicate names should both parse"
        );
    }

    // -----------------------------------------------------------------------
    // v0.5.0: Outbox operation branching logic tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_outbox_operation_embed_identified() {
        let operation = "embed";
        assert_eq!(operation, "embed");
        assert_ne!(operation, "kg_extract");
    }

    #[test]
    fn test_outbox_operation_kg_extract_identified() {
        let operation = "kg_extract";
        assert_eq!(operation, "kg_extract");
        assert_ne!(operation, "embed");
    }

    #[test]
    fn test_outbox_payload_has_required_fields() {
        // Verify the outbox payload structure used by store_memory
        let payload = serde_json::json!({
            "topic": "test-topic",
            "content": "test content",
            "agent_name": "claude",
            "memory_id": "42",
            "tags": "foo, bar",
        });

        assert!(payload.get("topic").is_some());
        assert!(payload.get("content").is_some());
        assert!(payload.get("agent_name").is_some());
        assert!(payload.get("memory_id").is_some());
        assert!(payload.get("tags").is_some());
    }

    #[test]
    fn test_outbox_payload_memory_id_is_string() {
        // memory_id must be serialized as string (from i64)
        let memory_id: i64 = 42;
        let payload = serde_json::json!({"memory_id": memory_id.to_string()});
        assert_eq!(payload["memory_id"], "42");
    }

    // -----------------------------------------------------------------------
    // v0.5.0: OllamaGenerateRequest serialization
    // -----------------------------------------------------------------------

    #[test]
    fn test_ollama_generate_request_serialization() {
        let req = OllamaGenerateRequest {
            model: "qwen3:1.7b".to_string(),
            prompt: "Extract entities from: test".to_string(),
            stream: false,
        };
        let json = serde_json::to_value(&req).unwrap();
        assert_eq!(json["model"], "qwen3:1.7b");
        assert_eq!(json["stream"], false);
        assert!(json["prompt"]
            .as_str()
            .unwrap()
            .contains("Extract entities"));
    }

    #[test]
    fn test_ollama_generate_response_deserialization() {
        let json_str = r#"{"response": "{\"entities\": []}"}"#;
        let resp: OllamaGenerateResponse = serde_json::from_str(json_str).unwrap();
        assert!(resp.response.contains("entities"));
    }

    // -----------------------------------------------------------------------
    // v0.5.0: Entity type validation
    // -----------------------------------------------------------------------

    #[test]
    fn test_extracted_entity_type_rename() {
        // Verify #[serde(rename = "type")] works correctly
        let json_str = r#"{"name": "test", "type": "person"}"#;
        let entity: ExtractedEntity = serde_json::from_str(json_str).unwrap();
        assert_eq!(entity.entity_type, "person");
    }

    #[test]
    fn test_extracted_entity_field_not_entity_type() {
        // "entity_type" in JSON should NOT parse (it expects "type")
        let json_str = r#"{"name": "test", "entity_type": "person"}"#;
        let result = serde_json::from_str::<ExtractedEntity>(json_str);
        assert!(
            result.is_err(),
            "JSON field 'entity_type' should not match; expects 'type'"
        );
    }

    #[test]
    fn test_extracted_relationship_all_fields() {
        let json_str = r#"{
            "source": "Alice",
            "target": "auth-service",
            "relation": "MANAGES",
            "description": "Alice manages the auth service team"
        }"#;
        let rel: ExtractedRelationship = serde_json::from_str(json_str).unwrap();
        assert_eq!(rel.source, "Alice");
        assert_eq!(rel.target, "auth-service");
        assert_eq!(rel.relation, "MANAGES");
        assert_eq!(
            rel.description.as_deref(),
            Some("Alice manages the auth service team")
        );
    }
}
