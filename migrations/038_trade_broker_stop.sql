-- Migration 038: a real stop order at the broker for each open trade.
--
-- Stops used to live only in this database and were checked when the fetcher
-- ran (hourly at best). A gap or a fast drop between runs sold far below the
-- stop: ARM closed at -16.4% against a -15% hard stop. Each open trade now
-- keeps a GTC sell-stop at Alpaca for its whole shares (fractional orders are
-- day-only, so the fractional remainder is sold when the stop fills).
--
-- `stop_order_id` is the live Alpaca order; `broker_stop_price` its stop price,
-- which only ever moves up. `broker_stop_filled_at` is set once the stop has
-- sold its shares, so the close that follows (the fractional remainder, often
-- in a later run) is still recorded as a stop-out at the stop's time.
ALTER TABLE paper_trades ADD COLUMN stop_order_id TEXT;
ALTER TABLE paper_trades ADD COLUMN broker_stop_price REAL;
ALTER TABLE paper_trades ADD COLUMN broker_stop_filled_at TEXT;
