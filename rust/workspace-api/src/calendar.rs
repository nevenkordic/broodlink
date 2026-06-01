/*
 * Broodlink workspace-api — Calendar endpoints.
 * Ported from the workspace app routes/calendar_routes.py. JSON contract preserved so
 * the existing calendar UI works unchanged.
 *
 * Implemented now (local data plane): calendar + event CRUD, server-side
 * recurrence expansion, per-event/per-calendar colour, all-day vs timed +
 * is_utc serialization, .ics export, and CalDAV credential storage.
 *
 * Also implemented: .ics import (real VEVENT parser) and quick-parse
 * (natural language -> event via the LLM).
 *
 * Stubbed (network subsystem): POST /sync, /test — CalDAV pull is not ported.
 *
 * Datetimes are stored NAIVE (Postgres TIMESTAMP) to preserve the workspace app's
 * wall-clock semantics; `is_utc` controls whether a `Z` suffix is emitted.
 */

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap};
use axum::response::{IntoResponse, Response};
use axum::Json;
use chrono::{Datelike, Duration, NaiveDate, NaiveDateTime, NaiveTime};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{owner_from, AppState, WsError};

const DEFAULT_COLOR: &str = "#5b8abf";
const MAX_OCCURRENCES: usize = 732; // ~2 years of daily, safety cap

// ---------------------------------------------------------------------------
// Schema
// ---------------------------------------------------------------------------

