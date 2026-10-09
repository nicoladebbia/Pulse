//! Event signals: ways into a trade besides the cross-signal score
//! (`event_signals`, migration 045). Every event is recorded whether or not
//! it is traded, so `learning.rs` can measure all of them against SPY.
//!
//! - **news_surprise**: the AI-scored tone of a company's news today differs
//!   from its last 30 days by at least `SURPRISE_MIN`. Tested 2026-10-08 on
//!   six months of mentions (event study in the PR): a +0.5 jump returned
//!   about +1.7 points vs SPY over 3 days when bought the next morning, but
//!   nothing measurable when bought at the first run after the briefing, the
//!   only entry this system can make. It trades small and short so the paper
//!   account can settle the question; the learning report tracks it.
//! - **insider_cluster**: two or more insiders buying on the open market
//!   within 30 days, $100k+ together (Cohen, Malloy and Pomorski 2012: such
//!   "opportunistic" buys earned excess returns over the following months).
//!   Six months of our Form 4s showed no edge on 9-20 events, and neither
//!   did 2018-2026 (SEC insider data sets, `research/event-backtest`): no
//!   longer detected; trades already open keep their 20-day hold.
//! - **insider_director_buy**: an outside director (not an officer, not a 10%
//!   owner) buying $10k+ on the open market, filed in the last 3 days. The
//!   one insider rule that held up in 2018-2026: about +1.2% over the
//!   following 60 trading days vs stocks of the same size (t 2.5), +0.5%
//!   after a 6x ATR trailing stop (t 2.2; weaker in 2023-26 alone). Held 60
//!   trading days with loose stops (`ExitRules::LOOSE`) and no ATR floor: the
//!   bot's 3x ATR exits and the calm-stock filter both erased it.
//!
//! News was tested again on 2023-2026 Benzinga headlines (next-day, 10:00
//! and dip entries, both directions): nothing beat the trading cost, so news
//! events are recorded and scored but only traded with
//! `NEWS_TRADES_ENABLED=true`.
//!
//! Event trades are `EVENT_SIZE` of the smallest normal entry (sized as a
//! `SIZING_SCORE` signal, whatever their strength), skip signal decay (they
//! have no compound score to decay from) and close after `max_hold_days`.
//! `EVENT_TRADES_ENABLED=false` stops them; events are still recorded.

use rusqlite::Connection;

/// Tone change (sentiment runs -1..1) that counts as a surprise.
pub const SURPRISE_MIN: f64 = 0.5;
/// Average tone of all news mentions, the baseline for a company with no
/// recent coverage (0.24 over six months).
pub const TYPICAL_TONE: f64 = 0.24;
/// Smallest director buy that counts (bigger ones did no better).
pub const DIRECTOR_DOLLARS: f64 = 10_000.0;
/// Fraction of a normal entry an event trade gets.
pub const EVENT_SIZE: f64 = 0.5;
/// The compound score an event trade is sized as: the bottom tier. Strength
/// is not a compound score, and an unproven signal must not size like a
/// strong proven one.
pub const SIZING_SCORE: f64 = 0.30;

pub const NEWS_SURPRISE: &str = "news_surprise";
pub const INSIDER_CLUSTER: &str = "insider_cluster";
pub const INSIDER_DIRECTOR: &str = "insider_director_buy";

/// Trading days a convergence trade is held at most: the 90 calendar days
/// the backtester's strategy has always assumed (`pulse-weights` strategy
/// `max_hold_days`). Stops and decay usually end it well before; this only
/// retires a position that drifted on with neither firing.
pub const CONVERGENCE_MAX_HOLD: i64 = 60;

/// Trading days a trade is held at most, by what opened it.
pub fn max_hold_days(trigger: &str) -> Option<i64> {
    match trigger {
        NEWS_SURPRISE => Some(5),
        INSIDER_CLUSTER => Some(20),
        INSIDER_DIRECTOR => Some(60),
        _ => Some(CONVERGENCE_MAX_HOLD),
    }
}

