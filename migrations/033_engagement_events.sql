-- Local-only engagement instrumentation.
--
-- Pulse already records everything it PRODUCES (briefings, filings, backtests,
-- portfolio snapshots) and nothing it CONSUMES. That gap made the keep-or-kill
-- question unanswerable: `backtest_results` and `portfolio_snapshots` each hold
-- roughly one row per day, which reads as "Trading is in use" but is really a
-- scheduler writing to itself. Nothing in the schema could say whether a human
-- ever opened the page.
--
-- This table records the consumption side only. It never leaves the machine:
-- no network, no identifiers, no message bodies — `detail` carries small JSON
-- like {"sector":"ai"} and never user text.
--
-- Deliberately NO foreign key on story_id/briefing_id. Migration 023 already
-- rebuilt `stories` once (table rebuild, new rowids); a FK would either block a
-- future rebuild or cascade-delete the history this table exists to accumulate.
-- A dangling story_id is an acceptable cost — the event still counts.

CREATE TABLE IF NOT EXISTS engagement_events (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    occurred_at  TEXT    NOT NULL DEFAULT (datetime('now')),
    surface      TEXT    NOT NULL,
    event        TEXT    NOT NULL,
    story_id     INTEGER,
    briefing_id  INTEGER,
    sector       TEXT,
    -- Milliseconds a story stayed open. Written on close, so it is NULL for an
    -- open that never got a matching close (app quit, reload). Opens are counted
    -- from the `story_open` rows, which are durable; dwell is best-effort.
    dwell_ms     INTEGER,
    detail       TEXT
);

-- The three questions this table gets asked: what happened lately, how much has
-- each surface been used, and which stories get opened.
CREATE INDEX IF NOT EXISTS idx_engagement_occurred
    ON engagement_events(occurred_at);
CREATE INDEX IF NOT EXISTS idx_engagement_surface
    ON engagement_events(surface, event, occurred_at);
CREATE INDEX IF NOT EXISTS idx_engagement_story
    ON engagement_events(story_id) WHERE story_id IS NOT NULL;
