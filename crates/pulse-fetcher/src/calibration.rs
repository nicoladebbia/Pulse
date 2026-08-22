use rusqlite::Connection;
use std::path::Path;

/// Automated signal calibration system (Phase 8).
///
/// After each pipeline run:
/// 1. Evaluate open paper trades against current prices
/// 2. Compute Brier scores for resolved predictions
/// 3. Track which signal dimensions predict actual outcomes
/// 4. Propose a reweight based on empirical hit rates — written to
///    `pending_calibration` for manual approval, NOT applied automatically
///    (2026-07-23, Task 3.3 — see `adjust_weights` doc comment for why)
/// 5. Flag dead signals (consistently < 50% hit rate) for review
///
/// Runs as pipeline Phase 14 (after cross-signal detection).

/// The compound-score weight vector now lives in the `pulse-weights` crate, so
/// that `src-tauri` can read it without depending on this one. Re-exported here
/// because everything in this module already refers to it by this path, and
/// because this is where the rationale for each zeroed dimension is looked for.
pub(crate) use pulse_weights::DEFAULT_WEIGHTS;

/// Run the full calibration pipeline.
pub async fn run_calibration(db_path: &Path) -> anyhow::Result<CalibrationReport> {
    let conn = Connection::open(db_path)?;
    let today = chrono::Local::now().format("%Y-%m-%dT%H:%M:%S").to_string();

    let mut report = CalibrationReport::default();

    // 1. Evaluate open paper trades against current prices
    report.positions_evaluated = evaluate_open_positions(&conn, &today).await?;

    // 2. Compute Brier scores for predictions
    report.brier_scores_updated = compute_brier_scores(&conn)?;

    // 3. Analyze signal dimension hit rates
    report.signal_analysis = analyze_signal_performance(&conn)?;

    // 4. Propose a reweight if we have enough data — holds in pending_calibration,
    // does not touch live weights (see adjust_weights doc comment). Threshold
    // duplicated in src-tauri/src/commands/trading.rs::get_calibration_gate_status
    // for the UI's progress display — keep both in sync if this changes.
    if report.signal_analysis.total_resolved >= 10 {
        report.weights_adjusted = adjust_weights(&conn, &report.signal_analysis)?;
    }

    // 5. Generate calibration summary
    report.confidence_calibration = compute_confidence_calibration(&conn)?;

    Ok(report)
}

#[derive(Debug, Default)]
pub struct CalibrationReport {
    pub positions_evaluated: usize,
    pub brier_scores_updated: usize,
    pub signal_analysis: SignalAnalysis,
    /// True if a reweight PROPOSAL was written to `pending_calibration` this
    /// run — NOT that live weights changed. See `adjust_weights`.
    pub weights_adjusted: bool,
    pub confidence_calibration: Vec<(f64, f64, usize)>, // (predicted, actual, count)
}

#[derive(Debug, Default)]
pub struct SignalAnalysis {
    pub total_resolved: usize,
    pub overall_hit_rate: f64,
    pub dimension_hit_rates: Vec<(String, f64, usize)>, // (dimension, hit_rate, sample_size)
    pub dead_signals: Vec<String>, // dimensions with < 50% hit rate
}

