//! Records every post-dedup news article of a daily run into `fetch_candidates`
//! (migration 034), with how far it got through the pipeline.
//!
//! The briefing only ever persisted the curated winners, so the stories each stage
//! dropped existed in memory and nowhere else. This is the record of the losers:
//! the substrate for "what did the pipeline bury?" and the input set any shadow
//! ranker is judged against.
//!
//! Strictly observational. It runs after the briefing is written, reads nothing the
//! briefing depends on, and every failure is the caller's to log and drop.

use std::collections::HashSet;

use rusqlite::Connection;

use crate::sources::RawArticle;

/// Snippets are stored as fetched, capped. A pre-enrichment snippet is short; the cap
/// is a backstop against a source that stuffs a whole body into its description.
const SNIPPET_MAX_CHARS: usize = 2000;

/// Which cut decided what reached summarization. Stored as `precurate_ran`:
/// 0 none (pool at or under the threshold, or Groq failed on a pool small enough
/// that the fallback keeps everything), 1 the Groq pre-curate, 2 the
/// sector-balanced fallback cap, 3 the Jev per-article scores. A fallback day is not the model's judgment, so
/// an eval of the Groq cut must be able to tell the two apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PreCurateCut {
    None = 0,
    Groq = 1,
    FallbackCap = 2,
    /// Per-article Jev scores (`crate::jev`), sector-balanced.
    Jev = 3,
}

/// How far a pool article got. Stages are matched by url_hash — the same key
/// `stories.url_hash` uses — so a URL that differs only by query string counts once.
pub struct StageOutcome {
    pub precurate_cut: PreCurateCut,
    /// url_hashes of the articles that went on to summarization.
    pub kept: HashSet<String>,
    /// url_hashes of the articles that came back summarized.
    pub summarized: HashSet<String>,
}

pub fn url_hashes<'a>(urls: impl IntoIterator<Item = &'a str>) -> HashSet<String> {
    urls.into_iter().map(crate::dedup::url_hash).collect()
}

fn cap_chars(s: &str, max: usize) -> &str {
    match s.char_indices().nth(max) {
        Some((i, _)) => &s[..i],
        None => s,
    }
}

