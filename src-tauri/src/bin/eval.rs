//! RAG Evaluation Harness for Pulse
//!
//! Measures retrieval quality against ground truth test cases.
//! Run `cargo run --bin pulse-eval -- --help` for usage.

use anyhow::{Context, Result};
use clap::Parser;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::{self, BufRead, Write as IoWrite};
use std::path::PathBuf;

use pulse_lib::services::{embeddings::{self, EmbeddingProvider}, search};

// ---------------------------------------------------------------------------
// CLI
// ---------------------------------------------------------------------------

#[derive(Parser, Debug)]
#[command(name = "pulse-eval", about = "RAG evaluation harness for Pulse")]
struct Args {
    /// Path to SQLite database
    #[arg(long, default_value = "~/Library/Application Support/com.pulse.app/pulse.db")]
    db_path: String,

    /// Path to test cases JSON file
    #[arg(long, default_value = "eval/cases.json")]
    cases: PathBuf,

    /// Number of results to evaluate
    #[arg(long, default_value_t = 10)]
    k: usize,

    /// Judge mode: review results and mark relevant ones
    #[arg(long)]
    judge: bool,

    /// Single query for judge mode (instead of reading cases file)
    #[arg(long)]
    query: Option<String>,

    /// Path to a file with queries to judge, one per line. Use with --judge.
    /// Each query is run in turn; relevant results are marked (interactively
    /// unless --auto-judge is set). New cases are appended to --cases.
    #[arg(long)]
    query_file: Option<PathBuf>,

    /// Auto-judge with Claude (Haiku) instead of prompting on stdin. Each query's
    /// top-K results get marked relevant/not by Haiku. Must be combined with --judge.
    #[arg(long)]
    auto_judge: bool,

    /// Disable graph expansion
    #[arg(long)]
    no_graph: bool,

    /// Disable HyDE embedding (semantic search uses raw query only)
    #[arg(long)]
    no_hyde: bool,

    /// Disable semantic dedup (keep headline-only dedup)
    #[arg(long)]
    no_dedup: bool,

    /// Disable entity alias expansion
    #[arg(long)]
    no_entity_expand: bool,

    /// Disable rerank stages (Voyage + LLM rerank). When ON (default), the eval
    /// matches the production Ask Pulse path. When OFF, measures retrieval-only.
    #[arg(long)]
    no_rerank: bool,

    /// How many candidates to retrieve before reranking. Default 30 — bigger
    /// pool gives the reranker more room to surface deep matches.
    #[arg(long, default_value_t = 30)]
    candidate_pool: usize,

    /// Print top-N (instead of top-k) for missing-rank diagnosis. Use 30 to see
    /// where missing headlines actually rank when reranking is off.
    #[arg(long)]
    diagnose: bool,

    /// Disable LLM query rewrite (Haiku synonym/HyDE expansion). Default ON to
    /// match production. With this flag the eval matches the OLD behavior — the
    /// previous baseline measured retrieval WITHOUT the rewrite stage.
    #[arg(long)]
    no_rewrite: bool,
}

// ---------------------------------------------------------------------------
// Test case data
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize, Deserialize)]
struct EvalCases {
    cases: Vec<TestCase>,
}

#[derive(Debug, Serialize, Deserialize)]
struct TestCase {
    query: String,
    expected_headlines: Vec<String>,
    #[serde(default)]
    notes: String,
}

// ---------------------------------------------------------------------------
// Metrics
// ---------------------------------------------------------------------------

struct QueryResult {
    query: String,
    recall: f64,
    precision: f64,
    mrr: f64,
    expected_count: usize,
    found_count: usize,
    _hits: Vec<(usize, String)>,    // (rank, headline) of found expected stories
    misses: Vec<String>,            // expected headlines not in top-k
    miss_diagnosis: Vec<(String, Option<usize>)>, // (headline, rank-in-full-pool or None)
    miss_modes: Vec<MissModeDiag>,  // per-miss FTS/sem/vocab breakdown
    top_results: Vec<(f64, String, bool)>, // (score, headline, is_relevant)
}

fn compute_recall(expected: &[String], results: &[search::ScoredStory], k: usize) -> (f64, usize, Vec<(usize, String)>, Vec<String>) {
    if expected.is_empty() {
        return (1.0, 0, vec![], vec![]);
    }

    let top_k: Vec<&search::ScoredStory> = results.iter().take(k).collect();
    let mut hits = Vec::new();
    let mut misses = Vec::new();

    for exp in expected {
        let exp_lower = exp.to_lowercase();
        if let Some(pos) = top_k.iter().position(|s| {
            s.headline.to_lowercase().contains(&exp_lower)
                || exp_lower.contains(&s.headline.to_lowercase())
        }) {
            hits.push((pos + 1, top_k[pos].headline.clone()));
        } else {
            misses.push(exp.clone());
        }
    }

    let recall = hits.len() as f64 / expected.len() as f64;
    (recall, hits.len(), hits, misses)
}


/// Precision@k = fraction of top-k results that are relevant.
/// Recall says "did we find them"; precision says "is the top noisy".
fn compute_precision(expected: &[String], results: &[search::ScoredStory], k: usize) -> f64 {
    if results.is_empty() || expected.is_empty() {
        return 0.0;
    }
    let top_k = results.iter().take(k);
    let relevant_count = top_k
        .filter(|s| is_relevant(&s.headline, expected))
        .count();
    relevant_count as f64 / k as f64
}