/// Refresh the displayed P&L on open paper trades (MEASURE-ONLY).
/// Does NOT close positions or place orders — Phase 13.6 `manage_open_positions`
/// is the single exit authority. See the body comment for why.
async fn evaluate_open_positions(conn: &Connection, _today: &str) -> anyhow::Result<usize> {
    let open_trades: Vec<(i64, String, f64, String, f64, f64)> = {
        let mut stmt = conn.prepare(
            "SELECT id, ticker, entry_price, entry_date, COALESCE(original_compound_score, confidence), position_size
             FROM paper_trades WHERE status = 'open'"
        )?;
        stmt.query_map([], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?,
                row.get::<_, f64>(4).unwrap_or(0.5), row.get::<_, f64>(5).unwrap_or(0.0)))
        })?
        .filter_map(|r| r.ok())
        .collect()
    };

    if open_trades.is_empty() {
        return Ok(0);
    }

    tracing::info!("Calibration: measuring P&L on {} open positions (measure-only)", open_trades.len());

    // MEASURE-ONLY (2026-07-15). Calibration used to be a SECOND seller here:
    // it called evaluate_position + placed Alpaca sell orders + closed trades,
    // bypassing the EXIT_DRY_RUN kill switch entirely (a duplicate of Phase 13.6
    // `manage_open_positions` without the flag). That is removed. Phase 13.6 is
    // now the SINGLE exit authority; calibration only refreshes the displayed
    // P&L on still-open positions.
    //
    // Deliberately does NOT call evaluate_position/check_signal_decay: those
    // write high_water_mark/trailing_stop, and doing so here — from STALE
    // entity_prices — would fight Phase 13.6's writes from fresh Alpaca prices,
    // corrupting the exact stop 13.6 depends on. Raw P&L from last close only.
    let mut evaluated = 0;
    for (trade_id, ticker, entry_price, _entry_date, _original_score, position_size) in &open_trades {
        let current_price: f64 = conn
            .query_row(
                "SELECT close FROM entity_prices WHERE ticker = ?1 ORDER BY date DESC LIMIT 1",
                [ticker],
                |row| row.get(0),
            )
            .unwrap_or(0.0);

        if current_price <= 0.0 {
            continue;
        }

        let pnl_pct = ((current_price - entry_price) / entry_price) * 100.0;
        let pnl_dollars = pnl_pct / 100.0 * position_size;

        conn.execute(
            "UPDATE paper_trades SET pnl = ?1, pnl_pct = ?2 WHERE id = ?3",
            rusqlite::params![pnl_dollars, pnl_pct, trade_id],
        ).ok();
        tracing::info!("Position P&L: {} — {:.1}% (${:.2})", ticker, pnl_pct, pnl_dollars);
        evaluated += 1;
    }

    Ok(evaluated)
}

/// Compute Brier scores for predictions that have been validated or invalidated.
fn compute_brier_scores(conn: &Connection) -> anyhow::Result<usize> {
    let mut stmt = conn.prepare(
        "SELECT id, confidence, status FROM insights
         WHERE insight_type = 'prediction'
         AND status IN ('validated', 'invalidated')
         AND brier_score IS NULL"
    )?;

    let to_score: Vec<(i64, f64, String)> = stmt
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?
        .filter_map(|r| r.ok())
        .collect();

    let mut updated = 0;
    for (id, confidence, status) in &to_score {
        let outcome: f64 = if status == "validated" { 1.0 } else { 0.0 };
        let brier = (confidence - outcome).powi(2);
        conn.execute(
            "UPDATE insights SET brier_score = ?1 WHERE id = ?2",
            rusqlite::params![brier, id],
        )?;
        updated += 1;
    }

    Ok(updated)
}

/// Analyze which signal dimensions actually predict positive outcomes.
fn analyze_signal_performance(conn: &Connection) -> anyhow::Result<SignalAnalysis> {
    let mut analysis = SignalAnalysis::default();

    // Get resolved trades with their signal profiles
    let mut stmt = conn.prepare(
        "SELECT signal_profile, pnl_pct FROM paper_trades
         WHERE status IN ('closed', 'stopped_out', 'expired')
         AND pnl_pct IS NOT NULL"
    )?;

    let trades: Vec<(String, f64)> = stmt
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
        .filter_map(|r| r.ok())
        .collect();

    analysis.total_resolved = trades.len();
    if trades.is_empty() {
        return Ok(analysis);
    }

    let wins = trades.iter().filter(|(_, pnl)| *pnl > 0.0).count();
    analysis.overall_hit_rate = wins as f64 / trades.len() as f64;

    // Per-dimension analysis: for each dimension, what's the hit rate when it's positive?
    let dimensions = [
        "insider", "news", "government", "institutional",
        "search", "patent", "supply_chain", "political",
    ];

    for dim in &dimensions {
        let mut dim_trades = 0;
        let mut dim_wins = 0;

        for (profile_json, pnl) in &trades {
            if let Ok(profile) = serde_json::from_str::<serde_json::Value>(profile_json) {
                let val = profile.get(dim).and_then(|v| v.as_f64()).unwrap_or(0.0);
                if val > 0.2 {
                    dim_trades += 1;
                    if *pnl > 0.0 {
                        dim_wins += 1;
                    }
                }
            }
        }

        if dim_trades > 0 {
            let hit_rate = dim_wins as f64 / dim_trades as f64;
            analysis.dimension_hit_rates.push((
                dim.to_string(),
                hit_rate,
                dim_trades,
            ));
            if hit_rate < 0.5 && dim_trades >= 5 {
                analysis.dead_signals.push(dim.to_string());
            }
        }
    }

    Ok(analysis)
}

