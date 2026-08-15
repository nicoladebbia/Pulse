use super::*;

/// Log API usage to the database (opens its own connection for real-time visibility).
///
/// `pub(crate)` so out-of-pipeline modes (e.g. `--mode backfill-embeddings` in main.rs)
/// account for their spend too — the backfill used to be invisible to the daily cost cap.
pub(crate) fn log_usage(db_path: &Path, provider: &str, model: &str, endpoint: &str, input_tokens: i64, output_tokens: i64) {
    if let Ok(conn) = rusqlite::Connection::open(db_path) {
        crate::db::log_api_usage(&conn, provider, model, endpoint, input_tokens, output_tokens);
    }
}

/// Default daily cost cap in USD. Override via PULSE_DAILY_COST_CAP env var.
///
/// This is a RUNAWAY GUARD, not a budget — it should sit well above a busy-but-healthy
/// day so it only ever fires on a genuine loop. Raised 0.50 -> 2.00 on 2026-08-15 after
/// the old value started tripping on normal use: measured spend Jul 25 - Aug 14 was
/// $0.24-$0.37 on a plain day, $0.46 on a day with one manual rerun, and $0.50 on
/// 2026-08-14 — which aborted `run_freedoms` (it runs last, so it always starves first).
/// A full healthy day is ~$0.33: daily pipeline ~$0.30 plus freedoms ~$0.034. At $2.00
/// there is ~6x headroom for reruns while a real runaway (10-100x) still trips it.
///
/// Note this is checked at the TOP of `run()` / `run_freedoms()`, not per API call, so a
/// single run that goes haywire is not stopped mid-flight — the cap only prevents the
/// NEXT run from starting. Raising it does not change that; per-call enforcement would.
pub(crate) const DEFAULT_DAILY_COST_CAP_USD: f64 = 2.00;

/// Read today's accumulated API spend and abort if it's over the cap.
/// Called at the top of `run()` / `run_freedoms()` so a stuck loop cannot keep
/// burning money across multiple manual reruns in the same day.
pub(crate) fn check_daily_cost_cap(db_path: &Path) -> anyhow::Result<()> {
    let cap = std::env::var("PULSE_DAILY_COST_CAP")
        .ok()
        .and_then(|v| v.parse::<f64>().ok())
        .unwrap_or(DEFAULT_DAILY_COST_CAP_USD);

    let conn = match rusqlite::Connection::open(db_path) {
        Ok(c) => c,
        // If we can't open the DB at all, let the rest of the pipeline surface the real error.
        Err(_) => return Ok(()),
    };

    // Range predicate, NOT `date(created_at) = date('now')`. Wrapping the column in a
    // function makes every index unusable: the old form planned as a full SCAN of the
    // 314k-row api_usage table on EVERY pipeline start. This form uses idx_api_usage_date.
    // String comparison is correct here because created_at is 'YYYY-MM-DD HH:MM:SS', which
    // sorts identically to its date prefix.
    let spent: f64 = conn
        .query_row(
            "SELECT COALESCE(SUM(estimated_cost_usd), 0.0)
             FROM api_usage
             WHERE created_at >= date('now')
               AND created_at <  date('now', '+1 day')",
            [],
            |row| row.get(0),
        )
        .unwrap_or(0.0);

    match cap_verdict(spent, cap) {
        CapVerdict::Abort => anyhow::bail!(
            "Daily cost cap hit: ${:.4} spent today (cap: ${:.2}). \
             Aborting before more API calls are made. \
             Override with PULSE_DAILY_COST_CAP=<usd> env var.",
            spent,
            cap
        ),
        CapVerdict::Warn { threshold } => {
            tracing::warn!(
                "Daily spend so far: ${:.4} — above the ${:.2} expected-day threshold (cap ${:.2}, {:.0}% used)",
                spent, threshold, cap, spent / cap * 100.0
            );
            Ok(())
        }
        CapVerdict::Ok => Ok(()),
    }
}

/// How long per-call API usage rows are kept. The cost cap only ever reads today's rows
/// and the reporting views look back weeks, so 90 days is generous — the table had grown
/// to 329,719 rows since 2026-04-10 with no policy at all.
pub(crate) const API_USAGE_RETENTION_DAYS: i64 = 90;

/// Alert if overall embedding coverage sits below this. Deliberately set BELOW the
/// current 58.8% so it does not scream every single day while the scheduled backfill
/// drains the ~15.8k-story backlog — it is a floor that must not be crossed again, not a
/// target. Raise it toward 95% once the backlog is clear.
pub(crate) const EMBEDDING_COVERAGE_FLOOR_PCT: f64 = 55.0;

