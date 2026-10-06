//! Weekly signal scorecard (`--mode scorecard`, table `signal_scorecard`).
//!
//! Answers "which signals actually make money" from two angles:
//! - every buy-grade signal of the last `LOOKBACK_DAYS` days, bought or not,
//!   and how the stock did over the next `HORIZON` trading days against SPY
//!   (bought at the next open, as the auto-trader would), and
//! - the bot's own closed trades in the same groups.
//!
//! Groups are each signal dimension that fired and each score range. A ticker
//! signalling day after day is one episode: after a counted signal, the same
//! ticker is not counted again for `HORIZON` trading days, so a long streak
//! cannot outvote everything else.

use std::collections::HashMap;

use pulse_alpaca::DailyBar;
use rusqlite::Connection;

pub const LOOKBACK_DAYS: i64 = 120;
/// Trading days held, from the next open to the close `HORIZON` bars later.
pub const HORIZON: usize = 10;
/// A dimension counts as having fired at this normalized value.
pub const FIRED: f64 = 0.25;
/// Lowest compound score included; 0.20-0.30 shows the near misses.
pub const MIN_SCORE: f64 = 0.20;

pub const DIMENSIONS: [(&str, &str); 8] = [
    ("insider_signal", "insider"),
    ("news_momentum", "news"),
    ("government_signal", "government"),
    ("search_trend", "search"),
    ("institutional_flow", "institutional"),
    ("patent_signal", "patent"),
    ("supply_chain", "supply_chain"),
    ("political_signal", "political"),
];

#[derive(Debug, Clone)]
pub struct Sample {
    pub ticker: String,
    pub date: String,
    pub score: f64,
    /// Dimension keys (second column of `DIMENSIONS`) that fired.
    pub fired: Vec<&'static str>,
}

pub fn score_bucket(score: f64) -> &'static str {
    match score {
        s if s < 0.30 => "0.20-0.30 (not bought)",
        s if s < 0.35 => "0.30-0.35",
        s if s < 0.40 => "0.35-0.40",
        s if s < 0.50 => "0.40-0.50",
        _ => "0.50+",
    }
}

/// Count each ticker's signal once per `HORIZON` trading days. `samples` must
/// be sorted by (ticker, date); `trading_days` is SPY's calendar.
pub fn episodes(samples: Vec<Sample>, trading_days: &[String]) -> Vec<Sample> {
    let index = |d: &str| trading_days.partition_point(|t| t.as_str() <= d);
    let mut out: Vec<Sample> = Vec::new();
    let mut last: Option<(String, usize)> = None;
    for s in samples {
        let at = index(&s.date);
        if let Some((t, i)) = &last
            && *t == s.ticker
            && at < i + HORIZON
        {
            continue;
        }
        last = Some((s.ticker.clone(), at));
        out.push(s);
    }
    out
}

/// Return from the first open after `date` to the close `HORIZON` bars on,
/// as (entry date, exit date, return). None when the window isn't complete.
pub fn forward(bars: &[DailyBar], date: &str) -> Option<(String, String, f64)> {
    let i = bars.partition_point(|b| b.date.as_str() <= date);
    let entry = bars.get(i)?;
    let exit = bars.get(i + HORIZON - 1)?;
    (entry.open > 0.0).then(|| (entry.date.clone(), exit.date.clone(), exit.close / entry.open - 1.0))
}

/// SPY's return over the same entry/exit dates.
pub fn spy_return(spy: &[DailyBar], entry: &str, exit: &str) -> Option<f64> {
    let e = spy.iter().find(|b| b.date == entry)?;
    let x = spy.iter().find(|b| b.date == exit)?;
    (e.open > 0.0).then(|| x.close / e.open - 1.0)
}

#[derive(Debug, Default, Clone, PartialEq)]
pub struct Group {
    pub excess: Vec<f64>,
    pub trade_pnl: f64,
    pub trade_wins: usize,
    pub trades: usize,
}