/// Compute a proposed reweight from empirical performance and hold it in
/// `pending_calibration` for manual approval — does NOT touch
/// `user_profile.calibrated_weights` (see `apply_pending_calibration` in
/// src-tauri/src/commands/trading.rs for the explicit apply step).
///
/// 2026-07-23 (calibration-backtest-universe audit, Task 3.3): this used to
/// INSERT OR REPLACE the live weights directly, silently, the moment
/// total_resolved crossed 10 — no significance test, no held-out check, no
/// human in the loop. The corrected 41-day historical backfill (Finding C)
/// found only ~4-5 independent resolved episodes even with clean signal
/// computation, and the live paper_trades sample is smaller still — nowhere
/// near enough to trust an automatic reweight. Proposals are logged loudly
/// and held until Nicola explicitly applies them.
fn adjust_weights(conn: &Connection, analysis: &SignalAnalysis) -> anyhow::Result<bool> {
    if analysis.dimension_hit_rates.is_empty() {
        return Ok(false);
    }

    // Compute the full proposed weight vector (same math as before)
    let mut weights: Vec<(String, f64)> = DEFAULT_WEIGHTS
        .iter()
        .map(|(k, v)| (k.to_string(), *v))
        .collect();

    // dimension -> (hit_rate, sample_size), for logging into pending_calibration
    let mut applied_dims: Vec<(&str, f64, usize)> = Vec::new();

    for (dim, hit_rate, sample_size) in &analysis.dimension_hit_rates {
        // Only propose an adjustment if we have enough data
        if *sample_size < 5 {
            continue;
        }

        // Map dimension name to weight key
        let weight_key = match dim.as_str() {
            "insider" => "insider_signal",
            "news" => "news_momentum",
            "government" => "government_signal",
            "institutional" => "institutional_flow",
            "search" => "search_trend",
            "patent" => "patent_signal",
            "supply_chain" => "supply_chain",
            "political" => "political_signal",
            _ => continue,
        };

        // Adjust weight: multiply by (hit_rate / 0.5) to scale around baseline
        // Hit rate 0.7 → multiply by 1.4, hit rate 0.3 → multiply by 0.6
        let adjustment = hit_rate / 0.5;
        for (k, v) in weights.iter_mut() {
            if k == weight_key {
                *v *= adjustment;
            }
        }
        applied_dims.push((weight_key, *hit_rate, *sample_size));
    }

    if applied_dims.is_empty() {
        tracing::info!("Calibration: no dimension had >=5 resolved samples — no proposal generated");
        return Ok(false);
    }

    // Normalize weights to sum to 1.0
    let total: f64 = weights.iter().map(|(_, v)| *v).sum();
    if total > 0.0 {
        for (_, v) in weights.iter_mut() {
            *v /= total;
        }
    }

    let batch_id = uuid_like_batch_id();
    let computed_at = chrono::Local::now().format("%Y-%m-%dT%H:%M:%S").to_string();
    let old_by_key: std::collections::HashMap<&str, f64> =
        DEFAULT_WEIGHTS.iter().map(|(k, v)| (*k, *v)).collect();

    for (key, new_weight) in &weights {
        let old_weight = *old_by_key.get(key.as_str()).unwrap_or(&0.0);
        let (hit_rate, sample_size) = applied_dims
            .iter()
            .find(|(k, _, _)| *k == key.as_str())
            .map(|(_, hr, n)| (Some(*hr), Some(*n as i64)))
            .unwrap_or((None, None));
        conn.execute(
            "INSERT INTO pending_calibration
                (batch_id, computed_at, dimension, old_weight, new_weight, hit_rate, sample_size, total_resolved, status)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 'pending')",
            rusqlite::params![
                batch_id,
                computed_at,
                key,
                old_weight,
                new_weight,
                hit_rate,
                sample_size,
                analysis.total_resolved as i64,
            ],
        )?;
    }

    tracing::warn!(
        "Calibration: PROPOSAL {} computed from {} resolved trades — held for manual approval, NOT applied. Review with get_pending_calibration, apply with apply_pending_calibration.",
        batch_id, analysis.total_resolved
    );
    for (dim, hit_rate, n) in &analysis.dimension_hit_rates {
        tracing::info!("  {} hit rate: {:.0}% (n={})", dim, hit_rate * 100.0, n);
    }
    if !analysis.dead_signals.is_empty() {
        tracing::warn!("  Dead signals (< 50% hit rate): {}", analysis.dead_signals.join(", "));
    }

    Ok(true)
}

/// Cheap, dependency-free batch id — not a real UUID, just unique enough to
/// group one calibration run's dimension rows together for review/apply.
fn uuid_like_batch_id() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    format!("cal-{}-{}", now.as_secs(), now.subsec_nanos())
}

