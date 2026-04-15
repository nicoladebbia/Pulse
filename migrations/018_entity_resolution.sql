-- Migration 018: Entity resolution — canonical entity deduplication
-- Merges duplicate entities ("NVIDIA Corp", "Nvidia", "NVIDIA CORPORATION") into one canonical entity.

-- Canonical entities table — the "golden record" for each real-world entity
CREATE TABLE IF NOT EXISTS entity_canonical (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    canonical_name  TEXT NOT NULL,
    ticker          TEXT,
    cik             TEXT,
    sector          TEXT,
    entity_type     TEXT DEFAULT 'company',
    created_at      TEXT DEFAULT (datetime('now')),
    UNIQUE(canonical_name)
);

CREATE INDEX IF NOT EXISTS idx_canonical_ticker ON entity_canonical(ticker);
CREATE INDEX IF NOT EXISTS idx_canonical_cik ON entity_canonical(cik);
