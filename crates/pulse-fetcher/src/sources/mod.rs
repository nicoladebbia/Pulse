pub mod google_news;
pub mod rss_feeds;
pub mod hacker_news;
pub mod reddit;
pub mod arxiv;
pub mod biorxiv;
pub mod usaspending;
pub mod federal_register;
pub mod edgar;
pub mod fred;
pub mod fec;
pub mod eia;
pub mod lobbying;
pub mod patents;
pub mod wikipedia;

use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU32, Ordering};

/// Per-provider counter of actual HTTP requests made during a fetch run.
/// Each source module bumps its counter once per real HTTP call.
/// The pipeline reads these at the end of collect_all and writes 1
/// api_usage row per call (not per batch).
pub struct ApiCallCounters {
    pub google_news: AtomicU32,
    pub hacker_news: AtomicU32,
    pub reddit: AtomicU32,
    pub arxiv: AtomicU32,
    pub biorxiv: AtomicU32,
    pub rss_feeds: AtomicU32,
    pub wikipedia: AtomicU32,
    pub fred: AtomicU32,
    pub fec: AtomicU32,
    pub eia: AtomicU32,
    pub sec_edgar: AtomicU32,
    pub usaspending: AtomicU32,
    pub federal_register: AtomicU32,
    pub lda: AtomicU32,
    pub uspto: AtomicU32,
}

impl ApiCallCounters {
    const fn new() -> Self {
        Self {
            google_news: AtomicU32::new(0),
            hacker_news: AtomicU32::new(0),
            reddit: AtomicU32::new(0),
            arxiv: AtomicU32::new(0),
            biorxiv: AtomicU32::new(0),
            rss_feeds: AtomicU32::new(0),
            wikipedia: AtomicU32::new(0),
            fred: AtomicU32::new(0),
            fec: AtomicU32::new(0),
            eia: AtomicU32::new(0),
            sec_edgar: AtomicU32::new(0),
            usaspending: AtomicU32::new(0),
            federal_register: AtomicU32::new(0),
            lda: AtomicU32::new(0),
            uspto: AtomicU32::new(0),
        }
    }

    /// Reset all counters to zero (call at start of each fetch run).
    pub fn reset(&self) {
        self.google_news.store(0, Ordering::Relaxed);
        self.hacker_news.store(0, Ordering::Relaxed);
        self.reddit.store(0, Ordering::Relaxed);
        self.arxiv.store(0, Ordering::Relaxed);
        self.biorxiv.store(0, Ordering::Relaxed);
        self.rss_feeds.store(0, Ordering::Relaxed);
        self.wikipedia.store(0, Ordering::Relaxed);
        self.fred.store(0, Ordering::Relaxed);
        self.fec.store(0, Ordering::Relaxed);
        self.eia.store(0, Ordering::Relaxed);
        self.sec_edgar.store(0, Ordering::Relaxed);
        self.usaspending.store(0, Ordering::Relaxed);
        self.federal_register.store(0, Ordering::Relaxed);
        self.lda.store(0, Ordering::Relaxed);
        self.uspto.store(0, Ordering::Relaxed);
    }

    /// Return (provider_name, call_count) pairs for all non-zero counters.
    pub fn snapshot(&self) -> Vec<(&'static str, u32)> {
        let pairs = [
            ("google_news", self.google_news.load(Ordering::Relaxed)),
            ("hacker_news", self.hacker_news.load(Ordering::Relaxed)),
            ("reddit", self.reddit.load(Ordering::Relaxed)),
            ("arxiv", self.arxiv.load(Ordering::Relaxed)),
            ("biorxiv", self.biorxiv.load(Ordering::Relaxed)),
            ("rss_feeds", self.rss_feeds.load(Ordering::Relaxed)),
            ("wikipedia", self.wikipedia.load(Ordering::Relaxed)),
            ("fred", self.fred.load(Ordering::Relaxed)),
            ("fec", self.fec.load(Ordering::Relaxed)),
            ("eia", self.eia.load(Ordering::Relaxed)),
            ("sec_edgar", self.sec_edgar.load(Ordering::Relaxed)),
            ("usaspending", self.usaspending.load(Ordering::Relaxed)),
            ("federal_register", self.federal_register.load(Ordering::Relaxed)),
            ("lda", self.lda.load(Ordering::Relaxed)),
            ("uspto", self.uspto.load(Ordering::Relaxed)),
        ];
        pairs.into_iter().filter(|(_, n)| *n > 0).collect()
    }
}

pub static API_CALLS: ApiCallCounters = ApiCallCounters::new();

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RawArticle {
    pub title: String,
    pub url: String,
    pub source_name: String,
    pub source_url: String,
    pub published_at: Option<String>,
    pub content_snippet: String,
    pub sector: String,
    pub feed_id: String,
    pub language: String,
    /// "news" for regular articles, "financial" for structured financial data
    #[serde(default = "default_source_type")]
    pub source_type: String,
    /// JSON blob with structured financial data (only for source_type="financial")
    #[serde(default)]
    pub financial_metadata: Option<String>,
}

