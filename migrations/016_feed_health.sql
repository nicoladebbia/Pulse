-- Add missing columns to freedom_stories (needed by the freedoms pipeline INSERT)
ALTER TABLE freedom_stories ADD COLUMN original_title TEXT NOT NULL DEFAULT '';
ALTER TABLE freedom_stories ADD COLUMN original_language TEXT NOT NULL DEFAULT 'en';
ALTER TABLE freedom_stories ADD COLUMN content_snippet TEXT NOT NULL DEFAULT '';
ALTER TABLE freedom_stories ADD COLUMN source_url TEXT NOT NULL DEFAULT '';

-- Feed health tracking: per-feed article counts and failure detection
CREATE TABLE IF NOT EXISTS feed_health (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    feed_name TEXT NOT NULL,
    source_type TEXT NOT NULL,  -- 'google_news', 'rss', 'hacker_news'
    article_count INTEGER NOT NULL DEFAULT 0,
    error_message TEXT,
    checked_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE INDEX IF NOT EXISTS idx_feed_health_name ON feed_health(feed_name, checked_at);

-- Pipeline health summary per run
CREATE TABLE IF NOT EXISTS pipeline_health (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    run_date TEXT NOT NULL,
    stories_fetched INTEGER NOT NULL DEFAULT 0,
    stories_summarized INTEGER NOT NULL DEFAULT 0,
    stories_embedded INTEGER NOT NULL DEFAULT 0,
    entities_extracted INTEGER NOT NULL DEFAULT 0,
    predictions_validated INTEGER NOT NULL DEFAULT 0,
    feeds_failed INTEGER NOT NULL DEFAULT 0,
    summary_failures INTEGER NOT NULL DEFAULT 0,
    embedding_coverage_pct REAL NOT NULL DEFAULT 0.0,
    duration_secs REAL NOT NULL DEFAULT 0.0,
    warnings TEXT,  -- JSON array of warning strings
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);
