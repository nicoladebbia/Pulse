#!/usr/bin/env python3
"""
Pulse Backtester — Validate cross-signal strategies against historical data.

Runs against the Pulse SQLite database. Uses accumulated entity_prices and
cross_signals data to simulate trades and compute performance metrics.

Usage:
    python3 scripts/backtester.py [--db-path pulse.db] [--days 180] [--threshold 0.3]

Outputs results to backtest_results table and prints summary.
"""

import argparse
import json
import math
import os
import sqlite3
import sys
from dataclasses import dataclass, field
from datetime import datetime, timedelta
from typing import Optional


@dataclass
class SimulatedTrade:
    entity_id: int
    ticker: str
    entry_date: str
    entry_price: float
    exit_date: Optional[str] = None
    exit_price: Optional[float] = None
    signal_score: float = 0.0
    signal_profile: str = ""
    pnl_pct: Optional[float] = None
    holding_days: int = 0
    result: str = "open"  # "win", "loss", "stopped", "expired"


@dataclass
class BacktestResult:
    start_date: str
    end_date: str
    total_signals: int = 0
    trades_taken: int = 0
    wins: int = 0
    losses: int = 0
    stopped_out: int = 0
    hit_rate: float = 0.0
    avg_return: float = 0.0
    max_drawdown: float = 0.0
    sharpe_ratio: float = 0.0
    avg_holding_days: float = 0.0
    signal_profile: str = ""
    trades: list = field(default_factory=list)


def get_db_path():
    """Find the database."""
    parser = argparse.ArgumentParser(description="Pulse Backtester")
    parser.add_argument("--db-path", default=None, help="Path to pulse.db")
    parser.add_argument("--days", type=int, default=180, help="Lookback period in days")
    parser.add_argument("--threshold", type=float, default=0.3, help="Minimum compound score to trade")
    parser.add_argument("--hold-days", type=int, default=90, help="Max holding period")
    parser.add_argument("--stop-loss", type=float, default=-10.0, help="Stop loss percentage")
    args = parser.parse_args()

    if args.db_path:
        return args.db_path, args

    # Try common locations
    candidates = [
        os.path.join(os.path.dirname(__file__), "..", "pulse.db"),
        os.path.expanduser("~/Library/Application Support/com.pulse.app/pulse.db"),
    ]
    for path in candidates:
        if os.path.exists(path):
            return path, args

    print("Error: Could not find pulse.db. Use --db-path to specify.", file=sys.stderr)
    sys.exit(1)


