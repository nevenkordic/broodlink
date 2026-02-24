// Broodlink — Multi-agent AI orchestration
// Copyright (C) 2025–2026 Neven Kordic <neven@broodlink.ai>
// SPDX-License-Identifier: AGPL-3.0-or-later
//
//! Shared formula types and bidirectional TOML/JSONB conversion.
//!
//! System formulas live in `.beads/formulas/*.formula.toml` (git-tracked).
//! User formulas auto-persist to `.beads/formulas/custom/*.formula.toml`.
//! Postgres `formula_registry` is the runtime store.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::Path;
use tracing::{info, warn};

// ---------------------------------------------------------------------------
// Core types (moved from coordinator, now shared across services)
// ---------------------------------------------------------------------------

/// Top-level formula file structure — deserializes from both TOML and JSONB.
#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct FormulaFile {
    pub formula: FormulaMetadata,
    #[serde(default)]
    pub steps: Vec<FormulaStep>,
    #[serde(default)]
    pub on_failure: Option<FormulaStep>,
    /// Top-level parameters (JSONB registry format uses an array here).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parameters: Option<serde_json::Value>,
}

#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct FormulaMetadata {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Parameters in TOML dict form: `{ param_name = { type, required, ... } }`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parameters: Option<BTreeMap<String, FormulaParam>>,
}

#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct FormulaStep {
    pub name: String,
    pub agent_role: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<String>>,
    pub prompt: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<FormulaInput>,
    pub output: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub when: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retries: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backoff: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_seconds: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<u32>,
}

/// A step's input can be a single key or multiple keys.
#[derive(Deserialize, Serialize, Debug, Clone)]
#[serde(untagged)]
pub enum FormulaInput {
    Single(String),
    Multiple(Vec<String>),
}

/// Parameter definition in TOML dict form.
#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct FormulaParam {
    #[serde(rename = "type", default = "default_param_type")]
    pub param_type: String,
    #[serde(default)]
    pub required: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<serde_json::Value>,
}

fn default_param_type() -> String {
    "string".to_string()
}

/// Metadata needed when persisting a formula to TOML (not part of the definition JSONB).
pub struct FormulaTomlMeta<'a> {
    pub display_name: &'a str,
    pub description: &'a str,
}

// ---------------------------------------------------------------------------
// Hashing
// ---------------------------------------------------------------------------

/// SHA-256 hash of the canonical JSON representation of a definition.
/// Deterministic because serde_json preserves insertion order and we
/// serialize the same Value object.
pub fn definition_hash(def: &serde_json::Value) -> String {
    let canonical = serde_json::to_string(def).unwrap_or_default();
    let mut hasher = Sha256::new();
    hasher.update(canonical.as_bytes());
    format!("{:x}", hasher.finalize())
}

// ---------------------------------------------------------------------------
// TOML ↔ JSONB parameter conversion
// ---------------------------------------------------------------------------

/// Convert JSONB parameters array `[{name, type, ...}]` to TOML dict form.
pub fn jsonb_params_to_toml(params: &serde_json::Value) -> BTreeMap<String, FormulaParam> {
    let mut map = BTreeMap::new();
    if let Some(arr) = params.as_array() {
        for item in arr {
            let name = item
                .get("name")
                .and_then(|n| n.as_str())
                .unwrap_or("unknown");
            let param = FormulaParam {
                param_type: item
                    .get("type")
                    .and_then(|t| t.as_str())
                    .unwrap_or("string")
                    .to_string(),
                required: item
                    .get("required")
                    .and_then(|r| r.as_bool())
                    .unwrap_or(false),
                description: item
                    .get("description")
                    .and_then(|d| d.as_str())
                    .map(String::from),
                default: item.get("default").cloned(),
            };
            map.insert(name.to_string(), param);
        }
    }
    map
}

