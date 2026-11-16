//! Reddit source — public Atom feeds (no auth), quality-filtered.
//!
//! Reddit refuses its JSON listings (`/r/<sub>/top.json`) to every client that is
//! not signed in: a 403 page, "blocked by network security". Pulse read those
//! listings, so from 2026-05-27 not one Reddit post reached a briefing. The same
//! listings are still public as Atom feeds (`/r/<sub>/top/.rss`), read here.
//!
//! The feeds allow ONE request a minute. Subreddits that share a listing are
//! therefore read in one request (`/r/a+b+c/top/.rss`): two requests a run, the
//! second after the pause the first answer's rate-limit headers ask for. A
//! combined feed holds the same posts as the separate ones (compared 2026-11-16)
//! and every entry names its subreddit. A subreddit that is gone does not fail
//! the request: it has no entries, which is what its `feed_runs` row then says.
//! The 100 entries a feed can hold are shared by its subreddits, so a listing
//! that grows past a few busy ones should be split, at a minute per request.
//!
//! Per-sub listing choice based on post-type research:
//! - `hot` for news-heavy subs (longevity, Biohackers) — external links dominate
//! - `top?t=day` for discussion-heavy subs — surfaces day's best discussion,
//!   avoids megathreads and stickied noise that pollute `hot`
//!
//! A feed carries no score, comment count, upvote ratio, sticky or NSFW mark, so
//! the thresholds the JSON listings were filtered on cannot be applied. Rank
//! stands in for them: both listings come best first, and only the first
//! `KEEP_PER_SUBREDDIT` usable posts of each subreddit are kept. Still rejected:
//! AutoModerator and deleted authors, removed posts, and image/video domains (we
//! want articles), now with Reddit's own image galleries among them.
//!
//! A post's page on reddit.com is never fetched for more text: Reddit serves a
//! bot challenge there (`article_text::wants_body`). What the feed gives is all.

use super::RawArticle;
use std::collections::HashSet;
use std::time::Duration;

const USER_AGENT: &str = "macos:com.pulse.fetcher:0.1.0 (by /u/nicoladebbia)";

const BASE_URL: &str = "https://www.reddit.com";

/// (subreddit, listing, sector)
const SUBREDDITS: &[(&str, &str, &str)] = &[
    ("financialindependence", "top", "freedom_wealth"),
    ("fatFIRE", "top", "freedom_wealth"),
    ("digitalnomad", "top", "freedom_location"),
    ("expats", "top", "freedom_location"),
    ("productivity", "top", "freedom_time"),
    ("longevity", "hot", "freedom_health"),
    ("Biohackers", "hot", "freedom_health"),
    ("whoop", "hot", "freedom_whoop"),
];

/// The listings, in the order they are requested: one request each.
const LISTINGS: &[&str] = &["top", "hot"];

/// Entries asked of a feed; Reddit gives 100 at most.
const FEED_LIMIT: usize = 100;

/// Posts kept per subreddit and run, taken from the top of its ranking.
const KEEP_PER_SUBREDDIT: usize = 10;

/// Reddit's rate-limit window: the pause after a 429 that names none.
const WINDOW_SECS: u64 = 60;

/// The longest pause taken for the rate limit, whatever the headers ask for.
/// Two requests with a wait before each and a retry each stay inside the
/// source's time budget (`SOURCE_TIMEOUT_SECS`).
const MAX_PAUSE_SECS: u64 = 120;

const BLACKLISTED_DOMAINS: &[&str] = &[
    "i.redd.it", "v.redd.it", "imgur.com", "youtube.com", "youtu.be",
];

/// One feed entry, reduced to what Pulse uses.
#[derive(Debug, Clone, PartialEq)]
struct Post {
    /// As the feed spells it.
    subreddit: String,
    title: String,
    /// The post's own page on Reddit.
    permalink: String,
    /// What a link post points to. `None` for a text post.
    link: Option<String>,
    /// Without the `/u/` the feed puts in front.
    author: String,
    /// The post's own text, tags stripped. Empty when it has none.
    body: String,
    published: Option<chrono::DateTime<chrono::Utc>>,
}

