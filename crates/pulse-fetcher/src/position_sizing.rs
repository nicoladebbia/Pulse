//! Position sizing for auto-trades.
//!
//! Single source of truth for converting a compound signal score into a
//! dollar notional. Used by both the entry path (`auto_trade_on_convergence`)
//! and the scale-in path so that the two cannot drift apart.

const ENTRY_FLOOR: f64 = 50.0;
const ENTRY_CAP: f64 = 20_000.0;
const SCALE_IN_PCT: f64 = 0.01;
const SCALE_IN_CAP: f64 = 5_000.0;

/// The largest fraction of the portfolio any tier will allocate to one entry.
pub const TOP_TIER_PCT: f64 = 0.10;

/// Hard ceiling on total exposure to a single ticker, as a fraction of
/// portfolio value.
///
/// Held equal to `TOP_TIER_PCT` **by construction**, because the cap exists to
/// bound ACCUMULATION — repeated signals or duplicate fills stacking one name,
/// as META did when 5 duplicate fills put 23% of equity into one position — and
/// not to veto a single deliberate top-conviction entry.
///
/// That invariant held by coincidence from 2026-05-01, when the cap was added
/// at 0.05 while the top tier was also 0.05, until 2026-07-23, when the tiers
/// were deliberately doubled to 2/5/10% and this constant was not revisited.
/// For those weeks the top tier could not trade at all: it sized 10% and the
/// cap rejected anything over 5%, so the highest-conviction signals were the
/// only ones guaranteed to be skipped. Deriving the cap from the tier is what
/// makes that drift impossible to reintroduce — `the_cap_can_never_sit_below_
/// the_top_tier` fails if someone raises one without the other.
pub const MAX_PER_TICKER_PCT: f64 = TOP_TIER_PCT;

// Checked by the COMPILER, not merely by `cargo test`: a cap below the top tier
// vetoes the tier it exists to bound, which is exactly what shipped between
// 2026-07-23 and its fix. As a #[test] assertion this only failed once someone
// ran the suite; as a const assertion the build refuses to produce a binary.
const _: () = assert!(
    MAX_PER_TICKER_PCT >= TOP_TIER_PCT,
    "a cap below the top tier vetoes the tier it is supposed to bound"
);

/// Tier the compound score into a portfolio-percentage allocation.
/// Long-term design (2026-07-23): doubled from 1%/2%/5% — fewer, bigger,
/// longer-held positions fit a conviction-based long-term approach better
/// than many small short-term-sized trades.
fn tier_pct(score: f64) -> f64 {
    if score > 0.6 {
        TOP_TIER_PCT
    } else if score > 0.4 {
        0.05
    } else {
        0.02
    }
}

/// Notional dollars to allocate for a new entry.
///
/// `tier_pct` is documented as a *portfolio*-percentage allocation, so
/// `portfolio_value` is the denominator. It used to be handed `buying_power`,
/// which on a margin account is a multiple of portfolio value — so a 10% tier
/// became 20% of the actual portfolio, and then collided with a cap measured
/// against the honest denominator. Buying power still bounds the result: it is
/// what can actually be spent, not what the position should be worth.
///
/// Falls back to `buying_power` as the denominator only when Alpaca reports no
/// portfolio value, matching the concentration check's own fallback.
///
/// Returns `None` when nothing above the entry floor can be afforded — the
/// caller should skip the trade rather than place an undersized order.
pub fn entry_notional(portfolio_value: f64, buying_power: f64, compound_score: f64) -> Option<f64> {
    if buying_power < ENTRY_FLOOR {
        return None;
    }
    let denominator = if portfolio_value > 0.0 {
        portfolio_value
    } else {
        buying_power
    };
    let sized = (denominator * tier_pct(compound_score)).clamp(ENTRY_FLOOR, ENTRY_CAP);
    let affordable = sized.min(buying_power);
    (affordable >= ENTRY_FLOOR).then_some(affordable)
}

