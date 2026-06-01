/*
 * Broodlink workspace-api — Hardware fit (Cookbook).
 * Ported from the workspace app hwfit_routes / services/hwfit (llmfit-derived).
 * FULLY native: detects this machine's RAM/CPU/GPU and scores a model catalog
 * against it. No external deps beyond system probes (sysinfo + nvidia-smi/sysctl).
 *
 * Model download / vLLM·llama.cpp serving is out of scope (that's the
 * external-runtime half of Cookbook) and is not wired here.
 */

use axum::extract::Query;
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};
use sysinfo::System;

// ---------------------------------------------------------------------------
// Hardware detection
// ---------------------------------------------------------------------------

struct SystemInfo {
    total_ram_gb: f64,
    available_ram_gb: f64,
    cpu_cores: usize,
    cpu_name: String,
    has_gpu: bool,
    gpu_name: Option<String>,
    gpu_vram_gb: f64,
    backend: String,
    unified_memory: bool,
}

fn detect_system() -> SystemInfo {
    let mut sys = System::new_all();
    sys.refresh_memory();
    sys.refresh_cpu();
    let total_ram_gb = sys.total_memory() as f64 / 1e9;
    let available_ram_gb = sys.available_memory() as f64 / 1e9;
    let cpu_cores = sys.cpus().len();
    let cpu_name = sys
        .cpus()
        .first()
        .map(|c| c.brand().trim().to_string())
        .unwrap_or_default();

    // GPU: try NVIDIA first, then Apple Silicon (unified Metal), else CPU.
    let mut info = SystemInfo {
        total_ram_gb,
        available_ram_gb,
        cpu_cores,
        cpu_name: cpu_name.clone(),
        has_gpu: false,
        gpu_name: None,
        gpu_vram_gb: 0.0,
        backend: if cfg!(target_arch = "aarch64") {
            "cpu_arm".into()
        } else {
            "cpu_x86".into()
        },
        unified_memory: false,
    };

    if let Some((name, vram)) = detect_nvidia() {
        info.has_gpu = true;
        info.gpu_name = Some(name);
        info.gpu_vram_gb = vram;
        info.backend = "cuda".into();
    } else if cfg!(target_os = "macos") && cpu_name.contains("Apple") {
        // Apple Silicon: unified memory, Metal backend; usable VRAM ≈ most of RAM.
        info.has_gpu = true;
        info.gpu_name = Some(cpu_name.clone());
        info.gpu_vram_gb = (total_ram_gb * 0.75).round();
        info.backend = "metal".into();
        info.unified_memory = true;
    }
    info
}

fn detect_nvidia() -> Option<(String, f64)> {
    let out = std::process::Command::new("nvidia-smi")
        .args([
            "--query-gpu=name,memory.total",
            "--format=csv,noheader,nounits",
        ])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let line = text.lines().next()?;
    let mut parts = line.split(',');
    let name = parts.next()?.trim().to_string();
    let mb: f64 = parts.next()?.trim().parse().ok()?;
    Some((name, mb / 1024.0))
}

fn system_json(s: &SystemInfo) -> Value {
    json!({
        "total_ram_gb": (s.total_ram_gb * 10.0).round() / 10.0,
        "available_ram_gb": (s.available_ram_gb * 10.0).round() / 10.0,
        "cpu_cores": s.cpu_cores,
        "cpu_name": s.cpu_name,
        "has_gpu": s.has_gpu,
        "gpu_name": s.gpu_name,
        "gpu_vram_gb": (s.gpu_vram_gb * 10.0).round() / 10.0,
        "gpu_count": if s.has_gpu { 1 } else { 0 },
        "backend": s.backend,
        "unified_memory": s.unified_memory,
        "gpu_error": Value::Null,
    })
}

// ---------------------------------------------------------------------------
// Model catalog + fit scoring
// ---------------------------------------------------------------------------

struct CatModel {
    name: &'static str,
    provider: &'static str,
    params_b: f64,
    use_case: &'static str,
    context: u32,
}

const CATALOG: &[CatModel] = &[
    CatModel {
        name: "Qwen2.5 0.5B",
        provider: "Qwen",
        params_b: 0.5,
        use_case: "chat",
        context: 32768,
    },
    CatModel {
        name: "Llama 3.2 1B",
        provider: "Meta",
        params_b: 1.0,
        use_case: "chat",
        context: 131072,
    },
    CatModel {
        name: "Llama 3.2 3B",
        provider: "Meta",
        params_b: 3.0,
        use_case: "general",
        context: 131072,
    },
    CatModel {
        name: "Qwen2.5 7B",
        provider: "Qwen",
        params_b: 7.0,
        use_case: "general",
        context: 32768,
    },
    CatModel {
        name: "Qwen2.5 Coder 7B",
        provider: "Qwen",
        params_b: 7.0,
        use_case: "coding",
        context: 32768,
    },
    CatModel {
        name: "Llama 3.1 8B",
        provider: "Meta",
        params_b: 8.0,
        use_case: "general",
        context: 131072,
    },
    CatModel {
        name: "Gemma 2 9B",
        provider: "Google",
        params_b: 9.0,
        use_case: "chat",
        context: 8192,
    },
    CatModel {
        name: "Phi-4 14B",
        provider: "Microsoft",
        params_b: 14.0,
        use_case: "reasoning",
        context: 16384,
    },
    CatModel {
        name: "Qwen2.5 32B",
        provider: "Qwen",
        params_b: 32.0,
        use_case: "reasoning",
        context: 32768,
    },
    CatModel {
        name: "Llama 3.3 70B",
        provider: "Meta",
        params_b: 70.0,
        use_case: "general",
        context: 131072,
    },
    CatModel {
        name: "Mistral Large 123B",
        provider: "Mistral",
        params_b: 123.0,
        use_case: "reasoning",
        context: 32768,
    },
];

