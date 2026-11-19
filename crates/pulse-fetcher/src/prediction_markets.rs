//! What prediction markets say, for the briefing page (table
//! `prediction_markets`, migration 049).
//!
//! Polymarket publishes every market's price without a key. A yes/no market's
//! price is the chance of "Yes" that people are paying for, so a price that
//! moved is a crowd changing its mind with money. Each reading asks for the most
//! traded open markets of a few topics (`TOPICS`, one request each), keeps one
//! question per event, and stores a short list: the questions whose odds moved
//! most in 24 hours and the ones with the most money traded.
//!
//! It is reading matter. Nothing here feeds a signal, a score or an order.
//!
//! The market list is text written by strangers: it is parsed leniently, cut to
//! length, and a link is only built from a slug made of plain characters.
//!
//! Refreshed at most every `REFRESH_HOURS`, so every hourly wake of the `daily`
//! mode can call `refresh_if_stale`. `PULSE_PREDICTION_MARKETS=off` switches it
//! off. A reading that fails as a whole or in part keeps the previous one.

use chrono::{NaiveDate, NaiveDateTime};
use rusqlite::{Connection, params};
use serde_json::Value;
use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::io::Read;

const BASE: &str = "https://gamma-api.polymarket.com";
const SOURCE: &str = "polymarket";

/// Topics asked for, as (name stored, the source's tag id). A market filed
/// under several topics is counted under the first one listed here, and so is
/// the rest of its event. Ids read from `/tags/slug/<name>` on 2026-11-19.
pub const TOPICS: &[(&str, u32)] =
    &[("ai", 439), ("tech", 1401), ("economy", 100_328), ("finance", 120), ("geopolitics", 100_265), ("politics", 2)];

/// Markets asked for per topic (most traded first).
const PAGE: usize = 100;
/// Dollars traded in 24 hours below which a price is not worth quoting.
pub const MIN_VOLUME: f64 = 10_000.0;
/// Dollars resting in the order book below which one small order moves the
/// price. Measured 2026-11-19: the +31 and +36 point "moves" of the day sat on
/// $243 and $263 of liquidity.
pub const MIN_LIQUIDITY: f64 = 10_000.0;
/// A change of at least this much in 24 hours (0.05 = five points) is a move.
pub const MOVE_POINTS: f64 = 0.05;
/// Outside this band a question that did not move is as good as settled.
const OPEN_BAND: (f64, f64) = (0.03, 0.97);
/// Questions kept per topic, in each of the two lists.
const PER_TOPIC: usize = 2;
/// Questions kept in each of the two lists.
const PER_KIND: usize = 5;
/// Skip the requests when the last reading is more recent than this.
pub const REFRESH_HOURS: i64 = 3;
/// Days of readings kept.
pub const KEEP_DAYS: i64 = 180;
/// All the requests of one reading together may take this long (they take
/// about three seconds). The reading runs before the briefing, which must not
/// wait on a slow source.
const BUDGET: std::time::Duration = std::time::Duration::from_secs(45);
/// Pause between requests.
const CALL_GAP: std::time::Duration = std::time::Duration::from_millis(300);
/// The largest reply read, after unpacking (a page is under 1 MB).
const MAX_BODY: u64 = 20 * 1024 * 1024;
const QUESTION_MAX_CHARS: usize = 300;
const STAMP: &str = "%Y-%m-%dT%H:%M:%S";

#[derive(Debug, Clone, PartialEq)]
pub struct Market {
    pub id: String,
    /// The event the market belongs to ("Fed decision in December" has one
    /// market per outcome). The market's own id when the source names none.
    pub event_id: String,
    pub question: String,
    pub event_title: Option<String>,
    pub url: Option<String>,
    /// Chance of "Yes", 0..1.
    pub probability: f64,
    pub change_1d: Option<f64>,
    pub change_1w: Option<f64>,
    pub volume_24h: f64,
    pub liquidity: f64,
    pub end_date: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// The odds moved at least `MOVE_POINTS` in 24 hours.
    Mover,
    /// No such move; listed for the money traded.
    Active,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Mover => "mover",
            Kind::Active => "active",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    pub market: Market,
    pub topic: &'static str,
    pub kind: Kind,
}