def run_backtest(conn: sqlite3.Connection, args) -> BacktestResult:
    """Run backtest using accumulated data."""
    cursor = conn.cursor()

    end_date = datetime.now().strftime("%Y-%m-%d")
    start_date = (datetime.now() - timedelta(days=args.days)).strftime("%Y-%m-%d")

    result = BacktestResult(start_date=start_date, end_date=end_date)

    # Get all cross-signal scores in the period
    cursor.execute("""
        SELECT cs.entity_id, cs.ticker, cs.compound_score, cs.computed_at,
               cs.insider_signal, cs.news_momentum, cs.government_signal,
               cs.source_diversity, cs.convergence_detected,
               e.name
        FROM cross_signals cs
        LEFT JOIN entities e ON e.id = cs.entity_id
        WHERE cs.computed_at >= ? AND cs.computed_at <= ?
          AND cs.compound_score >= ?
          AND cs.ticker IS NOT NULL
        ORDER BY cs.computed_at ASC
    """, (start_date, end_date, args.threshold))

    signals = cursor.fetchall()
    result.total_signals = len(signals)

    if not signals:
        print(f"No signals found above threshold {args.threshold} in {start_date} to {end_date}")
        return result

    # Simulate trades
    active_tickers = set()
    trades = []

    for (entity_id, ticker, score, signal_date, insider, news, gov, diversity, convergence, name) in signals:
        if not ticker or ticker in active_tickers:
            continue

        # Get entry price (next day's close after signal)
        cursor.execute("""
            SELECT close, date FROM entity_prices
            WHERE ticker = ? AND date > ?
            ORDER BY date ASC LIMIT 1
        """, (ticker, signal_date))
        entry_row = cursor.fetchone()
        if not entry_row:
            continue

        entry_price, entry_date = entry_row

        # Find exit: stop-loss, time limit, or end of data
        cursor.execute("""
            SELECT close, date FROM entity_prices
            WHERE ticker = ? AND date > ?
            ORDER BY date ASC
        """, (ticker, entry_date))
        future_prices = cursor.fetchall()

        trade = SimulatedTrade(
            entity_id=entity_id,
            ticker=ticker,
            entry_date=entry_date,
            entry_price=entry_price,
            signal_score=score,
            signal_profile=json.dumps({
                "insider": round(insider or 0, 3),
                "news": round(news or 0, 3),
                "government": round(gov or 0, 3),
                "diversity": diversity or 0,
                "convergence": bool(convergence),
            }),
        )

        for close_price, close_date in future_prices:
            pnl_pct = ((close_price - entry_price) / entry_price) * 100
            days_held = (datetime.strptime(close_date, "%Y-%m-%d") - datetime.strptime(entry_date, "%Y-%m-%d")).days

            # Stop loss
            if pnl_pct <= args.stop_loss:
                trade.exit_price = close_price
                trade.exit_date = close_date
                trade.pnl_pct = pnl_pct
                trade.holding_days = days_held
                trade.result = "stopped"
                break

            # Time limit
            if days_held >= args.hold_days:
                trade.exit_price = close_price
                trade.exit_date = close_date
                trade.pnl_pct = pnl_pct
                trade.holding_days = days_held
                trade.result = "win" if pnl_pct > 0 else "loss"
                break

        # If no exit found, use latest price
        if trade.exit_price is None and future_prices:
            last_price, last_date = future_prices[-1]
            pnl_pct = ((last_price - entry_price) / entry_price) * 100
            days_held = (datetime.strptime(last_date, "%Y-%m-%d") - datetime.strptime(entry_date, "%Y-%m-%d")).days
            trade.exit_price = last_price
            trade.exit_date = last_date
            trade.pnl_pct = pnl_pct
            trade.holding_days = days_held
            trade.result = "win" if pnl_pct > 0 else "loss"

        if trade.exit_price is not None:
            trades.append(trade)
            active_tickers.add(ticker)

    # Compute metrics
    result.trades_taken = len(trades)
    result.trades = trades

    if not trades:
        return result

    returns = [t.pnl_pct for t in trades if t.pnl_pct is not None]
    result.wins = sum(1 for r in returns if r > 0)
    result.losses = sum(1 for r in returns if r <= 0)
    result.stopped_out = sum(1 for t in trades if t.result == "stopped")
    result.hit_rate = result.wins / len(returns) if returns else 0
    result.avg_return = sum(returns) / len(returns) if returns else 0
    result.avg_holding_days = sum(t.holding_days for t in trades) / len(trades)

    # Max drawdown (sequential)
    cumulative = 0.0
    peak = 0.0
    max_dd = 0.0
    for r in returns:
        cumulative += r
        peak = max(peak, cumulative)
        drawdown = peak - cumulative
        max_dd = max(max_dd, drawdown)
    result.max_drawdown = max_dd

    # Sharpe ratio (annualized, assuming 252 trading days)
    if len(returns) > 1:
        mean_r = result.avg_return
        std_r = math.sqrt(sum((r - mean_r) ** 2 for r in returns) / (len(returns) - 1))
        if std_r > 0:
            avg_hold = result.avg_holding_days if result.avg_holding_days > 0 else 30
            trades_per_year = 252 / avg_hold
            result.sharpe_ratio = (mean_r / std_r) * math.sqrt(trades_per_year)

    return result


