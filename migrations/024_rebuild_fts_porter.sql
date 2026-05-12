-- Migration 024: Rebuild stories_fts with porter unicode61 tokenizer
-- and drop orphan triggers / indexes from earlier migrations.
--
-- Audit findings addressed (2026-05-12):
--   #32: Migration 005 created triggers stories_ai / stories_ad / stories_au.
--        Migration 017 attempted to DROP "stories_fts_insert/_delete/_update"
--        (names that didn't yet exist as those identifiers), so the migration-005
--        triggers were never removed. They persisted alongside the new triggers,
--        causing duplicate or conflicting FTS writes.
--   #33: Migration 017's CREATE VIRTUAL TABLE stories_fts (as written in
--        migrations/017_financial_data.sql) omits tokenize='porter unicode61'.
--        connection.rs has its own inline copy with porter (correct), but
--        crates/pulse-fetcher/src/db.rs has an inline copy WITHOUT porter (bug).
--        Whichever process opened the DB first determined the tokenizer; this
--        rebuild guarantees porter regardless of which writer ran first.
--   #34: Same as #33 — fetcher's inline migration 17 is also patched in this
--        commit so fresh-DB installs get porter from day one.
--   #35: Migration 003 created idx_signals_topic on signals(topic, sector).
--        Migration 010 created idx_signals_topic_sector on the same columns
--        without dropping the old one. Both indexes exist; this drops the
--        orphan.
--
-- Note on finding #31: Migration 011 silently dropped 5 columns
-- (original_title, original_language, content_snippet, source_url,
-- relevance_reason) from freedom_stories during table rebuild. Those rows are
-- IRRECOVERABLE — the data was discarded at migration time. Migration 016
-- restored the schema (NOT NULL with defaults), so the schema is sound and
-- new rows are fine. Historical rows that pre-date migration 011 have NULL/
-- default values in those columns. This migration does NOT attempt to backfill.

-- =============================================================================
-- Drop orphan triggers from migration 005 (never dropped by migration 017)
-- =============================================================================

DROP TRIGGER IF EXISTS stories_ai;
DROP TRIGGER IF EXISTS stories_ad;
DROP TRIGGER IF EXISTS stories_au;

-- Drop migration-017 triggers (idempotent — we recreate them below)
DROP TRIGGER IF EXISTS stories_fts_insert;
DROP TRIGGER IF EXISTS stories_fts_delete;
DROP TRIGGER IF EXISTS stories_fts_update;

-- =============================================================================
-- Drop orphan index from migration 003 (superseded by migration 010)
-- =============================================================================

DROP INDEX IF EXISTS idx_signals_topic;

-- =============================================================================
-- Rebuild stories_fts with porter unicode61 tokenizer
-- =============================================================================

DROP TABLE IF EXISTS stories_fts;

CREATE VIRTUAL TABLE stories_fts USING fts5(
    headline, summary, key_facts, why_it_matters, context_prefix,
    content='stories',
    content_rowid='id',
    tokenize='porter unicode61'
);

-- Reindex all existing stories with the porter tokenizer.
-- COALESCE handles NULL context_prefix on rows that pre-date migration 005.
INSERT INTO stories_fts(rowid, headline, summary, key_facts, why_it_matters, context_prefix)
SELECT id, headline, summary, key_facts, why_it_matters, COALESCE(context_prefix, '')
FROM stories;

-- =============================================================================
-- Recreate FTS triggers with porter-tokenizer-aware writes
-- =============================================================================

CREATE TRIGGER stories_fts_insert AFTER INSERT ON stories BEGIN
    INSERT INTO stories_fts(rowid, headline, summary, key_facts, why_it_matters, context_prefix)
    VALUES (NEW.id, NEW.headline, NEW.summary, NEW.key_facts, NEW.why_it_matters, COALESCE(NEW.context_prefix, ''));
END;

CREATE TRIGGER stories_fts_delete AFTER DELETE ON stories BEGIN
    INSERT INTO stories_fts(stories_fts, rowid, headline, summary, key_facts, why_it_matters, context_prefix)
    VALUES ('delete', OLD.id, OLD.headline, OLD.summary, OLD.key_facts, OLD.why_it_matters, COALESCE(OLD.context_prefix, ''));
END;

CREATE TRIGGER stories_fts_update AFTER UPDATE ON stories BEGIN
    INSERT INTO stories_fts(stories_fts, rowid, headline, summary, key_facts, why_it_matters, context_prefix)
    VALUES ('delete', OLD.id, OLD.headline, OLD.summary, OLD.key_facts, OLD.why_it_matters, COALESCE(OLD.context_prefix, ''));
    INSERT INTO stories_fts(rowid, headline, summary, key_facts, why_it_matters, context_prefix)
    VALUES (NEW.id, NEW.headline, NEW.summary, NEW.key_facts, NEW.why_it_matters, COALESCE(NEW.context_prefix, ''));
END;
