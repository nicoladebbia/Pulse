# Pulse — Daily Intelligence Briefing

## What This Is
macOS dock app that delivers AI-curated daily news at 8 AM. Tauri 2.0 + SvelteKit + Tailwind.

## Architecture
- `src/` — SvelteKit frontend (Svelte 5, Tailwind CSS v4)
- `src-tauri/` — Rust backend (Tauri 2.0, rusqlite, sqlite-vec)
- `crates/pulse-fetcher/` — Standalone fetch binary (runs via launchd at 8 AM)
- `crates/pulse-snapshot/` — Builds the phone snapshot (risk logic lives in `risk.rs`; `src-tauri/src/services/risk.rs` re-exports it); `crates/pulse-calendar/` — earnings + macro calendar view
- `ios/` — SwiftUI companion (XcodeGen: `cd ios && xcodegen generate`; read-only, iOS 17). Signing uses the team in `ios/project.yml` (`DEVELOPMENT_TEAM`); change it there if it is not yours.
- `migrations/` — SQLite schema migrations

## Commands
```bash
pnpm install                    # Install frontend deps
pnpm tauri dev                  # Dev mode (frontend + Tauri)
pnpm tauri build                # Production build
cargo build -p pulse-fetcher    # Build fetcher only
./target/debug/pulse-fetcher --mode daily  # Manual fetch test
```

## Key Decisions
- Fetch pipeline models: llama-3.1-8b-instant (Groq) for story/exec summaries, llama-3.3-70b-versatile (Groq) for pre-curation + freedoms analysis, Claude Haiku 4.5 (Anthropic) for the daily cross-sector `analyze` (falls back to Groq 70B on error; `PULSE_ANALYZE_PROVIDER=groq` forces the old path). Anthropic also powers in-app chat, contextual prefixes, entity extraction, and predictions. The `claude/` module name predates the Groq migration.
- SQLite + sqlite-vec for storage and vector search
- Voyage-3-lite for embeddings (512 dims)
- All news sources are free APIs (Google News RSS, HN, direct RSS)
- Magazine layout, dark mode only, vim-style keyboard shortcuts

## Crash Prevention Rules
- **NEVER use `.unwrap()` on `partial_cmp()`** — f64 NaN values return None. Always use `.unwrap_or(std::cmp::Ordering::Equal)`.
- **NEVER use bare `.unwrap()` in Tauri commands** — a panic kills the app. Use `.map_err()`, `.unwrap_or()`, or `.ok()`.
- **All Tauri command errors must be `Result<T, String>`** — never panic, always return Err.
- **Frontend must `.catch()` every Tauri invoke** — an unhandled rejection crashes the webview.
- **Test nullable DB columns** — use `Option<T>` for any column that can be NULL, even if a WHERE clause filters NULLs.

## Environment Variables
Required in `.env`: ANTHROPIC_API_KEY, VOYAGE_API_KEY, GROQ_API_KEY — or `PULSE_LLM=local` to run every AI call on Ollama with no keys (`crates/pulse-llm`, `scripts/setup-local-ai.sh`). In local mode `PULSE_CLOUD_TASKS=summarize` (comma list of `GroqClient::call` endpoints, or `all`) still sends those tasks to Groq with the real `GROQ_API_KEY`, falling back to Ollama on error. New AI call sites must go through `pulse_llm` (`messages_url`, `messages_body`, `api_key`, `embeddings_body`) or they will silently bypass local mode.

