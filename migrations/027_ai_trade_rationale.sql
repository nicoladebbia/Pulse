-- Migration 027: Cache an AI-generated plain-English rationale per trade.
--
-- The Signals page and trade detail page already render the RAW signal
-- scores/headlines that fired a trade (signal_profile JSON). This adds a
-- cached natural-language explanation generated once (Claude Haiku) from
-- that same data, so repeat views never re-call the API.
ALTER TABLE paper_trades ADD COLUMN ai_rationale TEXT;
ALTER TABLE paper_trades ADD COLUMN ai_rationale_at TEXT;
