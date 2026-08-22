use rusqlite::Connection;

/// Position management: ATR-based trailing stops, profit targets, signal decay.
///
/// Long-term design (2026-07-23): no calendar-based max hold — a position is
/// held indefinitely until a stop, target, or signal decay closes it. The
/// trailing stop is flat (does not tighten with age) so short-term volatility
/// doesn't shake out a long-term thesis.
/// - Trailing stop at 3x ATR below high-water mark (flat, no time-based tightening)
/// - Profit target at 3x ATR from entry (close 50%)
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

/// A price bar older than this says nothing about today's volatility.
///
/// `entity_prices` retains rows for tickers that have stopped updating. Only
/// ~200 of its ~1190 tickers can be quoted on any given day (see
/// `market_prices::DAILY_PRICE_SLOTS`), so coverage rotates and names drop out:
/// measured 2026-08-17, 161 tickers were stale table-wide, and among the names
/// that had actually signalled convergence, 14 were stale and 15 had never been
/// priced at all. Without a date bound, `compute_atr` will happily build a 3x
/// ATR trailing stop out of ten-week-old candles, and `evaluate_position` never
/// reaches its documented `atr <= 0.0` fallback because rows do exist. 45 days
/// leaves room for holidays and gaps while still covering a 14-day window.
pub const ATR_MAX_STALENESS_DAYS: i64 = 45;

/// Compute Average True Range (ATR) from recent price data.
/// ATR = EMA of True Range over `period` days.
/// True Range = max(high-low, abs(high-prev_close), abs(low-prev_close))
///
/// Returns 0.0 — meaning "no ATR", not "zero volatility" — when the data
/// cannot support one: no bars inside the freshness window, or too few of
/// them. Callers must treat 0.0 as the no-ATR fallback, never as a stop
/// distance.
pub fn compute_atr(conn: &Connection, ticker: &str, period: usize) -> f64 {
    let cutoff = (chrono::Local::now() - chrono::Duration::days(ATR_MAX_STALENESS_DAYS))
        .format("%Y-%m-%d")
        .to_string();

    let mut stmt = match conn.prepare(
        "SELECT high, low, close FROM entity_prices
         WHERE ticker = ?1 AND date >= ?2 AND high IS NOT NULL AND low IS NOT NULL
         ORDER BY date DESC LIMIT ?3"
    ) {
        Ok(s) => s,
        Err(_) => return 0.0,
    };

    let candles: Vec<(f64, f64, f64)> = stmt
        .query_map(rusqlite::params![ticker, cutoff, period + 1], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })
        .ok()
        .map(|rows| rows.filter_map(|r| r.ok()).collect())
        .unwrap_or_default();

    // A handful of fresh bars is not a 14-day ATR. Two candles yield a single
    // true range, and one quiet day would put the 3x trailing stop within
    // pennies of the high-water mark — an instant exit on noise. Below half
    // the period, report no ATR and let the fixed stop take over.
    if candles.len() < period / 2 + 1 {
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
/// Long-term design — no calendar-based expiry. Returns a `PositionAction`
/// based on:
/// - ATR-based trailing stop, flat 3x ATR regardless of how long it's been held
/// - Profit target at 3x ATR
/// - Hard stop-loss at -15% (safety net if ATR is too wide)
pub fn evaluate_position(
    conn: &Connection,
    trade_id: i64,
    ticker: &str,
    entry_price: f64,
    current_price: f64,
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
        // No ATR data — fall back to fixed stop-loss. No time-based fallback
        // expiry anymore (long-term design has no calendar-based max hold).
        if pnl_pct <= -10.0 {
            return PositionAction::CloseAll {
                reason: format!("fixed_stop_loss ({:.1}%, no ATR data)", pnl_pct),
            };
        }
        return PositionAction::Hold;
    }

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

    // Flat trailing stop — does not tighten with age (long-term design:
    // short-term volatility shouldn't shake out a long-term thesis).
    let atr_mult = 3.0;
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

    PositionAction::Hold
}

/// How old a `cross_signals` row may be and still count as this ticker's
/// current reading.
///
/// Signals recompute daily, so one missed day must not read as decay — but a
/// ticker that has dropped out of the topic set for three days genuinely has no
/// current signal, and the old query had no bound at all: it would happily
/// return a reading from months earlier as "current".
const SIGNAL_STALE_AFTER_DAYS: i64 = 3;

