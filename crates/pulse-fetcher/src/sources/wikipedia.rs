use super::RawArticle;

/// Wikipedia Pageviews API — free proxy for search interest.
/// API: https://wikimedia.org/api/rest_v1/metrics/pageviews/per-article/{project}/{access}/{agent}/{article}/{granularity}/{start}/{end}
/// No API key needed. Rate limit: ~100 req/s (generous).
///
/// Strategy: For entities with tickers (public companies), fetch their Wikipedia page views.
/// Compare last 7 days vs prior 30-day average. A spike in page views correlates with
/// search interest and often precedes price moves.
pub async fn fetch(db_path: &std::path::Path) -> anyhow::Result<Vec<RawArticle>> {
    let conn = rusqlite::Connection::open(db_path)?;

    // Get entities with tickers that have recent signals (worth tracking).
    // 2026-07-23: this used to be an unordered LIMIT 50 — an arbitrary 2% of
    // the ~2,500 ticker-mapped entities, picked by whatever SQLite's query
    // planner returned first, despite the comment already claiming "recent
    // signals" filtering that the SQL never actually did. That's most of why
    // search_trend_delta had only 3 real signals in 90 days. Now actually
    // orders by most-recent entity_mentions activity (indexed) and raises
    // the cap — Wikipedia's pageviews API is ~100 req/s and this fetch runs
    // at ~150ms/entity, so 300 entities is ~45s, well within budget.
    let entities: Vec<(String, String)> = {
        let mut stmt = conn.prepare(
            // GROUP BY the company, NOT (company, ticker). Grouping by ticker
            // returns one row per share class — GOOG and GOOGL, T and T-PA — and
            // both rows carry the SAME canonical_name, so they produce identical
            // title candidates, identical HTTP calls and an identical story. The
            // batch dedup downstream cannot collapse them because feed_id embeds
            // the ticker, so its (feed_id, url) key differs while the url does not.
            // Measured 2026-08-22: 9 of 300 slots were duplicates.
            // MIN(ticker) picks the common share class in practice (suffixed
            // variants sort after the base symbol) and is at worst deterministic.
            "SELECT ec.canonical_name, MIN(et.ticker)
             FROM entity_canonical ec
             JOIN entities e ON e.canonical_id = ec.id
             JOIN entity_tickers et ON et.entity_id = e.id
             LEFT JOIN entity_mentions em ON em.entity_id = e.id
             WHERE et.ticker IS NOT NULL
             GROUP BY ec.canonical_name
             ORDER BY MAX(em.mentioned_at) DESC
             LIMIT 300"
        )?;
        stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
            .filter_map(|r| r.ok())
            .collect()
    };

    if entities.is_empty() {
        // Fallback: try entities table directly (same recency ordering as above)
        let mut stmt = conn.prepare(
            "SELECT e.name, MIN(et.ticker)
             FROM entities e
             JOIN entity_tickers et ON et.entity_id = e.id
             LEFT JOIN entity_mentions em ON em.entity_id = e.id
             WHERE et.ticker IS NOT NULL
             GROUP BY e.name
             ORDER BY MAX(em.mentioned_at) DESC
             LIMIT 300"
        )?;
        let fallback: Vec<(String, String)> = stmt
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
            .filter_map(|r| r.ok())
            .collect();
        if fallback.is_empty() {
            tracing::info!("Wikipedia Pageviews: no entities with tickers found, skipping");
            return Ok(Vec::new());
        }
        return fetch_pageviews(&fallback).await;
    }

    fetch_pageviews(&entities).await
}


/// Legal-form suffixes stripped from the tail of an SEC entity name, longest
/// phrase first so `SAB DE CV` is consumed before `SA` can bite off half of it.
/// Matched whole-word and case-insensitively.
const LEGAL_SUFFIXES: &[&str] = &[
    "s.a.b. de c.v.", "sab de cv", "incorporated", "corporation", "company",
    "limited", "l.l.c.", "p.l.c.", "inc.", "inc", "corp.", "corp", "co.", "co",
    "ltd.", "ltd", "llc", "plc", "n.v.", "nv", "s.a.", "sa", "ag",
];