pub fn enabled() -> bool {
    std::env::var("EVENT_TRADES_ENABLED").map(|v| !(v.eq_ignore_ascii_case("false") || v == "0")).unwrap_or(true)
}

/// Whether events of `kind` are traded. News is off unless
/// `NEWS_TRADES_ENABLED=true` (no backtest edge); the rest follow `enabled`.
pub fn kind_traded(kind: &str) -> bool {
    let news_on = std::env::var("NEWS_TRADES_ENABLED").map(|v| v.eq_ignore_ascii_case("true") || v == "1").unwrap_or(false);
    kind_traded_with(kind, news_on)
}

fn kind_traded_with(kind: &str, news_on: bool) -> bool {
    kind != NEWS_SURPRISE || news_on
}

/// Whether short events are traded (sold short) as well as long ones.
/// `SHORTS_ENABLED=false` keeps the bot long-only.
pub fn shorts_enabled() -> bool {
    std::env::var("SHORTS_ENABLED").map(|v| !(v.eq_ignore_ascii_case("false") || v == "0")).unwrap_or(true)
}

/// The directions auto-trade acts on.
pub fn directions() -> &'static [&'static str] {
    if shorts_enabled() { &["long", "short"] } else { &["long"] }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Event {
    pub kind: &'static str,
    pub ticker: String,
    pub entity_id: Option<i64>,
    pub day: String,
    pub direction: &'static str,
    pub strength: f64,
    pub detail: serde_json::Value,
}

/// Today's tone for one ticker against its recent baseline.
#[derive(Debug, Clone)]
pub struct ToneRow {
    pub ticker: String,
    pub entity_id: i64,
    pub tone: f64,
    pub mentions: i64,
    pub baseline: Option<f64>,
    pub baseline_mentions: i64,
    pub headline: String,
}

/// News-tone surprises among `rows`.
pub fn news_surprises(rows: &[ToneRow], day: &str) -> Vec<Event> {
    rows.iter()
        .filter_map(|r| {
            let base = r.baseline.filter(|_| r.baseline_mentions > 0).unwrap_or(TYPICAL_TONE);
            let surprise = r.tone - base;
            if !surprise.is_finite() || surprise.abs() < SURPRISE_MIN {
                return None;
            }
            Some(Event {
                kind: NEWS_SURPRISE,
                ticker: r.ticker.clone(),
                entity_id: Some(r.entity_id),
                day: day.to_string(),
                direction: if surprise > 0.0 { "long" } else { "short" },
                strength: surprise.abs().min(1.0),
                detail: serde_json::json!({
                    "tone": r.tone, "baseline": base, "surprise": surprise,
                    "mentions": r.mentions, "baseline_mentions": r.baseline_mentions,
                    "headline": r.headline,
                }),
            })
        })
        .collect()
}

/// Tone of news mentions stored in the last 20 hours (this morning's and last
/// night's briefings) per ticker, against the ticker's earlier 30 days.
pub fn load_tone(conn: &Connection) -> rusqlite::Result<Vec<ToneRow>> {
    let mut stmt = conn.prepare(
        "WITH m AS (
             SELECT et.ticker, em.entity_id, em.sentiment, em.created_at, em.mentioned_at, s.headline
             FROM entity_mentions em
             JOIN stories s ON s.id = em.story_id
             JOIN entity_tickers et ON et.entity_id = em.entity_id
             WHERE s.source_type = 'news' AND em.mentioned_at >= date('now', '-31 days')
         ),
         recent AS (
             SELECT ticker, MIN(entity_id) AS entity_id, AVG(sentiment) AS tone, COUNT(*) AS n,
                    MAX(headline) AS headline
             FROM m WHERE created_at >= datetime('now', '-20 hours')
             GROUP BY ticker
         )
         SELECT r.ticker, r.entity_id, r.tone, r.n,
                (SELECT AVG(sentiment) FROM m WHERE m.ticker = r.ticker AND m.created_at < datetime('now', '-20 hours')),
                (SELECT COUNT(*) FROM m WHERE m.ticker = r.ticker AND m.created_at < datetime('now', '-20 hours')),
                COALESCE(r.headline, '')
         FROM recent r",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok(ToneRow {
            ticker: r.get(0)?,
            entity_id: r.get(1)?,
            tone: r.get(2)?,
            mentions: r.get(3)?,
            baseline: r.get(4)?,
            baseline_mentions: r.get(5)?,
            headline: r.get(6)?,
        })
    })?;
    rows.collect()
}

