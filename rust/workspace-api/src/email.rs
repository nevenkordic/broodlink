/*
 * Broodlink workspace-api — Email endpoints.
 * Ported from the workspace app routes/email_routes.py + helpers. JSON contract
 * preserved so the existing email UI works unchanged.
 *
 * Implemented:
 *   - Account CRUD + config + settings/writing-style + scheduled-send (+ poller)
 *   - Live IMAP (folders, list, read, search, flags, move/delete, attachments)
 *     and SMTP (send, draft, accounts/test) via the email_net module
 *   - LLM triage: summarize + ai-reply (via chat::complete_text)
 *   Account passwords are encrypted at rest (crypto module).
 *
 * Still stubbed: extract-style (needs sent-mail sampling) and the auto-triage
 * background poller (auto-tag/spam/urgency).
 */

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::Json;
use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{owner_from, AppState, WsError};

const LLM_PENDING: &str = "AI triage needs the LLM (not yet ported)";

// ---------------------------------------------------------------------------
// Schema
// ---------------------------------------------------------------------------

pub async fn ensure_email_schema(pg: &sqlx::PgPool) -> Result<(), sqlx::Error> {
    sqlx::raw_sql(
        r#"
        CREATE TABLE IF NOT EXISTS ws_email_accounts (
            id            TEXT PRIMARY KEY,
            owner         TEXT,
            name          TEXT NOT NULL,
            is_default    BOOLEAN NOT NULL DEFAULT FALSE,
            enabled       BOOLEAN NOT NULL DEFAULT TRUE,
            imap_host     TEXT NOT NULL DEFAULT '',
            imap_port     INTEGER NOT NULL DEFAULT 993,
            imap_user     TEXT NOT NULL DEFAULT '',
            imap_password TEXT NOT NULL DEFAULT '',
            imap_starttls BOOLEAN NOT NULL DEFAULT TRUE,
            smtp_host     TEXT NOT NULL DEFAULT '',
            smtp_port     INTEGER NOT NULL DEFAULT 465,
            smtp_user     TEXT NOT NULL DEFAULT '',
            smtp_password TEXT NOT NULL DEFAULT '',
            from_address  TEXT NOT NULL DEFAULT '',
            created_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
            updated_at    TIMESTAMPTZ NOT NULL DEFAULT now()
        );
        CREATE INDEX IF NOT EXISTS ws_email_accounts_owner_idx ON ws_email_accounts(owner);

        CREATE TABLE IF NOT EXISTS ws_email_settings (
            owner          TEXT PRIMARY KEY,
            writing_style  TEXT NOT NULL DEFAULT '',
            auto_summarize BOOLEAN NOT NULL DEFAULT FALSE,
            auto_reply     BOOLEAN NOT NULL DEFAULT FALSE,
            auto_tag       BOOLEAN NOT NULL DEFAULT FALSE,
            auto_spam      BOOLEAN NOT NULL DEFAULT FALSE,
            auto_calendar  BOOLEAN NOT NULL DEFAULT FALSE,
            updated_at     TIMESTAMPTZ NOT NULL DEFAULT now()
        );

        CREATE TABLE IF NOT EXISTS ws_scheduled_emails (
            id             TEXT PRIMARY KEY,
            owner          TEXT,
            to_addr        TEXT NOT NULL,
            cc             TEXT,
            bcc            TEXT,
            subject        TEXT,
            body           TEXT NOT NULL,
            in_reply_to    TEXT,
            references_hdr TEXT,
            attachments    TEXT,
            send_at        TIMESTAMPTZ NOT NULL,
            created_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
            status         TEXT NOT NULL DEFAULT 'pending',
            error          TEXT,
            account_id     TEXT,
            kind  TEXT
        );
        CREATE INDEX IF NOT EXISTS ws_scheduled_emails_owner_idx ON ws_scheduled_emails(owner);
        "#,
    )
    .execute(pg)
    .await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Accounts
// ---------------------------------------------------------------------------

#[derive(sqlx::FromRow)]
struct AccountRow {
    id: String,
    name: String,
    is_default: bool,
    enabled: bool,
    imap_host: String,
    imap_port: i32,
    imap_user: String,
    imap_password: String,
    imap_starttls: bool,
    smtp_host: String,
    smtp_port: i32,
    smtp_user: String,
    smtp_password: String,
    from_address: String,
}

fn account_json(a: &AccountRow) -> Value {
    json!({
        "id": a.id,
        "name": a.name,
        "is_default": a.is_default,
        "enabled": a.enabled,
        "imap_host": a.imap_host,
        "imap_port": a.imap_port,
        "imap_user": a.imap_user,
        "imap_starttls": a.imap_starttls,
        "smtp_host": a.smtp_host,
        "smtp_port": a.smtp_port,
        "smtp_user": a.smtp_user,
        "from_address": a.from_address,
        "has_imap_password": !a.imap_password.is_empty(),
        "has_smtp_password": !a.smtp_password.is_empty(),
    })
}

const ACCOUNT_SELECT: &str = "SELECT id, name, is_default, enabled, imap_host, imap_port, \
    imap_user, imap_password, imap_starttls, smtp_host, smtp_port, smtp_user, smtp_password, \
    from_address FROM ws_email_accounts";

pub async fn list_accounts(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Json<Value>, WsError> {
    let owner = owner_from(&headers);
    let rows = sqlx::query_as::<_, AccountRow>(&format!(
        "{ACCOUNT_SELECT} WHERE owner = $1 ORDER BY is_default DESC, created_at ASC"
    ))
    .bind(&owner)
    .fetch_all(&state.pg)
    .await?;
    let accounts: Vec<Value> = rows.iter().map(account_json).collect();
    Ok(Json(json!({ "accounts": accounts })))
}

#[derive(Deserialize, Default)]
pub struct AccountBody {
    name: Option<String>,
    is_default: Option<bool>,
    enabled: Option<bool>,
    imap_host: Option<String>,
    imap_port: Option<i32>,
    imap_user: Option<String>,
    imap_password: Option<String>,
    imap_starttls: Option<bool>,
    smtp_host: Option<String>,
    smtp_port: Option<i32>,
    smtp_user: Option<String>,
    smtp_password: Option<String>,
    from_address: Option<String>,
}

async fn clear_other_defaults(state: &AppState, owner: &str, keep: &str) -> Result<(), WsError> {
    sqlx::query("UPDATE ws_email_accounts SET is_default = FALSE WHERE owner = $1 AND id <> $2")
        .bind(owner)
        .bind(keep)
        .execute(&state.pg)
        .await?;
    Ok(())
}

pub async fn create_account(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(body): Json<AccountBody>,
) -> Result<Json<Value>, WsError> {
    let owner = owner_from(&headers);
    let name = body
        .name
        .clone()
        .ok_or_else(|| WsError::BadRequest("name is required".into()))?;
    let id = Uuid::new_v4().to_string();

    let existing: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM ws_email_accounts WHERE owner = $1")
            .bind(&owner)
            .fetch_one(&state.pg)
            .await?;
    let is_default = body.is_default.unwrap_or(false) || existing == 0;

    sqlx::query(
        "INSERT INTO ws_email_accounts \
         (id, owner, name, is_default, enabled, imap_host, imap_port, imap_user, imap_password, \
          imap_starttls, smtp_host, smtp_port, smtp_user, smtp_password, from_address) \
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15)",
    )
    .bind(&id)
    .bind(&owner)
    .bind(&name)
    .bind(is_default)
    .bind(body.enabled.unwrap_or(true))
    .bind(body.imap_host.unwrap_or_default())
    .bind(body.imap_port.unwrap_or(993))
    .bind(body.imap_user.unwrap_or_default())
    .bind(
        state
            .cipher
            .encrypt(&body.imap_password.unwrap_or_default()),
    )
    .bind(body.imap_starttls.unwrap_or(true))
    .bind(body.smtp_host.unwrap_or_default())
    .bind(body.smtp_port.unwrap_or(465))
    .bind(body.smtp_user.unwrap_or_default())
    .bind(
        state
            .cipher
            .encrypt(&body.smtp_password.unwrap_or_default()),
    )
    .bind(body.from_address.unwrap_or_default())
    .execute(&state.pg)
    .await?;

    if is_default {
        clear_other_defaults(&state, &owner, &id).await?;
    }
    Ok(Json(json!({ "ok": true, "id": id })))
}

async fn assert_account_owned(state: &AppState, id: &str, owner: &str) -> Result<(), WsError> {
    let found: Option<String> =
        sqlx::query_scalar("SELECT id FROM ws_email_accounts WHERE id = $1 AND owner = $2")
            .bind(id)
            .bind(owner)
            .fetch_optional(&state.pg)
            .await?;
    found
        .map(|_| ())
        .ok_or_else(|| WsError::NotFound("account not found".into()))
}

pub async fn update_account(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<AccountBody>,
) -> Result<Json<Value>, WsError> {
    let owner = owner_from(&headers);
    assert_account_owned(&state, &id, &owner).await?;

    // Passwords only overwritten when a non-empty value is supplied; encrypted.
    let imap_pw = body
        .imap_password
        .filter(|p| !p.is_empty())
        .map(|p| state.cipher.encrypt(&p));
    let smtp_pw = body
        .smtp_password
        .filter(|p| !p.is_empty())
        .map(|p| state.cipher.encrypt(&p));

    sqlx::query(
        "UPDATE ws_email_accounts SET \
            name = COALESCE($2, name), \
            is_default = COALESCE($3, is_default), \
            enabled = COALESCE($4, enabled), \
            imap_host = COALESCE($5, imap_host), \
            imap_port = COALESCE($6, imap_port), \
            imap_user = COALESCE($7, imap_user), \
            imap_password = COALESCE($8, imap_password), \
            imap_starttls = COALESCE($9, imap_starttls), \
            smtp_host = COALESCE($10, smtp_host), \
            smtp_port = COALESCE($11, smtp_port), \
            smtp_user = COALESCE($12, smtp_user), \
            smtp_password = COALESCE($13, smtp_password), \
            from_address = COALESCE($14, from_address), \
            updated_at = now() \
         WHERE id = $1",
    )
    .bind(&id)
    .bind(&body.name)
    .bind(body.is_default)
    .bind(body.enabled)
    .bind(&body.imap_host)
    .bind(body.imap_port)
    .bind(&body.imap_user)
    .bind(&imap_pw)
    .bind(body.imap_starttls)
    .bind(&body.smtp_host)
    .bind(body.smtp_port)
    .bind(&body.smtp_user)
    .bind(&smtp_pw)
    .bind(&body.from_address)
    .execute(&state.pg)
    .await?;

    if body.is_default == Some(true) {
        clear_other_defaults(&state, &owner, &id).await?;
    }
    Ok(Json(json!({ "ok": true, "id": id })))
}

pub async fn delete_account(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Value>, WsError> {
    let owner = owner_from(&headers);
    assert_account_owned(&state, &id, &owner).await?;
    let was_default: bool =
        sqlx::query_scalar("SELECT is_default FROM ws_email_accounts WHERE id = $1")
            .bind(&id)
            .fetch_one(&state.pg)
            .await?;
    sqlx::query("DELETE FROM ws_email_accounts WHERE id = $1")
        .bind(&id)
        .execute(&state.pg)
        .await?;
    if was_default {
        // Promote the next-oldest enabled account.
        sqlx::query(
            "UPDATE ws_email_accounts SET is_default = TRUE WHERE id = ( \
                SELECT id FROM ws_email_accounts WHERE owner = $1 AND enabled = TRUE \
                ORDER BY created_at ASC LIMIT 1 )",
        )
        .bind(&owner)
        .execute(&state.pg)
        .await?;
    }
    Ok(Json(json!({ "ok": true })))
}

pub async fn set_default_account(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Value>, WsError> {
    let owner = owner_from(&headers);
    assert_account_owned(&state, &id, &owner).await?;
    sqlx::query("UPDATE ws_email_accounts SET is_default = (id = $2) WHERE owner = $1")
        .bind(&owner)
        .bind(&id)
        .execute(&state.pg)
        .await?;
    Ok(Json(json!({ "ok": true })))
}

// ---------------------------------------------------------------------------
// Settings (auto flags + writing style)
// ---------------------------------------------------------------------------

#[derive(sqlx::FromRow, Default)]
struct SettingsRow {
    writing_style: String,
    auto_summarize: bool,
    auto_reply: bool,
    auto_tag: bool,
    auto_spam: bool,
    auto_calendar: bool,
}

async fn load_settings(state: &AppState, owner: &str) -> Result<SettingsRow, WsError> {
    let row = sqlx::query_as::<_, SettingsRow>(
        "SELECT writing_style, auto_summarize, auto_reply, auto_tag, auto_spam, auto_calendar \
         FROM ws_email_settings WHERE owner = $1",
    )
    .bind(owner)
    .fetch_optional(&state.pg)
    .await?;
    Ok(row.unwrap_or_default())
}

// ---------------------------------------------------------------------------
// Config (maps onto the default account + settings)
// ---------------------------------------------------------------------------

pub async fn get_config(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Json<Value>, WsError> {
    let owner = owner_from(&headers);
    let acct = sqlx::query_as::<_, AccountRow>(&format!(
        "{ACCOUNT_SELECT} WHERE owner = $1 ORDER BY is_default DESC, created_at ASC LIMIT 1"
    ))
    .bind(&owner)
    .fetch_optional(&state.pg)
    .await?;
    let s = load_settings(&state, &owner).await?;

    let mut out = json!({
        "email_auto_summarize": s.auto_summarize,
        "email_auto_reply": s.auto_reply,
        "email_auto_tag": s.auto_tag,
        "email_auto_spam": s.auto_spam,
        "email_auto_calendar": s.auto_calendar,
    });
    if let Some(a) = acct {
        out["account_id"] = json!(a.id);
        out["account_name"] = json!(a.name);
        out["smtp_host"] = json!(a.smtp_host);
        out["smtp_port"] = json!(a.smtp_port);
        out["smtp_user"] = json!(a.smtp_user);
        out["imap_host"] = json!(a.imap_host);
        out["imap_port"] = json!(a.imap_port);
        out["imap_user"] = json!(a.imap_user);
        out["imap_starttls"] = json!(a.imap_starttls);
        out["from_address"] = json!(a.from_address);
    }
    Ok(Json(out))
}

#[derive(Deserialize, Default)]
pub struct ConfigBody {
    smtp_host: Option<String>,
    smtp_port: Option<i32>,
    smtp_user: Option<String>,
    smtp_password: Option<String>,
    imap_host: Option<String>,
    imap_port: Option<i32>,
    imap_user: Option<String>,
    imap_password: Option<String>,
    imap_starttls: Option<bool>,
    from_address: Option<String>,
    email_auto_summarize: Option<bool>,
    email_auto_reply: Option<bool>,
    email_auto_tag: Option<bool>,
    email_auto_spam: Option<bool>,
    email_auto_calendar: Option<bool>,
}

pub async fn put_config(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(body): Json<ConfigBody>,
) -> Result<Json<Value>, WsError> {
    let owner = owner_from(&headers);

    // Persist auto flags into the settings row (upsert).
    sqlx::query(
        "INSERT INTO ws_email_settings (owner, auto_summarize, auto_reply, auto_tag, auto_spam, auto_calendar, updated_at) \
         VALUES ($1, COALESCE($2,FALSE), COALESCE($3,FALSE), COALESCE($4,FALSE), COALESCE($5,FALSE), COALESCE($6,FALSE), now()) \
         ON CONFLICT (owner) DO UPDATE SET \
            auto_summarize = COALESCE($2, ws_email_settings.auto_summarize), \
            auto_reply     = COALESCE($3, ws_email_settings.auto_reply), \
            auto_tag       = COALESCE($4, ws_email_settings.auto_tag), \
            auto_spam      = COALESCE($5, ws_email_settings.auto_spam), \
            auto_calendar  = COALESCE($6, ws_email_settings.auto_calendar), \
            updated_at = now()",
    )
    .bind(&owner)
    .bind(body.email_auto_summarize)
    .bind(body.email_auto_reply)
    .bind(body.email_auto_tag)
    .bind(body.email_auto_spam)
    .bind(body.email_auto_calendar)
    .execute(&state.pg)
    .await?;

    // Update (or create) the default account.
    let default_id: Option<String> = sqlx::query_scalar(
        "SELECT id FROM ws_email_accounts WHERE owner = $1 ORDER BY is_default DESC, created_at ASC LIMIT 1",
    )
    .bind(&owner)
    .fetch_optional(&state.pg)
    .await?;

    let imap_pw = body
        .imap_password
        .clone()
        .filter(|p| !p.is_empty())
        .map(|p| state.cipher.encrypt(&p));
    let smtp_pw = body
        .smtp_password
        .clone()
        .filter(|p| !p.is_empty())
        .map(|p| state.cipher.encrypt(&p));

    match default_id {
        Some(id) => {
            sqlx::query(
                "UPDATE ws_email_accounts SET \
                    smtp_host = COALESCE($2, smtp_host), smtp_port = COALESCE($3, smtp_port), \
                    smtp_user = COALESCE($4, smtp_user), smtp_password = COALESCE($5, smtp_password), \
                    imap_host = COALESCE($6, imap_host), imap_port = COALESCE($7, imap_port), \
                    imap_user = COALESCE($8, imap_user), imap_password = COALESCE($9, imap_password), \
                    imap_starttls = COALESCE($10, imap_starttls), from_address = COALESCE($11, from_address), \
                    updated_at = now() \
                 WHERE id = $1",
            )
            .bind(&id)
            .bind(&body.smtp_host)
            .bind(body.smtp_port)
            .bind(&body.smtp_user)
            .bind(&smtp_pw)
            .bind(&body.imap_host)
            .bind(body.imap_port)
            .bind(&body.imap_user)
            .bind(&imap_pw)
            .bind(body.imap_starttls)
            .bind(&body.from_address)
            .execute(&state.pg)
            .await?;
        }
        None => {
            sqlx::query(
                "INSERT INTO ws_email_accounts \
                 (id, owner, name, is_default, smtp_host, smtp_port, smtp_user, smtp_password, \
                  imap_host, imap_port, imap_user, imap_password, imap_starttls, from_address) \
                 VALUES ($1,$2,'Default',TRUE,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12)",
            )
            .bind(Uuid::new_v4().to_string())
            .bind(&owner)
            .bind(body.smtp_host.unwrap_or_default())
            .bind(body.smtp_port.unwrap_or(465))
            .bind(body.smtp_user.unwrap_or_default())
            .bind(smtp_pw.unwrap_or_default())
            .bind(body.imap_host.unwrap_or_default())
            .bind(body.imap_port.unwrap_or(993))
            .bind(body.imap_user.unwrap_or_default())
            .bind(imap_pw.unwrap_or_default())
            .bind(body.imap_starttls.unwrap_or(true))
            .bind(body.from_address.unwrap_or_default())
            .execute(&state.pg)
            .await?;
        }
    }

    Ok(Json(json!({ "success": true })))
}

// ---------------------------------------------------------------------------
// Writing style
// ---------------------------------------------------------------------------

pub async fn get_style(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Json<Value>, WsError> {
    let owner = owner_from(&headers);
    let s = load_settings(&state, &owner).await?;
    Ok(Json(json!({ "style": s.writing_style })))
}

#[derive(Deserialize)]
pub struct StyleBody {
    #[serde(default)]
    style: String,
}

pub async fn put_style(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(body): Json<StyleBody>,
) -> Result<Json<Value>, WsError> {
    let owner = owner_from(&headers);
    sqlx::query(
        "INSERT INTO ws_email_settings (owner, writing_style, updated_at) VALUES ($1,$2,now()) \
         ON CONFLICT (owner) DO UPDATE SET writing_style = EXCLUDED.writing_style, updated_at = now()",
    )
    .bind(&owner)
    .bind(&body.style)
    .execute(&state.pg)
    .await?;
    Ok(Json(json!({ "success": true })))
}

// ---------------------------------------------------------------------------
// Scheduled send (queue lives in DB; the actual SMTP dispatch is phase 2)
// ---------------------------------------------------------------------------

pub async fn list_scheduled(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Json<Value>, WsError> {
    let owner = owner_from(&headers);
    let rows = sqlx::query_as::<
        _,
        (
            String,
            String,
            Option<String>,
            Option<String>,
            DateTime<Utc>,
            DateTime<Utc>,
            String,
            Option<String>,
        ),
    >(
        "SELECT id, to_addr, cc, subject, send_at, created_at, status, error \
         FROM ws_scheduled_emails WHERE owner = $1 AND status IN ('pending','failed') \
         ORDER BY send_at ASC",
    )
    .bind(&owner)
    .fetch_all(&state.pg)
    .await?;
    let scheduled: Vec<Value> = rows
        .into_iter()
        .map(
            |(id, to, cc, subject, send_at, created_at, status, error)| {
                json!({
                    "id": id,
                    "to": to,
                    "cc": cc,
                    "subject": subject,
                    "send_at": send_at.to_rfc3339(),
                    "created_at": created_at.to_rfc3339(),
                    "status": status,
                    "error": error,
                })
            },
        )
        .collect();
    Ok(Json(json!({ "scheduled": scheduled })))
}

#[derive(Deserialize)]
pub struct ScheduleBody {
    to: String,
    cc: Option<String>,
    bcc: Option<String>,
    subject: Option<String>,
    #[serde(default)]
    body: String,
    in_reply_to: Option<String>,
    references: Option<String>,
    attachments: Option<Value>,
    send_at: String,
    account_id: Option<String>,
    kind: Option<String>,
}

pub async fn schedule(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(body): Json<ScheduleBody>,
) -> Result<Json<Value>, WsError> {
    let owner = owner_from(&headers);
    let send_at = DateTime::parse_from_rfc3339(&body.send_at)
        .map_err(|_| WsError::BadRequest("invalid send_at".into()))?
        .with_timezone(&Utc);
    if send_at <= Utc::now() {
        return Err(WsError::BadRequest("send_at must be in the future".into()));
    }
    let id = Uuid::new_v4().to_string();
    sqlx::query(
        "INSERT INTO ws_scheduled_emails \
         (id, owner, to_addr, cc, bcc, subject, body, in_reply_to, references_hdr, attachments, \
          send_at, account_id, kind) \
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13)",
    )
    .bind(&id)
    .bind(&owner)
    .bind(&body.to)
    .bind(&body.cc)
    .bind(&body.bcc)
    .bind(&body.subject)
    .bind(&body.body)
    .bind(&body.in_reply_to)
    .bind(&body.references)
    .bind(body.attachments.map(|v| v.to_string()))
    .bind(send_at)
    .bind(&body.account_id)
    .bind(&body.kind)
    .execute(&state.pg)
    .await?;
    Ok(Json(
        json!({ "success": true, "id": id, "send_at": send_at.to_rfc3339() }),
    ))
}

pub async fn delete_scheduled(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(sid): Path<String>,
) -> Result<Json<Value>, WsError> {
    let owner = owner_from(&headers);
    sqlx::query(
        "DELETE FROM ws_scheduled_emails WHERE id = $1 AND owner = $2 AND status = 'pending'",
    )
    .bind(&sid)
    .bind(&owner)
    .execute(&state.pg)
    .await?;
    Ok(Json(json!({ "success": true })))
}

// ---------------------------------------------------------------------------
// PHASE 2 stubs — IMAP/SMTP/LLM. Documented response shapes, no network.
// ---------------------------------------------------------------------------

fn default_inbox() -> String {
    "INBOX".into()
}

async fn creds_for(
    state: &AppState,
    owner: &str,
    account_id: &Option<String>,
) -> Result<crate::email_net::MailCreds, WsError> {
    resolve_creds(state, owner, account_id, None).await
}

async fn run_blocking<T, F>(f: F) -> Result<T, WsError>
where
    T: Send + 'static,
    F: FnOnce() -> T + Send + 'static,
{
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| WsError::Internal(format!("join: {e}")))
}

#[derive(Deserialize)]
pub struct ListQuery {
    #[serde(default = "default_inbox")]
    folder: String,
    #[serde(default = "default_limit")]
    limit: i64,
    #[serde(default)]
    offset: i64,
    #[serde(default)]
    filter: Option<String>,
    account_id: Option<String>,
}
fn default_limit() -> i64 {
    50
}

pub async fn list_mail(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(q): Query<ListQuery>,
) -> Result<Json<Value>, WsError> {
    let owner = owner_from(&headers);
    let creds = creds_for(&state, &owner, &q.account_id).await?;
    let folder = q.folder.clone();
    let filter = q.filter.clone().unwrap_or_else(|| "all".into());
    let limit = q.limit.clamp(1, 200) as usize;
    let offset = q.offset.max(0) as usize;
    let f2 = folder.clone();
    let res =
        run_blocking(move || crate::email_net::list_blocking(creds, f2, limit, offset, filter))
            .await?;
    match res {
        Ok((emails, total)) => Ok(Json(json!({
            "emails": emails, "total": total, "folder": folder, "offset": q.offset
        }))),
        Err(e) => Err(WsError::Internal(e.to_string())),
    }
}

#[derive(Deserialize)]
pub struct AccountQuery {
    account_id: Option<String>,
    #[serde(default = "default_inbox")]
    folder: String,
}

pub async fn folders(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(q): Query<AccountQuery>,
) -> Result<Json<Value>, WsError> {
    let owner = owner_from(&headers);
    let creds = creds_for(&state, &owner, &q.account_id).await?;
    let res = run_blocking(move || crate::email_net::folders_blocking(creds)).await?;
    match res {
        Ok(folders) => Ok(Json(json!({ "folders": folders }))),
        Err(e) => Err(WsError::Internal(e.to_string())),
    }
}

#[derive(Deserialize)]
pub struct SearchQuery {
    #[serde(default)]
    q: String,
    #[serde(default = "default_inbox")]
    folder: String,
    #[serde(default = "default_limit")]
    limit: i64,
    account_id: Option<String>,
}

pub async fn search(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(q): Query<SearchQuery>,
) -> Result<Json<Value>, WsError> {
    let owner = owner_from(&headers);
    if q.q.trim().is_empty() {
        return Ok(Json(json!({ "emails": [], "total": 0, "query": q.q })));
    }
    let creds = creds_for(&state, &owner, &q.account_id).await?;
    let (folder, query, limit) = (
        q.folder.clone(),
        q.q.clone(),
        q.limit.clamp(1, 100) as usize,
    );
    let res = run_blocking(move || crate::email_net::search_blocking(creds, folder, query, limit))
        .await?;
    match res {
        Ok((emails, total)) => Ok(Json(
            json!({ "emails": emails, "total": total, "query": q.q }),
        )),
        Err(e) => Err(WsError::Internal(e.to_string())),
    }
}

#[derive(Deserialize)]
pub struct AttQuery {
    account_id: Option<String>,
    #[serde(default = "default_inbox")]
    folder: String,
}

pub async fn attachments(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(uid): Path<String>,
    Query(q): Query<AttQuery>,
) -> Result<Json<Value>, WsError> {
    let owner = owner_from(&headers);
    let uid: u32 = uid
        .parse()
        .map_err(|_| WsError::BadRequest("invalid uid".into()))?;
    let creds = creds_for(&state, &owner, &q.account_id).await?;
    let folder = q.folder.clone();
    let res =
        run_blocking(move || crate::email_net::fetch_attachments_blocking(creds, folder, uid))
            .await?;
    let atts = res.map_err(WsError::Internal)?;
    let list: Vec<Value> = atts
        .iter()
        .enumerate()
        .map(|(i, (name, ct, bytes))| json!({
            "index": i, "filename": name, "content_type": ct, "size": bytes.len(), "is_inline": false
        }))
        .collect();
    Ok(Json(json!({ "attachments": list, "uid": uid })))
}

pub async fn attachment_download(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path((uid, index)): Path<(String, usize)>,
    Query(q): Query<AttQuery>,
) -> Result<axum::response::Response, WsError> {
    use axum::response::IntoResponse;
    let owner = owner_from(&headers);
    let uid: u32 = uid
        .parse()
        .map_err(|_| WsError::BadRequest("invalid uid".into()))?;
    let creds = creds_for(&state, &owner, &q.account_id).await?;
    let folder = q.folder.clone();
    let res =
        run_blocking(move || crate::email_net::fetch_attachments_blocking(creds, folder, uid))
            .await?;
    let atts = res.map_err(WsError::Internal)?;
    let (name, ct, bytes) = atts
        .into_iter()
        .nth(index)
        .ok_or_else(|| WsError::NotFound("attachment not found".into()))?;
    Ok((
        [
            (axum::http::header::CONTENT_TYPE, ct),
            (
                axum::http::header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"{}\"", name.replace('"', "")),
            ),
        ],
        axum::body::Body::from(bytes),
    )
        .into_response())
}

pub async fn contacts(headers: HeaderMap) -> Json<Value> {
    let _ = owner_from(&headers);
    Json(json!({ "contacts": [], "count": 0 }))
}

#[derive(Deserialize)]
pub struct ReadQuery {
    account_id: Option<String>,
    #[serde(default = "default_inbox")]
    folder: String,
    #[serde(default = "default_true")]
    mark_seen: bool,
}
fn default_true() -> bool {
    true
}

pub async fn read_mail(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(uid): Path<String>,
    Query(q): Query<ReadQuery>,
) -> Result<Json<Value>, WsError> {
    let owner = owner_from(&headers);
    let uid: u32 = uid
        .parse()
        .map_err(|_| WsError::BadRequest("invalid uid".into()))?;
    let creds = creds_for(&state, &owner, &q.account_id).await?;
    let folder = q.folder.clone();
    let res =
        run_blocking(move || crate::email_net::read_blocking(creds, folder, uid, q.mark_seen))
            .await?;
    match res {
        Ok(Some(v)) => Ok(Json(v)),
        Ok(None) => Err(WsError::NotFound("message not found".into())),
        Err(e) => Err(WsError::Internal(e.to_string())),
    }
}

#[derive(Deserialize)]
pub struct ActionQuery {
    account_id: Option<String>,
    #[serde(default = "default_inbox")]
    folder: String,
    dest: Option<String>,
}

/// Flag / move / delete dispatcher, keyed off the route's trailing action.
async fn do_action(
    state: &AppState,
    owner: &str,
    uid: u32,
    q: &ActionQuery,
    action: &str,
) -> Result<Value, WsError> {
    let creds = creds_for(state, owner, &q.account_id).await?;
    let folder = q.folder.clone();
    let dest = q.dest.clone();
    let action = action.to_string();
    let res = run_blocking(move || -> imap::error::Result<()> {
        use crate::email_net::*;
        match action.as_str() {
            "mark-read" => store_flag_blocking(creds, folder, uid, "\\Seen", true),
            "mark-unread" => store_flag_blocking(creds, folder, uid, "\\Seen", false),
            "mark-answered" => store_flag_blocking(creds, folder, uid, "\\Answered", true),
            "clear-answered" => store_flag_blocking(creds, folder, uid, "\\Answered", false),
            "archive" => move_to_role_blocking(creds, folder, uid, "archive"),
            "delete" => move_to_role_blocking(creds, folder, uid, "trash"),
            "delete-permanent" => delete_permanent_blocking(creds, folder, uid),
            "move" => move_to_blocking(creds, folder, uid, dest.unwrap_or_default()),
            _ => Ok(()),
        }
    })
    .await?;
    res.map(|_| json!({ "success": true }))
        .map_err(|e| WsError::Internal(e.to_string()))
}

macro_rules! action_handler {
    ($name:ident, $action:literal) => {
        pub async fn $name(
            State(state): State<Arc<AppState>>,
            headers: HeaderMap,
            Path(uid): Path<String>,
            Query(q): Query<ActionQuery>,
        ) -> Result<Json<Value>, WsError> {
            let owner = owner_from(&headers);
            let uid: u32 = uid
                .parse()
                .map_err(|_| WsError::BadRequest("invalid uid".into()))?;
            Ok(Json(do_action(&state, &owner, uid, &q, $action).await?))
        }
    };
}

action_handler!(mark_read, "mark-read");
action_handler!(mark_unread, "mark-unread");
action_handler!(mark_answered, "mark-answered");
action_handler!(clear_answered, "clear-answered");
action_handler!(archive, "archive");
action_handler!(move_mail, "move");
action_handler!(delete_mail, "delete");
action_handler!(delete_permanent, "delete-permanent");

#[derive(Deserialize)]
pub struct SendBody {
    to: String,
    cc: Option<String>,
    subject: Option<String>,
    #[serde(default)]
    body: String,
    body_html: Option<String>,
    account_id: Option<String>,
}

/// Core send/draft path shared by the HTTP handlers and the scheduled poller.
#[allow(clippy::too_many_arguments)]
async fn send_core(
    state: &AppState,
    owner: &str,
    account_id: &Option<String>,
    to: String,
    cc: Option<String>,
    subject: String,
    body: String,
    body_html: Option<String>,
    as_draft: bool,
) -> Result<Value, WsError> {
    let creds = creds_for(state, owner, account_id).await?;
    let from: String = sqlx::query_scalar(
        "SELECT COALESCE(NULLIF(from_address,''), smtp_user, imap_user) FROM ws_email_accounts \
         WHERE owner = $1 ORDER BY is_default DESC, created_at ASC LIMIT 1",
    )
    .bind(owner)
    .fetch_optional(&state.pg)
    .await?
    .unwrap_or_default();
    if from.is_empty() {
        return Err(WsError::BadRequest("no sending account configured".into()));
    }
    let mail = crate::email_net::OutgoingMail {
        from,
        to,
        cc,
        subject,
        body,
        body_html,
    };
    let res = run_blocking(move || {
        if as_draft {
            crate::email_net::draft_blocking(creds, mail)
        } else {
            crate::email_net::send_blocking(creds, mail)
        }
    })
    .await?;
    Ok(res)
}

pub async fn send(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(body): Json<SendBody>,
) -> Result<Json<Value>, WsError> {
    let owner = owner_from(&headers);
    let v = send_core(
        &state,
        &owner,
        &body.account_id,
        body.to,
        body.cc,
        body.subject.unwrap_or_default(),
        body.body,
        body.body_html,
        false,
    )
    .await?;
    Ok(Json(v))
}

pub async fn draft(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(body): Json<SendBody>,
) -> Result<Json<Value>, WsError> {
    let owner = owner_from(&headers);
    let v = send_core(
        &state,
        &owner,
        &body.account_id,
        body.to,
        body.cc,
        body.subject.unwrap_or_default(),
        body.body,
        body.body_html,
        true,
    )
    .await?;
    Ok(Json(v))
}

// ---------------------------------------------------------------------------
// Scheduled-send poller — dispatches due rows from ws_scheduled_emails.
// ---------------------------------------------------------------------------

/// Background loop: every 30s, send any pending scheduled emails whose time has
/// arrived, marking each row 'sent' or 'failed'. Spawned once at startup.
pub async fn run_scheduled_poller(state: Arc<AppState>) {
    let mut ticker = tokio::time::interval(std::time::Duration::from_secs(30));
    loop {
        ticker.tick().await;
        if let Err(e) = poll_once(&state).await {
            tracing::warn!(error = %e, "scheduled-email poll failed");
        }
    }
}

async fn poll_once(state: &AppState) -> Result<(), WsError> {
    let due = sqlx::query_as::<
        _,
        (
            String,
            Option<String>,
            String,
            Option<String>,
            Option<String>,
            String,
            Option<String>,
        ),
    >(
        "SELECT id, owner, to_addr, cc, subject, body, account_id \
         FROM ws_scheduled_emails WHERE status = 'pending' AND send_at <= now() \
         ORDER BY send_at ASC LIMIT 20",
    )
    .fetch_all(&state.pg)
    .await?;

    for (id, owner, to, cc, subject, body, account_id) in due {
        let owner = owner.unwrap_or_default();
        let outcome = send_core(
            state,
            &owner,
            &account_id,
            to,
            cc,
            subject.unwrap_or_default(),
            body,
            None,
            false,
        )
        .await;

        let (status, err): (&str, Option<String>) = match outcome {
            Ok(v) if v["success"] == Value::Bool(true) => ("sent", None),
            Ok(v) => (
                "failed",
                Some(v["error"].as_str().unwrap_or("send failed").to_string()),
            ),
            Err(e) => ("failed", Some(e.to_string())),
        };
        sqlx::query("UPDATE ws_scheduled_emails SET status = $2, error = $3 WHERE id = $1")
            .bind(&id)
            .bind(status)
            .bind(&err)
            .execute(&state.pg)
            .await?;
        tracing::info!(id = %id, status, "scheduled email processed");
    }
    Ok(())
}

#[derive(Deserialize, Default)]
pub struct TestBody {
    account_id: Option<String>,
    imap_host: Option<String>,
    imap_port: Option<i32>,
    imap_user: Option<String>,
    imap_password: Option<String>,
    imap_starttls: Option<bool>,
    smtp_host: Option<String>,
    smtp_port: Option<i32>,
    smtp_user: Option<String>,
    smtp_password: Option<String>,
}

/// Build live creds from an account row, overlaying any inline fields. Inline
/// passwords win; empty inline password falls back to the stored one.
async fn resolve_creds(
    state: &AppState,
    owner: &str,
    account_id: &Option<String>,
    inline: Option<&TestBody>,
) -> Result<crate::email_net::MailCreds, WsError> {
    let sql = match account_id {
        Some(_) => format!("{ACCOUNT_SELECT} WHERE owner = $1 AND id = $2"),
        None => format!(
            "{ACCOUNT_SELECT} WHERE owner = $1 ORDER BY is_default DESC, created_at ASC LIMIT 1"
        ),
    };
    let mut q = sqlx::query_as::<_, AccountRow>(&sql).bind(owner);
    if let Some(id) = account_id {
        q = q.bind(id);
    }
    let acct = q.fetch_optional(&state.pg).await?;

    // base from account (or empty)
    let (
        mut imap_host,
        mut imap_port,
        mut imap_user,
        mut imap_pass,
        mut imap_starttls,
        mut smtp_host,
        mut smtp_port,
        mut smtp_user,
        mut smtp_pass,
    ) = match &acct {
        Some(a) => (
            a.imap_host.clone(),
            a.imap_port as u16,
            a.imap_user.clone(),
            a.imap_password.clone(),
            a.imap_starttls,
            a.smtp_host.clone(),
            a.smtp_port as u16,
            a.smtp_user.clone(),
            a.smtp_password.clone(),
        ),
        None => (
            String::new(),
            993,
            String::new(),
            String::new(),
            true,
            String::new(),
            465,
            String::new(),
            String::new(),
        ),
    };

    if let Some(b) = inline {
        if let Some(v) = &b.imap_host {
            if !v.is_empty() {
                imap_host = v.clone();
            }
        }
        if let Some(v) = b.imap_port {
            imap_port = v as u16;
        }
        if let Some(v) = &b.imap_user {
            if !v.is_empty() {
                imap_user = v.clone();
            }
        }
        if let Some(v) = &b.imap_password {
            if !v.is_empty() {
                imap_pass = v.clone();
            }
        }
        if let Some(v) = b.imap_starttls {
            imap_starttls = v;
        }
        if let Some(v) = &b.smtp_host {
            if !v.is_empty() {
                smtp_host = v.clone();
            }
        }
        if let Some(v) = b.smtp_port {
            smtp_port = v as u16;
        }
        if let Some(v) = &b.smtp_user {
            if !v.is_empty() {
                smtp_user = v.clone();
            }
        }
        if let Some(v) = &b.smtp_password {
            if !v.is_empty() {
                smtp_pass = v.clone();
            }
        }
    }
    // Stored passwords are encrypted at rest; decrypt for live use. Inline
    // passwords (from a test form) arrive plaintext and pass through unchanged.
    imap_pass = state.cipher.decrypt(&imap_pass);
    smtp_pass = state.cipher.decrypt(&smtp_pass);

    if smtp_user.is_empty() {
        smtp_user = imap_user.clone();
    }
    if smtp_pass.is_empty() {
        smtp_pass = imap_pass.clone();
    }

    Ok(crate::email_net::MailCreds {
        imap_host,
        imap_port,
        imap_user,
        imap_pass,
        imap_starttls,
        smtp_host,
        smtp_port,
        smtp_user,
        smtp_pass,
    })
}

pub async fn test_accounts(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(body): Json<TestBody>,
) -> Result<Json<Value>, WsError> {
    let owner = owner_from(&headers);
    let creds = resolve_creds(&state, &owner, &body.account_id, Some(&body)).await?;
    let result = tokio::task::spawn_blocking(move || crate::email_net::test_blocking(creds))
        .await
        .map_err(|e| WsError::Internal(format!("join: {e}")))?;
    Ok(Json(result))
}

pub async fn urgency_state(headers: HeaderMap) -> Json<Value> {
    let _ = owner_from(&headers);
    Json(json!({ "total_unread": 0, "total_urgent": 0, "max_score": 0, "per_uid": {} }))
}

pub async fn llm_action(headers: HeaderMap) -> Json<Value> {
    let _ = owner_from(&headers);
    Json(json!({ "success": false, "error": LLM_PENDING }))
}

/// Summarize an email via the owner's default model.
pub async fn summarize(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Json<Value> {
    let owner = owner_from(&headers);
    let text = format!(
        "Subject: {}\nFrom: {}\n\n{}",
        body["subject"].as_str().unwrap_or(""),
        body["from"].as_str().unwrap_or(""),
        body["body"].as_str().unwrap_or("")
    );
    let sys = "Summarize this email in 1-3 concise bullet points. Output only the bullets.";
    match crate::chat::complete_text(&state, &owner, sys, &text).await {
        Ok(s) => Json(json!({ "success": true, "summary": s.trim(), "model_used": "default" })),
        Err(e) => Json(json!({ "success": false, "error": e.to_string() })),
    }
}

/// Draft a reply in the user's saved writing style.
pub async fn ai_reply(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Json<Value> {
    let owner = owner_from(&headers);
    let style = load_settings(&state, &owner)
        .await
        .map(|s| s.writing_style)
        .unwrap_or_default();
    let sys = format!(
        "You are writing an email reply AS the user (first person). Write only the reply body, \
         no subject, no preamble, no signature block. Never invent facts. {}",
        if style.is_empty() {
            String::new()
        } else {
            format!("Match this writing style: {style}")
        }
    );
    let user = format!(
        "Reply to this email.\nTo: {}\nSubject: {}\n\n{}",
        body["to"].as_str().unwrap_or(""),
        body["subject"].as_str().unwrap_or(""),
        body["original_body"].as_str().unwrap_or("")
    );
    match crate::chat::complete_text(&state, &owner, &sys, &user).await {
        Ok(s) => Json(json!({ "success": true, "reply": s.trim(), "model_used": "default" })),
        Err(e) => Json(json!({ "success": false, "error": e.to_string() })),
    }
}
