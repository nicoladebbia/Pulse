use std::collections::HashMap;
use rusqlite::{params, Connection};
use anyhow::Result;

use super::search::decode_story_id;

// Boost range: reputation 0.0 → 0.8x, 0.5 → 1.0x, 1.0 → 1.2x
const BOOST_FLOOR: f32 = 0.8;
const BOOST_RANGE: f32 = 0.4;
const FEEDBACK_WINDOW_DAYS: i64 = 90;

/// Compute Laplace-smoothed reputation from up/down counts.
/// Returns (reputation, boost) where:
/// - reputation in [0, 1]: (ups + 1) / (ups + downs + 2)
/// - boost in [0.8, 1.2]: 0.8 + 0.4 * reputation
fn compute_boost(upvotes: i64, downvotes: i64) -> (f64, f64) {
    let reputation = (upvotes as f64 + 1.0) / (upvotes as f64 + downvotes as f64 + 2.0);
    let boost = BOOST_FLOOR as f64 + BOOST_RANGE as f64 * reputation;
    (reputation, boost)
}

/// After a feedback submission, trace the message back to its source stories
/// and recompute reputation for all affected sources and sectors.
pub fn propagate_feedback(conn: &Connection, message_id: &str) -> Result<()> {
    // 1. Load the sources JSON from the assistant message
    let sources_json: Option<String> = conn.query_row(
        "SELECT sources FROM chat_messages WHERE id = ?1 AND role = 'assistant'",
        params![message_id],
        |row| row.get(0),
    ).ok().flatten();

    let Some(json_str) = sources_json else {
        tracing::debug!("No sources found for message {}", message_id);
        return Ok(());
    };

    let story_ids: Vec<i64> = serde_json::from_str(&json_str).unwrap_or_default();
    if story_ids.is_empty() {
        return Ok(());
    }

    // 2. Look up source_name and sector for each story
    let mut affected_sources: Vec<String> = Vec::new();
    let mut affected_sectors: Vec<String> = Vec::new();

    for &story_id in &story_ids {
        let (real_id, is_freedom) = decode_story_id(story_id);

        let result: Option<(String, String)> = if is_freedom {
            conn.query_row(
                "SELECT source_name, freedom FROM freedom_stories WHERE id = ?1",
                params![real_id],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            ).ok()
        } else {
            conn.query_row(
                "SELECT source_name, sector FROM stories WHERE id = ?1",
                params![story_id],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            ).ok()
        };

        if let Some((source_name, sector)) = result {
            if !source_name.is_empty() {
                affected_sources.push(source_name);
            }
            if !sector.is_empty() {
                affected_sectors.push(sector);
            }
        }
    }

    // 3. Deduplicate
    affected_sources.sort();
    affected_sources.dedup();
    affected_sectors.sort();
    affected_sectors.dedup();

    // 4. Recompute reputation for each affected source
    for source in &affected_sources {
        recompute_reputation(conn, "source", source)?;
    }
    for sector in &affected_sectors {
        recompute_reputation(conn, "sector", sector)?;
    }

    tracing::info!(
        "Propagated feedback for message {}: {} sources, {} sectors updated",
        message_id, affected_sources.len(), affected_sectors.len()
    );

    Ok(())
}

/// Recompute reputation for a single source or sector by aggregating
/// all feedback within the rolling window.
fn recompute_reputation(conn: &Connection, kind: &str, name: &str) -> Result<()> {
    let key = format!("{}:{}", kind, name);
    let window = format!("-{} days", FEEDBACK_WINDOW_DAYS);

    // Count ups and downs for this source/sector within the window.
    // Join path: chat_feedback → chat_messages.sources (JSON) → stories.source_name/sector
    //
    // Since sources is a JSON array, we use json_each() to extract story IDs,
    // then join to stories to check source_name or sector.
    let (table, column) = match kind {
        "source" => ("stories", "source_name"),
        "sector" => ("stories", "sector"),
        _ => return Ok(()),
    };

    let sql = format!(
        "SELECT
            SUM(CASE WHEN cf.rating = 'up' THEN 1 ELSE 0 END) as ups,
            SUM(CASE WHEN cf.rating = 'down' THEN 1 ELSE 0 END) as downs
         FROM chat_feedback cf
         JOIN chat_messages cm ON cm.id = cf.message_id
         JOIN json_each(cm.sources) je
         JOIN {table} s ON s.id = CAST(je.value AS INTEGER)
         WHERE cf.created_at > datetime('now', ?1)
           AND s.{column} = ?2"
    );

    let (ups, downs): (i64, i64) = conn.query_row(
        &sql,
        params![window, name],
        |row| Ok((row.get::<_, i64>(0).unwrap_or(0), row.get::<_, i64>(1).unwrap_or(0))),
    ).unwrap_or((0, 0));

    // Also check freedom_stories if computing source reputation
    let (f_ups, f_downs) = if kind == "source" {
        let f_sql =
            "SELECT
                SUM(CASE WHEN cf.rating = 'up' THEN 1 ELSE 0 END),
                SUM(CASE WHEN cf.rating = 'down' THEN 1 ELSE 0 END)
             FROM chat_feedback cf
             JOIN chat_messages cm ON cm.id = cf.message_id
             JOIN json_each(cm.sources) je
             JOIN freedom_stories fs ON fs.id = -(CAST(je.value AS INTEGER) + 100000)
             WHERE cf.created_at > datetime('now', ?1)
               AND fs.source_name = ?2
               AND CAST(je.value AS INTEGER) < 0";

        conn.query_row(
            f_sql,
            params![window, name],
            |row| Ok((row.get::<_, i64>(0).unwrap_or(0), row.get::<_, i64>(1).unwrap_or(0))),
        ).unwrap_or((0, 0))
    } else {
        (0, 0)
    };

    let total_ups = ups + f_ups;
    let total_downs = downs + f_downs;
    let (reputation, boost) = compute_boost(total_ups, total_downs);

    conn.execute(
        "INSERT INTO feedback_reputation (key, kind, upvotes, downvotes, reputation, boost, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, datetime('now'))
         ON CONFLICT(key) DO UPDATE SET
            upvotes = excluded.upvotes,
            downvotes = excluded.downvotes,
            reputation = excluded.reputation,
            boost = excluded.boost,
            updated_at = excluded.updated_at",
        params![key, kind, total_ups, total_downs, reputation, boost],
    )?;

    Ok(())
}

/// Load all reputation boosts into a HashMap for fast lookup during search.
/// Returns two maps: source_name → boost, sector → boost.
pub fn load_reputation_cache(conn: &Connection) -> (HashMap<String, f32>, HashMap<String, f32>) {
    let mut source_boosts: HashMap<String, f32> = HashMap::new();
    let mut sector_boosts: HashMap<String, f32> = HashMap::new();

    let mut stmt = match conn.prepare(
        "SELECT key, kind, boost FROM feedback_reputation"
    ) {
        Ok(s) => s,
        Err(_) => return (source_boosts, sector_boosts),
    };

    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, f64>(2)?,
        ))
    });

    if let Ok(rows) = rows {
        for row in rows.flatten() {
            let (key, kind, boost) = row;
            // key format is "source:{name}" or "sector:{name}"
            let name = key.splitn(2, ':').nth(1).unwrap_or("").to_string();
            if name.is_empty() { continue; }

            match kind.as_str() {
                "source" => { source_boosts.insert(name, boost as f32); }
                "sector" => { sector_boosts.insert(name, boost as f32); }
                _ => {}
            }
        }
    }

    (source_boosts, sector_boosts)
}