/// Open-market buys by outside directors (not officers, not 10% owners) of
/// at least `DIRECTOR_DOLLARS`, filed in the last 3 days: one event per
/// ticker and filing day. Filings stored before the 10% owner flag was parsed
/// count as not 10% owners.
pub fn director_buys(conn: &Connection) -> rusqlite::Result<Vec<Event>> {
    let mut stmt = conn.prepare(
        "WITH p AS (
             SELECT UPPER(json_extract(financial_metadata, '$.ticker')) AS ticker,
                    json_extract(financial_metadata, '$.filing_date') AS day,
                    json_extract(financial_metadata, '$.owner_name') AS owner,
                    COALESCE(json_extract(financial_metadata, '$.total_value'), 0) AS value
             FROM stories
             WHERE source_type = 'financial' AND json_valid(financial_metadata)
               AND json_extract(financial_metadata, '$.transaction_code') = 'P'
               AND COALESCE(json_extract(financial_metadata, '$.is_director'), 0) = 1
               AND COALESCE(json_extract(financial_metadata, '$.is_officer'), 0) = 0
               AND COALESCE(json_extract(financial_metadata, '$.is_ten_percent_owner'), 0) = 0
               AND json_extract(financial_metadata, '$.filing_date') >= date('now', '-3 days')
         )
         SELECT p.ticker, p.day, COUNT(DISTINCT p.owner), SUM(p.value), GROUP_CONCAT(DISTINCT p.owner),
                (SELECT entity_id FROM entity_tickers et WHERE et.ticker = p.ticker LIMIT 1)
         FROM p WHERE p.ticker IS NOT NULL AND p.day IS NOT NULL
         GROUP BY p.ticker, p.day
         HAVING SUM(p.value) >= ?1",
    )?;
    let rows = stmt.query_map([DIRECTOR_DOLLARS], |r| {
        let directors: i64 = r.get(2)?;
        let dollars: f64 = r.get(3)?;
        let names: Option<String> = r.get(4)?;
        Ok(Event {
            kind: INSIDER_DIRECTOR,
            ticker: r.get(0)?,
            entity_id: r.get(5)?,
            day: r.get(1)?,
            direction: "long",
            // Size did not predict the return: every buy ranks the same.
            strength: 0.5,
            detail: serde_json::json!({ "directors": directors, "dollars": dollars, "names": names }),
        })
    })?;
    rows.collect()
}

/// Record events. A later sighting the same day replaces the reading (more of
/// the day's news is in by then) but keeps the first detection time. Returns
/// how many were new or changed.
pub fn record(conn: &Connection, events: &[Event]) -> rusqlite::Result<usize> {
    let mut n = 0;
    for e in events {
        n += conn.execute(
            "INSERT INTO event_signals (kind, ticker, entity_id, day, direction, strength, detail, detected_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, datetime('now', 'localtime'))
             ON CONFLICT(kind, ticker, day) DO UPDATE SET direction = excluded.direction,
                 strength = excluded.strength, detail = excluded.detail
             WHERE event_signals.strength != excluded.strength OR event_signals.direction != excluded.direction",
            rusqlite::params![e.kind, e.ticker, e.entity_id, e.day, e.direction, e.strength, e.detail.to_string()],
        )?;
    }
    Ok(n)
}

