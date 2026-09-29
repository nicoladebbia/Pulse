-- Migration 036: track whether an entry order has filled, how many shares it
-- bought, and why each trade closed.
--
-- `order_status` separates "Alpaca accepted the buy" from "Alpaca filled it".
-- Auto-trade runs around 00:20, so its market orders sit unfilled until the
-- open. The row used to be written as a plain 'open' trade anyway; position
-- management then asked Alpaca for the position in the same run, got a 404,
-- and closed the row with NULL P&L. The buy filled at 9:30 into a position no
-- row tracked, and the next run bought the ticker again because nothing in the
-- DB was open. 37 rows between 2026-08-22 and 2026-09-28 closed this way, and
-- every one of their orders filled. A 'pending' row is settled against its
-- order id instead of against the position endpoint.
--
-- `filled_qty` is the share count Alpaca reported, so P&L can be computed from
-- shares instead of estimated from dollar notional / entry price.
--
-- `exit_reason` records which rule closed the trade (stop, signal decay,
-- manual, reconciliation). Before this the only record was a free-text journal
-- written on some paths and not others.
--
-- Existing rows default to 'filled': every order behind them has since settled.
ALTER TABLE paper_trades ADD COLUMN order_status TEXT NOT NULL DEFAULT 'filled'
    CHECK (order_status IN ('pending', 'filled'));
ALTER TABLE paper_trades ADD COLUMN filled_qty REAL;
ALTER TABLE paper_trades ADD COLUMN exit_reason TEXT;
