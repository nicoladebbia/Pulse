# Pulse

**A local-first news-intelligence engine in Rust: a hybrid (BM25 + dense-vector) RAG pipeline with HyDE query expansion, two-stage reranking, and query-class-aware recency decay — shipped as a single Tauri 2 desktop app over a 22k-document SQLite corpus.**

The interesting part of Pulse is not "an app that summarizes news." It's the retrieval layer: a multi-stage RAG pipeline written in Rust that fuses keyword and semantic search, rewrites and decomposes queries with an LLM, reranks with a cross-encoder, decays results by recency *differently depending on what kind of question was asked*, and degrades gracefully when any external model call fails — all running in-process against a local SQLite database with no vector-database dependency.

---

## The problem

Retrieval-augmented generation over a news archive is hard for reasons that don't show up in a toy demo:

- **Keyword search and semantic search fail on opposite queries.** BM25 nails `"NVDA Q3 earnings"` and whiffs on `"chip makers facing supply pressure"`; dense vectors do the reverse. You need both, fused, without one channel's noise drowning the other.
- **Recency means different things for different questions.** `"what happened today"` should bury a relevant-but-old story; `"compare GPT-4 vs Claude over the last year"` should *not*. A single recency weight is wrong for both.
- **Every quality stage is a network call to a flaky external model.** Query rewrite, embedding, reranking, and synthesis each hit a third-party API. Any one can time out. A naive pipeline turns one slow API into a hung UI or a crashed desktop app.

Pulse's retrieval layer is built around these three facts.

---

## Architecture

```
                          ┌─────────────────────── SvelteKit (Svelte 5 runes) ───────────────────────┐
                          │  Ask (RAG chat)   Briefing   Signals/Trends   Predictions   Paper journal │
                          └───────────────────────────────┬──────────────────────────────────────────┘
                                              Tauri IPC (typed commands, streaming Channel<T>)
┌─────────────────────────────────────────────────────────┴───────────────────────────────────────────┐
│ src-tauri/  — Rust backend (56 Tauri commands, all Result<T, String>, panic-free)                     │
│                                                                                                       │
│   Ask query ─▶ classify query type ─▶ ┌─ Haiku rewrite + HyDE + decompose ─┐ (parallel)               │
│                (Breaking/Analytical/   └─ Voyage embed raw query ───────────┘                          │
│                 Comparative/Advisory)            │                                                     │
│                                                  ▼                                                     │
│              entity/graph FTS expansion ─▶ HYBRID RETRIEVE ──┬─ BM25 FTS5 (field-weighted)            │
│                                                              └─ brute-force cosine over embeddings     │
│                                                  │                                                     │
│              score-weighted RRF ─▶ query-class recency decay ─▶ 2-stage rerank ─▶ web fallback        │
│                                                  │              (Voyage rerank-2-lite                  │
│                                                  ▼               → Haiku → original order)             │
│                                          stream synthesis (Sonnet) ─▶ Channel<ChatStreamEvent>        │
│                                                                                                       │
│   SQLite (rusqlite, bundled) — stories + FTS5 + 512-dim embedding blobs + entity graph                │
└───────────────────────────────────────────────────────────────────────────────────────────────────┘
                                                  ▲
┌─────────────────────────────────────────────────┴────────────────────────────────────────────────────┐
│ crates/pulse-fetcher/  — standalone Rust binary, run by launchd at 08:00 / 21:00                       │
│   collect (16 free sources, concurrent, non-fatal) ─▶ dedup ─▶ summarize (Haiku) ─▶ cross-sector       │
│   analysis (Sonnet) ─▶ embed (Voyage) ─▶ extract entities ─▶ write SQLite                              │
└───────────────────────────────────────────────────────────────────────────────────────────────────────┘
```

Two Rust crates in one Cargo workspace: the Tauri app (`src-tauri/`, the query/serve side) and a headless fetcher (`crates/pulse-fetcher/`, the ingest side). They share one SQLite file and nothing else — the fetcher can run on a schedule with the app closed.

---

