#!/usr/bin/env python3
"""
Pulse Replay Engine — Day-by-day signal simulation on historical data.

Reads from pulse_historical.db (populated by bootstrap_historical.py).
Simulates the cross-signal detection pipeline day by day, opens trades
on convergence, evaluates positions, and reports performance metrics.

Usage:
    python3 scripts/replay_engine.py [--db pulse_historical.db] [--start 2024-04-01] [--end 2026-04-01]
    python3 scripts/replay_engine.py --walk-forward  # Walk-forward optimization
    python3 scripts/replay_engine.py --monte-carlo 1000  # Monte Carlo significance test

GO/NO-GO Gate Requirements:
  - 50+ simulated trades
  - Hit rate > 52% (out-of-sample for walk-forward)
  - Sharpe ratio > 0.5
  - Monte Carlo p-value < 0.10 for 2+ signal dimensions
"""

import argparse
import json
import math
import os
import random
import sqlite3
import sys
from collections import defaultdict
from dataclasses import dataclass, field
from datetime import datetime, timedelta
from typing import Optional

DB_PATH = os.path.join(os.path.dirname(__file__), "..", "pulse_historical.db")

# Signal weights — synced to cleaned live defaults (load_calibrated_weights in
# pipeline.rs, 2026-06-05): institutional + supply_chain zeroed (dead/buggy),
# their weight redistributed across live signals. Backtest measures the CLEANED
# signal, not the old noisy one.
WEIGHTS = {
    "insider": 0.2391,
    "institutional": 0.0,
    "news": 0.2391,
    "government": 0.1848,
    "search": 0.0543,
    "patent": 0.0435,
    "supply_chain": 0.0,
    "political": 0.2391,
}

# Normalization scales — must match pipeline.rs
SCALES = {
    "insider": 1_000_000.0,
    "government_contract": 10_000_000.0,
    "government_regulatory": 3.0,
    "patent": 5.0,
    "lobbying": 100_000.0,
}

# Entry strategy: "convergence" (original 8-signal fusion) or "news" (news-led,
# Path A). Set from --strategy in main(). Module-level so compute_signals_for_ticker
# can branch without threading a param through run_replay/walk_forward.
STRATEGY = "convergence"

STOP_LOSS_PCT = -10.0
MAX_HOLD_DAYS = 90
# Realistic round-trip friction deducted from every trade's return: slippage on
# entry + exit (market orders cross the spread) + implicit costs. 20 bps total is
# slightly conservative for liquid US equities; the previous backtest deducted
# ZERO, which inflated every result. A real edge must survive friction.
ROUND_TRIP_COST_PCT = 0.20
CONVERGENCE_THRESHOLD = 0.2
MIN_CONVERGENT_SIGNALS = 2
MIN_SOURCE_DIVERSITY = 2
MIN_COMPOUND_SCORE = 0.20  # Require meaningful compound score


@dataclass
class Position:
    ticker: str
    entry_date: str
    entry_price: float
    signal_score: float
    signal_profile: dict
    high_water_mark: float = 0.0
    pnl_pct: float = 0.0
    days_held: int = 0

    def __post_init__(self):
        self.high_water_mark = self.entry_price


@dataclass
class ClosedTrade:
    ticker: str
    entry_date: str
    exit_date: str
    entry_price: float
    exit_price: float
    pnl_pct: float
    holding_days: int
    signal_score: float
    signal_profile: dict
    exit_reason: str  # "stop_loss", "expired", "target"


def normalize_signal(value: float, scale: float) -> float:
    if value <= 0:
        return 0.0
    return 1.0 - math.exp(-value / scale)


