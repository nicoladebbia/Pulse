use tauri::State;
use crate::db::DbState;
use crate::models::Story;

#[tauri::command]
pub fn full_text_search(db: State<DbState>, query: String) -> Result<Vec<Story>, String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;

    let mut stmt = conn
        .prepare(
            "SELECT s.id, s.briefing_id, s.sector, s.headline, s.summary, s.key_facts,
                    s.why_it_matters, s.what_to_watch, s.importance_score, s.relevance_score,
                    s.relevance_reason, s.is_hero, s.display_order, s.original_url, s.source_name,
                    s.published_at, s.created_at
             FROM stories_fts fts
             JOIN stories s ON s.id = fts.rowid
             WHERE stories_fts MATCH ?1
             ORDER BY rank
             LIMIT 20",
        )
        .map_err(|e| e.to_string())?;

    let stories = stmt
        .query_map([&query], |row| {
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
            })
        })
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;

    Ok(stories)
}
