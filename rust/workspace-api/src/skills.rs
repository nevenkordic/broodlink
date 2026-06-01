/*
 * Broodlink workspace-api — Skills.
 * Ported from the workspace app skills_routes (was file-based; here in Postgres).
 *
 * Implemented: skill CRUD, lightweight index, keyword search, SKILL.md get/set,
 * and built-in tool overrides. Each skill's full record is kept as a JSON blob
 * plus a few indexed columns.
 *
 * Stubbed (need the agent loop + LLM judge): /test, /audit-all and friends.
 */

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::HeaderMap;
use axum::Json;
use serde_json::{json, Value};

use crate::{owner_from, AppState, WsError};

const BUILTIN_TOOLS: &[&str] = &[
    "sessions",
    "memory",
    "documents",
    "notes",
    "calendar",
    "email",
    "tasks",
    "skills",
];

pub async fn ensure_skills_schema(pg: &sqlx::PgPool) -> Result<(), sqlx::Error> {
    sqlx::raw_sql(
        r#"
        CREATE TABLE IF NOT EXISTS ws_skills (
            id          TEXT PRIMARY KEY,
            owner       TEXT,
            name        TEXT NOT NULL,
            description TEXT NOT NULL DEFAULT '',
            category    TEXT NOT NULL DEFAULT 'general',
            status      TEXT NOT NULL DEFAULT 'draft',
            source      TEXT NOT NULL DEFAULT 'user',
            uses        INTEGER NOT NULL DEFAULT 0,
            data        TEXT NOT NULL,
            markdown    TEXT,
            created_at  TIMESTAMPTZ NOT NULL DEFAULT now()
        );
        CREATE INDEX IF NOT EXISTS ws_skills_owner_idx ON ws_skills(owner);

        CREATE TABLE IF NOT EXISTS ws_skill_overrides (
            owner TEXT NOT NULL,
            name  TEXT NOT NULL,
            text  TEXT NOT NULL,
            PRIMARY KEY (owner, name)
        );
        "#,
    )
    .execute(pg)
    .await?;
    Ok(())
}

fn slug(s: &str) -> String {
    let base: String = s
        .to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '-' })
        .collect();
    let trimmed = base.trim_matches('-').to_string();
    if trimmed.is_empty() {
        "skill".into()
    } else {
        trimmed
    }
}

/// Build a complete skill record from arbitrary input, filling defaults.
fn build_skill(owner: &str, mut input: Value) -> (String, Value) {
    let name = input["name"]
        .as_str()
        .filter(|s| !s.is_empty())
        .map(String::from)
        .unwrap_or_else(|| input["title"].as_str().unwrap_or("Skill").to_string());
    let id = slug(&name);
    let arr = |v: &Value, k: &str| {
        v.get(k)
            .cloned()
            .filter(|x| x.is_array())
            .unwrap_or(json!([]))
    };
    let skill = json!({
        "id": id,
        "name": name,
        "description": input["description"].as_str().unwrap_or(""),
        "category": input["category"].as_str().unwrap_or("general"),
        "tags": arr(&input, "tags"),
        "platforms": arr(&input, "platforms"),
        "requires_toolsets": arr(&input, "requires_toolsets"),
        "fallback_for_toolsets": arr(&input, "fallback_for_toolsets"),
        "status": input["status"].as_str().unwrap_or("draft"),
        "confidence": input["confidence"].as_f64().unwrap_or(0.8),
        "version": input["version"].as_str().unwrap_or("1.0.0"),
        "source": input["source"].as_str().unwrap_or("user"),
        "teacher_model": input.get("teacher_model").cloned().unwrap_or(Value::Null),
        "owner": owner,
        "when_to_use": input["when_to_use"].as_str().unwrap_or(""),
        "procedure": arr(&input, "procedure"),
        "pitfalls": arr(&input, "pitfalls"),
        "verification": arr(&input, "verification"),
        "body_extra": input.get("body_extra").cloned().unwrap_or(Value::Null),
        "uses": 0,
        "audited_at": Value::Null,
        "audit_verdict": Value::Null,
        "audit_by_teacher": false,
        "necessity": Value::Null,
    });
    input.take(); // drop original
    (id, skill)
}

