/*
 * Broodlink - Multi-agent AI orchestration system
 * Copyright (C) 2025–2026 Neven Kordic <neven@broodlink.ai>
 * SPDX-License-Identifier: AGPL-3.0-or-later
 */

//! Model management API: list, pull, delete Ollama models.
//! Proxies to the Ollama API and adds Broodlink-specific context.

use axum::extract::{Path, Query, State};
use axum::response::sse::{Event, Sse};
use axum::Json;
use futures::stream::Stream;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::AppState;

const OLLAMA_URL: &str = "http://localhost:11434";

// -- List models --

#[derive(Serialize)]
pub struct ModelListResponse {
    pub models: Vec<ModelEntry>,
}

#[derive(Serialize)]
pub struct ModelEntry {
    pub name: String,
    pub size: String,
    pub size_bytes: u64,
    pub modified: String,
    pub role: Option<String>, // "chat", "code", "vision", "embedding", etc.
}

/// GET /api/v1/models
pub async fn list_models(State(_state): State<Arc<AppState>>) -> Json<ModelListResponse> {
    let resp = reqwest::Client::new()
        .get(format!("{OLLAMA_URL}/api/tags"))
        .timeout(std::time::Duration::from_secs(5))
        .send()
        .await;

    let models = match resp {
        Ok(r) => {
            let body: serde_json::Value = r.json().await.unwrap_or_default();
            body.get("models")
                .and_then(|m| m.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|m| {
                            let name = m.get("name")?.as_str()?.to_string();
                            let size_bytes = m.get("size")?.as_u64()?;
                            Some(ModelEntry {
                                role: infer_role(&name),
                                name,
                                size: format_size(size_bytes),
                                size_bytes,
                                modified: m.get("modified_at")?.as_str()?.to_string(),
                            })
                        })
                        .collect()
                })
                .unwrap_or_default()
        }
        Err(_) => vec![],
    };

    Json(ModelListResponse { models })
}

// -- Pull model --

#[derive(Deserialize)]
pub struct PullRequest {
    pub name: String,
}

#[derive(Serialize)]
pub struct PullResponse {
    pub started: bool,
    pub message: String,
}

/// POST /api/v1/models/pull — start pulling a model (non-blocking)
pub async fn pull_model(
    State(_state): State<Arc<AppState>>,
    Json(req): Json<PullRequest>,
) -> Json<PullResponse> {
    // Validate model name (basic sanity check)
    if req.name.is_empty() || req.name.len() > 128 {
        return Json(PullResponse {
            started: false,
            message: "Invalid model name".to_string(),
        });
    }

    // Start pull in background — progress is streamed via SSE
    let name = req.name.clone();
    tokio::spawn(async move {
        let _ = reqwest::Client::new()
            .post(format!("{OLLAMA_URL}/api/pull"))
            .json(&serde_json::json!({ "name": name, "stream": false }))
            .timeout(std::time::Duration::from_secs(3600))
            .send()
            .await;
    });

    Json(PullResponse {
        started: true,
        message: format!("Pulling {}...", req.name),
    })
}

#[derive(Deserialize)]
pub struct ProgressQuery {
    pub name: Option<String>,
}

/// GET /api/v1/models/pull/progress?name=<model> — SSE stream of real pull progress
pub async fn pull_progress_sse(
    State(_state): State<Arc<AppState>>,
    Query(query): Query<ProgressQuery>,
) -> Sse<impl Stream<Item = Result<Event, std::convert::Infallible>>> {
    let model_name = query.name.unwrap_or_default();

    let stream = async_stream::stream! {
        let client = reqwest::Client::new();

        // Stream the actual pull progress from Ollama using chunk() API
        let resp = client
            .post(format!("{OLLAMA_URL}/api/pull"))
            .json(&serde_json::json!({ "name": model_name, "stream": true }))
            .timeout(std::time::Duration::from_secs(3600))
            .send()
            .await;

        match resp {
            Ok(response) => {
                let mut buf = String::new();
                let mut r = response;

                // Read chunks until the stream ends
                loop {
                    match r.chunk().await {
                        Ok(Some(bytes)) => {
                            buf.push_str(&String::from_utf8_lossy(&bytes));
                            // Ollama sends newline-delimited JSON
                            while let Some(pos) = buf.find('\n') {
                                let line = buf[..pos].to_string();
                                buf = buf[pos + 1..].to_string();
                                if line.trim().is_empty() { continue; }

                                // Parse Ollama progress JSON and forward it
                                if let Ok(val) = serde_json::from_str::<serde_json::Value>(&line) {
                                    let status = val.get("status").and_then(|s| s.as_str()).unwrap_or("");
                                    let total = val.get("total").and_then(|t| t.as_u64()).unwrap_or(0);
                                    let completed = val.get("completed").and_then(|c| c.as_u64()).unwrap_or(0);
                                    let digest = val.get("digest").and_then(|d| d.as_str()).unwrap_or("");

                                    let pct = if total > 0 {
                                        (completed as f64 / total as f64 * 100.0) as u64
                                    } else { 0 };

                                    let evt = serde_json::json!({
                                        "status": status,
                                        "total": total,
                                        "completed": completed,
                                        "percent": pct,
                                        "digest": digest,
                                    });

                                    yield Ok(Event::default().data(evt.to_string()));
                                }
                            }
                        }
                        Ok(None) => break, // Stream ended
                        Err(e) => {
                            let evt = serde_json::json!({ "error": e.to_string() });
                            yield Ok(Event::default().data(evt.to_string()));
                            break;
                        }
                    }
                }

                // Final done event
                yield Ok(Event::default().data(
                    serde_json::json!({ "status": "done", "percent": 100 }).to_string()
                ));
            }
            Err(e) => {
                yield Ok(Event::default().data(
                    serde_json::json!({ "error": format!("Ollama unreachable: {e}") }).to_string()
                ));
            }
        }
    };

    Sse::new(stream)
}