/// A number given as a number or as text, if it is finite.
fn num(v: Option<&Value>) -> Option<f64> {
    match v? {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
    .filter(|n| n.is_finite())
}

/// A list given as a list or (as this source does) as JSON inside a string.
fn list(v: Option<&Value>) -> Vec<Value> {
    match v {
        Some(Value::Array(a)) => a.clone(),
        Some(Value::String(s)) => serde_json::from_str(s).unwrap_or_default(),
        _ => Vec::new(),
    }
}

fn text(v: Option<&Value>) -> Option<String> {
    let s = v?.as_str()?.split_whitespace().collect::<Vec<_>>().join(" ");
    (!s.is_empty()).then_some(s)
}

/// An id given as text or as a number.
fn id(v: Option<&Value>) -> Option<String> {
    match v? {
        Value::String(s) if !s.trim().is_empty() => Some(s.trim().to_string()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

fn clip(s: String, max: usize) -> String {
    if s.chars().count() <= max {
        return s;
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// The page of an event, only when its slug is made of plain characters.
fn event_url(slug: Option<&str>) -> Option<String> {
    let slug = slug?;
    let plain = !slug.is_empty() && slug.len() <= 200 && slug.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
    plain.then(|| format!("https://polymarket.com/event/{slug}"))
}

/// The open yes/no markets in a reply. Anything else (a closed market, one
/// with other outcomes, one without a usable price) is left out.
pub fn parse_markets(body: &Value) -> Vec<Market> {
    body.as_array().map(Vec::as_slice).unwrap_or_default().iter().filter_map(parse_market).collect()
}

fn parse_market(m: &Value) -> Option<Market> {
    let flag = |k: &str| m.get(k).and_then(Value::as_bool);
    if flag("closed") == Some(true) || flag("active") == Some(false) || flag("archived") == Some(true) || flag("acceptingOrders") == Some(false) {
        return None;
    }
    let outcomes = list(m.get("outcomes"));
    let named = |i: usize, want: &str| outcomes.get(i).and_then(Value::as_str).is_some_and(|o| o.eq_ignore_ascii_case(want));
    if outcomes.len() != 2 || !named(0, "yes") || !named(1, "no") {
        return None;
    }
    let probability = num(list(m.get("outcomePrices")).first()).filter(|p| (0.0..=1.0).contains(p))?;
    let market_id = id(m.get("id"))?;
    let question = clip(text(m.get("question"))?, QUESTION_MAX_CHARS);
    let event = m.get("events").and_then(Value::as_array).and_then(|e| e.first());
    let change = |k: &str| num(m.get(k)).filter(|c| c.abs() <= 1.0);
    Some(Market {
        event_id: event.and_then(|e| id(e.get("id"))).unwrap_or_else(|| format!("market-{market_id}")),
        id: market_id,
        question,
        event_title: event.and_then(|e| text(e.get("title"))).map(|t| clip(t, QUESTION_MAX_CHARS)),
        url: event_url(event.and_then(|e| e.get("slug")).and_then(Value::as_str)),
        probability,
        change_1d: change("oneDayPriceChange"),
        change_1w: change("oneWeekPriceChange"),
        volume_24h: num(m.get("volume24hr")).unwrap_or(0.0).max(0.0),
        liquidity: num(m.get("liquidityNum")).or_else(|| num(m.get("liquidity"))).unwrap_or(0.0).max(0.0),
        end_date: text(m.get("endDate")).map(|d| clip(d, 40)),
    })
}

fn moved(m: &Market) -> bool {
    m.change_1d.is_some_and(|c| c.abs() >= MOVE_POINTS)
}

fn move_size(m: &Market) -> f64 {
    m.change_1d.map_or(0.0, f64::abs)
}

/// Bets on how often someone posts or which words a speech will contain: a
/// lot of money and nothing to learn from the price.
fn is_novelty(m: &Market) -> bool {
    let question = m.question.to_lowercase();
    let event = m.event_title.as_deref().unwrap_or_default().to_lowercase();
    let says = |s: &str| s.contains(" say \"") || s.contains(" say “") || s.contains(" say '");
    question.contains(" tweets") || event.contains(" tweets") || says(&question) || (event.starts_with("what will") && event.contains(" say"))
}

/// Enough money behind the price, and either it moved or it is still open.
fn worth_showing(m: &Market) -> bool {
    m.volume_24h >= MIN_VOLUME
        && m.liquidity >= MIN_LIQUIDITY
        && !is_novelty(m)
        && (moved(m) || (OPEN_BAND.0..=OPEN_BAND.1).contains(&m.probability))
}

fn desc(a: f64, b: f64) -> Ordering {
    b.partial_cmp(&a).unwrap_or(Ordering::Equal)
}

/// The one question that stands for an event. When outcomes moved it is the
/// one that moved most, except that a loss gives way to a gain at least half
/// its size: money leaving one outcome lands on another, and "raises $75B to
/// $85B: 97%, up 94" reads better than its mirror "raises $65B to $75B: 2%,
/// down 81". When none moved it is the one nearest to even odds (in "ceasefire
/// by November / December / March" that is the date still in doubt; in "who
/// wins" it is the favourite, unless that one is all but certain).
fn lead<'a>(markets: &[&'a Market]) -> Option<(&'a Market, Kind)> {
    let biggest = |a: &&&Market, b: &&&Market| desc(move_size(a), move_size(b)).then(desc(a.volume_24h, b.volume_24h)).then(a.id.cmp(&b.id));
    let rose = |m: &Market| m.change_1d.is_some_and(|c| c > 0.0);
    if let Some(top) = markets.iter().filter(|m| moved(m)).min_by(biggest) {
        let gain = markets.iter().filter(|m| moved(m) && rose(m) && move_size(m) >= move_size(top) / 2.0).min_by(biggest);
        return Some((*gain.unwrap_or(top), Kind::Mover));
    }
    // In tenths of a point, so 10% and 90% are the same distance from even.
    let doubt = |m: &Market| ((m.probability - 0.5).abs() * 1000.0).round();
    markets
        .iter()
        .min_by(|a, b| desc(doubt(b), doubt(a)).then(desc(a.probability, b.probability)).then(a.id.cmp(&b.id)))
        .map(|m| (*m, Kind::Active))
}

/// The day's short list from the topics' pages, in display order: the
/// questions that moved most, then the most traded of the rest. One question
/// per event, at most `PER_TOPIC` per topic and `PER_KIND` in each list.
pub fn select(pages: &[(&'static str, Vec<Market>)]) -> Vec<Row> {
    let mut seen: HashSet<&str> = HashSet::new();
    let mut by_event: HashMap<&str, usize> = HashMap::new();
    let mut events: Vec<(&'static str, Vec<&Market>)> = Vec::new();
    for (topic, markets) in pages {
        for m in markets {
            if !seen.insert(m.id.as_str()) || !worth_showing(m) {
                continue;
            }
            let at = *by_event.entry(m.event_id.as_str()).or_insert_with(|| {
                events.push((*topic, Vec::new()));
                events.len() - 1
            });
            events[at].1.push(m);
        }
    }

    let mut movers = Vec::new();
    let mut active = Vec::new();
    for (topic, markets) in &events {
        if let Some((market, kind)) = lead(markets) {
            let row = Row { market: market.clone(), topic, kind };
            match kind {
                Kind::Mover => movers.push(row),
                Kind::Active => active.push(row),
            }
        }
    }
    let by_id = |a: &Row, b: &Row| a.market.id.cmp(&b.market.id);
    movers.sort_by(|a, b| desc(move_size(&a.market), move_size(&b.market)).then(desc(a.market.volume_24h, b.market.volume_24h)).then(by_id(a, b)));
    active.sort_by(|a, b| desc(a.market.volume_24h, b.market.volume_24h).then(by_id(a, b)));

    let mut out = capped(movers);
    out.extend(capped(active));
    out
}

fn capped(rows: Vec<Row>) -> Vec<Row> {
    let mut per_topic: HashMap<&str, usize> = HashMap::new();
    let mut out = Vec::new();
    for row in rows {
        if out.len() >= PER_KIND {
            break;
        }
        let n = per_topic.entry(row.topic).or_insert(0);
        if *n < PER_TOPIC {
            *n += 1;
            out.push(row);
        }
    }
    out
}

/// Replace `day`'s reading with `rows` and drop days older than `KEEP_DAYS`.
pub fn store(conn: &Connection, rows: &[Row], day: NaiveDate, fetched_at: NaiveDateTime) -> rusqlite::Result<usize> {
    let tx = conn.unchecked_transaction()?;
    tx.execute(
        "DELETE FROM prediction_markets WHERE day = ?1 OR day < ?2",
        params![day.to_string(), (day - chrono::Duration::days(KEEP_DAYS)).to_string()],
    )?;
    let stamp = fetched_at.format(STAMP).to_string();
    for (i, r) in rows.iter().enumerate() {
        let m = &r.market;
        tx.execute(
            "INSERT INTO prediction_markets
               (day, source, market_id, topic, question, event_title, url, probability, change_1d, change_1w,
                volume_24h, liquidity, end_date, kind, rank, fetched_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
            params![
                day.to_string(),
                SOURCE,
                m.id,
                r.topic,
                m.question,
                m.event_title,
                m.url,
                m.probability,
                m.change_1d,
                m.change_1w,
                m.volume_24h,
                m.liquidity,
                m.end_date,
                r.kind.as_str(),
                (i + 1) as i64,
                stamp,
            ],
        )?;
    }
    tx.commit()?;
    Ok(rows.len())
}

/// Whether the newest reading is younger than `REFRESH_HOURS`. A reading
/// stamped in the future (the clock went back) does not count.
fn fresh(conn: &Connection, now: NaiveDateTime) -> bool {
    conn.query_row("SELECT MAX(fetched_at) FROM prediction_markets", [], |r| r.get::<_, Option<String>>(0))
        .ok()
        .flatten()
        .and_then(|s| NaiveDateTime::parse_from_str(&s, STAMP).ok())
        .is_some_and(|last| {
            let age = now - last;
            age >= chrono::Duration::zero() && age < chrono::Duration::hours(REFRESH_HOURS)
        })
}

fn switched_off(value: Option<&str>) -> bool {
    value.is_some_and(|v| matches!(v.trim().to_lowercase().as_str(), "off" | "false" | "0"))
}

/// A reply's body as text bytes, unpacked when the server packed it.
fn unpack(bytes: &[u8], gzip: bool) -> anyhow::Result<Vec<u8>> {
    let mut out = Vec::new();
    if gzip {
        flate2::read::GzDecoder::new(bytes).take(MAX_BODY + 1).read_to_end(&mut out)?;
    } else {
        out.extend_from_slice(bytes);
    }
    if out.len() as u64 > MAX_BODY {
        anyhow::bail!("prediction markets reply is larger than {MAX_BODY} bytes");
    }
    Ok(out)
}

async fn fetch_topic(client: &reqwest::Client, tag_id: u32) -> anyhow::Result<Vec<Market>> {
    let resp = client
        .get(format!("{BASE}/markets"))
        .query(&[
            ("tag_id", tag_id.to_string()),
            ("related_tags", "true".to_string()),
            ("active", "true".to_string()),
            ("closed", "false".to_string()),
            ("order", "volume24hr".to_string()),
            ("ascending", "false".to_string()),
            ("limit", PAGE.to_string()),
            ("liquidity_num_min", format!("{MIN_LIQUIDITY:.0}")),
        ])
        // A page is 800 KB plain and 100 KB packed. Asked for by hand because
        // the shared HTTP client is built without unpacking.
        .header(reqwest::header::ACCEPT_ENCODING, "gzip")
        .send()
        .await?;
    if !resp.status().is_success() {
        anyhow::bail!("prediction markets returned {}", resp.status());
    }
    let gzip = resp
        .headers()
        .get(reqwest::header::CONTENT_ENCODING)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.trim().eq_ignore_ascii_case("gzip"));
    let bytes = resp.bytes().await?;
    let body: Value = serde_json::from_slice(&unpack(&bytes, gzip)?)?;
    if !body.is_array() {
        anyhow::bail!("prediction markets reply is not a list");
    }
    Ok(parse_markets(&body))
}

/// Read the markets and store the day's short list unless that was done in the
/// last `REFRESH_HOURS` (`force` skips that check). Returns the rows stored,
/// or None when there was nothing to do: switched off, no database yet, or a
/// recent reading. Any failed request keeps the previous reading.
pub async fn refresh(db_path: &std::path::Path, force: bool) -> anyhow::Result<Option<usize>> {
    if switched_off(std::env::var("PULSE_PREDICTION_MARKETS").ok().as_deref()) {
        return Ok(None);
    }
    // Creating the database is the briefing writer's job.
    if !db_path.exists() {
        return Ok(None);
    }
    let conn = Connection::open(db_path)?;
    conn.busy_timeout(std::time::Duration::from_secs(10))?;
    // This runs before the briefing has brought the schema up to date.
    crate::db::run_migrations(&conn)?;
    if !force && fresh(&conn, chrono::Local::now().naive_local()) {
        return Ok(None);
    }

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        .connect_timeout(std::time::Duration::from_secs(10))
        .user_agent("Pulse/1.0 (news reader)")
        .build()?;
    let mut pages = Vec::new();
    let mut calls = 0u32;
    let mut failure = None;
    let deadline = tokio::time::Instant::now() + BUDGET;
    for (topic, tag_id) in TOPICS {
        if calls > 0 {
            tokio::time::sleep(CALL_GAP).await;
        }
        calls += 1;
        let answer = match tokio::time::timeout_at(deadline, fetch_topic(&client, *tag_id)).await {
            Ok(answer) => answer,
            Err(_) => Err(anyhow::anyhow!("the reading took longer than {} seconds", BUDGET.as_secs())),
        };
        match answer {
            Ok(markets) => pages.push((*topic, markets)),
            Err(e) => {
                failure = Some(e.context(format!("topic {topic}")));
                break;
            }
        }
    }
    crate::db::log_fetch_calls(&conn, SOURCE, "prediction_markets", calls);
    if let Some(e) = failure {
        return Err(e);
    }
    let read: usize = pages.iter().map(|(_, m)| m.len()).sum();
    let rows = select(&pages);
    // Nothing worth showing out of every topic is a broken reply, not a quiet
    // day: keep what is there.
    if rows.is_empty() {
        anyhow::bail!("prediction markets: nothing usable in {read} markets");
    }
    let now = chrono::Local::now().naive_local();
    let n = store(&conn, &rows, now.date(), now)?;
    let movers = rows.iter().filter(|r| r.kind == Kind::Mover).count();
    tracing::info!("Prediction markets: {n} questions stored ({movers} moved, {read} markets read, {calls} request(s))");
    Ok(Some(n))
}

pub async fn refresh_if_stale(db_path: &std::path::Path) -> anyhow::Result<Option<usize>> {
    refresh(db_path, false).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn market(id: &str, event: &str, p: f64, change: Option<f64>, volume: f64) -> Market {
        Market {
            id: id.to_string(),
            event_id: event.to_string(),
            question: format!("Question {id}?"),
            event_title: Some(format!("Event {event}")),
            url: None,
            probability: p,
            change_1d: change,
            change_1w: None,
            volume_24h: volume,
            liquidity: 50_000.0,
            end_date: None,
        }
    }

    fn ids(rows: &[Row]) -> Vec<&str> {
        rows.iter().map(|r| r.market.id.as_str()).collect()
    }

    fn conn() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(include_str!("../../../migrations/049_prediction_markets.sql")).unwrap();
        c
    }

    fn at(s: &str) -> NaiveDateTime {
        NaiveDateTime::parse_from_str(s, STAMP).unwrap()
    }

    /// One entry as the source sends it (2026-11-19), cut to the fields read.
    fn sample() -> Value {
        json!({
            "id": "2412744",
            "question": "Will the Fed increase interest rates by 25 bps after the December 2026 meeting?",
            "slug": "will-the-fed-increase-interest-rates-by-25-bps",
            "endDate": "2026-12-09T00:00:00Z",
            "outcomes": "[\"Yes\", \"No\"]",
            "outcomePrices": "[\"0.915\", \"0.085\"]",
            "active": true,
            "closed": false,
            "acceptingOrders": true,
            "volume24hr": 504231.45,
            "liquidityNum": 913957.2,
            "liquidity": "913957.2",
            "oneDayPriceChange": -0.02,
            "oneWeekPriceChange": 0.05,
            "events": [{"id": "548858", "slug": "fed-decision-in-december", "title": "Fed Decision in December?"}]
        })
    }

    #[test]
    fn parses_a_market_as_the_source_sends_it() {
        let got = parse_markets(&json!([sample()]));
        assert_eq!(got.len(), 1);
        let m = &got[0];
        assert_eq!(m.id, "2412744");
        assert_eq!(m.event_id, "548858");
        assert_eq!(m.event_title.as_deref(), Some("Fed Decision in December?"));
        assert_eq!(m.url.as_deref(), Some("https://polymarket.com/event/fed-decision-in-december"));
        assert!((m.probability - 0.915).abs() < 1e-9);
        assert_eq!(m.change_1d, Some(-0.02));
        assert_eq!(m.change_1w, Some(0.05));
        assert!((m.volume_24h - 504_231.45).abs() < 1e-6);
        assert!((m.liquidity - 913_957.2).abs() < 1e-6);
        assert_eq!(m.end_date.as_deref(), Some("2026-12-09T00:00:00Z"));
    }

    #[test]
    fn leaves_out_what_is_not_an_open_yes_no_market() {
        let with = |f: &dyn Fn(&mut Value)| {
            let mut m = sample();
            f(&mut m);
            m
        };
        let body = json!([
            with(&|m| m["closed"] = json!(true)),
            with(&|m| m["active"] = json!(false)),
            with(&|m| m["acceptingOrders"] = json!(false)),
            with(&|m| m["outcomes"] = json!("[\"Anthropic\", \"OpenAI\"]")),
            with(&|m| m["outcomes"] = json!("[\"Yes\", \"No\", \"Maybe\"]")),
            with(&|m| m["outcomes"] = json!("not json")),
            with(&|m| m["outcomePrices"] = json!("[\"abc\", \"0.1\"]")),
            with(&|m| m["outcomePrices"] = json!("[\"1.5\", \"0.1\"]")),
            with(&|m| m["outcomePrices"] = json!("[]")),
            with(&|m| m["question"] = json!("   ")),
            with(&|m| {
                m.as_object_mut().unwrap().remove("id");
            }),
            json!("text"),
            json!(null),
        ]);
        assert_eq!(parse_markets(&body), vec![]);
        // A reply that is not a list at all.
        assert_eq!(parse_markets(&json!({"error": "nope"})), vec![]);
    }

    #[test]
    fn missing_or_odd_fields_do_not_drop_a_market() {
        let mut m = sample();
        m["id"] = json!(77);
        m["outcomes"] = json!(["yes", "NO"]);
        m["outcomePrices"] = json!([0.4, 0.6]);
        m["oneDayPriceChange"] = json!(null);
        m["oneWeekPriceChange"] = json!(7.5);
        m["volume24hr"] = json!("1200.5");
        m["question"] = json!(format!("  Will   it\nrain? {}", "x".repeat(400)));
        let o = m.as_object_mut().unwrap();
        o.remove("events");
        o.remove("liquidityNum");
        o.remove("endDate");
        let got = parse_markets(&json!([m]));
        assert_eq!(got.len(), 1);
        let m = &got[0];
        assert_eq!(m.id, "77");
        assert_eq!(m.event_id, "market-77");
        assert_eq!(m.event_title, None);
        assert_eq!(m.url, None);
        assert_eq!(m.probability, 0.4);
        assert_eq!(m.change_1d, None);
        // A change of more than the whole scale is not a change.
        assert_eq!(m.change_1w, None);
        assert_eq!(m.volume_24h, 1200.5);
        // Falls back to the text field.
        assert!((m.liquidity - 913_957.2).abs() < 1e-6);
        assert!(m.question.starts_with("Will it rain? x"));
        assert_eq!(m.question.chars().count(), QUESTION_MAX_CHARS);
        assert!(m.question.ends_with('…'));
    }

    #[test]
    fn a_link_is_only_built_from_a_plain_slug() {
        assert_eq!(event_url(Some("fed-decision-2026")).as_deref(), Some("https://polymarket.com/event/fed-decision-2026"));
        for bad in ["", "a/b", "../x", "a?b=c", "a b", "UPPER", "javascript:alert(1)", "a#b", "é"] {
            assert_eq!(event_url(Some(bad)), None, "{bad}");
        }
        assert_eq!(event_url(Some(&"a".repeat(201))), None);
        assert_eq!(event_url(None), None);
    }

    #[test]
    fn thin_settled_and_novelty_markets_are_not_shown() {
        let ok = market("ok", "e", 0.5, None, 20_000.0);
        assert!(worth_showing(&ok));
        assert!(!worth_showing(&Market { volume_24h: MIN_VOLUME - 1.0, ..ok.clone() }));
        assert!(!worth_showing(&Market { liquidity: MIN_LIQUIDITY - 1.0, change_1d: Some(0.4), ..ok.clone() }));
        // All but settled and not moving.
        assert!(!worth_showing(&Market { probability: 0.001, ..ok.clone() }));
        assert!(!worth_showing(&Market { probability: 0.99, change_1d: Some(0.01), ..ok.clone() }));
        // Settled today: that is the news.
        assert!(worth_showing(&Market { probability: 0.99, change_1d: Some(0.6), ..ok.clone() }));
        assert!(worth_showing(&Market { probability: 0.002, change_1d: Some(-0.3), ..ok.clone() }));
        for (question, event) in [
            ("Will Elon Musk post 300-319 tweets from November 13 to November 20, 2026?", "Elon Musk # tweets November 13 - November 20, 2026"),
            ("Will Trump say \"Nuclear\" or \"Nuke\" during his meeting?", "What will Trump say during his meeting?"),
            ("Will the word come up?", "What will Powell say at the press conference?"),
        ] {
            let m = Market { question: question.to_string(), event_title: Some(event.to_string()), change_1d: Some(0.3), ..ok.clone() };
            assert!(!worth_showing(&m), "{question}");
        }
        // The word alone is not enough to drop a real question.
        let real = Market { question: "Will the Fed say rates are on hold?".to_string(), event_title: Some("Fed in December".to_string()), ..ok.clone() };
        assert!(worth_showing(&real));
    }

    #[test]
    fn a_gain_stands_for_an_event_before_its_mirror_loss() {
        let pages = vec![
            ("ai", vec![market("down", "raise", 0.02, Some(-0.8), 900_000.0), market("up", "raise", 0.6, Some(0.5), 20_000.0)]),
            // Nothing gained: the biggest loss.
            ("tech", vec![market("d1", "out", 0.3, Some(-0.06), 900_000.0), market("d2", "out", 0.1, Some(-0.2), 20_000.0)]),
            // A collapse is not hidden behind a small gain elsewhere in the event.
            ("economy", vec![market("crash", "deal", 0.05, Some(-0.6), 50_000.0), market("blip", "deal", 0.4, Some(0.06), 900_000.0)]),
        ];
        let rows = select(&pages);
        assert_eq!(ids(&rows), vec!["crash", "up", "d2"]);
    }

    #[test]
    fn an_event_is_one_question_the_one_that_moved_most() {
        let pages = vec![(
            "finance",
            vec![
                market("a", "ipo", 0.27, Some(-0.06), 900_000.0),
                market("b", "ipo", 0.53, Some(0.12), 200_000.0),
                market("c", "ipo", 0.16, Some(0.01), 400_000.0),
            ],
        )];
        let rows = select(&pages);
        assert_eq!(ids(&rows), vec!["b"]);
        assert_eq!(rows[0].kind, Kind::Mover);
        assert_eq!(rows[0].topic, "finance");
    }

    #[test]
    fn a_quiet_event_is_the_outcome_still_in_doubt() {
        // A ladder of dates: the far end is the open question.
        let ladder = vec![market("nov", "truce", 0.94, Some(0.0), 300_000.0), market("dec", "truce", 0.65, Some(-0.02), 100_000.0)];
        // A race: the favourite.
        let race = vec![market("x", "race", 0.06, None, 500_000.0), market("y", "race", 0.46, Some(-0.03), 100_000.0), market("z", "race", 0.2, None, 90_000.0)];
        // Even distance from 50%: the likelier one.
        let even = vec![market("lo", "tie", 0.1, None, 50_000.0), market("hi", "tie", 0.9, None, 40_000.0)];
        let rows = select(&[("geopolitics", ladder), ("politics", race), ("economy", even)]);
        assert_eq!(ids(&rows), vec!["dec", "y", "hi"]);
        assert!(rows.iter().all(|r| r.kind == Kind::Active));
    }

    #[test]
    fn movers_come_first_by_size_then_the_most_traded() {
        let pages = vec![
            ("ai", vec![market("small-move", "e1", 0.4, Some(0.05), 900_000.0), market("busy", "e2", 0.5, Some(0.01), 800_000.0)]),
            ("tech", vec![market("big-drop", "e3", 0.2, Some(-0.4), 20_000.0), market("quiet", "e4", 0.5, None, 30_000.0)]),
        ];
        let rows = select(&pages);
        assert_eq!(ids(&rows), vec!["big-drop", "small-move", "busy", "quiet"]);
        assert_eq!(rows.iter().map(|r| r.kind).collect::<Vec<_>>(), vec![Kind::Mover, Kind::Mover, Kind::Active, Kind::Active]);
    }

    #[test]
    fn a_market_under_two_topics_counts_once_under_the_first() {
        let shared = market("fed", "fed-dec", 0.9, Some(-0.02), 500_000.0);
        // Another outcome of the same event, seen only under the later topic.
        let sibling = market("fed-50", "fed-dec", 0.5, Some(0.2), 400_000.0);
        let pages = vec![("economy", vec![shared.clone()]), ("politics", vec![shared, sibling])];
        let rows = select(&pages);
        assert_eq!(ids(&rows), vec!["fed-50"]);
        assert_eq!(rows[0].topic, "economy");
    }

    #[test]
    fn no_topic_takes_more_than_its_share_and_lists_are_capped() {
        let mut pages = Vec::new();
        for (t, topic) in ["ai", "tech", "economy", "finance"].into_iter().enumerate() {
            let mut markets = Vec::new();
            for i in 0..4 {
                let n = t * 10 + i;
                markets.push(market(&format!("m{n:02}"), &format!("em{n}"), 0.5, Some(0.5 - n as f64 / 100.0), 20_000.0));
                markets.push(market(&format!("a{n:02}"), &format!("ea{n}"), 0.5, None, 1_000_000.0 - n as f64));
            }
            pages.push((topic, markets));
        }
        let rows = select(&pages);
        let movers: Vec<&Row> = rows.iter().filter(|r| r.kind == Kind::Mover).collect();
        let active: Vec<&Row> = rows.iter().filter(|r| r.kind == Kind::Active).collect();
        assert_eq!(movers.len(), PER_KIND);
        assert_eq!(active.len(), PER_KIND);
        // Movers before the rest, as displayed.
        assert!(rows[..PER_KIND].iter().all(|r| r.kind == Kind::Mover));
        for list in [&movers, &active] {
            for topic in ["ai", "tech", "economy", "finance"] {
                assert!(list.iter().filter(|r| r.topic == topic).count() <= PER_TOPIC, "{topic}");
            }
        }
        // The first topic's two biggest, then the next topic's, not its third.
        assert_eq!(movers.iter().map(|r| r.market.id.as_str()).collect::<Vec<_>>(), vec!["m00", "m01", "m10", "m11", "m20"]);
    }

    #[test]
    fn a_market_with_numbers_that_are_not_numbers_is_left_out() {
        let mut odd = market("nan", "e1", 0.5, Some(f64::NAN), f64::NAN);
        odd.liquidity = f64::NAN;
        let pages = vec![("ai", vec![odd, market("ok", "e2", 0.5, Some(0.1), 20_000.0)])];
        assert_eq!(ids(&select(&pages)), vec!["ok"]);
    }

    #[test]
    fn a_later_reading_replaces_the_day_and_old_days_go() {
        let c = conn();
        let day: NaiveDate = "2026-11-19".parse().unwrap();
        let old = day - chrono::Duration::days(KEEP_DAYS + 1);
        let kept = day - chrono::Duration::days(KEEP_DAYS);
        let row = |id: &str, kind| Row { market: market(id, "e", 0.4, Some(0.1), 20_000.0), topic: "ai", kind };
        store(&c, &[row("gone", Kind::Mover)], old, old.and_hms_opt(8, 0, 0).unwrap()).unwrap();
        store(&c, &[row("stays", Kind::Mover)], kept, kept.and_hms_opt(8, 0, 0).unwrap()).unwrap();
        store(&c, &[row("morning", Kind::Mover), row("both", Kind::Active)], day, at("2026-11-19T08:00:00")).unwrap();
        let n = store(&c, &[row("both", Kind::Mover), row("evening", Kind::Active)], day, at("2026-11-19T21:00:00")).unwrap();
        assert_eq!(n, 2);

        let mut stmt = c.prepare("SELECT day, market_id, kind, rank, fetched_at FROM prediction_markets ORDER BY day, rank").unwrap();
        let got: Vec<(String, String, String, i64, String)> =
            stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))).unwrap().map(Result::unwrap).collect();
        assert_eq!(
            got,
            vec![
                (kept.to_string(), "stays".into(), "mover".into(), 1, format!("{kept}T08:00:00")),
                ("2026-11-19".into(), "both".into(), "mover".into(), 1, "2026-11-19T21:00:00".into()),
                ("2026-11-19".into(), "evening".into(), "active".into(), 2, "2026-11-19T21:00:00".into()),
            ]
        );
        let (source, topic, p, change): (String, String, f64, Option<f64>) =
            c.query_row("SELECT source, topic, probability, change_1d FROM prediction_markets WHERE market_id = 'evening'", [], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
            })
            .unwrap();
        assert_eq!((source.as_str(), topic.as_str(), p, change), ("polymarket", "ai", 0.4, Some(0.1)));
    }

    #[test]
    fn a_reading_is_fresh_for_three_hours() {
        let c = conn();
        let now = at("2026-11-19T12:00:00");
        assert!(!fresh(&c, now), "an empty table is never fresh");
        let row = Row { market: market("a", "e", 0.4, None, 20_000.0), topic: "ai", kind: Kind::Active };
        store(&c, std::slice::from_ref(&row), now.date(), at("2026-11-19T09:00:00")).unwrap();
        assert!(!fresh(&c, now), "three hours old is due");
        store(&c, std::slice::from_ref(&row), now.date(), at("2026-11-19T09:00:01")).unwrap();
        assert!(fresh(&c, now));
        // Stamped later than now: the clock went back, so read again.
        store(&c, std::slice::from_ref(&row), now.date(), at("2026-11-19T12:30:00")).unwrap();
        assert!(!fresh(&c, now));
    }

    #[test]
    fn the_switch_reads_off_false_and_zero() {
        for off in ["off", "OFF", " false ", "0"] {
            assert!(switched_off(Some(off)), "{off}");
        }
        for on in ["", "on", "1", "true"] {
            assert!(!switched_off(Some(on)), "{on}");
        }
        assert!(!switched_off(None));
    }

    #[test]
    fn a_packed_reply_is_unpacked_and_a_plain_one_left_alone() {
        use std::io::Write;
        let text = serde_json::to_vec(&json!([sample()])).unwrap();
        let mut packer = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        packer.write_all(&text).unwrap();
        let packed = packer.finish().unwrap();
        assert_ne!(packed, text);
        assert_eq!(unpack(&packed, true).unwrap(), text);
        assert_eq!(unpack(&text, false).unwrap(), text);
        assert!(unpack(&text, true).is_err(), "not packed although it says so");
    }

    #[tokio::test]
    async fn no_database_means_nothing_to_do() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("missing.db");
        assert_eq!(refresh(&path, true).await.unwrap(), None);
        assert!(!path.exists(), "it must not create the database");
    }

    #[test]
    fn the_topic_names_are_distinct() {
        let names: HashSet<&str> = TOPICS.iter().map(|(n, _)| *n).collect();
        let tags: HashSet<u32> = TOPICS.iter().map(|(_, t)| *t).collect();
        assert_eq!(names.len(), TOPICS.len());
        assert_eq!(tags.len(), TOPICS.len());
    }
}
