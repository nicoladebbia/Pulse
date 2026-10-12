//! Extra checks a candidate must pass before a new buy (Step 2 of the trading
//! plan, 2026-10-05). Each one answers a failure seen in the first 37 closed
//! trades (29.7% win rate, profit factor 0.30):
//!
//! - **Liquidity and price.** $1 stocks and thin names gap through stops; the
//!   price floor is $5 and the 20-day average dollar volume must be real.
//! - **Earnings.** A report is a coin flip the signal knows nothing about, so
//!   no buys in the 3 trading days before one.
//! - **Sector cap.** Several "different" signals in one industry are one bet.
//! - **Market regime.** With SPY under its 50-day average, buys are half size.
//! - **Book size.** At most `MAX_OPEN_POSITIONS` open at once.
//! - **Volatility floor** (Step 3, 2026-10-05). The signal only paid on stocks
//!   that move: from April to October, convergence buys in names with a 14-day
//!   ATR of at least 4% of price beat SPY by +4.5% / +8.7% over 20 days (first
//!   and second half), while calmer names lagged by -4.9% / -0.8%. Ordinary
//!   stocks showed no such gap (both about -2% / +1.7%), so this is the
//!   signal's edge, not just higher beta. The floor sits at 3% to keep most of
//!   the trades; below it every threshold tested lost money in both halves.
//!
//! Pure rules are here with their tests; the fetches feeding them are below.

use std::collections::HashMap;

use chrono::NaiveDate;
use pulse_alpaca::DailyBar;
use rusqlite::Connection;

/// Most open positions at once. Heat (summed risk to the stops) usually binds
/// first; this bounds the book when stops are tight.
pub const MAX_OPEN_POSITIONS: i64 = 40;
/// Lowest share price bought.
pub const MIN_PRICE: f64 = 5.0;
/// Lowest 20-day average dollar volume (consolidated tape).
pub const MIN_DOLLAR_VOLUME: f64 = 10_000_000.0;
/// Largest share of equity in one industry, counting the new buy.
pub const MAX_SECTOR_PCT: f64 = 0.30;
pub use pulse_calendar::{EARNINGS_BLACKOUT_DAYS, add_trading_days};
/// Size multiplier while SPY is below its 50-day average.
pub const WEAK_MARKET_SIZE: f64 = 0.5;
/// Lowest 14-day ATR as a share of price.
pub const MIN_ATR_PCT: f64 = 0.03;
/// Days in the ATR.
pub const ATR_DAYS: usize = 14;

/// Average close x volume over the last `days` bars. None when there are too
/// few bars to judge (a recent listing), which callers treat as a fail.
pub fn avg_dollar_volume(bars: &[DailyBar], days: usize) -> Option<f64> {
    if days == 0 || bars.len() < days.min(10) {
        return None;
    }
    let recent = &bars[bars.len().saturating_sub(days)..];
    Some(recent.iter().map(|b| b.close * b.volume).sum::<f64>() / recent.len() as f64)
}

/// Simple moving average of the last `n` closes.
pub fn sma(bars: &[DailyBar], n: usize) -> Option<f64> {
    if n == 0 || bars.len() < n {
        return None;
    }
    Some(bars[bars.len() - n..].iter().map(|b| b.close).sum::<f64>() / n as f64)
}

/// 1.0, or `WEAK_MARKET_SIZE` when the last SPY close is below its 50-day
/// average. Unknown (no data) counts as normal: this only trims size.
pub fn regime_multiplier(spy: &[DailyBar]) -> f64 {
    match (spy.last(), sma(spy, 50)) {
        (Some(last), Some(avg)) if last.close < avg => WEAK_MARKET_SIZE,
        _ => 1.0,
    }
}

/// Average true range over the last `n` bars as a share of the last close.
/// None with fewer than `n + 1` bars (the first range needs a prior close).
pub fn atr_pct(bars: &[DailyBar], n: usize) -> Option<f64> {
    if n == 0 || bars.len() < n + 1 {
        return None;
    }
    let recent = &bars[bars.len() - n - 1..];
    let atr = recent
        .windows(2)
        .map(|w| {
            let (prev, b) = (w[0].close, &w[1]);
            (b.high - b.low).max((b.high - prev).abs()).max((b.low - prev).abs())
        })
        .sum::<f64>()
        / n as f64;
    let last = recent.last()?.close;
    (last > 0.0).then(|| atr / last)
}

/// Whether any report date falls from today through the blackout window.
pub fn in_earnings_blackout(report_dates: &[NaiveDate], today: NaiveDate) -> bool {
    let last = add_trading_days(today, EARNINGS_BLACKOUT_DAYS);
    report_dates.iter().any(|d| *d >= today && *d <= last)
}

