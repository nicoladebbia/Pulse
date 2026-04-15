-- Migration 017: Financial data support
-- Adds finance sector, source_type/financial_metadata to stories,
-- expands entity types, adds financial infrastructure tables,
-- adds signal dimension columns.

-- =============================================================================
-- STORIES TABLE: Add finance sector + financial data columns
-- =============================================================================
-- Must recreate table because SQLite CHECK constraints can't be altered.
-- The existing sector CHECK is ('ai','miami','italy','tech') — need to add 'finance'.

CREATE TABLE stories_new (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    briefing_id     INTEGER NOT NULL REFERENCES briefings(id),
    sector          TEXT NOT NULL CHECK(sector IN ('ai', 'miami', 'italy', 'tech', 'finance')),
    original_title  TEXT NOT NULL,
    original_url    TEXT NOT NULL,
    original_language TEXT NOT NULL DEFAULT 'en',
    content_snippet TEXT,
    source_name     TEXT NOT NULL,
    published_at    TEXT,
    headline        TEXT NOT NULL,
    summary         TEXT NOT NULL,
    key_facts       TEXT NOT NULL,
    why_it_matters  TEXT NOT NULL,
    what_to_watch   TEXT NOT NULL,
    importance_score INTEGER NOT NULL DEFAULT 5,
    relevance_score  INTEGER,
    relevance_reason TEXT,
    is_hero         INTEGER NOT NULL DEFAULT 0,
    display_order   INTEGER NOT NULL DEFAULT 0,
    created_at      TEXT NOT NULL DEFAULT (datetime('now')),
    url_hash        TEXT NOT NULL,
    title_hash      TEXT NOT NULL,
    context_prefix  TEXT,
    summary_depth   TEXT DEFAULT 'standard',
    deep_summary    TEXT,
    sentiment       REAL,
    novelty         REAL,
    event_type      TEXT,
    -- New financial columns
    source_type     TEXT NOT NULL DEFAULT 'news' CHECK(source_type IN ('news', 'financial')),
    financial_metadata TEXT  -- JSON blob for structured financial data
);

-- Copy existing data with explicit column lists
INSERT INTO stories_new (
    id, briefing_id, sector, original_title, original_url, original_language,
    content_snippet, source_name, published_at, headline, summary, key_facts,
    why_it_matters, what_to_watch, importance_score, relevance_score, relevance_reason,
    is_hero, display_order, created_at, url_hash, title_hash,
    context_prefix, summary_depth, deep_summary, sentiment, novelty, event_type,
    source_type, financial_metadata
)
SELECT
    id, briefing_id, sector, original_title, original_url, original_language,
    content_snippet, source_name, published_at, headline, summary, key_facts,
    why_it_matters, what_to_watch, importance_score, relevance_score, relevance_reason,
    is_hero, display_order, created_at, url_hash, title_hash,
    context_prefix, summary_depth, deep_summary, sentiment, novelty, event_type,
    'news', NULL
FROM stories;

DROP TABLE stories;
ALTER TABLE stories_new RENAME TO stories;

-- Recreate indexes on stories
CREATE INDEX IF NOT EXISTS idx_stories_briefing ON stories(briefing_id);
CREATE INDEX IF NOT EXISTS idx_stories_sector ON stories(sector);
CREATE INDEX IF NOT EXISTS idx_stories_date ON stories(created_at);
CREATE INDEX IF NOT EXISTS idx_stories_url_hash ON stories(url_hash);
CREATE INDEX IF NOT EXISTS idx_stories_title_hash ON stories(title_hash);
CREATE INDEX IF NOT EXISTS idx_stories_source_type ON stories(source_type);
CREATE INDEX IF NOT EXISTS idx_stories_published_at ON stories(published_at);

-- Recreate FTS table (content sync depends on stories table existing)
DROP TABLE IF EXISTS stories_fts;
CREATE VIRTUAL TABLE stories_fts USING fts5(
    headline, summary, key_facts, why_it_matters, context_prefix,
    content='stories',
    content_rowid='id'
);
INSERT INTO stories_fts(rowid, headline, summary, key_facts, why_it_matters, context_prefix)
    SELECT id, headline, summary, key_facts, why_it_matters, context_prefix FROM stories;

-- Recreate FTS triggers
DROP TRIGGER IF EXISTS stories_fts_insert;
DROP TRIGGER IF EXISTS stories_fts_delete;
DROP TRIGGER IF EXISTS stories_fts_update;

