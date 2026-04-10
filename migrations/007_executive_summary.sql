-- Migration 007: Executive summary for briefings
-- AI-generated 3-5 sentence summary displayed at the top of the briefing page.
-- Works for both 'daily' and 'freedoms' briefing types.

ALTER TABLE briefings ADD COLUMN executive_summary TEXT;
