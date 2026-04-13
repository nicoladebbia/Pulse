//! RAG Evaluation Harness for Pulse
//!
//! Measures retrieval quality against ground truth test cases.
//! Run `cargo run --bin pulse-eval -- --help` for usage.

use anyhow::{Context, Result};
use clap::Parser;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use std::io::{self, BufRead, Write as IoWrite};
use std::path::PathBuf;

use pulse_lib::services::{embeddings, search};

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
    mrr: f64,
    expected_count: usize,
    found_count: usize,
    _hits: Vec<(usize, String)>,    // (rank, headline) of found expected stories
    misses: Vec<String>,            // expected headlines not found
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

    // Step 3: Load query embedding from a representative story (no API call)
    // We search with FTS + whatever embeddings are stored. No Voyage calls.
    // For semantic search, we need a query embedding. We can approximate by
    // finding the best FTS match and using its embedding as a proxy.
    let query_embedding: Option<Vec<f32>> = {
        let fts_results = search::fts_search(conn, &final_query, 1)?;
        if let Some((story_id, _)) = fts_results.first() {
            conn.query_row(
                "SELECT embedding FROM story_embeddings WHERE story_id = ?1",
                [story_id],
                |row| row.get::<_, Vec<u8>>(0),
            )
            .ok()
            .map(|blob| embeddings::deserialize_embedding(&blob))
        } else {
            None
        }
    };

    // Step 4: Run hybrid search
    let query_type = search::classify_query_type(query);
    let stories = search::hybrid_search_with_hyde(
        conn,
        &final_query,
        query_embedding.as_deref(),
        None, // No HyDE embedding (would need API call)
        k * 3, // Fetch more, then truncate
        query_type,
        None,
    )?;

    Ok(stories.into_iter().take(k).collect())
}

// ---------------------------------------------------------------------------
// Judge mode
// ---------------------------------------------------------------------------

fn run_judge_mode(conn: &Connection, args: &Args) -> Result<()> {
    let queries: Vec<(String, String)> = if let Some(ref q) = args.query {
        vec![(q.clone(), String::new())]
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

    for (query, notes) in &queries {
        println!("\n{}", "=".repeat(70));
        println!("Query: \"{}\"", query);
        if !notes.is_empty() {
            println!("Notes: {}", notes);
        }
        println!("{}", "-".repeat(70));

        let results = run_search(conn, query, args.k, args.no_graph, args.no_entity_expand)?;

        if results.is_empty() {
            println!("  (no results found)");
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

        println!("\n  Enter relevant result numbers (e.g., 1,2,3), 'skip' to keep existing, or 'none':");
        print!("  > ");
        stdout.flush()?;

        let mut input = String::new();
        stdin.lock().read_line(&mut input)?;
        let input = input.trim();

        if input == "skip" {
            if let Some(existing) = existing_map.remove(query) {
                all_cases.push(existing);
            }
            continue;
        }

        let expected_headlines: Vec<String> = if input == "none" {
            vec![]
        } else {
            input
                .split(',')
                .filter_map(|s| s.trim().parse::<usize>().ok())
                .filter(|&n| n >= 1 && n <= results.len())
                .map(|n| results[n - 1].headline.clone())
                .collect()
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

// ---------------------------------------------------------------------------
// Eval mode
// ---------------------------------------------------------------------------

fn run_eval(conn: &Connection, args: &Args) -> Result<()> {
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

    let mut results: Vec<QueryResult> = Vec::new();

    for case in &judged_cases {
        let stories = run_search(conn, &case.query, args.k, args.no_graph, args.no_entity_expand)?;

        let (recall, found, hits, misses) =
            compute_recall(&case.expected_headlines, &stories, args.k);
        let mrr = compute_mrr(&case.expected_headlines, &stories, args.k);

        let top_results: Vec<(f64, String, bool)> = stories
            .iter()
            .take(args.k)
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
            mrr,
            expected_count: case.expected_headlines.len(),
            found_count: found,
            _hits: hits,
            misses,
            top_results,
        });
    }

    // Print per-query results
    for r in &results {
        println!("\n{}", "-".repeat(70));
        println!(
            "Query: \"{}\"",
            r.query
        );
        println!(
            "Recall@{}: {}/{} ({:.1}%)  MRR: {:.3}",
            args.k, r.found_count, r.expected_count, r.recall * 100.0, r.mrr
        );

        for (score, headline, relevant) in &r.top_results {
            let marker = if *relevant { "+" } else { " " };
            println!("  {} [{:.3}] {}", marker, score, truncate(headline, 65));
        }

        if !r.misses.is_empty() {
            for miss in &r.misses {
                println!("  X MISSING: {}", truncate(miss, 60));
            }
        }
    }

    // Aggregate metrics
    let avg_recall = results.iter().map(|r| r.recall).sum::<f64>() / results.len() as f64;
    let avg_mrr = results.iter().map(|r| r.mrr).sum::<f64>() / results.len() as f64;
    let perfect_count = results.iter().filter(|r| r.recall == 1.0).count();

    println!("\n{}", "=".repeat(70));
    println!("RAG Evaluation Report (k={})", args.k);
    println!("{}", "=".repeat(70));
    println!("Queries:        {}", results.len());
    println!("Avg Recall:     {:.1}%", avg_recall * 100.0);
    println!("Avg MRR:        {:.3}", avg_mrr);
    println!("Perfect Recall: {}/{}", perfect_count, results.len());
    println!();

    let mut flags = Vec::new();
    flags.push(format!("graph={}", if args.no_graph { "OFF" } else { "ON" }));
    flags.push(format!("hyde={}", if args.no_hyde { "OFF" } else { "ON" }));
    flags.push(format!("dedup={}", if args.no_dedup { "OFF" } else { "ON" }));
    flags.push(format!(
        "entity_expand={}",
        if args.no_entity_expand { "OFF" } else { "ON" }
    ));
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

    if args.judge {
        run_judge_mode(&conn, &args)?;
    } else {
        run_eval(&conn, &args)?;
    }

    Ok(())
}
