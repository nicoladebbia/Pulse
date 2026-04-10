-- Migration 006: Freedom stories search support
-- Adds context_prefix column and FTS5 index for freedom_stories,
-- making them searchable via the chat system.

ALTER TABLE freedom_stories ADD COLUMN context_prefix TEXT;

-- FTS5 index for freedom stories (parallel to stories_fts)
CREATE VIRTUAL TABLE IF NOT EXISTS freedom_stories_fts USING fts5(
    headline,
    summary,
    key_facts,
    why_it_matters,
    context_prefix,
    content='freedom_stories',
    content_rowid='id',
    tokenize='porter unicode61'
);

-- Triggers to keep FTS in sync
CREATE TRIGGER IF NOT EXISTS freedom_stories_fts_ai AFTER INSERT ON freedom_stories BEGIN
    INSERT INTO freedom_stories_fts(rowid, headline, summary, key_facts, why_it_matters, context_prefix)
    VALUES (new.id, new.headline, new.summary, new.key_facts, new.why_it_matters, COALESCE(new.context_prefix, ''));
END;

CREATE TRIGGER IF NOT EXISTS freedom_stories_fts_ad AFTER DELETE ON freedom_stories BEGIN
    INSERT INTO freedom_stories_fts(freedom_stories_fts, rowid, headline, summary, key_facts, why_it_matters, context_prefix)
    VALUES ('delete', old.id, old.headline, old.summary, old.key_facts, old.why_it_matters, COALESCE(old.context_prefix, ''));
END;

CREATE TRIGGER IF NOT EXISTS freedom_stories_fts_au AFTER UPDATE ON freedom_stories BEGIN
    INSERT INTO freedom_stories_fts(freedom_stories_fts, rowid, headline, summary, key_facts, why_it_matters, context_prefix)
    VALUES ('delete', old.id, old.headline, old.summary, old.key_facts, old.why_it_matters, COALESCE(old.context_prefix, ''));
    INSERT INTO freedom_stories_fts(rowid, headline, summary, key_facts, why_it_matters, context_prefix)
    VALUES (new.id, new.headline, new.summary, new.key_facts, new.why_it_matters, COALESCE(new.context_prefix, ''));
END;

-- Populate FTS from existing data
INSERT INTO freedom_stories_fts(rowid, headline, summary, key_facts, why_it_matters, context_prefix)
SELECT id, headline, summary, key_facts, why_it_matters, COALESCE(context_prefix, '') FROM freedom_stories;