/// Alert if coverage falls this many percentage points in a single run. Catches a sudden
/// collapse (an outright Voyage outage) immediately, rather than waiting for the slow
/// average to sink past the floor — the decay that went unnoticed for five months.
pub(crate) const EMBEDDING_COVERAGE_DROP_PCT: f64 = 2.0;

/// A day's spend is "abnormal" above this even though it is nowhere near the cap.
/// ~2x a healthy day (measured $0.24-$0.37, Jul 25 - Aug 14 2026).
pub(crate) const ABNORMAL_DAY_USD: f64 = 0.75;

#[derive(Debug, PartialEq)]
pub(crate) enum CapVerdict {
    Ok,
    Warn { threshold: f64 },
    Abort,
}

/// Pure decision half of `check_daily_cost_cap`, split out so the thresholds are testable
/// without a DB or an env var.
///
/// Warns at whichever comes FIRST: half the cap, or `ABNORMAL_DAY_USD`. The second term is
/// what keeps the early warning alive now that the cap is a loose runaway guard — 50% of
/// $2.00 is 3x a healthy day, so a purely cap-relative warning would never fire and the
/// first sign of a loop would be the hard abort itself.
pub(crate) fn cap_verdict(spent: f64, cap: f64) -> CapVerdict {
    if spent >= cap {
        return CapVerdict::Abort;
    }
    let threshold = (cap * 0.5).min(ABNORMAL_DAY_USD);
    if spent > threshold {
        CapVerdict::Warn { threshold }
    } else {
        CapVerdict::Ok
    }
}

#[cfg(test)]
mod cost_cap_tests {
    use super::*;

    /// The regression this raise exists for: 2026-08-14 spent $0.5031 and the old $0.50
    /// cap aborted `run_freedoms`. Same spend must now pass.
    #[test]
    fn real_aug14_spend_no_longer_aborts() {
        assert_eq!(
            cap_verdict(0.5031, DEFAULT_DAILY_COST_CAP_USD),
            CapVerdict::Ok
        );
        assert_eq!(cap_verdict(0.5031, 0.50), CapVerdict::Abort, "old cap did abort");
    }

    /// A healthy day (measured $0.24-$0.37) must be silent — no warning noise.
    #[test]
    fn healthy_day_is_silent() {
        for spent in [0.24, 0.30, 0.37, 0.4591] {
            assert_eq!(
                cap_verdict(spent, DEFAULT_DAILY_COST_CAP_USD),
                CapVerdict::Ok,
                "${spent} should be unremarkable"
            );
        }
    }

    /// The whole point of ABNORMAL_DAY_USD: at a $2.00 cap, half-the-cap would be $1.00
    /// and nothing between a normal day and the hard abort would ever warn. Without the
    /// min(), this case returns Ok and the first sign of a loop is the abort itself.
    #[test]
    fn warns_well_before_the_abort_at_a_loose_cap() {
        assert_eq!(
            cap_verdict(0.90, DEFAULT_DAILY_COST_CAP_USD),
            CapVerdict::Warn { threshold: ABNORMAL_DAY_USD }
        );
        assert!(ABNORMAL_DAY_USD < DEFAULT_DAILY_COST_CAP_USD * 0.5);
    }

    /// With a tight override the cap-relative half still governs, so a small cap keeps
    /// its proportional early warning instead of jumping straight to abort.
    #[test]
    fn tight_cap_uses_the_proportional_threshold() {
        assert_eq!(cap_verdict(0.30, 0.50), CapVerdict::Warn { threshold: 0.25 });
        assert_eq!(cap_verdict(0.20, 0.50), CapVerdict::Ok);
    }

    /// Boundary: the abort is `>=`, and it wins over the warn.
    #[test]
    fn spend_exactly_at_cap_aborts() {
        assert_eq!(cap_verdict(2.00, 2.00), CapVerdict::Abort);
        assert_eq!(cap_verdict(1.9999, 2.00), CapVerdict::Warn { threshold: ABNORMAL_DAY_USD });
    }

    /// A runaway is still caught — that is what the cap is for.
    #[test]
    fn runaway_still_aborts() {
        assert_eq!(cap_verdict(5.0, DEFAULT_DAILY_COST_CAP_USD), CapVerdict::Abort);
    }
}
