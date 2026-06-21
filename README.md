# Pulse

> A macOS desktop app that fetches, summarizes, and connects the day's news into an AI-curated intelligence briefing — then turns the signals into trends, predictions, and a paper-trading sandbox.

Pulse runs a scheduled fetch pipeline twice a day, pulls from a wide set of free public data sources, summarizes stories with Claude, embeds them for semantic search, and surfaces what's accelerating across sectors. The desktop app is a magazine-style reader with a RAG chat ("Ask"), entity trend tracking, calibrated predictions, and a paper-trading layer driven by the same signals.

## What it does

- **Daily briefing** — A curated, sector-organized briefing generated from the latest fetch, with per-story summaries, "why it matters", key facts, and cross-sector connection insights.
- **Multi-source fetch pipeline** — Concurrently collects from ~16 free sources (see below), deduplicates, summarizes with Claude, and stores everything in SQLite.
- **Ask (RAG chat)** — Streaming chat grounded in the story corpus, with retrieval, reranking, source citations, follow-ups, and thumbs-up/down feedback that feeds a source-reputation model.
- **Signals & Trends** — Extracts named entities from stories, tracks 7/30/90-day mention windows, computes acceleration, and labels trajectories (rising / hot / dominant / fading / dormant).
- **Cross-sector signals** — Convergence alerts and signal evidence linking entities across sectors, with live and historical price context.
- **Predictions** — Forward-looking predictions with validation, calibration stats, and automatic expiry of stale predictions.
- **Paper trading** — A paper-trading portfolio with trade execution, position management, exit evaluation, backtesting, a trade journal, and portfolio analytics. (Paper only — driven by signals, no real orders.)
- **Four Freedoms** — A separate themed pipeline producing per-freedom briefing sections.
- **Full-text search & archive** — FTS over the story corpus plus a browsable briefing archive.

## Tech stack

- **Desktop shell:** [Tauri 2](https://tauri.app/) (Rust backend)
- **Frontend:** SvelteKit (Svelte 5 runes) + Tailwind CSS v4, static adapter, TypeScript, Vite 6
- **Backend (`src-tauri/`):** Rust, `rusqlite` (bundled SQLite), Tauri commands organized by domain (briefing, stories, chat, trends, predictions, cross-signals, trading, usage)
- **Fetch pipeline (`crates/pulse-fetcher/`):** Standalone Rust binary run on a schedule, async via Tokio + reqwest, `feed-rs` for RSS
- **AI:** Anthropic Claude (Haiku 4.5 for story summaries / entity extraction, Sonnet 4.6 for cross-sector analysis); Voyage `voyage-3-lite` embeddings for semantic search
- **Storage:** SQLite with FTS and vector embeddings; schema managed by numbered SQL migrations in `migrations/`
- **Scheduling:** macOS `launchd` agent (`com.pulse.daily-fetch`) at 08:00 and 21:00

## Data sources

The fetcher collects from free public APIs and feeds, all non-fatal (a failed source is logged and skipped):

Google News RSS, direct RSS feeds, Hacker News, Reddit, arXiv, bioRxiv, USASpending, Federal Register, SEC EDGAR, FRED, FEC, EIA, LDA lobbying disclosures, USPTO patents, SBIR, Wikipedia.

## Project structure

```
src/                      SvelteKit frontend (routes: briefing, ask, signals, trends, predictions, ideas, journal, archive, freedoms)
src-tauri/                Tauri 2 Rust backend (commands/, services/, db/, models/)
crates/pulse-fetcher/     Standalone fetch + summarize + embed pipeline binary
migrations/               Numbered SQLite schema migrations
launchd/                  macOS launchd plist for scheduled fetches
scripts/                  Sidecar build + launchd install/uninstall + Python backtester/replay tools
eval/                     Retrieval/chat eval cases
```

## Setup

Requirements: macOS, Rust toolchain, Node + [pnpm](https://pnpm.io/).

1. **Install frontend dependencies**
   ```bash
   pnpm install
   ```

2. **Configure API keys** — copy `.env.example` to `.env` and fill in:
   ```bash
   cp .env.example .env
   ```
   ```
   ANTHROPIC_API_KEY=sk-ant-...
   VOYAGE_API_KEY=pa-...
   CURRENTS_API_KEY=...
   ```
   The fetcher and the app load keys from `~/Projects/Pulse/.env` at runtime.

## Run

```bash
pnpm tauri dev                 # Run the desktop app in dev mode (frontend + Tauri)
pnpm tauri build               # Production app build
```

### Fetch pipeline

```bash
cargo build -p pulse-fetcher                  # Build the fetcher (debug)
cargo build --release -p pulse-fetcher        # Build for the scheduled agent
./target/debug/pulse-fetcher --mode daily     # Manual fetch
./target/debug/pulse-fetcher --mode daily --force   # Force re-fetch even if a briefing exists
```

The fetcher supports additional modes, including: `daily`, `freedoms`, `manual`, `backfill-embeddings`, `extract-entities`, `fetch-form4`, `backfill-tickers`, and `manage-positions`.

### Scheduled fetches (launchd)

```bash
./scripts/build-sidecar.sh        # Build the release fetcher + stage the Tauri sidecar binary
./scripts/install-launchd.sh      # Install the launchd agent (runs at 08:00 and 21:00)
./scripts/uninstall-launchd.sh    # Remove the agent
```

### Type checking

```bash
pnpm check                        # svelte-kit sync + svelte-check
```

## Status

Personal project, actively developed. Versioned `0.1.0`. macOS-only. The trading layer is **paper trading only** — it simulates positions from signals and does not place real orders. API keys are required for the fetch pipeline, chat, embeddings, and predictions to function.
