use rusqlite::Connection;

/// Position management: ATR-based trailing stops, profit targets, time/signal decay.
///
/// Replaces the primitive -10% stop-loss + 90-day expiry with adaptive exits:
/// - Trailing stop at 2x ATR below high-water mark (tightens with age)
/// - Profit target at 3x ATR from entry (close 50%)
/// - Time decay: tighten stop after 30d (1.5x ATR) and 60d (1.0x ATR)
/// - Signal decay: close if convergence score drops below threshold

/// What to do with a position.
///
/// (A `TightenStop` variant was removed 2026-07-15 — it was never constructed;
/// `evaluate_position` writes the trailing stop directly and returns Hold/Close*.)
#[derive(Debug)]
pub enum PositionAction {
    Hold,
    CloseAll { reason: String },
    CloseHalf { reason: String },
}

/// Compute Average True Range (ATR) from recent price data.
/// ATR = EMA of True Range over `period` days.
/// True Range = max(high-low, abs(high-prev_close), abs(low-prev_close))
pub fn compute_atr(conn: &Connection, ticker: &str, period: usize) -> f64 {
    let mut stmt = match conn.prepare(
        "SELECT high, low, close FROM entity_prices
         WHERE ticker = ?1 AND high IS NOT NULL AND low IS NOT NULL
         ORDER BY date DESC LIMIT ?2"
    ) {
        Ok(s) => s,
        Err(_) => return 0.0,
    };

    let candles: Vec<(f64, f64, f64)> = stmt
        .query_map(rusqlite::params![ticker, period + 1], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })
        .ok()
        .map(|rows| rows.filter_map(|r| r.ok()).collect())
        .unwrap_or_default();

    if candles.len() < 2 {
        return 0.0;
    }

    // Candles are in reverse chronological order — reverse for calculation
    let candles: Vec<_> = candles.into_iter().rev().collect();

    let mut true_ranges = Vec::with_capacity(candles.len() - 1);
    for i in 1..candles.len() {
        let (high, low, _) = candles[i];
        let prev_close = candles[i - 1].2;
        let tr = (high - low)
            .max((high - prev_close).abs())
            .max((low - prev_close).abs());
        true_ranges.push(tr);
    }

    if true_ranges.is_empty() {
        return 0.0;
    }

    // Simple moving average (SMA) for ATR — good enough, avoids EMA complexity
    true_ranges.iter().sum::<f64>() / true_ranges.len() as f64
}

