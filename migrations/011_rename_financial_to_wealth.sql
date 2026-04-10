-- Migration 011: Rename "financial" freedom to "wealth"
-- Must recreate table to update CHECK constraint

CREATE TABLE freedom_stories_new (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    briefing_id     INTEGER NOT NULL REFERENCES briefings(id),
    freedom         TEXT NOT NULL CHECK(freedom IN ('time', 'wealth', 'location', 'health')),
    headline        TEXT NOT NULL,
    summary         TEXT NOT NULL,
    key_facts       TEXT NOT NULL DEFAULT '[]',
    why_it_matters  TEXT NOT NULL DEFAULT '',
    what_to_watch   TEXT NOT NULL DEFAULT '',
    importance_score INTEGER NOT NULL DEFAULT 5,
    relevance_score  INTEGER,
    is_hero         INTEGER NOT NULL DEFAULT 0,
    display_order   INTEGER NOT NULL DEFAULT 0,
    original_url    TEXT NOT NULL DEFAULT '',
    source_name     TEXT NOT NULL DEFAULT '',
    published_at    TEXT,
    created_at      TEXT NOT NULL DEFAULT (datetime('now')),
    context_prefix  TEXT,
    url_hash        TEXT,
    title_hash      TEXT
);

INSERT INTO freedom_stories_new
SELECT id, briefing_id,
    CASE freedom WHEN 'financial' THEN 'wealth' ELSE freedom END,
    headline, summary, key_facts, why_it_matters, what_to_watch,
    importance_score, relevance_score, is_hero, display_order,
    original_url, source_name, published_at, created_at,
    context_prefix, url_hash, title_hash
FROM freedom_stories;

DROP TABLE freedom_stories;
ALTER TABLE freedom_stories_new RENAME TO freedom_stories;

CREATE INDEX IF NOT EXISTS idx_freedom_stories_bf ON freedom_stories(briefing_id, freedom, display_order);