/// Turn one SEC-derived entity name into the ordered, de-duplicated list of
/// Wikipedia article titles worth trying, URL-path ready.
///
/// This exists because the old code did `name.replace(' ', "_")` on the raw
/// canonical name, and those names carry SEC boilerplate: `Cloudflare, Inc.
/// (CIK 0001477333)`, `KENNAMETAL INC  (KMT)`, `CITIZENS FINANCIAL GROUP
/// INC/RI`. Measured 2026-08-22 against the live API over the 25 most recently
/// mentioned ticker-mapped entities: **4/25 resolved**. Every miss `continue`s
/// silently, which is why `search_trend_delta` is non-zero in 2 of 26,721
/// `signals` rows and `search_trend` in 1 of ~400 daily `cross_signals` rows.
///
/// Note this should REDUCE total HTTP calls despite considering more forms: a
/// miss currently burns all three variants, and the normalised title usually
/// hits on the first.
pub(crate) fn title_candidates(entity_name: &str) -> Vec<String> {
    // 1. Drop the trailing parenthetical — `(CIK 0001477333)`, `(BKKT, BKKT-WT)`.
    let mut name = match entity_name.find(" (") {
        Some(i) => &entity_name[..i],
        None => entity_name,
    }
    .trim()
    .to_string();

    // 2. A trailing state qualifier rides on the last word: `INC/RI`.
    if let Some(last) = name.split_whitespace().next_back()
        && let Some((head, _)) = last.split_once('/') {
            let head = head.to_string();
            let without = name[..name.len() - last.len()].trim_end().to_string();
            name = if head.is_empty() { without } else { format!("{without} {head}") };
        }

    // 3. Peel legal suffixes off the tail, repeatedly — `Bakkt, Inc.` and
    //    `CEMEX SAB DE CV` both need more than one pass.
    loop {
        let trimmed = name.trim().trim_end_matches([',', '.', ' ']).to_string();
        let lower = trimmed.to_ascii_lowercase();
        let mut cut = None;
        for suffix in LEGAL_SUFFIXES {
            let bare = suffix.trim_end_matches('.');
            if let Some(head) = lower.strip_suffix(bare) {
                // Whole-word only: never turn `Cisco` into `Cis`.
                if head.is_empty() || head.ends_with([' ', ',']) {
                    cut = Some(head.trim_end_matches([' ', ',']).len());
                    break;
                }
            }
        }
        match cut {
            Some(0) | None => {
                name = trimmed;
                break;
            }
            Some(i) => name = trimmed[..i].to_string(),
        }
    }

    // 4. SEC names are frequently SHOUTED. Title-case them, hyphen-aware, so
    //    `BIO-TECHNE` becomes `Bio-Techne`. Names that already carry lowercase
    //    are left alone — `Hims & Hers Health` is already correct.
    let mut forms = Vec::new();
    if !name.is_empty() && !name.chars().any(|c| c.is_ascii_lowercase()) {
        forms.push(title_case(&name));
    }
    if !name.is_empty() {
        // Kept even when title-cased above: an acronym like `BTCS` survives here
        // and would be mangled to `Btcs` by the title-caser.
        forms.push(name.clone());
    }

    let mut out: Vec<String> = Vec::new();
    for form in &forms {
        for candidate in [form.clone(), format!("{form} (company)")] {
            let encoded = candidate.replace(' ', "_").replace('&', "%26");
            if !out.contains(&encoded) {
                out.push(encoded);
            }
        }
    }
    out
}

fn title_case(s: &str) -> String {
    s.split(' ')
        .map(|word| {
            word.split('-')
                .map(|part| {
                    let mut chars = part.chars();
                    match chars.next() {
                        Some(first) => {
                            first.to_uppercase().collect::<String>()
                                + &chars.as_str().to_lowercase()
                        }
                        None => String::new(),
                    }
                })
                .collect::<Vec<_>>()
                .join("-")
        })
        .collect::<Vec<_>>()
        .join(" ")
}

