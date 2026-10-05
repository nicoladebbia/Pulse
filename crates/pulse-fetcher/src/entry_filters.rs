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
//!
//! Pure rules are here with their tests; the fetches feeding them are below.

use std::collections::HashMap;

use chrono::{Datelike, NaiveDate, Weekday};
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
/// No buys this many trading days before an earnings report.
pub const EARNINGS_BLACKOUT_DAYS: u32 = 3;
/// Size multiplier while SPY is below its 50-day average.
pub const WEAK_MARKET_SIZE: f64 = 0.5;

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

/// `today` plus `n` weekdays (holidays are ignored — erring a day long).
pub fn add_trading_days(today: NaiveDate, n: u32) -> NaiveDate {
    let mut d = today;
    let mut left = n;
    while left > 0 {
        d = d.succ_opt().unwrap_or(d);
        if !matches!(d.weekday(), Weekday::Sat | Weekday::Sun) {
            left -= 1;
        }
    }
    d
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

/// Every US report date in the coming ~10 days, by symbol, from one call to
/// Finnhub's earnings calendar (one call per run instead of one per
/// candidate, which the free 60/minute limit would not survive).
pub async fn upcoming_earnings(
    client: &reqwest::Client,
    finnhub_key: &str,
    today: NaiveDate,
) -> Result<HashMap<String, Vec<NaiveDate>>, String> {
    if finnhub_key.is_empty() {
        return Err("no FINNHUB_API_KEY".into());
    }
    let to = today + chrono::Duration::days(10);
    let resp = client
        .get("https://finnhub.io/api/v1/calendar/earnings")
        .query(&[("from", today.to_string()), ("to", to.to_string()), ("token", finnhub_key.to_string())])
        .send()
        .await
        .map_err(|e| format!("earnings calendar request failed: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("earnings calendar returned {}", resp.status()));
    }
    let body: serde_json::Value = resp.json().await.map_err(|e| e.to_string())?;
    Ok(parse_earnings_dates(&body))
}

pub fn parse_earnings_dates(body: &serde_json::Value) -> HashMap<String, Vec<NaiveDate>> {
    let mut out: HashMap<String, Vec<NaiveDate>> = HashMap::new();
    for e in body.get("earningsCalendar").and_then(|v| v.as_array()).map(Vec::as_slice).unwrap_or_default() {
        let symbol = e.get("symbol").and_then(|v| v.as_str());
        let date = e.get("date").and_then(|v| v.as_str()).and_then(|d| d.parse::<NaiveDate>().ok());
        if let (Some(symbol), Some(date)) = (symbol, date) {
            out.entry(symbol.to_string()).or_default().push(date);
        }
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
        if let Some(ind) = industry(client, conn, finnhub_key, symbol).await {
            *out.entry(ind).or_default() += value;
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
            .map(|(i, c)| DailyBar { date: format!("2026-09-{:02}", i + 1), close: *c, volume })
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
    fn earnings_dates_parse_from_finnhub() {
        let body = serde_json::json!({"earningsCalendar": [
            {"symbol": "PEP", "date": "2026-10-08", "hour": "bmo"},
            {"symbol": "PEP", "date": "bad"}
        ]});
        let parsed = parse_earnings_dates(&body);
        assert_eq!(parsed.get("PEP"), Some(&vec![d("2026-10-08")]));
        assert_eq!(parsed.len(), 1);
        assert!(parse_earnings_dates(&serde_json::json!({})).is_empty());
    }
}
