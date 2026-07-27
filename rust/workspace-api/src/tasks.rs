/*
 * Broodlink workspace-api — Scheduled Tasks endpoints.
 * Ported from the workspace app routes/task_routes.py. Full data plane (CRUD,
 * schedule math, run history) plus a real execution engine: run_now and a 60s
 * poller execute due tasks, run LLM tasks via chat::complete_text, record runs,
 * reschedule, and chain then_task_id. (action-type tasks are not yet wired.)
 */

use std::str::FromStr;
use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::Json;
use chrono::{DateTime, Datelike, Duration, TimeZone, Utc};
use cron::Schedule;
use rand::distributions::Alphanumeric;
use rand::Rng;
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{owner_from, AppState, WsError};

#[derive(sqlx::FromRow)]
struct TaskRow {
    id: String,
    owner: Option<String>,
    name: String,
    prompt: Option<String>,
    task_type: String,
    action: Option<String>,
    schedule: Option<String>,
    scheduled_time: Option<String>,
    scheduled_day: Option<i32>,
    scheduled_date: Option<DateTime<Utc>>,
    cron_expression: Option<String>,
    trigger_type: String,
    trigger_event: Option<String>,
    trigger_count: Option<i32>,
    trigger_counter: i32,
    next_run: Option<DateTime<Utc>>,
    last_run: Option<DateTime<Utc>>,
    status: String,
    output_target: String,
    session_id: Option<String>,
    model: Option<String>,
    endpoint_url: Option<String>,
    run_count: i32,
    webhook_token: Option<String>,
    then_task_id: Option<String>,
    notifications_enabled: bool,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

fn ts(dt: &DateTime<Utc>) -> String {
    dt.format("%Y-%m-%dT%H:%M:%SZ").to_string()
}
fn ts_opt(dt: &Option<DateTime<Utc>>) -> Value {
    dt.as_ref().map(|d| json!(ts(d))).unwrap_or(Value::Null)
}

fn task_json(t: &TaskRow) -> Value {
    let mut v = json!({
        "id": t.id,
        "name": t.name,
        "prompt": t.prompt,
        "task_type": t.task_type,
        "action": t.action,
        "schedule": t.schedule,
        "scheduled_time": t.scheduled_time,
        "scheduled_day": t.scheduled_day,
        "scheduled_date": ts_opt(&t.scheduled_date),
        "cron_expression": t.cron_expression,
        "trigger_type": t.trigger_type,
        "trigger_event": t.trigger_event,
        "trigger_count": t.trigger_count,
        "trigger_counter": t.trigger_counter,
        "next_run": ts_opt(&t.next_run),
        "last_run": ts_opt(&t.last_run),
        "status": t.status,
        "output_target": t.output_target,
        "session_id": t.session_id,
        "crew_member_id": Value::Null,
        "model": t.model,
        "endpoint_url": t.endpoint_url,
        "run_count": t.run_count,
        "then_task_id": t.then_task_id,
        "notifications_enabled": t.notifications_enabled,
        "created_at": ts(&t.created_at),
        "updated_at": ts(&t.updated_at),
        "is_builtin": false,
        "is_modified": false,
        "owner": t.owner,
    });
    if t.trigger_type == "webhook" {
        v["webhook_token"] = json!(t.webhook_token);
    }
    v
}

const SELECT: &str =
    "SELECT id, owner, name, prompt, task_type, action, schedule, scheduled_time, \
    scheduled_day, scheduled_date, cron_expression, trigger_type, trigger_event, trigger_count, \
    trigger_counter, next_run, last_run, status, output_target, session_id, model, endpoint_url, \
    run_count, webhook_token, then_task_id, notifications_enabled, created_at, updated_at \
    FROM ws_scheduled_tasks";

async fn fetch_owned(state: &AppState, id: &str, owner: &str) -> Result<TaskRow, WsError> {
    let row = sqlx::query_as::<_, TaskRow>(&format!("{SELECT} WHERE id = $1"))
        .bind(id)
        .fetch_optional(&state.pg)
        .await?
        .ok_or_else(|| WsError::NotFound("task not found".into()))?;
    if row.owner.as_deref() != Some(owner) && row.owner.is_some() {
        return Err(WsError::NotFound("task not found".into()));
    }
    Ok(row)
}

// ---------------------------------------------------------------------------
// Schedule math
// ---------------------------------------------------------------------------

fn parse_hm(time: &Option<String>) -> (u32, u32) {
    let s = time.as_deref().unwrap_or("09:00");
    let mut parts = s.split(':');
    let h = parts.next().and_then(|p| p.parse().ok()).unwrap_or(9);
    let m = parts.next().and_then(|p| p.parse().ok()).unwrap_or(0);
    (h.min(23), m.min(59))
}

/// Compute the next run instant (UTC). Mirrors the workspace app's compute_next_run for
/// once/daily/weekly/monthly, and uses a cron parser for cron expressions.
fn compute_next_run(
    schedule: &Option<String>,
    scheduled_time: &Option<String>,
    scheduled_day: Option<i32>,
    scheduled_date: &Option<DateTime<Utc>>,
    cron_expression: &Option<String>,
    now: DateTime<Utc>,
) -> Option<DateTime<Utc>> {
    match schedule.as_deref() {
        Some("once") => scheduled_date.filter(|d| *d > now),
        Some("cron") => cron_expression
            .as_ref()
            .and_then(|c| Schedule::from_str(c).ok())
            .and_then(|sched| sched.upcoming(Utc).next()),
        Some("daily") => {
            let (h, m) = parse_hm(scheduled_time);
            let mut candidate = Utc
                .with_ymd_and_hms(now.year(), now.month(), now.day(), h, m, 0)
                .single()?;
            if candidate <= now {
                candidate += Duration::days(1);
            }
            Some(candidate)
        }
        Some("weekly") => {
            let (h, m) = parse_hm(scheduled_time);
            let target = scheduled_day.unwrap_or(0).clamp(0, 6) as i64; // 0=Mon
            let today = now.weekday().num_days_from_monday() as i64;
            let mut delta = (target - today).rem_euclid(7);
            let base = Utc
                .with_ymd_and_hms(now.year(), now.month(), now.day(), h, m, 0)
                .single()?;
            if delta == 0 && base <= now {
                delta = 7;
            }
            Some(base + Duration::days(delta))
        }
        Some("monthly") => {
            let (h, m) = parse_hm(scheduled_time);
            let dom = scheduled_day.unwrap_or(1).clamp(1, 28) as u32;
            let mut candidate = Utc
                .with_ymd_and_hms(now.year(), now.month(), dom, h, m, 0)
                .single()?;
            if candidate <= now {
                let (y, mo) = if now.month() == 12 {
                    (now.year() + 1, 1)
                } else {
                    (now.year(), now.month() + 1)
                };
                candidate = Utc.with_ymd_and_hms(y, mo, dom, h, m, 0).single()?;
            }
            Some(candidate)
        }
        _ => None,
    }
}

fn random_token() -> String {
    rand::thread_rng()
        .sample_iter(&Alphanumeric)
        .take(32)
        .map(char::from)
        .collect()
}

// ---------------------------------------------------------------------------
// GET /api/tasks
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct ListQuery {
    status: Option<String>,
    #[serde(default)]
    include_last_run: bool,
}

pub async fn list(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(q): Query<ListQuery>,
) -> Result<Json<Value>, WsError> {
    let owner = owner_from(&headers);
    let mut sql = format!("{SELECT} WHERE owner = $1");
    if q.status.is_some() {
        sql.push_str(" AND status = $2");
    }
    sql.push_str(" ORDER BY created_at DESC");

    let mut query = sqlx::query_as::<_, TaskRow>(&sql).bind(&owner);
    if let Some(status) = &q.status {
        query = query.bind(status);
    }
    let rows = query.fetch_all(&state.pg).await?;

    let mut tasks = Vec::with_capacity(rows.len());
    for t in &rows {
        let mut v = task_json(t);
        if q.include_last_run {
            let last = sqlx::query_as::<_, (String, Option<String>)>(
                "SELECT status, result FROM ws_task_runs WHERE task_id = $1 \
                 ORDER BY started_at DESC LIMIT 1",
            )
            .bind(&t.id)
            .fetch_optional(&state.pg)
            .await?;
            if let Some((status, result)) = last {
                v["last_run_status"] = json!(status);
                v["last_run_result"] = json!(result
                    .map(|r| r.chars().take(500).collect::<String>())
                    .unwrap_or_default());
            } else {
                v["last_run_status"] = Value::Null;
                v["last_run_result"] = Value::Null;
            }
        }
        tasks.push(v);
    }
    Ok(Json(json!({ "tasks": tasks })))
}

// ---------------------------------------------------------------------------
// POST /api/tasks
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct TaskCreate {
    name: Option<String>,
    prompt: Option<String>,
    #[serde(default = "default_task_type")]
    task_type: String,
    action: Option<String>,
    schedule: Option<String>,
    scheduled_time: Option<String>,
    scheduled_day: Option<i32>,
    scheduled_date: Option<String>,
    cron_expression: Option<String>,
    #[serde(default = "default_trigger_type")]
    trigger_type: String,
    trigger_event: Option<String>,
    trigger_count: Option<i32>,
    #[serde(default = "default_output_target")]
    output_target: String,
    model: Option<String>,
    endpoint_url: Option<String>,
    then_task_id: Option<String>,
    notifications_enabled: Option<bool>,
}

fn default_task_type() -> String {
    "llm".into()
}
fn default_trigger_type() -> String {
    "schedule".into()
}
fn default_output_target() -> String {
    "session".into()
}

pub async fn create(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(body): Json<TaskCreate>,
) -> Result<Json<Value>, WsError> {
    let owner = owner_from(&headers);

    if matches!(body.task_type.as_str(), "llm" | "research") && body.prompt.is_none() {
        return Err(WsError::BadRequest(
            "prompt is required for llm/research tasks".into(),
        ));
    }
    if body.task_type == "action" && body.action.is_none() {
        return Err(WsError::BadRequest(
            "action is required for action tasks".into(),
        ));
    }
    if body.schedule.as_deref() == Some("cron") {
        if let Some(c) = &body.cron_expression {
            Schedule::from_str(c)
                .map_err(|_| WsError::BadRequest("invalid cron expression".into()))?;
        } else {
            return Err(WsError::BadRequest(
                "cron_expression required for cron schedule".into(),
            ));
        }
    }

    let id = Uuid::new_v4().to_string();
    let name = body
        .name
        .clone()
        .or_else(|| body.prompt.clone().map(|p| p.chars().take(60).collect()))
        .unwrap_or_else(|| "Untitled task".into());

    let scheduled_date: Option<DateTime<Utc>> = body
        .scheduled_date
        .as_ref()
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|d| d.with_timezone(&Utc));

