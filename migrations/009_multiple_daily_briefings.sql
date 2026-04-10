-- Migration 009: Support multiple briefings per day
-- Each briefing gets a time_label (e.g. "8:00 AM", "9:00 PM") so the archive
-- can show separate time windows.

-- Drop the old unique index that prevents multiple dailies per date
DROP INDEX IF EXISTS idx_briefings_date_type;

-- Add time label column
ALTER TABLE briefings ADD COLUMN time_label TEXT;

-- New index: non-unique, for lookup performance
CREATE INDEX IF NOT EXISTS idx_briefings_date_type_time ON briefings(date, briefing_type, created_at DESC);
