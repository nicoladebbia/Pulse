# Trading System — Status & How To Read It

**Auto-buy is OFF and should stay OFF.** Last verified NO-GO: 2026-06-17.
Do not re-enable live trading without first re-running the backtest below and
seeing it PASS the gate. Markdown numbers go stale — always run the live source.

## How to read the live verdict (do NOT trust numbers pasted here)

```bash
# Full 2-year historical backtest, both strategies + significance:
python3 scripts/replay_engine.py --strategy news        --monte-carlo 2000
python3 scripts/replay_engine.py --strategy convergence --monte-carlo 2000
# The honest out-of-sample test (this is the one that matters):
python3 scripts/replay_engine.py --strategy news        --walk-forward
```

GO requires ALL of: 50+ trades, hit rate >52%, Sharpe >0.5, 2+ significant dims.

## Why it's NO-GO (as of 2026-06-17, re-measured on full data)

- News-momentum is the only signal that ever tests significant (p=0.006 under
  the convergence strategy, n=25). Insider is noise (p≈0.80). Everything else
  is zeroed or absent in the historical data.
- **Walk-forward OOS hit rate collapses to 34%** (in-sample 60.6%), OOS Sharpe
  −0.83. That is overfitting: it looks good on data it was tuned on, loses money
  on data it hasn't seen. One in-sample-significant dimension ≠ a tradeable edge.
- Net result is a loss after the 20bps round-trip cost deducted in replay_engine.

## The real blocker (this is what "improve signal quality" actually needs)

`pulse_historical.db` contains **SEC filings only** (Form 4, 8-K). The live
system computes 8 signal dimensions, but the backtest can only reconstruct ~3,
and 2 of those (`political`, `patent`) are hardcoded to 0.0 (see
`compute_signals_for_ticker` in replay_engine.py, lines ~237-238). The richer
live signals — real news momentum, search-trend deltas, lobbying spend, patent
filings, institutional flow — have **no historical counterpart**, so they cannot
be validated. You can't prove (or disprove) an edge you have no history for.

**Next data project, not a tuning tweak:** extend `bootstrap_historical.py` to
backfill the non-SEC signal sources over 2+ years, then re-run the gate. Only
then is there new information that could flip the NO-GO. Until that data exists,
the signals are research/watchlist intelligence — informative to read, not to
trade.

## Ledger state (live pulse.db `paper_trades`)

- 17 rows → 9 after removing 8 phantom duplicate rows (pre-fix launchd retry
  storm; the duplicate-order bug is already fixed via deterministic
  `client_order_id` in pipeline.rs, commit 4ed1846). Backup:
  `/tmp/paper_trades_pre_dedup_20260617.sql`.
- P&L dollar column was NULL/0 on positions that closed between fetcher runs
  (pipeline.rs reconciliation path didn't set `pnl`). Fixed forward + backfilled
  recoverable rows.
- 2 INTC rows have corrupt `position_size` (0.01 / 0.19) recorded at entry — the
  +67% gains are real but dollar P&L is unrecoverable. Left flagged, not faked.
- Corrected net on the 7 recoverable closed trades: ≈ −$116. Still a loss on a
  tiny sample. The cleanup made the ledger honest; it did not produce an edge.
