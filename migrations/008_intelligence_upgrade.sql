-- Migration 008: Intelligence upgrade
-- Adds API usage tracking, project ideas, and adaptive summaries.

-- Track token usage and costs across all API providers
CREATE TABLE IF NOT EXISTS api_usage (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    provider TEXT NOT NULL,
    model TEXT,
    endpoint TEXT NOT NULL,
    input_tokens INTEGER DEFAULT 0,
    output_tokens INTEGER DEFAULT 0,
    estimated_cost_usd REAL DEFAULT 0.0,
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE INDEX IF NOT EXISTS idx_api_usage_provider ON api_usage(provider, created_at);
CREATE INDEX IF NOT EXISTS idx_api_usage_date ON api_usage(created_at);

-- Project ideas generated from news trends
CREATE TABLE IF NOT EXISTS project_ideas (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    title TEXT NOT NULL,
    description TEXT NOT NULL,
    why_now TEXT NOT NULL,
    competitors TEXT,
    differentiation TEXT NOT NULL,
    tech_stack TEXT,
    difficulty TEXT CHECK(difficulty IN ('weekend', 'week', 'month')),
    relevance_score REAL,
    source_story_ids TEXT,
    status TEXT NOT NULL DEFAULT 'new' CHECK(status IN ('new', 'saved', 'dismissed', 'building')),
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE INDEX IF NOT EXISTS idx_project_ideas_status ON project_ideas(status, created_at DESC);

-- Adaptive summaries: deep analysis for high-impact stories
ALTER TABLE stories ADD COLUMN summary_depth TEXT DEFAULT 'standard';
ALTER TABLE stories ADD COLUMN deep_summary TEXT;
