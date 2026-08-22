-- briefings.story_count counted EVERY row in `stories`, including bulk regulatory
-- filings (SEC EDGAR, FEC, Senate LDA, Federal Register, USASpending, Google Patents).
--
-- Every surface that renders the column labels it "stories": the header's
-- "N stories across 4 sectors", the sidebar's All row, and each day in the archive
-- timeline. So 2026-08-14 advertised 582 stories when the day actually held 120 news
-- stories and 462 filings, and the front page's "View all N stories in Archive" link
-- promised the same inflated number.
--
-- Redefine the column as NEWS stories and backfill the existing history with the same
-- definition the pipeline now writes (pipeline/mod.rs, write_financial_stories).
-- The COALESCE is belt-and-braces: migration 017 defines source_type as NOT NULL DEFAULT
-- 'news' with a CHECK, so no row is actually NULL today.
--
-- Filings are not lost: the archive's daily view lists them in a separate digest
-- grouped by issuing body.
--
-- DAILY briefings only. A `freedoms` briefing keeps its stories in `freedom_stories`,
-- so its story_count is not derived from `stories` at all — 58 of them nonetheless have
-- financial rows pointing at their briefing_id, and recomputing from `stories` would
-- zero the archive's "Freedoms · N" badge.

UPDATE briefings
SET story_count = (
    SELECT COUNT(*)
    FROM stories
    WHERE stories.briefing_id = briefings.id
      AND COALESCE(stories.source_type, 'news') != 'financial'
)
WHERE briefings.briefing_type = 'daily'
  AND EXISTS (
    SELECT 1 FROM stories
    WHERE stories.briefing_id = briefings.id
      AND stories.source_type = 'financial'
);