## Key engineering decisions & tradeoffs

**1. Brute-force cosine instead of a vector index (sqlite-vec / FAISS / a vector DB).**
Semantic search is a linear scan: deserialize every stored embedding (`bytemuck::cast_slice` over the raw little-endian blob — zero-copy, no per-element parsing) and compute cosine similarity against the query vector. At ~22k documents × 512 dims this is an O(n) scan with no index to build, invalidate, or keep consistent with the source-of-truth rows. Adding an ANN index (HNSW/IVF) would trade exact recall and operational simplicity for sub-linear latency the corpus doesn't yet need. The seam is a single function (`find_similar`); swapping in `sqlite-vec` is a contained change if the corpus outgrows a linear scan. **This is deliberate — the code does *not* use sqlite-vec today**, and the README says so rather than implying an index that isn't there.

**2. Score-weighted Reciprocal Rank Fusion, not vanilla RRF.** Standard RRF fuses only on rank position (`1/(k+rank)`), discarding the magnitude of the similarity. Pulse keeps the raw scores: `contribution = raw_score / (k + rank + 1)`, with `k = 60` and a **1.3× boost on the semantic channel** — chosen because FTS5 returns more low-quality near-matches, so an unboosted fusion lets keyword noise outvote a strong semantic hit. A relative score cutoff (drop below `0.3 × top_score`, but always keep ≥3 results) trims the long tail without starving niche queries.

**3. Recency decay is a function of the *query class*, not a constant.** A keyword heuristic classifies each question into Breaking / Analytical / Comparative / Advisory / General, and each class carries its own `(alpha, half_life)`: Breaking weights recency hard (`alpha=0.4, half_life=3d`), Comparative almost ignores it (`alpha=0.95, half_life=60d`). Final score = `alpha · relevance + (1 - alpha) · 0.5^(age_days / half_life)`. The same corpus answers "what's new today" and "how has this evolved over a year" without a mode switch from the user.