    let next_run = if body.trigger_type == "schedule" {
        compute_next_run(
            &body.schedule,
            &body.scheduled_time,
            body.scheduled_day,
            &scheduled_date,
            &body.cron_expression,
            Utc::now(),
        )
    } else {
        None
    };

    let webhook_token = if body.trigger_type == "webhook" {
        Some(random_token())
    } else {
        None
    };

    let status = if next_run.is_some() || matches!(body.trigger_type.as_str(), "event" | "webhook")
    {
        "active"
    } else {
        "completed"
    };

    let notifications_enabled = body
        .notifications_enabled
        .unwrap_or(body.task_type != "action");

    let row = sqlx::query_as::<_, TaskRow>(&format!(
        "INSERT INTO ws_scheduled_tasks \
         (id, owner, name, prompt, task_type, action, schedule, scheduled_time, scheduled_day, \
          scheduled_date, cron_expression, trigger_type, trigger_event, trigger_count, next_run, \
          status, output_target, model, endpoint_url, then_task_id, notifications_enabled, \
          webhook_token) \
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18,$19,$20,$21,$22) \
         RETURNING {cols}",
        cols = SELECT
            .split("FROM")
            .next()
            .unwrap()
            .trim_start_matches("SELECT ")
            .trim()
    ))
    .bind(&id)
    .bind(&owner)
    .bind(&name)
    .bind(&body.prompt)
    .bind(&body.task_type)
    .bind(&body.action)
    .bind(&body.schedule)
    .bind(&body.scheduled_time)
    .bind(body.scheduled_day)
    .bind(scheduled_date)
    .bind(&body.cron_expression)
    .bind(&body.trigger_type)
    .bind(&body.trigger_event)
    .bind(body.trigger_count)
    .bind(next_run)
    .bind(status)
    .bind(&body.output_target)
    .bind(&body.model)
    .bind(&body.endpoint_url)
    .bind(&body.then_task_id)
    .bind(notifications_enabled)
    .bind(&webhook_token)
    .fetch_one(&state.pg)
    .await?;

