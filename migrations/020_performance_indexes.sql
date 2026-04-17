-- Performance indexes for trends/signals queries

CREATE INDEX IF NOT EXISTS idx_signals_trajectory ON signals(trajectory, acceleration);
CREATE INDEX IF NOT EXISTS idx_entity_mentions_date ON entity_mentions(mentioned_at);
