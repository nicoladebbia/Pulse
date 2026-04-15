-- Migration 019: Position management — ATR trailing stops, profit targets, signal decay
-- Adds tracking columns to paper_trades for adaptive exit management.

-- Portfolio snapshots for drawdown circuit breaker
CREATE TABLE IF NOT EXISTS portfolio_snapshots (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    date            TEXT NOT NULL UNIQUE,
    total_value     REAL NOT NULL,
    total_pnl       REAL NOT NULL,
    total_pnl_pct   REAL NOT NULL,
    open_positions  INTEGER NOT NULL,
    high_water_mark REAL NOT NULL,
    drawdown_pct    REAL NOT NULL,
    regime          TEXT,
    created_at      TEXT DEFAULT (datetime('now'))
);