**4. Every external-model stage is non-fatal and time-boxed.** This is the cross-cutting decision that makes the pipeline shippable as a desktop app:
- Query rewrite: 3s timeout → falls back to the raw query. Also *skipped entirely* for queries under 6 words (a deliberate latency cut — short lookups don't benefit from HyDE).
- Reranking: Voyage `rerank-2-lite` (8s timeout) → Haiku LLM-rerank → original fused order. Three tiers, each catching the one above.
- Web search: only triggered when archive confidence is low; otherwise skipped to stay grounded and cheap.
- Synthesis: streamed token-by-token over a Tauri `Channel`, with an `AtomicBool` abort flag so the user can cancel mid-generation.

No single API failure can hang the UI or crash the webview — a property enforced in code (every `#[tauri::command]` returns `Result<T, String>`, and the project bans bare `.unwrap()` in command paths and `partial_cmp().unwrap()` anywhere, using `.unwrap_or(Ordering::Equal)` to survive NaN scores).

---

## Notable implementation details

- **HyDE done right.** The query rewriter (Haiku) emits structured JSON — expanded keywords, a *hypothetical answer* for semantic matching, a parsed temporal filter (`"last quarter"` → an ISO date), and optional sub-queries for multi-entity questions. The hypothetical answer is embedded separately and merged into the same fusion, so a vague question retrieves against what a *good answer* would look like, not just the question's words.
- **Query decomposition.** A multi-topic question ("compare X's AI strategy and Y's chip roadmap") is split into ≤3 sub-queries, each retrieved independently and merged — with a guard that only decomposes when there are genuinely 2+ distinct entities (most questions return `[]`).
- **Entity-graph query expansion.** Before retrieval, the query is expanded with known entity aliases and graph neighbors from the corpus's own entity table (`"NVDA"` → `"NVDA Nvidia"`), so ticker/acronym/full-name mismatches don't cost recall.
- **Field-weighted BM25.** FTS5 `bm25(stories_fts, 50, 1, 5, 5)` weights headline matches 50× body text, with a LIKE-based fallback if a malformed FTS query throws.
- **An actual eval harness, not vibes.** `cargo run --bin pulse-eval` scores retrieval against **39 ground-truth cases** (`eval/cases.json`) computing **Recall@k, Precision@k, and MRR**, with ablation flags (`--no-hyde`, `--no-rerank`, `--no-graph`, `--no-entity-expand`, `--no-rewrite`) to measure each stage's marginal contribution, plus an `--auto-judge` mode that uses Haiku to label relevance. The pipeline is instrumented to be measured, not asserted.
- **A backtester that says no.** The paper-trading layer includes a backtester with transaction costs and a Monte Carlo significance test; the git history shows it repeatedly returning honest **NO-GO** verdicts ("edge was small-sample noise") — the signal-driven trading stays paper-only by design.

---

## Tech stack

| Layer | Choice |
|-------|--------|
| Desktop shell | Tauri 2 (Rust) |
| Frontend | SvelteKit, Svelte 5 (runes), Tailwind CSS v4, TypeScript, Vite 6 |
| Backend | Rust, `rusqlite` (bundled SQLite + FTS5), Tokio, `reqwest`, `async-trait` |
| Fetcher | Standalone Rust binary, `feed-rs`, concurrent async ingest |
| Embeddings | Voyage `voyage-3-lite` (512-dim) |
| Rerank | Voyage `rerank-2-lite` → Haiku fallback |
| LLMs | Claude Haiku 4.5 (rewrite / summarize / rerank), Sonnet (synthesis / cross-sector) |
| Storage | SQLite — FTS5 + embedding blobs + entity graph, 25 numbered SQL migrations |
| Scheduling | macOS `launchd` (08:00 / 21:00) |

---

## Running it

Requires macOS, a Rust toolchain, and Node + [pnpm](https://pnpm.io/).

```bash
pnpm install                  # frontend deps
cp .env.example .env          # then fill in ANTHROPIC_API_KEY, VOYAGE_API_KEY, CURRENTS_API_KEY

pnpm tauri dev                # run the desktop app (frontend + Rust backend)
pnpm tauri build             # production app build
```

**Fetch pipeline (headless):**
```bash
cargo build -p pulse-fetcher
./target/debug/pulse-fetcher --mode daily          # one manual fetch
./target/debug/pulse-fetcher --mode daily --force  # re-fetch even if today's briefing exists
```

**Tests & evaluation:**
```bash
cargo test                                          # 146 tests (fusion, cosine, recency, reranking, DB)
cargo run --bin pulse-eval -- --k 10                # retrieval eval: Recall@k / Precision@k / MRR
cargo run --bin pulse-eval -- --k 10 --no-rerank    # ablate the rerank stage
pnpm check                                          # svelte-check
```

**Scheduled fetches:**
```bash
./scripts/build-sidecar.sh && ./scripts/install-launchd.sh   # install the 08:00/21:00 agent
```

---

## Status

Personal project, actively developed, `0.1.0`, **macOS-only**. Single-user and local-first by design — there is no server, no multi-tenancy, and no auth; the SQLite file on disk is the whole backend.

Honest limitations, stated plainly:
- **Semantic search is a brute-force linear scan**, not an indexed ANN search. Correct and simple at the current corpus size (~22k docs); it will need an index (the `sqlite-vec` swap) before it scales an order of magnitude larger.
- **Query classification is keyword-heuristic**, not a learned classifier — cheap and debuggable, but it mis-routes adversarial phrasings.
- The **trading layer is paper-only** and intentionally stays that way; the backtester has repeatedly found no real edge.
- The pipeline depends on external APIs (Anthropic, Voyage, plus free data sources) — every stage degrades gracefully, but with no keys the RAG and fetch paths are inert.

The numbers cited above (22k corpus, 512-dim embeddings, 146 tests, 39 eval cases, 56 commands, 16 sources, 25 migrations) are read from the code and database, not estimated. The eval harness exists and runs; this README does **not** quote eval *scores*, because they depend on a live API run and would go stale — run `pulse-eval` to produce current numbers.
