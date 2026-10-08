-- Event signals (crates/pulse-fetcher/src/event_signals.rs): a sudden change
-- in the tone of a company's news, and insider cluster buys. Each is a
-- separate way into a trade, with its own size and holding period.

-- One row per ticker, kind and day, recorded whether it was traded or not,
-- so the learning report can measure every event against SPY.
CREATE TABLE IF NOT EXISTS event_signals (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    kind        TEXT NOT NULL,      -- news_surprise | insider_cluster
    ticker      TEXT NOT NULL,
    entity_id   INTEGER,
    day         TEXT NOT NULL,      -- YYYY-MM-DD the event became known
    direction   TEXT NOT NULL,      -- long | short
    strength    REAL NOT NULL,      -- 0..1
    detail      TEXT NOT NULL DEFAULT '{}',
    detected_at TEXT NOT NULL,
    UNIQUE (kind, ticker, day)
);
CREATE INDEX IF NOT EXISTS idx_event_signals_day ON event_signals(day);

-- What opened a trade: 'convergence' (the cross-signal score) or an event
-- kind. Event trades skip signal decay and close after their maximum hold.
ALTER TABLE paper_trades ADD COLUMN entry_trigger TEXT NOT NULL DEFAULT 'convergence';