fn quant_bpp(quant: &str) -> f64 {
    match quant {
        "Q4_K_M" => 0.55,
        "Q6_K" => 0.80,
        "Q8_0" => 1.06,
        "FP16" => 2.0,
        _ => 0.55,
    }
}

fn estimate_memory(params_b: f64, quant: &str, context: u32) -> f64 {
    params_b * quant_bpp(quant) + 0.000_008 * params_b * context as f64 + 0.5
}

fn quality_score(params_b: f64, name: &str) -> f64 {
    let base: f64 = if params_b < 1.0 {
        25.0
    } else if params_b < 4.0 {
        45.0
    } else if params_b < 8.0 {
        60.0
    } else if params_b < 12.0 {
        72.0
    } else if params_b < 25.0 {
        82.0
    } else if params_b < 50.0 {
        89.0
    } else {
        95.0
    };
    let bonus = if ["qwen", "deepseek", "llama", "phi"]
        .iter()
        .any(|k| name.to_lowercase().contains(k))
    {
        3.0
    } else {
        0.0
    };
    (base + bonus).min(100.0)
}

fn fit_score(required: f64, budget: f64) -> (f64, &'static str, &'static str) {
    if budget <= 0.0 {
        return (0.0, "too_tight", "no_fit");
    }
    let ratio = required / budget;
    let score = if ratio <= 0.5 {
        60.0 + (ratio / 0.5) * 40.0
    } else if ratio <= 0.8 {
        100.0
    } else if ratio <= 0.9 {
        70.0
    } else if ratio <= 1.0 {
        50.0
    } else {
        20.0
    };
    let level = if ratio <= 0.8 {
        "perfect"
    } else if ratio <= 0.9 {
        "good"
    } else if ratio <= 1.0 {
        "marginal"
    } else {
        "too_tight"
    };
    let mode = if ratio <= 1.0 { "gpu" } else { "no_fit" };
    (score, level, mode)
}

#[derive(Deserialize)]
pub struct ModelsQuery {
    use_case: Option<String>,
    #[serde(default = "default_sort")]
    sort: String,
    #[serde(default = "default_limit")]
    limit: usize,
    search: Option<String>,
    #[serde(default = "default_quant")]
    quant: String,
}
fn default_sort() -> String {
    "score".into()
}
fn default_limit() -> usize {
    50
}
fn default_quant() -> String {
    "Q4_K_M".into()
}

pub async fn system_endpoint() -> Json<Value> {
    Json(system_json(&detect_system()))
}

pub async fn models(Query(q): Query<ModelsQuery>) -> Json<Value> {
    let sys = detect_system();
    // Memory budget: GPU VRAM if present (else system RAM, leaving headroom).
    let budget = if sys.has_gpu && !sys.unified_memory {
        sys.gpu_vram_gb
    } else if sys.unified_memory {
        sys.gpu_vram_gb
    } else {
        (sys.total_ram_gb - 2.0).max(0.0)
    };

    let mut out: Vec<Value> = CATALOG
        .iter()
        .filter(|m| q.use_case.as_deref().map(|u| u == "general" || u == m.use_case).unwrap_or(true))
        .filter(|m| q.search.as_deref().map(|s| {
            let s = s.to_lowercase();
            m.name.to_lowercase().contains(&s) || m.provider.to_lowercase().contains(&s)
        }).unwrap_or(true))
        .map(|m| {
            let ctx = m.context.min(8192); // assume 8k working context for sizing
            let required = estimate_memory(m.params_b, &q.quant, ctx);
            let (fscore, level, mode) = fit_score(required, budget);
            let quality = quality_score(m.params_b, m.name);
            // crude throughput estimate
            let speed = if mode == "gpu" { (budget / required.max(0.1) * 25.0).min(120.0) } else { 0.0 };
            let context_score = 100.0;
            let score = quality * 0.45 + speed.min(100.0) * 0.30 + fscore * 0.15 + context_score * 0.10;
            json!({
                "name": m.name,
                "provider": m.provider,
                "params_b": m.params_b,
                "parameter_count": format!("{}B", m.params_b),
                "is_moe": false,
                "use_case": m.use_case,
                "fit_level": level,
                "run_mode": mode,
                "quant": q.quant,
                "context": m.context,
                "context_length": m.context,
                "required_gb": (required * 10.0).round() / 10.0,
                "speed_tps": (speed * 10.0).round() / 10.0,
                "score": (score * 10.0).round() / 10.0,
                "scores": { "quality": quality, "speed": speed.min(100.0), "fit": fscore, "context": context_score },
                "gguf_sources": [],
                "is_image_gen": false,
            })
        })
        .collect();

    out.sort_by(|a, b| {
        let key = |v: &Value| match q.sort.as_str() {
            "vram" => -v["required_gb"].as_f64().unwrap_or(0.0),
            "params" => v["params_b"].as_f64().unwrap_or(0.0),
            "speed" => v["speed_tps"].as_f64().unwrap_or(0.0),
            "context" => v["context"].as_f64().unwrap_or(0.0),
            _ => v["score"].as_f64().unwrap_or(0.0),
        };
        key(b)
            .partial_cmp(&key(a))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    out.truncate(q.limit);

    Json(json!({ "system": system_json(&sys), "models": out }))
}

pub async fn image_models() -> Json<Value> {
    Json(json!({ "system": system_json(&detect_system()), "models": [] }))
}
