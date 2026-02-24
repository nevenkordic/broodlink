-- Migration 027: Service events log (Postgres)
-- Persistent record of service-level events (model degradation, recovery, etc.)

CREATE TABLE IF NOT EXISTS service_events (
  id          BIGSERIAL PRIMARY KEY,
  service     VARCHAR(100) NOT NULL,
  event_type  VARCHAR(100) NOT NULL,
  severity    VARCHAR(20)  NOT NULL DEFAULT 'warning'
              CHECK (severity IN ('info', 'warning', 'error')),
  details     JSONB,
  created_at  TIMESTAMPTZ  DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_service_events_svc_time
  ON service_events(service, created_at DESC);
