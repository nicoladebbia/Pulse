//! Plain-language explanation of a trade: why Pulse bought it, how much and
//! why that much, when it will sell, and how it is doing.
//!
//! Built only from what the trade stored when it was opened (`signal_profile`,
//! `entry_context`, `entry_trigger`) plus its status, so it costs nothing and
//! never invents a reason. Shown on the Mac's Signals page and in the phone
//! snapshot, which both call `explain`.

use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Signals that carry no weight in the entry score, so they are never given as a reason.
const ZERO_WEIGHT: [&str; 2] = ["institutional", "supply_chain"];
/// A signal counts as a reason from this strength (0 to 1).
const REASON_MIN: f64 = 0.3;
const MAX_HEADLINES: usize = 2;

/// Exit rules per kind of entry. The fetcher's `ExitRules` and `max_hold_days` are
/// the source of truth; a test there checks these match.
pub struct Rules {
    pub atr_mult: f64,
    pub hard_stop_pct: f64,
    pub fixed_stop_pct: f64,
    pub take_half: bool,
    pub max_hold_days: i64,
    pub decay_exit: bool,
}

pub fn rules_for(trigger: &str) -> Rules {
    match trigger {
        "insider_director_buy" => Rules {
            atr_mult: 6.0,
            hard_stop_pct: 30.0,
            fixed_stop_pct: 20.0,
            take_half: false,
            max_hold_days: 60,
            decay_exit: false,
        },
        "news_surprise" => Rules {
            atr_mult: 3.0,
            hard_stop_pct: 15.0,
            fixed_stop_pct: 10.0,
            take_half: true,
            max_hold_days: 5,
            decay_exit: false,
        },
        "insider_cluster" => Rules {
            atr_mult: 3.0,
            hard_stop_pct: 15.0,
            fixed_stop_pct: 10.0,
            take_half: true,
            max_hold_days: 20,
            decay_exit: false,
        },
        _ => Rules {
            atr_mult: 3.0,
            hard_stop_pct: 15.0,
            fixed_stop_pct: 10.0,
            take_half: true,
            max_hold_days: 60,
            decay_exit: true,
        },
    }
}

/// What the explanation is built from: one `paper_trades` row.
#[derive(Debug, Clone, Default)]
pub struct TradeFacts {
    pub id: i64,
    pub ticker: String,
    pub name: Option<String>,
    pub direction: String,
    pub entry_price: f64,
    pub entry_date: String,
    pub position_size: f64,
    pub status: String,
    pub exit_price: Option<f64>,
    pub exit_date: Option<String>,
    pub exit_reason: Option<String>,
    pub pnl_pct: Option<f64>,
    pub signal_profile: Option<String>,
    pub entry_context: Option<String>,
    pub entry_trigger: String,
    pub stop_price: Option<f64>,
    pub scale_in_count: i64,
    /// Half of the position has already been sold at the profit target.
    pub half_closed: bool,
    /// Open trades: the live return in % (the caller has the price). Closed trades use `pnl_pct`.
    pub current_pct: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Explanation {
    /// A few words: what kind of buy this was.
    pub headline: String,
    /// Why Pulse bought it.
    pub why: String,
    /// How much, and why that much.
    pub size: Option<String>,
    /// When Pulse will sell (open trades).
    pub plan: Option<String>,
    /// How it is doing, or how it ended.
    pub now: Option<String>,
}

fn dim_label(key: &str) -> Option<&'static str> {
    Some(match key {
        "insider" => "company insiders trading its stock",
        "news" => "a rise in news coverage and attention",
        "government" => "government contracts or filings",
        "search" => "more people searching for it",
        "patent" => "new patents",
        "political" => "politicians trading it",
        _ => return None,
    })
}

fn pct(v: f64) -> String {
    format!("{:.0}%", v * 100.0)
}

fn money(v: f64) -> String {
    let n = v.abs().round() as i64;
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    format!("${out}")
}

fn price(v: f64) -> String {
    format!("${v:.2}")
}

fn parse(s: &Option<String>) -> Value {
    s.as_deref()
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or(Value::Null)
}

fn num(v: &Value, key: &str) -> Option<f64> {
    v.get(key).and_then(Value::as_f64).filter(|x| x.is_finite())
}