/// For each missing expected headline, find its rank (1-indexed) in `full_results`
/// or report "not retrieved" if it's beyond the candidate pool. Tells us whether
/// the bottleneck is "retrieval missed it entirely" or "k=10 was too tight".
fn diagnose_misses(misses: &[String], full_results: &[search::ScoredStory]) -> Vec<(String, Option<usize>)> {
    misses
        .iter()
        .map(|exp| {
            let exp_lower = exp.to_lowercase();
            let rank = full_results.iter().position(|s| {
                s.headline.to_lowercase().contains(&exp_lower)
                    || exp_lower.contains(&s.headline.to_lowercase())
            });
            (exp.clone(), rank.map(|r| r + 1))
        })
        .collect()
}

/// Failure-mode diagnostic for a single miss.
/// - `fts_rank`: rank of the expected story in pure FTS results (None = vocab miss / not retrieved)
/// - `sem_rank`: rank in pure semantic-only results (None = embedding distance too far)
/// - `vocab_overlap`: does the doc body contain ANY token from the expanded query?
///                    False → query and doc share no surface vocabulary.
#[derive(Debug, Clone)]
struct MissModeDiag {
    headline: String,
    fts_rank: Option<usize>,
    sem_rank: Option<usize>,
    vocab_overlap: bool,
}

/// Classify each miss into one of:
///   VOCAB    — query and doc share no tokens (expansion failed to bridge)
///   FTS_RANK — doc has tokens but BM25 buried it past the pool
///   SEM_ONLY — semantic finds it, FTS doesn't (HyDE/embedding carrying weight alone)
///   POOL     — neither lane retrieved it (raw vocab + embedding both miss)
///   FOUND    — at least one lane has it within candidate_pool (so the failure is downstream: merge/rerank/k cutoff)
fn diagnose_miss_modes(
    conn: &Connection,
    expanded_query: &str,
    query_embedding: Option<&[f32]>,
    hyde_embedding: Option<&[f32]>,
    misses: &[String],
    pool_size: usize,
) -> Vec<MissModeDiag> {
    if misses.is_empty() {
        return vec![];
    }

    // FTS-only candidates (story_id, score) → load headlines + body for matching/vocab.
    let fts_pairs = search::fts_search(conn, expanded_query, pool_size).unwrap_or_default();

    // Semantic-only — merge raw + HyDE the same way hybrid_search_with_hyde does.
    let mut sem_pairs: Vec<(i64, f32)> = Vec::new();
    if let Some(emb) = query_embedding {
        sem_pairs.extend(embeddings::find_similar(conn, emb, pool_size, 0.2).unwrap_or_default());
    }
    if let Some(hyde) = hyde_embedding {
        let hyde_results = embeddings::find_similar(conn, hyde, pool_size, 0.2).unwrap_or_default();
        for (id, score) in hyde_results {
            if let Some(existing) = sem_pairs.iter_mut().find(|(eid, _)| *eid == id) {
                existing.1 = existing.1.max(score);
            } else {
                sem_pairs.push((id, score));
            }
        }
        sem_pairs.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        sem_pairs.truncate(pool_size);
    }

    // Resolve story_ids → headlines for both lanes (only what we need to match against misses).
    let load_headline = |encoded_id: i64| -> Option<String> {
        let (real_id, is_freedom) = search::decode_story_id(encoded_id);
        let row = if is_freedom {
            conn.query_row(
                "SELECT headline FROM freedom_stories WHERE id = ?1",
                rusqlite::params![real_id],
                |r| r.get::<_, String>(0),
            )
        } else {
            conn.query_row(
                "SELECT headline FROM stories WHERE id = ?1",
                rusqlite::params![real_id],
                |r| r.get::<_, String>(0),
            )
        };
        row.ok()
    };

    let fts_headlines: Vec<String> = fts_pairs.iter()
        .filter_map(|(id, _)| load_headline(*id))
        .collect();
    let sem_headlines: Vec<String> = sem_pairs.iter()
        .filter_map(|(id, _)| load_headline(*id))
        .collect();

    // Tokens of the expanded query — alphanumeric, lowercase, length >= 3, drop FTS5 keywords.
    let stop: std::collections::HashSet<&str> = ["the","and","for","or","of","a","an","is","in","to","on","at","by","be"]
        .iter().copied().collect();
    let q_tokens: std::collections::HashSet<String> = expanded_query
        .split_whitespace()
        .map(|w| w.to_lowercase())
        .map(|w| w.chars().filter(|c| c.is_alphanumeric()).collect::<String>())
        .filter(|w| w.len() >= 3 && !stop.contains(w.as_str()) && w != "or" && w != "and")
        .collect();

    misses.iter().map(|exp| {
        let exp_lower = exp.to_lowercase();
        let rank_in = |pool: &[String]| -> Option<usize> {
            pool.iter().position(|h| {
                let hl = h.to_lowercase();
                hl.contains(&exp_lower) || exp_lower.contains(&hl)
            }).map(|r| r + 1)
        };

        let fts_rank = rank_in(&fts_headlines);
        let sem_rank = rank_in(&sem_headlines);

        // Vocab check: pull the doc body (headline + summary + key_facts) for the expected story
        // and see if any q_token appears. We don't have the story_id of the expected miss
        // (we only have a headline substring), so do a LIKE lookup.
        let exp_pattern = format!("%{}%", exp);
        let body: String = conn.query_row(
            "SELECT COALESCE(headline,'') || ' ' || COALESCE(summary,'') || ' ' || COALESCE(key_facts,'') || ' ' || COALESCE(why_it_matters,'')
             FROM stories WHERE headline LIKE ?1 LIMIT 1",
            rusqlite::params![exp_pattern],
            |r| r.get::<_, String>(0),
        ).or_else(|_| {
            conn.query_row(
                "SELECT COALESCE(headline,'') || ' ' || COALESCE(summary,'') || ' ' || COALESCE(key_facts,'') || ' ' || COALESCE(why_it_matters,'')
                 FROM freedom_stories WHERE headline LIKE ?1 LIMIT 1",
                rusqlite::params![exp_pattern],
                |r| r.get::<_, String>(0),
            )
        }).unwrap_or_default();
        let body_lower = body.to_lowercase();
        let vocab_overlap = q_tokens.iter().any(|t| body_lower.contains(t.as_str()));

        MissModeDiag {
            headline: exp.clone(),
            fts_rank,
            sem_rank,
            vocab_overlap,
        }
    }).collect()
}