/// Whether a buy of `notional` fits under the sector cap.
pub fn sector_has_room(held_in_sector: f64, notional: f64, equity: f64) -> bool {
    equity > 0.0 && held_in_sector + notional <= equity * MAX_SECTOR_PCT
}

// ---------------------------------------------------------------------------
// Fetches
// ---------------------------------------------------------------------------

/// Bars for the last ~`calendar_days`, oldest first.
pub async fn recent_bars(
    client: &reqwest::Client,
    creds: &pulse_alpaca::Credentials,
    symbol: &str,
    calendar_days: i64,
) -> Result<Vec<DailyBar>, String> {
    let start = (chrono::Local::now().date_naive() - chrono::Duration::days(calendar_days)).to_string();
    pulse_alpaca::daily_bars(client, creds, symbol, &start).await
}

/// Every US report date in the coming ~10 days, by symbol. Finnhub cuts a
/// response at 1500 rows and drops the earliest days, which in a busy week are
/// the ones the blackout needs, so `calendar::collect` splits full windows.
pub async fn upcoming_earnings(
    client: &reqwest::Client,
    finnhub_key: &str,
    today: NaiveDate,
) -> Result<HashMap<String, Vec<NaiveDate>>, String> {
    if finnhub_key.is_empty() {
        return Err("no FINNHUB_API_KEY".into());
    }
    let to = today + chrono::Duration::days(10);
    let got = crate::calendar::collect(today, to, crate::calendar::ROW_CAP, |a, b| {
        crate::calendar::fetch_window(client, finnhub_key, a, b)
    })
    .await;
    if let Some(e) = got.error {
        // Windows run earliest first, so what arrived covers the days nearest
        // to today, which is what the blackout needs most. Better than none.
        if got.rows.is_empty() {
            return Err(format!("earnings calendar request failed: {e}"));
        }
        tracing::warn!("Earnings calendar incomplete ({e}); using the days fetched before it failed");
    }
    if !got.full_days.is_empty() {
        tracing::warn!("Earnings calendar: {} day(s) came back full, some reports may be missing", got.full_days.len());
    }
    Ok(group_dates(got.rows))
}

fn group_dates(rows: Vec<crate::calendar::EarningsRow>) -> HashMap<String, Vec<NaiveDate>> {
    let mut out: HashMap<String, Vec<NaiveDate>> = HashMap::new();
    for r in rows {
        out.entry(r.ticker).or_default().push(r.date);
    }
    out
}

/// A ticker's industry: cached 30 days in `ticker_industry`, else Finnhub's
/// profile. None when unknown — the sector cap then cannot group it.
pub async fn industry(client: &reqwest::Client, conn: &Connection, finnhub_key: &str, ticker: &str) -> Option<String> {
    if let Ok(cached) = conn.query_row(
        "SELECT industry FROM ticker_industry WHERE ticker = ?1 AND checked_at >= datetime('now', '-30 days')",
        [ticker],
        |r| r.get::<_, Option<String>>(0),
    ) {
        return cached;
    }
    if finnhub_key.is_empty() {
        return None;
    }
    let resp = client
        .get("https://finnhub.io/api/v1/stock/profile2")
        .query(&[("symbol", ticker), ("token", finnhub_key)])
        .send()
        .await
        .ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let body: serde_json::Value = resp.json().await.ok()?;
    let found = body
        .get("finnhubIndustry")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    conn.execute(
        "INSERT INTO ticker_industry (ticker, industry, checked_at) VALUES (?1, ?2, datetime('now'))
         ON CONFLICT(ticker) DO UPDATE SET industry = excluded.industry, checked_at = excluded.checked_at",
        rusqlite::params![ticker, found],
    )
    .ok();
    found
}

