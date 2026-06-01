/*
 * Broodlink workspace-api — Contacts.
 * Ported from the workspace app contacts_routes. Implemented as a local
 * Postgres-backed address book (the original's JSON fallback). CardDAV sync is
 * stored-but-not-wired (network sync deferred, like the calendar CalDAV pull).
 */

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{owner_from, AppState, WsError};

pub async fn ensure_contacts_schema(pg: &sqlx::PgPool) -> Result<(), sqlx::Error> {
    sqlx::raw_sql(
        r#"
        CREATE TABLE IF NOT EXISTS ws_contacts (
            owner  TEXT NOT NULL,
            uid    TEXT NOT NULL,
            name   TEXT NOT NULL DEFAULT '',
            emails TEXT NOT NULL DEFAULT '[]',
            phones TEXT NOT NULL DEFAULT '[]',
            PRIMARY KEY (owner, uid)
        );
        CREATE TABLE IF NOT EXISTS ws_carddav_config (
            owner    TEXT PRIMARY KEY,
            url      TEXT NOT NULL DEFAULT '',
            username TEXT NOT NULL DEFAULT '',
            password TEXT NOT NULL DEFAULT ''
        );
        "#,
    )
    .execute(pg)
    .await?;
    Ok(())
}

fn arr(s: &str) -> Vec<String> {
    serde_json::from_str(s).unwrap_or_default()
}

fn contact_json(uid: &str, name: &str, emails: &str, phones: &str) -> Value {
    json!({ "uid": uid, "name": name, "emails": arr(emails), "phones": arr(phones) })
}

async fn all(
    state: &AppState,
    owner: &str,
) -> Result<Vec<(String, String, String, String)>, WsError> {
    Ok(sqlx::query_as::<_, (String, String, String, String)>(
        "SELECT uid, name, emails, phones FROM ws_contacts WHERE owner = $1 ORDER BY lower(name) ASC",
    )
    .bind(owner)
    .fetch_all(&state.pg)
    .await?)
}

async fn load_carddav(state: &AppState, owner: &str) -> Option<(String, String, String)> {
    let row = sqlx::query_as::<_, (String, String, String)>(
        "SELECT url, username, password FROM ws_carddav_config WHERE owner = $1",
    )
    .bind(owner)
    .fetch_optional(&state.pg)
    .await
    .ok()
    .flatten()?;
    let (url, user, pass) = row;
    if url.is_empty() {
        return None;
    }
    Some((url, user, state.cipher.decrypt(&pass)))
}

/// Parse vCards into (name, emails, phones). Handles FN/N, EMAIL*, TEL*.
pub fn parse_vcards(text: &str) -> Vec<(String, Vec<String>, Vec<String>)> {
    let mut out = Vec::new();
    let (mut name, mut emails, mut phones) = (String::new(), Vec::new(), Vec::new());
    for line in text.replace("\r\n", "\n").lines() {
        let l = line.trim();
        let up = l.to_uppercase();
        if up.starts_with("END:VCARD") {
            if !name.is_empty() || !emails.is_empty() {
                out.push((
                    std::mem::take(&mut name),
                    std::mem::take(&mut emails),
                    std::mem::take(&mut phones),
                ));
            }
        } else if let Some(v) = l.strip_prefix("FN:") {
            name = v.to_string();
        } else if up.starts_with("EMAIL") {
            if let Some(val) = l.split(':').next_back() {
                if !val.is_empty() {
                    emails.push(val.to_string());
                }
            }
        } else if up.starts_with("TEL") {
            if let Some(val) = l.split(':').next_back() {
                if !val.is_empty() {
                    phones.push(val.to_string());
                }
            }
        }
    }
    out
}

