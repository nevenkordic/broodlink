/*
 * Broodlink workspace-api — Web search.
 * Ported from the workspace app search_routes. Implements the SearXNG provider
 * (the self-hosted default) as an HTTP proxy. API-key providers (Brave, Tavily,
 * Serper, Google PSE) report unavailable until keys/wiring are added.
 *
 * SearXNG URL: env WORKSPACE_SEARXNG_URL or SEARXNG_INSTANCE (default
 * http://localhost:8080).
 */

use axum::http::HeaderMap;
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};

fn searxng_url() -> String {
    std::env::var("WORKSPACE_SEARXNG_URL")
        .or_else(|_| std::env::var("SEARXNG_INSTANCE"))
        .unwrap_or_else(|_| "http://localhost:8080".to_string())
}

pub async fn config() -> Json<Value> {
    Json(json!({
        "primary_provider": "searxng",
        "active_provider": "searxng",
        "has_api_key": false,
        "result_count": 5,
        "search_url": searxng_url(),
    }))
}

pub async fn providers() -> Json<Value> {
    Json(json!({ "providers": [
        { "id": "searxng",    "label": "SearXNG",      "available": true },
        { "id": "brave",      "label": "Brave Search", "available": false },
        { "id": "duckduckgo", "label": "DuckDuckGo",   "available": false },
        { "id": "google_pse", "label": "Google PSE",   "available": false },
        { "id": "tavily",     "label": "Tavily",       "available": false },
        { "id": "serper",     "label": "Serper",       "available": false }
    ] }))
}

#[derive(Deserialize)]
pub struct QueryBody {
    #[serde(default)]
    query: String,
    #[serde(default)]
    count: Option<u32>,
    #[serde(default)]
    time_filter: Option<String>,
    #[serde(default)]
    #[allow(dead_code)]
    provider: Option<String>,
}

/// Hit SearXNG's JSON API and normalize results.
async fn searxng(
    query: &str,
    count: usize,
    time_filter: &Option<String>,
) -> Result<Vec<Value>, String> {
    if query.trim().is_empty() {
        return Ok(Vec::new());
    }
    let base = searxng_url();
    let client = reqwest::Client::new();
    let mut req = client
        .get(format!("{}/search", base.trim_end_matches('/')))
        .query(&[("q", query), ("format", "json"), ("language", "en")]);
    if let Some(tf) = time_filter {
        if !tf.is_empty() {
            req = req.query(&[("time_range", tf.as_str())]);
        }
    }
    let resp = req.send().await.map_err(|e| e.to_string())?;
    let v: Value = resp.json().await.map_err(|e| e.to_string())?;
    let results = v["results"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .take(count)
                .map(|r| {
                    json!({
                        "title": r["title"].as_str().unwrap_or(""),
                        "url": r["url"].as_str().unwrap_or(""),
                        "snippet": r["content"].as_str().unwrap_or(""),
                        "age": r.get("publishedDate").and_then(|d| d.as_str()),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    Ok(results)
}

pub async fn query(headers: HeaderMap, Json(b): Json<QueryBody>) -> Json<Value> {
    let _ = headers;
    let count = b.count.unwrap_or(10).min(20) as usize;
    match searxng(&b.query, count, &b.time_filter).await {
        Ok(results) => Json(json!({ "results": results, "provider": "searxng", "time": 0.0 })),
        Err(e) => Json(json!({ "results": [], "provider": "searxng", "time": 0.0, "error": e })),
    }
}

#[derive(Deserialize)]
pub struct SearchBody {
    #[serde(default)]
    query: String,
    #[serde(default)]
    time_filter: Option<String>,
}

/// Public helper for other modules (e.g. research): SearXNG results, best-effort.
pub async fn fetch(query: &str, count: usize) -> Vec<Value> {
    searxng(query, count, &None).await.unwrap_or_default()
}

pub async fn search(headers: HeaderMap, Json(b): Json<SearchBody>) -> Json<Value> {
    let _ = headers;
    if b.query.trim().is_empty() {
        return Json(json!({ "context": "", "sources": [], "error": "query is required" }));
    }
    match searxng(&b.query, 5, &b.time_filter).await {
        Ok(results) => {
            let mut context = format!(
                "=====================================\nWEB SEARCH RESULTS\nQuery: {}\n=====================================\n",
                b.query
            );
            let mut sources = Vec::new();
            for (i, r) in results.iter().enumerate() {
                let title = r["title"].as_str().unwrap_or("");
                let url = r["url"].as_str().unwrap_or("");
                let snippet = r["snippet"].as_str().unwrap_or("");
                context.push_str(&format!(
                    "[{}] {title}\n    URL: {url}\n    {snippet}\n\n",
                    i + 1
                ));
                sources.push(json!({ "url": url, "title": title }));
            }
            Json(json!({ "context": context, "sources": sources }))
        }
        Err(e) => Json(json!({ "context": "", "sources": [], "error": e })),
    }
}