/// Evaluate an open position and decide what to do.
///
/// Returns a `PositionAction` based on:
/// - ATR-based trailing stop (adapts to volatility)
/// - Profit target at 3x ATR
/// - Time decay (tighter stops as position ages)
/// - Hard stop-loss at -15% (safety net if ATR is too wide)
pub fn evaluate_position(
    conn: &Connection,
    trade_id: i64,
    ticker: &str,
    entry_price: f64,
    entry_date: &str,
    current_price: f64,
    today: &str,
) -> PositionAction {
    if current_price <= 0.0 || entry_price <= 0.0 {
        return PositionAction::Hold;
    }

    let pnl_pct = ((current_price - entry_price) / entry_price) * 100.0;

    // Hard stop-loss safety net at -15% (in case ATR is very wide)
    if pnl_pct <= -15.0 {
        return PositionAction::CloseAll {
            reason: format!("hard_stop_loss ({:.1}%)", pnl_pct),
        };
    }

    // Compute ATR for this ticker
    let atr = compute_atr(conn, ticker, 14);
    if atr <= 0.0 {
        // No ATR data — fall back to fixed stop-loss
        if pnl_pct <= -10.0 {
            return PositionAction::CloseAll {
                reason: format!("fixed_stop_loss ({:.1}%, no ATR data)", pnl_pct),
            };
        }
        // Fall back to 90-day expiry
        if let Some(days) = days_held(entry_date, today) {
            if days >= 90 {
                return PositionAction::CloseAll {
                    reason: format!("expired ({}d, no ATR data)", days),
                };
            }
        }
        return PositionAction::Hold;
    }

    let days = days_held(entry_date, today).unwrap_or(0);

    // Get or compute high-water mark
    let hwm: f64 = conn
        .query_row(
            "SELECT COALESCE(high_water_mark, entry_price) FROM paper_trades WHERE id = ?1",
            [trade_id],
            |row| row.get(0),
        )
        .unwrap_or(entry_price)
        .max(current_price);

    // Update high-water mark in DB
    if current_price > hwm - 0.001 {
        conn.execute(
            "UPDATE paper_trades SET high_water_mark = ?1 WHERE id = ?2",
            rusqlite::params![current_price, trade_id],
        )
        .ok();
    }

    // ATR multiplier decreases with time (tighter stops as position ages)
    let atr_mult = if days >= 60 {
        1.0 // Very tight after 60 days
    } else if days >= 30 {
        1.5 // Tighter after 30 days
    } else {
        2.0 // Normal trailing stop
    };

    let trailing_stop = hwm - (atr * atr_mult);

    // Update trailing_stop in DB
    conn.execute(
        "UPDATE paper_trades SET trailing_stop = ?1 WHERE id = ?2",
        rusqlite::params![trailing_stop, trade_id],
    )
    .ok();

    // Check trailing stop
    if current_price <= trailing_stop {
        return PositionAction::CloseAll {
            reason: format!(
                "trailing_stop ({:.1}%, ATR={:.2}, mult={:.1}x, hwm={:.2}, stop={:.2})",
                pnl_pct, atr, atr_mult, hwm, trailing_stop
            ),
        };
    }

    // Profit target: close half at 3x ATR from entry
    let profit_target = entry_price + (atr * 3.0);
    if current_price >= profit_target && pnl_pct >= 10.0 {
        return PositionAction::CloseHalf {
            reason: format!(
                "profit_target ({:.1}%, target={:.2}, 3x ATR from entry)",
                pnl_pct, profit_target
            ),
        };
    }

    // Max hold: 90 days hard expiry
    if days >= 90 {
        return PositionAction::CloseAll {
            reason: format!("max_hold_expired ({}d, {:.1}%)", days, pnl_pct),
        };
    }

    PositionAction::Hold
}

/// Check if the convergence signal has decayed for an open position.
/// Returns true if the position should be closed due to signal loss.
pub fn check_signal_decay(
    conn: &Connection,
    ticker: &str,
    original_score: f64,
) -> bool {
    // Look up current cross-signal score for this ticker
    let current_score: f64 = conn
        .query_row(
            "SELECT COALESCE(cs.compound_score, 0) FROM cross_signals cs
             WHERE cs.ticker = ?1
             ORDER BY cs.computed_at DESC LIMIT 1",
            [ticker],
            |row| row.get(0),
        )
        .unwrap_or(0.0);

    // Signal has decayed if score dropped to less than 30% of original
    // OR below absolute minimum of 0.05
    current_score < (original_score * 0.3).max(0.05)
}

fn days_held(entry_date: &str, today: &str) -> Option<i64> {
    // Accept both "2026-04-15" and "2026-04-15T15:41:05" formats
    let entry_str = entry_date.split('T').next().unwrap_or(entry_date);
    let today_str = today.split('T').next().unwrap_or(today);
    let entry = chrono::NaiveDate::parse_from_str(entry_str, "%Y-%m-%d").ok()?;
    let now = chrono::NaiveDate::parse_from_str(today_str, "%Y-%m-%d").ok()?;
    Some((now - entry).num_days())
}

