//! arXiv ingest for the research lane.
//!
//! Uses the arXiv API (Atom), not the RSS feeds `sources/arxiv.rs` reads: the API
//! carries the versioned id, every category, the authors, and the author comment
//! ("26 pages, 5 figures") — none of which survive the RSS path.
//!
//! arXiv's terms ask for at most one API request every 3 seconds and for bots to
//! use export.arxiv.org. Both are honoured here.

use anyhow::Context;
use quick_xml::events::Event;
use quick_xml::Reader;
use std::time::Duration;

const API_BASE: &str = "https://export.arxiv.org/api/query";
const REQUEST_SPACING: Duration = Duration::from_secs(3);
const PAGE_SIZE: usize = 100;

/// The trading-relevant q-fin categories. q-fin.GN / econ.GN stay in the news
/// feed (`sources/arxiv.rs`); they are policy commentary far more than method.
pub const QFIN_CATEGORIES: &[&str] = &["q-fin.TR", "q-fin.ST", "q-fin.CP", "q-fin.PM", "q-fin.RM"];

/// cs.LG / stat.ML are 50x the volume of q-fin, so only papers whose abstract
/// names a market concept are pulled from them. EXACT PHRASES ONLY: arXiv stems
/// `abs:` terms, so `abs:trading` matches every "trade-off" in ML (11.4k hits vs
/// 771 for `abs:portfolio`, measured 2026-09-24) and `abs:alpha` matches the
/// hyperparameter. This form measured ~40 papers/month, mostly on-topic.
const ML_FINANCE_QUERY: &str = "(cat:cs.LG OR cat:stat.ML) AND (abs:portfolio OR abs:\"asset pricing\" OR abs:\"stock returns\" OR abs:\"stock market\" OR abs:\"algorithmic trading\" OR abs:\"quantitative trading\" OR abs:\"limit order book\" OR abs:\"market microstructure\" OR abs:\"return predictability\" OR abs:\"financial markets\")";

#[derive(Debug, Clone, PartialEq)]
pub struct ArxivPaper {
    /// Versionless id, e.g. "2609.27636".
    pub arxiv_id: String,
    pub version: i64,
    pub title: String,
    pub authors: Vec<String>,
    /// Primary category first, then the rest in feed order, no duplicates.
    pub categories: Vec<String>,
    pub abstract_text: String,
    pub pages: Option<i64>,
    /// RFC 3339, as the feed states it.
    pub published_at: String,
}

/// Split "http://arxiv.org/abs/2609.27636v2" into ("2609.27636", 2).
/// Old-style ids ("math/0601001v1") keep their slash.
fn split_id(raw: &str) -> Option<(String, i64)> {
    let tail = raw.trim().split("/abs/").nth(1)?;
    match tail.rfind('v') {
        Some(i) if tail[i + 1..].chars().all(|c| c.is_ascii_digit()) && i + 1 < tail.len() => {
            Some((tail[..i].to_string(), tail[i + 1..].parse().ok()?))
        }
        _ => Some((tail.to_string(), 1)),
    }
}

/// "26 pages, 5 figures" -> 26. Takes the number directly before "page".
fn pages_from_comment(comment: &str) -> Option<i64> {
    let re = regex::Regex::new(r"(?i)(\d+)\s*(?:pages?|pp\.?)\b").ok()?;
    re.captures(comment)?.get(1)?.as_str().parse().ok()
}

