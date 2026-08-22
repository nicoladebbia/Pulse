-- 031: Remove tables nothing reads or writes, backfill the normalised evidence links,
-- and put a retention policy on api_usage.
--
-- Audited 2026-08-15 against the live DB (row counts) AND the source tree (references
-- outside migrations/). Only tables with ZERO rows and ZERO code references are dropped —
-- a table with a live reader is a broken feature to repair, not dead weight to delete.
--
--   fetch_log          0 rows, 0 refs  -> drop
--   rag_eval_queries   0 rows, 0 refs  -> drop  (RAG eval tables with no eval harness)
--   rag_eval_runs      0 rows, 0 refs  -> drop
--   feed_health        0 rows, 0 refs  -> drop  (pipeline_health.feeds_failed now covers it)
--   alerts             0 rows, 1 ref   -> drop  (the "reference" is a comment in
--                                                connection.rs:223, not code)
--
-- DELIBERATELY KEPT, though also empty — each has live readers, so dropping them would
-- break code rather than remove cruft:
--   chat_feedback        empty because no chat response was ever rated (usage, not a bug)
--   feedback_reputation  derived from chat_feedback, so empty by consequence
--   entity_relationships has 3 readers; emptiness is unexplained and worth its own look
--   insight_evidence     repaired below rather than dropped

DROP TABLE IF EXISTS fetch_log;
DROP TABLE IF EXISTS rag_eval_queries;
DROP TABLE IF EXISTS rag_eval_runs;
DROP TABLE IF EXISTS feed_health;
DROP TABLE IF EXISTS alerts;

-- Backfill insight_evidence from the denormalised column so the app-side readers
-- (patterns.rs:414, predictions.rs:762) stop returning 0 for every prediction. Runs AFTER
-- migration 030, so only citations that survived the recency test are linked — the wrong
-- ones are not resurrected here. New predictions write both copies at insert time.
INSERT OR IGNORE INTO insight_evidence (insight_id, story_id, role)
SELECT i.id, CAST(je.value AS INTEGER), 'support'
FROM insights i, json_each(i.source_story_ids) je
WHERE i.insight_type = 'prediction'
  AND json_valid(i.source_story_ids)
  AND i.source_story_ids NOT IN ('', '[]')
  AND EXISTS (SELECT 1 FROM stories s WHERE s.id = CAST(je.value AS INTEGER));

-- api_usage retention. 329,719 rows accumulated since 2026-04-10 with no policy, in a
-- 357MB database. The cost cap only ever reads `date(created_at) = date('now')` and the
-- reporting views look back weeks, so 90 days is generous. The daily pipeline re-applies
-- this so the table cannot grow without bound again.
DELETE FROM api_usage WHERE created_at < datetime('now', '-90 day');

-- No new index here on purpose: `idx_api_usage_date` is ALREADY `api_usage(created_at)`,
-- so adding idx_api_usage_created would be an exact duplicate costing ~8.5MB for nothing
-- (measured with dbstat). `idx_api_usage_provider` is (provider, created_at) and covers
-- the per-provider reports. The prune above already plans as
-- `SEARCH api_usage USING INDEX idx_api_usage_date (created_at<?)`.
--
-- The cost-cap query was the real inefficiency: `WHERE date(created_at) = date('now')`
-- wraps the column in a function, which defeats every index and made a full SCAN of
-- 314k rows run on every single pipeline start. It is now a range predicate in
-- `pipeline::check_daily_cost_cap` — see that function's spent_today SQL.
