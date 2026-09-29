//! What Pulse's trading system IS, for the models that judge papers against it.
//!
//! Generated from `StrategyParams::live()` (parity-tested against the live code)
//! and live DB counts, never hand-copied — `scripts/replay_engine.py` kept its
//! own WEIGHTS copy and drifted. Only the dimension descriptions are prose.

use pulse_weights::{StrategyParams, DIMENSIONS};

fn describe(dim: &str) -> &'static str {
    match dim {
        "insider_signal" => "net dollar value of SEC Form 4 insider purchases/sales for the company over the lookback window",
        "institutional_flow" => "count of distinct 13F filers holding the stock (ZEROED: substring-match bug inflates it)",
        "news_momentum" => "acceleration of news mentions (7-day vs 30-day rate); needs >= 3 mentions in 7 days",
        "government_signal" => "0.6 x federal contract value (USASpending) + 0.4 x regulatory/8-K event severity",
        "search_trend" => "change in Wikipedia pageviews for the company's article",
        "patent_signal" => "patent grant rate (ZEROED: no entity has ever scored non-zero)",
        "supply_chain" => "import-volume delta (ZEROED: market-wide constant, no per-company signal)",
        "political_signal" => "lobbying spend delta from Senate LDA (ZEROED: source returns 403 since 2026-08-03)",
        _ => "",
    }
}

/// Live evidence available for testing — the model must know how thin it is.
pub struct HistoryStats {
    pub signal_days: i64,
    pub first_day: String,
    pub last_day: String,
    pub converged_rows: i64,
    pub closed_trades: i64,
}

pub fn history_stats(conn: &rusqlite::Connection) -> HistoryStats {
    let (signal_days, first_day, last_day, converged_rows) = conn
        .query_row(
            "SELECT COUNT(DISTINCT date(computed_at)), COALESCE(MIN(date(computed_at)), ''),
                    COALESCE(MAX(date(computed_at)), ''), COALESCE(SUM(convergence_detected), 0)
             FROM cross_signals",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .unwrap_or((0, String::new(), String::new(), 0));
    let closed_trades = conn
        // 'stopped_out' is a closed trade too (6 of 64 on 2026-09-25) — counting
        // only 'closed' understated the sample.
        .query_row("SELECT COUNT(*) FROM paper_trades WHERE status IN ('closed', 'stopped_out')", [], |r| r.get(0))
        .unwrap_or(0);
    HistoryStats { signal_days, first_day, last_day, converged_rows, closed_trades }
}

pub fn render(p: &StrategyParams, h: &HistoryStats) -> String {
    let mut dims = String::new();
    for (i, d) in DIMENSIONS.iter().enumerate() {
        dims.push_str(&format!("  - {d} (weight {:.4}): {}\n", p.weights[i], describe(d)));
    }
    let tiers = p
        .sizing_tiers
        .iter()
        .map(|t| {
            if t.above_score.is_finite() {
                format!("{}% of equity if score > {}", t.pct, t.above_score)
            } else {
                format!("{}% otherwise", t.pct)
            }
        })
        .collect::<Vec<_>>()
        .join("; ");

    format!(
        "PULSE TRADING SYSTEM (generated from the live code)

What it is: a long-only, daily-cadence, PAPER-traded US-equity strategy (Alpaca paper
account, universe: market cap >= $300M, price >= $1). It is NOT a price-based strategy:
it never uses returns, momentum, volatility, order flow or any market microstructure as a
signal. It scores companies from free public ALTERNATIVE data once a day and buys the
companies where several independent sources move together (\"convergence\").

Score: each dimension's raw value x is normalised n = 1 - exp(-x / scale), then
compound = clamp(sum(weight_i * n_i), 0, 1). Dimensions and live weights:
{dims}
Convergence: at least {mp} dimensions with n > {pt} AND weight > 0, from at least {md}
distinct sources — OR compound >= {co}.
Entry: converged AND compound > {ms}; top 5 new names per day; veto if insiders net-sold
> $1M. Sizing: {tiers}.
Exits (live): hard stop -15%, trailing stop 3 x ATR(14) below the high-water mark, sell half
at +3 x ATR once up >= 10%, close when the score decays below max(30% of entry score, 0.05).
No calendar expiry.

Data it has: SEC Form 4 and 8-K, 13F, USASpending contracts, Federal Register, Wikipedia
pageviews, Google News/RSS/HN mention counts, patents, FRED macro, daily OHLC bars
(Alpaca IEX / Finnhub) for ~1,300 tickers. It has NO intraday data, NO order book, NO
options, NO fundamentals database, NO paid data.

Evidence available to test a change: {days} days of scored history ({first} to {last}),
{conv} converged signal rows, {trades} closed paper trades. A 2026 walk-forward review
recorded NO-GO: 34% out-of-sample hit rate. Small samples: most changes will be
statistically inconclusive on this history.

What the what-if backtester can test directly (a 'param' proposal): the 8 weights;
the vote threshold ({pt}); minimum votes ({mp}); minimum source diversity ({md}); the
compound override ({co}); the entry min score ({ms}); max open positions ({mx}); the
sizing tiers; and a FIXED-% exit proxy (stop {sl}%, take-profit +{tp}%, max hold {mh} days)
— it cannot simulate the ATR trail or the half-exit. Scales and the government blend are
NOT testable yet (stored values are already normalised). Anything else — a new signal, new
data, a different model — is an implementation spec for a human.",
        dims = dims,
        mp = p.min_positive,
        pt = p.positive_threshold,
        md = p.min_diversity,
        co = p.compound_override,
        ms = p.min_score,
        tiers = tiers,
        days = h.signal_days,
        first = h.first_day,
        last = h.last_day,
        conv = h.converged_rows,
        trades = h.closed_trades,
        mx = p.max_positions,
        sl = p.stop_loss_pct,
        tp = p.take_profit_pct,
        mh = p.max_hold_days,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn card_carries_live_numbers() {
        let p = StrategyParams::live();
        let h = HistoryStats {
            signal_days: 145,
            first_day: "2026-04-14".into(),
            last_day: "2026-09-24".into(),
            converged_rows: 2990,
            closed_trades: 58,
        };
        let c = render(&p, &h);
        assert!(c.contains("insider_signal (weight 0.3411)"));
        assert!(c.contains("political_signal (weight 0.0000)"));
        assert!(c.contains("OR compound >= 0.4"));
        assert!(c.contains("10% of equity if score > 0.6"));
        assert!(c.contains("145 days of scored history (2026-04-14 to 2026-09-24)"));
        assert!(c.contains("58 closed paper trades"));
        for d in DIMENSIONS {
            assert!(!describe(d).is_empty(), "{d} has no description");
        }
    }
}
