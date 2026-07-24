-- Migration 029: Universe quality gate cache (calibration-backtest-universe
-- Task 5.1/5.2, 2026-07-24). Rejects sub-$300M market cap / sub-$1 price
-- tickers, and requires Alpaca to confirm the symbol is actually tradable,
-- before a signal can enter the auto-trade buy path. Built after the
-- 290-NULL-ticker candidate audit found that fixing the client-entity ticker
-- mapping bug (lobbying "client" fields wrongly typed to exclude ticker
-- mapping) would newly expose trade-association/nonprofit names to fuzzy
-- ticker matching — this gate is the safety net that catches a bad match
-- before it reaches real orders. Cached with a TTL so the live auto-trade
-- loop doesn't re-hit Finnhub/Alpaca every cycle for the same name.
CREATE TABLE IF NOT EXISTS ticker_eligibility_cache (
    ticker              TEXT PRIMARY KEY,
    market_cap_millions REAL,
    last_price          REAL,
    alpaca_tradable     INTEGER NOT NULL DEFAULT 0,
    alpaca_status       TEXT,
    eligible            INTEGER NOT NULL DEFAULT 0,
    checked_at          TEXT NOT NULL
);
