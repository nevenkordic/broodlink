/*
 * Broodlink - Multi-agent AI orchestration system
 * Copyright (C) 2025–2026 Neven Kordic <neven@broodlink.ai>
 * SPDX-License-Identifier: AGPL-3.0-or-later
 */

//! Serves the embedded dashboard static files.
//! The Hugo site is pre-built and included at compile time via rust-embed.

use axum::extract::State;
use axum::http::{header, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use rust_embed::Embed;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use crate::AppState;

#[derive(Embed)]
#[folder = "../../status-site/public"]
struct DashboardAssets;

/// Dynamic fallback — checks setup_complete on every request.
/// During setup: only /setup/* and static assets are served, everything else redirects.
/// After setup: full dashboard is served.
pub async fn dynamic_fallback(State(state): State<Arc<AppState>>, uri: Uri) -> Response {
    let path = uri.path();

    if !state.setup_complete.load(Ordering::Relaxed) {
        // Setup mode: only serve setup pages and static assets
        if path.starts_with("/setup")
            || path.starts_with("/css/")
            || path.starts_with("/js/")
            || path.starts_with("/img/")
            || path.starts_with("/fonts/")
        {
            return serve_path(path, &state);
        }
        return axum::response::Redirect::temporary("/setup/").into_response();
    }

    // Normal mode: redirect /setup/ back to dashboard
    if path.starts_with("/setup") {
        return axum::response::Redirect::temporary("/").into_response();
    }

    serve_path(path, &state)
}

fn serve_path(path: &str, state: &AppState) -> Response {
    let path = path.trim_start_matches('/').trim_end_matches('/');

    // Try exact path
    if let Some(content) = DashboardAssets::get(path) {
        return serve_embedded(path, &content, state);
    }

    // Try directory index (e.g. "workflows" -> "workflows/index.html")
    let with_index = format!("{path}/index.html");
    if let Some(content) = DashboardAssets::get(&with_index) {
        return serve_embedded(&with_index, &content, state);
    }

    // Try with .html extension (e.g. "about" -> "about.html")
    let with_ext = format!("{path}.html");
    if let Some(content) = DashboardAssets::get(&with_ext) {
        return serve_embedded(&with_ext, &content, state);
    }

    // SPA fallback — serve root index
    if let Some(content) = DashboardAssets::get("index.html") {
        return serve_embedded("index.html", &content, state);
    }

    StatusCode::NOT_FOUND.into_response()
}

fn serve_embedded(path: &str, content: &rust_embed::EmbeddedFile, state: &AppState) -> Response {
    let mime = mime_guess::from_path(path).first_or_octet_stream();

    // For HTML pages, inject runtime config (API key) so we never bake secrets
    // into the static Hugo build.
    if path.ends_with(".html") {
        let html = String::from_utf8_lossy(&content.data);
        let config_script = format!(
            r#"<script>window.BroodlinkConfig={{statusApiKey:"{}"}}</script>"#,
            state.status_api_key.replace('"', r#"\""#),
        );
        let injected = html.replacen("</head>", &format!("{config_script}</head>"), 1);
        return (
            [
                (header::CONTENT_TYPE, mime.as_ref().to_string()),
                (header::CACHE_CONTROL, cache_policy(path)),
            ],
            injected,
        )
            .into_response();
    }

    (
        [
            (header::CONTENT_TYPE, mime.as_ref().to_string()),
            (header::CACHE_CONTROL, cache_policy(path)),
        ],
        content.data.clone().into_owned(),
    )
        .into_response()
}

fn cache_policy(path: &str) -> String {
    if path.ends_with(".html") || path.ends_with('/') {
        "no-cache".to_string()
    } else {
        "public, max-age=3600".to_string()
    }
}