/// Dollars that may still be committed to `ticker` before the concentration cap.
///
/// `f64::INFINITY` when Alpaca reports no portfolio value — the same fallback
/// the entry path has always used, where `ENTRY_CAP` remains the only bound.
pub fn ticker_headroom(portfolio_value: f64, existing_exposure: f64) -> f64 {
    if portfolio_value <= 0.0 {
        return f64::INFINITY;
    }
    (portfolio_value * MAX_PER_TICKER_PCT - existing_exposure).max(0.0)
}

/// Trim `proposed` to whatever concentration headroom is left.
///
/// Clamping rather than skipping is the point. The previous behaviour dropped
/// the whole order when it did not fit, which meant a name already carrying 9%
/// of the portfolio contributed nothing on a fresh signal even though 1% of
/// room was available — and, because the tier collided with the cap, a
/// top-conviction entry into an *empty* name was dropped as well.
///
/// `None` when the remaining room is below `ENTRY_FLOOR`: an order that small
/// is not worth the commission and the round-trip, so skipping is correct
/// there. That is the one case where the old skip was the right answer.
pub fn clamp_to_ticker_cap(
    portfolio_value: f64,
    existing_exposure: f64,
    proposed: f64,
) -> Option<f64> {
    let room = ticker_headroom(portfolio_value, existing_exposure);
    let trimmed = proposed.min(room);
    (trimmed >= ENTRY_FLOOR && trimmed.is_finite()).then_some(trimmed)
}

/// Notional dollars for a scale-in into an already-open winning position.
/// Conservative by design — fixed 1% of buying power, lower cap.
pub fn scale_in_notional(buying_power: f64) -> Option<f64> {
    if buying_power < ENTRY_FLOOR {
        return None;
    }
    let raw = buying_power * SCALE_IN_PCT;
    Some(raw.clamp(ENTRY_FLOOR, SCALE_IN_CAP))
}

/// Share-weighted average cost after adding `add_notional` at `add_price` to a
/// position of `held_notional` opened at `held_entry`.
///
/// Scale-in only fires on a position that is already up, so the added shares
/// always cost more than the original ones and the true basis always rises.
/// Leaving `entry_price` at the first fill is not a rounding error:
/// `pnl_pct` is measured off it, `pnl` multiplies that percentage by the
/// grown `position_size`, and the -15% hard stop and 3x-ATR profit target are
/// both struck from it. An understated basis therefore overstates the reported
/// gain and pushes the stop further away than the money at risk allows.
///
/// Returns `None` when either leg's price is unusable — the caller must then
/// leave the recorded basis alone rather than write a wrong one.
pub fn blended_entry_price(
    held_notional: f64,
    held_entry: f64,
    add_notional: f64,
    add_price: f64,
) -> Option<f64> {
    if held_notional <= 0.0 || held_entry <= 0.0 || add_notional <= 0.0 || add_price <= 0.0 {
        return None;
    }
    let shares = held_notional / held_entry + add_notional / add_price;
    if shares <= 0.0 || !shares.is_finite() {
        return None;
    }
    let basis = (held_notional + add_notional) / shares;
    basis.is_finite().then_some(basis)
}

/// `SIZING_MODEL=risk` switches entries and scale-ins from the score tiers to
/// `pulse_weights::risk_sizing`. Anything else, or unset, keeps the tiers.
pub fn risk_sizing_enabled() -> bool {
    std::env::var("SIZING_MODEL")
        .map(|v| v.trim().eq_ignore_ascii_case("risk"))
        .unwrap_or(false)
}