/// Find and record today's events. Best effort per kind.
pub fn detect(conn: &Connection) -> usize {
    let today = chrono::Local::now().format("%Y-%m-%d").to_string();
    let mut events = match load_tone(conn) {
        Ok(rows) => news_surprises(&rows, &today),
        Err(e) => {
            tracing::warn!("Event signals: news tone unreadable: {}", e);
            Vec::new()
        }
    };
    match director_buys(conn) {
        Ok(d) => events.extend(d),
        Err(e) => tracing::warn!("Event signals: director buys unreadable: {}", e),
    }
    match record(conn, &events) {
        Ok(n) => {
            if n > 0 {
                tracing::info!("Event signals: {} new ({} seen this run)", n, events.len());
            }
            n
        }
        Err(e) => {
            tracing::warn!("Event signals: not recorded: {}", e);
            0
        }
    }
}

/// An event the auto-trade run may act on.
#[derive(Debug, Clone, PartialEq)]
pub struct EventCandidate {
    pub kind: String,
    pub ticker: String,
    pub entity_id: Option<i64>,
    pub name: String,
    pub direction: String,
    pub strength: f64,
    pub detail: String,
}

/// Events first seen today or yesterday (a Form 4 can be found days after
/// it was filed; a director buy waits 4 days, so one found after Friday's
/// last run is still bought Monday) that no trade has acted on, strongest
/// first, of kinds that are traded (`kind_traded`) in `directions`, at most 10.
pub fn candidates(conn: &Connection, directions: &[&str]) -> rusqlite::Result<Vec<EventCandidate>> {
    candidates_with(conn, directions, kind_traded)
}

fn candidates_with(conn: &Connection, directions: &[&str], traded: impl Fn(&str) -> bool) -> rusqlite::Result<Vec<EventCandidate>> {
    let mut stmt = conn.prepare(
        "SELECT ev.kind, ev.ticker, ev.entity_id, COALESCE(e.name, ev.ticker), ev.direction, ev.strength, ev.detail
         FROM event_signals ev
         LEFT JOIN entities e ON e.id = ev.entity_id
         WHERE (ev.day >= date('now', 'localtime', '-1 day')
                OR ev.detected_at >= date('now', 'localtime', '-1 day')
                OR (ev.kind = ?1 AND ev.detected_at >= date('now', 'localtime', '-4 days')))
           AND ev.ticker NOT IN (SELECT ticker FROM paper_trades WHERE status = 'open')
           AND NOT EXISTS (SELECT 1 FROM paper_trades pt WHERE pt.ticker = ev.ticker
                           AND pt.entry_trigger = ev.kind AND substr(pt.entry_date, 1, 10) >= ev.day)
         ORDER BY ev.strength DESC, ev.id",
    )?;
    let rows = stmt.query_map([INSIDER_DIRECTOR], |r| {
        Ok(EventCandidate {
            kind: r.get(0)?,
            ticker: r.get(1)?,
            entity_id: r.get(2)?,
            name: r.get(3)?,
            direction: r.get(4)?,
            strength: r.get(5)?,
            detail: r.get(6)?,
        })
    })?;
    Ok(rows
        .filter_map(Result::ok)
        .filter(|c| directions.contains(&c.direction.as_str()) && traded(&c.kind))
        .take(10)
        .collect())
}