def compute_signals_for_ticker(conn: sqlite3.Connection, ticker: str, as_of_date: str) -> dict:
    """Compute signal dimensions for a ticker as of a given date."""
    # Get CIK for this ticker
    row = conn.execute("SELECT cik FROM companies WHERE ticker = ?", (ticker,)).fetchone()
    if not row:
        return {}
    cik = row[0]

    date_30d = (datetime.strptime(as_of_date, "%Y-%m-%d") - timedelta(days=30)).strftime("%Y-%m-%d")
    date_90d = (datetime.strptime(as_of_date, "%Y-%m-%d") - timedelta(days=90)).strftime("%Y-%m-%d")
    date_14d = (datetime.strptime(as_of_date, "%Y-%m-%d") - timedelta(days=14)).strftime("%Y-%m-%d")

    # Insider signal: weighted buy volume (enriched) + raw filing count (unenriched)
    # Tier 1: enriched Form 4s with actual transaction data
    insider_vol_enriched = conn.execute(
        """SELECT COALESCE(SUM(total_value * signal_weight), 0)
           FROM filings WHERE cik = ? AND form_type = '4'
           AND file_date >= ? AND file_date <= ?
           AND signal_weight IS NOT NULL AND signal_weight > 0""",
        (cik, date_30d, as_of_date)
    ).fetchone()[0]

    # Tier 2: count purchases specifically (enriched with P code)
    purchase_count = conn.execute(
        """SELECT COUNT(*) FROM filings WHERE cik = ? AND form_type = '4'
           AND file_date >= ? AND file_date <= ?
           AND transaction_code = 'P'""",
        (cik, date_30d, as_of_date)
    ).fetchone()[0]

    # Negative signal from informative sales
    insider_sells = conn.execute(
        """SELECT COALESCE(SUM(total_value * ABS(signal_weight)), 0)
           FROM filings WHERE cik = ? AND form_type = '4'
           AND file_date >= ? AND file_date <= ?
           AND signal_weight IS NOT NULL AND signal_weight < 0""",
        (cik, date_30d, as_of_date)
    ).fetchone()[0]

    # If no enriched buy data but purchases exist, use count * $100K as estimate
    if insider_vol_enriched == 0 and purchase_count > 0:
        insider_vol_enriched = purchase_count * 100_000.0

    insider_vol = insider_vol_enriched - insider_sells

    # Cluster buy detection: multiple distinct insiders PURCHASING in 14 days
    distinct_buyers = conn.execute(
        """SELECT COUNT(DISTINCT COALESCE(owner_name, entity_name)) FROM filings
           WHERE cik = ? AND form_type = '4' AND transaction_code = 'P'
           AND file_date >= ? AND file_date <= ?""",
        (cik, date_14d, as_of_date)
    ).fetchone()[0]

    # Cluster buy is the strongest insider signal (academic research confirms)
    cluster_bonus = 2.0 if distinct_buyers >= 3 else (1.5 if distinct_buyers >= 2 else 1.0)
    insider_vol *= cluster_bonus

    # Net insider sentiment: if more informative sales than buys, suppress signal
    informative_sale_count = conn.execute(
        """SELECT COUNT(*) FROM filings WHERE cik = ? AND form_type = '4'
           AND trade_classification = 'informative_sale'
           AND file_date >= ? AND file_date <= ?""",
        (cik, date_30d, as_of_date)
    ).fetchone()[0]

    purchase_count_check = conn.execute(
        """SELECT COUNT(*) FROM filings WHERE cik = ? AND form_type = '4'
           AND transaction_code = 'P'
           AND file_date >= ? AND file_date <= ?""",
        (cik, date_30d, as_of_date)
    ).fetchone()[0]

    # If insiders are net selling, zero out the buy signal
    if informative_sale_count > purchase_count_check:
        insider_vol = 0.0

    # News momentum: total filings in last 14 days weighted by type
    form4_count = conn.execute(
        """SELECT COUNT(*) FROM filings
           WHERE cik = ? AND form_type = '4' AND file_date >= ? AND file_date <= ?""",
        (cik, date_14d, as_of_date)
    ).fetchone()[0]

    eightk_count = conn.execute(
        """SELECT COUNT(*) FROM filings
           WHERE cik = ? AND form_type = '8-K' AND file_date >= ? AND file_date <= ?""",
        (cik, date_14d, as_of_date)
    ).fetchone()[0]

    # Weight: 8-K is more newsworthy than Form 4
    filing_count_14d = form4_count + (eightk_count * 3)

    # Government/regulatory signal from 8-K events
    gov_events = conn.execute(
        """SELECT COUNT(*), COALESCE(MAX(event_severity), 0) FROM filings
           WHERE cik = ? AND form_type = '8-K'
           AND event_classification IN ('material_agreement', 'acquisition_completion', 'regulation_fd', 'earnings_results')
           AND file_date >= ? AND file_date <= ?""",
        (cik, date_90d, as_of_date)
    ).fetchone()
    gov_contract = gov_events[0] if gov_events else 0

    # 8-K event severity (max in last 14 days)
    event_severity = conn.execute(
        """SELECT COALESCE(MAX(event_severity), 0)
           FROM filings WHERE cik = ? AND form_type = '8-K'
           AND file_date >= ? AND file_date <= ?
           AND event_severity IS NOT NULL""",
        (cik, date_14d, as_of_date)
    ).fetchone()[0]

    # Source diversity: count distinct form types with filings in last 7 days
    date_7d = (datetime.strptime(as_of_date, "%Y-%m-%d") - timedelta(days=7)).strftime("%Y-%m-%d")
    source_diversity = conn.execute(
        """SELECT COUNT(DISTINCT form_type) FROM filings
           WHERE cik = ? AND file_date >= ? AND file_date <= ?""",
        (cik, date_7d, as_of_date)
    ).fetchone()[0]

    # Normalize signals
    insider_norm = normalize_signal(insider_vol, SCALES["insider"])
    news_norm = normalize_signal(filing_count_14d * 2.0, 20.0)  # Scale filing count
    contract_norm = normalize_signal(gov_contract, 3.0)
    reg_norm = normalize_signal(event_severity * 3.0, SCALES["government_regulatory"])
    gov_norm = min(contract_norm * 0.6 + reg_norm * 0.4, 1.0)
    political_norm = 0.0  # No lobbying data in historical
    patent_norm = 0.0     # No patent data in historical

    profile = {
        "insider": insider_norm,
        "news": news_norm,
        "government": gov_norm,
        "political": political_norm,
        "patent": patent_norm,
    }

    # Compound score
    compound = sum(profile.get(k, 0) * w for k, w in WEIGHTS.items())
    compound = max(0.0, min(1.0, compound))

    # Convergence rule depends on STRATEGY (set from --strategy):
    #
    #  "convergence" (original): trade when insider OR government fires. But the
    #     honest backtest showed insider is NOISE (p=0.805) and this rule never
    #     lets news (the only significant dim, p=0.006) trigger a trade alone.
    #
    #  "news" (Path A): trade led by news-momentum, the one significant signal.
    #     News must be strong; government can co-confirm. Insider is ignored as
    #     an entry trigger since it tested as noise.
    has_insider = profile.get("insider", 0) > 0.15
    has_gov = profile.get("government", 0) > 0.15
    has_news = profile.get("news", 0) > 0.30

    if STRATEGY == "news":
        # News-led: a meaningful news-momentum spike, optionally confirmed by gov.
        convergence = has_news or (has_gov and profile.get("news", 0) > 0.20)
    else:
        convergence = has_insider or (has_gov and compound >= 0.25)

    return {
        "compound": compound,
        "convergence": convergence,
        "profile": profile,
        "source_diversity": source_diversity,
        "cluster_buyers": distinct_buyers,
    }