impl Group {
    pub fn win_rate(&self) -> Option<f64> {
        (!self.excess.is_empty()).then(|| self.excess.iter().filter(|x| **x > 0.0).count() as f64 / self.excess.len() as f64)
    }
    pub fn mean(&self) -> Option<f64> {
        (!self.excess.is_empty()).then(|| self.excess.iter().sum::<f64>() / self.excess.len() as f64)
    }
    pub fn median(&self) -> Option<f64> {
        if self.excess.is_empty() {
            return None;
        }
        let mut v = self.excess.clone();
        v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let n = v.len();
        Some(if n % 2 == 1 { v[n / 2] } else { (v[n / 2 - 1] + v[n / 2]) / 2.0 })
    }
}

fn group_keys(score: f64, fired: &[&'static str]) -> Vec<(&'static str, String)> {
    let mut keys = vec![("score", score_bucket(score).to_string())];
    // Dimensions are judged on signals the bot would buy.
    if score >= 0.30 {
        keys.extend(fired.iter().map(|d| ("dimension", d.to_string())));
        keys.push(("dimension", "all buy-grade".to_string()));
    }
    keys
}

pub fn load_samples(conn: &Connection) -> rusqlite::Result<Vec<Sample>> {
    let cols = DIMENSIONS.iter().map(|(c, _)| format!("COALESCE({c}, 0)")).collect::<Vec<_>>().join(", ");
    let sql = format!(
        "SELECT ticker, date(computed_at), compound_score, {cols} FROM cross_signals
         WHERE convergence_detected = 1 AND ticker IS NOT NULL AND ticker != ''
           AND compound_score >= ?1 AND computed_at >= date('now', ?2)
         ORDER BY ticker, computed_at"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(rusqlite::params![MIN_SCORE, format!("-{LOOKBACK_DAYS} days")], |r| {
        let mut fired = Vec::new();
        for (i, (_, key)) in DIMENSIONS.iter().enumerate() {
            if r.get::<_, f64>(3 + i)? >= FIRED {
                fired.push(*key);
            }
        }
        Ok(Sample { ticker: r.get(0)?, date: r.get(1)?, score: r.get(2)?, fired })
    })?;
    rows.collect()
}

/// Closed trades as (score, fired dimensions, realized $).
pub fn load_trades(conn: &Connection) -> rusqlite::Result<Vec<(f64, Vec<&'static str>, f64)>> {
    let mut stmt = conn.prepare(
        "SELECT COALESCE(original_compound_score, confidence, 0), signal_profile, COALESCE(realized_pnl, pnl, 0)
         FROM paper_trades
         WHERE status != 'open' AND pnl_pct IS NOT NULL AND entry_date >= date('now', ?1)",
    )?;
    let rows = stmt.query_map([format!("-{LOOKBACK_DAYS} days")], |r| {
        let profile: Option<String> = r.get(1)?;
        let json: serde_json::Value = profile.as_deref().and_then(|p| serde_json::from_str(p).ok()).unwrap_or_default();
        // signal_profile keys: insider, news, government, search, institutional, patent, supply_chain, political
        let fired = DIMENSIONS
            .iter()
            .map(|(_, k)| *k)
            .filter(|k| json.get(*k).and_then(|v| v.as_f64()).unwrap_or(0.0) >= FIRED)
            .collect();
        Ok((r.get(0)?, fired, r.get(2)?))
    })?;
    rows.collect()
}