/// Weekdays from `from` (exclusive) to `to` (inclusive): trading days held,
/// ignoring holidays.
pub fn weekdays_between(from: chrono::NaiveDate, to: chrono::NaiveDate) -> i64 {
    use chrono::Datelike;
    let mut d = from;
    let mut n = 0;
    while d < to {
        d = d.succ_opt().unwrap_or(to);
        if d.weekday().number_from_monday() <= 5 {
            n += 1;
        }
    }
    n
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(ticker: &str, tone: f64, baseline: Option<f64>, n: i64) -> ToneRow {
        ToneRow {
            ticker: ticker.into(),
            entity_id: 1,
            tone,
            mentions: 2,
            baseline,
            baseline_mentions: n,
            headline: "h".into(),
        }
    }

    #[test]
    fn a_tone_jump_is_an_event_in_its_direction() {
        let rows = [
            tone("UP", 0.8, Some(0.1), 5),      // +0.7
            tone("DOWN", -0.5, Some(0.3), 5),   // -0.8
            tone("SAME", 0.7, Some(0.6), 5),    // +0.1
            tone("NEW", 0.8, None, 0),          // vs typical 0.24: +0.56
            tone("NEWNEG", -0.2, Some(0.9), 0), // baseline ignored without mentions
        ];
        let ev = news_surprises(&rows, "2026-10-08");
        let got: Vec<(&str, &str)> = ev.iter().map(|e| (e.ticker.as_str(), e.direction)).collect();
        assert_eq!(got, vec![("UP", "long"), ("DOWN", "short"), ("NEW", "long")]);
        assert!((ev[1].strength - 0.8).abs() < 1e-9);
        assert_eq!(ev[0].detail["mentions"], 2);
    }

    fn db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::run_migrations(&conn).unwrap();
        conn.execute_batch(
            "INSERT INTO entities (id, name, name_normalized, entity_type, first_seen, last_seen)
                 VALUES (1, 'Acme', 'acme', 'company', '2026-01-01', '2026-01-01');
             INSERT INTO entity_tickers (entity_id, ticker) VALUES (1, 'ACME');
             INSERT INTO briefings (id, date) VALUES (1, '2026-10-08');",
        )
        .unwrap();
        conn
    }

    fn story(conn: &Connection, sentiment: f64, created: &str) {
        conn.execute(
            "INSERT INTO stories (briefing_id, sector, original_title, original_url, source_name, headline, summary,
                 key_facts, why_it_matters, what_to_watch, url_hash, title_hash, source_type)
             VALUES (1, 'tech', 't', 'u', 'Wire', 'Acme news', '', '[]', '', '', hex(randomblob(8)), hex(randomblob(8)), 'news')",
            [],
        )
        .unwrap();
        let id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO entity_mentions (entity_id, story_id, sentiment, mentioned_at, created_at)
             VALUES (1, ?1, ?2, date(?3), ?3)",
            rusqlite::params![id, sentiment, created],
        )
        .unwrap();
    }

    #[test]
    fn tone_compares_today_with_the_month_before() {
        let conn = db();
        story(&conn, 0.1, &chrono::Utc::now().checked_sub_days(chrono::Days::new(5)).unwrap().format("%Y-%m-%d %H:%M:%S").to_string());
        story(&conn, 0.9, &chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string());
        story(&conn, 0.7, &chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string());
        let rows = load_tone(&conn).unwrap();
        assert_eq!(rows.len(), 1);
        assert!((rows[0].tone - 0.8).abs() < 1e-9);
        assert_eq!((rows[0].mentions, rows[0].baseline_mentions), (2, 1));
        assert!((rows[0].baseline.unwrap() - 0.1).abs() < 1e-9);

        let ev = news_surprises(&rows, "2026-10-08");
        assert_eq!(record(&conn, &ev).unwrap(), 1);
        assert_eq!(record(&conn, &ev).unwrap(), 0, "the same reading changes nothing");
    }

    #[test]
    fn a_later_reading_replaces_the_first_and_late_or_crowded_events_still_trade() {
        let conn = db();
        let ev = |t: &str, day: String, dir: &'static str, s: f64| Event {
            kind: INSIDER_CLUSTER,
            ticker: t.into(),
            entity_id: None,
            day,
            direction: dir,
            strength: s,
            detail: serde_json::json!({}),
        };
        let today = chrono::Local::now().format("%Y-%m-%d").to_string();
        // A Form 4 filed four days ago, found today.
        let old = (chrono::Local::now().date_naive() - chrono::Duration::days(4)).to_string();
        record(&conn, &[ev("LATE", old.clone(), "long", 0.5)]).unwrap();
        assert_eq!(record(&conn, &[ev("LATE", old, "long", 0.75)]).unwrap(), 1);
        let strength: f64 = conn.query_row("SELECT strength FROM event_signals WHERE ticker = 'LATE'", [], |r| r.get(0)).unwrap();
        assert_eq!(strength, 0.75);
        // Twelve stronger shorts do not push the long out of the ten slots.
        let shorts: Vec<Event> = (0..12).map(|i| ev(&format!("S{i}"), today.clone(), "short", 0.9)).collect();
        record(&conn, &shorts).unwrap();
        let c = candidates(&conn, &["long"]).unwrap();
        assert_eq!(c.iter().map(|c| c.ticker.as_str()).collect::<Vec<_>>(), vec!["LATE"]);
        assert_eq!(candidates(&conn, &["long", "short"]).unwrap().len(), 10);
    }

    #[test]
    fn traded_or_held_events_are_not_candidates() {
        let conn = db();
        let today = chrono::Local::now().format("%Y-%m-%d").to_string();
        let ev = |t: &str, dir: &'static str, s: f64| Event {
            kind: NEWS_SURPRISE,
            ticker: t.into(),
            entity_id: Some(1),
            day: today.clone(),
            direction: dir,
            strength: s,
            detail: serde_json::json!({}),
        };
        record(&conn, &[ev("ACME", "long", 0.6), ev("BBB", "long", 0.9), ev("CCC", "short", 0.7), ev("DDD", "long", 0.5)]).unwrap();
        conn.execute(
            "INSERT INTO paper_trades (ticker, direction, entry_price, entry_date, position_size, confidence, signal_profile, status, entry_trigger)
             VALUES ('BBB', 'long', 10, datetime('now', 'localtime'), 100, 0.5, '{}', 'closed', 'news_surprise'),
                    ('DDD', 'long', 10, '2026-01-01', 100, 0.5, '{}', 'open', 'convergence')",
            [],
        )
        .unwrap();
        let c = candidates_with(&conn, &["long"], |_| true).unwrap();
        assert_eq!(c.iter().map(|c| c.ticker.as_str()).collect::<Vec<_>>(), vec!["ACME"]);
        assert_eq!(c[0].name, "Acme");
        assert_eq!(candidates_with(&conn, &["long", "short"], |_| true).unwrap().len(), 2);
    }

    fn form4(conn: &Connection, ticker: &str, owner: &str, code: &str, value: f64, roles: (bool, bool, Option<bool>), filed: &str) {
        let (director, officer, ten) = roles;
        let mut meta = serde_json::json!({
            "ticker": ticker, "owner_name": owner, "transaction_code": code, "total_value": value,
            "is_director": director, "is_officer": officer, "filing_date": filed,
        });
        if let Some(t) = ten {
            meta["is_ten_percent_owner"] = serde_json::json!(t);
        }
        conn.execute(
            "INSERT INTO stories (briefing_id, sector, original_title, original_url, source_name, headline, summary,
                 key_facts, why_it_matters, what_to_watch, url_hash, title_hash, source_type, financial_metadata)
             VALUES (1, 'finance', 't', 'u', 'SEC', 'Form 4', '', '[]', '', '', hex(randomblob(8)), hex(randomblob(8)), 'financial', ?1)",
            [meta.to_string()],
        )
        .unwrap();
    }

    #[test]
    fn only_outside_director_buys_of_10k_are_events() {
        let conn = db();
        let today = chrono::Local::now().format("%Y-%m-%d").to_string();
        let old = (chrono::Local::now().date_naive() - chrono::Duration::days(10)).to_string();
        form4(&conn, "ACME", "Ann", "P", 6_000.0, (true, false, Some(false)), &today);
        form4(&conn, "ACME", "Bob", "P", 5_000.0, (true, false, None), &today); // old row, no flag
        form4(&conn, "CEO", "Cat", "P", 90_000.0, (true, true, Some(false)), &today); // also an officer
        form4(&conn, "OWN", "Fund", "P", 90_000.0, (true, false, Some(true)), &today); // 10% owner
        form4(&conn, "SELL", "Dan", "S", 90_000.0, (true, false, Some(false)), &today);
        form4(&conn, "SMALL", "Eve", "P", 9_000.0, (true, false, Some(false)), &today);
        form4(&conn, "STALE", "Fay", "P", 90_000.0, (true, false, Some(false)), &old);
        let ev = director_buys(&conn).unwrap();
        assert_eq!(ev.iter().map(|e| e.ticker.as_str()).collect::<Vec<_>>(), vec!["ACME"]);
        assert_eq!((ev[0].kind, ev[0].direction, ev[0].entity_id), (INSIDER_DIRECTOR, "long", Some(1)));
        assert_eq!(ev[0].detail["directors"], 2);
        assert_eq!(ev[0].detail["dollars"], 11_000.0);
        // Insider clusters are no longer detected.
        form4(&conn, "ACME", "Cat", "P", 200_000.0, (false, true, Some(false)), &today);
        detect(&conn);
        let kinds: Vec<String> = conn
            .prepare("SELECT DISTINCT kind FROM event_signals").unwrap()
            .query_map([], |r| r.get(0)).unwrap().map(Result::unwrap).collect();
        assert_eq!(kinds, vec![INSIDER_DIRECTOR.to_string()]);
    }

    #[test]
    fn news_events_trade_only_when_switched_on() {
        assert!(!kind_traded_with(NEWS_SURPRISE, false));
        assert!(kind_traded_with(NEWS_SURPRISE, true));
        assert!(kind_traded_with(INSIDER_DIRECTOR, false));
        // Untraded news cannot crowd a director buy out of the ten slots.
        let conn = db();
        let today = chrono::Local::now().format("%Y-%m-%d").to_string();
        let ev = |t: &str, kind: &'static str, s: f64, day: String| Event {
            kind, ticker: t.into(), entity_id: None, day, direction: "long", strength: s, detail: serde_json::json!({}),
        };
        let news: Vec<Event> = (0..12).map(|i| ev(&format!("N{i}"), NEWS_SURPRISE, 0.9, today.clone())).collect();
        record(&conn, &news).unwrap();
        record(&conn, &[ev("DIR", INSIDER_DIRECTOR, 0.5, today.clone())]).unwrap();
        let c = candidates_with(&conn, &["long"], |k| kind_traded_with(k, false)).unwrap();
        assert_eq!(c.iter().map(|c| c.ticker.as_str()).collect::<Vec<_>>(), vec!["DIR"]);
        // A director buy found three days ago (Friday evening) is still a candidate.
        let filed = (chrono::Local::now().date_naive() - chrono::Duration::days(4)).to_string();
        record(&conn, &[ev("WKND", INSIDER_DIRECTOR, 0.5, filed)]).unwrap();
        conn.execute("UPDATE event_signals SET detected_at = datetime('now', 'localtime', '-3 days') WHERE ticker = 'WKND'", []).unwrap();
        let c = candidates_with(&conn, &["long"], |k| kind_traded_with(k, false)).unwrap();
        assert!(c.iter().any(|c| c.ticker == "WKND"));
    }

    #[test]
    fn holding_days_skip_weekends() {
        let d = |s: &str| chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap();
        assert_eq!(weekdays_between(d("2026-10-02"), d("2026-10-05")), 1); // Fri -> Mon
        assert_eq!(weekdays_between(d("2026-10-05"), d("2026-10-12")), 5);
        assert_eq!(weekdays_between(d("2026-10-05"), d("2026-10-05")), 0);
        assert_eq!(max_hold_days(NEWS_SURPRISE), Some(5));
        assert_eq!(max_hold_days(INSIDER_DIRECTOR), Some(60));
        assert_eq!(max_hold_days("convergence"), Some(CONVERGENCE_MAX_HOLD));
    }
}