    Ok(Json(task_json(&row)))
}

// ---------------------------------------------------------------------------
// GET / PUT / DELETE /api/tasks/:id
// ---------------------------------------------------------------------------

pub async fn get_one(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Value>, WsError> {
    let owner = owner_from(&headers);
    let row = fetch_owned(&state, &id, &owner).await?;
    Ok(Json(task_json(&row)))
}

#[derive(Deserialize)]
pub struct TaskUpdate {
    name: Option<String>,
    prompt: Option<String>,
    task_type: Option<String>,
    action: Option<String>,
    schedule: Option<String>,
    scheduled_time: Option<String>,
    scheduled_day: Option<i32>,
    cron_expression: Option<String>,
    output_target: Option<String>,
    model: Option<String>,
    endpoint_url: Option<String>,
    then_task_id: Option<String>,
    notifications_enabled: Option<bool>,
}

pub async fn update(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<TaskUpdate>,
) -> Result<Json<Value>, WsError> {
    let owner = owner_from(&headers);
    let existing = fetch_owned(&state, &id, &owner).await?;

    // Recompute next_run if scheduling changed and task is active+schedule-driven.
    let schedule = body.schedule.clone().or(existing.schedule.clone());
    let scheduled_time = body
        .scheduled_time
        .clone()
        .or(existing.scheduled_time.clone());
    let scheduled_day = body.scheduled_day.or(existing.scheduled_day);
    let cron_expression = body
        .cron_expression
        .clone()
        .or(existing.cron_expression.clone());
    let next_run = if existing.status == "active" && existing.trigger_type == "schedule" {
        compute_next_run(
            &schedule,
            &scheduled_time,
            scheduled_day,
            &existing.scheduled_date,
            &cron_expression,
            Utc::now(),
        )
    } else {
        existing.next_run
    };

    let row = sqlx::query_as::<_, TaskRow>(&format!(
        "UPDATE ws_scheduled_tasks SET \
            name = COALESCE($2, name), \
            prompt = COALESCE($3, prompt), \
            task_type = COALESCE($4, task_type), \
            action = COALESCE($5, action), \
            schedule = COALESCE($6, schedule), \
            scheduled_time = COALESCE($7, scheduled_time), \
            scheduled_day = COALESCE($8, scheduled_day), \
            cron_expression = COALESCE($9, cron_expression), \
            output_target = COALESCE($10, output_target), \
            model = COALESCE($11, model), \
            endpoint_url = COALESCE($12, endpoint_url), \
            then_task_id = COALESCE($13, then_task_id), \
            notifications_enabled = COALESCE($14, notifications_enabled), \
            next_run = $15, \
            updated_at = now() \
         WHERE id = $1 \
         RETURNING {cols}",
        cols = SELECT
            .split("FROM")
            .next()
            .unwrap()
            .trim_start_matches("SELECT ")
            .trim()
    ))
    .bind(&id)
    .bind(&body.name)
    .bind(&body.prompt)
    .bind(&body.task_type)
    .bind(&body.action)
    .bind(&body.schedule)
    .bind(&body.scheduled_time)
    .bind(body.scheduled_day)
    .bind(&body.cron_expression)
    .bind(&body.output_target)
    .bind(&body.model)
    .bind(&body.endpoint_url)
    .bind(&body.then_task_id)
    .bind(body.notifications_enabled)
    .bind(next_run)
    .fetch_one(&state.pg)
    .await?;

    Ok(Json(task_json(&row)))
}

pub async fn delete_one(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Value>, WsError> {
    let owner = owner_from(&headers);
    fetch_owned(&state, &id, &owner).await?;
    sqlx::query("DELETE FROM ws_scheduled_tasks WHERE id = $1")
        .bind(&id)
        .execute(&state.pg)
        .await?;
    Ok(Json(json!({ "ok": true })))
}

// ---------------------------------------------------------------------------
// pause / resume
// ---------------------------------------------------------------------------

pub async fn pause(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Value>, WsError> {
    let owner = owner_from(&headers);
    fetch_owned(&state, &id, &owner).await?;
    sqlx::query(
        "UPDATE ws_scheduled_tasks SET status = 'paused', updated_at = now() WHERE id = $1",
    )
    .bind(&id)
    .execute(&state.pg)
    .await?;
    Ok(Json(json!({ "ok": true, "status": "paused" })))
}

pub async fn resume(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Value>, WsError> {
    let owner = owner_from(&headers);
    let t = fetch_owned(&state, &id, &owner).await?;
    let next_run = if t.trigger_type == "schedule" {
        compute_next_run(
            &t.schedule,
            &t.scheduled_time,
            t.scheduled_day,
            &t.scheduled_date,
            &t.cron_expression,
            Utc::now(),
        )
    } else {
        t.next_run
    };
    sqlx::query(
        "UPDATE ws_scheduled_tasks SET status = 'active', next_run = $2, updated_at = now() WHERE id = $1",
    )
    .bind(&id)
    .bind(next_run)
    .execute(&state.pg)
    .await?;
    Ok(Json(
        json!({ "ok": true, "status": "active", "next_run": ts_opt(&next_run) }),
    ))
}

// ---------------------------------------------------------------------------
// POST /api/tasks/:id/run  — execution engine not yet ported (see module docs)
// ---------------------------------------------------------------------------

pub async fn run_now(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Value>, WsError> {
    let owner = owner_from(&headers);
    fetch_owned(&state, &id, &owner).await?;
    let st = Arc::clone(&state);
    tokio::spawn(async move {
        run_task(&st, &id).await;
    });
    Ok(Json(json!({ "ok": true, "message": "Task triggered" })))
}

/// Execute a single task: record a run, run the work (LLM tasks call the
/// owner's default model), store result, bump counters, and reschedule.
pub async fn run_task(state: &AppState, task_id: &str) {
    let task = match sqlx::query_as::<_, TaskRow>(&format!("{SELECT} WHERE id = $1"))
        .bind(task_id)
        .fetch_optional(&state.pg)
        .await
    {
        Ok(Some(t)) => t,
        _ => return,
    };
    let owner = task.owner.clone().unwrap_or_default();
    let run_id = Uuid::new_v4().to_string();
    let _ = sqlx::query(
        "INSERT INTO ws_task_runs (id, task_id, status, model) VALUES ($1,$2,'running',$3)",
    )
    .bind(&run_id)
    .bind(task_id)
    .bind(&task.model)
    .execute(&state.pg)
    .await;

    // Run the work.
    let (status, result, error): (&str, Option<String>, Option<String>) =
        match task.task_type.as_str() {
            "llm" | "research" => match &task.prompt {
                Some(p) if !p.is_empty() => {
                    match crate::chat::complete_text(
                        state,
                        &owner,
                        "You are a helpful assistant running a scheduled task.",
                        p,
                    )
                    .await
                    {
                        Ok(out) => ("success", Some(out), None),
                        Err(e) => ("error", None, Some(e.to_string())),
                    }
                }
                _ => ("error", None, Some("no prompt".into())),
            },
            other => (
                "error",
                None,
                Some(format!("task type '{other}' not supported yet")),
            ),
        };

    let _ = sqlx::query(
        "UPDATE ws_task_runs SET status = $2, result = $3, error = $4, finished_at = now() WHERE id = $1",
    )
    .bind(&run_id)
    .bind(status)
    .bind(&result)
    .bind(&error)
    .execute(&state.pg)
    .await;

    // Bump counters + reschedule.
    let next = if task.trigger_type == "schedule" && task.schedule.as_deref() != Some("once") {
        compute_next_run(
            &task.schedule,
            &task.scheduled_time,
            task.scheduled_day,
            &task.scheduled_date,
            &task.cron_expression,
            Utc::now(),
        )
    } else {
        None
    };
    let new_status = if task.schedule.as_deref() == Some("once") {
        "completed"
    } else {
        "active"
    };
    let _ = sqlx::query(
        "UPDATE ws_scheduled_tasks SET run_count = run_count + 1, last_run = now(), \
            next_run = $2, status = CASE WHEN status = 'paused' THEN 'paused' ELSE $3 END, \
            updated_at = now() WHERE id = $1",
    )
    .bind(task_id)
    .bind(next)
    .bind(new_status)
    .execute(&state.pg)
    .await;

    // Chain: run the follow-up task if configured (then_task_id).
    if status == "success" {
        if let Some(then_id) = task.then_task_id.clone() {
            Box::pin(run_task(state, &then_id)).await;
        }
    }
}

/// Background loop: every 60s, run due scheduled tasks.
pub async fn run_scheduled_poller(state: Arc<AppState>) {
    let mut ticker = tokio::time::interval(std::time::Duration::from_secs(60));
    loop {
        ticker.tick().await;
        let due: Vec<String> = sqlx::query_scalar(
            "SELECT id FROM ws_scheduled_tasks WHERE status = 'active' AND trigger_type = 'schedule' \
             AND next_run IS NOT NULL AND next_run <= now() LIMIT 20",
        )
        .fetch_all(&state.pg)
        .await
        .unwrap_or_default();
        for id in due {
            run_task(&state, &id).await;
        }
    }
}

// ---------------------------------------------------------------------------
// Run history
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct RecentQuery {
    limit: Option<i64>,
}

pub async fn recent_runs(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(q): Query<RecentQuery>,
) -> Result<Json<Value>, WsError> {
    let owner = owner_from(&headers);
    let limit = q.limit.unwrap_or(50).clamp(1, 200);
    let rows = sqlx::query_as::<
        _,
        (
            String,
            String,
            DateTime<Utc>,
            Option<DateTime<Utc>>,
            String,
            Option<String>,
            Option<String>,
            Option<i32>,
            Option<String>,
            String,
            String,
            Option<String>,
            Option<String>,
            Option<String>,
            String,
        ),
    >(
        "SELECT r.id, r.task_id, r.started_at, r.finished_at, r.status, r.result, r.error, \
                r.tokens_used, r.model, t.name, t.task_type, t.action, t.endpoint_url, \
                t.session_id, t.output_target \
         FROM ws_task_runs r JOIN ws_scheduled_tasks t ON t.id = r.task_id \
         WHERE t.owner = $1 ORDER BY r.started_at DESC LIMIT $2",
    )
    .bind(&owner)
    .bind(limit)
    .fetch_all(&state.pg)
    .await?;

    let runs: Vec<Value> = rows
        .into_iter()
        .map(|r| {
            json!({
                "id": r.0,
                "task_id": r.1,
                "started_at": ts(&r.2),
                "finished_at": ts_opt(&r.3),
                "status": r.4,
                "result": r.5,
                "error": r.6,
                "tokens_used": r.7,
                "model": r.8,
                "task_name": r.9,
                "task_type": r.10,
                "action": r.11,
                "endpoint_url": r.12,
                "session_id": r.13,
                "output_target": r.14,
            })
        })
        .collect();
    Ok(Json(json!({ "runs": runs })))
}

#[derive(Deserialize)]
pub struct TaskRunsQuery {
    limit: Option<i64>,
    offset: Option<i64>,
}

pub async fn task_runs(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(q): Query<TaskRunsQuery>,
) -> Result<Json<Value>, WsError> {
    let owner = owner_from(&headers);
    fetch_owned(&state, &id, &owner).await?;
    let limit = q.limit.unwrap_or(20).clamp(1, 200);
    let offset = q.offset.unwrap_or(0).max(0);

    let total: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM ws_task_runs WHERE task_id = $1")
        .bind(&id)
        .fetch_one(&state.pg)
        .await?;

    let rows = sqlx::query_as::<
        _,
        (
            String,
            DateTime<Utc>,
            Option<DateTime<Utc>>,
            String,
            Option<String>,
            Option<String>,
            Option<i32>,
            Option<String>,
        ),
    >(
        "SELECT id, started_at, finished_at, status, result, error, tokens_used, model \
         FROM ws_task_runs WHERE task_id = $1 ORDER BY started_at DESC LIMIT $2 OFFSET $3",
    )
    .bind(&id)
    .bind(limit)
    .bind(offset)
    .fetch_all(&state.pg)
    .await?;

    let runs: Vec<Value> = rows
        .into_iter()
        .map(|r| {
            json!({
                "id": r.0,
                "started_at": ts(&r.1),
                "finished_at": ts_opt(&r.2),
                "status": r.3,
                "result": r.4,
                "error": r.5,
                "tokens_used": r.6,
                "model": r.7,
            })
        })
        .collect();
    Ok(Json(json!({ "runs": runs, "total": total })))
}

// ---------------------------------------------------------------------------
// Notifications poll (in-app reminder queue — backed by the notifications
// subsystem, not yet ported; returns empty so the frontend poller is a no-op)
// ---------------------------------------------------------------------------

pub async fn notifications(headers: HeaderMap) -> Json<Value> {
    let _owner = owner_from(&headers);
    Json(json!({ "notifications": [] }))
}

// ---------------------------------------------------------------------------
// Onboarding flag (UI hint state)
// ---------------------------------------------------------------------------

pub async fn get_onboarding(headers: HeaderMap) -> Json<Value> {
    let _owner = owner_from(&headers);
    Json(json!({ "opened": true, "enabled": false }))
}

#[derive(Deserialize)]
pub struct OnboardingBody {
    #[serde(default)]
    enabled: bool,
}

pub async fn set_onboarding(headers: HeaderMap, Json(b): Json<OnboardingBody>) -> Json<Value> {
    let _owner = owner_from(&headers);
    Json(json!({ "opened": true, "enabled": b.enabled }))
}

// ---------------------------------------------------------------------------
// Metadata endpoints (drive the task-composer UI dropdowns)
// ---------------------------------------------------------------------------

pub async fn meta_output_targets() -> Json<Value> {
    Json(json!({ "targets": [
        { "value": "session", "label": "Session", "description": "Deliver the result into a chat session" },
        { "value": "notification", "label": "Notification", "description": "Show an in-app notification" },
        { "value": "email", "label": "Email me", "description": "Send the result by email" }
    ] }))
}

pub async fn meta_actions() -> Json<Value> {
    // Builtin actions are part of the not-yet-ported execution engine.
    Json(json!({ "actions": [] }))
}

pub async fn meta_events() -> Json<Value> {
    Json(json!({ "events": [
        { "name": "session_created", "description": "A new chat session was created" },
        { "name": "message_sent", "description": "A message was sent" },
        { "name": "document_created", "description": "A document was created" },
        { "name": "memory_added", "description": "A memory was stored" },
        { "name": "research_completed", "description": "A research run finished" },
        { "name": "email_received", "description": "An email arrived" },
        { "name": "skill_added", "description": "A skill was added" }
    ] }))
}