pub fn build(
    samples: &[Sample],
    bars: &HashMap<String, Vec<DailyBar>>,
    spy: &[DailyBar],
    trades: &[(f64, Vec<&'static str>, f64)],
) -> HashMap<(&'static str, String), Group> {
    let mut groups: HashMap<(&'static str, String), Group> = HashMap::new();
    for s in samples {
        let Some(b) = bars.get(&s.ticker) else { continue };
        let Some((entry, exit, ret)) = forward(b, &s.date) else { continue };
        let Some(spy_ret) = spy_return(spy, &entry, &exit) else { continue };
        for key in group_keys(s.score, &s.fired) {
            groups.entry(key).or_default().excess.push((ret - spy_ret) * 100.0);
        }
    }
    for (score, fired, pnl) in trades {
        for key in group_keys(*score, fired) {
            let g = groups.entry(key).or_default();
            g.trades += 1;
            g.trade_pnl += pnl;
            if *pnl > 0.0 {
                g.trade_wins += 1;
            }
        }
    }
    groups
}

pub fn store(conn: &Connection, today: &str, groups: &HashMap<(&'static str, String), Group>) -> rusqlite::Result<usize> {
    let tx = conn.unchecked_transaction()?;
    tx.execute("DELETE FROM signal_scorecard WHERE computed_at = ?1", [today])?;
    for ((kind, grp), g) in groups {
        tx.execute(
            "INSERT INTO signal_scorecard (computed_at, grp_kind, grp, signals, win_rate, avg_excess, median_excess, trades, trade_pnl, trade_win_rate)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            rusqlite::params![
                today, kind, grp, g.excess.len() as i64, g.win_rate(), g.mean(), g.median(),
                g.trades as i64, (g.trades > 0).then_some(g.trade_pnl),
                (g.trades > 0).then(|| g.trade_wins as f64 / g.trades as f64),
            ],
        )?;
    }
    tx.commit()?;
    Ok(groups.len())
}

/// `AAPL`, `BRK.B`: what Alpaca's multi-symbol bars endpoint accepts.
pub fn plain_ticker(t: &str) -> bool {
    let (base, class) = t.split_once('.').unwrap_or((t, ""));
    (1..=5).contains(&base.len())
        && base.bytes().all(|b| b.is_ascii_uppercase())
        && (class.is_empty() || (class.len() == 1 && class.bytes().all(|b| b.is_ascii_uppercase())))
}

/// Fetch, compute and store. Returns the number of groups written.
pub async fn run(db_path: &std::path::Path) -> anyhow::Result<usize> {
    let key = std::env::var("ALPACA_API_KEY").unwrap_or_default();
    let secret = std::env::var("ALPACA_SECRET_KEY").unwrap_or_default();
    if key.is_empty() || secret.is_empty() {
        anyhow::bail!("ALPACA_API_KEY / ALPACA_SECRET_KEY not set");
    }
    let creds = pulse_alpaca::Credentials { key, secret };
    let client = reqwest::Client::builder().timeout(std::time::Duration::from_secs(30)).build()?;
    let conn = Connection::open(db_path)?;
    crate::db::run_migrations(&conn)?;

    let samples = load_samples(&conn)?;
    let trades = load_trades(&conn)?;
    // Alpaca rejects the whole request over one odd symbol (ASB-PE, a
    // preferred share), so only plain tickers are asked for.
    let mut symbols: Vec<String> = samples.iter().map(|s| s.ticker.clone()).filter(|t| plain_ticker(t)).collect();
    symbols.sort();
    symbols.dedup();
    symbols.push("SPY".into());
    let start = (chrono::Local::now().date_naive() - chrono::Duration::days(LOOKBACK_DAYS + 10)).to_string();
    let mut bars = pulse_alpaca::daily_bars_multi(&client, &creds, &symbols, &start)
        .await
        .map_err(|e| anyhow::anyhow!(e))?;
    // Today's bar is still moving; score only finished sessions.
    let today = chrono::Local::now().format("%Y-%m-%d").to_string();
    for list in bars.values_mut() {
        list.retain(|b| b.date < today);
    }
    let spy = bars.remove("SPY").unwrap_or_default();
    if spy.is_empty() {
        anyhow::bail!("no SPY bars");
    }
    let calendar: Vec<String> = spy.iter().map(|b| b.date.clone()).collect();
    let counted = episodes(samples, &calendar);
    let groups = build(&counted, &bars, &spy, &trades);
    let n = store(&conn, &today, &groups)?;
    tracing::info!("Scorecard: {} signal episodes, {} closed trades, {} groups written", counted.len(), trades.len(), n);
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bar(date: &str, open: f64, close: f64) -> DailyBar {
        DailyBar { date: date.into(), open, high: close, low: close, close, volume: 1.0 }
    }

    fn days(n: usize) -> Vec<String> {
        (1..=n).map(|d| format!("2026-09-{d:02}")).collect()
    }

    fn sample(ticker: &str, date: &str, score: f64, fired: &[&'static str]) -> Sample {
        Sample { ticker: ticker.into(), date: date.into(), score, fired: fired.to_vec() }
    }

    #[test]
    fn a_signal_is_bought_at_the_next_open_and_held_ten_bars() {
        let bars: Vec<DailyBar> = days(15).iter().enumerate().map(|(i, d)| bar(d, 100.0 + i as f64, 100.0 + i as f64)).collect();
        // Signal on 09-02: entry 09-03 open (102), exit 09-12 close (111).
        let (entry, exit, ret) = forward(&bars, "2026-09-02").unwrap();
        assert_eq!((entry.as_str(), exit.as_str()), ("2026-09-03", "2026-09-12"));
        assert!((ret - (111.0 / 102.0 - 1.0)).abs() < 1e-12);
        assert!(forward(&bars, "2026-09-10").is_none(), "window not complete yet");
    }

    #[test]
    fn a_streak_counts_once_per_horizon() {
        let cal = days(30);
        let s = vec![
            sample("A", "2026-09-01", 0.4, &[]),
            sample("A", "2026-09-02", 0.4, &[]),
            sample("A", "2026-09-10", 0.4, &[]),
            sample("A", "2026-09-11", 0.4, &[]),
            sample("B", "2026-09-02", 0.4, &[]),
        ];
        let kept: Vec<(String, String)> = episodes(s, &cal).into_iter().map(|s| (s.ticker, s.date)).collect();
        assert_eq!(
            kept,
            vec![("A".into(), "2026-09-01".into()), ("A".into(), "2026-09-11".into()), ("B".into(), "2026-09-02".into())]
        );
    }

    #[test]
    fn groups_measure_excess_over_spy_and_trades_separately() {
        let cal = days(15);
        let up: Vec<DailyBar> = cal.iter().map(|d| bar(d, 100.0, 110.0)).collect();
        let spy: Vec<DailyBar> = cal.iter().map(|d| bar(d, 100.0, 102.0)).collect();
        let bars = HashMap::from([("A".to_string(), up.clone()), ("B".to_string(), spy.clone())]);
        let samples = vec![sample("A", "2026-09-01", 0.45, &["news"]), sample("B", "2026-09-01", 0.25, &["news"])];
        let trades = vec![(0.45, vec!["news"], 120.0), (0.32, vec!["insider"], -50.0)];
        let g = build(&samples, &bars, &spy, &trades);

        let news = &g[&("dimension", "news".to_string())];
        assert_eq!(news.excess.len(), 1, "the 0.25 signal is below buy grade: not in dimensions");
        assert!((news.excess[0] - 8.0).abs() < 1e-9, "10% vs SPY's 2%");
        assert_eq!((news.trades, news.trade_wins), (1, 1));
        let near_miss = &g[&("score", "0.20-0.30 (not bought)".to_string())];
        assert!(near_miss.excess[0].abs() < 1e-9);
        assert_eq!(g[&("dimension", "insider".to_string())].trade_pnl, -50.0);
        assert_eq!(g[&("dimension", "all buy-grade".to_string())].trades, 2);
    }

    #[test]
    fn only_plain_tickers_are_requested() {
        for t in ["AAPL", "BRK.B", "F"] {
            assert!(plain_ticker(t), "{t}");
        }
        for t in ["ASB-PE", "", "TOOLONG", "aapl", "BRK.BB", "X1"] {
            assert!(!plain_ticker(t), "{t}");
        }
    }

    #[test]
    fn stats_handle_empty_and_even_groups() {
        let g = Group { excess: vec![-1.0, 3.0], ..Default::default() };
        assert_eq!((g.win_rate(), g.mean(), g.median()), (Some(0.5), Some(1.0), Some(1.0)));
        assert_eq!(Group::default().median(), None);
    }
}