/// The share of a normal day's signal output the recent window must reach before
/// a missing reading may be believed.
///
/// Deliberately low. This is a catastrophe detector, not a quality bar: it must
/// fire on "the pipeline is broken", not on "today was quiet". Erring low means
/// occasionally holding a position a day longer; erring high means selling the
/// book because the fetcher had a bad morning.
const MIN_HEALTHY_OUTPUT_FRACTION: f64 = 0.25;

/// Whether the pipeline is producing enough signal output to believe an absence.
///
/// `history` is one row count per day the pipeline wrote anything in the last 30
/// days; the median of those is "a normal day". An existence check is not enough
/// — a degraded run that writes 20 rows where 500 is normal satisfies it while
/// leaving almost every ticker unscored, and every unscored open position then
/// reads as fully decayed.
///
/// With no history at all (cold start, fresh DB) this returns false: nothing is
/// known about normal, so nothing may be concluded from an absence.
pub(crate) fn pipeline_output_is_healthy(recent_rows: i64, history: &[i64]) -> bool {
    let mut days: Vec<i64> = history.iter().copied().filter(|&c| c > 0).collect();
    if days.is_empty() {
        return false;
    }
    days.sort_unstable();
    let median = days[days.len() / 2] as f64;
    recent_rows as f64 >= median * MIN_HEALTHY_OUTPUT_FRACTION
}

/// Whether an open position's entry thesis has decayed.
///
/// Split out from the queries so the three-way decision is testable, because
/// the interesting case is not arithmetic — it is telling "this ticker went
/// quiet" apart from "the pipeline produced nothing for anyone".
///
/// `latest_fresh` is `None` when this ticker has no reading inside the staleness
/// window. That is real decay: `unwrap_or(0.0)` in the original already encoded
/// the intent that no signal means zero, it just never noticed that a
/// months-old row is no signal.
///
/// `pipeline_ran_recently` is the guard that makes the above safe. Pulse's fetch
/// pipeline dies for days at a time — Groq IP blocks (2026-07-03..12,
/// 2026-08-08..13) and the model decommission that produced no briefing from
/// 2026-08-16 to 2026-08-22. During those windows NO ticker has a fresh row.
/// Without this flag, the first run after an outage would read every open
/// position as fully decayed and liquidate the entire book on the strength of
/// the pipeline being broken. An outage is missing information, not a sell
/// signal, so decay abstains until the pipeline is producing again.
pub(crate) fn signal_has_decayed(
    original_score: f64,
    latest_fresh: Option<f64>,
    pipeline_ran_recently: bool,
) -> bool {
    if !pipeline_ran_recently {
        return false;
    }
    // Signal has decayed if score dropped to less than 30% of original
    // OR below absolute minimum of 0.05
    latest_fresh.unwrap_or(0.0) < (original_score * 0.3).max(0.05)
}