/// The signals that drove a convergence entry, strongest first.
fn drivers(profile: &Value) -> Vec<(&'static str, f64)> {
    let mut all: Vec<(&'static str, f64)> = [
        "insider",
        "news",
        "government",
        "search",
        "patent",
        "political",
    ]
    .iter()
    .filter(|k| !ZERO_WEIGHT.contains(k))
    .filter_map(|k| {
        num(profile, k)
            .filter(|v| *v > 0.05)
            .map(|v| (dim_label(k).unwrap_or(k), v))
    })
    .collect();
    all.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    let strong: Vec<_> = all
        .iter()
        .copied()
        .filter(|(_, v)| *v >= REASON_MIN)
        .collect();
    if strong.is_empty() {
        all.into_iter().take(2).collect()
    } else {
        strong
    }
}

fn headlines(profile: &Value) -> Vec<String> {
    profile
        .get("stories")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|s| {
                    let h = s
                        .get("headline")
                        .or_else(|| s.get("title"))
                        .and_then(Value::as_str)?;
                    // Wikipedia and filing notices read like noise; the rest are real headlines.
                    if h.contains("Wikipedia views") || h.contains("filed insider transaction") {
                        return None;
                    }
                    let h: String = h.chars().take(110).collect();
                    Some(match s.get("source").and_then(Value::as_str) {
                        Some(src) if !src.is_empty() => format!("“{h}” ({src})"),
                        _ => format!("“{h}”"),
                    })
                })
                .fold(Vec::<String>::new(), |mut seen, h| {
                    if !seen.contains(&h) {
                        seen.push(h);
                    }
                    seen
                })
                .into_iter()
                .take(MAX_HEADLINES)
                .collect()
        })
        .unwrap_or_default()
}

fn join_and(items: &[String]) -> String {
    match items {
        [] => String::new(),
        [a] => a.clone(),
        [a, b] => format!("{a} and {b}"),
        [rest @ .., last] => format!("{}, and {last}", rest.join(", ")),
    }
}

/// "Corteva, Inc.  (CTVA)" -> "Corteva, Inc.": drop the ticker suffix and extra spaces.
fn clean_name(name: Option<&str>) -> Option<String> {
    let name = name?;
    let base = name.split('(').next().unwrap_or(name);
    let out = base.split_whitespace().collect::<Vec<_>>().join(" ");
    (!out.is_empty()).then_some(out)
}

/// Shown for a position at the broker that has no trade of Pulse's behind it.
pub fn not_a_pulse_trade() -> Explanation {
    Explanation {
        headline: "Not opened by Pulse".into(),
        why: "This position is at Alpaca, but Pulse has no record of buying it, so there is no reason or exit plan on file for it.".into(),
        size: None,
        plan: None,
        now: None,
    }
}

fn is_short(f: &TradeFacts) -> bool {
    f.direction == "short"
}

