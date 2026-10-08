-- Open trades bought before 042 were scored when news momentum counted every
-- mention (Wikipedia pageviews, filings), so their original compound score
-- sits far above anything the corrected formula gives the same stock. Signal
-- decay compares against that score and would sell almost all of them at
-- once. Each is re-anchored to its first score computed after this date
-- (`rebase_original_score` in pipeline/trading.rs); decay waits until then.
ALTER TABLE paper_trades ADD COLUMN score_rebase_after TEXT;
UPDATE paper_trades SET score_rebase_after = date('now', 'localtime') WHERE status = 'open';