/// Check if the convergence signal has decayed for an open position.
/// Returns true if the position should be closed due to signal loss.
pub fn check_signal_decay(
    conn: &Connection,
    ticker: &str,
    original_score: f64,
) -> bool {
    let window = format!("-{} days", SIGNAL_STALE_AFTER_DAYS);

    // This ticker's current reading, or None if its freshest row is stale.
    let latest_fresh: Option<f64> = conn
        .query_row(
            "SELECT cs.compound_score FROM cross_signals cs
             WHERE cs.ticker = ?1 AND cs.computed_at >= date('now', ?2)
             ORDER BY cs.computed_at DESC LIMIT 1",
            rusqlite::params![ticker, &window],
            |row| row.get(0),
        )
        .ok();

    // Did the pipeline produce a NORMAL amount of output in that window?
    //
    // Existence is not enough. A degraded run reaches the cross-signals stage
    // with few topics and writes tens of rows where a healthy day writes
    // hundreds — which is exactly the shape of the 2026-08-17..22 outage, where
    // `stories` kept trickling in at 1–46/day against a normal 205–911. Under an
    // existence check that reads as "pipeline ran", and every open position whose
    // ticker missed the cut gets `latest_fresh = None` and is sold. That is the
    // whole-book liquidation this guard exists to prevent, produced by the guard.
    let recent_rows: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM cross_signals WHERE computed_at >= date('now', ?1)",
            rusqlite::params![&window],
            |row| row.get(0),
        )
        .unwrap_or(0);
    let history: Vec<i64> = conn
        .prepare(
            "SELECT COUNT(*) FROM cross_signals
             WHERE computed_at >= date('now', '-30 days')
             GROUP BY date(computed_at)",
        )
        .ok()
        .map(|mut stmt| {
            stmt.query_map([], |row| row.get(0))
                .ok()
                .map(|rows| rows.filter_map(|r| r.ok()).collect::<Vec<_>>())
                .unwrap_or_default()
        })
        .unwrap_or_default();
    let pipeline_ran_recently = pipeline_output_is_healthy(recent_rows, &history);

    if !pipeline_ran_recently {
        tracing::warn!(
            "Signal decay: abstaining for {} — only {} cross_signals rows in the last {} days, \
             so a missing reading means the pipeline is degraded, not that the thesis died",
            ticker, recent_rows, SIGNAL_STALE_AFTER_DAYS
        );
    }

    signal_has_decayed(original_score, latest_fresh, pipeline_ran_recently)
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

    // 'expired' no longer describes anything this engine does — the 90-day
    // calendar limit it used to name was removed on 2026-07-15 (see
    // evaluate_position, which has no time-based exit at all) and nothing has
    // written the status since. Left mapped rather than dropped so an older
    // database still renders, but it no longer claims a rule that was deleted.
    let exit_reason = match status {
        "stopped_out" => "trailing stop was hit",
        "expired" => "the retired calendar-expiry engine closed it (that rule no longer exists)",
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
mod atr_recency_tests {
    use super::*;

    /// A ticker with `count` daily bars, the newest `age_days` old.
    /// Each bar has a 2.00-wide high/low range, so a healthy ATR is ~2.00.
    fn seed(conn: &Connection, ticker: &str, count: i64, age_days: i64) {
        for i in 0..count {
            let date = (chrono::Local::now() - chrono::Duration::days(age_days + i))
                .format("%Y-%m-%d")
                .to_string();
            conn.execute(
                "INSERT INTO entity_prices (ticker, date, high, low, close)
                 VALUES (?1, ?2, 101.0, 99.0, 100.0)",
                rusqlite::params![ticker, date],
            )
            .unwrap();
        }
    }

    fn db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE entity_prices (
                ticker TEXT NOT NULL, date TEXT NOT NULL,
                high REAL, low REAL, close REAL NOT NULL,
                UNIQUE(ticker, date));",
        )
        .unwrap();
        conn
    }

    #[test]
    fn a_ticker_that_stopped_updating_reports_no_atr() {
        let conn = db();
        // 20 bars, but the newest is 73 days old — the real INTC/META shape:
        // last bar 2026-06-03 while the table's max date was 2026-08-15.
        seed(&conn, "STALE", 20, 73);
        assert_eq!(
            compute_atr(&conn, "STALE", 14),
            0.0,
            "ten-week-old candles must not set a live trailing stop"
        );
    }

    #[test]
    fn too_few_fresh_bars_report_no_atr() {
        let conn = db();
        // Fresh, but only 4 bars — under half the 14-day period. A 3-true-range
        // average is not an ATR, and a thin one puts the 3x stop on top of the
        // high-water mark.
        seed(&conn, "SPARSE", 4, 1);
        assert_eq!(compute_atr(&conn, "SPARSE", 14), 0.0);
    }

    #[test]
    fn a_currently_traded_ticker_still_gets_its_atr() {
        let conn = db();
        seed(&conn, "FRESH", 20, 1);
        let atr = compute_atr(&conn, "FRESH", 14);
        assert!(
            (atr - 2.0).abs() < 0.001,
            "expected the 2.00 daily range, got {atr}"
        );
    }

    #[test]
    fn stale_bars_do_not_pad_out_a_sparse_fresh_window() {
        let conn = db();
        // 3 fresh bars plus 20 ancient ones. Unbounded, the query would fill
        // its LIMIT from the old rows and return a confident ATR.
        seed(&conn, "MIXED", 3, 1);
        seed(&conn, "MIXED", 20, 200);
        assert_eq!(compute_atr(&conn, "MIXED", 14), 0.0);
    }

    #[test]
    fn no_atr_falls_back_to_the_fixed_stop_not_an_instant_exit() {
        let conn = db();
        conn.execute_batch(
            "CREATE TABLE paper_trades (id INTEGER PRIMARY KEY,
                entry_price REAL, high_water_mark REAL, trailing_stop REAL);
             INSERT INTO paper_trades (id, entry_price) VALUES (1, 100.0);",
        )
        .unwrap();
        seed(&conn, "STALE", 20, 73);

        // Down 5% on a stale ticker: hold, because there is no ATR to stop on.
        assert!(matches!(
            evaluate_position(&conn, 1, "STALE", 100.0, 95.0),
            PositionAction::Hold
        ));
        // Down 12%: the documented fixed -10% fallback, not a trailing stop.
        match evaluate_position(&conn, 1, "STALE", 100.0, 88.0) {
            PositionAction::CloseAll { reason } => {
                assert!(reason.starts_with("fixed_stop_loss"), "got {reason}")
            }
            other => panic!("expected the no-ATR fixed stop, got {other:?}"),
        }
    }
}

