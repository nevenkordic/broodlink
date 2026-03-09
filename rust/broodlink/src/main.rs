/*
 * Broodlink - Multi-agent AI orchestration system
 * Copyright (C) 2025–2026 Neven Kordic <neven@broodlink.ai>
 *
 * This program is free software: you can redistribute it
 * and/or modify it under the terms of the GNU Affero
 * General Public License as published by the Free Software
 * Foundation, either version 3 of the License, or (at your
 * option) any later version.
 */

use anyhow::Result;
use axum::response::IntoResponse;
use clap::{Parser, Subcommand};
use std::net::SocketAddr;
use std::sync::atomic::AtomicBool;
use tracing_subscriber::EnvFilter;

mod dashboard;
mod models;
mod process_manager;
mod setup;

#[derive(Parser)]
#[command(name = "broodlink", about = "Broodlink AI orchestration platform")]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Start all Broodlink services and open the dashboard
    Start {
        /// Port for the unified web interface
        #[arg(long, default_value = "3310")]
        port: u16,
    },
    /// Stop a running Broodlink instance
    Stop,
    /// Check system status without starting services
    Status,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| {
            EnvFilter::new("broodlink=info,tower_http=info")
        }))
        .init();

    let cli = Cli::parse();

    match cli.command.unwrap_or(Commands::Start { port: 3310 }) {
        Commands::Start { port } => cmd_start(port).await,
        Commands::Stop => cmd_stop().await,
        Commands::Status => cmd_status().await,
    }
}

async fn cmd_start(port: u16) -> Result<()> {
    let addr = SocketAddr::from(([0, 0, 0, 0], port));

    // Check if setup is complete
    let setup_state = setup::check_system_state().await;
    let needs_setup = !setup_state.is_ready();

    // Load config + API key for proxying to status-api
    let bl_dir = broodlink_dir();
    let (status_api_url, status_api_key) = load_status_api_config(&bl_dir).await;

    // Build the shared app state
    let state = AppState {
        port,
        broodlink_dir: bl_dir,
        process_manager: process_manager::ProcessManager::new(),
        setup_complete: AtomicBool::new(!needs_setup),
        status_api_url,
        status_api_key,
        http_client: reqwest::Client::new(),
    };
    let shared = std::sync::Arc::new(state);

    // Build router
    let app = build_router(shared.clone());

    tracing::info!("Broodlink starting on http://localhost:{port}");

    if needs_setup {
        tracing::info!("First run detected — opening setup wizard");
    } else {
        // Start all backend services
        tracing::info!("Starting backend services...");
        if let Err(e) = shared.process_manager.start_all(&shared.broodlink_dir).await {
            tracing::warn!(error = %e, "Some services failed to start — check setup");
        }
    }

    // Open browser — go to setup wizard on first run, dashboard otherwise
    if needs_setup {
        let _ = open_browser(&format!("http://localhost:{port}/setup/"));
    } else {
        let _ = open_browser(&format!("http://localhost:{port}"));
    }

    // Serve
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;

    // Cleanup
    tracing::info!("Shutting down services...");
    shared.process_manager.stop_all().await;
    tracing::info!("Broodlink stopped.");

    Ok(())
}

async fn cmd_stop() -> Result<()> {
    let pid_file = broodlink_dir().join(".broodlink/broodlink.pid");
    if pid_file.exists() {
        let pid_str = std::fs::read_to_string(&pid_file)?;
        if let Ok(pid) = pid_str.trim().parse::<u32>() {
            tracing::info!(pid, "Sending shutdown signal");
            #[cfg(unix)]
            {
                // Safety: sending SIGTERM to an existing process
                unsafe { libc::kill(pid as i32, libc::SIGTERM); }
            }
            #[cfg(windows)]
            {
                let _ = std::process::Command::new("taskkill")
                    .args(["/PID", &pid.to_string()])
                    .output();
            }
            let _ = std::fs::remove_file(&pid_file);
        }
    } else {
        tracing::info!("No running Broodlink instance found");
    }
    Ok(())
}

async fn cmd_status() -> Result<()> {
    let state = setup::check_system_state().await;
    println!("{}", serde_json::to_string_pretty(&state)?);
    Ok(())
}