/// Dollars held per industry, from Alpaca's positions. Unknown industries are
/// left out.
pub async fn exposure_by_industry(
    client: &reqwest::Client,
    creds: &pulse_alpaca::Credentials,
    conn: &Connection,
    finnhub_key: &str,
) -> Result<HashMap<String, f64>, String> {
    let resp = creds
        .auth(client.get(format!("{}/positions", pulse_alpaca::PAPER_URL)))
        .send()
        .await
        .map_err(|e| format!("positions request failed: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("positions returned {}", resp.status()));
    }
    let positions: serde_json::Value = resp.json().await.map_err(|e| e.to_string())?;
    let mut out: HashMap<String, f64> = HashMap::new();
    for p in positions.as_array().map(Vec::as_slice).unwrap_or_default() {
        let Some(symbol) = p.get("symbol").and_then(|v| v.as_str()) else { continue };
        let value = p
            .get("market_value")
            .and_then(|v| v.as_str())
            .and_then(|s| s.parse::<f64>().ok())
            .unwrap_or(0.0)
            .abs();
        match industry(client, conn, finnhub_key, symbol).await {
            Some(ind) => *out.entry(ind).or_default() += value,
            None if !finnhub_key.is_empty() => {
                tracing::warn!("Sector cap: industry of held {} unknown — its ${:.0} is not counted", symbol, value)
            }
            None => {}
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bars(closes: &[f64], volume: f64) -> Vec<DailyBar> {
        closes
            .iter()
            .enumerate()
            .map(|(i, c)| DailyBar { date: format!("2026-09-{:02}", i + 1), open: *c, high: *c, low: *c, close: *c, volume })
            .collect()
    }

    fn d(s: &str) -> NaiveDate {
        s.parse().unwrap()
    }

    #[test]
    fn dollar_volume_averages_the_recent_bars_only() {
        let mut b = bars(&[10.0; 30], 1_000_000.0);
        b[0].volume = 1e12; // outside the 20-day window
        assert_eq!(avg_dollar_volume(&b, 20), Some(10_000_000.0));
        assert_eq!(avg_dollar_volume(&bars(&[10.0; 5], 1.0), 20), None, "too new to judge");
    }

    #[test]
    fn a_market_below_its_50_day_average_halves_size() {
        let mut up: Vec<f64> = (0..60).map(|i| 100.0 + i as f64).collect();
        assert_eq!(regime_multiplier(&bars(&up, 1.0)), 1.0);
        *up.last_mut().unwrap() = 100.0;
        assert_eq!(regime_multiplier(&bars(&up, 1.0)), WEAK_MARKET_SIZE);
        assert_eq!(regime_multiplier(&bars(&[1.0; 10], 1.0)), 1.0, "too little data: no change");
    }

    #[test]
    fn atr_is_the_average_true_range_over_price() {
        // Every day ranges 98..102 around a flat 100 close: ATR 4, i.e. 4%.
        let mut b = bars(&[100.0; 20], 1.0);
        for bar in &mut b {
            bar.high = 102.0;
            bar.low = 98.0;
        }
        let pct = atr_pct(&b, ATR_DAYS).unwrap();
        assert!((pct - 0.04).abs() < 1e-9, "{pct}");
        // A gap counts from the prior close, not just the day's own range.
        let last = b.len() - 1;
        b[last] = DailyBar { date: "2026-09-30".into(), open: 110.0, high: 111.0, low: 110.0, close: 110.0, volume: 1.0 };
        let gapped = atr_pct(&b, ATR_DAYS).unwrap();
        assert!((gapped - (13.0 * 4.0 + 11.0) / 14.0 / 110.0).abs() < 1e-9, "{gapped}");
        assert_eq!(atr_pct(&b[..14], ATR_DAYS), None, "needs a prior close");
        assert_eq!(atr_pct(&bars(&[100.0; 20], 1.0), ATR_DAYS), Some(0.0), "a flat line has no range");
    }

    #[test]
    fn trading_days_skip_weekends() {
        // Thursday + 3 trading days = Tuesday.
        assert_eq!(add_trading_days(d("2026-10-08"), 3), d("2026-10-13"));
        assert_eq!(add_trading_days(d("2026-10-05"), 3), d("2026-10-08"));
    }

    #[test]
    fn earnings_inside_three_trading_days_block_the_buy() {
        let today = d("2026-10-08"); // Thursday
        assert!(in_earnings_blackout(&[d("2026-10-08")], today), "reports today");
        assert!(in_earnings_blackout(&[d("2026-10-13")], today), "Tuesday is 3 trading days out");
        assert!(!in_earnings_blackout(&[d("2026-10-14")], today));
        assert!(!in_earnings_blackout(&[d("2026-10-07")], today), "already reported");
        assert!(!in_earnings_blackout(&[], today));
    }

    #[test]
    fn the_sector_cap_counts_the_new_buy() {
        assert!(sector_has_room(25_000.0, 5_000.0, 100_000.0));
        assert!(!sector_has_room(25_000.0, 5_001.0, 100_000.0));
        assert!(!sector_has_room(0.0, 1.0, 0.0));
    }

    #[test]
    fn report_dates_group_by_symbol() {
        let row = |t: &str, day: &str| crate::calendar::EarningsRow { ticker: t.into(), date: d(day), hour: None, eps_estimate: None, revenue_estimate: None };
        let grouped = group_dates(vec![row("PEP", "2026-10-08"), row("PEP", "2026-10-09"), row("KO", "2026-10-08")]);
        assert_eq!(grouped.get("PEP"), Some(&vec![d("2026-10-08"), d("2026-10-09")]));
        assert_eq!(grouped.len(), 2);
        assert!(group_dates(vec![]).is_empty());
    }
}
