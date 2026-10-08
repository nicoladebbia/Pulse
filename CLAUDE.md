# Pulse — Daily Intelligence Briefing

## What This Is
macOS dock app that delivers AI-curated daily news at 8 AM. Tauri 2.0 + SvelteKit + Tailwind.

## Architecture
- `src/` — SvelteKit frontend (Svelte 5, Tailwind CSS v4)
- `src-tauri/` — Rust backend (Tauri 2.0, rusqlite, sqlite-vec)
- `crates/pulse-fetcher/` — Standalone fetch binary (runs via launchd at 8 AM)
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
- Each held position keeps a GTC sell-stop at Alpaca (`crates/pulse-alpaca/src/stops.rs`); any other sell must cancel it first.
- New buys pass `crates/pulse-fetcher/src/entry_filters.rs`: $5 price, $10M/day dollar volume (SIP bars), no earnings within 3 trading days, 30% per industry, 40 positions max, half size when SPY < 50-day average, and a 14-day ATR of at least 3% of price (the signal has only paid on stocks that move; backtest notes in the file).
- Signals page extras: every auto-trade run logs why each candidate was or wasn't bought to `trade_decisions` (`crates/pulse-fetcher/src/decisions.rs`, kept 180 days); trading runs stamp SPY's price on trades bought/sold that day (`spy_entry_price` / `spy_exit_price`) for the "vs the S&P 500" card (`src-tauri/src/services/benchmark.rs`); `--mode scorecard` (weekly, from `scheduled-fetch.sh`) writes `signal_scorecard`: every buy-grade signal of the last 120 days held 10 trading days vs SPY.
- `--mode learn` (daily from 17:00, `scripts/scheduled-fetch.sh`; `crates/pulse-fetcher/src/learning.rs`) reviews every closed trade into `trade_reviews` (return vs SPY, best/worst point, price 5/10/20 days after the exit, lesson tags, a plain story), scores each signal source and each auto-trade filter on what its signals did over the next 10 days, and stores the "What Pulse learned" report in `learning_reports`. A source with |t| >= 2 on 30+ signals moves its weight 10%, at most weekly, within 0.5x–1.5x of its default, never reviving a zeroed one; `LEARNING_AUTO_APPLY=false` only reports. Each buy stores `paper_trades.entry_context` (score, rank, dims, weights, ATR, liquidity, regime, sizing, rules version `RULES_VERSION`) and every booking appends to `trade_events`.
- Test runs place real orders. Use `AUTO_TRADE_ENABLED=false EXIT_DRY_RUN=true` (and `--db-path <copy>` to keep off the live DB; there is no DB-path env var), and `AUTO_TRADE_PREVIEW=true` to see what auto-trade would buy without sending orders.

## Releases & updates
- `git tag v0.2.0 && git push --tags` → `.github/workflows/release.yml` builds the macOS app, signs the update (secrets `TAURI_SIGNING_PRIVATE_KEY` / `_PASSWORD`, key at `~/.tauri/pulse-updater.key`) and publishes `latest.json`. The tag is the version and must go up each release.
- Installed apps check every 4h and show an "Update to vX" button in the sidebar (`src/lib/updater.ts`).
- `scripts/install-app.sh` installs the latest release (or a local `Pulse.app`) into /Applications and schedules the 08:00 / 21:00 briefings (`scripts/scheduled-fetch.sh`) and the live signals (`scripts/live-signals.sh`). Re-run it (or copy both scripts and plists by hand) after a release that changes them. Installed builds read `.env` from `~/Library/Application Support/com.pulse.app/.env`.
