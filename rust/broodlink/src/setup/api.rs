/*
 * Broodlink - Multi-agent AI orchestration system
 * Copyright (C) 2025–2026 Neven Kordic <neven@broodlink.ai>
 * SPDX-License-Identifier: AGPL-3.0-or-later
 */

//! Setup wizard HTTP API endpoints.
//! The web UI calls these to drive the setup process.

use axum::extract::State;
use axum::Json;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::AppState;

/// GET /setup/status — returns full system state
pub async fn get_status(
    State(_state): State<Arc<AppState>>,
) -> Json<super::SystemState> {
    Json(super::check_system_state().await)
}

#[derive(Deserialize)]
pub struct InstallRequest {
    pub name: String,
}

#[derive(Serialize)]
pub struct InstallResult {
    pub success: bool,
    pub message: String,
}

/// POST /setup/install — install a dependency via brew/apt
pub async fn install_dependency(
    State(_state): State<Arc<AppState>>,
    Json(req): Json<InstallRequest>,
) -> Json<InstallResult> {
    let result = match req.name.as_str() {
        "ollama" => install_ollama().await,
        "dolt" => install_brew_or_apt("dolt", "dolt").await,
        "postgres" => install_brew_or_apt("postgresql", "postgresql").await,
        "nats" => install_brew_or_apt("nats-server", "nats-server").await,
        "qdrant" => install_qdrant().await,
        other => Err(anyhow::anyhow!("unknown dependency: {other}")),
    };

    match result {
        Ok(msg) => Json(InstallResult { success: true, message: msg }),
        Err(e) => Json(InstallResult { success: false, message: e.to_string() }),
    }
}

/// POST /setup/init-databases — create DBs and run migrations
pub async fn init_databases(
    State(state): State<Arc<AppState>>,
) -> Json<InstallResult> {
    let (shell, script_name) = if cfg!(windows) {
        ("powershell", "scripts/db-setup.ps1")
    } else {
        ("bash", "scripts/db-setup.sh")
    };
    let script = state.broodlink_dir.join(script_name);
    if !script.exists() {
        return Json(InstallResult {
            success: false,
            message: format!("{script_name} not found"),
        });
    }

    let mut cmd = tokio::process::Command::new(shell);
    if cfg!(windows) {
        cmd.args(["-ExecutionPolicy", "Bypass", "-File"]);
    }
    let output = cmd
        .arg(&script)
        .current_dir(&state.broodlink_dir)
        .output()
        .await;

    match output {
        Ok(o) if o.status.success() => Json(InstallResult {
            success: true,
            message: "Databases created and migrations applied".to_string(),
        }),
        Ok(o) => Json(InstallResult {
            success: false,
            message: String::from_utf8_lossy(&o.stderr).to_string(),
        }),
        Err(e) => Json(InstallResult {
            success: false,
            message: e.to_string(),
        }),
    }
}

/// POST /setup/start-services — start external deps and Broodlink services
pub async fn start_services(
    State(state): State<Arc<AppState>>,
) -> Json<InstallResult> {
    match state.process_manager.start_all(&state.broodlink_dir).await {
        Ok(()) => Json(InstallResult {
            success: true,
            message: "All services started".to_string(),
        }),
        Err(e) => Json(InstallResult {
            success: false,
            message: e.to_string(),
        }),
    }
}

/// POST /setup/install-all — install all missing dependencies in sequence
pub async fn install_all_missing(
    State(_state): State<Arc<AppState>>,
) -> Json<InstallResult> {
    let state = super::check_system_state().await;
    let mut installed = Vec::new();
    let mut failed = Vec::new();

    let deps: Vec<(&str, bool)> = vec![
        ("ollama", state.ollama.installed),
        ("dolt", state.dolt.installed),
        ("postgres", state.postgres.installed),
        ("nats", state.nats.installed),
        ("qdrant", state.qdrant.installed),
    ];

    for (name, is_installed) in deps {
        if is_installed {
            continue;
        }
        let result = match name {
            "ollama" => install_ollama().await,
            "dolt" => install_brew_or_apt("dolt", "dolt").await,
            "postgres" => install_brew_or_apt("postgresql", "postgresql").await,
            "nats" => install_brew_or_apt("nats-server", "nats-server").await,
            "qdrant" => install_qdrant().await,
            _ => continue,
        };
        match result {
            Ok(_) => installed.push(name.to_string()),
            Err(e) => failed.push(format!("{name}: {e}")),
        }
    }

    if failed.is_empty() {
        Json(InstallResult {
            success: true,
            message: if installed.is_empty() {
                "All dependencies already installed".to_string()
            } else {
                format!("Installed: {}", installed.join(", "))
            },
        })
    } else {
        Json(InstallResult {
            success: false,
            message: format!("Failed: {}", failed.join("; ")),
        })
    }
}

/// POST /setup/complete — mark setup as done, unlock dashboard
pub async fn complete_setup(
    State(state): State<Arc<AppState>>,
) -> Json<InstallResult> {
    // Flip the flag — dashboard is now accessible
    state.setup_complete.store(true, std::sync::atomic::Ordering::Relaxed);
    tracing::info!("Setup complete — dashboard unlocked");
    Json(InstallResult {
        success: true,
        message: "Setup complete! Broodlink is ready.".to_string(),
    })
}

// -- Installation helpers --

