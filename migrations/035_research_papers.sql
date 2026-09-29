-- Research lane: quant-finance papers read in full and mined for trading changes.
--
-- Until now arXiv q-fin papers entered Pulse as news: the RSS abstract was
-- summarized like a headline and routed to Freedoms. Nothing kept the paper
-- itself, so nothing could be learned from it. These tables hold the paper, its
-- full text, the deep read, and every trading proposal the read produced.
--
-- Numbered 035, not 034: a parallel branch (feat/phase2-archive-trends) owns
-- 034_story_feedback.sql. Neither migration touches the other's tables.
--
-- No foreign keys to trading tables. A proposal is a hypothesis about the
-- strategy, not a row that must follow cross_signals or paper_trades around.

-- One row per arXiv paper (versionless id). Every fetched paper gets a row and a
-- triage verdict, including the ones never read, so nothing disappears silently.
CREATE TABLE IF NOT EXISTS research_papers (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    arxiv_id        TEXT    NOT NULL UNIQUE,
    version         INTEGER NOT NULL DEFAULT 1,
    title           TEXT    NOT NULL,
    authors         TEXT    NOT NULL DEFAULT '',   -- comma-joined
    categories      TEXT    NOT NULL DEFAULT '',   -- space-joined, primary first
    abstract        TEXT    NOT NULL DEFAULT '',
    pages           INTEGER,                        -- from arxiv:comment, when stated
    published_at    TEXT    NOT NULL,
    -- new → triaged | skipped → reading (batch submitted) → read | failed
    status          TEXT    NOT NULL DEFAULT 'new',
    triage_score    INTEGER,                        -- 0-10 relevance to the trading system
    triage_json     TEXT,
    read_batch_id   TEXT,                           -- Anthropic batch holding the deep read
    -- Priced from count_tokens at submit time. The research spend cap counts
    -- in-flight batches by this, since their real cost is only logged on collect.
    read_cost_estimate REAL,
    read_submitted_at  TEXT,
    error           TEXT,
    created_at      TEXT    NOT NULL DEFAULT (datetime('now')),
    updated_at      TEXT    NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX IF NOT EXISTS idx_research_papers_status ON research_papers(status);
CREATE INDEX IF NOT EXISTS idx_research_papers_published ON research_papers(published_at);

-- Full text kept apart from the list table: ~150-250 KB per paper would bloat
-- every list query otherwise.
CREATE TABLE IF NOT EXISTS paper_texts (
    paper_id        INTEGER PRIMARY KEY,
    source          TEXT    NOT NULL,               -- 'html' | 'pdf'
    body            TEXT,                           -- extracted text; NULL when source = 'pdf'
    fetched_at      TEXT    NOT NULL DEFAULT (datetime('now'))
);

-- A deep read. More than one per paper is allowed (re-read with a newer model).
CREATE TABLE IF NOT EXISTS paper_reads (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    paper_id        INTEGER NOT NULL,
    model           TEXT    NOT NULL,
    study_json      TEXT    NOT NULL,
    input_tokens    INTEGER NOT NULL DEFAULT 0,
    output_tokens   INTEGER NOT NULL DEFAULT 0,
    cost_usd        REAL    NOT NULL DEFAULT 0,
    created_at      TEXT    NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX IF NOT EXISTS idx_paper_reads_paper ON paper_reads(paper_id);

-- A concrete change a paper suggests. `delta_json` is set only for changes the
-- what-if backtester can express (weights, thresholds, sizing, exits); anything
-- else (a new signal, new data) carries an implementation spec in `spec_md`.
CREATE TABLE IF NOT EXISTS paper_proposals (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    paper_id        INTEGER NOT NULL,
    read_id         INTEGER NOT NULL,
    kind            TEXT    NOT NULL,               -- param | new_signal | data | other
    component       TEXT    NOT NULL,
    title           TEXT    NOT NULL,
    rationale       TEXT    NOT NULL DEFAULT '',
    falsifier       TEXT    NOT NULL DEFAULT '',    -- what result would kill the idea
    delta_json      TEXT,
    spec_md         TEXT,
    -- proposed → tested → kept | rejected (a human decision; never applied automatically)
    status          TEXT    NOT NULL DEFAULT 'proposed',
    result_json     TEXT,
    tested_at       TEXT,
    created_at      TEXT    NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX IF NOT EXISTS idx_paper_proposals_paper ON paper_proposals(paper_id);

-- Small key/value state for the lane (e.g. last_ingest_at, so the hourly
-- fetcher wake does not query arXiv every hour).
CREATE TABLE IF NOT EXISTS research_state (
    key             TEXT PRIMARY KEY,
    value           TEXT NOT NULL,
    updated_at      TEXT NOT NULL DEFAULT (datetime('now'))
);