pub async fn fetch() -> anyhow::Result<Vec<RawArticle>> {
    let client = client().build()?;

    let mut articles = Vec::new();
    let mut seen_urls = HashSet::new();
    let mut pause: Option<Duration> = None;

    for listing in LISTINGS {
        let subs: Vec<(&str, &str)> = SUBREDDITS
            .iter()
            .filter(|(_, l, _)| l == listing)
            .map(|(sub, _, sector)| (*sub, *sector))
            .collect();
        if subs.is_empty() { continue; }

        if let Some(wait) = pause.take() {
            tracing::info!("Reddit: waiting {}s before the {} feed (one request a minute)", wait.as_secs(), listing);
            tokio::time::sleep(wait).await;
        }

        let names: Vec<&str> = subs.iter().map(|(sub, _)| *sub).collect();
        let url = feed_url(BASE_URL, listing, &names);
        let started = std::time::Instant::now();
        let (posts, next_pause) = fetch_feed(&client, &url).await;
        pause = next_pause;

        match posts {
            Ok(posts) => {
                let (picked, entries) = pick(&posts, &subs, &mut seen_urls);
                for ((sub, sector), entries) in subs.iter().zip(entries) {
                    crate::feed_runs::note(feed_id(sub), "reddit", &format!("r/{sub}"), sector, started, Ok(entries));
                    let kept = picked.iter().filter(|a| a.feed_id == feed_id(sub)).count();
                    tracing::info!("Reddit r/{}: {} posts kept (of {} in the {} feed)", sub, kept, entries, listing);
                }
                articles.extend(picked);
            }
            Err(e) => {
                for (sub, sector) in &subs {
                    crate::feed_runs::note(feed_id(sub), "reddit", &format!("r/{sub}"), sector, started, Err(&e));
                }
                tracing::warn!("Reddit {} feed failed (non-fatal): {:#}", listing, e);
            }
        }
    }

    Ok(articles)
}

fn client() -> reqwest::ClientBuilder {
    reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .timeout(Duration::from_secs(15))
}

/// The id a subreddit's posts carry, and its row in `feed_runs`.
fn feed_id(sub: &str) -> String {
    format!("reddit:{sub}")
}

/// One listing of several subreddits as a single Atom feed.
fn feed_url(base: &str, listing: &str, subs: &[&str]) -> String {
    let subs = subs.join("+");
    if listing == "top" {
        format!("{base}/r/{subs}/top/.rss?t=day&limit={FEED_LIMIT}")
    } else {
        format!("{base}/r/{subs}/hot/.rss?limit={FEED_LIMIT}")
    }
}

/// Read one feed. A 429 is waited out as its headers ask and the feed is read
/// once more. Also returns the pause to take before the next request.
async fn fetch_feed(client: &reqwest::Client, url: &str) -> (anyhow::Result<Vec<Post>>, Option<Duration>) {
    let mut waited = None;
    loop {
        super::API_CALLS.reddit.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let resp = match client.get(url).send().await {
            Ok(resp) => resp,
            Err(e) => return (Err(e.into()), None),
        };
        let status = resp.status();
        let rate_limited = status == reqwest::StatusCode::TOO_MANY_REQUESTS;
        let pause = pause_after(resp.headers(), rate_limited);

        if rate_limited {
            if let Some(waited) = waited {
                let e = anyhow::anyhow!("Reddit rate-limited (429) again after waiting {waited}s");
                return (Err(e), pause);
            }
            let wait = pause.unwrap_or(Duration::from_secs(WINDOW_SECS));
            tracing::info!("Reddit: rate-limited (429), trying again in {}s", wait.as_secs());
            tokio::time::sleep(wait).await;
            waited = Some(wait.as_secs());
            continue;
        }
        // The page Reddit blocks a client with is not a feed: name the status
        // rather than the parser's complaint about it.
        if !status.is_success() {
            return (Err(anyhow::anyhow!("HTTP {status}")), pause);
        }
        let posts = match resp.bytes().await {
            Ok(body) => parse_feed(&body).map_err(|e| anyhow::anyhow!("HTTP {}: {}", status.as_u16(), e)),
            Err(e) => Err(e.into()),
        };
        return (posts, pause);
    }
}

/// The pause Reddit's rate-limit headers ask for before the next request: none
/// while the window has requests left, otherwise until it ends
/// (`x-ratelimit-reset`, in seconds) and one second more, so the next request
/// lands inside the next window.
fn pause_after(headers: &reqwest::header::HeaderMap, rate_limited: bool) -> Option<Duration> {
    let number = |name: &str| {
        headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.trim().parse::<f64>().ok())
            .filter(|n| n.is_finite())
    };
    let used_up = rate_limited || number("x-ratelimit-remaining").is_some_and(|left| left < 1.0);
    if !used_up {
        return None;
    }
    let secs = number("x-ratelimit-reset")
        .or_else(|| number("retry-after"))
        .unwrap_or(WINDOW_SECS as f64)
        .clamp(0.0, MAX_PAUSE_SECS as f64);
    Some(Duration::from_secs(secs.ceil() as u64 + 1))
}