fn default_source_type() -> String {
    "news".to_string()
}

/// Wall-clock budget for a single source inside Phase 1.
///
/// `collect_all` joins 14 source futures, so the stage lasts as long as its slowest
/// member. Sources that loop sequentially with their own retries (EDGAR walks tracked
/// CIKs with a 30s per-request timeout) can stretch to hours on a degraded network:
/// measured Phase-1 durations were 34m, 65m and 120m, all returning 0 articles, versus
/// 9m for the slowest HEALTHY run (1267 articles). 600s clears every healthy run with
/// margin while capping the stage near 10 minutes, and the timeout log line names the
/// culprit — which the join'd per-source logs cannot, since they all print after the
/// join returns.
const SOURCE_TIMEOUT_SECS: u64 = 600;

/// How many sources `collect_all` joins. Kept next to the join! so the two move together.
pub const SOURCE_COUNT: usize = 14;

/// Sources finished (successfully or not) in the current Phase 1. The progress heartbeat
/// reads this to move the bar during a collect that legitimately runs minutes with no
/// stage change — a bar frozen at 0% reads as "stuck" even when the run is healthy.
///
/// Process-global, reset at the top of every `collect_all`. That is safe only because two
/// collects never overlap: `--mode daily` and `--mode freedoms` (pipeline.rs, the second
/// `collect_all` caller) run sequentially, and the flock in main.rs keeps a second process
/// out. It also only matters while a record is in-progress — the heartbeat stops writing
/// once the progress file reads `complete`/`failed`, which it does before freedoms starts.
/// If a concurrent collect path is ever added, this counter must become per-run state.
pub static SOURCES_DONE: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Sources that errored or timed out in the current Phase 1. Read by the pipeline for the
/// `pipeline_health.feeds_failed` column, which had been a permanent 0 on all 161 rows
/// because nothing ever counted this — a run losing half its sources looked identical to
/// a clean one in the health table.
pub static SOURCES_FAILED: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