/// Convert TOML parameter dict to JSONB array form `[{name, type, ...}]`.
pub fn toml_params_to_jsonb(params: &BTreeMap<String, FormulaParam>) -> serde_json::Value {
    let arr: Vec<serde_json::Value> = params
        .iter()
        .map(|(name, p)| {
            let mut obj = serde_json::json!({
                "name": name,
                "type": p.param_type,
                "required": p.required,
            });
            if let Some(ref desc) = p.description {
                obj["description"] = serde_json::json!(desc);
            }
            if let Some(ref def) = p.default {
                obj["default"] = def.clone();
            }
            obj
        })
        .collect();
    serde_json::Value::Array(arr)
}

// ---------------------------------------------------------------------------
// FormulaFile ↔ JSONB definition conversion
// ---------------------------------------------------------------------------

/// Convert a FormulaFile (parsed from TOML) into the JSONB definition format
/// used by the Postgres `formula_registry.definition` column.
///
/// Key transformation: `formula.parameters` (dict) → top-level `parameters` (array).
pub fn formula_to_jsonb(f: &FormulaFile) -> serde_json::Value {
    let mut def = serde_json::to_value(f).unwrap_or(serde_json::json!({}));

    // Transform parameters from TOML dict to JSONB array
    if let Some(params_dict) = f.formula.parameters.as_ref() {
        let params_array = toml_params_to_jsonb(params_dict);
        def["parameters"] = params_array;
    }

    // Remove the nested formula.parameters (it's now at top level)
    if let Some(formula_obj) = def.get_mut("formula").and_then(|f| f.as_object_mut()) {
        formula_obj.remove("parameters");
    }

    def
}

// ---------------------------------------------------------------------------
// Load formulas from disk
// ---------------------------------------------------------------------------

/// Result of loading a single formula from TOML.
pub struct LoadedFormula {
    pub name: String,
    pub formula: FormulaFile,
    pub definition: serde_json::Value,
    pub hash: String,
}

/// Read all `*.formula.toml` files from a directory (non-recursive, skips `custom/` subdir).
pub fn load_toml_formulas(dir: &str) -> Vec<LoadedFormula> {
    let path = Path::new(dir);
    if !path.is_dir() {
        warn!(dir = dir, "formulas directory not found");
        return Vec::new();
    }

    let entries = match std::fs::read_dir(path) {
        Ok(e) => e,
        Err(e) => {
            warn!(dir = dir, error = %e, "failed to read formulas directory");
            return Vec::new();
        }
    };

    let mut formulas = Vec::new();
    for entry in entries.flatten() {
        let entry_path = entry.path();

        // Skip directories (including custom/)
        if entry_path.is_dir() {
            continue;
        }

        let name_os = entry.file_name();
        let name_str = name_os.to_string_lossy();
        if !name_str.ends_with(".formula.toml") {
            continue;
        }

        let formula_name = name_str.trim_end_matches(".formula.toml").to_string();

        match load_single_toml(&entry_path) {
            Ok((formula, definition, hash)) => {
                formulas.push(LoadedFormula {
                    name: formula_name,
                    formula,
                    definition,
                    hash,
                });
            }
            Err(e) => {
                warn!(
                    file = %entry_path.display(),
                    error = %e,
                    "failed to load formula TOML"
                );
            }
        }
    }

    formulas
}

/// Read all `*.formula.toml` files from the custom formulas directory.
pub fn load_custom_formulas(custom_dir: &str) -> Vec<LoadedFormula> {
    let path = Path::new(custom_dir);
    if !path.is_dir() {
        return Vec::new();
    }

    let entries = match std::fs::read_dir(path) {
        Ok(e) => e,
        Err(e) => {
            warn!(dir = custom_dir, error = %e, "failed to read custom formulas directory");
            return Vec::new();
        }
    };

    let mut formulas = Vec::new();
    for entry in entries.flatten() {
        let entry_path = entry.path();
        if entry_path.is_dir() {
            continue;
        }

        let name_os = entry.file_name();
        let name_str = name_os.to_string_lossy();
        if !name_str.ends_with(".formula.toml") {
            continue;
        }

        let formula_name = name_str.trim_end_matches(".formula.toml").to_string();

        match load_single_toml(&entry_path) {
            Ok((formula, definition, hash)) => {
                formulas.push(LoadedFormula {
                    name: formula_name,
                    formula,
                    definition,
                    hash,
                });
            }
            Err(e) => {
                warn!(
                    file = %entry_path.display(),
                    error = %e,
                    "skipping malformed custom formula"
                );
            }
        }
    }

    formulas
}