fn parse_feed(xml: &[u8]) -> anyhow::Result<Vec<Post>> {
    // The entry's HTML as Reddit wrote it: `link_target` and `self_text` read
    // its markup, which the parser's clean-up (when compiled in) rewrites.
    let feed = feed_rs::parser::Builder::new().sanitize_content(false).build().parse(xml)?;
    let posts = feed
        .entries
        .into_iter()
        .filter_map(|entry| {
            let subreddit = entry.categories.first()?.term.clone();
            let title = entry.title.map(|t| t.content).unwrap_or_default();
            let permalink = entry.links.first().map(|l| l.href.clone()).unwrap_or_default();
            if title.is_empty() || permalink.is_empty() {
                return None;
            }
            let html = entry.content.and_then(|c| c.body).unwrap_or_default();
            Some(Post {
                subreddit,
                title,
                // A text post's `[link]` is the post itself.
                link: link_target(&html).filter(|link| link.trim_end_matches('/') != permalink.trim_end_matches('/')),
                permalink,
                // A deleted account leaves the entry without an author.
                author: entry
                    .authors
                    .first()
                    .map(|a| a.name.trim().trim_start_matches("/u/").to_string())
                    .unwrap_or_else(|| "[deleted]".to_string()),
                body: self_text(&html),
                published: entry.published.or(entry.updated),
            })
        })
        .collect();
    Ok(posts)
}

/// Where an entry's `[link]` points: the article for a link post, the post
/// itself for a text post. Reddit writes it after the post's own text, so it is
/// the last one: a link the author labelled "[link]" comes before it.
fn link_target(html: &str) -> Option<String> {
    const HREF: &str = "href=\"";
    let end = html.rfind("\">[link]</a>")?;
    let start = html[..end].rfind(HREF)? + HREF.len();
    Some(html[start..end].replace("&amp;", "&"))
}

/// A post's own text: what Reddit puts in the entry's `md` block, tags stripped.
fn self_text(html: &str) -> String {
    const OPEN: &str = "<div class=\"md\">";
    let Some(start) = html.find(OPEN).map(|at| at + OPEN.len()) else {
        return String::new();
    };
    let end = html[start..].find("<!-- SC_ON -->").map_or(html.len(), |at| start + at);
    crate::article_text::strip_tags(&html[start..end])
}

/// A feed's posts as articles: for each of `subs` (subreddit, sector), its first
/// `KEEP_PER_SUBREDDIT` usable posts in the feed's order, which is its ranking.
/// Also returns how many entries the feed held for each of `subs`, in their order.
fn pick(posts: &[Post], subs: &[(&str, &str)], seen_urls: &mut HashSet<String>) -> (Vec<RawArticle>, Vec<usize>) {
    let mut articles = Vec::new();
    let mut entries = vec![0usize; subs.len()];
    let mut kept = vec![0usize; subs.len()];

    for p in posts {
        // A post from a subreddit nobody asked for has no sector to go to.
        let Some(at) = subs.iter().position(|(sub, _)| sub.eq_ignore_ascii_case(&p.subreddit)) else { continue };
        let (sub, sector) = subs[at];
        entries[at] += 1;
        if kept[at] >= KEEP_PER_SUBREDDIT || !is_quality_post(p) { continue; }

        // Dedupe crossposts by URL across subs: the first subreddit keeps the
        // post, so in `feed_runs` a later one can answer in full and hand over
        // nothing (`entries` 25, `fetched` 0) without anything being wrong.
        let canonical_url = p.link.clone().unwrap_or_else(|| p.permalink.clone());
        if !seen_urls.insert(canonical_url.clone()) { continue; }

        // Build content: (truncated) text of a text post, else the title
        let content_snippet = if p.link.is_none() {
            let truncated: String = p.body.chars().take(2000).collect();
            if truncated.len() < p.body.len() { format!("{}...", truncated) } else { truncated }
        } else {
            p.title.clone()
        };

        articles.push(RawArticle {
            title: p.title.clone(),
            url: canonical_url,
            source_name: format!("r/{sub}"),
            source_url: format!("https://www.reddit.com/r/{sub}"),
            published_at: p.published.map(|dt| dt.to_rfc3339()),
            content_snippet,
            sector: sector.to_string(),
            feed_id: feed_id(sub),
            language: "en".to_string(),
            source_type: "news".to_string(),
            financial_metadata: None,
        });
        kept[at] += 1;
    }

    (articles, entries)
}

