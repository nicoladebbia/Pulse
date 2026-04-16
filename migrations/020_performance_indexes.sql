-- Performance indexes for trends page queries
-- The LOWER(sig.topic) join was causing 8-second full table scans

CREATE INDEX IF NOT EXISTS idx_signals_topic_lower ON signals(LOWER(topic));
CREATE INDEX IF NOT EXISTS idx_entity_mentions_date ON entity_mentions(mentioned_at);
CREATE INDEX IF NOT EXISTS idx_entity_mentions_entity_date ON entity_mentions(entity_id, mentioned_at);
