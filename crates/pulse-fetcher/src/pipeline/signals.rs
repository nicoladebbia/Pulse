use super::*;

/// Historical signal backfill (via `--mode backfill-signals`). Loops `[start, end]`
/// ascending, recomputing `signals` + `cross_signals` as-of each historical date using
/// the resolved calibration-backtest-universe plan's window/hold shrink (window_days=30
/// for this pass, though the parameter is general). Filtering is by ingestion date
/// (`entity_mentions.mentioned_at`, when the pipeline first extracted the entity) — NOT
/// `stories.published_at`, which for financial sources holds heterogeneous, unreliable
/// dates (USASpending contract-start dates back to 1978, FEC filing periods extending to
/// Dec 2026) and can't be used as a clean content-date filter. Ingestion date is also the
/// theoretically correct lookahead-safety boundary: it's exactly when the algorithm could
/// have known about the fact, regardless of how old the underlying event was. Every
/// windowed stage now bounds ABOVE by `<= ?1` (as_of) in addition to the existing lower
/// bound — without it, a backfilled as_of in the past still read every mention through
/// today, leaking ~2 months of future data into every historical signal (caught by
/// advisor review 2026-07-23, before any calibration/backtest numbers were trusted).
///
/// Refuses to run against what looks like the live production DB path unless
/// `i_know_this_is_a_copy` is set — this DELETEs and rewrites `cross_signals` rows across
/// the given date range, which would corrupt live trading data.
pub(crate) fn run_backfill_signals(
    db_path: &Path,
    start: &str,
    end: &str,
    window_days: i64,
    i_know_this_is_a_copy: bool,
) -> anyhow::Result<usize> {
    let production_path = dirs::data_dir().map(|d| d.join("com.pulse.app").join("pulse.db"));
    let looks_like_production = production_path
        .as_ref()
        .map(|p| {
            if p == db_path {
                return true;
            }
            // Fall back to canonicalized comparison for relative/symlinked paths that
            // point at the same file. Only trust this when BOTH sides canonicalize
            // successfully — if either fails (e.g. db_path doesn't exist yet), treat
            // as NOT production rather than let two `None`s look like a match.
            match (std::fs::canonicalize(p), std::fs::canonicalize(db_path)) {
                (Ok(a), Ok(b)) => a == b,
                _ => false,
            }
        })
        .unwrap_or(false);
    if looks_like_production && !i_know_this_is_a_copy {
        anyhow::bail!(
            "Refusing to run backfill-signals against what looks like the live production \
             DB ({}). This DELETEs and rewrites cross_signals rows in the given date range. \
             Run it against a scratch copy made with `sqlite3 <live db> \".backup <copy>\"` \
             (never `cp` — the live fetcher may be mid-write), or pass \
             --i-know-this-is-a-copy if you're certain this path is safe.",
            db_path.display()
        );
    }

    let start_date = chrono::NaiveDate::parse_from_str(start, "%Y-%m-%d")
        .map_err(|e| anyhow::anyhow!("invalid --start '{}': {}", start, e))?;
    let end_date = chrono::NaiveDate::parse_from_str(end, "%Y-%m-%d")
        .map_err(|e| anyhow::anyhow!("invalid --end '{}': {}", end, e))?;
    if start_date > end_date {
        anyhow::bail!("--start ({}) must be <= --end ({})", start, end);
    }

    let conn = rusqlite::Connection::open(db_path)?;

    // Wipe stale cross_signals rows in-range FIRST, before the loop writes anything.
    // compute_cross_signals uses INSERT OR REPLACE keyed on (entity_id, date(computed_at))
    // and SKIPS writing any entity that drops below threshold — so without this wipe, an
    // entity whose corrected score no longer clears convergence keeps its old inflated
    // stale row, and those surviving stale (inflated) rows would dominate candidate
    // selection instead of being replaced by the corrected (lower/absent) values.
    let wiped = conn.execute(
        "DELETE FROM cross_signals WHERE date(computed_at) BETWEEN ?1 AND ?2",
        rusqlite::params![start, end],
    )?;
    tracing::info!("Backfill-signals: wiped {} stale cross_signals rows in [{}, {}]", wiped, start, end);

    let total_days = (end_date - start_date).num_days() + 1;
    let mut date = start_date;
    let mut processed = 0usize;
    while date <= end_date {
        let date_str = date.format("%Y-%m-%d").to_string();
        match recompute_signals_pipeline(&conn, &date_str, window_days) {
            Ok(n) => tracing::info!("Backfill-signals {}: recomputed {} entity signals", date_str, n),
            Err(e) => tracing::warn!("Backfill-signals {}: signal recompute failed (non-fatal): {}", date_str, e),
        }
        match compute_cross_signals(db_path, &date_str) {
            Ok(n) => tracing::info!("Backfill-signals {}: computed {} cross-signal scores", date_str, n),
            Err(e) => tracing::warn!("Backfill-signals {}: cross-signal computation failed (non-fatal): {}", date_str, e),
        }
        processed += 1;
        tracing::info!("Backfill-signals progress: {}/{} days", processed, total_days);
        date = date.succ_opt().ok_or_else(|| anyhow::anyhow!("date overflow while backfilling"))?;
    }

    Ok(processed)
}

