use super::*;

/// Compute cross-signal scores for entities after signal recomputation.
/// This is called from the pipeline after entity extraction.
/// Auto-populate entity_tickers from SEC company_tickers.json.
/// Run the ticker-mapping backfill standalone (via `--mode backfill-tickers`),
/// processing ALL unmapped entities in one pass instead of the daily 200-cap.
/// Lets the CIK-mapping fix apply immediately rather than waiting ~37 daily runs.
pub(crate) fn backfill_tickers(db_path: &Path) -> anyhow::Result<usize> {
    populate_tickers_limited(db_path, 100_000)
}

/// Rebuild the entity-canonical graph from scratch (via `--mode recanonicalize`).
///
/// `resolve_entities` only processes entities with a NULL `canonical_id`, so tightening the
/// canonicalization match logic does NOT un-merge groups that were already over-merged (e.g. the
/// 129 unrelated companies swept into canonical "Arm" by the old substring match). This standalone
/// pass wipes the existing grouping and rebuilds it under the current (fixed) logic, then re-runs
/// the downstream ticker + signal + cross-signal computation so the Signals tabs reflect clean
/// groups. No Groq/LLM calls — pure DB work, safe to run against a copy first.
pub(crate) fn run_recanonicalize(db_path: &Path) -> anyhow::Result<usize> {
    {
        let conn = rusqlite::Connection::open(db_path)?;
        // Reset the poisoned grouping. entity_canonical is fully derived from entities, so
        // clearing both and rebuilding is lossless (nothing else owns canonical rows).
        tracing::info!("Recanonicalize: clearing existing canonical grouping...");
        conn.execute("UPDATE entities SET canonical_id = NULL", [])?;
        conn.execute("DELETE FROM entity_canonical", [])?;

        // Purge the poisoned 0.6-confidence tickers (the old contains-match tier). These carried
        // both a wrong ticker AND a wrong CIK (e.g. Arm Holdings got HYFM + Hydrofarm's CIK), which
        // then force spurious ticker/CIK merges in resolve_entities. Deleting them drops those
        // entities back into populate_tickers_limited's unmapped set (it skips ids already in
        // entity_tickers), so they get re-resolved with the fixed word-boundary logic. A ticker
        // whose ONLY source was a coincidentally-correct 0.6 guess is intentionally sacrificed —
        // a lucky wrong-method match isn't worth keeping for "super correct".
        let purged = conn.execute("DELETE FROM entity_tickers WHERE confidence = 0.6", [])?;
        tracing::info!("Recanonicalize: purged {} poisoned 0.6-confidence tickers", purged);
    }

    // Rebuild canonical groups. resolve_entities processes up to 500 per call, so loop until
    // it drains (returns 0). Bounded to avoid any pathological infinite loop.
    let mut total = 0usize;
    for _ in 0..500 {
        let n = resolve_entities(db_path)?;
        total += n;
        if n == 0 {
            break;
        }
    }
    tracing::info!("Recanonicalize: resolved {} entities into canonical groups", total);

    // Re-populate per-entity tickers (highest-confidence pick) across the whole table.
    match populate_tickers_limited(db_path, 100_000) {
        Ok(n) => tracing::info!("Recanonicalize: mapped {} entity tickers", n),
        Err(e) => tracing::warn!("Recanonicalize ticker mapping failed (non-fatal): {}", e),
    }

    // Propagate those tickers up to the canonical groups. MUST run after ticker population —
    // the resolve loop above ran before entity_tickers was filled, so canonical rows for
    // newly-mapped entities are still NULL until this pass. Without it "Arm Holdings" stays
    // stale (the exact symptom that outlived the interrupted run).
    match refresh_canonical_tickers(db_path) {
        Ok(n) => tracing::info!("Recanonicalize: propagated tickers to {} canonical groups", n),
        Err(e) => tracing::warn!("Recanonicalize canonical ticker refresh failed (non-fatal): {}", e),
    }

    // Recompute entity signals + cross-signals from the clean groups so convergence is rebuilt.
    let today = chrono::Local::now().format("%Y-%m-%d").to_string();
    {
        let conn = rusqlite::Connection::open(db_path)?;
        if let Err(e) = recompute_signals_pipeline(&conn, &today, 90) {
            tracing::warn!("Recanonicalize signal recompute failed (non-fatal): {}", e);
        }
    }
    match compute_cross_signals(db_path, &today) {
        Ok(n) => tracing::info!("Recanonicalize: recomputed {} cross-signal scores", n),
        Err(e) => tracing::warn!("Recanonicalize cross-signal computation failed (non-fatal): {}", e),
    }

    // compute_cross_signals only writes today's rows; historical cross_signals rows keep
    // stale tickers from before this recanonicalization. The auto-buy window reads the last
    // 1-day of rows, so a stale ticker (Arm's HYFM) would reach the money path. Re-derive
    // every canonical-grouped row's ticker from the now-corrected entity_canonical mapping.
    match refresh_cross_signal_tickers(db_path) {
        Ok(n) => tracing::info!("Recanonicalize: repaired {} cross-signal tickers from canonical", n),
        Err(e) => tracing::warn!("Recanonicalize cross-signal ticker repair failed (non-fatal): {}", e),
    }

    Ok(total)
}