pub struct AppState {
    pub port: u16,
    pub broodlink_dir: std::path::PathBuf,
    pub process_manager: process_manager::ProcessManager,
    pub setup_complete: AtomicBool,
    pub status_api_url: String,
    pub status_api_key: String,
    pub http_client: reqwest::Client,
}

fn build_router(
    state: std::sync::Arc<AppState>,
) -> axum::Router {
    use axum::routing::{get, post, delete, any};

    let app = axum::Router::new()
        // Setup wizard API (always available)
        .route("/setup/status", get(setup::api::get_status))
        .route("/setup/install", post(setup::api::install_dependency))
        .route("/setup/install-all", post(setup::api::install_all_missing))
        .route("/setup/init-databases", post(setup::api::init_databases))
        .route("/setup/start-services", post(setup::api::start_services))
        .route("/setup/complete", post(setup::api::complete_setup))
        // Model management API (always available)
        .route("/api/v1/models", get(models::list_models))
        .route("/api/v1/models/pull", post(models::pull_model))
        .route("/api/v1/models/pull/progress", get(models::pull_progress_sse))
        .route("/api/v1/models/:name", delete(models::delete_model))
        .route("/api/v1/models/recommended", get(models::recommended_models))
        // Reverse proxy: forward unmatched /api/v1/* to status-api
        .route("/api/v1/*rest", any(proxy_to_status_api))
        // Ollama proxy for native chat UI
        .route("/api/ollama/*rest", any(proxy_to_ollama))
        // Health
        .route("/health", get(health_handler))
        // Dynamic fallback — checks setup_complete flag on every request
        .fallback(dashboard::dynamic_fallback)
        .with_state(state);

    // CORS for local development
    let cors = tower_http::cors::CorsLayer::permissive();
    app.layer(cors)
}

/// Reverse proxy: forward /api/v1/* requests to the status-api with the API key injected.
async fn proxy_to_status_api(
    axum::extract::State(state): axum::extract::State<std::sync::Arc<AppState>>,
    axum::extract::Path(rest): axum::extract::Path<String>,
    req: axum::http::Request<axum::body::Body>,
) -> impl IntoResponse {
    let method_str = req.method().as_str().to_string();
    let query = req.uri().query().map(|q| format!("?{q}")).unwrap_or_default();
    let target = format!("{}/api/v1/{}{}", state.status_api_url, rest, query);

    let body_bytes = axum::body::to_bytes(req.into_body(), 1_048_576)
        .await
        .unwrap_or_default();

    let reqwest_method = reqwest::Method::from_bytes(method_str.as_bytes())
        .unwrap_or(reqwest::Method::GET);

    let mut builder = state.http_client.request(reqwest_method, &target)
        .header("X-Broodlink-Api-Key", &state.status_api_key)
        .header("Accept", "application/json");

    if !body_bytes.is_empty() {
        builder = builder
            .header("Content-Type", "application/json")
            .body(body_bytes);
    }

    match builder.timeout(std::time::Duration::from_secs(30)).send().await {
        Ok(resp) => {
            let status = axum::http::StatusCode::from_u16(resp.status().as_u16())
                .unwrap_or(axum::http::StatusCode::BAD_GATEWAY);
            let body = resp.bytes().await.unwrap_or_default();
            (status, [(axum::http::header::CONTENT_TYPE, "application/json")], body)
                .into_response()
        }
        Err(e) => {
            (
                axum::http::StatusCode::BAD_GATEWAY,
                axum::Json(serde_json::json!({ "error": format!("status-api unreachable: {e}") })),
            ).into_response()
        }
    }
}

