use tauri::State;
use crate::db::DbState;
use crate::models::Story;

#[tauri::command]
pub fn full_text_search(db: State<DbState>, query: String) -> Result<Vec<Story>, String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;

    // Sanitize FTS query: strip non-alphanumeric, prefix-match each word
    let fts_query = query
        .split_whitespace()
        .map(|word| {
            let clean: String = word.chars().filter(|c| c.is_alphanumeric()).collect();
            if clean.is_empty() { String::new() } else { format!("{}*", clean) }
        })
        .filter(|w| !w.is_empty())
        .collect::<Vec<_>>()
        .join(" OR ");

    if fts_query.is_empty() {
        return Ok(vec![]);
    }

    let mut stmt = conn
        .prepare(
            "SELECT s.id, s.briefing_id, s.sector, s.headline, s.summary, s.key_facts,
                    s.why_it_matters, s.what_to_watch, s.importance_score, s.relevance_score,
                    s.relevance_reason, s.is_hero, s.display_order, s.original_url, s.source_name,
                    s.published_at, s.created_at, s.summary_depth, s.deep_summary,
                    s.source_type, s.financial_metadata
             FROM stories_fts fts
             JOIN stories s ON s.id = fts.rowid
             WHERE stories_fts MATCH ?1
             ORDER BY rank
             LIMIT 20",
        )
        .map_err(|e| e.to_string())?;

    let stories = stmt
        .query_map([&fts_query], |row| {
            let key_facts_json: String = row.get(5)?;
            let key_facts: Vec<String> =
                serde_json::from_str(&key_facts_json).unwrap_or_default();
            Ok(Story {
                id: row.get(0)?,
                briefing_id: row.get(1)?,
                sector: row.get(2)?,
                headline: row.get(3)?,
                summary: row.get(4)?,
                key_facts,
                why_it_matters: row.get(6)?,
                what_to_watch: row.get(7)?,
                importance_score: row.get(8)?,
                relevance_score: row.get(9)?,
                relevance_reason: row.get(10)?,
                is_hero: { let v: i32 = row.get(11)?; v != 0 },
                display_order: row.get(12)?,
                original_url: row.get(13)?,
                source_name: row.get(14)?,
                published_at: row.get(15)?,
                created_at: row.get(16)?,
                summary_depth: row.get(17).ok(),
                deep_summary: row.get(18).ok(),
                source_type: row.get(19).ok(),
                financial_metadata: row.get(20).ok(),
            })
        })
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;

    Ok(stories)
}
