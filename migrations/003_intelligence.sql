-- Migration 003: Intelligence Engine
-- Adds: embeddings storage, entity knowledge graph, insights ledger,
--        signal tracking, chat threads, and recreated chat_messages

-- =============================================================================
-- EMBEDDING STORAGE
-- =============================================================================

-- Story embeddings: 512-dim f32 vectors stored as BLOBs (2048 bytes each)
-- Used for semantic similarity search via cosine distance computed in Rust
CREATE TABLE IF NOT EXISTS story_embeddings (
    story_id    INTEGER PRIMARY KEY REFERENCES stories(id) ON DELETE CASCADE,
    embedding   BLOB NOT NULL,
    created_at  TEXT NOT NULL DEFAULT (datetime('now'))
);

-- =============================================================================
-- ENTITY KNOWLEDGE GRAPH
-- =============================================================================

-- Core entities: companies, people, topics, products, regulations
CREATE TABLE IF NOT EXISTS entities (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    name            TEXT NOT NULL,
    name_normalized TEXT NOT NULL,  -- lowercase, trimmed for dedup matching
    entity_type     TEXT NOT NULL CHECK(entity_type IN ('company', 'person', 'topic', 'product', 'regulation')),
    sector          TEXT,
    first_seen      TEXT NOT NULL,  -- date
    last_seen       TEXT NOT NULL,  -- date
    mention_count   INTEGER NOT NULL DEFAULT 1,
    sentiment_avg   REAL NOT NULL DEFAULT 0.0,  -- -1.0 to 1.0
    created_at      TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE UNIQUE INDEX IF NOT EXISTS idx_entities_name_type ON entities(name_normalized, entity_type);
CREATE INDEX IF NOT EXISTS idx_entities_sector ON entities(sector);
CREATE INDEX IF NOT EXISTS idx_entities_mention_count ON entities(mention_count DESC);

-- Individual entity mentions linked to specific stories
CREATE TABLE IF NOT EXISTS entity_mentions (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    entity_id   INTEGER NOT NULL REFERENCES entities(id) ON DELETE CASCADE,
    story_id    INTEGER NOT NULL REFERENCES stories(id) ON DELETE CASCADE,
    sentiment   REAL NOT NULL DEFAULT 0.0,  -- -1.0 to 1.0
    context     TEXT,  -- brief snippet explaining the mention
    mentioned_at TEXT NOT NULL,  -- date of the story
    created_at  TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE INDEX IF NOT EXISTS idx_entity_mentions_entity ON entity_mentions(entity_id, mentioned_at);
CREATE INDEX IF NOT EXISTS idx_entity_mentions_story ON entity_mentions(story_id);

-- Relationships between entities (co-occurrence, competition, collaboration, etc.)
CREATE TABLE IF NOT EXISTS entity_relationships (
    id                  INTEGER PRIMARY KEY AUTOINCREMENT,
    entity_a_id         INTEGER NOT NULL REFERENCES entities(id) ON DELETE CASCADE,
    entity_b_id         INTEGER NOT NULL REFERENCES entities(id) ON DELETE CASCADE,
    relationship_type   TEXT NOT NULL CHECK(relationship_type IN (
        'collaborates', 'competes', 'regulates', 'acquires', 'invests',
        'supplies', 'co_occurs', 'opposes', 'subsidiary'
    )),
    strength            REAL NOT NULL DEFAULT 1.0,  -- higher = stronger relationship
    first_seen          TEXT NOT NULL,
    last_seen           TEXT NOT NULL,
    mention_count       INTEGER NOT NULL DEFAULT 1,
    created_at          TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE UNIQUE INDEX IF NOT EXISTS idx_entity_rel_pair ON entity_relationships(entity_a_id, entity_b_id, relationship_type);
CREATE INDEX IF NOT EXISTS idx_entity_rel_a ON entity_relationships(entity_a_id);
CREATE INDEX IF NOT EXISTS idx_entity_rel_b ON entity_relationships(entity_b_id);

-- =============================================================================
-- INSIGHTS LEDGER (bot's learned conclusions)
-- =============================================================================

CREATE TABLE IF NOT EXISTS insights (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    insight_type    TEXT NOT NULL CHECK(insight_type IN (
        'causal_chain', 'prediction', 'signal', 'pattern', 'contrarian'
    )),
    title           TEXT NOT NULL,
    content         TEXT NOT NULL,
    confidence      REAL NOT NULL DEFAULT 0.5 CHECK(confidence >= 0.0 AND confidence <= 1.0),
    evidence        TEXT,  -- JSON: array of {story_id, reasoning}
    sector          TEXT,
    status          TEXT NOT NULL DEFAULT 'active' CHECK(status IN (
        'active', 'validated', 'partially_validated', 'invalidated', 'expired'
    )),
    predicted_date  TEXT,  -- for predictions: when the event is expected
    actual_outcome  TEXT,  -- filled in when prediction is validated/invalidated
    created_at      TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at      TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE INDEX IF NOT EXISTS idx_insights_type ON insights(insight_type, status);
CREATE INDEX IF NOT EXISTS idx_insights_sector ON insights(sector, status);
CREATE INDEX IF NOT EXISTS idx_insights_status ON insights(status, updated_at);

-- Links insights to the stories that serve as evidence
CREATE TABLE IF NOT EXISTS insight_evidence (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    insight_id  INTEGER NOT NULL REFERENCES insights(id) ON DELETE CASCADE,
    story_id    INTEGER NOT NULL REFERENCES stories(id) ON DELETE CASCADE,
    role        TEXT NOT NULL CHECK(role IN ('trigger', 'support', 'counter')),
    created_at  TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE INDEX IF NOT EXISTS idx_insight_evidence_insight ON insight_evidence(insight_id);
CREATE INDEX IF NOT EXISTS idx_insight_evidence_story ON insight_evidence(story_id);

-- =============================================================================
-- SIGNAL TRACKING
-- =============================================================================

-- Tracks mention velocity and acceleration per topic/entity
CREATE TABLE IF NOT EXISTS signals (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    topic       TEXT NOT NULL,  -- entity name or topic string
    sector      TEXT,
    window_7d   INTEGER NOT NULL DEFAULT 0,
    window_30d  INTEGER NOT NULL DEFAULT 0,
    window_90d  INTEGER NOT NULL DEFAULT 0,
    acceleration REAL NOT NULL DEFAULT 0.0,  -- (7d rate) / (30d rate)
    trajectory  TEXT NOT NULL DEFAULT 'dormant' CHECK(trajectory IN (
        'emerging', 'growing', 'peaking', 'declining', 'dormant'
    )),
    updated_at  TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE UNIQUE INDEX IF NOT EXISTS idx_signals_topic ON signals(topic, sector);
CREATE INDEX IF NOT EXISTS idx_signals_trajectory ON signals(trajectory, acceleration DESC);

-- =============================================================================
-- CHAT SYSTEM (threads + messages)
-- =============================================================================

-- Conversation threads, auto-classified by topic
CREATE TABLE IF NOT EXISTS chat_threads (
    id          TEXT PRIMARY KEY,  -- UUID
    topic       TEXT NOT NULL DEFAULT 'general' CHECK(topic IN (
        'ai', 'miami', 'italy', 'tech', 'general'
    )),
    title       TEXT,  -- AI-generated after first exchange
    created_at  TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at  TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE INDEX IF NOT EXISTS idx_chat_threads_topic ON chat_threads(topic, updated_at DESC);
CREATE INDEX IF NOT EXISTS idx_chat_threads_updated ON chat_threads(updated_at DESC);

-- Drop and recreate chat_messages (table exists from migration 001 but is empty/unused)
DROP TABLE IF EXISTS chat_messages;

CREATE TABLE chat_messages (
    id          TEXT PRIMARY KEY,  -- UUID
    thread_id   TEXT NOT NULL REFERENCES chat_threads(id) ON DELETE CASCADE,
    role        TEXT NOT NULL CHECK(role IN ('user', 'assistant', 'system')),
    content     TEXT NOT NULL,
    sources     TEXT,   -- JSON: array of story IDs referenced
    metadata    TEXT,   -- JSON: {token_count, model, retrieval_method, predictions_made, ...}
    created_at  TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE INDEX IF NOT EXISTS idx_chat_messages_thread ON chat_messages(thread_id, created_at);