#[cfg(test)]
mod tests {
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

#[cfg(test)]
mod signal_decay_tests {
    use super::{check_signal_decay, pipeline_output_is_healthy, signal_has_decayed};
    use rusqlite::Connection;

    const ORIG: f64 = 0.3408; // AIRI's stored entry score; threshold is 0.1022

    #[test]
    fn a_healthy_current_signal_holds_the_position() {
        assert!(!signal_has_decayed(ORIG, Some(0.32), true));
    }

    #[test]
    fn a_collapsed_current_signal_closes_it() {
        assert!(signal_has_decayed(ORIG, Some(0.05), true));
    }

    /// The half the old query got wrong. A ticker with no reading inside the
    /// window has no signal, and no signal is decay — `unwrap_or(0.0)` always
    /// meant that, it just never noticed a months-old row was not a reading.
    #[test]
    fn a_ticker_that_went_quiet_has_decayed() {
        assert!(signal_has_decayed(ORIG, None, true));
    }

    /// The half that makes the above safe to ship. Pulse's pipeline dies for
    /// days at a time; on the first run after an outage every position looks
    /// fully decayed at once. Liquidating the book because the fetcher broke is
    /// worse than holding through it.
    #[test]
    fn an_outage_never_liquidates_the_book() {
        assert!(!signal_has_decayed(ORIG, None, false));
        // Even a genuinely collapsed reading is not acted on mid-outage, because
        // during an outage we cannot tell a real reading from a stale one.
        assert!(!signal_has_decayed(ORIG, Some(0.01), false));
    }

    /// The 0.05 floor governs when 30% of the original is below it, so a trade
    /// entered on a weak signal cannot be held forever by a proportional rule.
    #[test]
    fn the_absolute_floor_governs_a_weak_entry() {
        // 30% of 0.10 is 0.03, under the 0.05 floor — so 0.04 must still close.
        assert!(signal_has_decayed(0.10, Some(0.04), true));
        assert!(!signal_has_decayed(0.10, Some(0.06), true));
    }