fn why(f: &TradeFacts, profile: &Value, ctx: &Value) -> (String, String) {
    let who = clean_name(f.name.as_deref()).unwrap_or_else(|| f.ticker.clone());
    match profile.get("source").and_then(Value::as_str) {
        Some("manual") => {
            return (
                "Bought by you".into(),
                format!(
                    "You placed this {} yourself. Pulse did not pick it.",
                    if is_short(f) { "short sale" } else { "buy" }
                ),
            );
        }
        Some("reconciled") => {
            return (
                "Found at the broker".into(),
                "This position was already at Alpaca when Pulse checked, so it was added to the list. Pulse did not choose it, and the reasons Pulse uses are not recorded for it.".into(),
            );
        }
        _ => {}
    }
    let event = ctx.get("event").unwrap_or(&Value::Null);
    let mut text = match f.entry_trigger.as_str() {
        "insider_director_buy" => {
            let dollars = num(event, "dollars").map(money);
            let names = event.get("names").and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_str).map(String::from).collect::<Vec<_>>()).unwrap_or_default();
            let who_bought = if names.is_empty() { "An outside director".to_string() } else { format!("{}, a director", join_and(&names)) };
            format!(
                "{who_bought} of {who} recently bought {} of its shares on the open market. Directors who are not executives or big owners rarely buy with their own money unless they expect the stock to rise, so Pulse follows them and holds on for a long time.",
                dollars.map(|d| format!("about {d}")).unwrap_or_else(|| "a meaningful amount".into())
            )
        }
        "news_surprise" => {
            let h = event.get("headline").and_then(Value::as_str).map(|h| format!(" The headline behind it: “{}”.", h.chars().take(110).collect::<String>())).unwrap_or_default();
            format!(
                "The tone of today’s news about {who} was very different from its usual tone over the last 30 days. Pulse bet that the price would {} in the next few days.{h}",
                if is_short(f) { "fall" } else { "rise" }
            )
        }
        "insider_cluster" => format!("Several insiders at {who} bought around the same time, which Pulse treats as a sign they expect the stock to rise."),
        _ => {
            let d = drivers(profile);
            if d.is_empty() {
                format!("Pulse’s daily scan ranked {who} among the strongest names, but the individual signals were not saved for this older trade.")
            } else {
                let parts: Vec<String> = d.iter().map(|(l, v)| format!("{l} ({} strength)", pct(*v))).collect();
                let rank = match (num(ctx, "rank"), num(ctx, "candidates")) {
                    (Some(r), Some(c)) if c > 1.0 => format!(" It ranked #{} of {} stocks that qualified that day.", r as i64, c as i64),
                    _ => String::new(),
                };
                format!(
                    "Several separate kinds of evidence pointed at {who} at the same time: {}. When different signals agree, Pulse treats it as stronger than any one alone.{rank}",
                    join_and(&parts)
                )
            }
        }
    };
    let hl = headlines(profile);
    if !hl.is_empty() && f.entry_trigger != "news_surprise" {
        text.push_str(&format!(" Recent coverage: {}.", hl.join("; ")));
    }
    let headline = match f.entry_trigger.as_str() {
        "insider_director_buy" => "A director bought the stock",
        "news_surprise" => "News tone changed sharply",
        "insider_cluster" => "Several insiders bought",
        _ => "Several signals agreed",
    };
    (headline.to_string(), text)
}

fn size(f: &TradeFacts, ctx: &Value) -> Option<String> {
    let source = parse(&f.signal_profile).get("source").and_then(Value::as_str).map(String::from);
    if matches!(source.as_deref(), Some("manual" | "reconciled")) {
        return None;
    }
    if f.position_size <= 0.0 {
        return None;
    }
    let verb = if is_short(f) {
        "sold short"
    } else {
        "invested"
    };
    let mut s = format!("About {} {verb}", money(f.position_size));
    if let Some(q) = num(ctx, "qty").filter(|q| *q > 0.0) {
        s.push_str(&format!(
            " ({} share{})",
            q,
            if q == 1.0 { "" } else { "s" }
        ));
    }
    s.push('.');
    let sizing = ctx.get("sizing").unwrap_or(&Value::Null);
    if sizing.is_object() {
        s.push_str(" Pulse sizes each trade by how much the stock normally swings, so wilder stocks get less money");
    } else {
        s.push_str(" Pulse sizes each trade as a fixed share of the account");
    }
    let mut cuts = vec![];
    if num(sizing, "edge_mult").is_some_and(|m| m < 1.0) {
        cuts.push("it is a smaller bet because this kind of signal is less proven");
    }
    if !is_short(f) && num(ctx, "regime").is_some_and(|m| m < 1.0) {
        cuts.push(
            "it was cut in half because the S&P 500 was below its 50-day average, a weak market",
        );
    }
    if num(sizing, "dd_mult").is_some_and(|m| m < 1.0) {
        cuts.push("it was reduced because the account was in a drawdown");
    }
    if !cuts.is_empty() {
        s.push_str(&format!("; {}", cuts.join("; ")));
    }
    s.push('.');
    if f.scale_in_count > 0 {
        s.push_str(&format!(
            " Pulse added to it {} time{} as the signal stayed strong.",
            f.scale_in_count,
            if f.scale_in_count == 1 { "" } else { "s" }
        ));
    }
    Some(s)
}