/// Bucket label for a MissModeDiag.
fn classify_miss(d: &MissModeDiag) -> &'static str {
    match (d.fts_rank, d.sem_rank, d.vocab_overlap) {
        (Some(_), _, _) | (_, Some(_), _) => "FOUND_IN_LANE", // at least one lane retrieved it within pool
        (None, None, false) => "VOCAB",                       // query and doc share no surface tokens
        (None, None, true)  => "POOL",                        // tokens overlap but ranking pushed it out
    }
}

fn compute_mrr(expected: &[String], results: &[search::ScoredStory], k: usize) -> f64 {
    if expected.is_empty() {
        return 1.0;
    }

    let top_k: Vec<&search::ScoredStory> = results.iter().take(k).collect();
    for (rank, story) in top_k.iter().enumerate() {
        let headline_lower = story.headline.to_lowercase();
        for exp in expected {
            let exp_lower = exp.to_lowercase();
            if headline_lower.contains(&exp_lower) || exp_lower.contains(&headline_lower) {
                return 1.0 / (rank + 1) as f64;
            }
        }
    }
    0.0
}

fn is_relevant(headline: &str, expected: &[String]) -> bool {
    let h_lower = headline.to_lowercase();
    expected.iter().any(|exp| {
        let e_lower = exp.to_lowercase();
        h_lower.contains(&e_lower) || e_lower.contains(&h_lower)
    })
}

// ---------------------------------------------------------------------------
// Search runner
// ---------------------------------------------------------------------------

fn run_search(
    conn: &Connection,
    query: &str,
    k: usize,
    no_graph: bool,
    no_entity_expand: bool,
    no_rerank: bool,
    candidate_pool: usize,
    query_embedding: Option<&[f32]>,
    hyde_embedding: Option<&[f32]>,
    rt: &tokio::runtime::Runtime,
) -> Result<Vec<search::ScoredStory>> {
    // Step 1: Entity alias expansion
    let expanded = if no_entity_expand {
        query.to_string()
    } else {
        search::expand_query_with_entities(conn, query)
    };

    // Step 2: Graph expansion
    let final_query = if no_graph {
        expanded
    } else {
        let (graph_expanded, _entities) = search::expand_query_with_graph(conn, &expanded);
        graph_expanded
    };

    // Step 3: Hybrid search with pre-computed embeddings.
    let query_type = search::classify_query_type(query);
    let mut stories = search::hybrid_search_with_hyde(
        conn,
        &final_query,
        query_embedding,
        hyde_embedding,
        candidate_pool,
        query_type,
        None,
    )?;

    // Step 4: Rerank — production parity. Voyage's `voyage_rerank` early-returns
    // when stories.len() <= top_k, so we must pass top_k STRICTLY less than the
    // pool size to actually exercise the API. We rerank to k*2 (default 20) so
    // the top-k slice we evaluate is genuinely the reranked top.
    if !no_rerank && !stories.is_empty() {
        let api_key = std::env::var("ANTHROPIC_API_KEY").unwrap_or_default();
        let rerank_target = (k * 2).min(stories.len().saturating_sub(1).max(1));
        if stories.len() > rerank_target {
            let pre = stories.iter().take(rerank_target).map(|s| s.story_id).collect::<Vec<_>>();
            let reranked = rt.block_on(search::voyage_rerank(&final_query, stories.clone(), rerank_target));
            let post = reranked.iter().take(rerank_target).map(|s| s.story_id).collect::<Vec<_>>();
            let voyage_changed = pre != post;
            stories = if voyage_changed {
                tracing::info!("Voyage rerank changed order");
                reranked
            } else if !api_key.is_empty() {
                tracing::info!("Voyage rerank no-op or failed; trying LLM rerank");
                rt.block_on(pulse_lib::services::reranking::llm_rerank(
                    &api_key, &final_query, stories, rerank_target,
                ))
            } else {
                reranked
            };
        }
    }

    Ok(stories)
}

// ---------------------------------------------------------------------------
// HyDE: generate hypothetical documents via Haiku, then embed via Voyage
// ---------------------------------------------------------------------------