/// Closed trades the edge gate reads: the most recent `limit` with a return.
///
/// Returns are per-trade `pnl_pct`, which is price-based and so unaffected by
/// the dollar-P&L errors the ledger carried before the 2026-09 repair.
pub fn load_edge_stats(conn: &rusqlite::Connection, limit: usize) -> pulse_weights::risk_sizing::EdgeStats {
    let returns: Vec<f64> = conn
        .prepare(
            "SELECT pnl_pct FROM paper_trades
             WHERE status IN ('closed', 'stopped_out') AND pnl_pct IS NOT NULL
             ORDER BY exit_date DESC LIMIT ?1",
        )
        .and_then(|mut stmt| {
            stmt.query_map([limit as i64], |row| row.get::<_, f64>(0))
                .map(|rows| rows.filter_map(|r| r.ok()).filter(|r| r.is_finite()).collect())
        })
        .unwrap_or_default();
    pulse_weights::risk_sizing::EdgeStats::from_returns(&returns)
}

/// Fraction the account sits below its high-water mark. The mark is the
/// highest snapshot or today's value, whichever is larger; no snapshots
/// means no drawdown.
pub fn current_drawdown(conn: &rusqlite::Connection, portfolio_value: f64) -> f64 {
    let hwm: f64 = conn
        .query_row("SELECT MAX(high_water_mark) FROM portfolio_snapshots", [], |row| {
            row.get::<_, Option<f64>>(0)
        })
        .ok()
        .flatten()
        .unwrap_or(0.0)
        .max(portfolio_value);
    if hwm > 0.0 && portfolio_value > 0.0 {
        ((hwm - portfolio_value) / hwm).max(0.0)
    } else {
        0.0
    }
}

/// Latest close and 14-day ATR for sizing. With no close on file the ATR is
/// meaningless, so the stop falls back to the fixed percentage: (1.0, 0.0)
/// makes `stop_distance_pct` use `fallback_stop_pct`, which is independent of
/// price.
pub fn price_and_atr(conn: &rusqlite::Connection, ticker: &str) -> (f64, f64) {
    let close: f64 = conn
        .query_row(
            "SELECT close FROM entity_prices WHERE ticker = ?1 ORDER BY date DESC LIMIT 1",
            [ticker],
            |row| row.get(0),
        )
        .unwrap_or(0.0);
    if close > 0.0 {
        (close, crate::position_management::compute_atr(conn, ticker, 14))
    } else {
        (1.0, 0.0)
    }
}

/// Sizing state for one auto-trade run. Each placed order is committed back
/// so the next candidate in the same run sees the heat and cash it used.
pub struct RiskBook {
    pub params: pulse_weights::risk_sizing::RiskParams,
    pub book: pulse_weights::risk_sizing::Book,
    pub edge: pulse_weights::risk_sizing::EdgeStats,
}

impl RiskBook {
    pub fn size(
        &self,
        conn: &rusqlite::Connection,
        ticker: &str,
        existing_exposure: f64,
        unit: f64,
    ) -> Option<pulse_weights::risk_sizing::Sized> {
        let (price, atr) = price_and_atr(conn, ticker);
        pulse_weights::risk_sizing::size_order(
            &self.book, price, atr, existing_exposure, &self.edge, unit, &self.params,
        )
    }

    pub fn commit(&mut self, sized: &pulse_weights::risk_sizing::Sized) {
        self.book.open_risk += sized.risk;
        self.book.buying_power = (self.book.buying_power - sized.notional).max(0.0);
    }
}

#[cfg(test)]
mod basis_tests {
    use super::*;

    #[test]
    fn adding_higher_priced_shares_raises_the_basis() {
        // $5,000 at $100 = 50 shares. Add $2,500 at $110 = 22.727 shares.
        // $7,500 over 72.727 shares = $103.125.
        let basis = blended_entry_price(5_000.0, 100.0, 2_500.0, 110.0).unwrap();
        assert!((basis - 103.125).abs() < 0.001, "got {basis}");
    }

