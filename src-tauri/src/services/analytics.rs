use rusqlite::Connection;
use serde::Serialize;

/// Parse a stored trade date that may be either `YYYY-MM-DD` or a full timestamp
/// `YYYY-MM-DDTHH:MM:SS`. Historical rows are mixed (SEV3): `parse_from_str(_, "%Y-%m-%d")`
/// silently rejected the timestamped ones, making hold-times read "0 days". Take the leading
/// 10 chars (the date) before parsing so both shapes work.
fn parse_trade_date(s: &str) -> Option<chrono::NaiveDate> {
    let date_part = s.get(..10).unwrap_or(s);
    chrono::NaiveDate::parse_from_str(date_part, "%Y-%m-%d").ok()
}

// ---------------------------------------------------------------------------
// Analytics types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct PortfolioAnalytics {
    // Core metrics
    pub total_trades: usize,
    pub open_count: usize,
    pub win_rate: f64,
    pub loss_rate: f64,
    pub avg_win_pct: f64,
    pub avg_loss_pct: f64,
    pub profit_factor: f64,
    pub total_return_pct: f64,
    pub total_return_dollars: f64,

    // Risk metrics
    pub sharpe_ratio: f64,
    pub sortino_ratio: f64,
    pub max_drawdown_pct: f64,
    pub avg_holding_days: f64,

    // Best / worst
    pub best_trade: Option<TradeSummary>,
    pub worst_trade: Option<TradeSummary>,

    // Breakdowns
    pub signal_attribution: Vec<SignalAttribution>,
    pub sector_exposure: Vec<SectorExposure>,
    pub monthly_returns: Vec<MonthlyReturn>,
    pub equity_curve: Vec<EquityPoint>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TradeSummary {
    pub ticker: String,
    pub pnl_pct: f64,
    pub pnl_dollars: f64,
    pub entry_date: String,
    pub exit_date: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct SignalAttribution {
    pub dimension: String,
    pub avg_on_wins: f64,
    pub avg_on_losses: f64,
    pub predictive_edge: f64, // avg_on_wins - avg_on_losses
    pub sample_count: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct SectorExposure {
    pub sector: String,
    pub trade_count: usize,
    pub total_pnl: f64,
    pub win_rate: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct MonthlyReturn {
    pub month: String, // "2026-04"
    pub pnl_dollars: f64,
    pub pnl_pct: f64,
    pub trade_count: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct EquityPoint {
    pub date: String,
    pub value: f64,
}

// ---------------------------------------------------------------------------
// Internal types
// ---------------------------------------------------------------------------

#[allow(dead_code)]
struct ClosedTrade {
    ticker: String,
    entry_date: String,
    exit_date: String,
    entry_price: f64,
    exit_price: f64,
    position_size: f64,
    pnl_pct: f64,
    pnl_dollars: f64,
    signal_profile: String,
    sector: Option<String>,
}

const SIGNAL_DIMENSIONS: &[&str] = &[
    "insider", "institutional", "news", "government",
    "search", "patent", "supply_chain", "political",
];

// ---------------------------------------------------------------------------
// Main computation
// ---------------------------------------------------------------------------

pub fn compute_analytics(conn: &Connection) -> Result<PortfolioAnalytics, String> {
    let closed = get_closed_trades(conn)?;
    let open_count = get_open_count(conn)?;

    if closed.is_empty() {
        return Ok(empty_analytics(open_count));
    }

    // Core metrics
    let total = closed.len();
    let wins: Vec<&ClosedTrade> = closed.iter().filter(|t| t.pnl_pct > 0.0).collect();
    let losses: Vec<&ClosedTrade> = closed.iter().filter(|t| t.pnl_pct <= 0.0).collect();

    let win_rate = wins.len() as f64 / total as f64 * 100.0;
    let loss_rate = losses.len() as f64 / total as f64 * 100.0;

    let avg_win_pct = if wins.is_empty() { 0.0 }
        else { wins.iter().map(|t| t.pnl_pct).sum::<f64>() / wins.len() as f64 };
    let avg_loss_pct = if losses.is_empty() { 0.0 }
        else { losses.iter().map(|t| t.pnl_pct).sum::<f64>() / losses.len() as f64 };

    let total_win_dollars: f64 = wins.iter().map(|t| t.pnl_dollars).sum();
    let total_loss_dollars: f64 = losses.iter().map(|t| t.pnl_dollars.abs()).sum();
    let profit_factor = if total_loss_dollars > 0.0 { total_win_dollars / total_loss_dollars } else { f64::INFINITY };

    let total_return_dollars: f64 = closed.iter().map(|t| t.pnl_dollars).sum();
    let total_invested: f64 = closed.iter().map(|t| t.position_size).sum();
    let total_return_pct = if total_invested > 0.0 { total_return_dollars / total_invested * 100.0 } else { 0.0 };

    // Holding days
    let holding_days: Vec<f64> = closed.iter().filter_map(|t| {
        let entry = parse_trade_date(&t.entry_date)?;
        let exit = parse_trade_date(&t.exit_date)?;
        Some((exit - entry).num_days() as f64)
    }).collect();
    let avg_holding_days = if holding_days.is_empty() { 0.0 }
        else { holding_days.iter().sum::<f64>() / holding_days.len() as f64 };

    // Risk metrics from trade returns
    let returns: Vec<f64> = closed.iter().map(|t| t.pnl_pct / 100.0).collect();
    let sharpe_ratio = compute_sharpe(&returns);
    let sortino_ratio = compute_sortino(&returns);
    let max_drawdown_pct = compute_max_drawdown(&closed);

    // Best / worst
    let best = closed.iter().max_by(|a, b| a.pnl_pct.partial_cmp(&b.pnl_pct).unwrap_or(std::cmp::Ordering::Equal));
    let worst = closed.iter().min_by(|a, b| a.pnl_pct.partial_cmp(&b.pnl_pct).unwrap_or(std::cmp::Ordering::Equal));

    let best_trade = best.map(|t| TradeSummary {
        ticker: t.ticker.clone(), pnl_pct: t.pnl_pct, pnl_dollars: t.pnl_dollars,
        entry_date: t.entry_date.clone(), exit_date: t.exit_date.clone(),
    });
    let worst_trade = worst.map(|t| TradeSummary {
        ticker: t.ticker.clone(), pnl_pct: t.pnl_pct, pnl_dollars: t.pnl_dollars,
        entry_date: t.entry_date.clone(), exit_date: t.exit_date.clone(),
    });

    // Signal attribution
    let signal_attribution = compute_signal_attribution(&closed);

    // Sector exposure
    let sector_exposure = compute_sector_exposure(&closed);

    // Monthly returns
    let monthly_returns = compute_monthly_returns(&closed);

    // Equity curve (cumulative P&L by trade exit date)
    let equity_curve = compute_equity_curve(&closed);

    Ok(PortfolioAnalytics {
        total_trades: total,
        open_count,
        win_rate,
        loss_rate,
        avg_win_pct,
        avg_loss_pct,
        profit_factor,
        total_return_pct,
        total_return_dollars,
        sharpe_ratio,
        sortino_ratio,
        max_drawdown_pct,
        avg_holding_days,
        best_trade,
        worst_trade,
        signal_attribution,
        sector_exposure,
        monthly_returns,
        equity_curve,
    })
}

// ---------------------------------------------------------------------------
// DB queries
// ---------------------------------------------------------------------------

fn get_closed_trades(conn: &Connection) -> Result<Vec<ClosedTrade>, String> {
    let mut stmt = conn.prepare(
        "SELECT pt.ticker, pt.entry_date, pt.exit_date, pt.entry_price, pt.exit_price,
                pt.position_size, pt.pnl_pct, pt.signal_profile, e.sector
         FROM paper_trades pt
         LEFT JOIN entities e ON e.id = pt.entity_id
         WHERE pt.status IN ('closed', 'stopped_out', 'expired')
           AND pt.exit_price IS NOT NULL AND pt.pnl_pct IS NOT NULL
         ORDER BY pt.exit_date ASC"
    ).map_err(|e| e.to_string())?;

    let trades = stmt.query_map([], |row| {
        let entry_price: f64 = row.get(3)?;
        let exit_price: f64 = row.get(4)?;
        let position_size: f64 = row.get(5)?;
        let pnl_pct: f64 = row.get(6)?;
        // Dollar P&L = position_size * pnl_pct / 100
        let pnl_dollars = position_size * pnl_pct / 100.0;

        Ok(ClosedTrade {
            ticker: row.get(0)?,
            entry_date: row.get(1)?,
            exit_date: row.get::<_, Option<String>>(2)?.unwrap_or_default(),
            entry_price,
            exit_price,
            position_size,
            pnl_pct,
            pnl_dollars,
            signal_profile: row.get(7)?,
            sector: row.get(8)?,
        })
    }).map_err(|e| e.to_string())?
    .filter_map(|r| r.ok())
    .collect();

    Ok(trades)
}

fn get_open_count(conn: &Connection) -> Result<usize, String> {
    conn.query_row(
        "SELECT COUNT(*) FROM paper_trades WHERE status = 'open'",
        [],
        |row| row.get::<_, i64>(0),
    ).map(|c| c as usize).map_err(|e| e.to_string())
}

// ---------------------------------------------------------------------------
// Sharpe & Sortino (annualized from per-trade returns)
// ---------------------------------------------------------------------------

fn compute_sharpe(returns: &[f64]) -> f64 {
    if returns.len() < 2 { return 0.0; }
    let mean = returns.iter().sum::<f64>() / returns.len() as f64;
    let variance = returns.iter().map(|r| (r - mean).powi(2)).sum::<f64>() / (returns.len() - 1) as f64;
    let std_dev = variance.sqrt();
    if std_dev == 0.0 { return 0.0; }
    // Annualize: assume ~50 trades/year (rough estimate)
    let trades_per_year = 50.0_f64.min(returns.len() as f64);
    (mean / std_dev) * trades_per_year.sqrt()
}

fn compute_sortino(returns: &[f64]) -> f64 {
    if returns.len() < 2 { return 0.0; }
    let mean = returns.iter().sum::<f64>() / returns.len() as f64;
    let downside: Vec<f64> = returns.iter().filter(|r| **r < 0.0).map(|r| r.powi(2)).collect();
    if downside.is_empty() { return f64::INFINITY; }
    let downside_dev = (downside.iter().sum::<f64>() / downside.len() as f64).sqrt();
    if downside_dev == 0.0 { return 0.0; }
    let trades_per_year = 50.0_f64.min(returns.len() as f64);
    (mean / downside_dev) * trades_per_year.sqrt()
}

// ---------------------------------------------------------------------------
// Max drawdown from cumulative P&L
// ---------------------------------------------------------------------------

fn compute_max_drawdown(trades: &[ClosedTrade]) -> f64 {
    if trades.is_empty() { return 0.0; }

    let mut cumulative = 0.0_f64;
    let mut peak = 0.0_f64;
    let mut max_dd = 0.0_f64;

    for t in trades {
        cumulative += t.pnl_dollars;
        if cumulative > peak { peak = cumulative; }
        let dd = if peak > 0.0 { (peak - cumulative) / peak * 100.0 } else { 0.0 };
        if dd > max_dd { max_dd = dd; }
    }

    max_dd
}

// ---------------------------------------------------------------------------
// Signal attribution
// ---------------------------------------------------------------------------

fn compute_signal_attribution(trades: &[ClosedTrade]) -> Vec<SignalAttribution> {
    SIGNAL_DIMENSIONS.iter().map(|dim| {
        let mut win_vals = Vec::new();
        let mut loss_vals = Vec::new();

        for t in trades {
            if let Some(val) = parse_signal_value(&t.signal_profile, dim) {
                if val > 0.01 { // Only count if signal was present
                    if t.pnl_pct > 0.0 { win_vals.push(val); }
                    else { loss_vals.push(val); }
                }
            }
        }

        let avg_on_wins = if win_vals.is_empty() { 0.0 }
            else { win_vals.iter().sum::<f64>() / win_vals.len() as f64 };
        let avg_on_losses = if loss_vals.is_empty() { 0.0 }
            else { loss_vals.iter().sum::<f64>() / loss_vals.len() as f64 };

        SignalAttribution {
            dimension: dim.to_string(),
            avg_on_wins,
            avg_on_losses,
            predictive_edge: avg_on_wins - avg_on_losses,
            sample_count: win_vals.len() + loss_vals.len(),
        }
    }).collect()
}

fn parse_signal_value(profile: &str, dimension: &str) -> Option<f64> {
    let parsed: serde_json::Value = serde_json::from_str(profile).ok()?;
    parsed.get(dimension)?.as_f64()
}

// ---------------------------------------------------------------------------
// Sector exposure
// ---------------------------------------------------------------------------

fn compute_sector_exposure(trades: &[ClosedTrade]) -> Vec<SectorExposure> {
    let mut sectors: std::collections::HashMap<String, (usize, f64, usize)> = std::collections::HashMap::new();

    for t in trades {
        let sector = t.sector.clone().unwrap_or_else(|| "unknown".to_string());
        let entry = sectors.entry(sector).or_insert((0, 0.0, 0));
        entry.0 += 1; // count
        entry.1 += t.pnl_dollars; // total pnl
        if t.pnl_pct > 0.0 { entry.2 += 1; } // wins
    }

    let mut result: Vec<SectorExposure> = sectors.into_iter().map(|(sector, (count, pnl, wins))| {
        SectorExposure {
            sector,
            trade_count: count,
            total_pnl: pnl,
            win_rate: if count > 0 { wins as f64 / count as f64 * 100.0 } else { 0.0 },
        }
    }).collect();

    result.sort_by(|a, b| b.trade_count.cmp(&a.trade_count));
    result
}

// ---------------------------------------------------------------------------
// Monthly returns
// ---------------------------------------------------------------------------

fn compute_monthly_returns(trades: &[ClosedTrade]) -> Vec<MonthlyReturn> {
    let mut months: std::collections::BTreeMap<String, (f64, f64, usize)> = std::collections::BTreeMap::new();

    for t in trades {
        if t.exit_date.len() >= 7 {
            let month = t.exit_date[..7].to_string();
            let entry = months.entry(month).or_insert((0.0, 0.0, 0));
            entry.0 += t.pnl_dollars;
            entry.1 += t.position_size;
            entry.2 += 1;
        }
    }

    months.into_iter().map(|(month, (pnl, invested, count))| {
        MonthlyReturn {
            month,
            pnl_dollars: pnl,
            pnl_pct: if invested > 0.0 { pnl / invested * 100.0 } else { 0.0 },
            trade_count: count,
        }
    }).collect()
}

// ---------------------------------------------------------------------------
// Equity curve (cumulative P&L by exit date)
// ---------------------------------------------------------------------------

fn compute_equity_curve(trades: &[ClosedTrade]) -> Vec<EquityPoint> {
    let mut cumulative = 0.0;
    trades.iter().map(|t| {
        cumulative += t.pnl_dollars;
        EquityPoint {
            date: t.exit_date.clone(),
            value: cumulative,
        }
    }).collect()
}

// ---------------------------------------------------------------------------
// Empty state
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Trade Journal
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct TradeJournal {
    pub trade_id: i64,
    pub journal: String,
    pub signal_breakdown: Vec<SignalEntry>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SignalEntry {
    pub dimension: String,
    pub entry_value: f64,
}

/// Get or generate a trade journal for a closed trade.
pub fn get_or_generate_journal(conn: &Connection, trade_id: i64) -> Result<TradeJournal, String> {
    // Check if journal already exists
    let existing: Option<String> = conn.query_row(
        "SELECT trade_journal FROM paper_trades WHERE id = ?1",
        [trade_id],
        |row| row.get(0),
    ).map_err(|e| e.to_string())?;

    let trade = conn.query_row(
        "SELECT pt.ticker, pt.entry_date, pt.exit_date, pt.entry_price, pt.exit_price,
                pt.position_size, pt.pnl_pct, pt.pnl, pt.signal_profile, pt.status,
                pt.confidence, e.name, pt.entity_id
         FROM paper_trades pt
         LEFT JOIN entities e ON e.id = pt.entity_id
         WHERE pt.id = ?1",
        [trade_id],
        |row| Ok((
            row.get::<_, String>(0)?,  // ticker
            row.get::<_, String>(1)?,  // entry_date
            row.get::<_, Option<String>>(2)?, // exit_date
            row.get::<_, f64>(3)?,     // entry_price
            row.get::<_, Option<f64>>(4)?, // exit_price
            row.get::<_, f64>(5)?,     // position_size
            row.get::<_, Option<f64>>(6)?, // pnl_pct
            row.get::<_, Option<f64>>(7)?, // pnl
            row.get::<_, String>(8)?,  // signal_profile
            row.get::<_, String>(9)?,  // status
            row.get::<_, f64>(10)?,    // confidence
            row.get::<_, Option<String>>(11)?, // entity name
            row.get::<_, i64>(12)?,    // entity_id
        )),
    ).map_err(|e| e.to_string())?;

    let (ticker, entry_date, exit_date, entry_price, exit_price, position_size,
         pnl_pct, pnl, signal_profile, status, _confidence, entity_name, _entity_id) = trade;

    // Parse signal breakdown
    let signal_breakdown = parse_signal_breakdown(&signal_profile);

    let journal = if let Some(existing) = existing {
        existing
    } else {
        let generated = generate_journal_text(
            &ticker, entity_name.as_deref(), &entry_date, exit_date.as_deref(),
            entry_price, exit_price, position_size, pnl_pct, pnl,
            &status, &signal_breakdown,
        );
        // Store it
        conn.execute(
            "UPDATE paper_trades SET trade_journal = ?1 WHERE id = ?2",
            rusqlite::params![generated, trade_id],
        ).ok();
        generated
    };

    Ok(TradeJournal { trade_id, journal, signal_breakdown })
}

fn parse_signal_breakdown(profile: &str) -> Vec<SignalEntry> {
    let parsed: serde_json::Value = match serde_json::from_str(profile) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };

    SIGNAL_DIMENSIONS.iter().filter_map(|dim| {
        let val = parsed.get(dim)?.as_f64()?;
        if val > 0.01 {
            Some(SignalEntry { dimension: dim.to_string(), entry_value: val })
        } else {
            None
        }
    }).collect()
}

fn generate_journal_text(
    ticker: &str, entity_name: Option<&str>,
    entry_date: &str, exit_date: Option<&str>,
    entry_price: f64, exit_price: Option<f64>,
    position_size: f64, pnl_pct: Option<f64>, pnl: Option<f64>,
    status: &str, signals: &[SignalEntry],
) -> String {
    let mut parts = Vec::new();
    let name = entity_name.unwrap_or(ticker);

    // Entry narrative
    let top_signals: Vec<&SignalEntry> = {
        let mut sorted: Vec<&SignalEntry> = signals.iter().collect();
        sorted.sort_by(|a, b| b.entry_value.partial_cmp(&a.entry_value).unwrap_or(std::cmp::Ordering::Equal));
        sorted.into_iter().take(3).collect()
    };

    let drivers = if top_signals.is_empty() {
        "convergence signals".to_string()
    } else {
        top_signals.iter()
            .map(|s| format!("{} ({:.0}%)", s.dimension, s.entry_value * 100.0))
            .collect::<Vec<_>>()
            .join(", ")
    };

    parts.push(format!(
        "Entered {} long on {} at ${:.2} driven by {}. Position size: ${:.0}.",
        name, entry_date, entry_price, drivers, position_size,
    ));

    // Exit narrative
    if let (Some(exit_d), Some(exit_p), Some(pnl_p)) = (exit_date, exit_price, pnl_pct) {
        let holding_days = match (parse_trade_date(entry_date), parse_trade_date(exit_d)) {
            (Some(e), Some(x)) => (x - e).num_days(),
            _ => 0,
        };

        // SEV2: the DB has no close_reason column, so we can only state the reason the STATUS
        // actually encodes. stopped_out was written by the exit engine and carries a real
        // reason. A plain 'closed' row is a manual close with the reason NOT recorded — do
        // not invent "signal decay"/"profit target" (fabricated claims the data can't support).
        //
        // 'expired' used to read "the 90-day holding limit was reached". There is no such
        // limit: the calendar expiry was removed from position_management on 2026-07-15 and
        // nothing has written this status since. No row in the live ledger carries it, but an
        // older database might, and telling its owner a deleted rule closed their position is
        // exactly the fabrication the note above forbids.
        let exit_reason = match status {
            "stopped_out" => "the trailing stop was hit",
            "expired" => "the retired calendar-expiry engine closed it (that rule no longer exists)",
            _ => "it was closed manually (exit reason not recorded)",
        };

        let pnl_str = if let Some(pnl_d) = pnl {
            format!("{}{:.1}% (${}{:.0})",
                if pnl_p >= 0.0 { "+" } else { "" }, pnl_p,
                if pnl_d >= 0.0 { "+" } else { "" }, pnl_d)
        } else {
            format!("{}{:.1}%", if pnl_p >= 0.0 { "+" } else { "" }, pnl_p)
        };

        parts.push(format!(
            "Exited after {} days at ${:.2} — {} because {}.",
            holding_days, exit_p, pnl_str, exit_reason,
        ));
    }

    // Signal quality assessment
    if !top_signals.is_empty() {
        let strong: Vec<&&SignalEntry> = top_signals.iter().filter(|s| s.entry_value > 0.5).collect();
        if strong.len() >= 2 {
            parts.push("Multiple strong signals aligned on this trade.".to_string());
        } else if strong.len() == 1 {
            parts.push(format!("The {} signal was the primary driver.", strong[0].dimension));
        }
    }

    parts.join(" ")
}

// ---------------------------------------------------------------------------
// Empty state
// ---------------------------------------------------------------------------

fn empty_analytics(open_count: usize) -> PortfolioAnalytics {
    PortfolioAnalytics {
        total_trades: 0,
        open_count,
        win_rate: 0.0,
        loss_rate: 0.0,
        avg_win_pct: 0.0,
        avg_loss_pct: 0.0,
        profit_factor: 0.0,
        total_return_pct: 0.0,
        total_return_dollars: 0.0,
        sharpe_ratio: 0.0,
        sortino_ratio: 0.0,
        max_drawdown_pct: 0.0,
        avg_holding_days: 0.0,
        best_trade: None,
        worst_trade: None,
        signal_attribution: Vec::new(),
        sector_exposure: Vec::new(),
        monthly_returns: Vec::new(),
        equity_curve: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_trade_date_handles_both_shapes() {
        // Date-only (the 3 rows that used to work).
        let d1 = parse_trade_date("2026-04-15").unwrap();
        // Full timestamp (the 6 rows the old parser silently rejected → "0 days").
        let d2 = parse_trade_date("2026-06-08T21:17:52").unwrap();
        assert_eq!((d2 - d1).num_days(), 54);
    }

    #[test]
    fn parse_trade_date_timestamped_hold_is_nonzero() {
        // Regression for SEV3: id 1 held 9 days but journaled "0 days" under the old parser.
        let entry = parse_trade_date("2026-04-15T15:41:05").unwrap();
        let exit = parse_trade_date("2026-04-24T08:18:39").unwrap();
        assert_eq!((exit - entry).num_days(), 9);
    }

    #[test]
    fn parse_trade_date_rejects_garbage() {
        assert!(parse_trade_date("not-a-date").is_none());
        assert!(parse_trade_date("").is_none());
    }
}