    fn db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE cross_signals (
                 id INTEGER PRIMARY KEY AUTOINCREMENT, ticker TEXT,
                 compound_score REAL NOT NULL, computed_at TEXT);",
        )
        .unwrap();
        conn
    }

    fn row(conn: &Connection, ticker: &str, score: f64, days_ago: i64) {
        conn.execute(
            "INSERT INTO cross_signals (ticker, compound_score, computed_at)
             VALUES (?1, ?2, date('now', ?3))",
            rusqlite::params![ticker, score, format!("-{} days", days_ago)],
        )
        .unwrap();
    }

    /// AIRI's live shape on 2026-08-22: its newest row was 39 days old and the
    /// old query read it as current, holding a -6.96% position on a signal from
    /// before the trade's own entry month. Other tickers ARE fresh here, so the
    /// outage guard does not apply and decay is free to fire.
    #[test]
    fn a_month_old_row_is_not_a_current_reading() {
        let conn = db();
        row(&conn, "AIRI", 0.3408, 39);
        row(&conn, "OTHER", 0.42, 0); // pipeline is demonstrably alive
        assert!(
            check_signal_decay(&conn, "AIRI", ORIG),
            "a 39-day-old row must not hold a position open"
        );
    }

    /// Same stale row, but now nothing is fresh for anyone — the 2026-08-16..22
    /// outage. The identical input must produce the opposite decision.
    #[test]
    fn the_same_stale_row_abstains_when_the_pipeline_is_down() {
        let conn = db();
        row(&conn, "AIRI", 0.3408, 39);
        assert!(
            !check_signal_decay(&conn, "AIRI", ORIG),
            "with no fresh rows for any ticker, a missing reading means the pipeline is down"
        );
    }

    /// One missed day is not decay — signals recompute daily and the window has
    /// deliberate slack.
    #[test]
    fn a_one_day_gap_is_tolerated() {
        let conn = db();
        row(&conn, "AAA", 0.30, 1);
        assert!(!check_signal_decay(&conn, "AAA", ORIG));
    }

    /// An entirely empty table is the cold-start case: no pipeline history at
    /// all must not be read as every position having decayed.
    #[test]
    fn an_empty_table_abstains() {
        let conn = db();
        assert!(!check_signal_decay(&conn, "AAA", ORIG));
    }

    fn bulk(conn: &Connection, days_ago: i64, n: usize) {
        for i in 0..n {
            row(conn, &format!("T{}_{}", days_ago, i), 0.42, days_ago);
        }
    }

    /// The mutation the existence check could not survive. A DEGRADED run — the
    /// exact shape of 2026-08-17..22, where stories trickled at 1-46/day against
    /// a normal 205-911 — reaches the cross-signals stage and writes 20 rows
    /// where 500 is normal. `EXISTS` reads that as "the pipeline ran", every open
    /// position outside those 20 tickers scores as fully decayed, and the exit
    /// engine sells the whole book. Volume, not existence, is the discriminator.
    #[test]
    fn a_degraded_run_does_not_liquidate_the_book() {
        let conn = db();
        for d in 4..30 {
            bulk(&conn, d, 500);
        }
        bulk(&conn, 0, 20); // today: 4% of a normal day
        assert!(
            !check_signal_decay(&conn, "AIRI", ORIG),
            "20 rows against a 500-row norm is a broken pipeline, not 480 dead theses"
        );
    }

    /// The other side of the same discriminator: a full-volume run really does
    /// mean a ticker missing from it has no signal, and decay must still fire.
    /// Without this the guard could be satisfied by never firing at all.
    #[test]
    fn a_healthy_run_still_decays_a_ticker_it_omitted() {
        let conn = db();
        for d in 4..30 {
            bulk(&conn, d, 500);
        }
        bulk(&conn, 0, 500);
        assert!(
            check_signal_decay(&conn, "AIRI", ORIG),
            "a ticker absent from a full run genuinely has no signal"
        );
    }

    /// A quiet-but-real day sits between the two. The bar is deliberately low —
    /// a quarter of a normal day — because this is a catastrophe detector, not a
    /// quality bar, and holding a position one day too long is the cheap error.
    #[test]
    fn the_bar_is_a_quarter_of_a_normal_day() {
        let history: Vec<i64> = vec![500; 26];
        assert!(!pipeline_output_is_healthy(124, &history));
        assert!(pipeline_output_is_healthy(125, &history));
    }

    /// The median ignores days the pipeline wrote nothing, so a stretch of
    /// outage days cannot drag the baseline down until a broken run looks normal.
    #[test]
    fn outage_days_do_not_lower_the_baseline() {
        let mut history: Vec<i64> = vec![0; 20];
        history.extend(vec![400; 10]);
        assert!(
            !pipeline_output_is_healthy(50, &history),
            "zero-days must not be averaged in as normal"
        );
    }

    /// No history at all means nothing is known about normal, so nothing may be
    /// concluded from an absence.
    #[test]
    fn a_cold_start_is_never_healthy() {
        assert!(!pipeline_output_is_healthy(1000, &[]));
        assert!(!pipeline_output_is_healthy(0, &[0, 0]));
    }
}