pub(crate) fn populate_tickers(db_path: &Path) -> anyhow::Result<usize> {
    populate_tickers_limited(db_path, 200)
}

/// Propagate the best per-entity ticker up to each canonical group.
///
/// `resolve_entities` runs this same UPDATE at the end of each pass, but only against the
/// `entity_tickers` rows that exist AT THAT MOMENT. During a full recanonicalize the resolve
/// loop runs BEFORE `populate_tickers_limited` fills `entity_tickers`, so canonical rows for
/// newly-mapped entities would stay NULL (this is exactly why "Arm Holdings" kept a stale
/// ticker after the interrupted run). Call this AFTER ticker population to close the gap.
/// This is the AUTHORITATIVE pick — it runs last, after all tickers are populated, so it
/// deliberately does NOT gate on `ticker IS NULL`. The in-loop UPDATE at resolve_entities is
/// first-write-wins across passes (a group's ticker locks in on the first pass that links any
/// ticker, and a higher-confidence entity joining later can't replace it). Overwriting here
/// with the full ORDER BY makes the highest-confidence linked ticker win regardless of pass
/// order. The `id IN (...)` guard ensures the subquery always finds ≥1 linked ticker, so no
/// canonical row is ever nulled by this.
pub(crate) fn refresh_canonical_tickers(db_path: &Path) -> anyhow::Result<usize> {
    let conn = rusqlite::Connection::open(db_path)?;
    conn.execute(
        "UPDATE entity_canonical SET ticker = (
            SELECT et.ticker FROM entity_tickers et
            JOIN entities e ON e.id = et.entity_id
            WHERE e.canonical_id = entity_canonical.id
            ORDER BY et.confidence DESC, et.is_public DESC, et.last_verified DESC
            LIMIT 1
        ) WHERE id IN (
            SELECT DISTINCT e.canonical_id FROM entities e
            JOIN entity_tickers et ON et.entity_id = e.id
            WHERE e.canonical_id IS NOT NULL
        )",
        [],
    ).map_err(Into::into)
}

/// Re-derive `cross_signals.ticker` from the corrected `entity_canonical` mapping.
///
/// `compute_cross_signals` only writes rows for `computed_at = today`, so historical
/// rows keep whatever ticker was authoritative when they were written. After a
/// re-canonicalization corrects `entity_canonical.ticker` (e.g. Arm HYFM→ARM), every
/// prior cross_signals row for that entity is stale — and the auto-buy window
/// (`computed_at >= date('now','-1 day')`) reads yesterday's rows, so stale tickers
/// reach the money path. This repairs ALL canonical-grouped rows to match their
/// canonical, mirroring `refresh_canonical_tickers`' authority.
///
/// SCOPE IS DELIBERATELY canonical-only: rows whose entity has NO canonical group
/// (`canonical_id IS NULL`) are direct-mapped from a confident entity_tickers row
/// (e.g. CORECIVIC→CXW 0.95, UNITIL→UTL 0.98) and are NOT touched — NULLing them
/// would wipe legitimate signals. Emits real NULL where the canonical has no ticker
/// (so the buy query's `ticker IS NOT NULL` gate correctly rejects e.g. CIA/EY/ICE).
pub(crate) fn refresh_cross_signal_tickers(db_path: &Path) -> anyhow::Result<usize> {
    let conn = rusqlite::Connection::open(db_path)?;
    conn.execute(
        "UPDATE cross_signals SET ticker = (
            SELECT ec.ticker FROM entities e
            JOIN entity_canonical ec ON ec.id = e.canonical_id
            WHERE e.id = cross_signals.entity_id
        ) WHERE entity_id IN (
            SELECT id FROM entities WHERE canonical_id IS NOT NULL
        )",
        [],
    ).map_err(Into::into)
}