/// Generate 3 diverse HyDE variants per query, concatenated into one text.
/// Results are cached to eval/hyde_cache.json for reproducible eval runs.
async fn generate_hyde_texts(queries: &[String], cache_path: &std::path::Path) -> Result<Vec<String>> {
    // Check cache first
    if cache_path.exists() {
        let data = std::fs::read_to_string(cache_path)?;
        let cached: HashMap<String, String> = serde_json::from_str(&data).unwrap_or_default();
        if queries.iter().all(|q| cached.contains_key(q)) {
            println!("  (loaded HyDE texts from cache)");
            return Ok(queries.iter().map(|q| cached[q].clone()).collect());
        }
    }

    let api_key = std::env::var("ANTHROPIC_API_KEY")
        .context("ANTHROPIC_API_KEY not set")?;

    let queries_text = queries.iter().enumerate()
        .map(|(i, q)| format!("[{}] {}", i + 1, q))
        .collect::<Vec<_>>()
        .join("\n");

    let body = serde_json::json!({
        "model": "claude-haiku-4-5-20251001",
        "max_tokens": 8000,
        "system": r#"You generate hypothetical news article snippets for search retrieval. For each query, write 3 DIFFERENT news snippets (labeled a, b, c) covering diverse angles of the topic.

For example, "Italian politics latest" should have:
a) A snippet about parliamentary legislation or Meloni's government actions
b) A snippet about Italian court decisions or legal matters
c) A snippet about Italian elections or referendum results

Each snippet should be 2-3 sentences with specific names, companies, places, and terms that real articles would use.

Format:
[1a] snippet...
[1b] snippet...
[1c] snippet...
[2a] snippet...
etc."#,
        "messages": [{"role": "user", "content": format!("Generate 3 diverse hypothetical news snippets (a, b, c) for each query:\n{}", queries_text)}]
    });

    let client = reqwest::Client::new();
    let resp = client
        .post("https://api.anthropic.com/v1/messages")
        .header("x-api-key", &api_key)
        .header("anthropic-version", "2023-06-01")
        .header("content-type", "application/json")
        .json(&body)
        .send()
        .await
        .context("Haiku API request failed")?;

    if !resp.status().is_success() {
        let text = resp.text().await.unwrap_or_default();
        anyhow::bail!("Haiku API error: {}", text);
    }

    let response: serde_json::Value = resp.json().await?;
    let text = response["content"][0]["text"].as_str().unwrap_or("");
    parse_hyde_response(text, queries, cache_path)
}

fn parse_hyde_response(text: &str, queries: &[String], cache_path: &std::path::Path) -> Result<Vec<String>> {
    let mut per_query: Vec<Vec<String>> = vec![Vec::new(); queries.len()];

    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() { continue; }
        if let Some(rest) = trimmed.strip_prefix('[') {
            if let Some(bracket_end) = rest.find(']') {
                let tag = &rest[..bracket_end];
                let num_str: String = tag.chars().take_while(|c| c.is_ascii_digit()).collect();
                if let Ok(num) = num_str.parse::<usize>() {
                    let idx = num.saturating_sub(1);
                    if idx < queries.len() {
                        let content = rest[bracket_end + 1..].trim();
                        if !content.is_empty() {
                            per_query[idx].push(content.to_string());
                        }
                    }
                }
            }
        }
    }

    let hyde_texts: Vec<String> = per_query.iter()
        .map(|variants| variants.join(" "))
        .collect();

    // Cache for reproducibility
    let mut cache: HashMap<String, String> = HashMap::new();
    for (i, q) in queries.iter().enumerate() {
        cache.insert(q.clone(), hyde_texts.get(i).cloned().unwrap_or_default());
    }
    if let Ok(json) = serde_json::to_string_pretty(&cache) {
        let _ = std::fs::write(cache_path, json);
    }

    Ok(hyde_texts)
}

async fn batch_embed(texts: &[String], input_type: &str) -> Result<Vec<Option<Vec<f32>>>> {
    let provider = embeddings::VoyageProvider::from_env()?;

    // Filter out empty texts, track indices
    let non_empty: Vec<(usize, &String)> = texts.iter().enumerate()
        .filter(|(_, t)| !t.is_empty())
        .collect();

    if non_empty.is_empty() {
        return Ok(vec![None; texts.len()]);
    }

    let batch_texts: Vec<String> = non_empty.iter().map(|(_, t)| (*t).clone()).collect();
    let batch_indices: Vec<usize> = non_empty.iter().map(|(i, _)| *i).collect();

    // All 14 queries easily fit in one Voyage call (< 10K tokens)
    let embeddings_result = provider.embed(&batch_texts, input_type).await?;

    let mut result: Vec<Option<Vec<f32>>> = vec![None; texts.len()];
    for (j, emb) in embeddings_result.into_iter().enumerate() {
        if let Some(&idx) = batch_indices.get(j) {
            result[idx] = Some(emb);
        }
    }
    Ok(result)
}

// ---------------------------------------------------------------------------
// Judge mode
// ---------------------------------------------------------------------------

