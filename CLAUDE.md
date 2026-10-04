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
- `scripts/scheduled-fetch.sh` runs `--mode manage-positions` hourly and `--mode auto-trade` once a day during US market hours (10:00–15:59 ET, weekdays).

## Releases & updates
- `git tag v0.2.0 && git push --tags` → `.github/workflows/release.yml` builds the macOS app, signs the update (secrets `TAURI_SIGNING_PRIVATE_KEY` / `_PASSWORD`, key at `~/.tauri/pulse-updater.key`) and publishes `latest.json`. The tag is the version and must go up each release.
- Installed apps check every 4h and show an "Update to vX" button in the sidebar (`src/lib/updater.ts`).
- `scripts/install-app.sh` installs the latest release (or a local `Pulse.app`) into /Applications and schedules the 08:00 / 21:00 briefings (`scripts/scheduled-fetch.sh`). Installed builds read `.env` from `~/Library/Application Support/com.pulse.app/.env`.