    #[test]
    fn the_understated_basis_is_worth_real_money() {
        // Same position, marked at $110. Reporting the original $100 entry
        // claims +10% on $7,500 = $750. The truth is +6.67% = $500.
        let basis = blended_entry_price(5_000.0, 100.0, 2_500.0, 110.0).unwrap();
        let honest_pct = (110.0 - basis) / basis * 100.0;
        let stale_pct = (110.0 - 100.0) / 100.0 * 100.0;
        assert!((honest_pct - 6.6667).abs() < 0.01, "got {honest_pct}");
        assert!(stale_pct - honest_pct > 3.0, "the gap is the overstatement");

        // And the -15% hard stop moves up with it: $85.00 off the stale entry,
        // $87.66 off the real one — 2.66 dollars of unintended risk per share.
        assert!((basis * 0.85 - 87.656).abs() < 0.01);
    }

    #[test]
    fn adding_at_the_same_price_leaves_the_basis_alone() {
        let basis = blended_entry_price(5_000.0, 100.0, 2_500.0, 100.0).unwrap();
        assert!((basis - 100.0).abs() < 0.001, "got {basis}");
    }

    #[test]
    fn an_unusable_price_yields_no_basis_rather_than_a_wrong_one() {
        assert!(blended_entry_price(5_000.0, 100.0, 2_500.0, 0.0).is_none());
        assert!(blended_entry_price(5_000.0, 0.0, 2_500.0, 110.0).is_none());
        assert!(blended_entry_price(0.0, 100.0, 2_500.0, 110.0).is_none());
        assert!(blended_entry_price(5_000.0, 100.0, 0.0, 110.0).is_none());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entry_high_score_uses_10pct() {
        let n = entry_notional(100_000.0, 100_000.0, 0.7).unwrap();
        assert!((n - 10_000.0).abs() < 0.01);
    }

    #[test]
    fn entry_mid_score_uses_5pct() {
        let n = entry_notional(100_000.0, 100_000.0, 0.5).unwrap();
        assert!((n - 5_000.0).abs() < 0.01);
    }

    #[test]
    fn entry_low_score_uses_2pct() {
        let n = entry_notional(100_000.0, 100_000.0, 0.35).unwrap();
        assert!((n - 2_000.0).abs() < 0.01);
    }

    #[test]
    fn entry_respects_cap() {
        // 10% of $1M = $100K, must clamp to $20K
        let n = entry_notional(1_000_000.0, 1_000_000.0, 0.7).unwrap();
        assert!((n - 20_000.0).abs() < 0.01);
    }

    #[test]
    fn entry_respects_floor() {
        // 2% of $200 = $4, must clamp up to $50
        let n = entry_notional(200.0, 200.0, 0.3).unwrap();
        assert!((n - 50.0).abs() < 0.01);
    }

    #[test]
    fn entry_skipped_when_underfunded() {
        assert!(entry_notional(49.99, 49.99, 0.9).is_none());
    }

    #[test]
    fn the_cap_can_never_sit_below_the_top_tier() {
        // The whole defect in one assertion. Between 2026-07-23 and this commit
        // the top tier was 0.10 and the cap 0.05, so the highest-conviction
        // signals were the only ones the concentration check could never admit.
        for score in [0.0, 0.3, 0.41, 0.5, 0.61, 0.9, 1.0] {
            assert!(
                tier_pct(score) <= MAX_PER_TICKER_PCT,
                "score {score} sizes {} against a cap of {MAX_PER_TICKER_PCT}",
                tier_pct(score)
            );
        }
    }

    #[test]
    fn a_top_tier_score_sizes_strictly_larger_than_a_mid_tier_score() {
        // Post-cap, not pre-cap. Sizing the tiers apart is worthless if the cap
        // then flattens them: with the cap at 0.05 both of these clamp to
        // $5,000 and the 0.10 branch is dead code that reads as if it works.
        let pv = 100_000.0;
        let top = clamp_to_ticker_cap(pv, 0.0, entry_notional(pv, pv, 0.7).unwrap()).unwrap();
        let mid = clamp_to_ticker_cap(pv, 0.0, entry_notional(pv, pv, 0.5).unwrap()).unwrap();
        assert!(
            top > mid,
            "top tier sized {top} and mid tier {mid} — the tiers have collapsed"
        );
    }

    #[test]
    fn a_top_conviction_entry_into_an_empty_name_is_not_blocked() {
        // The reported symptom: score > 0.6 on a ticker held at zero.
        let pv = 100_000.0;
        let n = entry_notional(pv, pv, 0.7).unwrap();
        assert_eq!(clamp_to_ticker_cap(pv, 0.0, n), Some(10_000.0));
    }

    #[test]
    fn margin_buying_power_no_longer_inflates_the_position() {
        // Alpaca reports 2x buying power on a margin account. Sizing off it made
        // a "10% of portfolio" tier into 20% of the portfolio, which then could
        // not clear a cap measured against portfolio value at any setting.
        let n = entry_notional(100_000.0, 200_000.0, 0.7).unwrap();
        assert!((n - 10_000.0).abs() < 0.01, "sized {n}, expected 10% of equity");
    }

    #[test]
    fn buying_power_still_bounds_the_order() {
        // Equity says $10k is the right size; there is $3k of cash to do it with.
        let n = entry_notional(100_000.0, 3_000.0, 0.7).unwrap();
        assert!((n - 3_000.0).abs() < 0.01, "sized {n}");
    }

    #[test]
    fn no_portfolio_value_falls_back_to_buying_power() {
        // Alpaca occasionally reports no portfolio_value. Sizing must not become
        // zero — ENTRY_CAP stays the only bound, as it was before the cap existed.
        let n = entry_notional(0.0, 100_000.0, 0.7).unwrap();
        assert!((n - 10_000.0).abs() < 0.01, "sized {n}");
        assert_eq!(ticker_headroom(0.0, 50_000.0), f64::INFINITY);
    }

    #[test]
    fn a_name_already_at_the_cap_gets_nothing_more() {
        // The META case: the position is at 10% and a fresh signal arrives.
        assert_eq!(clamp_to_ticker_cap(100_000.0, 10_000.0, 5_000.0), None);
        assert_eq!(clamp_to_ticker_cap(100_000.0, 25_000.0, 5_000.0), None);
    }

    #[test]
    fn a_partly_filled_name_is_topped_up_to_the_cap_not_skipped() {
        // 9% held, a $5,000 signal arrives, $1,000 of room. The old code threw
        // the whole order away; the right answer is the $1,000.
        let got = clamp_to_ticker_cap(100_000.0, 9_000.0, 5_000.0).unwrap();
        assert!((got - 1_000.0).abs() < 0.01, "clamped to {got}");
    }

    #[test]
    fn a_sliver_of_headroom_is_skipped_rather_than_traded() {
        // $30 of room is not worth a round trip — below ENTRY_FLOOR, so skip.
        assert_eq!(clamp_to_ticker_cap(100_000.0, 9_970.0, 5_000.0), None);
    }

    #[test]
    fn a_scale_in_cannot_push_a_name_past_the_cap() {
        // The scale-in path applied no concentration check at all: the cap was a
        // function-local const in the entry path. A position sitting at the cap
        // could still be topped up by 1% of buying power, which on margin is a
        // larger number than the cap it was bypassing.
        let pv = 100_000.0;
        let add = scale_in_notional(200_000.0).unwrap();
        assert!((add - 2_000.0).abs() < 0.01, "scale-in sized {add}");
        assert_eq!(clamp_to_ticker_cap(pv, 10_000.0, add), None);
        let room = clamp_to_ticker_cap(pv, 8_500.0, add).unwrap();
        assert!((room - 1_500.0).abs() < 0.01, "clamped to {room}");
    }

    #[test]
    fn scale_in_caps_at_5k() {
        let n = scale_in_notional(1_000_000.0).unwrap();
        assert!((n - 5_000.0).abs() < 0.01);
    }

    #[test]
    fn scale_in_floored_at_50() {
        let n = scale_in_notional(500.0).unwrap();
        assert!((n - 50.0).abs() < 0.01);
    }
}

#[cfg(test)]
mod risk_book_tests {
    use super::*;
    use rusqlite::Connection;