/// Recompute all entity signals from entity_mentions data.
/// Pipeline version — matches src-tauri/src/services/signals.rs::recompute_signals
/// plus new dimensions (lobbying, regulatory, patent).
///
/// Performance: rewritten 2026-05-10 from per-topic subqueries (O(topics × stories))
/// to per-metric GROUP BY (O(stories) per metric). Was hanging at >25min on 6566
/// topics × 9 subqueries × full canonical scan. Now ~9 single-pass queries.
/// `window_days` is the lookback used for the "90-day" dimensions (topic
/// momentum/dormant classification, USASpending contract value, lobbying
/// spend) — required, not defaulted, so every call site must explicitly
/// choose. Live callers pass 90 (unchanged production behavior); the
/// historical backfill driver passes 30, per the resolved window/hold
/// shrink decision (2026-07-23 calibration-backtest-universe plan).
/// institutional_flow's 90-day window (Stage 10) is intentionally left at a
/// hardcoded 90 — it's zeroed in DEFAULT_WEIGHTS and out of scope for backfill.
pub(crate) fn recompute_signals_pipeline(conn: &rusqlite::Connection, today: &str, window_days: i64) -> anyhow::Result<usize> {
    use std::collections::HashMap;
    let window_clause = format!("-{} days", window_days);

    // Check if entity_canonical table exists for canonical grouping
    let has_canonical: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='entity_canonical')",
        [], |row| row.get(0),
    ).unwrap_or(false);

    // Build a temp table mapping entity_id -> (topic, sector). This makes every
    // downstream metric a simple JOIN on entity_id (indexed) instead of a
    // COALESCE(...) = ?  predicate that forces a full entities scan per topic.
    conn.execute_batch("DROP TABLE IF EXISTS temp_topic_map;")?;
    if has_canonical {
        conn.execute_batch(
            "CREATE TEMP TABLE temp_topic_map AS
             SELECT e.id AS entity_id,
                    COALESCE(ec.canonical_name, e.name) AS topic,
                    COALESCE(ec.sector, e.sector) AS sector
             FROM entities e
             LEFT JOIN entity_canonical ec ON ec.id = e.canonical_id;
             CREATE INDEX temp_topic_map_entity ON temp_topic_map(entity_id);
             CREATE INDEX temp_topic_map_topic ON temp_topic_map(topic, sector);"
        )?;
    } else {
        conn.execute_batch(
            "CREATE TEMP TABLE temp_topic_map AS
             SELECT e.id AS entity_id, e.name AS topic, e.sector AS sector
             FROM entities e;
             CREATE INDEX temp_topic_map_entity ON temp_topic_map(entity_id);
             CREATE INDEX temp_topic_map_topic ON temp_topic_map(topic, sector);"
        )?;
    }

    // Stage 1 — window counts (7d/30d/90d) + days_active per (topic, sector).
    // Single pass over entity_mentions ⋈ temp_topic_map.
    let mut stmt = conn.prepare(
        "SELECT tm.topic, tm.sector,
            SUM(CASE WHEN em.mentioned_at >= date(?1, '-7 days') THEN 1 ELSE 0 END) AS w7,
            SUM(CASE WHEN em.mentioned_at >= date(?1, '-30 days') THEN 1 ELSE 0 END) AS w30,
            SUM(CASE WHEN em.mentioned_at >= date(?1, ?2) THEN 1 ELSE 0 END) AS w90,
            COUNT(DISTINCT em.mentioned_at) AS days_active
         FROM entity_mentions em
         JOIN temp_topic_map tm ON tm.entity_id = em.entity_id
         WHERE em.mentioned_at >= date(?1, ?2) AND em.mentioned_at <= ?1
         GROUP BY tm.topic, tm.sector"
    )?;

    let window_rows: Vec<(String, Option<String>, i64, i64, i64, i64)> = stmt
        .query_map(rusqlite::params![today, window_clause], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?)))?
        .filter_map(|r| r.ok())
        .collect();

    // Map (topic, sector) -> aggregated metrics. Sector key is normalized to
    // empty string so it round-trips through HashMap (we restore Option<String> on write).
    type Key = (String, String);
    let key = |t: &str, s: &Option<String>| -> Key {
        (t.to_string(), s.clone().unwrap_or_default())
    };

    // Helper: load a single-metric GROUP BY result into a HashMap<Key, f64>.
    let load_metric = |sql: &str, params: &[&dyn rusqlite::ToSql]| -> rusqlite::Result<HashMap<Key, f64>> {
        let mut stmt = conn.prepare(sql)?;
        let rows = stmt
            .query_map(params, |row| {
                let topic: String = row.get(0)?;
                let sector: Option<String> = row.get(1)?;
                let val: f64 = row.get(2)?;
                Ok(((topic, sector.unwrap_or_default()), val))
            })?
            .filter_map(|r| r.ok());
        Ok(rows.collect())
    };

    // Stage 2 — source diversity (last 30 days). Distinct source_names per topic.
    // 30-day, NOT 7-day: news coverage is bursty, so a 7-day slice scores even
    // well-covered entities at diversity=1 (Nvidia: 1 over 7d vs 9 over 30d,
    // measured 2026-07-15). The convergence gate needs src_diversity>=2, and this
    // UPDATE (written at ~3958) runs AFTER — and clobbers — the financial writer in
    // extract_entities_from_financial_metadata, which already uses 30 days. A 7-day
    // window here silently overwrote that broader count with 1 and starved the
    // `positive>=2 && src_diversity>=2` convergence branch to ~32 of 2712 signals.
    // 30d aligns both writers and the news timescale (raises it to ~133; the
    // positive>=2 + compound>0.3 gates keep candidate volume bounded, not a firehose).
    let diversity_map: HashMap<Key, f64> = load_metric(
        "SELECT tm.topic, tm.sector, CAST(COUNT(DISTINCT s.source_name) AS REAL)
         FROM entity_mentions em
         JOIN temp_topic_map tm ON tm.entity_id = em.entity_id
         JOIN stories s ON s.id = em.story_id
         WHERE em.mentioned_at >= date(?1, '-30 days') AND em.mentioned_at <= ?1
         GROUP BY tm.topic, tm.sector",
        &[&today],
    ).unwrap_or_default();

    // Stage 3 — insider buy volume (Form 4, last 30d), weighted by signal_weight.
    let insider_map: HashMap<Key, f64> = load_metric(
        "SELECT tm.topic, tm.sector, COALESCE(SUM(
            CASE WHEN json_valid(s.financial_metadata)
                 AND json_extract(s.financial_metadata, '$.filing_type') IN ('4', 'form4')
            THEN COALESCE(CAST(json_extract(s.financial_metadata, '$.total_value') AS REAL), 0)
                 * COALESCE(CAST(json_extract(s.financial_metadata, '$.signal_weight') AS REAL), 0.0)
            ELSE 0 END
         ), 0)
         FROM entity_mentions em
         JOIN temp_topic_map tm ON tm.entity_id = em.entity_id
         JOIN stories s ON s.id = em.story_id
         WHERE s.source_type = 'financial'
           AND em.mentioned_at >= date(?1, '-30 days') AND em.mentioned_at <= ?1
         GROUP BY tm.topic, tm.sector",
        &[&today],
    ).unwrap_or_default();

    // Stage 4 — government contract value (USASpending, last 90d).
    let contract_map: HashMap<Key, f64> = load_metric(
        "SELECT tm.topic, tm.sector, COALESCE(SUM(
            CASE WHEN json_valid(s.financial_metadata)
                 AND s.source_name LIKE '%USASpending%'
            THEN COALESCE(CAST(json_extract(s.financial_metadata, '$.amount') AS REAL), 0)
            ELSE 0 END
         ), 0)
         FROM entity_mentions em
         JOIN temp_topic_map tm ON tm.entity_id = em.entity_id
         JOIN stories s ON s.id = em.story_id
         WHERE s.source_type = 'financial'
           AND em.mentioned_at >= date(?1, ?2) AND em.mentioned_at <= ?1
         GROUP BY tm.topic, tm.sector",
        &[&today as &dyn rusqlite::ToSql, &window_clause as &dyn rusqlite::ToSql],
    ).unwrap_or_default();

    // Stage 5 — lobbying spend (LDA / Senate, last 90d).
    let lobby_map: HashMap<Key, f64> = load_metric(
        "SELECT tm.topic, tm.sector, COALESCE(SUM(
            CASE WHEN json_valid(s.financial_metadata)
                 AND (s.source_name LIKE '%LDA%' OR s.source_name LIKE '%Senate%')
            THEN COALESCE(CAST(json_extract(s.financial_metadata, '$.amount') AS REAL), 0)
            ELSE 0 END
         ), 0)
         FROM entity_mentions em
         JOIN temp_topic_map tm ON tm.entity_id = em.entity_id
         JOIN stories s ON s.id = em.story_id
         WHERE s.source_type = 'financial'
           AND em.mentioned_at >= date(?1, ?2) AND em.mentioned_at <= ?1
         GROUP BY tm.topic, tm.sector",
        &[&today as &dyn rusqlite::ToSql, &window_clause as &dyn rusqlite::ToSql],
    ).unwrap_or_default();

    // Stage 6 — Federal Register mentions (last 30d).
    let reg_count_map: HashMap<Key, f64> = load_metric(
        "SELECT tm.topic, tm.sector, CAST(COUNT(*) AS REAL)
         FROM entity_mentions em
         JOIN temp_topic_map tm ON tm.entity_id = em.entity_id
         JOIN stories s ON s.id = em.story_id
         WHERE s.source_name LIKE '%Federal Register%'
           AND em.mentioned_at >= date(?1, '-30 days') AND em.mentioned_at <= ?1
         GROUP BY tm.topic, tm.sector",
        &[&today],
    ).unwrap_or_default();

    // Stage 7 — patent filings (USPTO / Google Patents, last 30d).
    let patent_map: HashMap<Key, f64> = load_metric(
        "SELECT tm.topic, tm.sector, CAST(COUNT(*) AS REAL)
         FROM entity_mentions em
         JOIN temp_topic_map tm ON tm.entity_id = em.entity_id
         JOIN stories s ON s.id = em.story_id
         WHERE (s.source_name = 'USPTO' OR s.source_name = 'Google Patents')
           AND em.mentioned_at >= date(?1, '-30 days') AND em.mentioned_at <= ?1
         GROUP BY tm.topic, tm.sector",
        &[&today],
    ).unwrap_or_default();

    // Stage 8 — 8-K event severity (max severity, last 14d).
    let event_severity_map: HashMap<Key, f64> = load_metric(
        "SELECT tm.topic, tm.sector, COALESCE(MAX(
            CASE WHEN json_valid(s.financial_metadata)
                 AND json_extract(s.financial_metadata, '$.event_severity') IS NOT NULL
            THEN CAST(json_extract(s.financial_metadata, '$.event_severity') AS REAL)
            ELSE 0 END
         ), 0)
         FROM entity_mentions em
         JOIN temp_topic_map tm ON tm.entity_id = em.entity_id
         JOIN stories s ON s.id = em.story_id
         WHERE s.source_name LIKE '%EDGAR 8-K%'
           AND em.mentioned_at >= date(?1, '-14 days') AND em.mentioned_at <= ?1
         GROUP BY tm.topic, tm.sector",
        &[&today],
    ).unwrap_or_default();

    // Stage 9 — Wikipedia search-trend delta (last 7d).
    let search_map: HashMap<Key, f64> = load_metric(
        "SELECT tm.topic, tm.sector, COALESCE(SUM(
            CASE WHEN json_valid(s.financial_metadata)
                 AND s.source_name = 'Wikipedia Pageviews'
            THEN COALESCE(CAST(json_extract(s.financial_metadata, '$.views_delta_pct') AS REAL), 0)
            ELSE 0 END
         ), 0)
         FROM entity_mentions em
         JOIN temp_topic_map tm ON tm.entity_id = em.entity_id
         JOIN stories s ON s.id = em.story_id
         WHERE s.source_type = 'financial'
           AND em.mentioned_at >= date(?1, '-7 days') AND em.mentioned_at <= ?1
         GROUP BY tm.topic, tm.sector",
        &[&today],
    ).unwrap_or_default();

    // Stage 10 — institutional flow (13F filings). Parse all 13F holdings JSON
    // ONCE upfront, build issuer-name index, then look up per topic.
    let inst_flow_map: HashMap<Key, f64> = {
        let mut stmt = conn.prepare(
            "SELECT s.financial_metadata FROM stories s
             WHERE s.source_name LIKE '%13F%'
               AND s.financial_metadata IS NOT NULL
               AND json_valid(s.financial_metadata)
               AND json_extract(s.financial_metadata, '$.holdings') IS NOT NULL
               AND s.published_at >= date(?1, '-90 days') AND s.published_at <= ?1"
        )?;

        let metadata_rows: Vec<String> = stmt.query_map([today], |row| row.get::<_, String>(0))?
            .filter_map(|r| r.ok())
            .collect();

        // Pre-extract normalized issuer names per filing.
        let normalize = |s: &str| -> String {
            s.to_uppercase()
                .replace(" INC", "").replace(" CORP", "").replace(" CO", "")
                .replace(" LTD", "").replace(" LLC", "").replace(" PLC", "")
                .replace(",", "").replace(".", "").trim().to_string()
        };

        // Per filing: set of normalized issuer names it holds.
        let filings_issuers: Vec<std::collections::HashSet<String>> = metadata_rows.iter()
            .filter_map(|md_str| serde_json::from_str::<serde_json::Value>(md_str).ok())
            .map(|md| {
                md.get("holdings")
                    .and_then(|h| h.as_array())
                    .map(|arr| arr.iter()
                        .filter_map(|h| h.get("issuer").and_then(|i| i.as_str()).map(normalize))
                        .collect::<std::collections::HashSet<_>>())
                    .unwrap_or_default()
            })
            .collect();

        // For each (topic, sector), count filings whose normalized issuer set
        // matches (substring either way). This is still O(topics × filings) but
        // the inner work is set lookup not SQL — typically a few hundred filings.
        let topic_keys: Vec<(String, Option<String>)> = window_rows.iter()
            .map(|(t, s, _, _, _, _)| (t.clone(), s.clone()))
            .collect();

        let mut map: HashMap<Key, f64> = HashMap::new();
        for (topic, sector) in &topic_keys {
            let topic_norm = normalize(topic);
            if topic_norm.is_empty() { continue; }
            let mut count = 0i64;
            for issuer_set in &filings_issuers {
                let hit = issuer_set.iter().any(|issuer_norm| {
                    issuer_norm == &topic_norm
                        || issuer_norm.contains(&topic_norm)
                        || topic_norm.contains(issuer_norm)
                });
                if hit { count += 1; }
            }
            if count > 0 {
                map.insert(key(topic, sector), count as f64);
            }
        }
        map
    };

    // Stage 11 — sector-level FRED indicator (single value, applied to all rows).
    // 2026-07-23: was hardcoded to `datetime('now', '-7 days')`, ignoring the
    // `today` param every other stage in this function uses — a historical
    // backfill run would always pull TODAY's FRED value regardless of the date
    // being recomputed. Bound to `today` so a backfill sees the value as of
    // that date, not the live value.
    let import_delta: f64 = conn.query_row(
        "SELECT COALESCE(AVG(ABS(
            CAST(json_extract(s.financial_metadata, '$.change_pct') AS REAL)
         )), 0)
         FROM stories s
         WHERE s.source_name = 'FRED'
           AND json_valid(s.financial_metadata)
           AND json_extract(s.financial_metadata, '$.series_id') IN ('INDPRO', 'PPIACO', 'DCOILWTICO')
           AND s.created_at >= datetime(?1, '-7 days')",
        rusqlite::params![today],
        |row| row.get(0),
    ).unwrap_or(0.0);

    // Final write — single transaction, two SQL statements per topic
    // (upsert windows + update metrics). All metric lookups are O(1).
    let tx = conn.unchecked_transaction()?;
    let mut count = 0usize;

    for (topic, sector, w7, w30, w90, days_active) in &window_rows {
        let rate_7d = *w7 as f64 / 7.0;
        let rate_30d = *w30 as f64 / 30.0;
        let acc = if *w30 == 0 { if *w7 > 0 { 10.0 } else { 0.0 } }
            else if rate_30d < 0.001 { if *w7 > 0 { 10.0 } else { 0.0 } }
            else { rate_7d / rate_30d };

        let total = (*w30).max(*w7);
        let traj = if *w7 == 0 && *w30 == 0 { "dormant" }
            else if total >= 14 && *days_active >= 10 { "dominant" }
            else if total >= 7 && *days_active >= 5 { "hot" }
            else if acc < 0.8 && total >= 3 { "fading" }
            else if total >= 3 || *days_active >= 2 { "rising" }
            else if *w7 > 0 { "rising" }
            else { "dormant" };

        tx.execute(
            "INSERT INTO signals (topic, sector, window_7d, window_30d, window_90d, acceleration, trajectory, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, datetime('now'))
             ON CONFLICT(topic, sector) DO UPDATE SET
                 window_7d = excluded.window_7d, window_30d = excluded.window_30d,
                 window_90d = excluded.window_90d, acceleration = excluded.acceleration,
                 trajectory = excluded.trajectory, updated_at = datetime('now')",
            rusqlite::params![topic, sector, w7, w30, w90, acc, traj],
        ).ok();

        let k = key(topic, sector);
        let diversity = diversity_map.get(&k).copied().unwrap_or(0.0) as i64;
        let insider_vol = insider_map.get(&k).copied().unwrap_or(0.0);
        let contract_val = contract_map.get(&k).copied().unwrap_or(0.0);
        let lobby_spend = lobby_map.get(&k).copied().unwrap_or(0.0);
        let reg_count = reg_count_map.get(&k).copied().unwrap_or(0.0);
        let patent_count = patent_map.get(&k).copied().unwrap_or(0.0);
        let event_boost = event_severity_map.get(&k).copied().unwrap_or(0.0);
        let inst_flow = inst_flow_map.get(&k).copied().unwrap_or(0.0);
        let search_delta = search_map.get(&k).copied().unwrap_or(0.0);

        // Composite regulatory_sentiment: Fed Reg count + 8-K event boost.
        // The *3.0 boost is a manually-set constant (Task 3.4,
        // calibration-backtest-universe audit, 2026-07-23) — deliberately NOT
        // part of automated calibration. The corrected historical backfill
        // found only ~4-5 independent resolved episodes; fitting a free
        // parameter here on that sample would overfit, not improve accuracy.
        // Revisit once forward paper-trading data accumulates enough volume.
        let reg_composite = reg_count + (event_boost * 3.0);

        tx.execute(
            "UPDATE signals SET source_diversity = ?1, insider_buy_volume = ?2, contract_value = ?3,
                 lobbying_spend_delta = ?4, regulatory_sentiment = ?5, patent_filing_rate = ?6,
                 institutional_flow = ?7, search_trend_delta = ?8, import_volume_delta = ?9
             WHERE topic = ?10 AND (sector = ?11 OR (?11 IS NULL AND sector IS NULL))",
            rusqlite::params![diversity, insider_vol, contract_val, lobby_spend, reg_composite, patent_count,
                              inst_flow, search_delta, import_delta, topic, sector],
        ).ok();

        count += 1;
    }

    tx.commit()?;
    conn.execute_batch("DROP TABLE IF EXISTS temp_topic_map;").ok();

    Ok(count)
}

