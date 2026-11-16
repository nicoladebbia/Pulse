-- Per-feed counts for every fetch run: whether each news feed answered, what it
-- handed over, how much of that this run could use, and how much was left once
-- repeats were dropped.
--
-- Before this table a feed was only visible through the articles that
-- survived. `fetch_candidates` (034) starts after the repeats are gone and
-- names the publisher, not the feed, so a feed that failed, returned nothing,
-- or only ever returned repeats left no trace anywhere but the log. The
-- numbers need weeks to mean anything, which is why the counting starts before
-- anything reads it.
--
-- News feeds only (RSS, Google News queries, Hacker News, Reddit, ArXiv,
-- bioRxiv). The financial sources give every filing its own id and are counted
-- in `api_usage`.
--
-- One row per feed per run; `run_at` (UTC) is the same for every row of a run.
-- A run that stops early (too few new articles) still has its rows:
-- `briefing_id` is set only when the run went on to make a briefing. A feed has
-- no row for a run in which its whole source was abandoned before the feed
-- answered, so rates are over the rows that exist. A feed that had answered when
-- its source failed as a whole has `ok = 0` and an error that says so (its
-- articles were dropped with the source); `entries` keeps what it answered.
--
--   entries  entries in the feed's answer, before any of this app's filters
--            (NULL when the feed's own fetch failed)
--   fetched  articles the feed handed to the run (after the feed's own limits:
--            its newest entries, recent enough, with a title and a link)
--   offered  of those, the ones this run could use (its topics, fresh enough):
--            what went into the repeat check
--   pooled   of those, the ones left once repeats were dropped
--
-- So `ok = 1`, `entries > 0` and `fetched = 0` is a feed that answers but has
-- nothing recent (or, for a subreddit, only posts an earlier subreddit already
-- gave: crossposts are kept once), `offered - pooled` is what was dropped as a
-- repeat, and a feed of another mode's topics has `fetched > 0` and
-- `offered = 0`.
--
-- Rows are kept 180 days (`feed_runs::KEEP_DAYS`). No foreign keys, for the
-- reason 033 and 034 give, so a briefing that is replaced later the same day
-- leaves its run's rows pointing at an id that is gone.

CREATE TABLE IF NOT EXISTS feed_runs (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    run_at       TEXT    NOT NULL,
    mode         TEXT    NOT NULL,            -- 'daily' | 'freedoms'
    briefing_id  INTEGER,
    feed_id      TEXT    NOT NULL,            -- as on the article (RawArticle.feed_id)
    kind         TEXT    NOT NULL,            -- rss, google_news, hacker_news, reddit, arxiv, biorxiv, other
    name         TEXT    NOT NULL DEFAULT '',
    sector       TEXT    NOT NULL DEFAULT '',
    ok           INTEGER NOT NULL DEFAULT 1,  -- 0: the fetch failed
    error        TEXT,
    millis       INTEGER,                     -- how long the fetch took
    entries      INTEGER,
    fetched      INTEGER NOT NULL DEFAULT 0,
    offered      INTEGER NOT NULL DEFAULT 0,
    pooled       INTEGER NOT NULL DEFAULT 0,
    UNIQUE (run_at, mode, feed_id)
);

CREATE INDEX IF NOT EXISTS idx_feed_runs_feed ON feed_runs(feed_id, run_at);
CREATE INDEX IF NOT EXISTS idx_feed_runs_briefing ON feed_runs(briefing_id);

-- The feed each pool article came from, so "which feeds fill the pool and which
-- make the briefing" is one join. NULL on rows from before this migration.
ALTER TABLE fetch_candidates ADD COLUMN feed_id TEXT;