async fn fetch_pageviews(entities: &[(String, String)]) -> anyhow::Result<Vec<RawArticle>> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()?;

    let today = chrono::Local::now();
    let end = today.format("%Y%m%d").to_string();
    // Pull 37 days; the recent-7d vs prior-30d split is done in-code below by
    // slicing the returned daily series (no separate 7d query window needed).
    let start_37d = (today - chrono::Duration::days(37)).format("%Y%m%d").to_string();
    let today_str = today.format("%Y-%m-%d").to_string();

    let mut articles = Vec::new();
    let mut seen_pages: std::collections::HashSet<String> = std::collections::HashSet::new();

    for (entity_name, ticker) in entities {
        let title_variants = title_candidates(entity_name);

        let mut found_resp = None;
        for wiki_title in &title_variants {
            let url = format!(
                "https://wikimedia.org/api/rest_v1/metrics/pageviews/per-article/en.wikipedia/all-access/all-agents/{}/daily/{}/{}",
                wiki_title, start_37d, end
            );

            super::API_CALLS.wikipedia.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let resp = match client
                .get(&url)
                .header("User-Agent", "Pulse/1.0 (pulse-app@example.com)")
                .send()
                .await
            {
                Ok(r) if r.status().is_success() => r,
                _ => {
                    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                    continue;
                }
            };

            found_resp = Some((resp, wiki_title.clone()));
            break;
        }

        let Some((resp, wiki_title)) = found_resp else {
            continue;
        };

        // Residual collision: two distinct company names resolving to one page
        // (a rename, an acquisition, a subsidiary). The query fix above cannot
        // see this because the names differ; only the resolved title reveals it.
        if already_emitted(&mut seen_pages, &wiki_title) {
            tracing::debug!(
                "Wikipedia: {} resolves to {}, already emitted this run — skipping duplicate",
                entity_name, wiki_title
            );
            continue;
        }

        let body: serde_json::Value = match resp.json().await {
            Ok(b) => b,
            Err(_) => continue,
        };

        let items = body.get("items").and_then(|v| v.as_array());
        let Some(items) = items else { continue };

        if items.len() < 10 {
            continue; // Not enough data for meaningful comparison
        }

        // Split into recent 7d and prior 30d
        let total_views: i64 = items.iter()
            .filter_map(|item| item.get("views").and_then(|v| v.as_i64()))
            .sum();

        let recent_7d: i64 = items.iter()
            .rev()
            .take(7)
            .filter_map(|item| item.get("views").and_then(|v| v.as_i64()))
            .sum();

        let prior_30d = total_views - recent_7d;
        let prior_daily_avg = if items.len() > 7 {
            prior_30d as f64 / (items.len() - 7) as f64
        } else {
            prior_30d as f64 / 30.0
        };

        let recent_daily_avg = recent_7d as f64 / 7.0;

        // Calculate delta percentage
        let views_delta_pct = if prior_daily_avg > 10.0 {
            ((recent_daily_avg - prior_daily_avg) / prior_daily_avg) * 100.0
        } else {
            0.0 // Too low baseline, skip
        };

        // Only report significant changes (>20% spike or drop)
        if views_delta_pct.abs() < 20.0 {
            continue;
        }

        let direction = if views_delta_pct > 0.0 { "spike" } else { "drop" };
        let title = format!(
            "{} Wikipedia views {} {:.0}% ({:.0}/day → {:.0}/day)",
            entity_name, direction, views_delta_pct.abs(), prior_daily_avg, recent_daily_avg
        );

        let metadata = serde_json::json!({
            "entity_name": entity_name,
            "ticker": ticker,
            "views_7d": recent_7d,
            "views_daily_avg_7d": recent_daily_avg,
            "views_daily_avg_30d": prior_daily_avg,
            "views_delta_pct": views_delta_pct,
            "direction": direction,
        });

        articles.push(RawArticle {
            title,
            url: format!("https://en.wikipedia.org/wiki/{}", wiki_title),
            source_name: "Wikipedia Pageviews".to_string(),
            source_url: "https://wikimedia.org".to_string(),
            published_at: Some(today_str.clone()),
            content_snippet: format!(
                "Wikipedia page views for {} changed {:.0}% over the past week compared to 30-day average.",
                entity_name, views_delta_pct
            ),
            sector: "finance".to_string(),
            feed_id: pageview_feed_id(ticker, &today_str),
            language: "en".to_string(),
            source_type: "financial".to_string(),
            financial_metadata: Some(serde_json::to_string(&metadata).unwrap_or_default()),
        });

        // Rate limit: 120ms between requests (well within 100 req/s)
        tokio::time::sleep(std::time::Duration::from_millis(120)).await;
    }

    tracing::info!("Wikipedia Pageviews: {} significant changes detected", articles.len());
    Ok(articles)
}