fn load_single_toml(
    path: &Path,
) -> Result<(FormulaFile, serde_json::Value, String), Box<dyn std::error::Error>> {
    let content = std::fs::read_to_string(path)?;
    let formula: FormulaFile = toml::from_str(&content)?;
    let definition = formula_to_jsonb(&formula);
    let hash = definition_hash(&definition);
    Ok((formula, definition, hash))
}

// ---------------------------------------------------------------------------
// Persist formula to disk (JSONB → TOML)
// ---------------------------------------------------------------------------

/// Convert a JSONB definition back to a TOML-formatted FormulaFile and write it to disk.
///
/// Uses atomic write (write to `.tmp` then rename) to prevent partial writes.
/// Creates the target directory if it doesn't exist.
pub fn persist_formula_toml(
    dir: &str,
    name: &str,
    definition: &serde_json::Value,
    meta: &FormulaTomlMeta<'_>,
) -> Result<(), Box<dyn std::error::Error>> {
    // Validate name to prevent path traversal
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(format!("invalid formula name: {name}").into());
    }

    // Build FormulaFile from JSONB definition
    let formula_file = jsonb_to_formula_file(name, definition, meta)?;

    let toml_str = toml::to_string_pretty(&formula_file)?;

    let header = "# Broodlink — Multi-agent AI orchestration\n\
         # Copyright (C) 2025–2026 Neven Kordic <neven@broodlink.ai>\n\
         # SPDX-License-Identifier: AGPL-3.0-or-later\n\
         #\n\
         # Auto-generated from formula registry. Do not edit directly.\n\n"
        .to_string();

    let full_content = format!("{header}{toml_str}");

    // Ensure directory exists
    let dir_path = Path::new(dir);
    std::fs::create_dir_all(dir_path)?;

    // Atomic write: tmp file then rename
    let target = dir_path.join(format!("{name}.formula.toml"));
    let tmp = dir_path.join(format!(".{name}.formula.toml.tmp"));
    std::fs::write(&tmp, full_content)?;
    std::fs::rename(&tmp, &target)?;

    info!(formula = name, path = %target.display(), "formula persisted to disk");
    Ok(())
}

/// Convert a JSONB definition into a FormulaFile suitable for TOML serialization.
fn jsonb_to_formula_file(
    name: &str,
    definition: &serde_json::Value,
    meta: &FormulaTomlMeta<'_>,
) -> Result<FormulaFile, Box<dyn std::error::Error>> {
    // Extract formula metadata
    let formula_section = definition.get("formula");
    let version = formula_section
        .and_then(|f| f.get("version"))
        .and_then(|v| v.as_str())
        .map(String::from);
    let description = if !meta.description.is_empty() {
        Some(meta.description.to_string())
    } else {
        formula_section
            .and_then(|f| f.get("description"))
            .and_then(|d| d.as_str())
            .map(String::from)
    };

    // Convert parameters from JSONB array to TOML dict
    let parameters = definition
        .get("parameters")
        .filter(|p| p.is_array() && !p.as_array().is_none_or(|a| a.is_empty()))
        .map(jsonb_params_to_toml);

    // Parse steps
    let steps: Vec<FormulaStep> = definition
        .get("steps")
        .and_then(|s| serde_json::from_value(s.clone()).ok())
        .unwrap_or_default();

    // Parse on_failure
    let on_failure: Option<FormulaStep> = definition.get("on_failure").and_then(|f| {
        if f.is_null() {
            None
        } else {
            serde_json::from_value(f.clone()).ok()
        }
    });

    Ok(FormulaFile {
        formula: FormulaMetadata {
            name: name.to_string(),
            version,
            description,
            parameters,
        },
        steps,
        on_failure,
        parameters: None, // Top-level parameters not used in TOML form
    })
}

// ---------------------------------------------------------------------------
// Utility
// ---------------------------------------------------------------------------