fn plan(f: &TradeFacts, ctx: &Value) -> Option<String> {
    if f.status != "open" {
        return None;
    }
    let r = rules_for(&f.entry_trigger);
    let swing = num(ctx, "atr_pct").filter(|a| *a > 0.0);
    let short = is_short(f);
    let (move_word, extreme) = if short {
        ("rises", "lowest")
    } else {
        ("drops", "highest")
    };
    let mut s = String::new();
    match swing {
        Some(a) => s.push_str(&format!(
            "Pulse sells {} if the price {move_word} {:.0}% ({}× its normal daily swing of {:.1}%) from the {extreme} price since buying, or {:.0}% {} the {} price, whichever comes first.",
            if short { "(buys back)" } else { "everything" },
            a * r.atr_mult,
            r.atr_mult,
            a,
            r.hard_stop_pct,
            if short { "above" } else { "below" },
            if short { "sale" } else { "purchase" }
        )),
        None => s.push_str(&format!(
            "Pulse sells {} if the price moves {:.0}% against it from the {} price.",
            if short { "(buys back)" } else { "everything" },
            r.fixed_stop_pct,
            if short { "sale" } else { "purchase" }
        )),
    }
    if r.take_half {
        if f.half_closed {
            s.push_str(" Half of it has already been sold to lock in a gain.");
        } else {
            s.push_str(&format!(
                " It closes half the position once it is up at least 10% and the gain reaches {}× the stock’s normal daily swing.",
                r.atr_mult
            ));
        }
    }
    if r.decay_exit {
        s.push_str(
            " After 5 trading days it can also sell early if the signals behind the buy fade.",
        );
    }
    s.push_str(&format!(
        " It never holds longer than {} trading days.",
        r.max_hold_days
    ));
    if let Some(p) = f.stop_price.filter(|p| *p > 0.0) {
        s.push_str(&format!(
            " A safety order at the broker is set at {} right now.",
            price(p)
        ));
    }
    Some(s)
}

fn exit_words(reason: &str, short: bool) -> &'static str {
    if reason.starts_with("trailing_stop") && short {
        return "the price rose far enough from its low to trigger the trailing stop";
    }
    const KNOWN: [(&str, &str); 9] = [
        (
            "trailing_stop",
            "the price fell far enough from its high to trigger the trailing stop",
        ),
        ("hard_stop", "the loss limit was reached"),
        ("fixed_stop", "the loss limit was reached"),
        ("broker_stop", "the safety stop at the broker sold it"),
        ("signal_decay", "the signals behind the buy faded"),
        (
            "max_hold",
            "it reached the longest time Pulse holds this kind of trade",
        ),
        ("profit_target", "the profit target was reached"),
        (
            "closed_between_runs",
            "it was sold between Pulse’s own runs",
        ),
        ("reconcile", "it was missing at the broker"),
    ];
    KNOWN
        .iter()
        .find(|(p, _)| reason.starts_with(p))
        .map(|(_, t)| *t)
        .unwrap_or("the position was closed")
}

fn day(s: &str) -> &str {
    s.get(..10).unwrap_or(s)
}

fn now(f: &TradeFacts) -> Option<String> {
    if f.status == "open" {
        let p = f.current_pct.filter(|p| p.is_finite())?;
        let word = if p >= 0.0 { "up" } else { "down" };
        let verdict = if p >= 0.0 {
            "so the reasons are playing out so far"
        } else {
            "so it has not worked yet; the exit rules above limit the loss"
        };
        return Some(format!(
            "Bought on {} at {}. It is {word} {:.1}% since, {verdict}.",
            day(&f.entry_date),
            price(f.entry_price),
            p.abs()
        ));
    }
    let end = f
        .exit_date
        .as_deref()
        .map(day)
        .map(|d| format!(" on {d}"))
        .unwrap_or_default();
    let at = f
        .exit_price
        .map(|p| format!(" at {}", price(p)))
        .unwrap_or_default();
    let result = match f.pnl_pct.filter(|p| p.is_finite()) {
        Some(p) if p >= 0.0 => format!("It made {:.1}%.", p),
        Some(p) => format!("It lost {:.1}%.", p.abs()),
        None => String::new(),
    };
    let why = f
        .exit_reason
        .as_deref()
        .filter(|r| !r.starts_with("merged_into"))
        .map(|r| format!(" Why it ended: {}.", exit_words(r, is_short(f))))
        .unwrap_or_default();
    Some(
        format!(
            "Bought on {} at {}; closed{end}{at}. {result}{why}",
            day(&f.entry_date),
            price(f.entry_price)
        )
        .replace(" .", "."),
    )
}

pub fn explain(f: &TradeFacts) -> Explanation {
    let profile = parse(&f.signal_profile);
    let ctx = parse(&f.entry_context);
    let (headline, why) = why(f, &profile, &ctx);
    Explanation {
        headline,
        why,
        size: size(f, &ctx),
        plan: plan(f, &ctx),
        now: now(f),
    }
}