pub(crate) fn populate_tickers_limited(db_path: &Path, limit: usize) -> anyhow::Result<usize> {
    let conn = rusqlite::Connection::open(db_path)?;

    // Check if entity_tickers table exists
    let table_exists: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='entity_tickers')",
            [],
            |row| row.get(0),
        )
        .unwrap_or(false);
    if !table_exists {
        return Ok(0);
    }

    // Get unmapped entities that could be companies (multiple entity types)
    let unmapped: Vec<(i64, String)> = {
        let mut stmt = conn.prepare(
            "SELECT e.id, e.name FROM entities e
             WHERE e.entity_type IN ('company', 'insider_trade', 'contract_award',
                   'patent_cluster', 'material_event', 'private_placement')
             AND e.id NOT IN (SELECT entity_id FROM entity_tickers)
             LIMIT ?1"
        )?;
        stmt.query_map([limit], |row| Ok((row.get(0)?, row.get(1)?)))?
            .filter_map(|r| r.ok())
            .collect()
    };

    if unmapped.is_empty() {
        return Ok(0);
    }

    // Step 1: Map from financial_metadata (CIK/ticker from SEC filings = highest confidence)
    let mut mapped = 0;
    let mut still_unmapped = Vec::new();

    for (entity_id, name) in &unmapped {
        let ticker_from_metadata: Option<(String, String)> = conn.query_row(
            "SELECT
                json_extract(s.financial_metadata, '$.ticker'),
                json_extract(s.financial_metadata, '$.cik')
             FROM entity_mentions em
             JOIN stories s ON em.story_id = s.id
             WHERE em.entity_id = ?1
               AND s.financial_metadata IS NOT NULL
               AND json_valid(s.financial_metadata)
               AND json_extract(s.financial_metadata, '$.ticker') IS NOT NULL
             LIMIT 1",
            [entity_id],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1).unwrap_or_default())),
        ).ok();

        if let Some((ticker, cik)) = ticker_from_metadata {
            // Story financial_metadata.ticker is LLM-extracted at fetch time and NOT cross-checked
            // against the entity — it can be flat wrong (story 2542 "Arm Holdings Wikipedia views"
            // carried ticker=HYFM). It used to be stamped 1.0, OUTRANKING the authoritative SEC CIK
            // match (0.95/0.98), so the group's ORDER BY confidence pick chose the LLM guess over a
            // government identifier. Demoted below every SEC-derived source; only the SEC contains
            // fallback (0.6) is weaker.
            conn.execute(
                "INSERT OR IGNORE INTO entity_tickers (entity_id, ticker, cik, confidence)
                 VALUES (?1, ?2, ?3, 0.75)",
                rusqlite::params![entity_id, ticker, cik],
            )?;
            mapped += 1;
        } else {
            still_unmapped.push((*entity_id, name.clone()));
        }
    }

    // Step 2: Download SEC company_tickers.json and match remaining (by name + CIK)
    if !still_unmapped.is_empty() {
        match download_sec_tickers() {
            Ok(sec_map) => {
                // Build reverse CIK→ticker map for CIK-bearing entity names
                let cik_map: std::collections::HashMap<String, (String, String)> = sec_map.values()
                    .filter(|(_, cik)| !cik.is_empty())
                    .map(|(ticker, cik)| (cik.clone(), (ticker.clone(), cik.clone())))
                    .collect();

                for (entity_id, name) in &still_unmapped {
                    // Try CIK extraction from entity name first (e.g. "UL Solutions Inc.  (CIK 0001901440)")
                    let mut found = false;
                    if let Some(cik_start) = name.to_lowercase().find("(cik") {
                        let cik_part = &name[cik_start..];
                        let cik_digits: String = cik_part.chars().filter(|c| c.is_ascii_digit()).collect();
                        // Entity names embed zero-padded 10-digit CIKs ("0001901440"),
                        // but SEC's company_tickers.json stores cik_str unpadded
                        // ("1901440"). Strip leading zeros so the lookup actually
                        // matches — without this, 0 of ~1,500 CIK-bearing entities map.
                        let cik_norm = cik_digits.trim_start_matches('0').to_string();
                        if !cik_norm.is_empty() {
                            if let Some((ticker, cik)) = cik_map.get(&cik_norm) {
                                // CIK match = a government-registered identifier embedded in the
                                // entity name. Strongest possible signal → TOP of the ladder (0.98),
                                // above story-metadata (0.75) and every fuzzy name match.
                                conn.execute(
                                    "INSERT OR IGNORE INTO entity_tickers (entity_id, ticker, cik, confidence)
                                     VALUES (?1, ?2, ?3, 0.98)",
                                    rusqlite::params![entity_id, ticker, cik],
                                )?;
                                mapped += 1;
                                found = true;
                            }
                        }
                    }
                    if found { continue; }

                    if let Some((ticker, cik, confidence)) = resolve_ticker_sec(name, &sec_map) {
                        conn.execute(
                            "INSERT OR IGNORE INTO entity_tickers (entity_id, ticker, cik, confidence)
                             VALUES (?1, ?2, ?3, ?4)",
                            rusqlite::params![entity_id, ticker, cik, confidence],
                        )?;
                        mapped += 1;
                    }
                }
            }
            Err(e) => tracing::warn!("SEC ticker download failed (non-fatal): {}", e),
        }
    }

    Ok(mapped)
}