def save_results(conn: sqlite3.Connection, result: BacktestResult):
    """Store backtest results in database."""
    trade_details = json.dumps([
        {
            "ticker": t.ticker,
            "entry_date": t.entry_date,
            "exit_date": t.exit_date,
            "entry_price": round(t.entry_price, 2),
            "exit_price": round(t.exit_price, 2) if t.exit_price else None,
            "pnl_pct": round(t.pnl_pct, 2) if t.pnl_pct else None,
            "holding_days": t.holding_days,
            "result": t.result,
            "signal_score": round(t.signal_score, 3),
        }
        for t in result.trades
    ])

    conn.execute("""
        INSERT INTO backtest_results (signal_profile, start_date, end_date,
            total_signals, hit_rate, avg_return, max_drawdown, sharpe_ratio,
            avg_holding_days, details)
        VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
    """, (
        result.signal_profile or "default",
        result.start_date,
        result.end_date,
        result.trades_taken,
        result.hit_rate,
        result.avg_return,
        result.max_drawdown,
        result.sharpe_ratio,
        result.avg_holding_days,
        trade_details,
    ))
    conn.commit()


def print_summary(result: BacktestResult):
    """Print backtest results."""
    print("\n" + "=" * 60)
    print("  BACKTEST RESULTS")
    print("=" * 60)
    print(f"  Period:         {result.start_date} → {result.end_date}")
    print(f"  Signals found:  {result.total_signals}")
    print(f"  Trades taken:   {result.trades_taken}")
    print(f"  Wins:           {result.wins}")
    print(f"  Losses:         {result.losses}")
    print(f"  Stopped out:    {result.stopped_out}")
    print(f"  Hit rate:       {result.hit_rate:.1%}")
    print(f"  Avg return:     {result.avg_return:+.2f}%")
    print(f"  Max drawdown:   {result.max_drawdown:.2f}%")
    print(f"  Sharpe ratio:   {result.sharpe_ratio:.2f}")
    print(f"  Avg hold days:  {result.avg_holding_days:.0f}")
    print("=" * 60)

    if result.trades:
        print("\n  Top trades:")
        sorted_trades = sorted(result.trades, key=lambda t: t.pnl_pct or 0, reverse=True)
        for t in sorted_trades[:5]:
            print(f"    {t.ticker:>6s}  {t.pnl_pct:+6.1f}%  ({t.holding_days}d)  entry=${t.entry_price:.2f}")
        if len(sorted_trades) > 5:
            print(f"    ... and {len(sorted_trades) - 5} more trades")

    # GO/NO-GO assessment
    print()
    if result.hit_rate >= 0.55 and result.trades_taken >= 10:
        print("  ✅ GO — Hit rate ≥55% with sufficient sample size")
        print("     Proceed to paper trading with these signal weights")
    elif result.trades_taken < 10:
        print("  ⏳ INSUFFICIENT DATA — Need more accumulated data")
        print(f"     Only {result.trades_taken} trades, need 10+ for significance")
    else:
        print("  ❌ NO-GO — Hit rate below 55%")
        print("     Investigate: which signals are noise? Wrong time horizon?")
    print()


def main():
    db_path, args = get_db_path()
    print(f"Backtesting against: {db_path}")
    print(f"  Lookback: {args.days} days | Threshold: {args.threshold} | Hold: {args.hold_days}d | Stop: {args.stop_loss}%")

    conn = sqlite3.connect(db_path)

    # Check we have data
    try:
        price_count = conn.execute("SELECT COUNT(*) FROM entity_prices").fetchone()[0]
        signal_count = conn.execute("SELECT COUNT(*) FROM cross_signals").fetchone()[0]
        print(f"  Data: {price_count} price records, {signal_count} cross-signal scores")
    except Exception as e:
        print(f"Error: {e}")
        print("Run the pipeline first to accumulate data.")
        sys.exit(1)

    if price_count == 0 or signal_count == 0:
        print("Not enough data for backtesting. Run the pipeline daily for 30+ days.")
        sys.exit(0)

    result = run_backtest(conn, args)
    save_results(conn, result)
    print_summary(result)

    conn.close()


if __name__ == "__main__":
    main()