fn run_judge_mode(conn: &Connection, args: &Args, rt: &tokio::runtime::Runtime) -> Result<()> {
    let queries: Vec<(String, String)> = if let Some(ref q) = args.query {
        vec![(q.clone(), String::new())]
    } else if let Some(ref qf) = args.query_file {
        let data = std::fs::read_to_string(qf)
            .with_context(|| format!("Failed to read query file {}", qf.display()))?;
        data.lines()
            .map(|l| l.trim())
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
            .map(|l| (l.to_string(), String::new()))
            .collect()
    } else {
        let data = std::fs::read_to_string(&args.cases)
            .context("Failed to read cases file")?;
        let cases: EvalCases = serde_json::from_str(&data)?;
        cases.cases.into_iter().map(|c| (c.query, c.notes)).collect()
    };

    let mut all_cases: Vec<TestCase> = Vec::new();

    // Load existing cases to preserve already-judged ones
    let existing: EvalCases = if args.cases.exists() {
        let data = std::fs::read_to_string(&args.cases).unwrap_or_default();
        serde_json::from_str(&data).unwrap_or(EvalCases { cases: vec![] })
    } else {
        EvalCases { cases: vec![] }
    };

    let mut existing_map: std::collections::HashMap<String, TestCase> = existing
        .cases
        .into_iter()
        .map(|c| (c.query.clone(), c))
        .collect();

    let stdin = io::stdin();
    let mut stdout = io::stdout();
    let api_key = std::env::var("ANTHROPIC_API_KEY").unwrap_or_default();
    if args.auto_judge && api_key.is_empty() {
        anyhow::bail!("--auto-judge requires ANTHROPIC_API_KEY in env");
    }

    for (query, notes) in &queries {
        println!("\n{}", "=".repeat(70));
        println!("Query: \"{}\"", query);
        if !notes.is_empty() {
            println!("Notes: {}", notes);
        }
        println!("{}", "-".repeat(70));

        let results = run_search(
            conn, query, args.k, args.no_graph, args.no_entity_expand,
            args.no_rerank, args.candidate_pool, None, None, rt,
        )?;

        if results.is_empty() {
            println!("  (no results found)");
            // Still record an empty case so we don't keep re-judging it.
            all_cases.push(TestCase {
                query: query.clone(),
                expected_headlines: vec![],
                notes: notes.clone(),
            });
            existing_map.remove(query);
            continue;
        }

        for (i, story) in results.iter().enumerate() {
            let relevant_marker = if let Some(existing) = existing_map.get(query) {
                if is_relevant(&story.headline, &existing.expected_headlines) {
                    " [MARKED]"
                } else {
                    ""
                }
            } else {
                ""
            };

            println!(
                "  {:>2}. [{:.3}] {} ({}, {}){}\n      {}",
                i + 1,
                story.score,
                story.headline,
                story.date,
                story.source_name,
                relevant_marker,
                truncate(&story.summary, 100),
            );
        }

        let expected_headlines: Vec<String> = if args.auto_judge {
            // Ask Haiku to mark relevant indices. Returns a comma list of 1-based ints.
            let picks = rt.block_on(haiku_auto_judge(&api_key, query, &results))?;
            println!("\n  AUTO-JUDGE picked: {:?}", picks);
            picks.into_iter()
                .filter(|&n| n >= 1 && n <= results.len())
                .map(|n| results[n - 1].headline.clone())
                .collect()
        } else {
            println!("\n  Enter relevant result numbers (e.g., 1,2,3), 'skip' to keep existing, or 'none':");
            print!("  > ");
            stdout.flush()?;

            let mut input = String::new();
            stdin.lock().read_line(&mut input)?;
            let input = input.trim().to_string();

            if input == "skip" {
                if let Some(existing) = existing_map.remove(query) {
                    all_cases.push(existing);
                }
                continue;
            }
            if input == "none" {
                vec![]
            } else {
                input.split(',')
                    .filter_map(|s| s.trim().parse::<usize>().ok())
                    .filter(|&n| n >= 1 && n <= results.len())
                    .map(|n| results[n - 1].headline.clone())
                    .collect()
            }
        };

        existing_map.remove(query);
        all_cases.push(TestCase {
            query: query.clone(),
            expected_headlines,
            notes: notes.clone(),
        });
    }

    // Add remaining existing cases that weren't re-judged
    for (_, case) in existing_map {
        all_cases.push(case);
    }

    // Save updated cases
    let output = EvalCases { cases: all_cases };
    let json = serde_json::to_string_pretty(&output)?;
    std::fs::write(&args.cases, json)?;
    println!("\nSaved {} cases to {}", output.cases.len(), args.cases.display());

    Ok(())
}


/// Ask Haiku 4.5 to mark which of the top-K results are relevant to the query.
/// Returns 1-based indices. Conservative — Haiku errs toward inclusion if
/// the headline plausibly matches the query topic. We accept that bias because
/// the eval downstream measures "would Pulse have surfaced this?" not "is this
/// a perfect match?"
async fn haiku_auto_judge(
    api_key: &str,
    query: &str,
    results: &[search::ScoredStory],
) -> Result<Vec<usize>> {
    if results.is_empty() {
        return Ok(vec![]);
    }
    let mut numbered = String::new();
    for (i, s) in results.iter().enumerate() {
        // Headline + first line of summary is enough signal for a topical relevance call.
        let summary_short = truncate(&s.summary, 140);
        numbered.push_str(&format!("{}. {} — {}\n", i + 1, s.headline, summary_short));
    }

    let system = "You judge search-result relevance for a news intelligence archive. \
Given a user query and a numbered list of result headlines, return ONLY the comma-separated 1-based \
indices of results that are TOPICALLY RELEVANT to the query. Be inclusive: include any result that \
covers the query's topic, even tangentially. Exclude results that are clearly off-topic. Return \
just the numbers, no prose. Example output: 1,3,4,7. If none are relevant return: none";

    let user = format!("Query: \"{}\"\n\nResults:\n{}", query, numbered);

    let body = serde_json::json!({
        "model": "claude-haiku-4-5-20251001",
        "max_tokens": 100,
        "system": system,
        "messages": [{"role": "user", "content": user}]
    });

    let client = reqwest::Client::new();
    let resp = client
        .post("https://api.anthropic.com/v1/messages")
        .header("x-api-key", api_key)
        .header("anthropic-version", "2023-06-01")
        .header("content-type", "application/json")
        .json(&body)
        .send()
        .await
        .context("Haiku auto-judge request failed")?;

    if !resp.status().is_success() {
        anyhow::bail!("Haiku auto-judge returned {}", resp.status());
    }

    let json: serde_json::Value = resp.json().await?;
    let text = json["content"][0]["text"].as_str().unwrap_or("").trim().to_string();

    if text.eq_ignore_ascii_case("none") || text.is_empty() {
        return Ok(vec![]);
    }
    // Parse comma-separated ints, ignore anything that isn't a 1-based valid index.
    let picks: Vec<usize> = text
        .split(|c: char| !c.is_ascii_digit())
        .filter_map(|s| s.parse::<usize>().ok())
        .filter(|&n| n >= 1 && n <= results.len())
        .collect();
    Ok(picks)
}