/// Download SEC company_tickers.json with 7-day file cache.
pub(crate) fn download_sec_tickers() -> anyhow::Result<std::collections::HashMap<String, (String, String)>> {
    tokio::task::block_in_place(|| {
        tokio::runtime::Handle::current().block_on(download_sec_tickers_async())
    })
}

pub(crate) async fn download_sec_tickers_async() -> anyhow::Result<std::collections::HashMap<String, (String, String)>> {
    // Check file cache first (7-day TTL)
    let cache_dir = dirs::home_dir().unwrap_or_default().join(".pulse");
    let cache_path = cache_dir.join("sec_tickers.json");

    if cache_path.exists() {
        if let Ok(metadata) = std::fs::metadata(&cache_path) {
            if let Ok(modified) = metadata.modified() {
                let age = std::time::SystemTime::now().duration_since(modified).unwrap_or_default();
                if age < std::time::Duration::from_secs(7 * 86400) {
                    // Cache hit
                    if let Ok(data) = std::fs::read_to_string(&cache_path) {
                        if let Ok(map) = serde_json::from_str::<std::collections::HashMap<String, (String, String)>>(&data) {
                            tracing::info!("SEC tickers: loaded {} from cache (age: {}h)", map.len(), age.as_secs() / 3600);
                            return Ok(map);
                        }
                    }
                }
            }
        }
    }

    let url = "https://www.sec.gov/files/company_tickers.json";
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()?;

    let resp = client
        .get(url)
        .header("User-Agent", "Pulse/1.0 (pulse-app@example.com)")
        .send()
        .await?;

    if !resp.status().is_success() {
        // Try cache even if expired
        if cache_path.exists() {
            if let Ok(data) = std::fs::read_to_string(&cache_path) {
                if let Ok(map) = serde_json::from_str(&data) {
                    tracing::warn!("SEC tickers: API returned {}, using stale cache", resp.status());
                    return Ok(map);
                }
            }
        }
        anyhow::bail!("SEC company_tickers.json returned {}", resp.status());
    }

    // SEC's company_tickers.json stores cik_str as an INTEGER (e.g. 1045810), not
    // a string. Declaring it String made resp.json() fail with "error decoding
    // response body" — silently killing the entire SEC ticker map. Deserialize as
    // u64 and stringify (unpadded), which matches the zero-stripped CIK lookup.
    #[derive(serde::Deserialize)]
    struct SecEntry { cik_str: u64, ticker: String, title: String }

    let raw: std::collections::HashMap<String, SecEntry> = resp.json().await?;
    let mut map = std::collections::HashMap::with_capacity(raw.len());
    for entry in raw.values() {
        map.insert(entry.title.to_lowercase(), (entry.ticker.clone(), entry.cik_str.to_string()));
    }

    // Write cache
    let _ = std::fs::create_dir_all(&cache_dir);
    let _ = std::fs::write(&cache_path, serde_json::to_string(&map).unwrap_or_default());

    tracing::info!("SEC tickers: downloaded {} mappings (cached)", map.len());
    Ok(map)
}