async fn fetch_skill(state: &AppState, id: &str, owner: &str) -> Result<Value, WsError> {
    let row = sqlx::query_as::<_, (String, i32, Option<String>)>(
        "SELECT data, uses, owner FROM ws_skills WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(&state.pg)
    .await?
    .ok_or_else(|| WsError::NotFound("skill not found".into()))?;
    let (data, uses, row_owner) = row;
    if row_owner.as_deref().map(|o| o != owner).unwrap_or(false) {
        return Err(WsError::NotFound("skill not found".into()));
    }
    let mut v: Value = serde_json::from_str(&data).unwrap_or(json!({}));
    v["uses"] = json!(uses);
    Ok(v)
}

pub async fn list(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Json<Value>, WsError> {
    let owner = owner_from(&headers);
    let rows = sqlx::query_as::<_, (String, i32)>(
        "SELECT data, uses FROM ws_skills WHERE owner = $1 ORDER BY created_at DESC",
    )
    .bind(&owner)
    .fetch_all(&state.pg)
    .await?;
    let skills: Vec<Value> = rows
        .into_iter()
        .filter_map(|(d, uses)| {
            serde_json::from_str::<Value>(&d).ok().map(|mut v| {
                v["uses"] = json!(uses);
                v
            })
        })
        .collect();
    let count = skills.len();
    Ok(Json(json!({ "skills": skills, "count": count })))
}

pub async fn index(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Json<Value>, WsError> {
    let owner = owner_from(&headers);
    let rows = sqlx::query_as::<_, (String, String, String)>(
        "SELECT name, description, category FROM ws_skills WHERE owner = $1 ORDER BY name ASC",
    )
    .bind(&owner)
    .fetch_all(&state.pg)
    .await?;
    let index: Vec<Value> = rows
        .into_iter()
        .map(|(name, description, category)| json!({ "name": name, "description": description, "category": category }))
        .collect();
    let count = index.len();
    Ok(Json(json!({ "index": index, "count": count })))
}

pub async fn add(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(input): Json<Value>,
) -> Result<Json<Value>, WsError> {
    let owner = owner_from(&headers);
    let (id, skill) = build_skill(&owner, input);
    sqlx::query(
        "INSERT INTO ws_skills (id, owner, name, description, category, status, source, data) \
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8) \
         ON CONFLICT (id) DO UPDATE SET name=EXCLUDED.name, description=EXCLUDED.description, \
            category=EXCLUDED.category, status=EXCLUDED.status, data=EXCLUDED.data",
    )
    .bind(&id)
    .bind(&owner)
    .bind(skill["name"].as_str().unwrap_or(""))
    .bind(skill["description"].as_str().unwrap_or(""))
    .bind(skill["category"].as_str().unwrap_or("general"))
    .bind(skill["status"].as_str().unwrap_or("draft"))
    .bind(skill["source"].as_str().unwrap_or("user"))
    .bind(skill.to_string())
    .execute(&state.pg)
    .await?;
    Ok(Json(
        json!({ "ok": true, "deduped": false, "skill": skill }),
    ))
}

pub async fn get_one(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Value>, WsError> {
    let owner = owner_from(&headers);
    Ok(Json(fetch_skill(&state, &id, &owner).await?))
}

pub async fn get_markdown(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Value>, WsError> {
    let owner = owner_from(&headers);
    let skill = fetch_skill(&state, &id, &owner).await?;
    let md: Option<String> = sqlx::query_scalar("SELECT markdown FROM ws_skills WHERE id = $1")
        .bind(&id)
        .fetch_optional(&state.pg)
        .await?
        .flatten();
    let markdown = md.unwrap_or_else(|| synth_markdown(&skill));
    Ok(Json(json!({ "name": skill["name"], "markdown": markdown })))
}

fn synth_markdown(s: &Value) -> String {
    let mut md = format!(
        "# {}\n\n{}\n",
        s["name"].as_str().unwrap_or(""),
        s["description"].as_str().unwrap_or("")
    );
    if let Some(w) = s["when_to_use"].as_str() {
        if !w.is_empty() {
            md.push_str(&format!("\n## When to use\n{w}\n"));
        }
    }
    if let Some(steps) = s["procedure"].as_array() {
        if !steps.is_empty() {
            md.push_str("\n## Procedure\n");
            for (i, step) in steps.iter().enumerate() {
                md.push_str(&format!("{}. {}\n", i + 1, step.as_str().unwrap_or("")));
            }
        }
    }
    md
}

pub async fn set_markdown(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> Result<Json<Value>, WsError> {
    let owner = owner_from(&headers);
    let skill = fetch_skill(&state, &id, &owner).await?;
    let markdown = body["markdown"].as_str().unwrap_or("").to_string();
    sqlx::query("UPDATE ws_skills SET markdown = $2 WHERE id = $1 AND owner = $3")
        .bind(&id)
        .bind(&markdown)
        .bind(&owner)
        .execute(&state.pg)
        .await?;
    Ok(Json(json!({ "ok": true, "name": skill["name"] })))
}

pub async fn update(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(patch): Json<Value>,
) -> Result<Json<Value>, WsError> {
    let owner = owner_from(&headers);
    let mut skill = fetch_skill(&state, &id, &owner).await?;
    if let (Value::Object(dst), Value::Object(src)) = (&mut skill, &patch) {
        for (k, v) in src {
            if k != "id" && k != "owner" && k != "uses" {
                dst.insert(k.clone(), v.clone());
            }
        }
    }
    sqlx::query(
        "UPDATE ws_skills SET name=$2, description=$3, category=$4, status=$5, data=$6 WHERE id=$1 AND owner=$7",
    )
    .bind(&id)
    .bind(skill["name"].as_str().unwrap_or(""))
    .bind(skill["description"].as_str().unwrap_or(""))
    .bind(skill["category"].as_str().unwrap_or("general"))
    .bind(skill["status"].as_str().unwrap_or("draft"))
    .bind(skill.to_string())
    .bind(&owner)
    .execute(&state.pg)
    .await?;
    Ok(Json(json!({ "ok": true })))
}

pub async fn delete_one(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Value>, WsError> {
    let owner = owner_from(&headers);
    sqlx::query("DELETE FROM ws_skills WHERE id = $1 AND owner = $2")
        .bind(&id)
        .bind(&owner)
        .execute(&state.pg)
        .await?;
    Ok(Json(json!({ "ok": true })))
}

pub async fn search(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Result<Json<Value>, WsError> {
    let owner = owner_from(&headers);
    let query = body["query"].as_str().unwrap_or("").to_string();
    let pat = format!("%{}%", query.replace('%', "\\%"));
    let rows = sqlx::query_as::<_, (String, i32)>(
        "SELECT data, uses FROM ws_skills WHERE owner = $1 AND (name ILIKE $2 OR description ILIKE $2) \
         ORDER BY uses DESC LIMIT 10",
    )
    .bind(&owner)
    .bind(&pat)
    .fetch_all(&state.pg)
    .await?;
    let skills: Vec<Value> = rows
        .into_iter()
        .filter_map(|(d, uses)| {
            serde_json::from_str::<Value>(&d).ok().map(|mut v| {
                v["uses"] = json!(uses);
                v
            })
        })
        .collect();
    let count = skills.len();
    Ok(Json(
        json!({ "skills": skills, "query": query, "count": count }),
    ))
}

// --- built-in tool overrides ----------------------------------------------

pub async fn builtin_list(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Json<Value>, WsError> {
    let owner = owner_from(&headers);
    let overridden: Vec<String> =
        sqlx::query_scalar("SELECT name FROM ws_skill_overrides WHERE owner = $1")
            .bind(&owner)
            .fetch_all(&state.pg)
            .await?;
    let builtin: Vec<Value> = BUILTIN_TOOLS
        .iter()
        .map(|n| json!({ "name": n, "description": "", "is_overridden": overridden.iter().any(|o| o == n) }))
        .collect();
    let count = builtin.len();
    Ok(Json(json!({ "builtin": builtin, "count": count })))
}

pub async fn builtin_get(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(name): Path<String>,
) -> Result<Json<Value>, WsError> {
    let owner = owner_from(&headers);
    let text: Option<String> =
        sqlx::query_scalar("SELECT text FROM ws_skill_overrides WHERE owner = $1 AND name = $2")
            .bind(&owner)
            .bind(&name)
            .fetch_optional(&state.pg)
            .await?;
    Ok(Json(json!({
        "name": name, "text": text.clone().unwrap_or_default(),
        "default": "", "is_overridden": text.is_some()
    })))
}

pub async fn builtin_set(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(name): Path<String>,
    Json(body): Json<Value>,
) -> Result<Json<Value>, WsError> {
    let owner = owner_from(&headers);
    let text = body["text"].as_str().unwrap_or("").to_string();
    sqlx::query(
        "INSERT INTO ws_skill_overrides (owner, name, text) VALUES ($1,$2,$3) \
         ON CONFLICT (owner, name) DO UPDATE SET text = EXCLUDED.text",
    )
    .bind(&owner)
    .bind(&name)
    .bind(&text)
    .execute(&state.pg)
    .await?;
    Ok(Json(
        json!({ "ok": true, "name": name, "is_overridden": true }),
    ))
}

pub async fn builtin_delete(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(name): Path<String>,
) -> Result<Json<Value>, WsError> {
    let owner = owner_from(&headers);
    sqlx::query("DELETE FROM ws_skill_overrides WHERE owner = $1 AND name = $2")
        .bind(&owner)
        .bind(&name)
        .execute(&state.pg)
        .await?;
    Ok(Json(
        json!({ "ok": true, "name": name, "is_overridden": false }),
    ))
}

// --- LLM-backed (deferred) ------------------------------------------------

pub async fn llm_stub() -> Json<Value> {
    Json(
        json!({ "ok": false, "status": "none", "error": "skill testing/audit needs the agent loop (not yet ported)" }),
    )
}