pub async fn ensure_calendar_schema(pg: &sqlx::PgPool) -> Result<(), sqlx::Error> {
    sqlx::raw_sql(
        r#"
        CREATE TABLE IF NOT EXISTS ws_calendars (
            id         TEXT PRIMARY KEY,
            owner      TEXT,
            name       TEXT NOT NULL,
            color      TEXT NOT NULL DEFAULT '#5b8abf',
            source     TEXT NOT NULL DEFAULT 'local',
            created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
            updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
        );
        CREATE INDEX IF NOT EXISTS ws_calendars_owner_idx ON ws_calendars(owner);

        CREATE TABLE IF NOT EXISTS ws_calendar_events (
            uid         TEXT PRIMARY KEY,
            calendar_id TEXT NOT NULL REFERENCES ws_calendars(id) ON DELETE CASCADE,
            summary     TEXT NOT NULL DEFAULT '',
            description TEXT NOT NULL DEFAULT '',
            location    TEXT NOT NULL DEFAULT '',
            dtstart     TIMESTAMP NOT NULL,
            dtend       TIMESTAMP NOT NULL,
            all_day     BOOLEAN NOT NULL DEFAULT FALSE,
            is_utc      BOOLEAN NOT NULL DEFAULT FALSE,
            rrule       TEXT NOT NULL DEFAULT '',
            color       TEXT,
            status      TEXT NOT NULL DEFAULT 'confirmed',
            importance  TEXT NOT NULL DEFAULT 'normal',
            event_type  TEXT,
            last_pinged TIMESTAMP,
            created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
            updated_at  TIMESTAMPTZ NOT NULL DEFAULT now()
        );
        CREATE INDEX IF NOT EXISTS ws_events_cal_idx ON ws_calendar_events(calendar_id);
        CREATE INDEX IF NOT EXISTS ws_events_dtstart_idx ON ws_calendar_events(dtstart);

        CREATE TABLE IF NOT EXISTS ws_caldav_config (
            owner      TEXT PRIMARY KEY,
            url        TEXT NOT NULL DEFAULT '',
            username   TEXT NOT NULL DEFAULT '',
            password   TEXT NOT NULL DEFAULT '',
            updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
        );
        "#,
    )
    .execute(pg)
    .await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Datetime helpers
// ---------------------------------------------------------------------------

/// Serialize a stored naive datetime per the contract.
fn fmt_dt(dt: &NaiveDateTime, all_day: bool, is_utc: bool) -> String {
    if all_day {
        dt.format("%Y-%m-%d").to_string()
    } else if is_utc {
        dt.format("%Y-%m-%dT%H:%M:%SZ").to_string()
    } else {
        dt.format("%Y-%m-%dT%H:%M:%S").to_string()
    }
}

/// Parse an input datetime string. Returns (naive_dt, is_utc).
/// tz-aware inputs are converted to a UTC wall-clock and flagged is_utc.
fn parse_dt(s: &str) -> Option<(NaiveDateTime, bool)> {
    let s = s.trim();
    // tz-aware (Z or ±hh:mm offset)
    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(s) {
        return Some((dt.naive_utc(), true));
    }
    // naive variants
    for fmt in ["%Y-%m-%dT%H:%M:%S", "%Y-%m-%dT%H:%M", "%Y-%m-%d %H:%M:%S"] {
        if let Ok(dt) = NaiveDateTime::parse_from_str(s, fmt) {
            return Some((dt, false));
        }
    }
    if let Ok(d) = NaiveDate::parse_from_str(s, "%Y-%m-%d") {
        return Some((d.and_time(NaiveTime::MIN), false));
    }
    None
}

/// Parse a window bound (date or datetime) to a naive datetime.
fn parse_bound(s: &str) -> Option<NaiveDateTime> {
    parse_dt(s).map(|(d, _)| d)
}

// ---------------------------------------------------------------------------
// Recurrence expansion (FREQ DAILY/WEEKLY/MONTHLY/YEARLY + INTERVAL/COUNT/UNTIL
// + weekly BYDAY). Covers the common cases; unknown rules fall back to the base.
// ---------------------------------------------------------------------------

struct Rule {
    freq: String,
    interval: i64,
    count: Option<usize>,
    until: Option<NaiveDateTime>,
    byday: Vec<u32>, // 0=Mon..6=Sun
}

fn parse_rrule(rrule: &str) -> Option<Rule> {
    if rrule.trim().is_empty() {
        return None;
    }
    let body = rrule.trim().trim_start_matches("RRULE:");
    let mut freq = String::new();
    let mut interval = 1i64;
    let mut count = None;
    let mut until = None;
    let mut byday = Vec::new();
    for part in body.split(';') {
        let mut kv = part.splitn(2, '=');
        let key = kv.next().unwrap_or("").trim().to_uppercase();
        let val = kv.next().unwrap_or("").trim();
        match key.as_str() {
            "FREQ" => freq = val.to_uppercase(),
            "INTERVAL" => interval = val.parse().unwrap_or(1).max(1),
            "COUNT" => count = val.parse().ok(),
            "UNTIL" => {
                until = parse_dt(val)
                    .map(|(d, _)| d)
                    .or_else(|| {
                        NaiveDate::parse_from_str(val, "%Y%m%d")
                            .ok()
                            .map(|d| d.and_time(NaiveTime::MIN))
                    })
                    .or_else(|| NaiveDateTime::parse_from_str(val, "%Y%m%dT%H%M%SZ").ok());
            }
            "BYDAY" => {
                for code in val.split(',') {
                    let c = code
                        .trim()
                        .trim_start_matches(['+', '-', '0', '1', '2', '3', '4', '5']);
                    let wd = match c {
                        "MO" => 0,
                        "TU" => 1,
                        "WE" => 2,
                        "TH" => 3,
                        "FR" => 4,
                        "SA" => 5,
                        "SU" => 6,
                        _ => continue,
                    };
                    byday.push(wd);
                }
            }
            _ => {}
        }
    }
    if freq.is_empty() {
        return None;
    }
    Some(Rule {
        freq,
        interval,
        count,
        until,
        byday,
    })
}

fn add_freq(dt: NaiveDateTime, freq: &str, n: i64) -> Option<NaiveDateTime> {
    match freq {
        "DAILY" => Some(dt + Duration::days(n)),
        "WEEKLY" => Some(dt + Duration::weeks(n)),
        "MONTHLY" => add_months(dt, n),
        "YEARLY" => add_months(dt, n * 12),
        _ => None,
    }
}

fn add_months(dt: NaiveDateTime, months: i64) -> Option<NaiveDateTime> {
    let total = (dt.year() as i64) * 12 + (dt.month() as i64 - 1) + months;
    let year = (total.div_euclid(12)) as i32;
    let month = (total.rem_euclid(12)) as u32 + 1;
    // clamp day to month length
    let day = dt.day().min(days_in_month(year, month));
    NaiveDate::from_ymd_opt(year, month, day).map(|d| d.and_time(dt.time()))
}

fn days_in_month(year: i32, month: u32) -> u32 {
    let (ny, nm) = if month == 12 {
        (year + 1, 1)
    } else {
        (year, month + 1)
    };
    let first_next = NaiveDate::from_ymd_opt(ny, nm, 1).unwrap();
    let first = NaiveDate::from_ymd_opt(year, month, 1).unwrap();
    (first_next - first).num_days() as u32
}

/// Generate (start) instants for a rule within [window_start - dur, window_end].
fn expand(
    base: NaiveDateTime,
    rule: &Rule,
    window_start: NaiveDateTime,
    window_end: NaiveDateTime,
) -> Vec<NaiveDateTime> {
    let mut out = Vec::new();
    let mut cursor = base;
    let mut emitted = 0usize;
    let mut guard = 0usize;

    while guard < MAX_OCCURRENCES * 2 {
        guard += 1;
        if cursor > window_end {
            break;
        }
        if let Some(until) = rule.until {
            if cursor > until {
                break;
            }
        }

        // For weekly BYDAY, emit each listed weekday in the current week.
        if rule.freq == "WEEKLY" && !rule.byday.is_empty() {
            let monday = cursor - Duration::days(cursor.weekday().num_days_from_monday() as i64);
            for &wd in &rule.byday {
                let occ = monday + Duration::days(wd as i64);
                let occ = occ.date().and_time(base.time());
                if occ < base {
                    continue;
                }
                if let Some(until) = rule.until {
                    if occ > until {
                        continue;
                    }
                }
                if occ <= window_end && occ >= window_start - Duration::days(1) {
                    out.push(occ);
                }
                emitted += 1;
                if rule.count.map(|c| emitted >= c).unwrap_or(false) {
                    break;
                }
            }
        } else {
            if cursor >= window_start - Duration::days(1) {
                out.push(cursor);
            }
            emitted += 1;
        }

        if rule.count.map(|c| emitted >= c).unwrap_or(false) {
            break;
        }
        if out.len() >= MAX_OCCURRENCES {
            break;
        }
        cursor = match add_freq(cursor, &rule.freq, rule.interval) {
            Some(c) => c,
            None => break,
        };
    }
    out.sort();
    out.dedup();
    out
}

// ---------------------------------------------------------------------------
// Row types
// ---------------------------------------------------------------------------

#[derive(sqlx::FromRow)]
struct EventRow {
    uid: String,
    summary: String,
    description: String,
    location: String,
    dtstart: NaiveDateTime,
    dtend: NaiveDateTime,
    all_day: bool,
    is_utc: bool,
    rrule: String,
    color: Option<String>,
    importance: String,
    event_type: Option<String>,
    cal_id: String,
    cal_name: String,
    cal_color: String,
}

fn occ_json(
    ev: &EventRow,
    start: NaiveDateTime,
    end: NaiveDateTime,
    uid: String,
    is_recurrence: bool,
) -> Value {
    json!({
        "uid": uid,
        "summary": ev.summary,
        "dtstart": fmt_dt(&start, ev.all_day, ev.is_utc),
        "dtend": fmt_dt(&end, ev.all_day, ev.is_utc),
        "all_day": ev.all_day,
        "is_utc": ev.is_utc,
        "description": ev.description,
        "location": ev.location,
        "rrule": ev.rrule,
        "calendar": ev.cal_name,
        "calendar_href": ev.cal_id,
        "color": ev.color.clone().unwrap_or_else(|| ev.cal_color.clone()),
        "event_type": ev.event_type,
        "importance": ev.importance,
        "is_recurrence": is_recurrence,
        "series_uid": ev.uid,
    })
}

const EVENT_SELECT: &str =
    "SELECT e.uid, e.summary, e.description, e.location, e.dtstart, e.dtend, \
    e.all_day, e.is_utc, e.rrule, e.color, e.importance, e.event_type, \
    c.id AS cal_id, c.name AS cal_name, c.color AS cal_color \
    FROM ws_calendar_events e JOIN ws_calendars c ON c.id = e.calendar_id";

// ---------------------------------------------------------------------------
// Calendar CRUD
// ---------------------------------------------------------------------------

async fn ensure_default_calendar(state: &AppState, owner: &str) -> Result<(), WsError> {
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM ws_calendars WHERE owner = $1")
        .bind(owner)
        .fetch_one(&state.pg)
        .await?;
    if count == 0 {
        sqlx::query(
            "INSERT INTO ws_calendars (id, owner, name, color, source) VALUES ($1,$2,'Personal',$3,'local')",
        )
        .bind(Uuid::new_v4().to_string())
        .bind(owner)
        .bind(DEFAULT_COLOR)
        .execute(&state.pg)
        .await?;
    }
    Ok(())
}

pub async fn list_calendars(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Json<Value>, WsError> {
    let owner = owner_from(&headers);
    ensure_default_calendar(&state, &owner).await?;
    let rows = sqlx::query_as::<_, (String, String, String)>(
        "SELECT id, name, color FROM ws_calendars WHERE owner = $1 ORDER BY created_at ASC",
    )
    .bind(&owner)
    .fetch_all(&state.pg)
    .await?;
    let calendars: Vec<Value> = rows
        .into_iter()
        .map(|(id, name, color)| json!({ "name": name, "href": id, "color": color }))
        .collect();
    Ok(Json(json!({ "calendars": calendars })))
}

#[derive(Deserialize)]
pub struct CalendarQuery {
    name: Option<String>,
    color: Option<String>,
}

pub async fn create_calendar(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(q): Query<CalendarQuery>,
) -> Result<Json<Value>, WsError> {
    let owner = owner_from(&headers);
    let id = Uuid::new_v4().to_string();
    let name = q.name.unwrap_or_else(|| "Imported".into());
    let color = q.color.unwrap_or_else(|| DEFAULT_COLOR.into());
    sqlx::query(
        "INSERT INTO ws_calendars (id, owner, name, color, source) VALUES ($1,$2,$3,$4,'local')",
    )
    .bind(&id)
    .bind(&owner)
    .bind(&name)
    .bind(&color)
    .execute(&state.pg)
    .await?;
    Ok(Json(
        json!({ "ok": true, "id": id, "name": name, "color": color }),
    ))
}

async fn assert_calendar_owned(state: &AppState, cal_id: &str, owner: &str) -> Result<(), WsError> {
    let found: Option<String> =
        sqlx::query_scalar("SELECT id FROM ws_calendars WHERE id = $1 AND owner = $2")
            .bind(cal_id)
            .bind(owner)
            .fetch_optional(&state.pg)
            .await?;
    found
        .map(|_| ())
        .ok_or_else(|| WsError::NotFound("calendar not found".into()))
}

pub async fn update_calendar(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(cal_id): Path<String>,
    Query(q): Query<CalendarQuery>,
) -> Result<Json<Value>, WsError> {
    let owner = owner_from(&headers);
    assert_calendar_owned(&state, &cal_id, &owner).await?;
    sqlx::query(
        "UPDATE ws_calendars SET name = COALESCE($2, name), color = COALESCE($3, color), \
         updated_at = now() WHERE id = $1",
    )
    .bind(&cal_id)
    .bind(&q.name)
    .bind(&q.color)
    .execute(&state.pg)
    .await?;
    Ok(Json(json!({ "ok": true })))
}

pub async fn delete_calendar(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(cal_id): Path<String>,
) -> Result<Json<Value>, WsError> {
    let owner = owner_from(&headers);
    assert_calendar_owned(&state, &cal_id, &owner).await?;
    sqlx::query("DELETE FROM ws_calendars WHERE id = $1")
        .bind(&cal_id)
        .execute(&state.pg)
        .await?;
    Ok(Json(json!({ "ok": true })))
}

// ---------------------------------------------------------------------------
// Events
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct EventsQuery {
    start: String,
    end: String,
    calendar: Option<String>,
}

pub async fn list_events(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(q): Query<EventsQuery>,
) -> Result<Json<Value>, WsError> {
    let owner = owner_from(&headers);
    let win_start =
        parse_bound(&q.start).ok_or_else(|| WsError::BadRequest("invalid start".into()))?;
    let win_end = parse_bound(&q.end).ok_or_else(|| WsError::BadRequest("invalid end".into()))?;

    let mut sql = format!(
        "{EVENT_SELECT} WHERE c.owner = $1 AND e.status <> 'cancelled' \
         AND ( (e.rrule = '' AND e.dtstart < $2 AND e.dtend > $3) OR (e.rrule <> '' AND e.dtstart < $2) )"
    );
    if q.calendar.is_some() {
        sql.push_str(" AND (c.id = $4 OR c.name = $4)");
    }

    let mut query = sqlx::query_as::<_, EventRow>(&sql)
        .bind(&owner)
        .bind(win_end)
        .bind(win_start);
    if let Some(cal) = &q.calendar {
        query = query.bind(cal);
    }
    let rows = query.fetch_all(&state.pg).await?;

    let mut events = Vec::new();
    for ev in &rows {
        let dur = ev.dtend - ev.dtstart;
        match parse_rrule(&ev.rrule) {
            Some(rule) => {
                for occ in expand(ev.dtstart, &rule, win_start, win_end) {
                    let occ_end = occ + dur;
                    if occ < win_end && occ_end > win_start {
                        let is_base = occ == ev.dtstart;
                        let uid = if is_base {
                            ev.uid.clone()
                        } else if ev.all_day {
                            format!("{}::{}", ev.uid, occ.format("%Y-%m-%d"))
                        } else {
                            format!("{}::{}", ev.uid, occ.format("%Y-%m-%dT%H:%M"))
                        };
                        events.push(occ_json(ev, occ, occ_end, uid, !is_base));
                    }
                }
            }
            None => {
                events.push(occ_json(ev, ev.dtstart, ev.dtend, ev.uid.clone(), false));
            }
        }
    }
    // sort by dtstart string (ISO sorts lexicographically)
    events.sort_by(|a, b| a["dtstart"].as_str().cmp(&b["dtstart"].as_str()));
    Ok(Json(json!({ "events": events })))
}

#[derive(Deserialize)]
pub struct EventCreate {
    #[serde(default)]
    summary: String,
    dtstart: String,
    dtend: Option<String>,
    #[serde(default)]
    all_day: bool,
    #[serde(default)]
    description: String,
    #[serde(default)]
    location: String,
    calendar_href: Option<String>,
    #[serde(default)]
    rrule: String,
    color: Option<String>,
}

pub async fn create_event(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(body): Json<EventCreate>,
) -> Result<Json<Value>, WsError> {
    let owner = owner_from(&headers);
    ensure_default_calendar(&state, &owner).await?;

    let cal_id = match body.calendar_href {
        Some(c) => {
            assert_calendar_owned(&state, &c, &owner).await?;
            c
        }
        None => {
            sqlx::query_scalar::<_, String>(
                "SELECT id FROM ws_calendars WHERE owner = $1 ORDER BY created_at ASC LIMIT 1",
            )
            .bind(&owner)
            .fetch_one(&state.pg)
            .await?
        }
    };

    let (dtstart, is_utc) =
        parse_dt(&body.dtstart).ok_or_else(|| WsError::BadRequest("invalid dtstart".into()))?;
    let (dtend, _) = match &body.dtend {
        Some(s) => parse_dt(s).ok_or_else(|| WsError::BadRequest("invalid dtend".into()))?,
        None => {
            let d = if body.all_day {
                dtstart + Duration::days(1)
            } else {
                dtstart + Duration::hours(1)
            };
            (d, is_utc)
        }
    };

    let uid = Uuid::new_v4().to_string();
    sqlx::query(
        "INSERT INTO ws_calendar_events \
         (uid, calendar_id, summary, description, location, dtstart, dtend, all_day, is_utc, rrule, color) \
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11)",
    )
    .bind(&uid)
    .bind(&cal_id)
    .bind(&body.summary)
    .bind(&body.description)
    .bind(&body.location)
    .bind(dtstart)
    .bind(dtend)
    .bind(body.all_day)
    .bind(is_utc)
    .bind(&body.rrule)
    .bind(&body.color)
    .execute(&state.pg)
    .await?;

    Ok(Json(json!({ "ok": true, "uid": uid })))
}

/// Strip a `::date` recurrence suffix to get the series UID.
fn base_uid(uid: &str) -> &str {
    uid.split("::").next().unwrap_or(uid)
}

async fn assert_event_owned(state: &AppState, uid: &str, owner: &str) -> Result<(), WsError> {
    let found: Option<String> = sqlx::query_scalar(
        "SELECT e.uid FROM ws_calendar_events e JOIN ws_calendars c ON c.id = e.calendar_id \
         WHERE e.uid = $1 AND c.owner = $2",
    )
    .bind(uid)
    .bind(owner)
    .fetch_optional(&state.pg)
    .await?;
    found
        .map(|_| ())
        .ok_or_else(|| WsError::NotFound("event not found".into()))
}

#[derive(Deserialize)]
pub struct EventUpdate {
    summary: Option<String>,
    dtstart: Option<String>,
    dtend: Option<String>,
    all_day: Option<bool>,
    description: Option<String>,
    location: Option<String>,
    rrule: Option<String>,
    color: Option<String>,
}

pub async fn update_event(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(uid): Path<String>,
    Json(body): Json<EventUpdate>,
) -> Result<Json<Value>, WsError> {
    let owner = owner_from(&headers);
    let uid = base_uid(&uid).to_string();
    assert_event_owned(&state, &uid, &owner).await?;

    // Datetime fields are parsed if present; is_utc tracks dtstart's form.
    let parsed_start = body.dtstart.as_deref().and_then(parse_dt);
    let dtstart = parsed_start.map(|(d, _)| d);
    let is_utc = parsed_start.map(|(_, u)| u);
    let dtend = body.dtend.as_deref().and_then(parse_dt).map(|(d, _)| d);

    sqlx::query(
        "UPDATE ws_calendar_events SET \
            summary = COALESCE($2, summary), \
            dtstart = COALESCE($3, dtstart), \
            dtend = COALESCE($4, dtend), \
            all_day = COALESCE($5, all_day), \
            is_utc = COALESCE($6, is_utc), \
            description = COALESCE($7, description), \
            location = COALESCE($8, location), \
            rrule = COALESCE($9, rrule), \
            color = COALESCE($10, color), \
            updated_at = now() \
         WHERE uid = $1",
    )
    .bind(&uid)
    .bind(&body.summary)
    .bind(dtstart)
    .bind(dtend)
    .bind(body.all_day)
    .bind(is_utc)
    .bind(&body.description)
    .bind(&body.location)
    .bind(&body.rrule)
    .bind(&body.color)
    .execute(&state.pg)
    .await?;

    Ok(Json(json!({ "ok": true })))
}

pub async fn delete_event(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(uid): Path<String>,
) -> Result<Json<Value>, WsError> {
    let owner = owner_from(&headers);
    let uid = base_uid(&uid).to_string();
    assert_event_owned(&state, &uid, &owner).await?;
    sqlx::query("DELETE FROM ws_calendar_events WHERE uid = $1")
        .bind(&uid)
        .execute(&state.pg)
        .await?;
    Ok(Json(json!({ "ok": true })))
}

// ---------------------------------------------------------------------------
// .ics export
// ---------------------------------------------------------------------------

fn ical_dt(dt: &NaiveDateTime, all_day: bool, is_utc: bool) -> String {
    if all_day {
        format!("VALUE=DATE:{}", dt.format("%Y%m%d"))
    } else if is_utc {
        format!(":{}", dt.format("%Y%m%dT%H%M%SZ"))
    } else {
        format!(":{}", dt.format("%Y%m%dT%H%M%S"))
    }
}

fn ical_escape(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace(';', "\\;")
        .replace(',', "\\,")
        .replace('\n', "\\n")
}

pub async fn export_calendar(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(cal_id): Path<String>,
) -> Result<Response, WsError> {
    let owner = owner_from(&headers);
    assert_calendar_owned(&state, &cal_id, &owner).await?;

    let name: String = sqlx::query_scalar("SELECT name FROM ws_calendars WHERE id = $1")
        .bind(&cal_id)
        .fetch_one(&state.pg)
        .await?;

    let rows = sqlx::query_as::<
        _,
        (
            String,
            String,
            String,
            String,
            NaiveDateTime,
            NaiveDateTime,
            bool,
            bool,
            String,
        ),
    >(
        "SELECT uid, summary, description, location, dtstart, dtend, all_day, is_utc, rrule \
         FROM ws_calendar_events WHERE calendar_id = $1 AND status <> 'cancelled'",
    )
    .bind(&cal_id)
    .fetch_all(&state.pg)
    .await?;

    let mut ics =
        String::from("BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Broodlink//Calendar//EN\r\n");
    ics.push_str(&format!("X-WR-CALNAME:{}\r\n", ical_escape(&name)));
    for (uid, summary, description, location, dtstart, dtend, all_day, is_utc, rrule) in rows {
        ics.push_str("BEGIN:VEVENT\r\n");
        ics.push_str(&format!("UID:{uid}\r\n"));
        ics.push_str(&format!("SUMMARY:{}\r\n", ical_escape(&summary)));
        ics.push_str(&format!(
            "DTSTART{}\r\n",
            ical_dt(&dtstart, all_day, is_utc)
        ));
        ics.push_str(&format!("DTEND{}\r\n", ical_dt(&dtend, all_day, is_utc)));
        if !description.is_empty() {
            ics.push_str(&format!("DESCRIPTION:{}\r\n", ical_escape(&description)));
        }
        if !location.is_empty() {
            ics.push_str(&format!("LOCATION:{}\r\n", ical_escape(&location)));
        }
        if !rrule.is_empty() {
            ics.push_str(&format!("RRULE:{}\r\n", rrule.trim_start_matches("RRULE:")));
        }
        ics.push_str("END:VEVENT\r\n");
    }
    ics.push_str("END:VCALENDAR\r\n");

    let filename = format!("{}.ics", name.replace(['/', '\\', '"'], "_"));
    Ok((
        [
            (header::CONTENT_TYPE, "text/calendar".to_string()),
            (
                header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"{filename}\""),
            ),
        ],
        ics,
    )
        .into_response())
}

// ---------------------------------------------------------------------------
// CalDAV config (stored locally; sync itself is stubbed)
// ---------------------------------------------------------------------------

pub async fn get_config(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Json<Value>, WsError> {
    let owner = owner_from(&headers);
    let row = sqlx::query_as::<_, (String, String, String)>(
        "SELECT url, username, password FROM ws_caldav_config WHERE owner = $1",
    )
    .bind(&owner)
    .fetch_optional(&state.pg)
    .await?;
    let (url, username, has_password) = match row {
        Some((u, n, p)) => (u, n, !p.is_empty()),
        None => (String::new(), String::new(), false),
    };
    let local = url.is_empty();
    Ok(Json(json!({
        "url": url,
        "username": username,
        "password": "",
        "has_password": has_password,
        "local": local,
    })))
}

#[derive(Deserialize)]
pub struct ConfigBody {
    #[serde(default)]
    url: String,
    #[serde(default)]
    username: String,
    #[serde(default)]
    password: String,
}

pub async fn set_config(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(body): Json<ConfigBody>,
) -> Result<Json<Value>, WsError> {
    let owner = owner_from(&headers);
    if body.url.trim().is_empty() {
        sqlx::query("DELETE FROM ws_caldav_config WHERE owner = $1")
            .bind(&owner)
            .execute(&state.pg)
            .await?;
        return Ok(Json(json!({ "ok": true, "cleared": true })));
    }
    // Empty password preserves the previously stored one.
    if body.password.is_empty() {
        sqlx::query(
            "INSERT INTO ws_caldav_config (owner, url, username, updated_at) \
             VALUES ($1,$2,$3,now()) \
             ON CONFLICT (owner) DO UPDATE SET url = EXCLUDED.url, username = EXCLUDED.username, updated_at = now()",
        )
        .bind(&owner)
        .bind(&body.url)
        .bind(&body.username)
        .execute(&state.pg)
        .await?;
    } else {
        sqlx::query(
            "INSERT INTO ws_caldav_config (owner, url, username, password, updated_at) \
             VALUES ($1,$2,$3,$4,now()) \
             ON CONFLICT (owner) DO UPDATE SET url = EXCLUDED.url, username = EXCLUDED.username, \
             password = EXCLUDED.password, updated_at = now()",
        )
        .bind(&owner)
        .bind(&body.url)
        .bind(&body.username)
        .bind(state.cipher.encrypt(&body.password))
        .execute(&state.pg)
        .await?;
    }
    Ok(Json(json!({ "ok": true })))
}

// --- Network / LLM stubs (see module docs) -------------------------------

pub async fn test_config() -> Json<Value> {
    Json(json!({ "ok": false, "error": "CalDAV connectivity test not yet ported" }))
}

pub async fn sync() -> Json<Value> {
    Json(json!({
        "calendars": 0,
        "events": 0,
        "deleted": 0,
        "errors": ["CalDAV pull not yet ported"]
    }))
}

/// A parsed VEVENT.
pub struct IcsEvent {
    pub summary: String,
    pub description: String,
    pub location: String,
    pub dtstart: NaiveDateTime,
    pub dtend: NaiveDateTime,
    pub all_day: bool,
    pub is_utc: bool,
    pub rrule: String,
}

/// Parse one ICS datetime value (+ whether the property was VALUE=DATE).
fn parse_ics_dt(value: &str, is_date: bool) -> Option<(NaiveDateTime, bool, bool)> {
    let v = value.trim();
    if is_date || v.len() == 8 {
        let d = NaiveDate::parse_from_str(v, "%Y%m%d").ok()?;
        return Some((d.and_time(NaiveTime::MIN), false, true));
    }
    if let Some(stripped) = v.strip_suffix('Z') {
        let dt = NaiveDateTime::parse_from_str(stripped, "%Y%m%dT%H%M%S").ok()?;
        return Some((dt, true, false));
    }
    let dt = NaiveDateTime::parse_from_str(v, "%Y%m%dT%H%M%S").ok()?;
    Some((dt, false, false))
}

/// Parse a full .ics document into VEVENTs (basic RFC-5545: line unfolding,
/// param-aware property keys, common fields).
pub fn parse_ics(text: &str) -> Vec<IcsEvent> {
    // Unfold continuation lines (leading space/tab).
    let mut lines: Vec<String> = Vec::new();
    for raw in text.replace("\r\n", "\n").lines() {
        if (raw.starts_with(' ') || raw.starts_with('\t')) && !lines.is_empty() {
            lines.last_mut().unwrap().push_str(raw.trim_start());
        } else {
            lines.push(raw.to_string());
        }
    }

    let mut out = Vec::new();
    let mut cur: Option<IcsEvent> = None;
    for line in lines {
        let l = line.trim();
        if l.eq_ignore_ascii_case("BEGIN:VEVENT") {
            cur = Some(IcsEvent {
                summary: String::new(),
                description: String::new(),
                location: String::new(),
                dtstart: NaiveDateTime::default(),
                dtend: NaiveDateTime::default(),
                all_day: false,
                is_utc: false,
                rrule: String::new(),
            });
            continue;
        }
        if l.eq_ignore_ascii_case("END:VEVENT") {
            if let Some(ev) = cur.take() {
                out.push(ev);
            }
            continue;
        }
        let ev = match cur.as_mut() {
            Some(e) => e,
            None => continue,
        };
        let (key, value) = match l.split_once(':') {
            Some(kv) => kv,
            None => continue,
        };
        let name = key.split(';').next().unwrap_or("").to_uppercase();
        let is_date = key.to_uppercase().contains("VALUE=DATE");
        let unescape = |s: &str| {
            s.replace("\\n", "\n")
                .replace("\\,", ",")
                .replace("\\;", ";")
                .replace("\\\\", "\\")
        };
        match name.as_str() {
            "SUMMARY" => ev.summary = unescape(value),
            "DESCRIPTION" => ev.description = unescape(value),
            "LOCATION" => ev.location = unescape(value),
            "RRULE" => ev.rrule = value.to_string(),
            "DTSTART" => {
                if let Some((dt, utc, all)) = parse_ics_dt(value, is_date) {
                    ev.dtstart = dt;
                    ev.is_utc = utc;
                    ev.all_day = all;
                }
            }
            "DTEND" => {
                if let Some((dt, _, _)) = parse_ics_dt(value, is_date) {
                    ev.dtend = dt;
                }
            }
            _ => {}
        }
    }
    // default dtend = dtstart + 1h (timed) / +1d (all-day)
    for ev in &mut out {
        if ev.dtend == NaiveDateTime::default() {
            ev.dtend = if ev.all_day {
                ev.dtstart + Duration::days(1)
            } else {
                ev.dtstart + Duration::hours(1)
            };
        }
    }
    out
}

pub async fn import_ics(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    mut mp: axum::extract::Multipart,
) -> Result<Json<Value>, WsError> {
    let owner = owner_from(&headers);
    let mut ics = String::new();
    let mut cal_name = "Imported".to_string();
    while let Some(field) = mp
        .next_field()
        .await
        .map_err(|e| WsError::BadRequest(e.to_string()))?
    {
        match field.name().unwrap_or("") {
            "file" => ics = field.text().await.unwrap_or_default(),
            "calendar_name" => {
                let v = field.text().await.unwrap_or_default();
                if !v.is_empty() {
                    cal_name = v.chars().take(120).collect();
                }
            }
            _ => {}
        }
    }
    if ics.trim().is_empty() {
        return Err(WsError::BadRequest("no .ics content".into()));
    }
    let events = parse_ics(&ics);

    let cal_id = Uuid::new_v4().to_string();
    sqlx::query(
        "INSERT INTO ws_calendars (id, owner, name, color, source) VALUES ($1,$2,$3,$4,'import')",
    )
    .bind(&cal_id)
    .bind(&owner)
    .bind(&cal_name)
    .bind(DEFAULT_COLOR)
    .execute(&state.pg)
    .await?;

    let mut imported = 0;
    let mut skipped = 0;
    let mut seen = std::collections::HashSet::new();
    for ev in events {
        let key = format!("{}|{}", ev.summary, ev.dtstart);
        if !seen.insert(key) {
            skipped += 1;
            continue;
        }
        sqlx::query(
            "INSERT INTO ws_calendar_events \
             (uid, calendar_id, summary, description, location, dtstart, dtend, all_day, is_utc, rrule) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10)",
        )
        .bind(Uuid::new_v4().to_string())
        .bind(&cal_id)
        .bind(&ev.summary)
        .bind(&ev.description)
        .bind(&ev.location)
        .bind(ev.dtstart)
        .bind(ev.dtend)
        .bind(ev.all_day)
        .bind(ev.is_utc)
        .bind(&ev.rrule)
        .execute(&state.pg)
        .await?;
        imported += 1;
    }

    Ok(Json(json!({
        "ok": true, "imported": imported, "skipped": skipped,
        "calendar": cal_name, "calendar_id": cal_id
    })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_vevents() {
        let ics = "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nSUMMARY:Team sync\r\nDTSTART:20240115T100000Z\r\nDTEND:20240115T103000Z\r\nLOCATION:Room A\r\nRRULE:FREQ=WEEKLY\r\nEND:VEVENT\r\nBEGIN:VEVENT\r\nSUMMARY:Holiday\r\nDTSTART;VALUE=DATE:20240120\r\nEND:VEVENT\r\nEND:VCALENDAR";
        let evs = parse_ics(ics);
        assert_eq!(evs.len(), 2);
        assert_eq!(evs[0].summary, "Team sync");
        assert!(evs[0].is_utc && !evs[0].all_day);
        assert_eq!(evs[0].rrule, "FREQ=WEEKLY");
        assert!(evs[1].all_day);
        // all-day default end = +1 day
        assert_eq!(evs[1].dtend - evs[1].dtstart, Duration::days(1));
    }
}

#[derive(Deserialize)]
pub struct QuickParseBody {
    #[serde(default)]
    text: String,
}

pub async fn quick_parse(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(body): Json<QuickParseBody>,
) -> Json<Value> {
    let owner = owner_from(&headers);
    let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
    let sys = format!(
        "Today is {today}. Extract a single calendar event from the user's text. \
         Respond with ONLY a JSON object (no markdown fences): \
         {{\"summary\":string,\"dtstart\":\"YYYY-MM-DDTHH:MM:SS\",\"dtend\":\"YYYY-MM-DDTHH:MM:SS\",\"all_day\":bool,\"location\":string,\"description\":string}}. \
         If no end time is implied use a 1-hour duration."
    );
    match crate::chat::complete_text(&state, &owner, &sys, &body.text).await {
        Ok(s) => {
            let cleaned = s
                .trim()
                .trim_start_matches("```json")
                .trim_start_matches("```")
                .trim_end_matches("```")
                .trim();
            match serde_json::from_str::<Value>(cleaned) {
                Ok(ev) => Json(json!({ "ok": true, "event": ev, "confidence": 0.7 })),
                Err(_) => {
                    Json(json!({ "ok": false, "error": "could not parse model output", "raw": s }))
                }
            }
        }
        Err(e) => Json(json!({ "ok": false, "error": e.to_string() })),
    }
}