const COLUMNS: &str = "pt.id, pt.ticker, e.name, pt.direction, pt.entry_price, pt.entry_date, pt.position_size, pt.status,
    pt.exit_price, pt.exit_date, pt.exit_reason, pt.pnl_pct, pt.signal_profile, pt.entry_context, pt.entry_trigger,
    pt.broker_stop_price, COALESCE(pt.scale_in_count, 0), pt.half_closed_at IS NOT NULL";

fn row(r: &rusqlite::Row) -> rusqlite::Result<TradeFacts> {
    Ok(TradeFacts {
        id: r.get(0)?,
        ticker: r.get(1)?,
        name: r.get(2)?,
        direction: r.get(3)?,
        entry_price: r.get(4)?,
        entry_date: r.get(5)?,
        position_size: r.get(6)?,
        status: r.get(7)?,
        exit_price: r.get(8)?,
        exit_date: r.get(9)?,
        exit_reason: r.get(10)?,
        pnl_pct: r.get(11)?,
        signal_profile: r.get(12)?,
        entry_context: r.get(13)?,
        entry_trigger: r
            .get::<_, Option<String>>(14)?
            .unwrap_or_else(|| "convergence".into()),
        stop_price: r.get(15)?,
        scale_in_count: r.get(16)?,
        half_closed: r.get(17)?,
        current_pct: None,
    })
}

/// Open trades, newest first. The caller fills `current_pct` from live prices.
pub fn load_open(conn: &Connection) -> rusqlite::Result<Vec<TradeFacts>> {
    let sql = format!(
        "SELECT {COLUMNS} FROM paper_trades pt LEFT JOIN entities e ON e.id = pt.entity_id
         WHERE pt.status = 'open' ORDER BY pt.entry_date DESC, pt.id DESC"
    );
    conn.prepare(&sql)?.query_map([], row)?.collect()
}

