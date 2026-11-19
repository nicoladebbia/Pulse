-- Migration 049: what prediction markets say, for the briefing page.
--
-- Filled by the fetcher (`crates/pulse-fetcher/src/prediction_markets.rs`) from
-- Polymarket's public market list: a handful of yes/no questions per reading,
-- the ones whose odds moved most in 24 hours and the ones with the most money
-- traded. It is something to read next to the news. Nothing here is a trading
-- signal and no trading code reads this table.
--
-- One reading per local day: a later reading the same day replaces the earlier
-- one, so `day` is the day shown and `fetched_at` (local time) says how old the
-- numbers are. Days are kept 180 days (`prediction_markets::KEEP_DAYS`).
--
--   probability  chance of "Yes" the market's price implies, 0..1
--   change_1d    how much that chance moved in 24 hours, in the same unit
--                (0.05 = five points); NULL when the source gave none
--   volume_24h   dollars traded in 24 hours
--   kind         'mover' (odds moved at least five points) or 'active'
--   rank         display order within the day, movers first
CREATE TABLE IF NOT EXISTS prediction_markets (
    day          TEXT    NOT NULL,
    source       TEXT    NOT NULL DEFAULT 'polymarket',
    market_id    TEXT    NOT NULL,
    topic        TEXT    NOT NULL,
    question     TEXT    NOT NULL,
    event_title  TEXT,
    url          TEXT,
    probability  REAL    NOT NULL,
    change_1d    REAL,
    change_1w    REAL,
    volume_24h   REAL    NOT NULL DEFAULT 0,
    liquidity    REAL,
    end_date     TEXT,
    kind         TEXT    NOT NULL,
    rank         INTEGER NOT NULL,
    fetched_at   TEXT    NOT NULL,
    PRIMARY KEY (day, source, market_id)
);
