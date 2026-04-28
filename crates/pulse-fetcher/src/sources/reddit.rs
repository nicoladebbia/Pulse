//! Reddit source — public JSON API (no auth), quality-filtered.
//!
//! Per-sub listing choice based on post-type research:
//! - `hot` for news-heavy subs (longevity, Biohackers) — external links dominate
//! - `top?t=day` for discussion-heavy subs — surfaces day's best discussion,
//!   avoids megathreads and stickied noise that pollute `hot`
//!
//! Quality filter rejects: stickied, AutoModerator, NSFW, removed, low-ratio,
//! low-comment posts. Image/video domains blacklisted (we want articles).

use super::RawArticle;
use serde::Deserialize;

const USER_AGENT: &str = "macos:com.pulse.fetcher:0.1.0 (by /u/nicoladebbia)";

/// (subreddit, listing, sector, filter_tier)
/// filter_tier: "strict" (research/news subs) or "relaxed" (discussion subs)
const SUBREDDITS: &[(&str, &str, &str, &str)] = &[
    ("financialindependence", "top", "freedom_wealth", "relaxed"),
    ("fatFIRE", "top", "freedom_wealth", "relaxed"),
    ("digitalnomad", "top", "freedom_location", "relaxed"),
    ("expats", "top", "freedom_location", "relaxed"),
    ("productivity", "top", "freedom_time", "relaxed"),
    ("longevity", "hot", "freedom_health", "strict"),
    ("Biohackers", "hot", "freedom_health", "strict"),
    ("whoop", "hot", "freedom_whoop", "relaxed"),
];

const BLACKLISTED_DOMAINS: &[&str] = &[
    "i.redd.it", "v.redd.it", "imgur.com", "youtube.com", "youtu.be",
];

#[derive(Deserialize)]
struct RedditListing {
    data: RedditListingData,
}

#[derive(Deserialize)]
struct RedditListingData {
    children: Vec<RedditChild>,
}

#[derive(Deserialize)]
struct RedditChild {
    data: RedditPost,
}

#[derive(Deserialize, Default)]
struct RedditPost {
    title: String,
    #[serde(default)]
    selftext: String,
    url: String,
    permalink: String,
    author: String,
    subreddit: String,
    domain: String,
    score: i64,
    num_comments: i64,
    upvote_ratio: f64,
    created_utc: f64,
    #[serde(default)]
    is_self: bool,
    #[serde(default)]
    stickied: bool,
    #[serde(default)]
    over_18: bool,
    #[serde(default)]
    removed_by_category: Option<String>,
}

pub async fn fetch() -> anyhow::Result<Vec<RawArticle>> {
    let client = reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .timeout(std::time::Duration::from_secs(15))
        .build()?;

    let mut articles = Vec::new();
    let mut seen_urls = std::collections::HashSet::new();

    for (sub, listing, sector, tier) in SUBREDDITS {
        let url = if *listing == "top" {
            format!("https://www.reddit.com/r/{}/top.json?limit=25&t=day&raw_json=1", sub)
        } else {
            format!("https://www.reddit.com/r/{}/hot.json?limit=25&raw_json=1", sub)
        };

        match fetch_listing(&client, &url).await {
            Ok(posts) => {
                let mut kept = 0;
                for p in posts {
                    if !is_quality_post(&p, tier) { continue; }

                    // Dedupe crossposts by URL across subs
                    let canonical_url = if p.is_self {
                        format!("https://www.reddit.com{}", p.permalink)
                    } else {
                        p.url.clone()
                    };
                    if !seen_urls.insert(canonical_url.clone()) { continue; }

                    // Build content: title + (truncated selftext if self-post, else url)
                    let content_snippet = if p.is_self {
                        let truncated: String = p.selftext.chars().take(2000).collect();
                        if truncated.len() < p.selftext.len() { format!("{}...", truncated) } else { truncated }
                    } else {
                        p.title.clone()
                    };

                    let published_at = chrono::DateTime::<chrono::Utc>::from_timestamp(p.created_utc as i64, 0)
                        .map(|dt| dt.to_rfc3339());

                    articles.push(RawArticle {
                        title: p.title.clone(),
                        url: canonical_url,
                        source_name: format!("r/{}", p.subreddit),
                        source_url: format!("https://www.reddit.com/r/{}", p.subreddit),
                        published_at,
                        content_snippet,
                        sector: sector.to_string(),
                        feed_id: format!("reddit:{}", sub),
                        language: "en".to_string(),
                        source_type: "news".to_string(),
                        financial_metadata: None,
                    });
                    kept += 1;
                }
                tracing::info!("Reddit r/{}: {} quality posts (from 25 fetched)", sub, kept);
            }
            Err(e) => {
                tracing::warn!("Reddit r/{} fetch failed (non-fatal): {}", sub, e);
            }
        }
    }

    Ok(articles)
}

async fn fetch_listing(client: &reqwest::Client, url: &str) -> anyhow::Result<Vec<RedditPost>> {
    super::API_CALLS.reddit.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let resp = client.get(url).send().await?;

    if resp.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
        let retry = resp.headers().get("retry-after")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("unknown");
        anyhow::bail!("Reddit rate-limited (429), retry-after: {}", retry);
    }

    let listing: RedditListing = resp.error_for_status()?.json().await?;
    Ok(listing.data.children.into_iter().map(|c| c.data).collect())
}

fn is_quality_post(p: &RedditPost, tier: &str) -> bool {
    // Universal rejection rules — always apply
    if p.stickied { return false; }
    if p.author == "AutoModerator" || p.author == "[deleted]" { return false; }
    if p.over_18 { return false; }
    if p.removed_by_category.is_some() { return false; }
    if p.selftext == "[removed]" || p.selftext == "[deleted]" { return false; }
    if p.upvote_ratio < 0.75 { return false; }

    // Blacklist image/video domains (no articles possible)
    for bad in BLACKLISTED_DOMAINS {
        if p.domain.contains(bad) { return false; }
    }

    // Tier-based thresholds
    // "strict" — research/news subs (longevity, Biohackers): require real upvote momentum
    // "relaxed" — discussion subs (financialindependence, digitalnomad): top-of-day
    //            posts often sit at <20 score due to sub size / timing
    let (min_score, min_comments) = if tier == "strict" {
        (20, 5)
    } else {
        (5, 2)
    };

    if p.score < min_score { return false; }
    if p.num_comments < min_comments { return false; }

    true
}