/// Title-case a slug: "build-feature" → "Build Feature".
pub fn title_case(slug: &str) -> String {
    slug.split('-')
        .map(|word| {
            let mut chars = word.chars();
            match chars.next() {
                Some(c) => format!("{}{}", c.to_uppercase(), chars.as_str()),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Validate a formula name: alphanumeric, hyphens, and underscores only.
pub fn validate_formula_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    const RESEARCH_TOML: &str = include_str!("../../../.beads/formulas/research.formula.toml");
    const BUILD_FEATURE_TOML: &str =
        include_str!("../../../.beads/formulas/build-feature.formula.toml");
    const DAILY_REVIEW_TOML: &str =
        include_str!("../../../.beads/formulas/daily-review.formula.toml");
    const KNOWLEDGE_GAP_TOML: &str =
        include_str!("../../../.beads/formulas/knowledge-gap.formula.toml");

    #[test]
    fn test_parse_research_toml() {
        let f: FormulaFile = toml::from_str(RESEARCH_TOML).expect("parse research");
        assert_eq!(f.formula.name, "research");
        assert_eq!(f.steps.len(), 3);
        assert!(f.formula.parameters.is_some());
        let params = f.formula.parameters.as_ref().unwrap();
        assert!(params.contains_key("topic"));
        assert_eq!(params["topic"].param_type, "string");
        assert!(params["topic"].required);
    }

    #[test]
    fn test_parse_build_feature_toml() {
        let f: FormulaFile = toml::from_str(BUILD_FEATURE_TOML).expect("parse build-feature");
        assert_eq!(f.formula.name, "build-feature");
        assert_eq!(f.steps.len(), 4);
        let params = f.formula.parameters.as_ref().unwrap();
        assert_eq!(params.len(), 2);
        assert!(params.contains_key("feature_name"));
        assert!(params.contains_key("feature_description"));
    }

    #[test]
    fn test_parse_daily_review_toml() {
        let f: FormulaFile = toml::from_str(DAILY_REVIEW_TOML).expect("parse daily-review");
        assert_eq!(f.formula.name, "daily-review");
        assert_eq!(f.steps.len(), 3);
        // input = ["metrics", "blockers"] on step 3
        if let Some(FormulaInput::Multiple(ref inputs)) = f.steps[2].input {
            assert_eq!(inputs.len(), 2);
        } else {
            panic!("expected multiple inputs on step 3");
        }
        // Parameter with default
        let params = f.formula.parameters.as_ref().unwrap();
        assert!(params["date"].default.is_some());
    }

    #[test]
    fn test_parse_knowledge_gap_toml() {
        let f: FormulaFile = toml::from_str(KNOWLEDGE_GAP_TOML).expect("parse knowledge-gap");
        assert_eq!(f.formula.name, "knowledge-gap");
        assert_eq!(f.steps.len(), 3);
        let params = f.formula.parameters.as_ref().unwrap();
        assert_eq!(params["max_gaps"].param_type, "integer");
        // default = 5 (integer)
        assert_eq!(params["max_gaps"].default, Some(serde_json::json!(5)));
    }

    #[test]
    fn test_toml_jsonb_roundtrip() {
        let f: FormulaFile = toml::from_str(RESEARCH_TOML).expect("parse");
        let jsonb = formula_to_jsonb(&f);

        // JSONB should have top-level parameters as array
        let params = jsonb.get("parameters").expect("params");
        assert!(params.is_array());
        let arr = params.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["name"], "topic");
        assert_eq!(arr[0]["type"], "string");

        // Convert back to FormulaFile
        let meta = FormulaTomlMeta {
            display_name: "Research",
            description: "Deep research workflow",
        };
        let roundtripped = jsonb_to_formula_file("research", &jsonb, &meta).expect("roundtrip");
        assert_eq!(roundtripped.formula.name, "research");
        assert_eq!(roundtripped.steps.len(), 3);
        let rt_params = roundtripped.formula.parameters.as_ref().unwrap();
        assert!(rt_params.contains_key("topic"));
        assert_eq!(rt_params["topic"].param_type, "string");
    }

    #[test]
    fn test_param_conversion_bidirectional() {
        let mut dict = BTreeMap::new();
        dict.insert(
            "topic".to_string(),
            FormulaParam {
                param_type: "string".to_string(),
                required: true,
                description: Some("The topic".to_string()),
                default: None,
            },
        );
        dict.insert(
            "max_results".to_string(),
            FormulaParam {
                param_type: "integer".to_string(),
                required: false,
                description: None,
                default: Some(serde_json::json!(10)),
            },
        );

        let jsonb_arr = toml_params_to_jsonb(&dict);
        let back = jsonb_params_to_toml(&jsonb_arr);

        assert_eq!(back.len(), 2);
        assert_eq!(back["topic"].param_type, "string");
        assert!(back["topic"].required);
        assert_eq!(back["topic"].description, Some("The topic".to_string()));
        assert_eq!(back["max_results"].param_type, "integer");
        assert!(!back["max_results"].required);
        assert_eq!(back["max_results"].default, Some(serde_json::json!(10)));
    }

    #[test]
    fn test_definition_hash_deterministic() {
        let f: FormulaFile = toml::from_str(RESEARCH_TOML).expect("parse");
        let def = formula_to_jsonb(&f);
        let h1 = definition_hash(&def);
        let h2 = definition_hash(&def);
        assert_eq!(h1, h2);
        assert_eq!(h1.len(), 64); // SHA-256 hex
    }

    #[test]
    fn test_definition_hash_differs() {
        let f1: FormulaFile = toml::from_str(RESEARCH_TOML).expect("parse 1");
        let f2: FormulaFile = toml::from_str(BUILD_FEATURE_TOML).expect("parse 2");
        let h1 = definition_hash(&formula_to_jsonb(&f1));
        let h2 = definition_hash(&formula_to_jsonb(&f2));
        assert_ne!(h1, h2);
    }

    #[test]
    fn test_persist_and_reload() {
        let f: FormulaFile = toml::from_str(RESEARCH_TOML).expect("parse");
        let jsonb = formula_to_jsonb(&f);

        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path().to_str().unwrap();

        let meta = FormulaTomlMeta {
            display_name: "Research",
            description: "Deep research workflow",
        };

        persist_formula_toml(dir, "research", &jsonb, &meta).expect("persist");

        // Read back
        let written =
            std::fs::read_to_string(tmp.path().join("research.formula.toml")).expect("read back");
        assert!(written.contains("[formula]"));
        assert!(written.contains("name = \"research\""));
        assert!(written.contains("[formula.parameters.topic]"));

        // Parse the written TOML
        // Skip the comment header
        let toml_body: &str = written
            .find("[formula]")
            .map(|i| &written[i..])
            .unwrap_or(&written);
        let reloaded: FormulaFile = toml::from_str(toml_body).expect("reload");
        assert_eq!(reloaded.formula.name, "research");
        assert_eq!(reloaded.steps.len(), 3);
    }

    #[test]
    fn test_title_case() {
        assert_eq!(title_case("build-feature"), "Build Feature");
        assert_eq!(title_case("research"), "Research");
        assert_eq!(title_case("daily-review"), "Daily Review");
        assert_eq!(title_case("knowledge-gap"), "Knowledge Gap");
    }

    #[test]
    fn test_validate_formula_name() {
        assert!(validate_formula_name("research"));
        assert!(validate_formula_name("build-feature"));
        assert!(validate_formula_name("my_formula_v2"));
        assert!(!validate_formula_name(""));
        assert!(!validate_formula_name("../etc/passwd"));
        assert!(!validate_formula_name("hello world"));
        assert!(!validate_formula_name("name;drop"));
    }

    #[test]
    fn test_load_system_formulas() {
        let formulas = load_toml_formulas("../../.beads/formulas");
        assert_eq!(formulas.len(), 4);
        let names: Vec<&str> = formulas.iter().map(|f| f.name.as_str()).collect();
        assert!(names.contains(&"research"));
        assert!(names.contains(&"build-feature"));
        assert!(names.contains(&"daily-review"));
        assert!(names.contains(&"knowledge-gap"));
    }
}