/// Insert the pool for `briefing_id`, then link each row to the story it became.
/// Returns (rows inserted, rows linked to a story). Idempotent per briefing: a
/// re-run for the same briefing inserts nothing new and re-links.
pub fn record(
    conn: &Connection,
    briefing_id: i64,
    pool: &[RawArticle],
    outcome: &StageOutcome,
) -> rusqlite::Result<(usize, usize)> {
    let tx = conn.unchecked_transaction()?;
    let mut inserted = 0usize;
    {
        let mut stmt = tx.prepare(
            "INSERT OR IGNORE INTO fetch_candidates (
                briefing_id, url_hash, url, title, source_name, sector, language,
                published_at, content_snippet, pool_position, precurate_ran,
                kept_by_precurate, summarized
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
        )?;
        for (i, a) in pool.iter().enumerate() {
            let hash = crate::dedup::url_hash(&a.url);
            inserted += stmt.execute(rusqlite::params![
                briefing_id,
                hash,
                a.url,
                a.title,
                a.source_name,
                a.sector,
                a.language,
                a.published_at,
                cap_chars(&a.content_snippet, SNIPPET_MAX_CHARS),
                i as i64,
                outcome.precurate_cut as i64,
                outcome.kept.contains(&hash),
                outcome.summarized.contains(&hash),
            ])?;
        }
    }
    tx.execute(
        "UPDATE fetch_candidates
         SET story_id = (
             SELECT s.id FROM stories s
             WHERE s.briefing_id = fetch_candidates.briefing_id
               AND s.url_hash = fetch_candidates.url_hash
               AND s.source_type = 'news'
             ORDER BY s.id LIMIT 1
         )
         WHERE briefing_id = ?1",
        [briefing_id],
    )?;
    // The UPDATE's own count includes rows it set to NULL, so count the links directly.
    let linked = tx.query_row(
        "SELECT COUNT(*) FROM fetch_candidates WHERE briefing_id = ?1 AND story_id IS NOT NULL",
        [briefing_id],
        |r| r.get::<_, i64>(0),
    )? as usize;
    tx.commit()?;
    Ok((inserted, linked))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn article(url: &str, sector: &str) -> RawArticle {
        RawArticle {
            title: format!("title for {url}"),
            url: url.to_string(),
            source_name: "src".into(),
            source_url: String::new(),
            published_at: Some("2026-09-24T08:00:00Z".into()),
            content_snippet: "snippet".into(),
            sector: sector.into(),
            feed_id: "feed".into(),
            language: "en".into(),
            source_type: "news".into(),
            financial_metadata: None,
        }
    }

    fn db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
        crate::db::run_migrations(&conn).unwrap();
        conn
    }

    fn insert_story(conn: &Connection, briefing_id: i64, url: &str, source_type: &str) -> i64 {
        conn.execute(
            "INSERT INTO stories (briefing_id, sector, original_title, original_url, source_name,
                 headline, summary, key_facts, why_it_matters, what_to_watch, importance_score,
                 is_hero, display_order, url_hash, title_hash, source_type)
             VALUES (?1, 'ai', 't', ?2, 'src', 'h', 's', '[]', 'w', 'x', 5, 0, 0, ?3, 'th', ?4)",
            rusqlite::params![briefing_id, url, crate::dedup::url_hash(url), source_type],
        )
        .unwrap();
        conn.last_insert_rowid()
    }

    fn new_briefing(conn: &Connection) -> i64 {
        conn.execute(
            "INSERT INTO briefings (date, story_count, status) VALUES ('2026-09-24', 0, 'complete')",
            [],
        )
        .unwrap();
        conn.last_insert_rowid()
    }

    /// Every stage is distinguishable afterwards: curated, summarized-but-not-curated,
    /// kept-but-failed-summary, and cut by pre-curate.
    #[test]
    fn records_how_far_each_article_got() {
        let conn = db();
        let bid = new_briefing(&conn);
        let pool = vec![
            article("https://a.com/curated", "ai"),
            article("https://a.com/summarized-only", "tech"),
            article("https://a.com/kept-summary-failed", "miami"),
            article("https://a.com/cut", "italy"),
        ];
        let story_id = insert_story(&conn, bid, "https://a.com/curated", "news");
        let outcome = StageOutcome {
            precurate_cut: PreCurateCut::Groq,
            kept: url_hashes(pool[..3].iter().map(|a| a.url.as_str())),
            summarized: url_hashes(pool[..2].iter().map(|a| a.url.as_str())),
        };

        let (inserted, linked) = record(&conn, bid, &pool, &outcome).unwrap();
        assert_eq!((inserted, linked), (4, 1));

        let cuts: Vec<i64> = conn
            .prepare("SELECT DISTINCT precurate_ran FROM fetch_candidates")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(cuts, vec![1], "a Groq cut is stored as 1");

        let rows: Vec<(String, i64, i64, Option<i64>, i64)> = conn
            .prepare(
                "SELECT url, kept_by_precurate, summarized, story_id, pool_position
                 FROM fetch_candidates ORDER BY pool_position",
            )
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(rows[0], ("https://a.com/curated".into(), 1, 1, Some(story_id), 0));
        assert_eq!(rows[1], ("https://a.com/summarized-only".into(), 1, 1, None, 1));
        assert_eq!(rows[2], ("https://a.com/kept-summary-failed".into(), 1, 0, None, 2));
        assert_eq!(rows[3], ("https://a.com/cut".into(), 0, 0, None, 3));
    }

    /// A filing that shares a URL with a news article must not claim the link, and a
    /// story in ANOTHER briefing (a same-day re-run) must not either.
    #[test]
    fn links_only_news_stories_of_the_same_briefing() {
        let conn = db();
        let bid = new_briefing(&conn);
        let other = new_briefing(&conn);
        let url = "https://a.com/x";
        insert_story(&conn, bid, url, "financial");
        insert_story(&conn, other, url, "news");
        let pool = vec![article(url, "ai")];
        let outcome = StageOutcome {
            precurate_cut: PreCurateCut::None,
            kept: url_hashes([url]),
            summarized: HashSet::new(),
        };
        let (_, linked) = record(&conn, bid, &pool, &outcome).unwrap();
        assert_eq!(linked, 0);
    }

    /// Re-running for the same briefing must not duplicate rows, and URLs that differ
    /// only by query string collapse to one candidate, as they do in `stories`.
    #[test]
    fn idempotent_and_query_string_insensitive() {
        let conn = db();
        let bid = new_briefing(&conn);
        let pool = vec![article("https://a.com/y?utm=1", "ai"), article("https://a.com/y", "ai")];
        let outcome = StageOutcome { precurate_cut: PreCurateCut::FallbackCap, kept: HashSet::new(), summarized: HashSet::new() };
        let (first, _) = record(&conn, bid, &pool, &outcome).unwrap();
        let (second, _) = record(&conn, bid, &pool, &outcome).unwrap();
        assert_eq!((first, second), (1, 0));
    }

    /// The stored integer is the enum discriminant; pin every value so a reorder of
    /// the variants cannot silently relabel history.
    #[test]
    fn cut_codes_are_stable() {
        assert_eq!(PreCurateCut::None as i64, 0);
        assert_eq!(PreCurateCut::Groq as i64, 1);
        assert_eq!(PreCurateCut::FallbackCap as i64, 2);
    }

    #[test]
    fn snippet_cap_is_char_safe() {
        let s = "è".repeat(SNIPPET_MAX_CHARS + 10);
        assert_eq!(cap_chars(&s, SNIPPET_MAX_CHARS).chars().count(), SNIPPET_MAX_CHARS);
        assert_eq!(cap_chars("short", SNIPPET_MAX_CHARS), "short");
    }
}