def get_price(conn: sqlite3.Connection, ticker: str, date: str) -> Optional[float]:
    """Get closing price for a ticker on or before a date."""
    row = conn.execute(
        "SELECT close FROM daily_prices WHERE ticker = ? AND date <= ? ORDER BY date DESC LIMIT 1",
        (ticker, date)
    ).fetchone()
    return row[0] if row else None


def run_replay(conn: sqlite3.Connection, start_date: str, end_date: str,
               verbose: bool = False) -> list[ClosedTrade]:
    """Run day-by-day signal replay."""
    tickers = [r[0] for r in conn.execute(
        "SELECT DISTINCT ticker FROM companies WHERE ticker IN (SELECT DISTINCT ticker FROM daily_prices)"
    ).fetchall()]

    if not tickers:
        print("ERROR: No tickers with price data")
        return []

    print(f"Replaying {start_date} to {end_date} across {len(tickers)} tickers...")

    positions: dict[str, Position] = {}
    closed_trades: list[ClosedTrade] = []
    signals_checked = 0

    current = datetime.strptime(start_date, "%Y-%m-%d")
    end = datetime.strptime(end_date, "%Y-%m-%d")

    while current <= end:
        date_str = current.strftime("%Y-%m-%d")

        # Skip weekends
        if current.weekday() >= 5:
            current += timedelta(days=1)
            continue

        # 1. Evaluate open positions
        for ticker in list(positions.keys()):
            pos = positions[ticker]
            price = get_price(conn, ticker, date_str)
            if price is None:
                continue

            pos.days_held += 1
            pos.high_water_mark = max(pos.high_water_mark, price)
            pos.pnl_pct = ((price - pos.entry_price) / pos.entry_price) * 100

            close_reason = None
            if pos.pnl_pct <= STOP_LOSS_PCT:
                close_reason = "stop_loss"
            elif pos.days_held >= MAX_HOLD_DAYS:
                close_reason = "expired"

            if close_reason:
                closed_trades.append(ClosedTrade(
                    ticker=ticker,
                    entry_date=pos.entry_date,
                    exit_date=date_str,
                    entry_price=pos.entry_price,
                    exit_price=price,
                    pnl_pct=pos.pnl_pct - ROUND_TRIP_COST_PCT,
                    holding_days=pos.days_held,
                    signal_score=pos.signal_score,
                    signal_profile=pos.signal_profile,
                    exit_reason=close_reason,
                ))
                del positions[ticker]
                if verbose:
                    print(f"  [{date_str}] CLOSE {ticker}: {pos.pnl_pct:+.1f}% ({close_reason})")

        # 2. Macro regime check — don't enter longs during market crashes
        # Use any available broad-market ticker as proxy
        market_proxy = None
        for proxy_ticker in ["AXP", "ABT", "CAT", "AIG", "AFL"]:  # Large-cap proxies
            date_30d_ago = (current - timedelta(days=30)).strftime("%Y-%m-%d")
            p_now = get_price(conn, proxy_ticker, date_str)
            p_30d = get_price(conn, proxy_ticker, date_30d_ago)
            if p_now and p_30d and p_30d > 0:
                market_proxy = ((p_now - p_30d) / p_30d) * 100
                break

        market_crash = market_proxy is not None and market_proxy < -8.0

        # 3. Check for new signals (only check tickers with filings on this date)
        active_tickers = [r[0] for r in conn.execute(
            """SELECT DISTINCT ticker FROM filings
               WHERE file_date = ? AND ticker IS NOT NULL""",
            (date_str,)
        ).fetchall()]

        for ticker in active_tickers:
            if ticker in positions:
                continue  # Already holding
            if len(positions) >= 15:
                continue  # Max positions

            # Skip during market crashes unless very high conviction
            if market_crash:
                continue

            signals = compute_signals_for_ticker(conn, ticker, date_str)
            signals_checked += 1

            if not signals.get("convergence"):
                continue

            # Get next day's price for entry
            next_day = (current + timedelta(days=1)).strftime("%Y-%m-%d")
            entry_price = get_price(conn, ticker, next_day)
            if entry_price is None or entry_price <= 0:
                continue

            positions[ticker] = Position(
                ticker=ticker,
                entry_date=next_day,
                entry_price=entry_price,
                signal_score=signals["compound"],
                signal_profile=signals["profile"],
            )

            if verbose:
                print(f"  [{next_day}] OPEN {ticker}: score={signals['compound']:.2f}, "
                      f"insider={signals['profile']['insider']:.2f}, "
                      f"news={signals['profile']['news']:.2f}, "
                      f"gov={signals['profile']['government']:.2f}")

        current += timedelta(days=1)

    # Close remaining positions at last available price
    for ticker, pos in positions.items():
        price = get_price(conn, ticker, end_date)
        if price:
            pos.pnl_pct = ((price - pos.entry_price) / pos.entry_price) * 100
            closed_trades.append(ClosedTrade(
                ticker=ticker,
                entry_date=pos.entry_date,
                exit_date=end_date,
                entry_price=pos.entry_price,
                exit_price=price,
                pnl_pct=pos.pnl_pct - ROUND_TRIP_COST_PCT,
                holding_days=pos.days_held,
                signal_score=pos.signal_score,
                signal_profile=pos.signal_profile,
                exit_reason="end_of_period",
            ))

    print(f"  Signals checked: {signals_checked}")
    return closed_trades


