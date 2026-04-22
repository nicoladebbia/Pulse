-- Migration 022: Predictions v2 schema
--
-- Adds v2 columns to `insights` table and expands status vocabulary.
-- Because SQLite can't ALTER a CHECK constraint, we rebuild the table.
--
-- New status: `needs_review` (used for BOTH legacy-wiped predictions and
-- new predictions that fail resolution 3 times).
--
-- New columns (all additive, safe for existing data):
--   target_metric        JSON {ticker, operator, value, unit, baseline_date?}
--   target_date          ISO date, falsifiable deadline
--   source_story_ids     JSON int[] — evidence grounding
--   source_signal_ids    JSON int[] — signal grounding
--   model_used           "claude-sonnet-4-6" | "claude-opus-4-7" | etc.
--   resolution_method    'market' | 'llm' | 'manual' | NULL
--   confidence_history   Vec<f64> JSON — correct sparkline schema
--   resolution_attempts  how many LLM checks tried (max 3 before manual)

-- 1) Rebuild insights with expanded CHECK constraint and new columns
CREATE TABLE insights_new (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    insight_type    TEXT NOT NULL CHECK(insight_type IN (
        'causal_chain', 'prediction', 'signal', 'pattern', 'contrarian'
    )),
    title           TEXT NOT NULL,
    content         TEXT NOT NULL,
    confidence      REAL NOT NULL DEFAULT 0.5 CHECK(confidence >= 0.0 AND confidence <= 1.0),
    evidence        TEXT,
    sector          TEXT,
    status          TEXT NOT NULL DEFAULT 'active' CHECK(status IN (
        'active', 'validated', 'partially_validated', 'invalidated', 'expired', 'needs_review'
    )),
    predicted_date  TEXT,
    actual_outcome  TEXT,
    created_at      TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at      TEXT NOT NULL DEFAULT (datetime('now')),
    probability_history TEXT DEFAULT '[]',
    brier_score     REAL,
    resolution_criteria TEXT,
    base_rate       REAL,
    -- v2 columns
    target_metric       TEXT,                          -- JSON
    target_date         TEXT,                          -- ISO date
    source_story_ids    TEXT DEFAULT '[]',             -- JSON int[]
    source_signal_ids   TEXT DEFAULT '[]',             -- JSON int[]
    model_used          TEXT,
    resolution_method   TEXT CHECK(resolution_method IN ('market', 'llm', 'manual') OR resolution_method IS NULL),
    confidence_history  TEXT DEFAULT '[]',             -- Vec<f64> JSON
    resolution_attempts INTEGER DEFAULT 0
);

INSERT INTO insights_new (
    id, insight_type, title, content, confidence, evidence, sector, status,
    predicted_date, actual_outcome, created_at, updated_at,
    probability_history, brier_score, resolution_criteria, base_rate
)
SELECT id, insight_type, title, content, confidence, evidence, sector, status,
       predicted_date, actual_outcome, created_at, updated_at,
       probability_history, brier_score, resolution_criteria, base_rate
FROM insights;

DROP TABLE insights;
ALTER TABLE insights_new RENAME TO insights;

-- Recreate indexes (same as pre-migration)
CREATE INDEX idx_insights_type ON insights(insight_type, status);
CREATE INDEX idx_insights_sector ON insights(sector, status);
CREATE INDEX idx_insights_status ON insights(status, updated_at);

-- 2) Legacy wipe — predictions validated by the broken semantic-similarity
-- logic get clean-slated. Their text stays; accuracy/brier reset.
UPDATE insights
   SET status = 'needs_review',
       brier_score = NULL,
       actual_outcome = NULL
 WHERE insight_type = 'prediction'
   AND status IN ('validated', 'partially_validated', 'invalidated');

-- 3) Calibration table — stores daily aggregations of accuracy
-- by confidence bucket / topic / timeframe / source.
CREATE TABLE prediction_calibration (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    computed_at             TEXT NOT NULL DEFAULT (datetime('now')),
    total_resolved          INTEGER NOT NULL,
    accuracy_overall        REAL,
    accuracy_by_confidence  TEXT,    -- JSON {"0.5-0.6": 0.58, ...}
    accuracy_by_topic       TEXT,    -- JSON {"ai": 0.62, ...}
    accuracy_by_timeframe   TEXT,    -- JSON {"7": 0.70, "30": 0.55, ...}
    accuracy_by_source      TEXT,    -- JSON {"SEC EDGAR": 0.65, ...}
    avg_brier               REAL
);
CREATE INDEX idx_calibration_computed ON prediction_calibration(computed_at DESC);
