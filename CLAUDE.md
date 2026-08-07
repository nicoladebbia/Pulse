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
Required in `.env`: ANTHROPIC_API_KEY, VOYAGE_API_KEY, GROQ_API_KEY