def compute_metrics(trades: list[ClosedTrade]) -> dict:
    """Compute performance metrics from closed trades."""
    if not trades:
        return {"total": 0, "wins": 0, "losses": 0, "hit_rate": 0, "avg_return": 0,
                "avg_win": 0, "avg_loss": 0, "max_drawdown": 0, "sharpe": 0, "avg_hold_days": 0}

    returns = [t.pnl_pct for t in trades]
    wins = [r for r in returns if r > 0]
    losses = [r for r in returns if r <= 0]

    hit_rate = len(wins) / len(returns) if returns else 0
    avg_return = sum(returns) / len(returns) if returns else 0
    avg_win = sum(wins) / len(wins) if wins else 0
    avg_loss = sum(losses) / len(losses) if losses else 0
    max_drawdown = min(returns) if returns else 0

    # Sharpe ratio (annualized, assuming ~20 trading days/month)
    if len(returns) >= 2:
        mean_r = sum(returns) / len(returns)
        var_r = sum((r - mean_r) ** 2 for r in returns) / (len(returns) - 1)
        std_r = math.sqrt(var_r) if var_r > 0 else 1
        avg_hold = sum(t.holding_days for t in trades) / len(trades)
        trades_per_year = 252 / max(avg_hold, 1)
        sharpe = (mean_r / std_r) * math.sqrt(trades_per_year)
    else:
        sharpe = 0

    return {
        "total": len(trades),
        "wins": len(wins),
        "losses": len(losses),
        "hit_rate": hit_rate,
        "avg_return": avg_return,
        "avg_win": avg_win,
        "avg_loss": avg_loss,
        "max_drawdown": max_drawdown,
        "sharpe": sharpe,
        "avg_hold_days": sum(t.holding_days for t in trades) / len(trades) if trades else 0,
    }


