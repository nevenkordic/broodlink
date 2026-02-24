-- Migration 028: Proactive skills — scheduled tasks, notification rules, notification log
-- Part of: schedule-task, incident-response, notification-dispatch skills

-- Scheduled tasks: one-shot delayed + recurring
CREATE TABLE IF NOT EXISTS scheduled_tasks (
  id               BIGSERIAL PRIMARY KEY,
  title            VARCHAR(255) NOT NULL,
  description      TEXT DEFAULT '',
  priority         INT DEFAULT 0,
  formula_name     VARCHAR(100),
  params           JSONB DEFAULT '{}',
  next_run_at      TIMESTAMPTZ NOT NULL,
  recurrence_secs  BIGINT,
  last_run_at      TIMESTAMPTZ,
  run_count        INT DEFAULT 0,
  max_runs         INT,
  enabled          BOOLEAN DEFAULT TRUE,
  created_by       VARCHAR(100),
  created_at       TIMESTAMPTZ DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_scheduled_tasks_due
  ON scheduled_tasks (next_run_at) WHERE enabled = TRUE;

-- Notification rules: condition → channel
CREATE TABLE IF NOT EXISTS notification_rules (
  id                BIGSERIAL PRIMARY KEY,
  name              VARCHAR(100) NOT NULL UNIQUE,
  condition_type    VARCHAR(50) NOT NULL,
  condition_config  JSONB NOT NULL DEFAULT '{}',
  channel           VARCHAR(20) NOT NULL,
  target            VARCHAR(255) NOT NULL,
  template          TEXT,
  cooldown_minutes  INT DEFAULT 30,
  enabled           BOOLEAN DEFAULT TRUE,
  last_triggered_at TIMESTAMPTZ,
  created_at        TIMESTAMPTZ DEFAULT NOW()
);

-- Notification delivery log
CREATE TABLE IF NOT EXISTS notification_log (
  id          BIGSERIAL PRIMARY KEY,
  rule_id     BIGINT REFERENCES notification_rules(id),
  rule_name   VARCHAR(100),
  channel     VARCHAR(20) NOT NULL,
  target      VARCHAR(255) NOT NULL,
  message     TEXT NOT NULL,
  status      VARCHAR(20) DEFAULT 'pending',
  error_msg   TEXT,
  created_at  TIMESTAMPTZ DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_notification_log_time
  ON notification_log (created_at DESC);