/// What can be told from a feed entry alone. It has no score to judge by: the
/// feed's ranking and `KEEP_PER_SUBREDDIT` do that part.
fn is_quality_post(p: &Post) -> bool {
    if p.author == "AutoModerator" || p.author == "[deleted]" { return false; }
    if p.body == "[removed]" || p.body == "[deleted]" { return false; }

    // Blacklist image/video domains (no articles possible)
    if let Some(link) = &p.link {
        let host = url::Url::parse(link).ok().and_then(|u| u.host_str().map(str::to_lowercase)).unwrap_or_default();
        for bad in BLACKLISTED_DOMAINS {
            if host.contains(bad) { return false; }
        }
        // Several images in one post: Reddit's own gallery page, images just the same.
        if crate::article_text::is_reddit_page(link) && link.contains("/gallery/") { return false; }
    }

    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// `feed_runs` holds one row per feed id per run: two feeds sharing an id
    /// would be counted as one.
    #[test]
    fn every_feed_has_its_own_id() {
        let mut seen = std::collections::HashSet::new();
        let repeated: Vec<_> = SUBREDDITS
            .iter()
            .map(|(sub, _, _)| feed_id(sub))
            .filter(|id| !seen.insert(id.clone()))
            .collect();
        assert!(repeated.is_empty(), "feed ids used twice: {repeated:?}");
    }

    /// The JSON listings answer 403 to a client that is not signed in; the Atom
    /// feeds of the same listings are what is still public.
    #[test]
    fn the_atom_feeds_are_requested_not_the_blocked_json_listings() {
        assert_eq!(
            feed_url(BASE_URL, "top", &["fatFIRE", "expats"]),
            "https://www.reddit.com/r/fatFIRE+expats/top/.rss?t=day&limit=100"
        );
        assert_eq!(
            feed_url(BASE_URL, "hot", &["longevity"]),
            "https://www.reddit.com/r/longevity/hot/.rss?limit=100"
        );
    }

    /// A subreddit whose listing is never requested would be dropped without a word.
    #[test]
    fn every_subreddit_is_in_a_listing_that_is_requested() {
        let left_out: Vec<_> = SUBREDDITS.iter().filter(|(_, listing, _)| !LISTINGS.contains(listing)).collect();
        assert!(left_out.is_empty(), "never requested: {left_out:?}");
    }

    const TEXT_POST: &str = "https://www.reddit.com/r/digitalnomad/comments/1aaaaaa/best_value_city_this_winter/";
    const LINK_POST: &str = "https://www.reddit.com/r/longevity/comments/1bbbbbb/trial_results_are_in/";
    const BARE_POST: &str = "https://www.reddit.com/r/expats/comments/1cccccc/a_question_in_the_title_only/";

    /// Three entries with the markup Reddit's feeds had on 2026-11-16 (the people
    /// and posts are made up): a text post, a link post with a thumbnail, and a
    /// text post that is a title and nothing else.
    fn sample_feed() -> String {
        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?><feed xmlns="http://www.w3.org/2005/Atom" xmlns:media="http://search.yahoo.com/mrss/"><category term="multi" label="r/multi"/><updated>2026-11-16T17:00:51+00:00</updated><id>/r/digitalnomad+longevity+expats/top/.rss?t=day&amp;limit=100</id><title>posts from digitalnomad, longevity, expats</title><entry><author><name>/u/slow_traveller</name><uri>https://www.reddit.com/user/slow_traveller</uri></author><category term="digitalnomad" label="r/digitalnomad"/><content type="html">&lt;!-- SC_OFF --&gt;&lt;div class=&quot;md&quot;&gt;&lt;p&gt;Last winter&amp;#39;s thread was useful, so here is this year&amp;#39;s.&lt;/p&gt; &lt;p&gt;Rent &amp;amp; food under $1,500 a month, please.&lt;/p&gt; &lt;/div&gt;&lt;!-- SC_ON --&gt; &amp;#32; submitted by &amp;#32; &lt;a href=&quot;https://www.reddit.com/user/slow_traveller&quot;&gt; /u/slow_traveller &lt;/a&gt; &amp;#32; to &amp;#32; &lt;a href=&quot;https://www.reddit.com/r/digitalnomad/&quot;&gt; r/digitalnomad &lt;/a&gt; &lt;br/&gt; &lt;span&gt;&lt;a href=&quot;{TEXT_POST}&quot;&gt;[link]&lt;/a&gt;&lt;/span&gt; &amp;#32; &lt;span&gt;&lt;a href=&quot;{TEXT_POST}&quot;&gt;[comments]&lt;/a&gt;&lt;/span&gt;</content><id>t3_1aaaaaa</id><link href="{TEXT_POST}" /><updated>2026-11-15T17:38:45+00:00</updated><published>2026-11-15T17:38:45+00:00</published><title>Best value city this winter?</title></entry><entry><author><name>/u/trial_watcher</name><uri>https://www.reddit.com/user/trial_watcher</uri></author><category term="longevity" label="r/longevity"/><content type="html">&lt;table&gt; &lt;tr&gt;&lt;td&gt; &lt;a href=&quot;{LINK_POST}&quot;&gt; &lt;img src=&quot;https://external-preview.redd.it/abc.jpeg?width=320&amp;amp;crop=smart&amp;amp;auto=webp&quot; alt=&quot;Trial results are in&quot; title=&quot;Trial results are in&quot; /&gt; &lt;/a&gt; &lt;/td&gt;&lt;td&gt; &amp;#32; submitted by &amp;#32; &lt;a href=&quot;https://www.reddit.com/user/trial_watcher&quot;&gt; /u/trial_watcher &lt;/a&gt; &amp;#32; to &amp;#32; &lt;a href=&quot;https://www.reddit.com/r/longevity/&quot;&gt; r/longevity &lt;/a&gt; &lt;br/&gt; &lt;span&gt;&lt;a href=&quot;https://journal.example.org/article?id=42&amp;amp;ref=feed&quot;&gt;[link]&lt;/a&gt;&lt;/span&gt; &amp;#32; &lt;span&gt;&lt;a href=&quot;{LINK_POST}&quot;&gt;[comments]&lt;/a&gt;&lt;/span&gt; &lt;/td&gt;&lt;/tr&gt;&lt;/table&gt;</content><id>t3_1bbbbbb</id><media:thumbnail url="https://external-preview.redd.it/abc.jpeg?width=320&amp;crop=smart&amp;auto=webp" /><link href="{LINK_POST}" /><updated>2026-11-15T15:19:23+00:00</updated><published>2026-11-15T15:19:23+00:00</published><title>Trial results are in</title></entry><entry><author><name>/u/new_in_town</name><uri>https://www.reddit.com/user/new_in_town</uri></author><category term="expats" label="r/expats"/><content type="html">&amp;#32; submitted by &amp;#32; &lt;a href=&quot;https://www.reddit.com/user/new_in_town&quot;&gt; /u/new_in_town &lt;/a&gt; &amp;#32; to &amp;#32; &lt;a href=&quot;https://www.reddit.com/r/expats/&quot;&gt; r/expats &lt;/a&gt; &lt;br/&gt; &lt;span&gt;&lt;a href=&quot;{BARE_POST}&quot;&gt;[link]&lt;/a&gt;&lt;/span&gt; &amp;#32; &lt;span&gt;&lt;a href=&quot;{BARE_POST}&quot;&gt;[comments]&lt;/a&gt;&lt;/span&gt;</content><id>t3_1cccccc</id><link href="{BARE_POST}" /><updated>2026-11-16T07:56:04+00:00</updated><published>2026-11-16T07:56:04+00:00</published><title>A question in the title only</title></entry></feed>"#
        )
    }

    #[test]
    fn a_reddit_feed_is_read_into_posts() {
        let posts = parse_feed(sample_feed().as_bytes()).expect("the sample is a feed");
        assert_eq!(posts.len(), 3);

        let text = &posts[0];
        assert_eq!(text.subreddit, "digitalnomad");
        assert_eq!(text.title, "Best value city this winter?");
        assert_eq!(text.permalink, TEXT_POST);
        assert_eq!(text.link, None, "a text post links to itself");
        assert_eq!(text.author, "slow_traveller");
        assert_eq!(
            text.body,
            "Last winter's thread was useful, so here is this year's. Rent & food under $1,500 a month, please."
        );
        assert_eq!(text.published.map(|dt| dt.to_rfc3339()).as_deref(), Some("2026-11-15T17:38:45+00:00"));

        let link = &posts[1];
        assert_eq!(link.subreddit, "longevity");
        assert_eq!(link.permalink, LINK_POST);
        assert_eq!(link.link.as_deref(), Some("https://journal.example.org/article?id=42&ref=feed"));
        assert_eq!(link.body, "");

        let bare = &posts[2];
        assert_eq!((bare.subreddit.as_str(), bare.link.as_deref(), bare.body.as_str()), ("expats", None, ""));
    }

    #[test]
    fn a_link_the_author_called_link_is_not_taken_for_the_posts_own() {
        let html = r#"<!-- SC_OFF --><div class="md"><p>Read <a href="https://elsewhere.example/">[link]</a> first.</p> </div><!-- SC_ON --> &#32; submitted by &#32; <a href="https://www.reddit.com/user/someone"> /u/someone </a> <br/> <span><a href="https://example.org/study?a=1&amp;b=2">[link]</a></span> &#32; <span><a href="https://www.reddit.com/r/longevity/comments/1/post/">[comments]</a></span>"#;
        assert_eq!(link_target(html).as_deref(), Some("https://example.org/study?a=1&b=2"));
        assert_eq!(self_text(html), "Read [link] first.");
        assert_eq!(link_target("<p>no anchors here</p>"), None);
    }

    /// The page Reddit blocks a client with must come out as an error, not as an
    /// empty feed that reads like a quiet day.
    #[test]
    fn a_page_that_is_not_a_feed_is_an_error() {
        let blocked = b"<body class=theme-beta><div>You've been blocked by network security.</div></body>";
        assert!(parse_feed(blocked).is_err());
    }

    fn post(sub: &str, n: usize) -> Post {
        Post {
            subreddit: sub.to_string(),
            title: format!("{sub} post {n}"),
            permalink: format!("https://www.reddit.com/r/{sub}/comments/{n}/post/"),
            link: None,
            author: "someone".to_string(),
            body: format!("What {sub} post {n} says."),
            published: "2026-11-15T17:38:45Z".parse().ok(),
        }
    }

    #[test]
    fn a_post_becomes_an_article_of_its_subreddits_sector() {
        let text = post("fatfire", 1); // the feed's spelling need not match ours
        let link = Post { link: Some("https://example.org/study".to_string()), ..post("longevity", 2) };
        let stray = post("somewhere_else", 3);
        let subs = [("fatFIRE", "freedom_wealth"), ("longevity", "freedom_health"), ("whoop", "freedom_whoop")];

        let (articles, entries) = pick(&[text, link, stray], &subs, &mut HashSet::new());

        assert_eq!(entries, vec![1, 1, 0], "entries per subreddit, in the order asked");
        assert_eq!(articles.len(), 2, "a post from a subreddit nobody asked for is left out");

        let a = &articles[0];
        assert_eq!(a.url, "https://www.reddit.com/r/fatfire/comments/1/post/");
        assert_eq!((a.source_name.as_str(), a.source_url.as_str()), ("r/fatFIRE", "https://www.reddit.com/r/fatFIRE"));
        assert_eq!((a.sector.as_str(), a.feed_id.as_str()), ("freedom_wealth", "reddit:fatFIRE"));
        assert_eq!(a.content_snippet, "What fatfire post 1 says.");
        assert_eq!(a.published_at.as_deref(), Some("2026-11-15T17:38:45+00:00"));

        let b = &articles[1];
        assert_eq!(b.url, "https://example.org/study", "a link post goes to the article");
        assert_eq!((b.sector.as_str(), b.feed_id.as_str()), ("freedom_health", "reddit:longevity"));
        assert_eq!(b.content_snippet, "longevity post 2");
    }

    #[test]
    fn a_long_text_post_is_cut_to_2000_characters() {
        let long = Post { body: "è".repeat(2500), ..post("expats", 1) };
        let (articles, _) = pick(&[long], &[("expats", "freedom_location")], &mut HashSet::new());
        assert_eq!(articles[0].content_snippet, format!("{}...", "è".repeat(2000)));
    }

    #[test]
    fn posts_with_nothing_to_read_are_rejected() {
        let linked = |to: &str| Post { link: Some(to.to_string()), ..post("whoop", 1) };

        assert!(is_quality_post(&post("whoop", 1)));
        assert!(is_quality_post(&linked("https://www.statnews.com/2026/11/16/a-story/")));

        assert!(!is_quality_post(&Post { author: "AutoModerator".into(), ..post("whoop", 1) }));
        assert!(!is_quality_post(&Post { author: "[deleted]".into(), ..post("whoop", 1) }));
        assert!(!is_quality_post(&Post { body: "[removed]".into(), ..post("whoop", 1) }));
        assert!(!is_quality_post(&Post { body: "[deleted]".into(), ..post("whoop", 1) }));
        assert!(!is_quality_post(&linked("https://i.redd.it/abc123.jpeg")));
        assert!(!is_quality_post(&linked("https://v.redd.it/abc123")));
        assert!(!is_quality_post(&linked("https://i.imgur.com/abc123.png")));
        assert!(!is_quality_post(&linked("https://www.youtube.com/watch?v=abc&t=1s")));
        assert!(!is_quality_post(&linked("https://youtu.be/abc")));
        assert!(!is_quality_post(&linked("https://www.reddit.com/gallery/1abc23")));
        // A link to another Reddit post (a crosspost) is still something to read.
        assert!(is_quality_post(&linked("https://www.reddit.com/r/longevity/comments/1abc/a_post/")));
    }

    /// The feed has no scores, so its ranking is the quality bar: a busy
    /// subreddit hands over its best-ranked posts and no more, and does not
    /// crowd out a quiet one.
    #[test]
    fn each_subreddit_keeps_only_its_best_ranked_posts() {
        let mut posts = vec![Post { author: "AutoModerator".into(), ..post("whoop", 0) }];
        posts.extend((1..=15).map(|n| post("whoop", n)));
        posts.push(post("longevity", 1));
        let subs = [("longevity", "freedom_health"), ("whoop", "freedom_whoop")];

        let (articles, entries) = pick(&posts, &subs, &mut HashSet::new());

        assert_eq!(entries, vec![1, 16], "every entry the feed held is counted, kept or not");
        let whoop: Vec<_> = articles.iter().filter(|a| a.feed_id == "reddit:whoop").map(|a| a.title.as_str()).collect();
        let expected: Vec<_> = (1..=KEEP_PER_SUBREDDIT).map(|n| format!("whoop post {n}")).collect();
        assert_eq!(whoop, expected, "the first usable posts, in the feed's order");
        assert_eq!(articles.iter().filter(|a| a.feed_id == "reddit:longevity").count(), 1);
    }

    #[test]
    fn a_link_posted_to_two_subreddits_is_kept_once() {
        let study = Some("https://example.org/study".to_string());
        let first = Post { link: study.clone(), ..post("longevity", 1) };
        let again = Post { link: study, ..post("Biohackers", 2) };
        let subs = [("longevity", "freedom_health"), ("Biohackers", "freedom_health")];
        let mut seen = HashSet::new();

        let (articles, entries) = pick(&[first, again.clone()], &subs, &mut seen);
        assert_eq!(entries, vec![1, 1]);
        assert_eq!(articles.iter().map(|a| a.feed_id.as_str()).collect::<Vec<_>>(), vec!["reddit:longevity"]);

        // ... and across the two feeds of a run, which share the set.
        let (later, _) = pick(&[again], &subs, &mut seen);
        assert!(later.is_empty());
    }

    fn headers(pairs: &[(&'static str, &'static str)]) -> reqwest::header::HeaderMap {
        pairs.iter().map(|(k, v)| (reqwest::header::HeaderName::from_static(k), reqwest::header::HeaderValue::from_static(v))).collect()
    }

    #[test]
    fn the_next_request_waits_for_the_next_rate_limit_window() {
        let secs = |pairs, rate_limited| pause_after(&headers(pairs), rate_limited).map(|d| d.as_secs());

        // What Reddit sends with a feed today: the one request of this minute is used.
        let used = [("x-ratelimit-used", "1"), ("x-ratelimit-remaining", "0.0"), ("x-ratelimit-reset", "56")];
        assert_eq!(secs(&used, false), Some(57));
        // Requests left in the window, or no word on it: go on.
        assert_eq!(secs(&[("x-ratelimit-remaining", "9.0"), ("x-ratelimit-reset", "56")], false), None);
        assert_eq!(secs(&[], false), None);
        // A 429 always waits: for what it names, else a whole window.
        assert_eq!(secs(&[("x-ratelimit-remaining", "0.0"), ("x-ratelimit-reset", "7")], true), Some(8));
        assert_eq!(secs(&[("retry-after", "30")], true), Some(31));
        assert_eq!(secs(&[], true), Some(WINDOW_SECS + 1));
        // Never longer than the cap, and nonsense is not obeyed.
        assert_eq!(secs(&[("x-ratelimit-reset", "86400")], true), Some(MAX_PAUSE_SECS + 1));
        assert_eq!(secs(&[("x-ratelimit-reset", "-5")], true), Some(1));
        assert_eq!(secs(&[("x-ratelimit-reset", "soon")], true), Some(WINDOW_SECS + 1));
        assert_eq!(secs(&[("x-ratelimit-reset", "NaN")], true), Some(WINDOW_SECS + 1));
    }

    fn reply(status: &str, extra_headers: &str, body: &str) -> String {
        format!("HTTP/1.1 {status}\r\n{extra_headers}content-length: {}\r\nconnection: close\r\n\r\n{body}", body.len())
    }

    /// A stand-in for Reddit on this machine: answers one connection with each of
    /// `replies` in turn, and hands back the requests it was sent.
    async fn stand_in(replies: Vec<String>) -> (String, tokio::task::JoinHandle<Vec<String>>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let served = tokio::spawn(async move {
            let mut requests = Vec::new();
            for reply in replies {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut head = Vec::new();
                let mut buf = [0u8; 1024];
                while !head.windows(4).any(|w| w == b"\r\n\r\n") {
                    let n = socket.read(&mut buf).await.unwrap();
                    if n == 0 { break; }
                    head.extend_from_slice(&buf[..n]);
                }
                requests.push(String::from_utf8_lossy(&head).to_lowercase());
                socket.write_all(reply.as_bytes()).await.unwrap();
                socket.shutdown().await.ok();
            }
            requests
        });
        (base, served)
    }

    /// The requests the stand-in was sent. It waits for one request per reply, so
    /// a client that sent fewer must fail the test, not hang it.
    async fn requests(served: tokio::task::JoinHandle<Vec<String>>) -> Vec<String> {
        tokio::time::timeout(Duration::from_secs(10), served)
            .await
            .expect("fewer requests were sent than the stand-in had replies for")
            .unwrap()
    }

    fn local_client() -> reqwest::Client {
        // No proxy: the stand-in is on this machine.
        client().no_proxy().build().unwrap()
    }

    #[tokio::test]
    async fn a_rate_limited_feed_is_read_again_after_the_wait_it_names() {
        let (base, served) = stand_in(vec![
            reply("429 Too Many Requests", "x-ratelimit-remaining: 0.0\r\nx-ratelimit-reset: 0\r\n", ""),
            reply("200 OK", "x-ratelimit-remaining: 0.0\r\nx-ratelimit-reset: 41\r\n", &sample_feed()),
        ])
        .await;
        let url = feed_url(&base, "top", &["digitalnomad", "longevity", "expats"]);

        let started = std::time::Instant::now();
        let (posts, pause) = fetch_feed(&local_client(), &url).await;

        let posts = posts.expect("the second answer is a feed");
        assert_eq!(posts.len(), 3);
        assert_eq!(pause, Some(Duration::from_secs(42)), "the pause the last answer asks for");
        assert!(started.elapsed() >= Duration::from_secs(1), "the 429 was waited out before asking again");

        let requests = requests(served).await;
        assert_eq!(requests.len(), 2, "asked once more after the 429");
        for request in &requests {
            assert!(request.starts_with("get /r/digitalnomad+longevity+expats/top/.rss?t=day&limit=100 "), "{request}");
            // Reddit turns away clients that do not say who they are.
            assert!(request.contains(&format!("user-agent: {}", USER_AGENT.to_lowercase())), "{request}");
        }
    }

    #[tokio::test]
    async fn a_feed_that_stays_rate_limited_is_an_error_not_a_loop() {
        let limited = || reply("429 Too Many Requests", "x-ratelimit-reset: 0\r\n", "");
        let (base, served) = stand_in(vec![limited(), limited()]).await;

        let (posts, _) = fetch_feed(&local_client(), &feed_url(&base, "hot", &["whoop"])).await;

        assert!(format!("{:#}", posts.unwrap_err()).contains("429"));
        assert_eq!(requests(served).await.len(), 2);
    }

    #[tokio::test]
    async fn a_blocked_feed_is_reported_with_its_status() {
        let blocked = "<body>You've been blocked by network security.</body>";
        let (base, _served) = stand_in(vec![reply("403 Forbidden", "retry-after: 0\r\n", blocked)]).await;

        let (posts, pause) = fetch_feed(&local_client(), &feed_url(&base, "hot", &["whoop"])).await;

        assert_eq!(format!("{:#}", posts.unwrap_err()), "HTTP 403 Forbidden");
        assert_eq!(pause, None);
    }

    /// Network test. Run explicitly:
    /// `cargo test -p pulse-fetcher -- --ignored --nocapture reddit_still_answers`
    ///
    /// Reddit closed the JSON listings without notice and nothing offline can
    /// tell when it changes the feeds. Two requests, about a minute apart.
    #[tokio::test]
    #[ignore = "makes live network calls"]
    async fn reddit_still_answers() {
        let articles = fetch().await.expect("fetch reports failures per feed, not as an error");

        for (sub, listing, _) in SUBREDDITS {
            let n = articles.iter().filter(|a| a.feed_id == feed_id(sub)).count();
            println!("  r/{sub:<24} {listing:<4} {n}");
        }
        for listing in LISTINGS {
            let got = SUBREDDITS
                .iter()
                .filter(|(_, l, _)| l == listing)
                .any(|(sub, _, _)| articles.iter().any(|a| a.feed_id == feed_id(sub)));
            assert!(got, "no post from any subreddit of the {listing} feed");
        }
    }
}
