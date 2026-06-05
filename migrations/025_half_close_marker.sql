-- Migration 025: Add half_closed_at marker to paper_trades.
--
-- The position-exit engine (position_management::evaluate_position) can return
-- CloseHalf when a position hits its 3x-ATR profit target. Without a marker, the
-- next pipeline run would re-trigger the same half-close every day. This column
-- records when a position was half-closed; the exit loop skips CloseHalf once set.
ALTER TABLE paper_trades ADD COLUMN half_closed_at TEXT;
