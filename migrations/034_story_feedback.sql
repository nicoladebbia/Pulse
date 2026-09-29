-- Ground truth for curation: what mattered to the reader, and what the pipeline
-- never showed them.
--
-- Two tables and a view. Nothing here alters an existing table.
--
-- `story_feedback` holds the one explicit signal — a one-tap "mattered" or
-- "didn't" on a shown story. One row per story; re-labelling overwrites, and
-- clearing the label deletes the row. Opens and read time are NOT duplicated
-- here: `engagement_events` (033) already records story_open / story_close with
-- dwell_ms, and the view below joins them in.
--
-- `fetch_candidates` is every news article that survived dedup in a daily run,
-- with how far it got: kept by the Groq pre-curate cut, summarized, curated into
-- the briefing (story_id set) — or buried. Before this table the losers of each
-- stage existed only in memory, so "what did the pipeline drop?" had no answer.
-- It is also the input set a shadow ranker is judged on.
--
-- No foreign keys, for the reason 033 gives: `stories` has been rebuilt once
-- (023) and may be again; a dangling id is cheaper than blocking a rebuild or
-- cascade-deleting the history these tables exist to accumulate.

CREATE TABLE IF NOT EXISTS story_feedback (
    story_id     INTEGER PRIMARY KEY,
    briefing_id  INTEGER,
    label        TEXT    NOT NULL CHECK (label IN ('mattered', 'didnt')),
    created_at   TEXT    NOT NULL DEFAULT (datetime('now')),
    updated_at   TEXT    NOT NULL DEFAULT (datetime('now'))
);

CREATE INDEX IF NOT EXISTS idx_story_feedback_briefing
    ON story_feedback(briefing_id);

CREATE TABLE IF NOT EXISTS fetch_candidates (
    id                 INTEGER PRIMARY KEY AUTOINCREMENT,
    briefing_id        INTEGER NOT NULL,
    url_hash           TEXT    NOT NULL,
    url                TEXT    NOT NULL,
    title              TEXT    NOT NULL,
    source_name        TEXT,
    sector             TEXT    NOT NULL,
    language           TEXT,
    published_at       TEXT,
    -- The snippet as fetched, before article-text enrichment, capped in code.
    content_snippet    TEXT,
    -- Position in the post-dedup pool (source collection order, not a rank).
    pool_position      INTEGER NOT NULL,
    -- Which cut decided what reached summarization: 0 none, 1 the Groq
    -- pre-curate, 2 the sector-balanced fallback cap (Groq failed on a pool
    -- over 150). Not a boolean: a fallback day is not the model's judgment.
    precurate_ran      INTEGER NOT NULL,
    -- 1 when the article reached summarization: kept by the cut, the
    -- sector-balanced fallback, or because no cut ran.
    kept_by_precurate  INTEGER NOT NULL,
    summarized         INTEGER NOT NULL,
    -- Set iff the article was curated into the briefing.
    story_id           INTEGER,
    created_at         TEXT    NOT NULL DEFAULT (datetime('now')),
    UNIQUE (briefing_id, url_hash)
);

CREATE INDEX IF NOT EXISTS idx_fetch_candidates_story
    ON fetch_candidates(story_id) WHERE story_id IS NOT NULL;

-- One row per story shown in a daily briefing, with every feedback signal the
-- app has for it. rank_shown is display_order: the position in the curated
-- list, grouped by sector. read_seconds sums every close, so re-reads add up;
-- it is 0 for an open whose close never landed (app quit mid-read).
CREATE VIEW IF NOT EXISTS story_feedback_signals AS
SELECT
    s.id                AS story_id,
    s.briefing_id       AS briefing_id,
    b.date              AS briefing_date,
    s.sector            AS sector,
    s.source_type       AS source_type,
    s.display_order     AS rank_shown,
    s.is_hero           AS is_hero,
    s.relevance_score   AS relevance_score,
    EXISTS (
        SELECT 1 FROM engagement_events e
        WHERE e.story_id = s.id AND e.event = 'story_open'
    )                   AS opened,
    COALESCE((
        SELECT SUM(e.dwell_ms) FROM engagement_events e
        WHERE e.story_id = s.id AND e.event = 'story_close'
    ), 0) / 1000.0      AS read_seconds,
    f.label             AS explicit_label,
    f.updated_at        AS labeled_at
FROM stories s
JOIN briefings b ON b.id = s.briefing_id
LEFT JOIN story_feedback f ON f.story_id = s.id
WHERE b.briefing_type = 'daily';