/// Compute confidence calibration curve.
/// Groups predictions by confidence bucket and compares predicted vs actual hit rate.
fn compute_confidence_calibration(conn: &Connection) -> anyhow::Result<Vec<(f64, f64, usize)>> {
    let mut stmt = conn.prepare(
        "SELECT confidence, status FROM insights
         WHERE insight_type = 'prediction'
         AND status IN ('validated', 'invalidated')"
    )?;

    let predictions: Vec<(f64, bool)> = stmt
        .query_map([], |row| {
            let conf: f64 = row.get(0)?;
            let status: String = row.get(1)?;
            Ok((conf, status == "validated"))
        })?
        .filter_map(|r| r.ok())
        .collect();

    if predictions.is_empty() {
        return Ok(Vec::new());
    }

    // Bucket by confidence: 0.0-0.2, 0.2-0.4, 0.4-0.6, 0.6-0.8, 0.8-1.0
    let buckets = [(0.0, 0.2), (0.2, 0.4), (0.4, 0.6), (0.6, 0.8), (0.8, 1.0)];
    let mut calibration = Vec::new();

    for (lo, hi) in &buckets {
        let bucket: Vec<&(f64, bool)> = predictions
            .iter()
            .filter(|(c, _)| *c >= *lo && *c < *hi)
            .collect();

        if !bucket.is_empty() {
            let predicted_avg = bucket.iter().map(|(c, _)| c).sum::<f64>() / bucket.len() as f64;
            let actual_rate = bucket.iter().filter(|(_, v)| *v).count() as f64 / bucket.len() as f64;
            calibration.push((predicted_avg, actual_rate, bucket.len()));
        }
    }

    Ok(calibration)
}

// Trade journal auto-generation on close moved to position_management.rs
// (2026-07-15) — it now fires from the single exit authority (Phase 13.6),
// since calibration is measure-only and no longer closes trades.

#[cfg(test)]
mod weight_tests {
    use super::DEFAULT_WEIGHTS;

    /// The compound score is a weighted average that nothing renormalises at
    /// runtime, so this sum IS the scale every gate is quoted against. Let it
    /// drift and `compound >= 0.40` silently means something different without
    /// any gate changing — which is exactly the failure that motivated the
    /// 2026-08-17 reweight in the first place.
    #[test]
    fn weights_sum_to_one() {
        let sum: f64 = DEFAULT_WEIGHTS.iter().map(|(_, w)| w).sum();
        assert!(
            (sum - 1.0).abs() < 1e-3,
            "DEFAULT_WEIGHTS sum to {sum}, not 1.0 — every gate is now quoted against the wrong scale"
        );
    }

    #[test]
    fn a_zeroed_dimension_is_exactly_zero() {
        // Not "small". A dark dimension left at a small weight still drags the
        // score toward zero for every entity; the point of zeroing is that its
        // share moves to a dimension that can actually earn it.
        for dim in ["institutional_flow", "supply_chain", "political_signal", "patent_signal", "search_trend"] {
            let w = DEFAULT_WEIGHTS.iter().find(|(k, _)| *k == dim).map(|(_, v)| *v);
            assert_eq!(w, Some(0.0), "{dim} is documented as dark but carries weight {w:?}");
        }
    }

    #[test]
    fn every_live_dimension_carries_weight() {
        // The mirror of the above: if a dimension is not in the dark list it
        // must be earning its share, or the sum invariant is being satisfied by
        // a dimension nobody is scoring.
        for dim in ["insider_signal", "news_momentum", "government_signal"] {
            let w = DEFAULT_WEIGHTS.iter().find(|(k, _)| *k == dim).map(|(_, v)| *v).unwrap_or(0.0);
            assert!(w > 0.0, "{dim} is treated as live but carries no weight");
        }
    }

    #[test]
    fn the_live_dimensions_keep_their_relative_ordering() {
        // Redistribution must be proportional. If a future edit rebalances by
        // hand and accidentally reorders insider/news against government, the
        // score changes meaning even though the sum still checks out.
        let w = |k: &str| DEFAULT_WEIGHTS.iter().find(|(n, _)| *n == k).map(|(_, v)| *v).unwrap();
        assert!((w("insider_signal") - w("news_momentum")).abs() < 1e-6,
            "insider and news were equal by design (0.2391 each) and must stay equal");
        assert!(w("insider_signal") > w("government_signal"),
            "government was the smaller of the three live weights and must stay smaller");
    }
}