// -- Delete model --

/// DELETE /api/v1/models/:name
pub async fn delete_model(
    State(_state): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> Json<PullResponse> {
    let resp = reqwest::Client::new()
        .delete(format!("{OLLAMA_URL}/api/delete"))
        .json(&serde_json::json!({ "name": name }))
        .send()
        .await;

    match resp {
        Ok(r) if r.status().is_success() => Json(PullResponse {
            started: true,
            message: format!("Deleted {name}"),
        }),
        Ok(r) => Json(PullResponse {
            started: false,
            message: format!("Failed: {}", r.status()),
        }),
        Err(e) => Json(PullResponse {
            started: false,
            message: e.to_string(),
        }),
    }
}

// -- Recommended models --

#[derive(Serialize)]
pub struct RecommendedModel {
    pub name: String,
    pub size: String,
    pub description: String,
    pub role: String,
    pub required: bool,
    pub category: String,
}

/// GET /api/v1/models/recommended
pub async fn recommended_models(
    State(_state): State<Arc<AppState>>,
) -> Json<Vec<RecommendedModel>> {
    Json(vec![
        // -- Core multi-agent stack (what most users need) --
        RecommendedModel {
            name: "gemma4:e4b".to_string(),
            size: "3.3 GB".to_string(),
            description: "General-purpose agent — fast, efficient, native tool calling, 256K context"
                .to_string(),
            role: "general".to_string(),
            required: false,
            category: "core".to_string(),
        },
        RecommendedModel {
            name: "nomic-embed-text".to_string(),
            size: "274 MB".to_string(),
            description: "Required for agent memory recall and semantic search".to_string(),
            role: "embedding".to_string(),
            required: false,
            category: "core".to_string(),
        },
        // -- Upgrade: better quality for users with more hardware --
        RecommendedModel {
            name: "gemma4:31b".to_string(),
            size: "20 GB".to_string(),
            description: "Primary agent model — top-tier reasoning, agentic workflows, native tool calling"
                .to_string(),
            role: "general".to_string(),
            required: false,
            category: "advanced".to_string(),
        },
        RecommendedModel {
            name: "gemma4:26b".to_string(),
            size: "16 GB".to_string(),
            description: "MoE agent — strong balance of quality and speed, code and general tasks"
                .to_string(),
            role: "coder".to_string(),
            required: false,
            category: "advanced".to_string(),
        },
        RecommendedModel {
            name: "deepseek-r1:32b".to_string(),
            size: "19 GB".to_string(),
            description: "Reasoning specialist — deep analysis, verification, complex problems"
                .to_string(),
            role: "reasoning".to_string(),
            required: false,
            category: "advanced".to_string(),
        },
    ])
}

// -- Helpers --

fn infer_role(name: &str) -> Option<String> {
    let n = name.to_lowercase();
    if n.contains("coder") || n.contains("code") {
        Some("code".to_string())
    } else if n.contains("embed") {
        Some("embedding".to_string())
    } else if n.contains("meditron") || n.contains("med") {
        Some("medical".to_string())
    } else if n.contains("gemma4") {
        Some("general".to_string())
    } else if n.contains("gemma") {
        Some("vision".to_string())
    } else if n.contains("deepseek") {
        Some("reasoning".to_string())
    } else if n.contains("glm") {
        Some("frontend".to_string())
    } else if n.contains("rerank") || n.contains("snowflake") {
        Some("reranker".to_string())
    } else {
        None
    }
}

fn format_size(bytes: u64) -> String {
    const GB: u64 = 1_073_741_824;
    const MB: u64 = 1_048_576;
    if bytes >= GB {
        format!("{:.1} GB", bytes as f64 / GB as f64)
    } else {
        format!("{:.0} MB", bytes as f64 / MB as f64)
    }
}