/// The `limit` most recently closed trades. Trades merged into another (scale-ins) are skipped.
pub fn load_closed(conn: &Connection, limit: usize) -> rusqlite::Result<Vec<TradeFacts>> {
    let sql = format!(
        "SELECT {COLUMNS} FROM paper_trades pt LEFT JOIN entities e ON e.id = pt.entity_id
         WHERE pt.status != 'open' AND pt.exit_date IS NOT NULL
           AND COALESCE(pt.exit_reason, '') NOT LIKE 'merged_into%'
         ORDER BY pt.exit_date DESC, pt.id DESC LIMIT ?1"
    );
    conn.prepare(&sql)?
        .query_map([limit as i64], row)?
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts(trigger: &str, profile: &str, ctx: &str) -> TradeFacts {
        TradeFacts {
            id: 1,
            ticker: "ACME".into(),
            name: Some("Acme Corp".into()),
            direction: "long".into(),
            entry_price: 100.0,
            entry_date: "2026-10-01".into(),
            position_size: 2500.0,
            status: "open".into(),
            signal_profile: Some(profile.into()),
            entry_context: Some(ctx.into()),
            entry_trigger: trigger.into(),
            current_pct: Some(4.2),
            ..Default::default()
        }
    }

    #[test]
    fn convergence_names_the_signals_and_skips_zero_weight_ones() {
        let p = r#"{"insider":0.0,"institutional":0.9,"news":0.88,"search":0.5,"supply_chain":0.7,"government":0.1,
            "stories":[{"headline":"Acme wins big contract","source":"Reuters"},{"headline":"Acme Wikipedia views spike 40%","source":"Wikipedia"}]}"#;
        let e = explain(&facts(
            "convergence",
            p,
            r#"{"rank":2,"candidates":5,"atr_pct":4.0,"qty":25}"#,
        ));
        assert!(
            e.why
                .contains("a rise in news coverage and attention (88% strength)"),
            "{}",
            e.why
        );
        assert!(e
            .why
            .contains("more people searching for it (50% strength)"));
        assert!(
            !e.why.contains("institutional")
                && !e.why.contains("funds")
                && !e.why.contains("supplier")
        );
        assert!(!e.why.contains("government"), "weak signals are left out");
        assert!(e.why.contains("#2 of 5"));
        assert!(
            e.why.contains("“Acme wins big contract” (Reuters)") && !e.why.contains("Wikipedia")
        );
        assert_eq!(e.headline, "Several signals agreed");
    }

    #[test]
    fn older_trades_without_context_still_read_well() {
        let e = explain(&facts("convergence", r#"{"news":0.7}"#, ""));
        assert!(e.why.contains("news coverage"));
        assert!(e.size.unwrap().contains("$2,500 invested"));
        assert!(
            e.plan.unwrap().contains("moves 10% against it"),
            "no ATR means the fixed stop"
        );
    }

    #[test]
    fn nothing_saved_says_so_instead_of_inventing_a_reason() {
        let mut f = facts("convergence", "", "");
        f.signal_profile = None;
        let e = explain(&f);
        assert!(e.why.contains("not saved"));
    }

    #[test]
    fn director_buy_has_loose_exits_and_no_half_close() {
        let ctx = r#"{"atr_pct":3.0,"event":{"dollars":250000.0,"names":["Jane Roe"]},"qty":10}"#;
        let e = explain(&facts("insider_director_buy", "{}", ctx));
        assert!(
            e.why.contains("Jane Roe, a director of Acme Corp") && e.why.contains("$250,000"),
            "{}",
            e.why
        );
        let plan = e.plan.unwrap();
        assert!(
            plan.contains("18%") && plan.contains("30%") && plan.contains("60 trading days"),
            "{plan}"
        );
        assert!(!plan.contains("closes half") && !plan.contains("signals behind the buy fade"));
    }

    #[test]
    fn short_news_trade_reads_as_a_bet_on_a_fall() {
        let mut f = facts(
            "news_surprise",
            "{}",
            r#"{"atr_pct":5.0,"event":{"headline":"Acme sued"},"sizing":{"edge_mult":0.5},"regime":0.5}"#,
        );
        f.direction = "short".into();
        let e = explain(&f);
        assert!(e.why.contains("fall") && e.why.contains("Acme sued"));
        let size = e.size.unwrap();
        assert!(size.contains("sold short") && size.contains("less proven"), "{size}");
        assert!(!size.contains("S&P 500"), "a falling market does not shrink a short: {size}");
        assert!(e.plan.unwrap().contains("rises 15%"));
    }

    #[test]
    fn manual_and_imported_trades_do_not_claim_a_signal() {
        let e = explain(&facts("convergence", r#"{"source":"manual"}"#, ""));
        assert!(e.why.contains("You placed") && e.size.is_none());
        let e = explain(&facts("convergence", r#"{"source":"reconciled"}"#, ""));
        assert!(e.why.contains("already at Alpaca"));
    }

    #[test]
    fn open_trade_says_how_it_is_doing_and_closed_says_how_it_ended() {
        let e = explain(&facts("convergence", r#"{"news":0.7}"#, ""));
        assert!(e.now.unwrap().contains("up 4.2%"));
        let mut f = facts("convergence", r#"{"news":0.7}"#, "");
        f.status = "stopped_out".into();
        f.exit_date = Some("2026-10-09 15:00:00".into());
        f.exit_price = Some(90.0);
        f.pnl_pct = Some(-10.0);
        f.exit_reason = Some("trailing_stop (x)".into());
        let e = explain(&f);
        let now = e.now.unwrap();
        assert!(
            now.contains(
                "closed on 2026-10-09 at $90.00. It lost 10.0%. Why it ended: the price fell"
            ),
            "{now}"
        );
        assert!(e.plan.is_none());
    }

    #[test]
    fn names_lose_the_ticker_suffix_and_repeated_headlines_show_once() {
        assert_eq!(clean_name(Some("Corteva, Inc.  (CTVA)")).as_deref(), Some("Corteva, Inc."));
        assert_eq!(clean_name(Some("  ")), None);
        let p = r#"{"news":0.9,"stories":[{"headline":"Same","source":"X"},{"headline":"Same","source":"X"},{"headline":"Other","source":"Y"}]}"#;
        let e = explain(&facts("convergence", p, ""));
        assert!(e.why.contains("“Same” (X); “Other” (Y)"), "{}", e.why);
    }

    #[test]
    fn money_has_thousands_separators() {
        assert_eq!(money(1234567.4), "$1,234,567");
        assert_eq!(money(950.0), "$950");
    }
}
