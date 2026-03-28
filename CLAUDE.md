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
- Haiku 4.5 for story summaries, Sonnet 4.6 for cross-sector analysis
- SQLite + sqlite-vec for storage and vector search
- Voyage-3-lite for embeddings (512 dims)
- All news sources are free APIs (Google News RSS, HN, direct RSS, Currents)
- Magazine layout, dark mode only, vim-style keyboard shortcuts

## Environment Variables
Required in `.env`: ANTHROPIC_API_KEY, VOYAGE_API_KEY, CURRENTS_API_KEY
