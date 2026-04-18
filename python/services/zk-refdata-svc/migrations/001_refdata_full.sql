-- Migration: extend cfg.instrument_refdata with lifecycle columns and add supporting tables.
-- Idempotent: safe to re-run.

-- Extend instrument_refdata with lifecycle and provenance columns.
ALTER TABLE cfg.instrument_refdata
  ADD COLUMN IF NOT EXISTS lifecycle_status TEXT NOT NULL DEFAULT 'active',
  ADD COLUMN IF NOT EXISTS first_seen_at TIMESTAMPTZ,
  ADD COLUMN IF NOT EXISTS last_seen_at TIMESTAMPTZ,
  ADD COLUMN IF NOT EXISTS source_name TEXT,
  ADD COLUMN IF NOT EXISTS source_run_id BIGINT;

-- Refresh run metadata.
CREATE TABLE IF NOT EXISTS cfg.refdata_refresh_run (
  run_id               BIGSERIAL PRIMARY KEY,
  source_name          TEXT NOT NULL,
  venue                TEXT NOT NULL,
  started_at           TIMESTAMPTZ NOT NULL DEFAULT now(),
  ended_at             TIMESTAMPTZ,
  status               TEXT NOT NULL DEFAULT 'running',
  instruments_added    INT NOT NULL DEFAULT 0,
  instruments_updated  INT NOT NULL DEFAULT 0,
  instruments_disabled INT NOT NULL DEFAULT 0,
  instruments_deprecated INT NOT NULL DEFAULT 0,
  error_detail         TEXT
);

-- Change event log for post-commit notification and watermark tracking.
CREATE TABLE IF NOT EXISTS cfg.refdata_change_event (
  event_id       BIGSERIAL PRIMARY KEY,
  run_id         BIGINT REFERENCES cfg.refdata_refresh_run(run_id),
  instrument_id  TEXT NOT NULL,
  venue          TEXT NOT NULL,
  change_class   TEXT NOT NULL,
  watermark_ms   BIGINT NOT NULL,
  created_at     TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_refdata_change_event_run
  ON cfg.refdata_change_event(run_id);

-- Current market session state per venue/market pair.
CREATE TABLE IF NOT EXISTS cfg.market_session_state (
  venue          TEXT NOT NULL,
  market         TEXT NOT NULL,
  session_state  TEXT NOT NULL,
  effective_at   TIMESTAMPTZ NOT NULL,
  updated_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
  PRIMARY KEY (venue, market)
);

-- Market session calendar entries.
CREATE TABLE IF NOT EXISTS cfg.market_session_calendar (
  calendar_id    BIGSERIAL PRIMARY KEY,
  venue          TEXT NOT NULL,
  market         TEXT NOT NULL,
  date           DATE NOT NULL,
  session_state  TEXT NOT NULL,
  open_time      TIMESTAMPTZ,
  close_time     TIMESTAMPTZ,
  source         TEXT,
  updated_at     TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_market_session_calendar_venue_date
  ON cfg.market_session_calendar(venue, market, date);