// ---------------------------------------------------------------------------
// Eval mode
// ---------------------------------------------------------------------------

fn run_eval(conn: &Connection, args: &Args, rt: &tokio::runtime::Runtime) -> Result<()> {
    let data = std::fs::read_to_string(&args.cases)
        .context("Failed to read cases file. Run with --judge first to create ground truth.")?;
    let cases: EvalCases = serde_json::from_str(&data)?;

    let judged_cases: Vec<&TestCase> = cases
        .cases
        .iter()
        .filter(|c| !c.expected_headlines.is_empty())
        .collect();

    if judged_cases.is_empty() {
        println!("No judged cases found. Run with --judge first to mark relevant results.");
        return Ok(());
    }

    let queries: Vec<String> = judged_cases.iter().map(|c| c.query.clone()).collect();

    // === Step A: Optional LLM query rewrite (production parity) ===
    // Production calls rewrite_query → returns {fts_keywords, semantic_text, ...}.
    // The vector embedding uses the ORIGINAL query; FTS uses fts_keywords; HyDE
    // embeds semantic_text. Without this step the eval was measuring a degraded
    // retrieval path and underreported what users actually see.
    let api_key = std::env::var("ANTHROPIC_API_KEY").unwrap_or_default();
    let rewrites: Vec<pulse_lib::services::search::ExpandedQuery> = if args.no_rewrite || api_key.is_empty() {
        if args.no_rewrite {
            println!("Query rewrite: DISABLED (--no-rewrite)");
        } else {
            println!("Query rewrite: SKIPPED (ANTHROPIC_API_KEY missing)");
        }
        queries.iter()
            .map(|q| pulse_lib::services::search::ExpandedQuery::from_original(q))
            .collect()
    } else {
        println!("Rewriting {} queries via Haiku (production parity)...", queries.len());
        let mut out = Vec::with_capacity(queries.len());
        for q in &queries {
            let exp = rt.block_on(search::rewrite_query(&api_key, q, None));
            // Log so we can see whether Haiku is producing useful synonyms or
            // hallucinating the wrong era. Critical diagnostic for whether the
            // bottleneck is rewrite quality vs FTS5 syntax handling.
            eprintln!("  REWRITE [{}]:", q);
            eprintln!("    fts_keywords: {}", truncate(&exp.fts_keywords, 200));
            eprintln!("    semantic_text: {}", truncate(&exp.semantic_text, 200));
            if let Some(ref df) = exp.date_from {
                eprintln!("    date_from:    {}", df);
            }
            out.push(exp);
        }
        out
    };

    // === Step B: Embed ORIGINAL queries for vector search ===
    println!("Embedding {} queries via Voyage...", queries.len());
    let query_embeddings = rt.block_on(batch_embed(&queries, "query"))
        .unwrap_or_else(|e| {
            eprintln!("  [warn] Query embedding failed: {}", e);
            vec![None; queries.len()]
        });

    // === Step C: HyDE — use rewritten semantic_text if available, else fall back
    //              to the standalone HyDE generator (kept for back-compat). ===
    let hyde_cache = args.cases.with_file_name("hyde_cache.json");
    let hyde_embeddings: Vec<Option<Vec<f32>>> = if args.no_hyde {
        vec![None; queries.len()]
    } else {
        // If rewrite produced semantic_text, embed it directly. Otherwise generate fresh HyDE.
        let hyde_texts: Vec<String> = rewrites.iter().enumerate().map(|(i, exp)| {
            if exp.semantic_text != exp.original && exp.semantic_text.len() > 20 {
                exp.semantic_text.clone()
            } else {
                String::new() // signal "use legacy HyDE generator"
            }
        }).collect();

        let need_legacy_hyde = hyde_texts.iter().any(|s| s.is_empty());
        let final_texts: Vec<String> = if need_legacy_hyde {
            println!("Generating HyDE texts for queries without rewrite...");
            match rt.block_on(generate_hyde_texts(&queries, &hyde_cache)) {
                Ok(legacy) => {
                    hyde_texts.iter().enumerate().map(|(i, t)| {
                        if t.is_empty() { legacy[i].clone() } else { t.clone() }
                    }).collect()
                }
                Err(e) => {
                    eprintln!("  [warn] Legacy HyDE generation failed: {}", e);
                    hyde_texts
                }
            }
        } else {
            hyde_texts
        };

        for (i, ht) in final_texts.iter().enumerate() {
            if !ht.is_empty() {
                eprintln!("  HyDE [{}]: {}", queries[i], truncate(ht, 80));
            }
        }
        println!("Embedding HyDE texts via Voyage...");
        std::thread::sleep(std::time::Duration::from_secs(21));
        rt.block_on(batch_embed(&final_texts, "document"))
            .unwrap_or_else(|e| {
                eprintln!("  [warn] HyDE embedding failed: {}", e);
                vec![None; queries.len()]
            })
    };

    let mut results: Vec<QueryResult> = Vec::new();

    for (i, case) in judged_cases.iter().enumerate() {
        let exp = &rewrites[i];
        // The query passed into run_search is the rewritten FTS keywords.
        // Entity-alias and graph expansion stack on top inside run_search.
        let search_query = if !exp.fts_keywords.is_empty() {
            &exp.fts_keywords
        } else {
            &case.query
        };

        let full_pool = run_search(
            conn,
            search_query,
            args.k,
            args.no_graph,
            args.no_entity_expand,
            args.no_rerank,
            args.candidate_pool,
            query_embeddings[i].as_deref(),
            hyde_embeddings[i].as_deref(),
            rt,
        )?;

        let top_k: Vec<search::ScoredStory> = full_pool.iter().take(args.k).cloned().collect();

        let (recall, found, hits, misses) =
            compute_recall(&case.expected_headlines, &top_k, args.k);
        let precision = compute_precision(&case.expected_headlines, &top_k, args.k);
        let mrr = compute_mrr(&case.expected_headlines, &top_k, args.k);
        let miss_diagnosis = diagnose_misses(&misses, &full_pool);

        // Failure-mode diagnostic: replicate the expansion run_search would do, so the
        // FTS-only and semantic-only ranks reflect the same query text production saw.
        let expanded_for_diag = {
            let after_alias = if args.no_entity_expand {
                search_query.to_string()
            } else {
                search::expand_query_with_entities(conn, search_query)
            };
            if args.no_graph {
                after_alias
            } else {
                let (g, _) = search::expand_query_with_graph(conn, &after_alias);
                g
            }
        };
        let miss_modes = diagnose_miss_modes(
            conn,
            &expanded_for_diag,
            query_embeddings[i].as_deref(),
            hyde_embeddings[i].as_deref(),
            &misses,
            args.candidate_pool,
        );

        let top_results: Vec<(f64, String, bool)> = top_k
            .iter()
            .map(|s| {
                (
                    s.score as f64,
                    s.headline.clone(),
                    is_relevant(&s.headline, &case.expected_headlines),
                )
            })
            .collect();

        results.push(QueryResult {
            query: case.query.clone(),
            recall,
            precision,
            mrr,
            expected_count: case.expected_headlines.len(),
            found_count: found,
            _hits: hits,
            misses,
            miss_diagnosis,
            miss_modes,
            top_results,
        });
    }

    // Per-query results
    for r in &results {
        println!("\n{}", "-".repeat(70));
        println!("Query: \"{}\"", r.query);
        println!(
            "Recall@{}: {}/{} ({:.1}%)  Precision@{}: {:.1}%  MRR: {:.3}",
            args.k, r.found_count, r.expected_count, r.recall * 100.0,
            args.k, r.precision * 100.0, r.mrr,
        );

        for (score, headline, relevant) in &r.top_results {
            let marker = if *relevant { "+" } else { " " };
            println!("  {} [{:.3}] {}", marker, score, truncate(headline, 65));
        }

        if !r.miss_diagnosis.is_empty() {
            for (miss, rank) in &r.miss_diagnosis {
                let rank_label = match rank {
                    Some(rk) if *rk <= args.k => format!("rank {} (BUG: in top-k but compute_recall missed it)", rk),
                    Some(rk) => format!("rank {} (retrieved but cut by k={})", rk, args.k),
                    None => format!("NOT RETRIEVED (beyond candidate pool of {})", args.candidate_pool),
                };
                // Find matching mode diag (by headline) for FTS / sem / vocab breakdown.
                let mode = r.miss_modes.iter().find(|m| m.headline == *miss);
                let mode_label = if let Some(m) = mode {
                    let bucket = classify_miss(m);
                    let fts = m.fts_rank.map(|r| format!("FTS#{}", r)).unwrap_or("FTS-".into());
                    let sem = m.sem_rank.map(|r| format!("SEM#{}", r)).unwrap_or("SEM-".into());
                    let vocab = if m.vocab_overlap { "vocab+" } else { "vocab-" };
                    format!(" [{} {} {} {}]", bucket, fts, sem, vocab)
                } else { String::new() };
                println!("  X MISSING: {} — {}{}", truncate(miss, 50), rank_label, mode_label);
            }
        }
    }

    let avg_recall = results.iter().map(|r| r.recall).sum::<f64>() / results.len() as f64;
    let avg_precision = results.iter().map(|r| r.precision).sum::<f64>() / results.len() as f64;
    let avg_mrr = results.iter().map(|r| r.mrr).sum::<f64>() / results.len() as f64;
    let perfect_count = results.iter().filter(|r| r.recall == 1.0).count();

    let mut in_pool_cut = 0;
    let mut not_retrieved = 0;
    for r in &results {
        for (_, rank) in &r.miss_diagnosis {
            match rank {
                Some(rk) if *rk > args.k => in_pool_cut += 1,
                Some(_) => {}
                None => not_retrieved += 1,
            }
        }
    }
    let total_misses = in_pool_cut + not_retrieved;

    println!("\n{}", "=".repeat(70));
    println!("RAG Evaluation Report (k={}, candidate_pool={})", args.k, args.candidate_pool);
    println!("{}", "=".repeat(70));
    println!("Queries:        {}", results.len());
    println!("Avg Recall@{}:  {:.1}%", args.k, avg_recall * 100.0);
    println!("Avg Precision@{}: {:.1}%", args.k, avg_precision * 100.0);
    println!("Avg MRR:        {:.3}", avg_mrr);
    println!("Perfect Recall: {}/{}", perfect_count, results.len());
    if total_misses > 0 {
        println!();
        println!("Miss breakdown:");
        println!(
            "  In pool but cut by k:  {}/{} ({:.0}%) — would be fixed by larger k or better rerank",
            in_pool_cut, total_misses,
            (in_pool_cut as f64 / total_misses as f64) * 100.0,
        );
        println!(
            "  Not retrieved at all:  {}/{} ({:.0}%) — retrieval bottleneck (FTS+vector both miss)",
            not_retrieved, total_misses,
            (not_retrieved as f64 / total_misses as f64) * 100.0,
        );

        // Failure-mode breakdown of all misses (FOUND_IN_LANE / VOCAB / POOL).
        let mut bucket_found = 0;
        let mut bucket_vocab = 0;
        let mut bucket_pool = 0;
        let mut fts_only = 0;     // FTS retrieved, semantic didn't
        let mut sem_only = 0;     // semantic retrieved, FTS didn't
        let mut both_lanes = 0;   // both lanes retrieved it (so it's a merge/rerank/k-cut issue)
        for r in &results {
            for m in &r.miss_modes {
                match classify_miss(m) {
                    "FOUND_IN_LANE" => {
                        bucket_found += 1;
                        match (m.fts_rank.is_some(), m.sem_rank.is_some()) {
                            (true, false) => fts_only += 1,
                            (false, true) => sem_only += 1,
                            (true, true)  => both_lanes += 1,
                            _ => {}
                        }
                    }
                    "VOCAB" => bucket_vocab += 1,
                    "POOL"  => bucket_pool  += 1,
                    _ => {}
                }
            }
        }
        println!();
        println!("Failure mode breakdown:");
        println!(
            "  FOUND_IN_LANE: {}/{} ({:.0}%) — at least one lane has it within pool (downstream issue: merge/rerank/k)",
            bucket_found, total_misses,
            (bucket_found as f64 / total_misses as f64) * 100.0,
        );
        if bucket_found > 0 {
            println!("    ├ FTS-only retrieved:  {}", fts_only);
            println!("    ├ Sem-only retrieved:  {}", sem_only);
            println!("    └ Both lanes retrieved: {}", both_lanes);
        }
        println!(
            "  VOCAB:         {}/{} ({:.0}%) — query and doc share zero tokens (expansion failed to bridge)",
            bucket_vocab, total_misses,
            (bucket_vocab as f64 / total_misses as f64) * 100.0,
        );
        println!(
            "  POOL:          {}/{} ({:.0}%) — tokens overlap but neither lane retrieved it (BM25/embedding ranking buried it)",
            bucket_pool, total_misses,
            (bucket_pool as f64 / total_misses as f64) * 100.0,
        );
    }
    println!();

    let mut flags = Vec::new();
    flags.push(format!("rewrite={}", if args.no_rewrite { "OFF" } else { "ON" }));
    flags.push(format!("graph={}", if args.no_graph { "OFF" } else { "ON" }));
    flags.push(format!("hyde={}", if args.no_hyde { "OFF" } else { "ON" }));
    flags.push(format!("dedup={}", if args.no_dedup { "OFF" } else { "ON" }));
    flags.push(format!(
        "entity_expand={}",
        if args.no_entity_expand { "OFF" } else { "ON" }
    ));
    flags.push(format!("rerank={}", if args.no_rerank { "OFF" } else { "ON" }));
    println!("Feature flags: {}", flags.join(", "));
    println!("{}", "=".repeat(70));

    Ok(())
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        format!("{}...", s.chars().take(max - 3).collect::<String>())
    }
}