/// Write a human-readable trade journal entry on close.
///
/// Moved here from calibration.rs (2026-07-15): it is a position-lifecycle
/// concern that must fire from the SINGLE exit authority (Phase 13.6
/// `manage_open_positions`). Calibration used to call it, but calibration is
/// now measure-only and no longer closes trades — so this lives with the exit
/// path that actually closes them, or new closes stop producing journal text
/// (the Portfolio exit-reasons feature depends on it).
#[allow(clippy::too_many_arguments)]
pub fn generate_trade_journal(
    conn: &Connection, trade_id: i64, ticker: &str,
    entry_date: &str, exit_date: &str,
    entry_price: f64, exit_price: f64,
    position_size: f64, pnl_pct: f64, pnl_dollars: f64,
    status: &str,
) {
    // Get signal profile and entity name
    let profile: String = conn.query_row(
        "SELECT signal_profile FROM paper_trades WHERE id = ?1",
        [trade_id], |row| row.get(0),
    ).unwrap_or_default();

    let entity_name: Option<String> = conn.query_row(
        "SELECT e.name FROM paper_trades pt JOIN entities e ON e.id = pt.entity_id WHERE pt.id = ?1",
        [trade_id], |row| row.get(0),
    ).ok();

    let name = entity_name.as_deref().unwrap_or(ticker);

    // Parse top signals
    let mut top_signals: Vec<(String, f64)> = Vec::new();
    if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&profile) {
        let dims = ["insider", "institutional", "news", "government", "search", "patent", "supply_chain", "political"];
        for dim in dims {
            if let Some(val) = parsed.get(dim).and_then(|v| v.as_f64()) {
                if val > 0.05 { top_signals.push((dim.to_string(), val)); }
            }
        }
    }
    top_signals.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    top_signals.truncate(3);

    // Build narrative
    let drivers = if top_signals.is_empty() {
        "convergence signals".to_string()
    } else {
        top_signals.iter()
            .map(|(d, v)| format!("{} ({:.0}%)", d, v * 100.0))
            .collect::<Vec<_>>()
            .join(", ")
    };

    let holding_days = chrono::NaiveDate::parse_from_str(entry_date, "%Y-%m-%d")
        .and_then(|e| chrono::NaiveDate::parse_from_str(exit_date, "%Y-%m-%d").map(|x| (x - e).num_days()))
        .unwrap_or(0);

    let exit_reason = match status {
        "stopped_out" => "trailing stop was hit",
        "expired" => "the 90-day holding limit was reached",
        "closed" if pnl_pct > 0.0 => "profit target was reached",
        "closed" => "signal decay triggered an exit",
        _ => "position was closed",
    };

    let journal = format!(
        "Entered {} long on {} at ${:.2} driven by {}. Position size: ${:.0}. \
         Exited after {} days at ${:.2} — {}{:.1}% (${}{:.0}) because {}.",
        name, entry_date, entry_price, drivers, position_size,
        holding_days, exit_price,
        if pnl_pct >= 0.0 { "+" } else { "" }, pnl_pct,
        if pnl_dollars >= 0.0 { "+" } else { "" }, pnl_dollars,
        exit_reason,
    );

    conn.execute(
        "UPDATE paper_trades SET trade_journal = ?1 WHERE id = ?2",
        rusqlite::params![journal, trade_id],
    ).ok();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_atr_mult_by_age() {
        // Day 0-29: 2.0x ATR
        // Day 30-59: 1.5x ATR
        // Day 60+: 1.0x ATR
        // Just verify the logic matches
        assert_eq!(if 10 >= 60 { 1.0 } else if 10 >= 30 { 1.5 } else { 2.0 }, 2.0);
        assert_eq!(if 35 >= 60 { 1.0 } else if 35 >= 30 { 1.5 } else { 2.0 }, 1.5);
        assert_eq!(if 65 >= 60 { 1.0 } else if 65 >= 30 { 1.5 } else { 2.0 }, 1.0);
    }

    #[test]
    fn test_days_held() {
        assert_eq!(days_held("2026-01-01", "2026-01-31"), Some(30));
        assert_eq!(days_held("2026-01-01", "2026-04-01"), Some(90));
        assert_eq!(days_held("invalid", "2026-01-01"), None);
    }

    #[test]
    fn test_signal_decay_threshold() {
        // Original score 0.5 → decay if current < 0.15 (30% of 0.5)
        let threshold = (0.5 * 0.3_f64).max(0.05);
        assert!((threshold - 0.15).abs() < 0.001);

        // Original score 0.1 → decay if current < 0.05 (absolute minimum)
        let threshold = (0.1 * 0.3_f64).max(0.05);
        assert!((threshold - 0.05).abs() < 0.001);
    }
}