pub async fn list(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Json<Value>, WsError> {
    let owner = owner_from(&headers);

    // Prefer CardDAV when configured; fall back to the local table on error.
    if let Some((url, user, pass)) = load_carddav(&state, &owner).await {
        if let Ok(xml) =
            crate::webdav::report(&url, &user, &pass, "1", crate::webdav::ADDRESSBOOK_QUERY).await
        {
            let blocks = crate::webdav::extract_data_blocks(&xml, "address-data");
            let mut contacts = Vec::new();
            for block in &blocks {
                for (name, emails, phones) in parse_vcards(block) {
                    contacts.push(json!({
                        "uid": Uuid::new_v4().to_string(),
                        "name": name, "emails": emails, "phones": phones
                    }));
                }
            }
            let count = contacts.len();
            return Ok(Json(json!({ "contacts": contacts, "count": count })));
        }
    }

    let rows = all(&state, &owner).await?;
    let contacts: Vec<Value> = rows
        .iter()
        .map(|(u, n, e, p)| contact_json(u, n, e, p))
        .collect();
    let count = contacts.len();
    Ok(Json(json!({ "contacts": contacts, "count": count })))
}

#[derive(Deserialize)]
pub struct SearchQ {
    #[serde(default)]
    q: String,
}

pub async fn search(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(sq): Query<SearchQ>,
) -> Result<Json<Value>, WsError> {
    let owner = owner_from(&headers);
    if sq.q.trim().len() < 2 {
        return Ok(Json(json!({ "results": [] })));
    }
    let needle = sq.q.to_lowercase();
    let rows = all(&state, &owner).await?;
    let results: Vec<Value> = rows
        .iter()
        .filter(|(_, n, e, _)| {
            n.to_lowercase().contains(&needle) || e.to_lowercase().contains(&needle)
        })
        .take(10)
        .map(|(u, n, e, p)| contact_json(u, n, e, p))
        .collect();
    Ok(Json(json!({ "results": results })))
}

#[derive(Deserialize)]
pub struct AddBody {
    #[serde(default)]
    name: String,
    #[serde(default)]
    email: String,
}

pub async fn add(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(b): Json<AddBody>,
) -> Result<Json<Value>, WsError> {
    let owner = owner_from(&headers);
    if !b.email.is_empty() {
        let rows = all(&state, &owner).await?;
        if rows
            .iter()
            .any(|(_, _, e, _)| arr(e).iter().any(|x| x.eq_ignore_ascii_case(&b.email)))
        {
            return Ok(Json(
                json!({ "success": true, "message": "Already exists" }),
            ));
        }
    }
    let uid = Uuid::new_v4().to_string();
    let emails = if b.email.is_empty() {
        json!([])
    } else {
        json!([b.email])
    };
    sqlx::query("INSERT INTO ws_contacts (owner, uid, name, emails) VALUES ($1,$2,$3,$4)")
        .bind(&owner)
        .bind(&uid)
        .bind(&b.name)
        .bind(emails.to_string())
        .execute(&state.pg)
        .await?;
    Ok(Json(json!({ "success": true })))
}

#[derive(Deserialize)]
pub struct UpdateBody {
    name: Option<String>,
    emails: Option<Vec<String>>,
    phones: Option<Vec<String>>,
}

pub async fn update(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(uid): Path<String>,
    Json(b): Json<UpdateBody>,
) -> Result<Json<Value>, WsError> {
    let owner = owner_from(&headers);
    let res = sqlx::query(
        "UPDATE ws_contacts SET name = COALESCE($3, name), \
            emails = COALESCE($4, emails), phones = COALESCE($5, phones) \
         WHERE owner = $1 AND uid = $2",
    )
    .bind(&owner)
    .bind(&uid)
    .bind(&b.name)
    .bind(b.emails.map(|e| json!(e).to_string()))
    .bind(b.phones.map(|p| json!(p).to_string()))
    .execute(&state.pg)
    .await?;
    if res.rows_affected() == 0 {
        return Err(WsError::NotFound("contact not found".into()));
    }
    Ok(Json(json!({ "success": true })))
}

pub async fn delete_one(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(uid): Path<String>,
) -> Result<Json<Value>, WsError> {
    let owner = owner_from(&headers);
    sqlx::query("DELETE FROM ws_contacts WHERE owner = $1 AND uid = $2")
        .bind(&owner)
        .bind(&uid)
        .execute(&state.pg)
        .await?;
    Ok(Json(json!({ "success": true })))
}

pub async fn clear(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Json<Value>, WsError> {
    let owner = owner_from(&headers);
    sqlx::query("DELETE FROM ws_contacts WHERE owner = $1")
        .bind(&owner)
        .execute(&state.pg)
        .await?;
    Ok(Json(json!({ "success": true })))
}

// --- config (stored; CardDAV network sync deferred) -----------------------

pub async fn get_config(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Json<Value>, WsError> {
    let owner = owner_from(&headers);
    let row = sqlx::query_as::<_, (String, String, String)>(
        "SELECT url, username, password FROM ws_carddav_config WHERE owner = $1",
    )
    .bind(&owner)
    .fetch_optional(&state.pg)
    .await?;
    let (url, username, has_pw) = match row {
        Some((u, n, p)) => (u, n, !p.is_empty()),
        None => (String::new(), String::new(), false),
    };
    Ok(Json(
        json!({ "url": url, "username": username, "password": if has_pw { "***" } else { "" } }),
    ))
}

#[derive(Deserialize)]
pub struct ConfigBody {
    #[serde(default)]
    carddav_url: String,
    #[serde(default)]
    carddav_username: String,
    #[serde(default)]
    carddav_password: String,
}

pub async fn set_config(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(b): Json<ConfigBody>,
) -> Result<Json<Value>, WsError> {
    let owner = owner_from(&headers);
    let pw = if b.carddav_password.is_empty() {
        String::new()
    } else {
        state.cipher.encrypt(&b.carddav_password)
    };
    sqlx::query(
        "INSERT INTO ws_carddav_config (owner, url, username, password) VALUES ($1,$2,$3,$4) \
         ON CONFLICT (owner) DO UPDATE SET url = EXCLUDED.url, username = EXCLUDED.username, \
            password = CASE WHEN EXCLUDED.password = '' THEN ws_carddav_config.password ELSE EXCLUDED.password END",
    )
    .bind(&owner)
    .bind(&b.carddav_url)
    .bind(&b.carddav_username)
    .bind(&pw)
    .execute(&state.pg)
    .await?;
    Ok(Json(json!({ "success": true })))
}

// --- import / export ------------------------------------------------------

#[derive(Deserialize)]
pub struct ImportBody {
    #[serde(default)]
    vcf: Option<String>,
    #[serde(default)]
    csv: Option<String>,
}

pub async fn import(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(b): Json<ImportBody>,
) -> Result<Json<Value>, WsError> {
    let owner = owner_from(&headers);
    let mut parsed: Vec<(String, Vec<String>, Vec<String>)> = Vec::new();

    if let Some(vcf) = &b.vcf {
        let mut name = String::new();
        let mut emails = Vec::new();
        let mut phones = Vec::new();
        for line in vcf.lines() {
            let l = line.trim();
            if l.eq_ignore_ascii_case("END:VCARD") {
                if !name.is_empty() || !emails.is_empty() {
                    parsed.push((
                        std::mem::take(&mut name),
                        std::mem::take(&mut emails),
                        std::mem::take(&mut phones),
                    ));
                }
            } else if let Some(v) = l.strip_prefix("FN:") {
                name = v.to_string();
            } else if let Some(v) = l.to_uppercase().strip_prefix("EMAIL") {
                if let Some(val) = v.split(':').next_back() {
                    emails.push(val.to_string());
                }
            } else if l.to_uppercase().starts_with("TEL") {
                if let Some(val) = l.split(':').next_back() {
                    phones.push(val.to_string());
                }
            }
        }
    }
    if let Some(csv) = &b.csv {
        let mut lines = csv.lines();
        if let Some(header) = lines.next() {
            let cols: Vec<String> = header.split(',').map(|c| c.trim().to_lowercase()).collect();
            let idx = |names: &[&str]| cols.iter().position(|c| names.contains(&c.as_str()));
            let ni = idx(&["name", "full_name", "display_name"]);
            let ei = idx(&["email", "email_address"]);
            let pi = idx(&["phone", "tel"]);
            for line in lines {
                let f: Vec<&str> = line.split(',').collect();
                let g = |i: Option<usize>| {
                    i.and_then(|x| f.get(x))
                        .map(|s| s.trim().to_string())
                        .unwrap_or_default()
                };
                let name = g(ni);
                let email = g(ei);
                let phone = g(pi);
                if name.is_empty() && email.is_empty() {
                    continue;
                }
                parsed.push((
                    name,
                    if email.is_empty() {
                        vec![]
                    } else {
                        vec![email]
                    },
                    if phone.is_empty() {
                        vec![]
                    } else {
                        vec![phone]
                    },
                ));
            }
        }
    }

    let total = parsed.len();
    let mut imported = 0;
    let existing = all(&state, &owner).await?;
    for (name, emails, phones) in parsed {
        let dup = emails.iter().any(|em| {
            existing
                .iter()
                .any(|(_, _, e, _)| arr(e).iter().any(|x| x.eq_ignore_ascii_case(em)))
        });
        if dup {
            continue;
        }
        sqlx::query(
            "INSERT INTO ws_contacts (owner, uid, name, emails, phones) VALUES ($1,$2,$3,$4,$5)",
        )
        .bind(&owner)
        .bind(Uuid::new_v4().to_string())
        .bind(&name)
        .bind(json!(emails).to_string())
        .bind(json!(phones).to_string())
        .execute(&state.pg)
        .await?;
        imported += 1;
    }
    Ok(Json(
        json!({ "imported": imported, "failed": total - imported, "total": total, "success": true }),
    ))
}

#[derive(Deserialize)]
pub struct ExportQ {
    #[serde(default = "default_fmt")]
    format: String,
}
fn default_fmt() -> String {
    "vcf".into()
}

pub async fn export(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(q): Query<ExportQ>,
) -> Result<Response, WsError> {
    let owner = owner_from(&headers);
    let rows = all(&state, &owner).await?;
    if q.format == "csv" {
        let mut out = String::from("name,email,phone\n");
        for (_, n, e, p) in &rows {
            let emails = arr(e);
            let phones = arr(p);
            let email = emails.first().cloned().unwrap_or_default();
            let phone = phones.first().cloned().unwrap_or_default();
            out.push_str(&format!("{},{},{}\n", n.replace(',', " "), email, phone));
        }
        Ok(([(header::CONTENT_TYPE, "text/csv; charset=utf-8")], out).into_response())
    } else {
        let mut out = String::new();
        for (uid, n, e, p) in &rows {
            out.push_str("BEGIN:VCARD\r\nVERSION:4.0\r\n");
            out.push_str(&format!(
                "UID:{uid}\r\nFN:{}\r\n",
                n.replace([',', ';'], " ")
            ));
            for em in arr(e) {
                out.push_str(&format!("EMAIL:{em}\r\n"));
            }
            for ph in arr(p) {
                out.push_str(&format!("TEL:{ph}\r\n"));
            }
            out.push_str("END:VCARD\r\n");
        }
        Ok(([(header::CONTENT_TYPE, "text/vcard; charset=utf-8")], out).into_response())
    }
}