/// Sigmoid normalization: maps raw value to [0, 1]. Matches cross_signals.rs.
pub(crate) fn normalize_signal(value: f64, scale: f64) -> f64 {
    if value <= 0.0 { return 0.0; }
    1.0 - (-value / scale).exp()
}

/// Compute cross-signal scores for all entities.
/// Unified with src-tauri/src/services/cross_signals.rs — same 8 dimensions,
/// same weights, same convergence threshold (3+ signals > 0.3, diversity >= 3).
/// `as_of` stamps the `computed_at` column — live callers pass today's date
/// (no behavior change), a historical backfill driver passes a past date.
/// NOTE: this function reads whatever is CURRENTLY in the upsert-only `signals`
/// table — it has no history layer, so a backfill run must call
/// `recompute_signals_pipeline(conn, as_of)` immediately before this for the
/// same `as_of`, per date, in ascending order.
pub(crate) fn compute_cross_signals(db_path: &Path, as_of: &str) -> anyhow::Result<usize> {
    let conn = rusqlite::Connection::open(db_path)?;
    let today = as_of;

    // Load calibrated weights if available, else use defaults
    let weights = load_calibrated_weights(&conn);

    // Get all non-dormant signals with ALL 8 dimensions
    let mut stmt = conn.prepare(
        "SELECT s.topic, s.sector, s.window_7d, s.window_30d, s.acceleration,
                COALESCE(s.source_diversity, 0),
                COALESCE(s.insider_buy_volume, 0),
                COALESCE(s.institutional_flow, 0),
                COALESCE(s.contract_value, 0),
                COALESCE(s.patent_filing_rate, 0),
                COALESCE(s.search_trend_delta, 0),
                COALESCE(s.import_volume_delta, 0),
                COALESCE(s.regulatory_sentiment, 0),
                COALESCE(s.lobbying_spend_delta, 0)
         FROM signals s
         WHERE s.trajectory != 'dormant'
         ORDER BY s.window_30d DESC"
    )?;

    let rows: Vec<(String, Option<String>, i64, i64, f64, i64, f64, f64, f64, f64, f64, f64, f64, f64)> = stmt
        .query_map([], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?,
                row.get(4)?, row.get(5)?, row.get(6)?, row.get(7)?,
                row.get(8)?, row.get(9)?, row.get(10)?, row.get(11)?,
                row.get(12)?, row.get(13)?))
        })?
        .filter_map(|r| r.ok())
        .collect();

    let mut count = 0;
    for (topic, _sector, w7, _w30, acceleration, src_diversity,
         insider_vol, inst_flow, contract_val, patent_rate,
         search_delta, import_delta, reg_sentiment, lobby_delta) in &rows
    {
        // Normalize each dimension.
        let insider_norm = normalize_signal(*insider_vol, 1_000_000.0);
        let inst_norm = normalize_signal(*inst_flow, 15.0); // Count of distinct 13F filers holding this stock
        // Pure acceleration ratio, gated by minimum sample.
        let news_norm = if *w7 >= 3 { normalize_signal(*acceleration, 2.0) } else { 0.0 };
        // Government signal: contract value + regulatory/8-K severity composite.
        // 2026-07-23: scale was $10M — measured against real USASpending data,
        // even the 10th percentile contract ($69.5M) already saturates a $10M
        // sigmoid to 99.9%, and ALL 543 nonzero contract_value rows in the DB
        // crossed the 0.3 threshold. Every contract from $12M to $410B was
        // normalizing to ~1.0, giving contract_norm zero discriminative power
        // despite government_signal carrying real 0.1848 weight. Rescaled to
        // $200M, anchored to the real median ($220M) so the sigmoid actually
        // spreads across the observed range (p10=$69.5M, p75=$797M, p90=$3.1B).
        let contract_norm = normalize_signal(*contract_val, 200_000_000.0);
        let reg_norm = normalize_signal(*reg_sentiment, 3.0);
        // The 0.6/0.4 blend is a manually-set constant (Task 3.4,
        // calibration-backtest-universe audit, 2026-07-23) — NOT part of
        // automated calibration, same reasoning as reg_composite's *3.0 above:
        // more free parameters fit to a ~4-5-episode sample is overfitting,
        // not signal. Revisit once forward paper-trading data accumulates.
        let gov_norm = (contract_norm * 0.6 + reg_norm * 0.4).min(1.0);
        let search_norm = normalize_signal(*search_delta, 50.0);
        let patent_norm = normalize_signal(*patent_rate, 5.0);
        let supply_norm = normalize_signal(*import_delta, 30.0);
        let political_norm = normalize_signal(*lobby_delta, 100_000.0);

        // Weighted compound score
        let compound = (insider_norm * weights[0]
            + inst_norm * weights[1]
            + news_norm * weights[2]
            + gov_norm * weights[3]
            + search_norm * weights[4]
            + patent_norm * weights[5]
            + supply_norm * weights[6]
            + political_norm * weights[7])
            .max(0.0).min(1.0);

        // Convergence: 3+ signals > 0.3 AND source_diversity >= 3
        // institutional_flow and supply_chain are excluded here — both are
        // zero-weighted in `compound` (weights[1]/weights[6], known bugs: the
        // substring-match false-positive and the market-wide constant), so
        // they must not be allowed to swing the convergence gate either.
        // Measured 2026-07-23: 363/1130 real candidates had institutional_flow
        // > 0.3, and 3 of those relied on it as the deciding vote (only 1 other
        // real dim also > 0.3) — including META on 2026-04-27.
        let positive = [insider_norm, news_norm, gov_norm,
            search_norm, patent_norm, political_norm]
            .iter().filter(|&&v| v > 0.3).count();
        // Convergence: 2+ signals > 0.3 AND diversity >= 2, OR very high compound score
        let convergence = (positive >= 2 && *src_diversity >= 2) || compound >= 0.40;

        if compound < 0.01 && !convergence {
            continue;
        }

        // Look up entity_id and ticker — prefer canonical entity for lookup
        let (eid, ticker) = {
            // Try canonical first
            let canonical: Option<(i64, Option<String>)> = conn.query_row(
                "SELECT ec.id, ec.ticker FROM entity_canonical ec WHERE ec.canonical_name = ?1",
                [topic], |row| Ok((row.get(0)?, row.get(1)?)),
            ).ok();

            if let Some((cid, ct)) = canonical {
                // Get first linked entity_id for the cross_signals table
                let eid: i64 = conn.query_row(
                    "SELECT id FROM entities WHERE canonical_id = ?1 LIMIT 1",
                    [cid], |row| row.get(0),
                ).unwrap_or(0);
                // Prefer canonical ticker, fall back to entity_tickers
                let ticker = ct.or_else(|| {
                    conn.query_row(
                        "SELECT et.ticker FROM entity_tickers et JOIN entities e ON e.id = et.entity_id WHERE e.canonical_id = ?1 LIMIT 1",
                        [cid], |row| row.get(0),
                    ).ok()
                });
                (eid, ticker)
            } else {
                // Fall back to direct name match
                let eid: i64 = conn.query_row(
                    "SELECT id FROM entities WHERE LOWER(name) = LOWER(?1) LIMIT 1",
                    [topic], |row| row.get(0),
                ).unwrap_or(0);
                let ticker: Option<String> = if eid > 0 {
                    conn.query_row("SELECT ticker FROM entity_tickers WHERE entity_id = ?1", [eid], |row| row.get(0)).ok()
                } else { None };
                (eid, ticker)
            }
        };

        conn.execute(
            "INSERT OR REPLACE INTO cross_signals (entity_id, ticker, compound_score,
                insider_signal, institutional_flow, news_momentum, government_signal,
                search_trend, patent_signal, supply_chain, political_signal,
                source_diversity, convergence_detected, computed_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
            rusqlite::params![
                eid, ticker, compound,
                insider_norm, inst_norm, news_norm, gov_norm,
                search_norm, patent_norm, supply_norm, political_norm,
                src_diversity, convergence as i32, today,
            ],
        ).ok();

        count += 1;
    }

    Ok(count)
}

