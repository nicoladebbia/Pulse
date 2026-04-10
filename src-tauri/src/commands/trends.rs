use serde::Serialize;
use tauri::State;
use crate::db::DbState;
use crate::services::signals;

#[derive(Debug, Clone, Serialize)]
pub struct TrendPoint {
    pub date: String,
    pub headline: String,
    pub significance: i32,
}

#[derive(Debug, Clone, Serialize)]
pub struct TrendThread {
    pub id: i64,
    pub title: String,
    pub sector: String,
    pub trajectory: String,
    pub acceleration: f64,
    pub points: Vec<TrendPoint>,
}

/// Recompute signals from entity_mentions data, then return trends.
/// This ensures signals are always fresh when the user opens the Trends page.
#[tauri::command]
pub fn get_trends(db: State<'_, DbState>) -> Result<Vec<TrendThread>, String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;

    // Recompute signals before reading to ensure freshness
    let today = chrono::Local::now().format("%Y-%m-%d").to_string();
    match signals::recompute_signals(&conn, &today) {
        Ok(count) => tracing::debug!("Recomputed {} signals", count),
        Err(e) => tracing::warn!("Signal recompute failed: {}", e),
    }

    // Single query: join signals -> entities -> mentions -> stories
    let mut stmt = conn
        .prepare(
            "SELECT sig.id, sig.topic, sig.sector, sig.trajectory, sig.acceleration,
                    em.mentioned_at, s.headline, s.importance_score
             FROM signals sig
             JOIN entities e ON LOWER(e.name) = LOWER(sig.topic)
             JOIN entity_mentions em ON em.entity_id = e.id
             JOIN stories s ON s.id = em.story_id
             WHERE sig.trajectory != 'dormant'
               AND em.mentioned_at >= date('now', '-14 days')
             ORDER BY sig.acceleration DESC, em.mentioned_at ASC",
        )
        .map_err(|e| e.to_string())?;

    let rows: Vec<(i64, String, Option<String>, String, f64, String, String, i32)> = stmt
        .query_map([], |row| {
            Ok((
                row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?,
                row.get(4)?, row.get(5)?, row.get(6)?, row.get(7)?,
            ))
        })
        .map_err(|e| e.to_string())?
        .filter_map(|r| r.map_err(|e| tracing::warn!("row read error: {e}")).ok())
        .collect();

    // Group by signal ID
    let mut thread_map: std::collections::HashMap<i64, TrendThread> = std::collections::HashMap::new();
    for (id, topic, sector, trajectory, acceleration, date, headline, significance) in rows {
        let entry = thread_map.entry(id).or_insert_with(|| TrendThread {
            id,
            title: topic,
            sector: sector.unwrap_or_else(|| "general".to_string()),
            trajectory,
            acceleration,
            points: Vec::new(),
        });
        if entry.points.len() < 20 {
            entry.points.push(TrendPoint { date, headline, significance });
        }
    }

    // Sort by acceleration descending
    let mut threads: Vec<TrendThread> = thread_map.into_values().collect();
    threads.sort_by(|a, b| b.acceleration.partial_cmp(&a.acceleration).unwrap_or(std::cmp::Ordering::Equal));
    threads.truncate(20);

    Ok(threads)
}