def per_dimension_analysis(trades: list[ClosedTrade]) -> dict:
    """Compute hit rate per signal dimension."""
    dims = ["insider", "news", "government", "political", "patent"]
    result = {}

    for dim in dims:
        active_trades = [t for t in trades if t.signal_profile.get(dim, 0) > 0.2]
        if not active_trades:
            continue
        wins = sum(1 for t in active_trades if t.pnl_pct > 0)
        result[dim] = {
            "count": len(active_trades),
            "hit_rate": wins / len(active_trades),
            "avg_return": sum(t.pnl_pct for t in active_trades) / len(active_trades),
        }

    return result


def monte_carlo_test(trades: list[ClosedTrade], n_iter: int = 1000) -> dict:
    """Shuffle signal-to-return associations. If real hit rate > 95th percentile, signal is significant."""
    if len(trades) < 10:
        return {"overall_p_value": 1.0, "overall_significant": False, "dimensions": {}}

    real_returns = [t.pnl_pct for t in trades]
    real_hit_rate = sum(1 for r in real_returns if r > 0) / len(real_returns)

    shuffled_hit_rates = []
    for _ in range(n_iter):
        shuffled = real_returns.copy()
        random.shuffle(shuffled)
        # Assign shuffled returns to same signals
        hit_rate = sum(1 for r in shuffled if r > 0) / len(shuffled)
        shuffled_hit_rates.append(hit_rate)

    # p-value: fraction of shuffled results >= real result
    p_value = sum(1 for s in shuffled_hit_rates if s >= real_hit_rate) / n_iter

    # Per-dimension Monte Carlo
    dim_results = {}
    for dim in ["insider", "news", "government"]:
        dim_trades = [(t.signal_profile.get(dim, 0), t.pnl_pct) for t in trades]
        active = [(s, r) for s, r in dim_trades if s > 0.2]
        if len(active) < 5:
            continue

        real_corr = sum(s * (1 if r > 0 else -1) for s, r in active) / len(active)

        shuffled_corrs = []
        returns_only = [r for _, r in active]
        for _ in range(n_iter):
            random.shuffle(returns_only)
            corr = sum(s * (1 if r > 0 else -1) for (s, _), r in zip(active, returns_only)) / len(active)
            shuffled_corrs.append(corr)

        dim_p = sum(1 for s in shuffled_corrs if s >= real_corr) / n_iter
        dim_results[dim] = {"p_value": dim_p, "significant": dim_p < 0.10}

    return {
        "overall_p_value": p_value,
        "overall_significant": p_value < 0.05,
        "dimensions": dim_results,
    }


