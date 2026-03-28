-- FTS5 virtual table for full-text search across stories
CREATE VIRTUAL TABLE IF NOT EXISTS stories_fts USING fts5(
    headline,
    summary,
    key_facts,
    why_it_matters,
    content='stories',
    content_rowid='id',
    tokenize='porter unicode61'
);

-- Triggers to keep FTS in sync
CREATE TRIGGER IF NOT EXISTS stories_ai AFTER INSERT ON stories BEGIN
    INSERT INTO stories_fts(rowid, headline, summary, key_facts, why_it_matters)
    VALUES (new.id, new.headline, new.summary, new.key_facts, new.why_it_matters);
END;

CREATE TRIGGER IF NOT EXISTS stories_ad AFTER DELETE ON stories BEGIN
    INSERT INTO stories_fts(stories_fts, rowid, headline, summary, key_facts, why_it_matters)
    VALUES ('delete', old.id, old.headline, old.summary, old.key_facts, old.why_it_matters);
END;

CREATE TRIGGER IF NOT EXISTS stories_au AFTER UPDATE ON stories BEGIN
    INSERT INTO stories_fts(stories_fts, rowid, headline, summary, key_facts, why_it_matters)
    VALUES ('delete', old.id, old.headline, old.summary, old.key_facts, old.why_it_matters);
    INSERT INTO stories_fts(rowid, headline, summary, key_facts, why_it_matters)
    VALUES (new.id, new.headline, new.summary, new.key_facts, new.why_it_matters);
END;