async fn bounded<T>(
    name: &'static str,
    fut: impl std::future::Future<Output = anyhow::Result<T>>,
) -> anyhow::Result<T> {
    let outcome =
        match tokio::time::timeout(std::time::Duration::from_secs(SOURCE_TIMEOUT_SECS), fut).await {
            Ok(result) => result,
            Err(_) => {
                tracing::warn!(
                    "SOURCE TIMEOUT: {} exceeded {}s and was abandoned",
                    name,
                    SOURCE_TIMEOUT_SECS
                );
                Err(anyhow::anyhow!(
                    "{} timed out after {}s",
                    name,
                    SOURCE_TIMEOUT_SECS
                ))
            }
        };
    SOURCES_DONE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    if outcome.is_err() {
        SOURCES_FAILED.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
    outcome
}

pub async fn collect_all() -> anyhow::Result<Vec<RawArticle>> {
    SOURCES_DONE.store(0, std::sync::atomic::Ordering::Relaxed);
    SOURCES_FAILED.store(0, std::sync::atomic::Ordering::Relaxed);
    // Fetch news and financial sources concurrently, each under its own time budget.
    let (google, rss, hn, reddit_posts, arxiv_papers, biorxiv_papers, usa_spending, fed_register, sec_edgar, fred_data, fec_data, eia_data, lda_data, patent_data) = tokio::join!(
        bounded("Google News", google_news::fetch_all()),
        bounded("RSS feeds", rss_feeds::fetch_all()),
        bounded("Hacker News", hacker_news::fetch()),
        bounded("Reddit", reddit::fetch()),
        bounded("ArXiv", arxiv::fetch()),
        bounded("bioRxiv", biorxiv::fetch()),
        bounded("USASpending", usaspending::fetch()),
        bounded("Federal Register", federal_register::fetch()),
        bounded("SEC EDGAR", edgar::fetch()),
        bounded("FRED", fred::fetch()),
        bounded("FEC", fec::fetch()),
        bounded("EIA", eia::fetch()),
        bounded("LDA Lobbying", lobbying::fetch()),
        bounded("Patents", patents::fetch()),
    );

    let mut articles = Vec::new();
    let mut failed_sources = Vec::new();

    // News sources. ORDER MATTERS: dedup keeps the FIRST copy of a duplicate
    // story, so direct-URL sources (RSS, HN) go before Google News — google
    // links are JS redirect shells that article_text::enrich cannot fetch and
    // the app cannot deep-link. With Google first, its shell copy won every
    // dedup tie and 100% of briefing news URLs were google shells (2026-08-07).
    match rss {
        Ok(a) => {
            tracing::info!("RSS feeds: {} articles", a.len());
            articles.extend(a);
        }
        Err(e) => {
            tracing::error!("RSS feeds FAILED: {}", e);
            failed_sources.push("RSS feeds");
        }
    }
    match hn {
        Ok(a) => {
            tracing::info!("Hacker News: {} articles", a.len());
            articles.extend(a);
        }
        Err(e) => {
            tracing::error!("Hacker News FAILED: {}", e);
            failed_sources.push("Hacker News");
        }
    }
    match google {
        Ok(a) => {
            tracing::info!("Google News: {} articles", a.len());
            articles.extend(a);
        }
        Err(e) => {
            tracing::error!("Google News FAILED: {}", e);
            failed_sources.push("Google News");
        }
    }
    match reddit_posts {
        Ok(a) => {
            tracing::info!("Reddit: {} articles", a.len());
            articles.extend(a);
        }
        Err(e) => {
            tracing::warn!("Reddit FAILED (non-fatal): {}", e);
            failed_sources.push("Reddit");
        }
    }
    match arxiv_papers {
        Ok(a) => {
            tracing::info!("ArXiv: {} articles", a.len());
            articles.extend(a);
        }
        Err(e) => {
            tracing::warn!("ArXiv FAILED (non-fatal): {}", e);
            failed_sources.push("ArXiv");
        }
    }
    match biorxiv_papers {
        Ok(a) => {
            tracing::info!("bioRxiv: {} articles", a.len());
            articles.extend(a);
        }
        Err(e) => {
            tracing::warn!("bioRxiv FAILED (non-fatal): {}", e);
            failed_sources.push("bioRxiv");
        }
    }

    // Financial sources (non-fatal — pipeline continues if any fail)
    match usa_spending {
        Ok(a) => {
            tracing::info!("USASpending: {} financial articles", a.len());
            articles.extend(a);
        }
        Err(e) => {
            tracing::warn!("USASpending FAILED (non-fatal): {}", e);
            failed_sources.push("USASpending");
        }
    }
    match fed_register {
        Ok(a) => {
            tracing::info!("Federal Register: {} financial articles", a.len());
            articles.extend(a);
        }
        Err(e) => {
            tracing::warn!("Federal Register FAILED (non-fatal): {}", e);
            failed_sources.push("Federal Register");
        }
    }
    match sec_edgar {
        Ok(a) => {
            tracing::info!("SEC EDGAR: {} financial articles", a.len());
            articles.extend(a);
        }
        Err(e) => {
            tracing::warn!("SEC EDGAR FAILED (non-fatal): {}", e);
            failed_sources.push("SEC EDGAR");
        }
    }
    match fred_data {
        Ok(a) => {
            if !a.is_empty() {
                tracing::info!("FRED: {} financial articles", a.len());
                articles.extend(a);
            }
        }
        Err(e) => {
            tracing::warn!("FRED FAILED (non-fatal): {}", e);
            failed_sources.push("FRED");
        }
    }
    match fec_data {
        Ok(a) => {
            if !a.is_empty() {
                tracing::info!("FEC: {} financial articles", a.len());
                articles.extend(a);
            }
        }
        Err(e) => {
            tracing::warn!("FEC FAILED (non-fatal): {}", e);
            failed_sources.push("FEC");
        }
    }
    match eia_data {
        Ok(a) => {
            if !a.is_empty() {
                tracing::info!("EIA: {} financial articles", a.len());
                articles.extend(a);
            }
        }
        Err(e) => {
            tracing::warn!("EIA FAILED (non-fatal): {}", e);
            failed_sources.push("EIA");
        }
    }
    match lda_data {
        Ok(a) => {
            if !a.is_empty() {
                tracing::info!("LDA Lobbying: {} financial articles", a.len());
                articles.extend(a);
            }
        }
        Err(e) => {
            tracing::warn!("LDA Lobbying FAILED (non-fatal): {}", e);
            failed_sources.push("LDA");
        }
    }
    match patent_data {
        Ok(a) => {
            if !a.is_empty() {
                tracing::info!("Patents: {} financial articles", a.len());
                articles.extend(a);
            }
        }
        Err(e) => {
            tracing::warn!("Patents FAILED (non-fatal): {}", e);
            failed_sources.push("Patents");
        }
    }

    if !failed_sources.is_empty() {
        tracing::warn!("SOURCE HEALTH: {} source(s) failed: {}", failed_sources.len(), failed_sources.join(", "));
    }

    let news_count = articles.iter().filter(|a| a.source_type == "news").count();
    let fin_count = articles.iter().filter(|a| a.source_type == "financial").count();
    tracing::info!("Total collected: {} articles ({} news, {} financial)", articles.len(), news_count, fin_count);

    if news_count < 50 {
        tracing::warn!("SOURCE HEALTH: only {} news articles (expected 100+)", news_count);
    }

    Ok(articles)
}