async fn install_ollama() -> anyhow::Result<String> {
    #[cfg(target_os = "macos")]
    {
        // Check if Ollama.app exists
        if std::path::Path::new("/Applications/Ollama.app").exists() {
            return Ok("Ollama already installed".to_string());
        }
        // Try brew
        let output = tokio::process::Command::new("brew")
            .args(["install", "--cask", "ollama"])
            .output()
            .await?;
        if output.status.success() {
            return Ok("Installed Ollama via Homebrew".to_string());
        }
    }

    #[cfg(target_os = "linux")]
    {
        let output = tokio::process::Command::new("sh")
            .args(["-c", "curl -fsSL https://ollama.com/install.sh | sh"])
            .output()
            .await?;
        if output.status.success() {
            return Ok("Installed Ollama via install script".to_string());
        }
    }

    #[cfg(target_os = "windows")]
    {
        // Download and run the Ollama Windows installer silently
        let output = tokio::process::Command::new("powershell")
            .args([
                "-Command",
                "Invoke-WebRequest -Uri 'https://ollama.com/download/OllamaSetup.exe' -OutFile \"$env:TEMP\\OllamaSetup.exe\"; Start-Process \"$env:TEMP\\OllamaSetup.exe\" -ArgumentList '/SILENT' -Wait",
            ])
            .output()
            .await?;
        if output.status.success() {
            return Ok("Installed Ollama via Windows installer".to_string());
        }
    }

    anyhow::bail!("Could not install Ollama automatically. Please install from https://ollama.com")
}

async fn install_qdrant() -> anyhow::Result<String> {
    // Qdrant is not in Homebrew — install via official binary or Docker
    let install_dir = dirs::home_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join(".local/bin");

    // Ensure install dir exists
    tokio::fs::create_dir_all(&install_dir).await?;

    #[cfg(target_os = "macos")]
    {
        let arch = if cfg!(target_arch = "aarch64") { "aarch64" } else { "x86_64" };
        let url = format!(
            "https://github.com/qdrant/qdrant/releases/latest/download/qdrant-{arch}-apple-darwin.tar.gz"
        );
        let tar_path = install_dir.join("qdrant.tar.gz");

        // Download
        let output = tokio::process::Command::new("curl")
            .args(["-fSL", "-o"])
            .arg(&tar_path)
            .arg(&url)
            .output()
            .await?;
        if !output.status.success() {
            anyhow::bail!("Failed to download Qdrant: {}", String::from_utf8_lossy(&output.stderr));
        }

        // Extract
        let output = tokio::process::Command::new("tar")
            .args(["xzf"])
            .arg(&tar_path)
            .arg("-C")
            .arg(&install_dir)
            .output()
            .await?;
        let _ = tokio::fs::remove_file(&tar_path).await;
        if !output.status.success() {
            anyhow::bail!("Failed to extract Qdrant");
        }

        return Ok(format!("Installed Qdrant to {}", install_dir.display()));
    }

    #[cfg(target_os = "linux")]
    {
        let url = "https://github.com/qdrant/qdrant/releases/latest/download/qdrant-x86_64-unknown-linux-musl.tar.gz";
        let tar_path = install_dir.join("qdrant.tar.gz");

        let output = tokio::process::Command::new("curl")
            .args(["-fSL", "-o"])
            .arg(&tar_path)
            .arg(url)
            .output()
            .await?;
        if !output.status.success() {
            anyhow::bail!("Failed to download Qdrant: {}", String::from_utf8_lossy(&output.stderr));
        }

        let output = tokio::process::Command::new("tar")
            .args(["xzf"])
            .arg(&tar_path)
            .arg("-C")
            .arg(&install_dir)
            .output()
            .await?;
        let _ = tokio::fs::remove_file(&tar_path).await;
        if !output.status.success() {
            anyhow::bail!("Failed to extract Qdrant");
        }

        return Ok(format!("Installed Qdrant to {}", install_dir.display()));
    }

    #[cfg(target_os = "windows")]
    {
        let url = "https://github.com/qdrant/qdrant/releases/latest/download/qdrant-x86_64-pc-windows-msvc.zip";
        let zip_path = install_dir.join("qdrant.zip");

        let output = tokio::process::Command::new("powershell")
            .args([
                "-Command",
                &format!(
                    "Invoke-WebRequest -Uri '{}' -OutFile '{}'; Expand-Archive -Path '{}' -DestinationPath '{}' -Force",
                    url,
                    zip_path.display(),
                    zip_path.display(),
                    install_dir.display(),
                ),
            ])
            .output()
            .await?;
        let _ = tokio::fs::remove_file(&zip_path).await;
        if !output.status.success() {
            anyhow::bail!("Failed to download/extract Qdrant: {}", String::from_utf8_lossy(&output.stderr));
        }

        return Ok(format!("Installed Qdrant to {}", install_dir.display()));
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    anyhow::bail!("Automatic Qdrant installation not supported on this OS. Install from https://qdrant.tech")
}

async fn install_brew_or_apt(_brew_name: &str, _apt_name: &str) -> anyhow::Result<String> {
    #[cfg(target_os = "macos")]
    {
        let output = tokio::process::Command::new("brew")
            .args(["install", _brew_name])
            .output()
            .await?;
        if output.status.success() {
            return Ok(format!("Installed {_brew_name} via Homebrew"));
        }
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("brew install {_brew_name} failed: {stderr}");
    }

    #[cfg(target_os = "linux")]
    {
        let output = tokio::process::Command::new("sudo")
            .args(["apt-get", "install", "-y", _apt_name])
            .output()
            .await?;
        if output.status.success() {
            return Ok(format!("Installed {_apt_name} via apt"));
        }
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("apt install {_apt_name} failed: {stderr}");
    }

    #[cfg(target_os = "windows")]
    {
        let output = tokio::process::Command::new("choco")
            .args(["install", "-y", _brew_name])
            .output()
            .await?;
        if output.status.success() {
            return Ok(format!("Installed {_brew_name} via Chocolatey"));
        }
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("choco install {_brew_name} failed: {stderr}");
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    anyhow::bail!("Automatic installation not supported on this OS")
}
