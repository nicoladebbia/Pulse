-- Migration 013: Materialized feedback reputation cache
-- Stores per-source and per-sector reputation scores derived from chat_feedback.
-- Recomputed on each new feedback submission.

CREATE TABLE IF NOT EXISTS feedback_reputation (
    key         TEXT PRIMARY KEY,  -- "source:{name}" or "sector:{name}"
    kind        TEXT NOT NULL CHECK(kind IN ('source', 'sector')),
    upvotes     INTEGER NOT NULL DEFAULT 0,
    downvotes   INTEGER NOT NULL DEFAULT 0,
    reputation  REAL NOT NULL DEFAULT 0.5,
    boost       REAL NOT NULL DEFAULT 1.0,
    updated_at  TEXT NOT NULL DEFAULT (datetime('now'))
);
