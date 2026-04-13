-- Entity aliases: structured ticker/acronym/alternate name mapping
CREATE TABLE IF NOT EXISTS entity_aliases (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    entity_id INTEGER NOT NULL REFERENCES entities(id),
    alias TEXT NOT NULL,
    alias_type TEXT NOT NULL CHECK(alias_type IN ('ticker', 'acronym', 'abbreviation', 'alternate_name')),
    created_at DATETIME DEFAULT CURRENT_TIMESTAMP,
    UNIQUE(entity_id, alias)
);

CREATE INDEX IF NOT EXISTS idx_entity_aliases_alias ON entity_aliases(LOWER(alias));