## Trading (Alpaca paper)
- All orders go to `paper-api.alpaca.markets` only. Keys: `ALPACA_API_KEY` / `ALPACA_SECRET_KEY`.
- Prices (daily quotes, Signals refresh, live stream) come from Alpaca's free IEX feed via `crates/pulse-alpaca` when those keys are set; Finnhub is the fallback. Without Finnhub the auto-trade universe gate uses the Alpaca-only rule (`eligible_without_market_cap`).
- Manual Buy/Close use `client_order_id`s (`pulse-manual-…`, `pulse-close-…`) so double clicks can't double-order.
- `scripts/scheduled-fetch.sh` runs `--mode manage-positions` hourly and `--mode auto-trade` twice a day (from 10:00 and from 13:00 ET) during US market hours. Buys only happen while the market is open, in whole shares.
- `scripts/live-signals.sh` (launchd `com.pulse.live-signals`, minutes 5–50) runs `--mode live-signals` every 5 minutes while the market is open: targeted Form 4s (250 strongest names, a quarter per run so each is checked every 20 minutes), EDGAR Form 4 / 8-K, the other financial sources once an hour (`--full-sources`), then signals and cross-signal scores. No AI calls; news momentum still comes from the briefings. From 10:00 to 15:30 ET (`--live-trade`) a stock that newly turns buy-grade runs auto-trade right away. It shares the fetcher lock and yields to a running briefing.
- Unknown `--mode` values are an error (they used to run the full pipeline); new schedulers check the binary supports a mode before calling it.
- Data-source fetches log one `api_usage` row per source per run with the request count in `calls`; count calls with `SUM(calls)`, not `COUNT(*)`. In local AI mode only AI providers are logged as `local`.
- Each held position keeps a GTC sell-stop at Alpaca (`crates/pulse-alpaca/src/stops.rs`); any other sell must cancel it first. A short keeps a GTC buy-stop above the price instead (`Side::Buy`), tightened as the price falls and never loosened.
- Exits: signal decay may close a convergence trade only after `DECAY_MIN_HELD_DAYS` (5) trading days (`position_management::decay_allowed`); every trade has a maximum hold by signal type (`event_signals::max_hold_days`: convergence 60 trading days, news 5, insider cluster 20, director buy 60). Stops are deliberately not tightened with age (flat ATR multiple). Stop distances come from `position_management::ExitRules::for_trigger`: 3x ATR / -15% hard / -10% without ATR / half at 3x ATR for everything except `insider_director_buy` (6x ATR / -30% / -20%, no half close); the broker stop, the exit check, risk sizing and the app's trade page all use them.
- Shorts: short event signals are sold short (whole shares, `paper_trades.direction = 'short'`, Alpaca qty is negative) when the account has `shorting_enabled` and the asset is `shortable` and `easy_to_borrow`; `SHORTS_ENABLED=false` keeps the bot long-only. Exits mirror longs (`evaluate_short`: trailing stop 3x ATR above the low kept in `high_water_mark`, +15% hard stop, half covered at 3x ATR profit, plus the event's max hold), and every P&L booking reads the row's direction (`trade_sign`).
- New buys pass `crates/pulse-fetcher/src/entry_filters.rs`: $5 price, $10M/day dollar volume (SIP bars), no earnings within 3 trading days, 30% per industry, 40 positions max, half size when SPY < 50-day average, and a 14-day ATR of at least 3% of price (the signal has only paid on stocks that move; backtest notes in the file; director buys skip this floor).
- Signals page extras: every auto-trade run logs why each candidate was or wasn't bought to `trade_decisions` (`crates/pulse-fetcher/src/decisions.rs`, kept 180 days); trading runs stamp SPY's price on trades bought/sold that day (`spy_entry_price` / `spy_exit_price`) for the "vs the S&P 500" card (`src-tauri/src/services/benchmark.rs`); `--mode scorecard` (weekly, from `scheduled-fetch.sh`) writes `signal_scorecard`: every buy-grade signal of the last 120 days held 10 trading days vs SPY.
- `--mode learn` (daily from 17:00, `scripts/scheduled-fetch.sh`; `crates/pulse-fetcher/src/learning.rs`) reviews every closed trade into `trade_reviews` (return vs SPY, best/worst point, price 5/10/20 days after the exit, lesson tags, a plain story), scores each signal source and each auto-trade filter on what its signals did over the next 10 days, and stores the "What Pulse learned" report in `learning_reports`. A source with |t| >= 2 on 30+ signals moves its weight 10%, at most weekly, within 0.5x–1.5x of its default, never reviving a zeroed one; `LEARNING_AUTO_APPLY=false` only reports. Each buy stores `paper_trades.entry_context` (score, rank, dims, weights, ATR, liquidity, regime, sizing, rules version `RULES_VERSION`) and every booking appends to `trade_events`.
- Event signals (`crates/pulse-fetcher/src/event_signals.rs`, table `event_signals`) are a second way in besides convergence: `news_surprise` (today's AI news tone vs the ticker's last 30 days, |change| >= 0.5) and `insider_director_buy` (an outside director — not an officer or 10% owner, `is_ten_percent_owner` parsed from Form 4s — buying $10k+, filed in the last 3 days). `insider_cluster` is no longer detected (no edge 2018-2026); open ones keep their 20-day hold. Detected at each auto-trade and learn run, recorded whether traded or not (a later reading the same day replaces the first), bought at half the smallest entry size (`SIZING_SCORE`) through the same entry filters, never scaled into, never signal-decayed, closed after `max_hold_days` (5 / 60 trading days; `paper_trades.entry_trigger`). News events are only traded with `NEWS_TRADES_ENABLED=true` (no backtest edge; `candidates` leaves them out). `EVENT_TRADES_ENABLED=false` stops all event trades. The learning report scores every event. Backtests: `research/event-backtest/`.
- Test runs place real orders. Use `AUTO_TRADE_ENABLED=false EXIT_DRY_RUN=true` (and `--db-path <copy>` to keep off the live DB; there is no DB-path env var), and `AUTO_TRADE_PREVIEW=true` to see what auto-trade would buy without sending orders.

- Go-live gate (`src-tauri/src/services/go_live.rs`, Signals page card, `get_go_live_gate`): fixed 7-point checklist (100+ closed trades, 90+ days, net profit, edge over SPY with t >= 2 on 30+ compared trades, profit factor >= 1.3, drawdown <= 15%, every open position has a broker stop; closes with no exit price and trades marked as tests do not count, and no account history fails the drawdown check). Thresholds are constants written down in advance; do not tune them to a result. It only reports; nothing switches to real money by itself.
- Exit safety: right before a sell, `manage_pass` re-reads the broker position (`recheck_close_qty`) so a sell never covers shares that are gone; a sell POST with no reply is looked up by `client_order_id` before the stop is restored; `record_full_close` and the app's Close button store the WHOLE trade's `pnl_pct` (`pulse_snapshot::ledger::whole_trade_pnl_pct`, from the `trade_events` legs), not the last leg's. New buys get their stop right away (`protect_new_positions`, and a second manage pass when a pending entry fills).

## Risk, Calendar, Backtest, Phone
- Risk page: `get_risk_report` (exposure, stops, beta over `BETA_LOOKBACK_DAYS`, stress test, drawdown) reads live Alpaca account/positions. The same logic builds the phone's risk card.
- Calendar page: `earnings_calendar` (migration 047) holds the followed tickers' reports, refreshed at most every 6h by `calendar::refresh_if_stale` (runs from the fetcher's `daily`, `learn` and hourly `--full-sources` runs). Finnhub caps a response at 1,500 rows and drops the EARLIEST days, so every fetch goes through `calendar::collect`, which splits full windows (it counts the feed's raw entries, not parsed rows). The trader's earnings blackout (`entry_filters::upcoming_earnings`) uses the same path. FOMC/CPI/PPI/jobs dates are a hardcoded table in `crates/pulse-calendar` (BLS blocks scraping); a test fails when fewer than 90 days remain, so extend it from the official schedules.
- Backtest page: `run_backtest` is async on its own connection; SPY comparison uses the paged `benchmark::spy_bars` (history past 1,000 bars).
- Phone snapshot: `--mode snapshot` (also run after `daily`, `manage-positions`, `auto-trade`, `learn` and the hourly live-signals run, never failing them) writes `pulse-snapshot.json` atomically to `PULSE_SNAPSHOT_DIR` (default `~/Library/Mobile Documents/com~apple~CloudDocs/Pulse`); `PULSE_SNAPSHOT=off` disables it. If Alpaca is unreachable the previous copy's account figures are kept and marked `account_as_of` (up to 72h). The contract is `crates/pulse-snapshot/tests/fixtures/snapshot_v1.json`, decoded by both the Rust and the Swift tests; change it with `UPDATE_FIXTURE=1 cargo test -p pulse-snapshot` and bump `SCHEMA_VERSION` for breaking changes. The app picks the iCloud Drive `Pulse` folder once (security-scoped bookmark), so it works with free signing.

## Releases & updates
- `git tag v0.2.0 && git push --tags` → `.github/workflows/release.yml` builds the macOS app, signs the update (secrets `TAURI_SIGNING_PRIVATE_KEY` / `_PASSWORD`, key at `~/.tauri/pulse-updater.key`) and publishes `latest.json`. The tag is the version and must go up each release.
- Installed apps check every 4h and show an "Update to vX" button in the sidebar (`src/lib/updater.ts`).
- `scripts/install-app.sh` installs the latest release (or a local `Pulse.app`) into /Applications and schedules the 08:00 / 21:00 briefings (`scripts/scheduled-fetch.sh`) and the live signals (`scripts/live-signals.sh`). Re-run it (or copy both scripts and plists by hand) after a release that changes them. Installed builds read `.env` from `~/Library/Application Support/com.pulse.app/.env`.
- Trade explanations (`crates/pulse-snapshot/src/explain.rs`): every open and closed trade gets a plain-language account (why bought, size, exit plan, how it is going) built from `signal_profile` / `entry_context` / `entry_trigger`, with no AI call. It is the `get_trade_explanations` Tauri command (Signals page cards) and the `explanation` field of each snapshot position / closed trade (phone Portfolio tab). `explain::rules_for` repeats the exit rules in words; a test in `position_management.rs` fails if it drifts from `ExitRules` / `max_hold_days`. Institutional and supply-chain signals are never given as a reason (zero weight).