    fn db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::run_migrations(&conn).unwrap();
        conn
    }

    fn trade(conn: &Connection, status: &str, pnl_pct: Option<f64>, exit_date: &str) {
        conn.execute(
            "INSERT INTO paper_trades (ticker, direction, entry_price, entry_date, position_size,
                 confidence, signal_profile, status, pnl_pct, exit_date)
             VALUES ('AAA', 'long', 10.0, '2026-09-01', 1000.0, 0.4, '{}', ?1, ?2, ?3)",
            rusqlite::params![status, pnl_pct, exit_date],
        )
        .unwrap();
    }

    #[test]
    fn edge_stats_read_only_closed_trades_with_a_return() {
        let conn = db();
        trade(&conn, "closed", Some(5.0), "2026-09-10");
        trade(&conn, "stopped_out", Some(-3.0), "2026-09-11");
        trade(&conn, "open", Some(9.0), "");
        trade(&conn, "expired", None, "2026-09-12");
        trade(&conn, "closed", None, "2026-09-13");
        let s = load_edge_stats(&conn, 100);
        assert_eq!((s.trades, s.wins), (2, 1));
    }

    #[test]
    fn edge_stats_keep_the_most_recent_trades() {
        let conn = db();
        trade(&conn, "closed", Some(-1.0), "2026-08-01");
        trade(&conn, "closed", Some(4.0), "2026-09-01");
        let s = load_edge_stats(&conn, 1);
        assert_eq!((s.trades, s.wins), (1, 1));
    }

    #[test]
    fn drawdown_is_measured_from_the_highest_snapshot() {
        let conn = db();
        assert_eq!(current_drawdown(&conn, 95_000.0), 0.0, "no snapshots, no drawdown");
        conn.execute(
            "INSERT INTO portfolio_snapshots (date, total_value, total_pnl, total_pnl_pct,
                 open_positions, high_water_mark, drawdown_pct)
             VALUES ('2026-09-28', 95000, -5000, -5, 17, 103065.75, 7.8)",
            [],
        )
        .unwrap();
        assert!((current_drawdown(&conn, 94_317.27) - (103_065.75 - 94_317.27) / 103_065.75).abs() < 1e-9);
        assert_eq!(current_drawdown(&conn, 110_000.0), 0.0, "a new high is no drawdown");
    }

    #[test]
    fn a_ticker_with_no_prices_sizes_to_the_fixed_stop() {
        let conn = db();
        let (price, atr) = price_and_atr(&conn, "NOPE");
        let p = pulse_weights::risk_sizing::RiskParams::default();
        assert_eq!(
            pulse_weights::risk_sizing::stop_distance_pct(price, atr, &p),
            p.fallback_stop_pct
        );
    }

    #[test]
    fn committing_an_order_uses_up_heat_and_cash() {
        let conn = db();
        let mut rb = RiskBook {
            params: pulse_weights::risk_sizing::RiskParams::default(),
            book: pulse_weights::risk_sizing::Book {
                equity: 100_000.0, buying_power: 20_000.0, drawdown: 0.0, open_risk: 9_700.0,
            },
            edge: Default::default(),
        };
        // No prices on file: 10% fallback stop. $300 of heat left -> $3,000.
        let s = rb.size(&conn, "NOPE", 0.0, 1.0).unwrap();
        assert!((s.notional - 3_000.0).abs() < 1e-6);
        rb.commit(&s);
        assert!((rb.book.open_risk - 10_000.0).abs() < 1e-6);
        assert!((rb.book.buying_power - 17_000.0).abs() < 1e-6);
        assert!(rb.size(&conn, "NOPE", 0.0, 1.0).is_none(), "heat is spent");
    }
}