def walk_forward(conn: sqlite3.Connection, start_date: str, end_date: str,
                 window_days: int = 60) -> dict:
    """Walk-forward optimization: train on window N, test on window N+1."""
    print(f"\nWalk-forward analysis ({window_days}-day windows)...")

    start = datetime.strptime(start_date, "%Y-%m-%d")
    end = datetime.strptime(end_date, "%Y-%m-%d")
    total_days = (end - start).days

    in_sample_trades = []
    out_sample_trades = []

    window_start = start
    window_num = 0

    while window_start + timedelta(days=window_days * 2) <= end:
        train_end = window_start + timedelta(days=window_days)
        test_end = train_end + timedelta(days=window_days)

        train_trades = run_replay(conn, window_start.strftime("%Y-%m-%d"), train_end.strftime("%Y-%m-%d"))
        test_trades = run_replay(conn, train_end.strftime("%Y-%m-%d"), test_end.strftime("%Y-%m-%d"))

        in_sample_trades.extend(train_trades)
        out_sample_trades.extend(test_trades)

        train_m = compute_metrics(train_trades)
        test_m = compute_metrics(test_trades)

        print(f"  Window {window_num}: train={train_m['total']} trades ({train_m['hit_rate']:.0%}), "
              f"test={test_m['total']} trades ({test_m['hit_rate']:.0%})")

        window_start = test_end
        window_num += 1

    is_metrics = compute_metrics(in_sample_trades)
    oos_metrics = compute_metrics(out_sample_trades)

    return {
        "in_sample": is_metrics,
        "out_of_sample": oos_metrics,
        "windows": window_num,
    }


def print_report(trades: list[ClosedTrade], metrics: dict, dim_analysis: dict,
                 mc_results: Optional[dict] = None, wf_results: Optional[dict] = None):
    """Print comprehensive backtest report."""
    print(f"\n{'='*60}")
    print(f"  REPLAY ENGINE RESULTS")
    print(f"{'='*60}")
    print(f"  Total trades:     {metrics['total']}")
    print(f"  Wins / Losses:    {metrics['wins']} / {metrics['losses']}")
    print(f"  Hit rate:         {metrics['hit_rate']:.1%}")
    print(f"  Avg return:       {metrics['avg_return']:+.2f}%")
    print(f"  Avg win:          {metrics['avg_win']:+.2f}%")
    print(f"  Avg loss:         {metrics['avg_loss']:+.2f}%")
    print(f"  Max drawdown:     {metrics['max_drawdown']:.2f}%")
    print(f"  Sharpe ratio:     {metrics['sharpe']:.2f}")
    print(f"  Avg hold (days):  {metrics['avg_hold_days']:.0f}")

    if dim_analysis:
        print(f"\n  Per-Dimension Analysis:")
        for dim, stats in sorted(dim_analysis.items(), key=lambda x: -x[1]["hit_rate"]):
            print(f"    {dim:>12}: {stats['hit_rate']:.0%} hit rate "
                  f"({stats['avg_return']:+.1f}% avg, n={stats['count']})")

    if mc_results:
        print(f"\n  Monte Carlo Significance:")
        print(f"    Overall p-value:  {mc_results['overall_p_value']:.3f} "
              f"({'SIGNIFICANT' if mc_results['overall_significant'] else 'not significant'})")
        for dim, stats in mc_results.get("dimensions", {}).items():
            print(f"    {dim:>12} p:   {stats['p_value']:.3f} "
                  f"({'SIGNIFICANT' if stats['significant'] else 'not significant'})")

    if wf_results:
        oos = wf_results["out_of_sample"]
        print(f"\n  Walk-Forward ({wf_results['windows']} windows):")
        print(f"    In-sample hit rate:  {wf_results['in_sample']['hit_rate']:.1%}")
        print(f"    Out-of-sample:       {oos['hit_rate']:.1%}")
        print(f"    OOS Sharpe:          {oos['sharpe']:.2f}")

    # GO/NO-GO assessment
    oos_hr = wf_results["out_of_sample"]["hit_rate"] if wf_results else metrics["hit_rate"]
    sig_dims = sum(1 for d in (mc_results or {}).get("dimensions", {}).values() if d.get("significant"))

    print(f"\n{'='*60}")
    print(f"  GO/NO-GO GATE ASSESSMENT")
    print(f"{'='*60}")
    checks = [
        (metrics["total"] >= 50, f"  [{'PASS' if metrics['total'] >= 50 else 'FAIL'}] 50+ trades: {metrics['total']}"),
        (oos_hr > 0.52, f"  [{'PASS' if oos_hr > 0.52 else 'FAIL'}] Hit rate > 52%: {oos_hr:.1%}"),
        (metrics["sharpe"] > 0.5, f"  [{'PASS' if metrics['sharpe'] > 0.5 else 'FAIL'}] Sharpe > 0.5: {metrics['sharpe']:.2f}"),
        (sig_dims >= 2, f"  [{'PASS' if sig_dims >= 2 else 'FAIL'}] 2+ significant dimensions: {sig_dims}"),
    ]
    for passed, msg in checks:
        print(msg)

    all_pass = all(p for p, _ in checks)
    print(f"\n  VERDICT: {'GO — proceed to Phase 5' if all_pass else 'NO-GO — improve signal quality first'}")
    print(f"{'='*60}")