fn collapse_ws(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Parse one arXiv API Atom response. Entries missing an id or title are dropped
/// (they cannot be stored); duplicates within the response are collapsed.
pub fn parse_feed(xml: &str) -> anyhow::Result<Vec<ArxivPaper>> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);

    #[derive(Default)]
    struct Draft {
        id: String,
        title: String,
        summary: String,
        published: String,
        authors: Vec<String>,
        primary: Option<String>,
        categories: Vec<String>,
        comment: String,
    }

    let mut out: Vec<ArxivPaper> = Vec::new();
    let mut draft: Option<Draft> = None;
    let mut path: Vec<String> = Vec::new();

    let decoder = reader.decoder();
    let attr = |e: &quick_xml::events::BytesStart, key: &[u8]| -> Option<String> {
        e.attributes()
            .flatten()
            .find(|a| a.key.as_ref() == key)
            .and_then(|a| a.decode_and_unescape_value(decoder).ok().map(|v| v.into_owned()))
    };

    loop {
        match reader.read_event().context("arXiv feed is not well-formed XML")? {
            Event::Start(e) => {
                let name = String::from_utf8_lossy(e.name().as_ref()).into_owned();
                if name == "entry" {
                    draft = Some(Draft::default());
                }
                if let Some(d) = draft.as_mut() {
                    match name.as_str() {
                        "category" => d.categories.extend(attr(&e, b"term")),
                        "arxiv:primary_category" => d.primary = attr(&e, b"term"),
                        _ => {}
                    }
                }
                path.push(name);
            }
            Event::Empty(e) => {
                if let Some(d) = draft.as_mut() {
                    match e.name().as_ref() {
                        b"category" => d.categories.extend(attr(&e, b"term")),
                        b"arxiv:primary_category" => d.primary = attr(&e, b"term"),
                        _ => {}
                    }
                }
            }
            Event::Text(t) => {
                let Some(d) = draft.as_mut() else { continue };
                let text = t.unescape().map(|c| c.into_owned()).unwrap_or_default();
                let parent = path.iter().rev().nth(1).map(String::as_str);
                match (path.last().map(String::as_str), parent) {
                    (Some("id"), Some("entry")) => d.id.push_str(&text),
                    (Some("title"), Some("entry")) => d.title.push_str(&text),
                    (Some("summary"), Some("entry")) => d.summary.push_str(&text),
                    (Some("published"), Some("entry")) => d.published.push_str(&text),
                    (Some("arxiv:comment"), _) => d.comment.push_str(&text),
                    (Some("name"), Some("author")) => d.authors.push(collapse_ws(&text)),
                    _ => {}
                }
            }
            Event::End(e) => {
                path.pop();
                if e.name().as_ref() == b"entry" {
                    let Some(d) = draft.take() else { continue };
                    let Some((arxiv_id, version)) = split_id(&d.id) else { continue };
                    if d.title.trim().is_empty() || out.iter().any(|p| p.arxiv_id == arxiv_id) {
                        continue;
                    }
                    let mut categories: Vec<String> = d.primary.into_iter().collect();
                    for c in d.categories {
                        if !categories.contains(&c) {
                            categories.push(c);
                        }
                    }
                    out.push(ArxivPaper {
                        arxiv_id,
                        version,
                        title: collapse_ws(&d.title),
                        authors: d.authors,
                        categories,
                        abstract_text: collapse_ws(&d.summary),
                        pages: pages_from_comment(&d.comment),
                        published_at: d.published.trim().to_string(),
                    });
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(out)
}

fn category_query() -> String {
    QFIN_CATEGORIES.iter().map(|c| format!("cat:{c}")).collect::<Vec<_>>().join(" OR ")
}

/// Fetch every paper submitted on or after `since` (YYYY-MM-DD) across the q-fin
/// categories and the finance-filtered ML query, newest first, paging until the
/// feed passes `since` or `limit` papers are collected.
pub async fn fetch_since(client: &reqwest::Client, since: &str, limit: usize) -> anyhow::Result<Vec<ArxivPaper>> {
    let mut all: Vec<ArxivPaper> = Vec::new();
    let mut first_request = true;
    for query in [category_query(), ML_FINANCE_QUERY.to_string()] {
        let mut start = 0usize;
        loop {
            if !first_request {
                tokio::time::sleep(REQUEST_SPACING).await;
            }
            first_request = false;
            let resp = client
                .get(API_BASE)
                .query(&[
                    ("search_query", query.as_str()),
                    ("sortBy", "submittedDate"),
                    ("sortOrder", "descending"),
                    ("start", &start.to_string()),
                    ("max_results", &PAGE_SIZE.to_string()),
                ])
                .timeout(Duration::from_secs(60))
                .send()
                .await
                .context("arXiv API request failed")?;
            let status = resp.status();
            let body = resp.text().await.context("arXiv API body unreadable")?;
            anyhow::ensure!(status.is_success(), "arXiv API {status}: {}", body.chars().take(200).collect::<String>());

            let page = parse_feed(&body)?;
            let page_len = page.len();
            let mut passed_since = false;
            for p in page {
                if p.published_at.get(..10).unwrap_or("") < since {
                    passed_since = true;
                    continue;
                }
                if !all.iter().any(|q| q.arxiv_id == p.arxiv_id) {
                    all.push(p);
                }
            }
            if passed_since || page_len < PAGE_SIZE || all.len() >= limit {
                break;
            }
            start += PAGE_SIZE;
        }
    }
    all.truncate(limit);
    Ok(all)
}

/// Insert new papers; an already-known id only has its version bumped (a revised
/// paper is not re-triaged or re-read automatically). Returns rows inserted.
pub fn upsert(conn: &rusqlite::Connection, papers: &[ArxivPaper]) -> anyhow::Result<usize> {
    let mut inserted = 0;
    for p in papers {
        let exists: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM research_papers WHERE arxiv_id = ?1)",
            [&p.arxiv_id],
            |r| r.get(0),
        )?;
        if exists {
            conn.execute(
                "UPDATE research_papers SET version = ?2, updated_at = datetime('now')
                 WHERE arxiv_id = ?1 AND version < ?2",
                rusqlite::params![p.arxiv_id, p.version],
            )?;
            continue;
        }
        conn.execute(
            "INSERT INTO research_papers (arxiv_id, version, title, authors, categories, abstract, pages, published_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            rusqlite::params![
                p.arxiv_id,
                p.version,
                p.title,
                p.authors.join(", "),
                p.categories.join(" "),
                p.abstract_text,
                p.pages,
                p.published_at,
            ],
        )?;
        inserted += 1;
    }
    Ok(inserted)
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = include_str!("arxiv_api_fixture.xml");

    #[test]
    fn parses_real_api_response() {
        let papers = parse_feed(FIXTURE).unwrap();
        // The specimen holds 3 <entry> elements, two of them the same paper.
        assert_eq!(papers.len(), 2);

        let a = &papers[0];
        assert_eq!(a.arxiv_id, "2609.27636");
        assert_eq!(a.version, 1);
        assert!(a.title.starts_with("Multi-Agent AI Architecture for Regulated Insurers"));
        assert_eq!(a.pages, Some(15));
        assert_eq!(a.categories[0], "q-fin.GN", "primary category first");
        assert!(a.categories.contains(&"q-fin.RM".to_string()));
        assert_eq!(a.authors, vec!["Walter Kurz".to_string()]);
        assert!(a.abstract_text.starts_with("This paper proposes"));
        assert!(!a.abstract_text.contains('\n'));
        assert_eq!(a.published_at, "2026-09-23T10:00:16Z");

        let b = &papers[1];
        assert_eq!(b.arxiv_id, "2609.27654");
        assert_eq!(b.pages, None, "no arxiv:comment -> no page count");
        assert_eq!(b.categories[0], "stat.ML");
        assert_eq!(b.authors.len(), 3);
    }

    #[test]
    fn id_and_pages_edge_cases() {
        assert_eq!(split_id("http://arxiv.org/abs/2609.27636v12"), Some(("2609.27636".into(), 12)));
        assert_eq!(split_id("http://arxiv.org/abs/math/0601001v1"), Some(("math/0601001".into(), 1)));
        assert_eq!(split_id("http://arxiv.org/abs/2609.27636"), Some(("2609.27636".into(), 1)));
        assert_eq!(pages_from_comment("26 pages, 5 figures"), Some(26));
        assert_eq!(pages_from_comment("Accepted at ICAIF; 9 pp."), Some(9));
        assert_eq!(pages_from_comment("1 page"), Some(1));
        assert_eq!(pages_from_comment("5 figures"), None);
    }

    #[test]
    fn upsert_is_idempotent_and_keeps_state() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        crate::db::run_migrations(&conn).unwrap();
        let papers = parse_feed(FIXTURE).unwrap();
        assert_eq!(upsert(&conn, &papers).unwrap(), 2);
        assert_eq!(upsert(&conn, &papers).unwrap(), 0, "rerun inserts nothing");

        // A triaged paper keeps its status when a revised version arrives.
        conn.execute("UPDATE research_papers SET status='triaged' WHERE arxiv_id='2609.27636'", []).unwrap();
        let mut v2 = papers[0].clone();
        v2.version = 2;
        assert_eq!(upsert(&conn, &[v2]).unwrap(), 0);
        let (ver, status): (i64, String) = conn
            .query_row("SELECT version, status FROM research_papers WHERE arxiv_id='2609.27636'", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert_eq!((ver, status.as_str()), (2, "triaged"));
        let n: i64 = conn.query_row("SELECT COUNT(*) FROM research_papers", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 2);
    }
}
