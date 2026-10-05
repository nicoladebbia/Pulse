-- Migration 039: each ticker's industry, for the sector cap on new buys.
--
-- From Finnhub's company profile (`finnhubIndustry`). Industries change
-- rarely, so a row is refreshed only after 30 days.
CREATE TABLE IF NOT EXISTS ticker_industry (
    ticker     TEXT PRIMARY KEY,
    industry   TEXT,
    checked_at TEXT NOT NULL
);
