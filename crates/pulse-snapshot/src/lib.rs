//! The read-only snapshot the iPhone companion shows: today's briefing, the
//! account and its positions, risk, the next earnings and macro dates, the
//! strongest signals and the latest closed trades.
//!
//! The fetcher writes it as one JSON file into iCloud Drive after each run; the
//! phone only reads it. The file is the contract between the two apps: field
//! names are snake_case, and `SCHEMA_VERSION` goes up only for a breaking
//! change (a removed or retyped field), which the phone shows as "update the
//! app". `tests/fixtures/snapshot_v1.json` is decoded by both sides' tests.

pub mod explain;
pub mod ledger;
pub mod live;
pub mod risk;

use chrono::{DateTime, NaiveDate, Utc};
use pulse_calendar::view::{CalendarData, SIGNAL_SCORE, build_calendar};
use risk::{HeldPosition, RiskInputs, RiskReport};
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

pub const SCHEMA_VERSION: u32 = 1;
pub const FILE_NAME: &str = "pulse-snapshot.json";
/// Days of earnings shown on the phone.
const EARNINGS_DAYS: i64 = 14;
/// Days of macro dates shown on the phone.
const CALENDAR_DAYS: i64 = 21;
const MAX_STORIES: usize = 12;
const MAX_SIGNALS: usize = 10;
const MAX_TRADES: usize = 10;
const MAX_DRAWDOWN_POINTS: usize = 90;
const SUMMARY_CHARS: usize = 700;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Snapshot {
    pub version: u32,
    /// RFC 3339, UTC.
    pub generated_at: String,
    /// None when Alpaca could not be reached for this run.
    pub account: Option<Account>,
    /// When `account`, `positions` and `risk` were read from Alpaca, if that was
    /// an earlier run (the broker was unreachable for this one). None when they
    /// are current.
    pub account_as_of: Option<String>,
    pub positions: Vec<Position>,
    /// Positions are listed separately above, so `risk.positions` is empty.
    pub risk: Option<RiskReport>,
    pub briefing: Option<Briefing>,
    pub signals: Vec<SignalItem>,
    pub calendar: CalendarData,
    pub recent_trades: Vec<ClosedTrade>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Account {
    pub equity: f64,
    pub cash: f64,
    /// Against the previous close's equity.
    pub day_change: Option<f64>,
    pub day_change_pct: Option<f64>,
    /// Sum of unrealized P&L over the open positions.
    pub open_pl: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Position {
    pub symbol: String,
    /// "long" or "short"
    pub side: String,
    pub qty: f64,
    pub price: f64,
    /// Negative for a short.
    pub market_value: f64,
    pub unrealized_pl: f64,
    pub unrealized_pl_pct: Option<f64>,
    pub stop_price: Option<f64>,
    pub industry: Option<String>,
    /// Why Pulse bought it, in plain words. Absent for a position Pulse has no trade for.
    #[serde(default)]
    pub explanation: Option<explain::Explanation>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Briefing {
    /// `YYYY-MM-DD`
    pub date: String,
    pub time_label: Option<String>,
    pub executive_summary: Option<String>,
    pub stories: Vec<Story>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Story {
    pub id: i64,
    pub sector: String,
    pub headline: String,
    pub summary: String,
    pub why_it_matters: Option<String>,
    pub source_name: Option<String>,
    pub url: Option<String>,
    pub is_hero: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SignalItem {
    pub ticker: String,
    pub name: Option<String>,
    pub score: f64,
    pub convergence: bool,
    pub held: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClosedTrade {
    pub ticker: String,
    pub direction: String,
    pub entry_date: String,
    pub exit_date: String,
    pub pnl: Option<f64>,
    pub pnl_pct: Option<f64>,
    #[serde(default)]
    pub explanation: Option<explain::Explanation>,
}

/// What Alpaca reports right now, fetched by the caller.
pub struct LiveAccount {
    pub equity: f64,
    pub cash: f64,
    /// Equity at the previous close.
    pub last_equity: Option<f64>,
    pub positions: Vec<HeldPosition>,
    /// Daily bars by symbol including SPY, for beta.
    pub bars: HashMap<String, Vec<pulse_alpaca::DailyBar>>,
    /// Problems met while reading the account, shown with the risk warnings.
    pub notes: Vec<String>,
}

fn clip(s: &str, max: usize) -> String {
    let s = s.trim();
    if s.chars().count() <= max {
        return s.to_string();
    }
    let cut: String = s.chars().take(max).collect();
    format!("{}…", cut.trim_end())
}

fn non_empty(s: Option<String>) -> Option<String> {
    s.map(|v| v.trim().to_string()).filter(|v| !v.is_empty())
}

pub fn load_briefing(conn: &Connection) -> rusqlite::Result<Option<Briefing>> {
    // The newest complete daily briefing that has stories: a half-finished
    // retry must not blank the phone.
    let head: Option<(i64, String, Option<String>, Option<String>)> = conn
        .query_row(
            "SELECT b.id, b.date, b.time_label, b.executive_summary FROM briefings b
             WHERE b.briefing_type = 'daily' AND b.status = 'complete'
               AND EXISTS (SELECT 1 FROM stories s WHERE s.briefing_id = b.id AND s.source_type = 'news')
             ORDER BY b.date DESC, b.created_at DESC LIMIT 1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .map(Some)
        .or_else(|e| if matches!(e, rusqlite::Error::QueryReturnedNoRows) { Ok(None) } else { Err(e) })?;
    let Some((id, date, time_label, exec)) = head else { return Ok(None) };

    let mut stmt = conn.prepare(
        "SELECT id, sector, COALESCE(NULLIF(headline, ''), original_title), COALESCE(summary, ''),
                why_it_matters, source_name, original_url, is_hero
         FROM stories WHERE briefing_id = ?1 AND source_type = 'news'
         ORDER BY is_hero DESC, importance_score DESC, display_order ASC LIMIT ?2",
    )?;
    let rows = stmt.query_map(params![id, MAX_STORIES as i64], |r| {
        Ok(Story {
            id: r.get(0)?,
            sector: r.get(1)?,
            headline: r.get::<_, Option<String>>(2)?.unwrap_or_default(),
            summary: clip(&r.get::<_, String>(3)?, SUMMARY_CHARS),
            why_it_matters: non_empty(r.get(4)?).map(|s| clip(&s, SUMMARY_CHARS)),
            source_name: non_empty(r.get(5)?),
            url: non_empty(r.get(6)?),
            is_hero: r.get::<_, Option<i64>>(7)?.unwrap_or(0) != 0,
        })
    })?;
    let stories = rows.collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(Some(Briefing { date, time_label: non_empty(time_label), executive_summary: non_empty(exec), stories }))
}

pub fn load_signals(conn: &Connection, held: &HashSet<String>) -> rusqlite::Result<Vec<SignalItem>> {
    let mut stmt = conn.prepare(
        "SELECT UPPER(COALESCE(et.ticker, cs.ticker)), MIN(e.name), MAX(cs.compound_score), MAX(cs.convergence_detected)
         FROM cross_signals cs
         LEFT JOIN entity_tickers et ON et.entity_id = cs.entity_id
         LEFT JOIN entities e ON e.id = cs.entity_id
         WHERE date(cs.computed_at) = (SELECT MAX(date(computed_at)) FROM cross_signals)
           AND COALESCE(et.ticker, cs.ticker) IS NOT NULL
         GROUP BY UPPER(COALESCE(et.ticker, cs.ticker))
         ORDER BY MAX(cs.compound_score) DESC LIMIT ?1",
    )?;
    let rows = stmt.query_map(params![MAX_SIGNALS as i64], |r| {
        let ticker: String = r.get(0)?;
        Ok(SignalItem {
            held: held.contains(&ticker),
            ticker,
            name: r.get(1)?,
            score: r.get::<_, Option<f64>>(2)?.filter(|s| s.is_finite()).unwrap_or(0.0),
            convergence: r.get::<_, Option<i64>>(3)?.unwrap_or(0) != 0,
        })
    })?;
    rows.collect()
}

pub fn load_recent_trades(conn: &Connection) -> rusqlite::Result<Vec<ClosedTrade>> {
    Ok(explain::load_closed(conn, MAX_TRADES)?
        .into_iter()
        .filter_map(|f| {
            let finite = |v: Option<f64>| v.filter(|v| v.is_finite());
            Some(ClosedTrade {
                explanation: Some(explain::explain(&f)),
                pnl: conn.query_row("SELECT pnl FROM paper_trades WHERE id = ?1", [f.id], |r| r.get::<_, Option<f64>>(0)).ok().flatten().filter(|v| v.is_finite()),
                pnl_pct: finite(f.pnl_pct),
                ticker: f.ticker,
                direction: f.direction,
                entry_date: f.entry_date,
                exit_date: f.exit_date?,
            })
        })
        .collect())
}

fn fin(v: &mut f64) {
    if !v.is_finite() {
        *v = 0.0;
    }
}

fn fin_opt(v: &mut Option<f64>) {
    if v.is_some_and(|x| !x.is_finite()) {
        *v = None;
    }
}

impl Snapshot {
    /// serde_json writes NaN and infinity as `null`, which a plain number on the
    /// phone cannot decode and would blank the whole file. Required numbers
    /// become 0 and optional ones become absent.
    fn sanitize(&mut self) {
        if let Some(a) = &mut self.account {
            fin(&mut a.equity);
            fin(&mut a.cash);
            fin(&mut a.open_pl);
            fin_opt(&mut a.day_change);
            fin_opt(&mut a.day_change_pct);
        }
        for p in &mut self.positions {
            fin(&mut p.qty);
            fin(&mut p.price);
            fin(&mut p.market_value);
            fin(&mut p.unrealized_pl);
            fin_opt(&mut p.unrealized_pl_pct);
            fin_opt(&mut p.stop_price);
        }
        if let Some(r) = &mut self.risk {
            for v in [
                &mut r.equity,
                &mut r.cash,
                &mut r.long_value,
                &mut r.short_value,
                &mut r.gross_pct,
                &mut r.net_pct,
                &mut r.cash_pct,
                &mut r.largest_position_pct,
                &mut r.stop_loss_dollars,
                &mut r.stop_loss_pct,
                &mut r.beta_coverage_pct,
                &mut r.drawdown.current_pct,
                &mut r.drawdown.max_pct,
                &mut r.drawdown.peak_equity,
            ] {
                fin(v);
            }
            fin_opt(&mut r.beta_exposure_pct);
            for i in &mut r.industries {
                for v in [&mut i.long_value, &mut i.short_value, &mut i.net_value, &mut i.gross_pct] {
                    fin(v);
                }
            }
            for s in &mut r.stress {
                for v in [&mut s.spy_move_pct, &mut s.pnl_dollars, &mut s.pnl_pct] {
                    fin(v);
                }
            }
            for pt in &mut r.drawdown.points {
                fin(&mut pt.equity);
                fin(&mut pt.drawdown_pct);
            }
        }
        for s in &mut self.signals {
            fin(&mut s.score);
        }
        for e in &mut self.calendar.earnings {
            fin_opt(&mut e.eps_estimate);
            fin_opt(&mut e.revenue_estimate);
            fin_opt(&mut e.signal_score);
        }
        for t in &mut self.recent_trades {
            fin_opt(&mut t.pnl);
            fin_opt(&mut t.pnl_pct);
        }
    }
}

fn position_pct(side_short: bool, market_value: f64, pl: f64) -> Option<f64> {
    let cost = if side_short { market_value.abs() + pl } else { market_value - pl };
    (cost > 0.0 && cost.is_finite()).then(|| pl / cost * 100.0)
}

/// A previous copy's account figures older than this are not carried forward.
const CARRY_MAX_HOURS: i64 = 72;

/// Build the snapshot. Without `live` (Alpaca unreachable) the account, positions
/// and risk are left out and everything from the local database still shows.
pub fn build(conn: &Connection, today: NaiveDate, now: DateTime<Utc>, live: Option<&LiveAccount>) -> Result<Snapshot, String> {
    build_from(conn, today, now, live, None)
}

/// Like `build`, but when `live` is missing the account, positions and risk of
/// `previous` (the file already on disk) are kept, marked with `account_as_of`,
/// so one broker hiccup does not blank the phone's portfolio.
pub fn build_from(
    conn: &Connection,
    today: NaiveDate,
    now: DateTime<Utc>,
    live: Option<&LiveAccount>,
    previous: Option<&Snapshot>,
) -> Result<Snapshot, String> {
    let err = |e: rusqlite::Error| e.to_string();
    let mut account_as_of = None;
    let stops = risk::load_stops(conn).map_err(err)?;
    let industries = risk::load_industries(conn).map_err(err)?;

    let mut open_trades = HashMap::new();
    for t in explain::load_open(conn).map_err(err)? {
        open_trades.entry(t.ticker.to_uppercase()).or_insert(t);
    }

    let (account, positions, risk_report) = match live {
        None => match previous.and_then(|p| carried_over(p, now)) {
            Some((as_of, account, positions, risk)) => {
                account_as_of = Some(as_of);
                (Some(account), positions, risk)
            }
            None => (None, Vec::new(), None),
        },
        Some(l) => {
            let positions: Vec<Position> = l
                .positions
                .iter()
                .map(|p| {
                    let short = p.qty < 0.0 || p.market_value < 0.0;
                    let pct = position_pct(short, p.market_value, p.unrealized_pl);
                    let explanation = Some(open_trades.get(&p.symbol.to_uppercase()).map_or_else(explain::not_a_pulse_trade, |t| {
                        let mut t = t.clone();
                        t.current_pct = pct;
                        explain::explain(&t)
                    }));
                    Position {
                        symbol: p.symbol.clone(),
                        side: if short { "short" } else { "long" }.to_string(),
                        qty: p.qty,
                        price: p.price,
                        market_value: p.market_value,
                        unrealized_pl: p.unrealized_pl,
                        unrealized_pl_pct: pct,
                        stop_price: stops.get(&p.symbol).copied(),
                        industry: industries.get(&p.symbol).cloned(),
                        explanation,
                    }
                })
                .collect();
            let day_change = l.last_equity.filter(|e| *e > 0.0 && e.is_finite()).map(|e| l.equity - e);
            let account = Account {
                equity: l.equity,
                cash: l.cash,
                day_change,
                day_change_pct: day_change.zip(l.last_equity).map(|(c, e)| c / e * 100.0),
                open_pl: l.positions.iter().map(|p| p.unrealized_pl).sum(),
            };
            let history = risk::load_equity_history(conn).map_err(err)?;
            let history = risk::with_live_equity(&history, &today.to_string(), l.equity);
            let mut report = risk::build_report(&RiskInputs {
                equity: l.equity,
                cash: l.cash,
                positions: &l.positions,
                stops: &stops,
                industries: &industries,
                bars: &l.bars,
                equity_history: &history,
            });
            report.positions.clear();
            report.warnings.extend(l.notes.iter().cloned());
            let n = report.drawdown.points.len();
            if n > MAX_DRAWDOWN_POINTS {
                report.drawdown.points.drain(..n - MAX_DRAWDOWN_POINTS);
            }
            (Some(account), positions, Some(report))
        }
    };

    let held: HashSet<String> = positions.iter().map(|p| p.symbol.to_uppercase()).collect();
    let mut calendar = build_calendar(conn, today, CALENDAR_DAYS)?;
    // The phone shows only what matters to this account: held names and strong signals.
    calendar.earnings.retain(|e| e.days_until <= EARNINGS_DAYS && (e.held || e.signal_score.is_some_and(|s| s >= SIGNAL_SCORE)));

    let mut snapshot = Snapshot {
        version: SCHEMA_VERSION,
        generated_at: now.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        account,
        account_as_of,
        positions,
        risk: risk_report,
        briefing: load_briefing(conn).map_err(err)?,
        signals: load_signals(conn, &held).map_err(err)?,
        calendar,
        recent_trades: load_recent_trades(conn).map_err(err)?,
    };
    snapshot.sanitize();
    Ok(snapshot)
}

/// The account figures of an earlier copy, with when they were read. None if
/// it has none, is from another schema, or is too old to still mean anything.
fn carried_over(previous: &Snapshot, now: DateTime<Utc>) -> Option<(String, Account, Vec<Position>, Option<RiskReport>)> {
    if previous.version != SCHEMA_VERSION {
        return None;
    }
    let account = previous.account.clone()?;
    let as_of = previous.account_as_of.clone().unwrap_or_else(|| previous.generated_at.clone());
    let read_at = DateTime::parse_from_rfc3339(&as_of).ok()?.with_timezone(&Utc);
    (now - read_at <= chrono::Duration::hours(CARRY_MAX_HOURS)).then(|| (as_of, account, previous.positions.clone(), previous.risk.clone()))
}

/// The snapshot already on disk, if there is a readable one.
pub fn read_previous(dir: &Path) -> Option<Snapshot> {
    serde_json::from_slice(&std::fs::read(dir.join(FILE_NAME)).ok()?).ok()
}

/// Where the snapshot goes: `PULSE_SNAPSHOT_DIR`, else a `Pulse` folder in iCloud
/// Drive. None when `PULSE_SNAPSHOT=off`, or there is no home directory.
pub fn output_dir() -> Option<PathBuf> {
    resolve_dir(
        std::env::var("PULSE_SNAPSHOT").ok().as_deref(),
        std::env::var_os("PULSE_SNAPSHOT_DIR"),
        std::env::var_os("HOME"),
    )
}

fn resolve_dir(switch: Option<&str>, dir: Option<std::ffi::OsString>, home: Option<std::ffi::OsString>) -> Option<PathBuf> {
    if switch.is_some_and(|v| matches!(v.trim().to_lowercase().as_str(), "off" | "false" | "0")) {
        return None;
    }
    if let Some(dir) = dir.filter(|d| !d.is_empty()) {
        return Some(PathBuf::from(dir));
    }
    let home = home.filter(|h| !h.is_empty())?;
    Some(PathBuf::from(home).join("Library/Mobile Documents/com~apple~CloudDocs/Pulse"))
}

/// Write the snapshot so a reader never sees half a file: a temp file in the
/// same folder, then a rename.
pub fn write_atomic(dir: &Path, snapshot: &Snapshot) -> Result<PathBuf, String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    let json = serde_json::to_vec(snapshot).map_err(|e| e.to_string())?;
    let target = dir.join(FILE_NAME);
    // A name of its own per write: two runs can overlap (the hook runs after the
    // fetcher lock is released), and a shared temp file would let one truncate
    // what the other is about to rename.
    static WRITES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = WRITES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
    let tmp = dir.join(format!(".{FILE_NAME}.{}.{nanos}.{n}.tmp", std::process::id()));
    let written = std::fs::File::create(&tmp).and_then(|mut f| {
        use std::io::Write;
        f.write_all(&json)?;
        f.sync_all()
    });
    if let Err(e) = written {
        let _ = std::fs::remove_file(&tmp);
        return Err(format!("cannot write {}: {e}", tmp.display()));
    }
    if let Err(e) = std::fs::rename(&tmp, &target) {
        let _ = std::fs::remove_file(&tmp);
        return Err(format!("cannot replace {}: {e}", target.display()));
    }
    Ok(target)
}

/// Seconds since the snapshot file was last written; None when it does not exist.
pub fn age_seconds(dir: &Path) -> Option<u64> {
    std::fs::metadata(dir.join(FILE_NAME)).ok()?.modified().ok()?.elapsed().ok().map(|d| d.as_secs())
}

#[cfg(test)]
mod tests;