/// Dedup key for one company's pageview story on one day.
///
/// `financial_dedup` is keyed `(source_type, source_id)` where source_type is this
/// feed_id and source_id is the article url — and its lookup has NO date bound, nor
/// does anything in the codebase ever DELETE from it (17,298 rows reaching back to
/// 2026-04-14 as of 2026-08-22). For SEC filings that is correct: a filing is a
/// one-time event and must never be re-emitted.
///
/// A pageview story is the opposite — it is a DAILY DELTA ("views spike 171% over
/// the past week"), and tomorrow's delta is a different fact about the same page.
/// Because the Wikipedia url is constant per company, the unbounded key meant each
/// company could emit a pageview story exactly ONCE, EVER. Putting the day in the
/// key makes the dedup mean "once per company per day", which is what the story is.
///
/// This also releases the 94 companies already burned under the old key format:
/// their stored rows say `wikipedia_bkkt`, and no future lookup asks for that again.
fn pageview_feed_id(ticker: &str, day: &str) -> String {
    format!("wikipedia_{}_{}", ticker.to_lowercase(), day)
}

/// True when `title` has already produced an article this run.
///
/// Several SEC entity rows collapse onto one Wikipedia page — a ticker row and a
/// CIK row for the same company, or two CIKs after a reorganisation. Each would
/// emit an identical story: 2026-06-27 stored 6 rows for a single page, and
/// `stories.url_hash` is a plain index, not UNIQUE, so nothing downstream stops
/// it. Normalising titles made collisions MORE likely, not less.
///
/// Deliberately keyed on the title that actually RESOLVED, never on the candidate
/// list: "Bakkt Holdings" and "Bakkt" share a candidate but may be distinct pages,
/// and skipping on a candidate match would silently drop a real signal.
fn already_emitted(seen: &mut std::collections::HashSet<String>, title: &str) -> bool {
    !seen.insert(title.to_string())
}

#[cfg(test)]
mod title_tests {
    use super::title_candidates;

    /// Every expectation below was confirmed against the live Pageviews API on
    /// 2026-08-22 before this function was written: the raw name 404s and the
    /// asserted title returns 200. The test therefore pins a REAL mapping, not
    /// a plausible-looking string.
    #[test]
    fn the_real_sec_names_normalise_to_titles_that_exist() {
        for (raw, want) in [
            ("Cloudflare, Inc.  (CIK 0001477333)", "Cloudflare"),
            ("KENNAMETAL INC  (KMT)", "Kennametal"),
            ("CITIZENS FINANCIAL GROUP INC/RI  (CIK 0000759944)", "Citizens_Financial_Group"),
            ("BIO-TECHNE Corp  (CIK 0000842023)", "Bio-Techne"),
            ("CEMEX SAB DE CV  (CIK 0001076378)", "Cemex"),
            ("Lifeway Foods, Inc.  (CIK 0000814586)", "Lifeway_Foods"),
            ("Bakkt, Inc.  (BKKT, BKKT-WT)", "Bakkt"),
            ("Hims & Hers Health, Inc.  (CIK 0001773751)", "Hims_%26_Hers_Health"),
        ] {
            let got = title_candidates(raw);
            assert!(
                got.first().map(|s| s.as_str()) == Some(want),
                "{raw} -> {got:?}, wanted {want} first"
            );
        }
    }

