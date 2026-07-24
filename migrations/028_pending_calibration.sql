-- Migration 028: Hold auto-calibration proposals for manual approval instead
-- of silently overwriting live weights (calibration-backtest-universe audit,
-- Task 3.3). The corrected 41-day backfill found only ~4-5 independent
-- resolved episodes in the historical archive — nowhere near enough to trust
-- an automatic reweight, and the live paper_trades sample is smaller still.
-- adjust_weights now writes a proposal here; nothing touches
-- user_profile.calibrated_weights until apply_pending_calibration runs.
CREATE TABLE IF NOT EXISTS pending_calibration (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    batch_id TEXT NOT NULL,
    computed_at TEXT NOT NULL,
    dimension TEXT NOT NULL,
    old_weight REAL NOT NULL,
    new_weight REAL NOT NULL,
    hit_rate REAL,
    sample_size INTEGER,
    total_resolved INTEGER NOT NULL,
    status TEXT NOT NULL DEFAULT 'pending',
    applied_at TEXT
);

CREATE INDEX IF NOT EXISTS idx_pending_calibration_batch ON pending_calibration(batch_id);
