-- 030: Strip the wrong prediction citations written before the index-space fix.
--
-- THE DEFECT: `generate_predictions` presented stories to the model as
-- `[{i}] story_id={id} ...`, giving two numbers per story. The model overwhelmingly
-- returned the bracket INDEX, and that raw value was stored in
-- `insights.source_story_ids` / `insights.evidence`.
--
-- Measured on the live DB 2026-08-15, before this migration:
--   883 predictions carried 1,641 story references
--   1,146 of those (70%) were <= 200, against a stories table whose max id is 44,923
--   only 534 (32.5%) resolved to any existing story at all
-- So most predictions cited an unrelated article, and the ones that "resolved" mostly
-- did so by coincidence — index 16 is also a valid story id from March.
--
-- WHY DELETE RATHER THAN REMAP: the remap needs the list of stories that was shown to
-- the model on that day, and that list was never persisted. It cannot be reconstructed.
-- A prediction with no citation is honest; one with a confidently wrong citation is a
-- fabrication wearing a citation's clothes, and Pulse's stated standard forbids that.
--
-- THE TEST FOR "PROBABLY REAL": keep a reference only if it resolves to a story that
-- the prediction could actually have been generated from — i.e. a story created within
-- the 7 days BEFORE the prediction. Predictions are generated from "today's top
-- stories", so a reference to an article from months earlier is a false positive of the
-- index collision, not a real source.
--
-- Membership is tested with json_each, NOT instr(): a substring test would match story
-- id 23 against the text '[1234]'.
--
-- Forward-only. New predictions are validated at write time by
-- `pipeline::resolve_story_refs`, so this repairs history once and does not recur.

-- Record what we are about to discard, so the repair is auditable after the fact.
CREATE TABLE IF NOT EXISTS prediction_citation_repair (
    insight_id       INTEGER PRIMARY KEY,
    old_story_ids    TEXT NOT NULL,
    old_evidence     TEXT,
    kept_story_ids   TEXT NOT NULL,
    repaired_at      TEXT NOT NULL DEFAULT (datetime('now'))
);

-- OR IGNORE, never OR REPLACE: on a second application `insights.source_story_ids` is
-- already the repaired value, so REPLACE would overwrite the audit trail's record of the
-- ORIGINAL bad ids with the cleaned ones and destroy the only evidence of what was
-- discarded. (Caught by running this migration twice against a copy of the live DB.)
INSERT OR IGNORE INTO prediction_citation_repair (insight_id, old_story_ids, old_evidence, kept_story_ids)
SELECT
    i.id,
    i.source_story_ids,
    i.evidence,
    COALESCE(
        (
            SELECT '[' || group_concat(kept.sid) || ']'
            FROM (
                SELECT DISTINCT CAST(je.value AS INTEGER) AS sid
                FROM json_each(i.source_story_ids) je
                JOIN stories s ON s.id = CAST(je.value AS INTEGER)
                WHERE s.created_at <= i.created_at
                  AND julianday(i.created_at) - julianday(s.created_at) <= 7
            ) kept
        ),
        '[]'
    )
FROM insights i
WHERE i.insight_type = 'prediction'
  AND i.source_story_ids IS NOT NULL
  AND json_valid(i.source_story_ids)
  AND i.source_story_ids NOT IN ('', '[]');

-- Apply: keep only the references that survived the recency test.
UPDATE insights
SET source_story_ids = (
        SELECT kept_story_ids FROM prediction_citation_repair r WHERE r.insight_id = insights.id
    )
WHERE id IN (SELECT insight_id FROM prediction_citation_repair);

-- `evidence` is the denormalised mirror of the same references, so it must not be left
-- pointing at the stories we just removed. Rebuilt from the kept ids, empty when none.
UPDATE insights
SET evidence = COALESCE(
        (
            SELECT '[' || group_concat(
                json_object('story_id', CAST(je.value AS INTEGER),
                            'reasoning', 'Evidence type: source')
            ) || ']'
            FROM prediction_citation_repair r, json_each(r.kept_story_ids) je
            WHERE r.insight_id = insights.id
        ),
        '[]'
    )
WHERE id IN (SELECT insight_id FROM prediction_citation_repair);