    /// The whole-word guard. Stripping `co` as a substring would turn Cisco
    /// into Cis — the classic suffix-matcher bug.
    #[test]
    fn a_name_ending_in_a_suffix_substring_is_left_alone() {
        assert_eq!(title_candidates("Cisco Systems, Inc.")[0], "Cisco_Systems");
        assert_eq!(title_candidates("Sage Therapeutics")[0], "Sage_Therapeutics");
    }

    /// An acronym must survive: title-casing BTCS to Btcs would be wrong, so
    /// the original casing is always kept as a later candidate.
    #[test]
    fn an_acronym_keeps_an_uppercase_candidate() {
        let got = title_candidates("BTCS Inc.  (BTCS)");
        assert!(got.contains(&"BTCS".to_string()), "{got:?}");
        assert!(got.contains(&"Btcs".to_string()), "{got:?}");
    }

    /// Ampersand names are one company, not two — they must not be split, and
    /// the ampersand must be percent-encoded for the URL path.
    #[test]
    fn an_ampersand_name_stays_whole_and_encoded() {
        let got = title_candidates("Hims & Hers Health, Inc.");
        assert_eq!(got[0], "Hims_%26_Hers_Health");
    }

    /// Candidates are de-duplicated: a mixed-case name must not produce the
    /// same title twice and double the HTTP calls.
    #[test]
    fn candidates_are_unique() {
        let got = title_candidates("Lifeway Foods, Inc.");
        let mut sorted = got.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), got.len(), "{got:?}");
    }

    /// Degenerate input must not panic or produce an empty title that would be
    /// fetched as a bare URL.
    #[test]
    fn degenerate_names_produce_no_candidates_rather_than_empty_ones() {
        for raw in ["", "   ", "Inc.", "CORP", " (CIK 0001)"] {
            let got = title_candidates(raw);
            assert!(
                got.iter().all(|t| !t.is_empty() && t != "_(company)"),
                "{raw} -> {got:?}"
            );
        }
    }
}

#[cfg(test)]
mod dedup_tests {
    use super::already_emitted;
    use std::collections::HashSet;

    #[test]
    fn the_first_sighting_of_a_page_is_emitted_and_the_second_is_not() {
        let mut seen = HashSet::new();
        assert!(!already_emitted(&mut seen, "Bakkt"), "first sighting must emit");
        assert!(already_emitted(&mut seen, "Bakkt"), "second sighting must be suppressed");
    }

    /// The inverted-boolean mutation: `seen.insert()` returns true when the value
    /// was NEW, so dropping the `!` suppresses every FIRST sighting and emits every
    /// duplicate — exactly backwards, and silent.
    #[test]
    fn distinct_pages_are_all_emitted() {
        let mut seen = HashSet::new();
        for t in ["Bakkt", "Bakkt_Holdings", "Mattel", "Snap_Inc."] {
            assert!(!already_emitted(&mut seen, t), "{t} is distinct and must emit");
        }
        assert_eq!(seen.len(), 4);
    }
}

#[cfg(test)]
mod feed_id_tests {
    use super::pageview_feed_id;

    /// The bug this pins: with the day absent from the key, `financial_dedup`
    /// suppressed a company's pageview story permanently after its first sighting,
    /// because the Wikipedia url never changes and the lookup has no date bound.
    #[test]
    fn the_same_company_gets_a_different_key_on_a_different_day() {
        assert_ne!(
            pageview_feed_id("BKKT", "2026-08-22"),
            pageview_feed_id("BKKT", "2026-08-23"),
            "a daily delta must not be deduped against yesterday's delta"
        );
    }

    /// Within one day it must still collapse — two runs on the same date (a forced
    /// re-run) should not store the same delta twice.
    #[test]
    fn the_same_company_on_the_same_day_gets_the_same_key() {
        assert_eq!(
            pageview_feed_id("BKKT", "2026-08-22"),
            pageview_feed_id("bkkt", "2026-08-22")
        );
    }

    #[test]
    fn distinct_companies_never_share_a_key() {
        assert_ne!(
            pageview_feed_id("BKKT", "2026-08-22"),
            pageview_feed_id("SNAP", "2026-08-22")
        );
    }
}
