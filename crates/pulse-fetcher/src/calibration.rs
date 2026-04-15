use rusqlite::Connection;
use std::path::Path;

/// Automated signal calibration system (Phase 8).
///
/// After each pipeline run:
/// 1. Evaluate open paper trades against current prices
/// 2. Compute Brier scores for resolved predictions
/// 3. Track which signal dimensions predict actual outcomes
/// 4. Auto-adjust signal weights based on empirical hit rates
/// 5. Prune dead signals (consistently < 50% hit rate)
///
/// Runs as pipeline Phase 14 (after cross-signal detection).

/// Calibration weights stored in DB for persistence across runs.
/// Zeroed dimensions (no data source) excluded from compound score.
/// Active weights sum to 1.0.
const DEFAULT_WEIGHTS: &[(&str, f64)] = &[
    ("insider_signal", 0.22),
    ("institutional_flow", 0.05),   // SEC 13F institutional holdings (new, low weight until calibrated)
    ("news_momentum", 0.22),
    ("government_signal", 0.18),
    ("search_trend", 0.05),         // Wikipedia Pageviews as proxy (new, low weight until calibrated)
    ("patent_signal", 0.05),
    ("supply_chain", 0.03),         // EIA energy trade (new, low weight until calibrated)
    ("political_signal", 0.20),
];

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

    // 4. Auto-adjust weights if we have enough data
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

/// Evaluate open paper trades using ATR-based position management.
/// Closes positions via Alpaca API when exit conditions are met.
async fn evaluate_open_positions(conn: &Connection, today: &str) -> anyhow::Result<usize> {
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

    tracing::info!("Calibration: evaluating {} open positions", open_trades.len());

    // Set up Alpaca client for selling
    let alpaca_key = std::env::var("ALPACA_API_KEY").unwrap_or_default();
    let alpaca_secret = std::env::var("ALPACA_SECRET_KEY").unwrap_or_default();
    let has_alpaca = !alpaca_key.is_empty() && !alpaca_secret.is_empty();
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()?;

    let mut evaluated = 0;
    for (trade_id, ticker, entry_price, entry_date, original_score, position_size) in &open_trades {
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

        use crate::position_management::{evaluate_position, check_signal_decay, PositionAction};

        let action = evaluate_position(conn, *trade_id, ticker, *entry_price, entry_date, current_price, today);
        let signal_decayed = check_signal_decay(conn, ticker, *original_score);

        // Determine if we need to close (full or half)
        let (should_close, close_fraction, status_str, reason) = match &action {
            PositionAction::CloseAll { reason } => (true, 1.0, "stopped_out", reason.clone()),
            PositionAction::CloseHalf { reason } => (true, 0.5, "open", reason.clone()),
            PositionAction::Hold if signal_decayed => {
                let s = if pnl_pct > 0.0 { "closed" } else { "expired" };
                (true, 1.0, s, "signal_decay".to_string())
            }
            PositionAction::TightenStop { new_stop } => {
                conn.execute(
                    "UPDATE paper_trades SET trailing_stop = ?1 WHERE id = ?2",
                    rusqlite::params![new_stop, trade_id],
                )?;
                (false, 0.0, "", String::new())
            }
            _ => (false, 0.0, "", String::new()),
        };

        if !should_close {
            // Update live P&L for tracking even on Hold
            conn.execute(
                "UPDATE paper_trades SET pnl = ?1, pnl_pct = ?2 WHERE id = ?3",
                rusqlite::params![pnl_dollars, pnl_pct, trade_id],
            ).ok();
            tracing::info!("Position hold: {} — {:.1}% (${:.2})", ticker, pnl_pct, pnl_dollars);
            evaluated += 1;
            continue;
        }

        // Send sell order to Alpaca
        if has_alpaca {
            let sell_notional = position_size * close_fraction;
            let sell_order = serde_json::json!({
                "symbol": ticker,
                "notional": format!("{:.2}", sell_notional),
                "side": "sell",
                "type": "market",
                "time_in_force": "day"
            });

            match client
                .post("https://paper-api.alpaca.markets/v2/orders")
                .header("APCA-API-KEY-ID", &alpaca_key)
                .header("APCA-API-SECRET-KEY", &alpaca_secret)
                .json(&sell_order)
                .send()
                .await
            {
                Ok(resp) if resp.status().is_success() => {
                    tracing::info!("Alpaca sell order placed: {} ${:.2} ({})", ticker, sell_notional, reason);
                }
                Ok(resp) => {
                    let body = resp.text().await.unwrap_or_default();
                    tracing::warn!("Alpaca sell failed for {}: {}", ticker, body);
                }
                Err(e) => {
                    tracing::warn!("Alpaca sell request failed for {}: {}", ticker, e);
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        }

        // Update DB
        if close_fraction >= 1.0 {
            conn.execute(
                "UPDATE paper_trades SET status = ?1, exit_price = ?2, exit_date = ?3, pnl = ?4, pnl_pct = ?5 WHERE id = ?6",
                rusqlite::params![status_str, current_price, today, pnl_dollars, pnl_pct, trade_id],
            )?;
            tracing::info!("Position closed: {} — {:.1}% (${:.2}) — {}", ticker, pnl_pct, pnl_dollars, reason);
        } else {
            // Half close — reduce position size, don't change status
            let new_size = position_size * (1.0 - close_fraction);
            conn.execute(
                "UPDATE paper_trades SET position_size = ?1 WHERE id = ?2",
                rusqlite::params![new_size, trade_id],
            )?;
            tracing::info!("Position halved: {} — {:.1}% — {}", ticker, pnl_pct, reason);
        }
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

/// Auto-adjust signal weights based on empirical performance.
fn adjust_weights(conn: &Connection, analysis: &SignalAnalysis) -> anyhow::Result<bool> {
    if analysis.dimension_hit_rates.is_empty() {
        return Ok(false);
    }

    // Store calibrated weights as a JSON config in the DB
    let mut weights: Vec<(String, f64)> = DEFAULT_WEIGHTS
        .iter()
        .map(|(k, v)| (k.to_string(), *v))
        .collect();

    for (dim, hit_rate, sample_size) in &analysis.dimension_hit_rates {
        // Only adjust if we have enough data
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
    }

    // Normalize weights to sum to 1.0
    let total: f64 = weights.iter().map(|(_, v)| *v).sum();
    if total > 0.0 {
        for (_, v) in weights.iter_mut() {
            *v /= total;
        }
    }

    // Store calibrated weights
    let weights_json = serde_json::to_string(&weights)?;
    conn.execute(
        "INSERT OR REPLACE INTO user_profile (key, value) VALUES ('calibrated_weights', ?1)",
        [&weights_json],
    )?;

    tracing::info!("Calibration: adjusted signal weights based on {} resolved trades", analysis.total_resolved);
    for (dim, hit_rate, n) in &analysis.dimension_hit_rates {
        tracing::info!("  {} hit rate: {:.0}% (n={})", dim, hit_rate * 100.0, n);
    }
    if !analysis.dead_signals.is_empty() {
        tracing::warn!("  Dead signals (< 50% hit rate): {}", analysis.dead_signals.join(", "));
    }

    Ok(true)
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
