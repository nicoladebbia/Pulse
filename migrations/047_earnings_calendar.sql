-- Migration 047: upcoming earnings reports for the tickers Pulse follows.
--
-- Filled by the fetcher from Finnhub's earnings calendar (one call per
-- refresh). Only tickers Pulse tracks or holds are kept. Future rows of a
-- ticker are replaced on each refresh because report dates move.
CREATE TABLE IF NOT EXISTS earnings_calendar (
    ticker           TEXT NOT NULL,
    report_date      TEXT NOT NULL,
    hour             TEXT,
    eps_estimate     REAL,
    revenue_estimate REAL,
    fetched_at       TEXT NOT NULL DEFAULT (datetime('now')),
    PRIMARY KEY (ticker, report_date)
);
CREATE INDEX IF NOT EXISTS idx_earnings_calendar_date ON earnings_calendar(report_date);