/// Load status-api URL and API key from config + secrets.
async fn load_status_api_config(bl_dir: &std::path::Path) -> (String, String) {
    let config_path = bl_dir.join("config.toml");
    let config_str = std::fs::read_to_string(&config_path).unwrap_or_default();
    let config: toml::Value = config_str.parse().unwrap_or(toml::Value::Table(Default::default()));

    let port = config.get("status_api")
        .and_then(|s| s.get("port"))
        .and_then(|p| p.as_integer())
        .unwrap_or(3312);
    let url = format!("http://localhost:{port}");

    let api_key_name = config.get("status_api")
        .and_then(|s| s.get("api_key_name"))
        .and_then(|k| k.as_str())
        .unwrap_or("BROODLINK_STATUS_API_KEY");

    // Try to decrypt from SOPS
    let secrets_file = config.get("secrets")
        .and_then(|s| s.get("sops_file"))
        .and_then(|f| f.as_str())
        .unwrap_or("./secrets.enc.json");
    let secrets_path = bl_dir.join(secrets_file);

    let identity = config.get("secrets")
        .and_then(|s| s.get("age_identity"))
        .and_then(|f| f.as_str())
        .map(|p| p.replace('~', &std::env::var("HOME").or_else(|_| std::env::var("USERPROFILE")).unwrap_or_default()))
        .unwrap_or_else(|| format!("{}/.broodlink/age-identity", std::env::var("HOME").or_else(|_| std::env::var("USERPROFILE")).unwrap_or_default()));

    let secrets_path_str = secrets_path.to_string_lossy().to_string();
    let api_key = match broodlink_secrets::create_provider(
        "sops",
        Some(&secrets_path_str),
        Some(&identity),
        None, None,
    ) {
        Ok(provider) => {
            match provider.get(api_key_name).await {
                Ok(key) => {
                    tracing::info!("Loaded status-api key from secrets");
                    key
                }
                Err(e) => {
                    tracing::warn!(error = %e, "Could not load status-api key — proxy will fail auth");
                    String::new()
                }
            }
        }
        Err(e) => {
            tracing::warn!(error = %e, "Could not initialize secrets provider — proxy will fail auth");
            String::new()
        }
    };

    (url, api_key)
}

/// Reverse proxy: forward /api/ollama/* to local Ollama for native chat UI.
/// Streams responses back to the client for real-time token delivery.
async fn proxy_to_ollama(
    axum::extract::State(state): axum::extract::State<std::sync::Arc<AppState>>,
    axum::extract::Path(rest): axum::extract::Path<String>,
    req: axum::http::Request<axum::body::Body>,
) -> impl IntoResponse {
    let method_str = req.method().as_str().to_string();
    let target = format!("http://localhost:11434/{rest}");

    let body_bytes = axum::body::to_bytes(req.into_body(), 1_048_576)
        .await
        .unwrap_or_default();

    let reqwest_method = reqwest::Method::from_bytes(method_str.as_bytes())
        .unwrap_or(reqwest::Method::GET);

    let mut builder = state.http_client.request(reqwest_method, &target);

    if !body_bytes.is_empty() {
        builder = builder
            .header("Content-Type", "application/json")
            .body(body_bytes);
    }

    // Use a long timeout for streaming chat responses
    match builder.timeout(std::time::Duration::from_secs(300)).send().await {
        Ok(resp) => {
            let status = axum::http::StatusCode::from_u16(resp.status().as_u16())
                .unwrap_or(axum::http::StatusCode::BAD_GATEWAY);
            let content_type = resp.headers().get("content-type")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("application/json")
                .to_string();
            let body = resp.bytes().await.unwrap_or_default();
            (status, [(axum::http::header::CONTENT_TYPE, content_type)], body)
                .into_response()
        }
        Err(e) => {
            (
                axum::http::StatusCode::BAD_GATEWAY,
                axum::Json(serde_json::json!({ "error": format!("Ollama unreachable: {e}") })),
            ).into_response()
        }
    }
}

async fn health_handler() -> axum::Json<serde_json::Value> {
    axum::Json(serde_json::json!({
        "status": "ok",
        "version": env!("CARGO_PKG_VERSION"),
    }))
}

fn broodlink_dir() -> std::path::PathBuf {
    std::env::var("BROODLINK_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| {
            dirs_or_home().join("broodlink")
        })
}

fn dirs_or_home() -> std::path::PathBuf {
    std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| std::path::PathBuf::from("."))
}

fn open_browser(url: &str) -> Result<()> {
    #[cfg(target_os = "macos")]
    std::process::Command::new("open").arg(url).spawn()?;
    #[cfg(target_os = "linux")]
    std::process::Command::new("xdg-open").arg(url).spawn()?;
    #[cfg(target_os = "windows")]
    std::process::Command::new("cmd").args(["/C", "start", "", url]).spawn()?;
    Ok(())
}

async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c().await.expect("ctrl+c handler");
    };

    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("SIGTERM handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = ctrl_c => {},
        () = terminate => {},
    }
}
