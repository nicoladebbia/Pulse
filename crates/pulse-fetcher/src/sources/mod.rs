pub mod google_news;
pub mod rss_feeds;
pub mod hacker_news;
pub mod usaspending;
pub mod federal_register;
pub mod sbir;
pub mod edgar;
pub mod fred;
pub mod fec;
pub mod eia;
pub mod lobbying;
pub mod patents;
pub mod wikipedia;

use serde::{Deserialize, Serialize};

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

pub async fn collect_all() -> anyhow::Result<Vec<RawArticle>> {
    // Fetch news and financial sources concurrently
    let (google, rss, hn, usa_spending, fed_register, sbir_awards, sec_edgar, fred_data, fec_data, eia_data, lda_data, patent_data) = tokio::join!(
        google_news::fetch_all(),
        rss_feeds::fetch_all(),
        hacker_news::fetch(),
        usaspending::fetch(),
        federal_register::fetch(),
        sbir::fetch(),
        edgar::fetch(),
        fred::fetch(),
        fec::fetch(),
        eia::fetch(),
        lobbying::fetch(),
        patents::fetch(),
    );

    let mut articles = Vec::new();
    let mut failed_sources = Vec::new();

    // News sources
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
    match sbir_awards {
        Ok(a) if !a.is_empty() => {
            tracing::info!("SBIR: {} financial articles", a.len());
            articles.extend(a);
        }
        Ok(_) => {} // SBIR API frequently returns 404 (deprecated), don't log as failure
        Err(_) => {} // Silently skip — API is deprecated
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