/// Word-boundary containment: is `needle` a contiguous run of WHOLE WORDS inside `hay`?
///
/// Replaces raw `str::contains` in the entity/ticker resolution paths. The substring version
/// merged any name/key containing a short token as a substring — "arm holdings" matched SEC key
/// "hydrofarm holdings, inc" (…hydrof-ARM HOLDINGS…) → attached HYFM + Hydrofarm's CIK to the
/// real Arm Holdings, poisoning the ticker AND forcing a CIK/ticker merge into Hydrofarm's group.
/// Requiring whole-word alignment keeps legit hits ("apple" ⊂ "apple inc") and drops the bleed.
/// `needle.len() < 3` returns false to avoid single-token noise (matches the ≥3 guards nearby).
pub(crate) fn contains_words(hay: &str, needle: &str) -> bool {
    if needle.len() < 3 {
        return false;
    }
    let hay_tokens: Vec<&str> = hay.split_whitespace().collect();
    let needle_tokens: Vec<&str> = needle.split_whitespace().collect();
    if needle_tokens.is_empty() || needle_tokens.len() > hay_tokens.len() {
        return false;
    }
    hay_tokens
        .windows(needle_tokens.len())
        .any(|w| w == needle_tokens.as_slice())
}

/// Resolve entity name to ticker using SEC data.
/// Tries: exact match → suffix-stripped match → word-boundary contains match.
pub(crate) fn resolve_ticker_sec(
    name: &str,
    sec_map: &std::collections::HashMap<String, (String, String)>,
) -> Option<(String, String, f64)> {
    // Strip CIK patterns like "(CIK 0001901440)" and clean up
    let cleaned = name
        .trim()
        .to_lowercase();
    let name_lower = if let Some(idx) = cleaned.find("(cik") {
        cleaned[..idx].trim().to_string()
    } else {
        cleaned
    };

    if name_lower.is_empty() || name_lower.len() < 2 {
        return None;
    }

    // 1. Exact name match against SEC company list. Strong, but below a CIK match (0.98) —
    // a name can collide, a CIK cannot. Was 1.0 (tied with story-metadata); now 0.95.
    if let Some((ticker, cik)) = sec_map.get(&name_lower) {
        return Some((ticker.clone(), cik.clone(), 0.95));
    }

    // 2. Strip common suffixes
    let suffixes = [
        " inc", " inc.", " corp", " corp.", " ltd", " ltd.", " llc",
        " co", " co.", " plc", " sa", " ag", " se", " nv",
        " holdings", " group", " technologies", " technology",
        " international", " solutions", " systems", " enterprises",
    ];
    let stripped = suffixes.iter().fold(name_lower.as_str(), |s, sfx| s.trim_end_matches(sfx)).trim();
    if stripped != name_lower && stripped.len() >= 3 {
        for (key, (ticker, cik)) in sec_map.iter() {
            let key_stripped = suffixes.iter().fold(key.as_str(), |s, sfx| s.trim_end_matches(sfx)).trim();
            if key_stripped == stripped {
                return Some((ticker.clone(), cik.clone(), 0.85));
            }
        }
    }

    // 3. Word-boundary contains match (min 5 chars to avoid false positives).
    // Was raw substring — "arm holdings" matched "hydrofarm holdings, inc" → HYFM poison.
    if name_lower.len() >= 5 {
        for (key, (ticker, cik)) in sec_map.iter() {
            if contains_words(key, &name_lower) || contains_words(&name_lower, key) {
                return Some((ticker.clone(), cik.clone(), 0.6));
            }
        }
    }

    None
}

