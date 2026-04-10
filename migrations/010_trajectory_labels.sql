-- Migration 010: Update trajectory labels from acceleration-based to volume-based
-- Old: emerging, growing, peaking, declining, dormant
-- New: dominant, hot, rising, fading, dormant

-- SQLite doesn't support ALTER CHECK constraints, so we recreate the table
CREATE TABLE signals_new (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    topic       TEXT NOT NULL,
    sector      TEXT,
    window_7d   INTEGER NOT NULL DEFAULT 0,
    window_30d  INTEGER NOT NULL DEFAULT 0,
    window_90d  INTEGER NOT NULL DEFAULT 0,
    acceleration REAL NOT NULL DEFAULT 0.0,
    trajectory  TEXT NOT NULL DEFAULT 'dormant' CHECK(trajectory IN (
        'dominant', 'hot', 'rising', 'fading', 'dormant'
    )),
    updated_at  TEXT NOT NULL DEFAULT (datetime('now'))
);

-- Copy data, mapping old labels to new ones
INSERT INTO signals_new (id, topic, sector, window_7d, window_30d, window_90d, acceleration, trajectory, updated_at)
SELECT id, topic, sector, window_7d, window_30d, window_90d, acceleration,
    CASE trajectory
        WHEN 'emerging' THEN 'rising'
        WHEN 'growing' THEN 'rising'
        WHEN 'peaking' THEN 'rising'
        WHEN 'declining' THEN 'fading'
        WHEN 'dormant' THEN 'dormant'
        ELSE 'dormant'
    END,
    updated_at
FROM signals;

DROP TABLE signals;
ALTER TABLE signals_new RENAME TO signals;

-- Recreate the unique index
CREATE UNIQUE INDEX IF NOT EXISTS idx_signals_topic_sector ON signals(topic, sector);
