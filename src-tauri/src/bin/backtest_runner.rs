//! Standalone backtest runner for Pulse.
//!
//! `backtester::run_backtest` takes a plain `&Connection`, so this points it
//! at an arbitrary DB file (e.g. a Phase-2-backfilled scratch copy) instead
//! of the live app's DbState connection — for re-running a backtest against
//! a historical snapshot without touching production data.
//! Run `cargo run --bin pulse-backtest -- --help` for usage.

use anyhow::Result;
use clap::Parser;
use rusqlite::Connection;
use std::path::PathBuf;

use pulse_lib::services::backtester::{self, BacktestConfig};

#[derive(Parser, Debug)]
#[command(name = "pulse-backtest", about = "Standalone backtest runner for Pulse")]
struct Args {
    /// Path to SQLite database (e.g. a scratch copy of a backfilled DB)
    #[arg(long)]
    db_path: String,

    #[arg(long)]
    start_date: String,

    #[arg(long)]
    end_date: String,

    #[arg(long, default_value_t = 0.30)]
    min_score: f64,

    #[arg(long, default_value_t = -10.0)]
    stop_loss_pct: f64,

    #[arg(long, default_value_t = 15.0)]
    take_profit_pct: f64,

    #[arg(long, default_value_t = 30)]
    max_hold_days: i64,

    #[arg(long, default_value_t = 10)]
    max_positions: usize,

    #[arg(long, default_value_t = 5.0)]
    position_size_pct: f64,
}

fn resolve_db_path(path: &str) -> PathBuf {
    if let Some(rest) = path.strip_prefix("~/") {
        if let Some(home) = dirs::home_dir() {
            return home.join(rest);
        }
    }
    PathBuf::from(path)
}

fn main() -> Result<()> {
    let args = Args::parse();

    let db_path = resolve_db_path(&args.db_path);
    if !db_path.exists() {
        anyhow::bail!("Database not found at {}. Use --db-path to specify location.", db_path.display());
    }

    let conn = Connection::open(&db_path)?;

    let config = BacktestConfig {
        start_date: args.start_date,
        end_date: args.end_date,
        min_score: args.min_score,
        stop_loss_pct: args.stop_loss_pct,
        take_profit_pct: args.take_profit_pct,
        max_hold_days: args.max_hold_days,
        max_positions: args.max_positions,
        position_size_pct: args.position_size_pct,
    };

    let result = backtester::run_backtest(&conn, config).map_err(|e| anyhow::anyhow!(e))?;

    println!("{}", result.config_summary);
    println!("Signals seen: {}", result.total_signals);
    println!("Trades taken: {} (won {}, lost {})", result.trades_taken, result.trades_won, result.trades_lost);
    println!("Hit rate: {:.1}%", result.hit_rate);
    println!("Avg return per trade: {:.2}%", result.avg_return_pct);
    println!("Total return: {:.2}%  (${:.2} -> ${:.2})", result.total_return_pct, result.starting_equity, result.ending_equity);
    println!("Max drawdown: {:.2}%", result.max_drawdown_pct);
    println!("Sharpe ratio: {:.3}", result.sharpe_ratio);
    println!("Avg holding days: {:.1}", result.avg_holding_days);
    println!();
    for t in &result.trades {
        println!(
            "  {} {} -> {} | entry {} @ ${:.2} | exit {} @ ${:.2} | {:+.1}% ({}) | held {}d | score {:.2}",
            t.ticker, t.entity_name, t.exit_reason,
            t.entry_date, t.entry_price, t.exit_date, t.exit_price,
            t.pnl_pct, if t.pnl_pct > 0.0 { "win" } else { "loss" },
            t.holding_days, t.compound_score
        );
    }

    Ok(())
}
