-- Migration 005: Contextual retrieval prefixes
-- Adds context_prefix column to stories and rebuilds FTS5 to include it.
-- This implements Anthropic's contextual retrieval technique: each story gets
-- a Haiku-generated prefix that situates it within its sector, names entities,
-- and connects it to ongoing narratives — improving both FTS and semantic search.

ALTER TABLE stories ADD COLUMN context_prefix TEXT;

-- Must drop and recreate FTS5 since it doesn't support ALTER.
-- Drop triggers first, then table, then recreate everything.
DROP TRIGGER IF EXISTS stories_ai;
DROP TRIGGER IF EXISTS stories_ad;
DROP TRIGGER IF EXISTS stories_au;
DROP TABLE IF EXISTS stories_fts;

CREATE VIRTUAL TABLE stories_fts USING fts5(
    headline,
    summary,
    key_facts,
    why_it_matters,
    context_prefix,
    content='stories',
    content_rowid='id',
    tokenize='porter unicode61'
);

-- Recreate triggers including context_prefix
CREATE TRIGGER stories_ai AFTER INSERT ON stories BEGIN
    INSERT INTO stories_fts(rowid, headline, summary, key_facts, why_it_matters, context_prefix)
    VALUES (new.id, new.headline, new.summary, new.key_facts, new.why_it_matters, COALESCE(new.context_prefix, ''));
END;

CREATE TRIGGER stories_ad AFTER DELETE ON stories BEGIN
    INSERT INTO stories_fts(stories_fts, rowid, headline, summary, key_facts, why_it_matters, context_prefix)
    VALUES ('delete', old.id, old.headline, old.summary, old.key_facts, old.why_it_matters, COALESCE(old.context_prefix, ''));
END;

CREATE TRIGGER stories_au AFTER UPDATE ON stories BEGIN
    INSERT INTO stories_fts(stories_fts, rowid, headline, summary, key_facts, why_it_matters, context_prefix)
    VALUES ('delete', old.id, old.headline, old.summary, old.key_facts, old.why_it_matters, COALESCE(old.context_prefix, ''));
    INSERT INTO stories_fts(rowid, headline, summary, key_facts, why_it_matters, context_prefix)
    VALUES (new.id, new.headline, new.summary, new.key_facts, new.why_it_matters, COALESCE(new.context_prefix, ''));
END;

-- Rebuild FTS index from existing data
INSERT INTO stories_fts(rowid, headline, summary, key_facts, why_it_matters, context_prefix)
SELECT id, headline, summary, key_facts, why_it_matters, COALESCE(context_prefix, '') FROM stories;