CREATE TRIGGER stories_fts_insert AFTER INSERT ON stories BEGIN
    INSERT INTO stories_fts(rowid, headline, summary, key_facts, why_it_matters, context_prefix)
    VALUES (NEW.id, NEW.headline, NEW.summary, NEW.key_facts, NEW.why_it_matters, NEW.context_prefix);
END;

CREATE TRIGGER stories_fts_delete AFTER DELETE ON stories BEGIN
    INSERT INTO stories_fts(stories_fts, rowid, headline, summary, key_facts, why_it_matters, context_prefix)
    VALUES ('delete', OLD.id, OLD.headline, OLD.summary, OLD.key_facts, OLD.why_it_matters, OLD.context_prefix);
END;

CREATE TRIGGER stories_fts_update AFTER UPDATE ON stories BEGIN
    INSERT INTO stories_fts(stories_fts, rowid, headline, summary, key_facts, why_it_matters, context_prefix)
    VALUES ('delete', OLD.id, OLD.headline, OLD.summary, OLD.key_facts, OLD.why_it_matters, OLD.context_prefix);
    INSERT INTO stories_fts(rowid, headline, summary, key_facts, why_it_matters, context_prefix)
    VALUES (NEW.id, NEW.headline, NEW.summary, NEW.key_facts, NEW.why_it_matters, NEW.context_prefix);
END;

-- =============================================================================
-- ENTITIES TABLE: Expand entity types for financial data
-- =============================================================================

CREATE TABLE entities_new (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    name            TEXT NOT NULL,
    name_normalized TEXT NOT NULL,
    entity_type     TEXT NOT NULL CHECK(entity_type IN (
        'company', 'person', 'topic', 'product', 'regulation',
        'insider_trade', 'contract_award', 'patent_cluster',
        'lobbying_disclosure', 'institutional_holding',
        'private_placement', 'material_event', 'regulatory_action'
    )),
    sector          TEXT,
    first_seen      TEXT NOT NULL,
    last_seen       TEXT NOT NULL,
    mention_count   INTEGER NOT NULL DEFAULT 1,
    sentiment_avg   REAL NOT NULL DEFAULT 0.0,
    created_at      TEXT NOT NULL DEFAULT (datetime('now'))
);

INSERT INTO entities_new (
    id, name, name_normalized, entity_type, sector,
    first_seen, last_seen, mention_count, sentiment_avg, created_at
)
SELECT
    id, name, name_normalized, entity_type, sector,
    first_seen, last_seen, mention_count, sentiment_avg, created_at
FROM entities;

DROP TABLE entities;
ALTER TABLE entities_new RENAME TO entities;

CREATE UNIQUE INDEX IF NOT EXISTS idx_entities_name_type ON entities(name_normalized, entity_type);
CREATE INDEX IF NOT EXISTS idx_entities_sector ON entities(sector);
CREATE INDEX IF NOT EXISTS idx_entities_mention_count ON entities(mention_count DESC);
CREATE INDEX IF NOT EXISTS idx_entities_type ON entities(entity_type);

-- =============================================================================
-- FINANCIAL INFRASTRUCTURE TABLES
-- =============================================================================

-- Entity-to-ticker mapping (public companies)
CREATE TABLE IF NOT EXISTS entity_tickers (
    entity_id   INTEGER PRIMARY KEY REFERENCES entities(id),
    ticker      TEXT NOT NULL,
    exchange    TEXT,
    cik         TEXT,
    is_public   INTEGER DEFAULT 1,
    confidence  REAL DEFAULT 1.0,
    last_verified TEXT,
    created_at  TEXT DEFAULT (datetime('now'))
);
CREATE INDEX IF NOT EXISTS idx_entity_tickers_ticker ON entity_tickers(ticker);
CREATE INDEX IF NOT EXISTS idx_entity_tickers_cik ON entity_tickers(cik);

-- Market price data (daily OHLCV)
CREATE TABLE IF NOT EXISTS entity_prices (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    entity_id   INTEGER REFERENCES entities(id),
    ticker      TEXT NOT NULL,
    date        TEXT NOT NULL,
    open        REAL,
    close       REAL NOT NULL,
    high        REAL,
    low         REAL,
    volume      INTEGER,
    change_1d   REAL,
    change_7d   REAL,
    change_30d  REAL,
    created_at  TEXT DEFAULT (datetime('now')),
    UNIQUE(ticker, date)
);
CREATE INDEX IF NOT EXISTS idx_entity_prices_entity ON entity_prices(entity_id, date);
CREATE INDEX IF NOT EXISTS idx_entity_prices_ticker ON entity_prices(ticker, date);