fn resolve_db_path(path: &str) -> PathBuf {
    if path.starts_with("~/") {
        if let Some(home) = dirs::home_dir() {
            return home.join(&path[2..]);
        }
    }
    PathBuf::from(path)
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

fn main() -> Result<()> {
    let args = Args::parse();

    // Load .env
    let env_path = dirs::home_dir()
        .unwrap_or_default()
        .join("Projects/Pulse/.env");
    dotenvy::from_path(&env_path).ok();

    let db_path = resolve_db_path(&args.db_path);
    if !db_path.exists() {
        anyhow::bail!(
            "Database not found at {}. Use --db-path to specify location.",
            db_path.display()
        );
    }

    let conn = Connection::open(&db_path)?;
    conn.execute_batch("PRAGMA journal_mode=WAL;")?;

    println!("Pulse RAG Eval");
    println!("DB: {}", db_path.display());

    // Quick stats
    let story_count: i64 = conn.query_row("SELECT COUNT(*) FROM stories", [], |r| r.get(0))?;
    let embedding_count: i64 =
        conn.query_row("SELECT COUNT(*) FROM story_embeddings", [], |r| r.get(0))?;
    let entity_count: i64 = conn.query_row("SELECT COUNT(*) FROM entities", [], |r| r.get(0))?;
    println!(
        "Stories: {}  Embeddings: {}  Entities: {}",
        story_count, embedding_count, entity_count
    );

    let rt = tokio::runtime::Runtime::new()?;

    if args.judge {
        run_judge_mode(&conn, &args, &rt)?;
    } else {
        run_eval(&conn, &args, &rt)?;
    }

    Ok(())
}
