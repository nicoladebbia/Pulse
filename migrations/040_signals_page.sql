-- Migration 040: data behind the Signals page's newer cards.
--
-- trade_decisions: what each auto-trade run did with each candidate — bought,
-- previewed, or skipped and why (earnings soon, too calm, sector full ...).
-- A run-level stop (market closed, no buying power) is one row with an empty
-- ticker. Rows older than 30 days are pruned by the fetcher.
CREATE TABLE IF NOT EXISTS trade_decisions (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    run_at     TEXT NOT NULL,
    ticker     TEXT NOT NULL DEFAULT '',
    name       TEXT,
    score      REAL,
    outcome    TEXT NOT NULL,  -- bought | preview | skipped | run_stopped
    reason     TEXT NOT NULL,  -- short machine key, e.g. earnings, too_calm
    detail     TEXT
);
CREATE INDEX IF NOT EXISTS idx_trade_decisions_run ON trade_decisions(run_at);

-- SPY at the moment a trade was bought and sold, so each trade can be judged
-- against simply holding the S&P 500 for the same days. Older trades fall back
-- to daily bars.
ALTER TABLE paper_trades ADD COLUMN spy_entry_price REAL;
ALTER TABLE paper_trades ADD COLUMN spy_exit_price REAL;

-- signal_scorecard: the weekly answer to "which signals make money". One row
-- per group (a signal dimension that fired, or a score range), from every
-- buy-grade signal of the last 120 days and its next 10 trading days vs SPY.
CREATE TABLE IF NOT EXISTS signal_scorecard (
    computed_at   TEXT NOT NULL,
    grp_kind      TEXT NOT NULL,  -- dimension | score
    grp           TEXT NOT NULL,
    signals       INTEGER NOT NULL,
    win_rate      REAL,           -- share beating SPY
    avg_excess    REAL,           -- mean 10-day return minus SPY, in %
    median_excess REAL,
    trades        INTEGER NOT NULL DEFAULT 0,
    trade_pnl     REAL,           -- realized $ of closed trades in the group
    trade_win_rate REAL,
    PRIMARY KEY (computed_at, grp_kind, grp)
);