-- Cross-signal composite scores
CREATE TABLE IF NOT EXISTS cross_signals (
    id                  INTEGER PRIMARY KEY AUTOINCREMENT,
    entity_id           INTEGER REFERENCES entities(id),
    ticker              TEXT,
    compound_score      REAL NOT NULL,
    insider_signal      REAL DEFAULT 0,
    institutional_flow  REAL DEFAULT 0,
    news_momentum       REAL DEFAULT 0,
    government_signal   REAL DEFAULT 0,
    search_trend        REAL DEFAULT 0,
    patent_signal       REAL DEFAULT 0,
    supply_chain        REAL DEFAULT 0,
    political_signal    REAL DEFAULT 0,
    source_diversity    INTEGER DEFAULT 0,
    convergence_detected INTEGER DEFAULT 0,
    computed_at         TEXT DEFAULT (datetime('now'))
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_cross_signals_entity_date ON cross_signals(entity_id, date(computed_at));
CREATE INDEX IF NOT EXISTS idx_cross_signals_entity ON cross_signals(entity_id);
CREATE INDEX IF NOT EXISTS idx_cross_signals_convergence ON cross_signals(convergence_detected);
CREATE INDEX IF NOT EXISTS idx_cross_signals_score ON cross_signals(compound_score);

-- Paper trading journal
CREATE TABLE IF NOT EXISTS paper_trades (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    entity_id       INTEGER REFERENCES entities(id),
    ticker          TEXT NOT NULL,
    direction       TEXT NOT NULL CHECK(direction IN ('long', 'short')),
    entry_price     REAL NOT NULL,
    entry_date      TEXT NOT NULL,
    exit_price      REAL,
    exit_date       TEXT,
    position_size   REAL NOT NULL,
    confidence      REAL NOT NULL,
    signal_profile  TEXT NOT NULL,
    prediction_id   INTEGER REFERENCES insights(id),
    alpaca_order_id TEXT,
    status          TEXT NOT NULL DEFAULT 'open' CHECK(status IN ('open', 'closed', 'stopped_out', 'expired')),
    pnl             REAL,
    pnl_pct         REAL,
    created_at      TEXT DEFAULT (datetime('now'))
);
CREATE INDEX IF NOT EXISTS idx_paper_trades_status ON paper_trades(status);
CREATE INDEX IF NOT EXISTS idx_paper_trades_entity ON paper_trades(entity_id);

-- Backtest results
CREATE TABLE IF NOT EXISTS backtest_results (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    signal_profile  TEXT NOT NULL,
    start_date      TEXT NOT NULL,
    end_date        TEXT NOT NULL,
    total_signals   INTEGER NOT NULL,
    hit_rate        REAL NOT NULL,
    avg_return      REAL,
    max_drawdown    REAL,
    sharpe_ratio    REAL,
    avg_holding_days REAL,
    details         TEXT,
    created_at      TEXT DEFAULT (datetime('now'))
);

-- Financial source deduplication
CREATE TABLE IF NOT EXISTS financial_dedup (
    source_type TEXT NOT NULL,
    source_id   TEXT NOT NULL,
    story_id    INTEGER REFERENCES stories(id),
    fetched_at  TEXT DEFAULT (datetime('now')),
    PRIMARY KEY (source_type, source_id)
);

-- =============================================================================
-- SIGNALS TABLE: Add financial signal dimensions
-- =============================================================================

ALTER TABLE signals ADD COLUMN insider_buy_volume REAL DEFAULT 0;
ALTER TABLE signals ADD COLUMN institutional_flow REAL DEFAULT 0;
ALTER TABLE signals ADD COLUMN contract_value REAL DEFAULT 0;
ALTER TABLE signals ADD COLUMN patent_filing_rate REAL DEFAULT 0;
ALTER TABLE signals ADD COLUMN search_trend_delta REAL DEFAULT 0;
ALTER TABLE signals ADD COLUMN import_volume_delta REAL DEFAULT 0;
ALTER TABLE signals ADD COLUMN regulatory_sentiment REAL DEFAULT 0;
ALTER TABLE signals ADD COLUMN lobbying_spend_delta REAL DEFAULT 0;
ALTER TABLE signals ADD COLUMN source_diversity INTEGER DEFAULT 0;
