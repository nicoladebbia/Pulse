-- Migration 014: Intelligence system upgrade
-- Adds probability tracking, novelty/sentiment scoring, and alerts

-- Probability history for predictions (Brier scoring)
ALTER TABLE insights ADD COLUMN probability_history TEXT DEFAULT '[]';
ALTER TABLE insights ADD COLUMN brier_score REAL;
ALTER TABLE insights ADD COLUMN resolution_criteria TEXT;
ALTER TABLE insights ADD COLUMN base_rate REAL;

-- Novelty + sentiment + event_type on stories
ALTER TABLE stories ADD COLUMN sentiment REAL;
ALTER TABLE stories ADD COLUMN novelty REAL;
ALTER TABLE stories ADD COLUMN event_type TEXT;

-- Alerts table for proactive intelligence
CREATE TABLE IF NOT EXISTS alerts (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    alert_type      TEXT NOT NULL,
    severity        TEXT NOT NULL CHECK(severity IN ('high', 'medium', 'low')),
    title           TEXT NOT NULL,
    body            TEXT NOT NULL,
    entity_id       INTEGER,
    story_ids       TEXT,
    acknowledged    INTEGER NOT NULL DEFAULT 0,
    created_at      TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE INDEX IF NOT EXISTS idx_alerts_unacked ON alerts(acknowledged, created_at DESC);