/// Load calibrated weights from DB, or return defaults.
/// Matches the weight order: [insider, institutional, news, government, search, patent, supply_chain, political]
pub(crate) fn load_calibrated_weights(conn: &rusqlite::Connection) -> [f64; 8] {
    // [insider, institutional, news, government, search, patent, supply, political]
    // institutional_flow (idx 1) and supply_chain (idx 6) are ZEROED: diagnosis
    // (2026-06-05) found supply_chain is a market-wide constant with no per-entity
    // discriminative power, and institutional_flow fires on a substring-match bug
    // (ticker "X" matched 61 funds) — both were injecting noise / dilution. Their
    // 0.08 is redistributed proportionally across the 6 live signals so weights
    // still sum to 1.0. Restore when those signals are fixed (see findings).
    //
    // 2026-07-23: derived from calibration::DEFAULT_WEIGHTS (single source of
    // truth) instead of a second hand-typed copy of the same 8 numbers — the
    // two had already drifted apart once (calibration-backtest-universe audit).
    let default_weight = |key: &str| -> f64 {
        crate::calibration::DEFAULT_WEIGHTS.iter()
            .find(|(k, _)| *k == key)
            .map(|(_, v)| *v)
            .unwrap_or(0.0)
    };
    let defaults = [
        default_weight("insider_signal"),
        default_weight("institutional_flow"),
        default_weight("news_momentum"),
        default_weight("government_signal"),
        default_weight("search_trend"),
        default_weight("patent_signal"),
        default_weight("supply_chain"),
        default_weight("political_signal"),
    ];

    let json: Option<String> = conn.query_row(
        "SELECT value FROM user_profile WHERE key = 'calibrated_weights'",
        [], |row| row.get(0),
    ).ok();

    if let Some(json_str) = json {
        if let Ok(pairs) = serde_json::from_str::<Vec<(String, f64)>>(&json_str) {
            let mut w = defaults;
            for (key, val) in &pairs {
                match key.as_str() {
                    "insider_signal" => w[0] = *val,
                    "institutional_flow" => w[1] = *val,
                    "news_momentum" => w[2] = *val,
                    "government_signal" => w[3] = *val,
                    "search_trend" => w[4] = *val,
                    "patent_signal" => w[5] = *val,
                    "supply_chain" => w[6] = *val,
                    "political_signal" => w[7] = *val,
                    _ => {}
                }
            }
            return w;
        }
    }

    defaults
}
