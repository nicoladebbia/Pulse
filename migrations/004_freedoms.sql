-- Migration 004: Four Freedoms support
-- Rebuild briefings table: remove UNIQUE on date, add (date,briefing_type) uniqueness instead.
-- This allows both a 'daily' and 'freedoms' briefing on the same date.

PRAGMA foreign_keys=OFF;

CREATE TABLE IF NOT EXISTS briefings_new (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    date            TEXT NOT NULL,
    created_at      TEXT NOT NULL DEFAULT (datetime('now')),
    story_count     INTEGER NOT NULL DEFAULT 0,
    ai_count        INTEGER NOT NULL DEFAULT 0,
    miami_count     INTEGER NOT NULL DEFAULT 0,
    italy_count     INTEGER NOT NULL DEFAULT 0,
    tech_count      INTEGER NOT NULL DEFAULT 0,
    hero_story_id   INTEGER,
    fetch_duration_ms INTEGER,
    status          TEXT NOT NULL DEFAULT 'pending',
    briefing_type   TEXT NOT NULL DEFAULT 'daily'
);

INSERT OR IGNORE INTO briefings_new (id, date, created_at, story_count, ai_count, miami_count,
    italy_count, tech_count, hero_story_id, fetch_duration_ms, status, briefing_type)
SELECT id, date, created_at, story_count, ai_count, miami_count,
    italy_count, tech_count, hero_story_id, fetch_duration_ms, status,
    'daily'
FROM briefings;

DROP TABLE IF EXISTS briefings;
ALTER TABLE briefings_new RENAME TO briefings;

CREATE UNIQUE INDEX IF NOT EXISTS idx_briefings_date_type ON briefings(date, briefing_type);

PRAGMA foreign_keys=ON;

-- Freedom stories table
CREATE TABLE IF NOT EXISTS freedom_stories (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    briefing_id     INTEGER NOT NULL REFERENCES briefings(id),
    freedom         TEXT NOT NULL CHECK(freedom IN ('time', 'financial', 'location', 'health')),
    headline        TEXT NOT NULL,
    summary         TEXT NOT NULL,
    key_facts       TEXT NOT NULL DEFAULT '[]',
    why_it_matters  TEXT NOT NULL DEFAULT '',
    what_to_watch   TEXT NOT NULL DEFAULT '',
    importance_score INTEGER NOT NULL DEFAULT 5,
    relevance_score INTEGER,
    relevance_reason TEXT,
    is_hero         INTEGER NOT NULL DEFAULT 0,
    display_order   INTEGER NOT NULL DEFAULT 0,
    original_title  TEXT,
    original_url    TEXT NOT NULL DEFAULT '',
    original_language TEXT NOT NULL DEFAULT 'en',
    content_snippet TEXT NOT NULL DEFAULT '',
    source_name     TEXT NOT NULL DEFAULT '',
    source_url      TEXT NOT NULL DEFAULT '',
    published_at    TEXT,
    created_at      TEXT NOT NULL DEFAULT (datetime('now')),
    url_hash        TEXT,
    title_hash      TEXT
);

CREATE INDEX IF NOT EXISTS idx_freedom_stories_briefing ON freedom_stories(briefing_id);
CREATE INDEX IF NOT EXISTS idx_freedom_stories_freedom ON freedom_stories(freedom);