def main():
    parser = argparse.ArgumentParser(description="Pulse Replay Engine")
    parser.add_argument("--db", default=DB_PATH, help="Historical database path")
    parser.add_argument("--start", default=None, help="Start date (YYYY-MM-DD)")
    parser.add_argument("--end", default=None, help="End date (YYYY-MM-DD)")
    parser.add_argument("--verbose", action="store_true")
    parser.add_argument("--walk-forward", action="store_true", help="Run walk-forward analysis")
    parser.add_argument("--monte-carlo", type=int, default=0, help="Monte Carlo iterations")
    parser.add_argument("--window", type=int, default=60, help="Walk-forward window (days)")
    parser.add_argument("--strategy", choices=["convergence", "news"], default="convergence",
                        help="Entry strategy: convergence (8-signal fusion) or news (news-led)")
    args = parser.parse_args()

    global STRATEGY
    STRATEGY = args.strategy
    print(f"Strategy: {STRATEGY}")

    if not os.path.exists(args.db):
        print(f"ERROR: Database not found: {args.db}")
        print(f"Run bootstrap_historical.py first.")
        sys.exit(1)

    conn = sqlite3.connect(args.db)

    # Auto-detect date range from data
    if not args.start:
        row = conn.execute("SELECT MIN(file_date) FROM filings WHERE file_date IS NOT NULL").fetchone()
        args.start = row[0] if row[0] else "2024-04-01"
    if not args.end:
        row = conn.execute("SELECT MAX(file_date) FROM filings WHERE file_date IS NOT NULL").fetchone()
        args.end = row[0] if row[0] else datetime.now().strftime("%Y-%m-%d")

    print(f"Database: {args.db}")
    filing_count = conn.execute("SELECT COUNT(*) FROM filings").fetchone()[0]
    price_count = conn.execute("SELECT COUNT(*) FROM daily_prices").fetchone()[0]
    print(f"Filings: {filing_count}, Price candles: {price_count}")

    # Run main replay
    trades = run_replay(conn, args.start, args.end, verbose=args.verbose)
    metrics = compute_metrics(trades)
    dim_analysis = per_dimension_analysis(trades)

    # Optional: Monte Carlo
    mc_results = None
    if args.monte_carlo > 0 and trades:
        print(f"\nRunning Monte Carlo ({args.monte_carlo} iterations)...")
        mc_results = monte_carlo_test(trades, args.monte_carlo)

    # Optional: Walk-forward
    wf_results = None
    if args.walk_forward:
        wf_results = walk_forward(conn, args.start, args.end, args.window)

    # Print report
    print_report(trades, metrics, dim_analysis, mc_results, wf_results)

    # Save results to DB
    conn.execute(
        """CREATE TABLE IF NOT EXISTS replay_results (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            start_date TEXT, end_date TEXT,
            total_trades INTEGER, hit_rate REAL, sharpe REAL,
            avg_return REAL, max_drawdown REAL,
            details TEXT,
            created_at TEXT DEFAULT (datetime('now'))
        )"""
    )
    conn.execute(
        """INSERT INTO replay_results (start_date, end_date, total_trades, hit_rate, sharpe, avg_return, max_drawdown, details)
           VALUES (?, ?, ?, ?, ?, ?, ?, ?)""",
        (args.start, args.end, metrics["total"], metrics["hit_rate"], metrics["sharpe"],
         metrics["avg_return"], metrics["max_drawdown"], json.dumps({
             "metrics": metrics,
             "dimensions": dim_analysis,
             "monte_carlo": mc_results,
             "walk_forward": wf_results,
         }))
    )
    conn.commit()
    conn.close()


if __name__ == "__main__":
    main()
