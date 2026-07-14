-- Migration 026: Drop stillborn trend-tracking tables.
--
-- trend_threads and trend_points were defined in 001_initial_schema.sql for a
-- topic-evolution feature that was never implemented: 0 rows, no writer in either
-- binary, no reader. Confirmed dead 2026-06-25 (grep across src-tauri + pulse-fetcher
-- found zero INSERT/SELECT references). Removing dead schema per the
-- delete-abandoned-experiments rule. If trend tracking is built later, it gets a
-- fresh, purpose-built schema — not these stubs.
--
-- DROP order: trend_points may FK-reference trend_threads, so drop the child first.

DROP TABLE IF EXISTS trend_points;
DROP TABLE IF EXISTS trend_threads;