/// Resolve entities to canonical records.
/// Merges duplicates like "NVIDIA Corp", "Nvidia", "NVIDIA CORPORATION" into one canonical entity.
/// Strategy: CIK match → ticker match → normalized name match → suffix-stripped match.
pub(crate) fn resolve_entities(db_path: &Path) -> anyhow::Result<usize> {
    let conn = rusqlite::Connection::open(db_path)?;
    crate::db::run_migrations(&conn)?;

    // Check if entity_canonical table exists
    let table_exists: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='entity_canonical')",
        [], |row| row.get(0),
    ).unwrap_or(false);
    if !table_exists { return Ok(0); }

    // Check if canonical_id column exists on entities
    let col_exists: bool = {
        let mut stmt = conn.prepare("PRAGMA table_info(entities)")?;
        stmt.query_map([], |row| row.get::<_, String>(1))?
            .any(|r| r.as_deref() == Ok("canonical_id"))
    };
    if !col_exists { return Ok(0); }

    // Get all entities without a canonical_id
    let unresolved: Vec<(i64, String, String, Option<String>)> = {
        let mut stmt = conn.prepare(
            "SELECT e.id, e.name, e.entity_type, et.ticker
             FROM entities e
             LEFT JOIN entity_tickers et ON et.entity_id = e.id
             WHERE e.canonical_id IS NULL
             ORDER BY e.mention_count DESC
             LIMIT 500"
        )?;
        stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)))?
            .filter_map(|r| r.ok())
            .collect()
    };

    if unresolved.is_empty() { return Ok(0); }

    // Load existing canonical entities for matching. MUTABLE: canonicals created during this
    // loop are pushed back in (see the create branch) so a later entity in the SAME batch can
    // match one an earlier entity just created. Without this, "Arm" (entity 122) and "Arm
    // Holdings" (entity 21) split into two groups because 21 couldn't see the canonical 122 made.
    let mut existing_canonicals: Vec<(i64, String, Option<String>, Option<String>)> = {
        let mut stmt = conn.prepare(
            "SELECT id, canonical_name, ticker, cik FROM entity_canonical"
        )?;
        stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)))?
            .filter_map(|r| r.ok())
            .collect()
    };

    // Common suffixes to strip for matching
    let suffixes = [
        " inc", " inc.", " corp", " corp.", " corporation", " ltd", " ltd.",
        " llc", " co", " co.", " plc", " sa", " ag", " se", " nv",
        " holdings", " group", " technologies", " technology",
        " international", " solutions", " systems", " enterprises",
        " company", " partners", " capital", " industries",
    ];

    let normalize = |name: &str| -> String {
        let mut n = name.to_lowercase().trim().to_string();
        // Strip CIK suffix like "(CIK 0001234567)"
        if let Some(idx) = n.find("(cik") {
            n = n[..idx].trim().to_string();
        }
        // Strip common suffixes
        for suffix in &suffixes {
            n = n.trim_end_matches(suffix).to_string();
        }
        n.trim().to_string()
    };

    // Uses the module-scope `contains_words` (also used by resolve_ticker_sec) for the
    // word-boundary name match below — "arm" no longer merges "hydrofarm"/"pharma".

    let mut resolved = 0usize;

    for (entity_id, name, entity_type, ticker) in &unresolved {
        let name_norm = normalize(name);
        if name_norm.len() < 2 { continue; }

        // Skip non-company-like entities (topics, generic terms)
        if matches!(entity_type.as_str(), "topic" | "regulation") { continue; }

        let mut matched_canonical_id: Option<i64> = None;

        // 1. Try CIK match (highest confidence)
        let entity_cik: Option<String> = conn.query_row(
            "SELECT cik FROM entity_tickers WHERE entity_id = ?1 AND cik IS NOT NULL AND cik != ''",
            [entity_id], |row| row.get(0),
        ).ok();

        if let Some(ref cik) = entity_cik {
            for (cid, _, _, c_cik) in &existing_canonicals {
                if c_cik.as_deref() == Some(cik) {
                    matched_canonical_id = Some(*cid);
                    break;
                }
            }
        }

        // 2. Try ticker match
        if matched_canonical_id.is_none() {
            if let Some(t) = ticker {
                for (cid, _, c_ticker, _) in &existing_canonicals {
                    if c_ticker.as_deref() == Some(t.as_str()) {
                        matched_canonical_id = Some(*cid);
                        break;
                    }
                }
            }
        }

        // 3. Try normalized name match against existing canonicals
        if matched_canonical_id.is_none() {
            for (cid, c_name, _, _) in &existing_canonicals {
                let c_norm = normalize(c_name);
                if c_norm == name_norm {
                    matched_canonical_id = Some(*cid);
                    break;
                }
                // Word-boundary containment (either direction). Requires whole-word alignment
                // so "arm" no longer matches "hydrofarm"/"pharma"; "nvidia" still matches
                // "nvidia corp". Min length enforced inside contains_words.
                if contains_words(&c_norm, &name_norm) || contains_words(&name_norm, &c_norm) {
                    matched_canonical_id = Some(*cid);
                    break;
                }
            }
        }

        // 4. No match — create a new canonical entity
        if matched_canonical_id.is_none() {
            // Use the best name we have
            let canon_name = name.trim().to_string();
            conn.execute(
                "INSERT OR IGNORE INTO entity_canonical (canonical_name, ticker, cik, sector, entity_type)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                rusqlite::params![
                    canon_name,
                    ticker,
                    entity_cik,
                    conn.query_row("SELECT sector FROM entities WHERE id = ?1", [entity_id], |row| row.get::<_, Option<String>>(0)).ok().flatten(),
                    if matches!(entity_type.as_str(), "company" | "insider_trade" | "contract_award" | "patent_cluster" | "material_event" | "private_placement") { "company" } else { entity_type.as_str() },
                ],
            ).ok();

            matched_canonical_id = conn.query_row(
                "SELECT id FROM entity_canonical WHERE canonical_name = ?1",
                [&canon_name], |row| row.get(0),
            ).ok();

            // Make this new canonical visible to later entities in the SAME batch, so intra-batch
            // duplicates ("Arm" then "Arm Holdings") collapse into it instead of each spawning a row.
            if let Some(new_cid) = matched_canonical_id {
                existing_canonicals.push((new_cid, canon_name.clone(), ticker.clone(), entity_cik.clone()));
            }
        }

        // Link entity to canonical
        if let Some(cid) = matched_canonical_id {
            conn.execute(
                "UPDATE entities SET canonical_id = ?1 WHERE id = ?2",
                rusqlite::params![cid, entity_id],
            ).ok();

            // Also populate entity_aliases
            let alias_norm = name.to_lowercase().trim().to_string();
            conn.execute(
                "INSERT OR IGNORE INTO entity_aliases (entity_id, alias, alias_type)
                 VALUES (?1, ?2, 'alternate_name')",
                rusqlite::params![entity_id, alias_norm],
            ).ok();

            // If we have a ticker, add it as an alias too
            if let Some(t) = ticker {
                conn.execute(
                    "INSERT OR IGNORE INTO entity_aliases (entity_id, alias, alias_type)
                     VALUES (?1, ?2, 'ticker')",
                    rusqlite::params![entity_id, t],
                ).ok();
            }

            resolved += 1;
        }
    }

    // Update canonical entities with best available ticker from linked entities.
    // Pick the HIGHEST-CONFIDENCE ticker, not an arbitrary one — the old `LIMIT 1` with no
    // ORDER BY grabbed a random linked ticker, so a polluted group could resolve to a junk
    // 0.6-confidence mapping over the correct 0.95 one. Tie-break by is_public then most-recent.
    conn.execute_batch(
        "UPDATE entity_canonical SET ticker = (
            SELECT et.ticker FROM entity_tickers et
            JOIN entities e ON e.id = et.entity_id
            WHERE e.canonical_id = entity_canonical.id
            ORDER BY et.confidence DESC, et.is_public DESC, et.last_verified DESC
            LIMIT 1
        ) WHERE ticker IS NULL AND id IN (
            SELECT DISTINCT e.canonical_id FROM entities e
            JOIN entity_tickers et ON et.entity_id = e.id
            WHERE e.canonical_id IS NOT NULL
        )"
    ).ok();

    Ok(resolved)
}
