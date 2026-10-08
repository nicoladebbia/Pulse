-- Learning from every trade (crates/pulse-fetcher/src/learning.rs).

-- What the system knew when it bought: score, every signal dimension, the
-- candidates it beat, price/volatility/liquidity, market regime, sizing
-- inputs, the stories behind it and the rules version. JSON.
ALTER TABLE paper_trades ADD COLUMN entry_context TEXT;

-- Everything that happened to a trade, in order: open, half_close,
-- partial_sale, stop_sale, resync, close.
CREATE TABLE IF NOT EXISTS trade_events (
    id        INTEGER PRIMARY KEY AUTOINCREMENT,
    trade_id  INTEGER NOT NULL REFERENCES paper_trades(id) ON DELETE CASCADE,
    at        TEXT NOT NULL,
    kind      TEXT NOT NULL,
    price     REAL,
    qty       REAL,
    reason    TEXT,
    detail    TEXT
);
CREATE INDEX IF NOT EXISTS idx_trade_events_trade ON trade_events(trade_id, at);

-- One review per closed trade: return vs SPY, best/worst point while held,
-- the price 5/10/20 trading days after the exit, lesson tags and a
-- plain-language story. Redone daily until the 20-day window is complete.
CREATE TABLE IF NOT EXISTS trade_reviews (
    trade_id    INTEGER PRIMARY KEY REFERENCES paper_trades(id) ON DELETE CASCADE,
    computed_at TEXT NOT NULL,
    hold_days   INTEGER NOT NULL,
    return_pct  REAL NOT NULL,
    spy_pct     REAL,
    excess_pct  REAL,
    mfe_pct     REAL,
    mae_pct     REAL,
    after5_pct  REAL,
    after10_pct REAL,
    after20_pct REAL,
    exit_kind   TEXT NOT NULL,
    lessons     TEXT NOT NULL DEFAULT '[]',
    story       TEXT NOT NULL,
    is_final    INTEGER NOT NULL DEFAULT 0
);

-- The daily learning report (JSON body), and whether it changed the weights.
CREATE TABLE IF NOT EXISTS learning_reports (
    computed_at     TEXT PRIMARY KEY,
    weights_applied INTEGER NOT NULL DEFAULT 0,
    body            TEXT NOT NULL
);
