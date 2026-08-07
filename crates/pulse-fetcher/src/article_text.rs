//! Fetch real article body text so the summarizer stops writing from title +
//! 1-2 sentence snippet. The measured failure (2026-08-06 A/B, see
//! .plans/summarize-ab-8b-vs-scout.md): snippet-only input makes every model
//! pad summaries with fabricated specifics (invented journal citations,
//! made-up court details). Grounding the input is the fix; model choice isn't.
//!
//! Best-effort by design: many publishers block bots, RSS URLs bounce through
//! redirects, timeouts happen. On any failure the article keeps its snippet —
//! a fetch problem must never cost a story its place in the briefing.

use crate::sources::RawArticle;
use futures::StreamExt;

/// Max chars of extracted body appended to the snippet. Roughly 400 tokens —
/// enough to ground the summary, cheap enough at 8B prices (~+$0.005/day for
/// 340 stories).
const BODY_CAP: usize = 1600;
/// Paragraphs shorter than this are usually nav/cookie/boilerplate.
const MIN_PARAGRAPH_CHARS: usize = 80;
const FETCH_CONCURRENCY: usize = 8;

/// Extract readable paragraph text from raw HTML: the contents of <p>…</p>
/// blocks, tags stripped, entities minimally decoded, boilerplate-length
/// filtered, joined until BODY_CAP. Pure function — unit-tested below.
pub fn extract_paragraphs(html: &str) -> String {
    let mut out = String::new();
    let lower = html.to_lowercase();
    let mut pos = 0usize;
    while let Some(start_rel) = lower[pos..].find("<p") {
        let start = pos + start_rel;
        // Must be `<p>` or `<p ...>`, not `<path>`/`<pre>` etc.
        let after = lower.as_bytes().get(start + 2).copied();
        if !matches!(after, Some(b'>') | Some(b' ') | Some(b'\t') | Some(b'\n')) {
            pos = start + 2;
            continue;
        }
        let Some(open_end_rel) = lower[start..].find('>') else { break };
        let content_start = start + open_end_rel + 1;
        let Some(close_rel) = lower[content_start..].find("</p") else { break };
        let content = &html[content_start..content_start + close_rel];
        pos = content_start + close_rel + 3;

        // Strip inner tags (<a>, <em>, …) and decode the common entities.
        let mut text = String::with_capacity(content.len());
        let mut in_tag = false;
        for ch in content.chars() {
            match ch {
                '<' => in_tag = true,
                '>' => in_tag = false,
                c if !in_tag => text.push(c),
                _ => {}
            }
        }
        let text = text
            .replace("&amp;", "&")
            .replace("&quot;", "\"")
            .replace("&#39;", "'")
            .replace("&apos;", "'")
            .replace("&nbsp;", " ")
            .replace("&lt;", "<")
            .replace("&gt;", ">");
        let text = text.split_whitespace().collect::<Vec<_>>().join(" ");

        if text.len() >= MIN_PARAGRAPH_CHARS {
            if !out.is_empty() {
                out.push(' ');
            }
            out.push_str(&text);
            if out.len() >= BODY_CAP {
                out.truncate(BODY_CAP);
                break;
            }
        }
    }
    out
}

async fn fetch_body(client: &reqwest::Client, url: &str) -> Option<String> {
    let resp = client.get(url).send().await.ok()?;
    if !resp.status().is_success() {
        return None;
    }
    // Skip non-HTML (PDFs, images) early.
    if let Some(ct) = resp.headers().get("content-type") {
        let ct = ct.to_str().unwrap_or("");
        if !ct.is_empty() && !ct.contains("html") && !ct.contains("xml") {
            return None;
        }
    }
    let html = resp.text().await.ok()?;
    let text = extract_paragraphs(&html);
    if text.len() >= MIN_PARAGRAPH_CHARS { Some(text) } else { None }
}

/// Fetch article bodies concurrently and append them to each article's
/// content_snippet (which the summarize prompt already sends). Best-effort:
/// articles whose fetch fails are returned unchanged.
pub async fn enrich(mut articles: Vec<RawArticle>) -> Vec<RawArticle> {
    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .connect_timeout(std::time::Duration::from_secs(5))
        .redirect(reqwest::redirect::Policy::limited(5))
        .user_agent("Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) pulse-fetcher/1.0")
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!("article_text: client build failed ({}) — summarizing from snippets", e);
            return articles;
        }
    };

    let bodies: Vec<(usize, String)> = futures::stream::iter(
        articles
            .iter()
            .enumerate()
            .filter(|(_, a)| {
                a.source_type == "news"
                    && !a.url.is_empty()
                    // Already grounded at parse time (RSS content:encoded).
                    && !a.content_snippet.contains("\n\nArticle text: ")
            })
            .map(|(i, a)| {
                let client = client.clone();
                let url = a.url.clone();
                async move { fetch_body(&client, &url).await.map(|b| (i, b)) }
            }),
    )
    .buffer_unordered(FETCH_CONCURRENCY)
    .filter_map(|r| async move { r })
    .collect()
    .await;

    let fetched = bodies.len();
    for (i, body) in bodies {
        let a = &mut articles[i];
        // Append, don't replace: the RSS snippet is verified-relevant, the
        // scraped body might be a paywall stub — keep both, snippet first.
        a.content_snippet = format!("{}\n\nArticle text: {}", a.content_snippet, body);
    }
    tracing::info!(
        "article_text: fetched real body for {}/{} articles (rest summarize from snippet)",
        fetched,
        articles.len()
    );
    articles
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_paragraphs_strips_tags_and_boilerplate() {
        let html = r#"<html><head><title>x</title></head><body>
            <p class="nav">Menu</p>
            <p>The European Commission announced <a href="/x">new rules</a> on Thursday that will require all AI providers operating in the EU to disclose training data sources.</p>
            <pre>code block not a paragraph</pre>
            <p>Cookie policy</p>
            <p>Industry groups responded that the timeline is &quot;unworkable&quot; &amp; warned of compliance costs exceeding early estimates.</p>
        </body></html>"#;
        let text = extract_paragraphs(html);
        assert!(text.contains("European Commission announced new rules"));
        assert!(text.contains("\"unworkable\" & warned"));
        assert!(!text.contains("Menu"));
        assert!(!text.contains("Cookie policy"));
        assert!(!text.contains("code block"));
    }

    #[test]
    fn caps_output_length() {
        let para = format!("<p>{}</p>", "word ".repeat(200));
        let html = para.repeat(20);
        let text = extract_paragraphs(&html);
        assert!(text.len() <= BODY_CAP);
    }

    #[test]
    fn empty_on_paragraphless_html() {
        assert_eq!(extract_paragraphs("<div>short</div>"), "");
    }
}


