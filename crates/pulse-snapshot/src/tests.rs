use super::*;
use pulse_alpaca::DailyBar;
use serde_json::Value;

const FIXTURE: &str = "tests/fixtures/snapshot_v1.json";

fn d(s: &str) -> NaiveDate {
    s.parse().unwrap()
}

fn now() -> DateTime<Utc> {
    "2026-10-11T12:00:00Z".parse().unwrap()
}

/// Only the tables the snapshot reads, with the columns it reads.
fn setup() -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE entities (id INTEGER PRIMARY KEY, name TEXT NOT NULL);
         CREATE TABLE entity_tickers (entity_id INTEGER, ticker TEXT NOT NULL, is_public INTEGER DEFAULT 1);
         CREATE TABLE briefings (id INTEGER PRIMARY KEY, date TEXT NOT NULL, created_at TEXT NOT NULL DEFAULT '2026-10-11 05:00:00',
           status TEXT NOT NULL, briefing_type TEXT NOT NULL DEFAULT 'daily', time_label TEXT, executive_summary TEXT);
         CREATE TABLE stories (id INTEGER PRIMARY KEY, briefing_id INTEGER NOT NULL, sector TEXT NOT NULL, original_title TEXT NOT NULL,
           original_url TEXT NOT NULL DEFAULT '', source_name TEXT NOT NULL DEFAULT '', headline TEXT NOT NULL DEFAULT '',
           summary TEXT NOT NULL DEFAULT '', why_it_matters TEXT NOT NULL DEFAULT '', importance_score INTEGER NOT NULL DEFAULT 5,
           is_hero INTEGER NOT NULL DEFAULT 0, display_order INTEGER NOT NULL DEFAULT 0, source_type TEXT NOT NULL DEFAULT 'news');
         CREATE TABLE cross_signals (id INTEGER PRIMARY KEY, entity_id INTEGER, ticker TEXT, compound_score REAL NOT NULL,
           convergence_detected INTEGER DEFAULT 0, computed_at TEXT);
         CREATE TABLE paper_trades (id INTEGER PRIMARY KEY, ticker TEXT NOT NULL, direction TEXT NOT NULL DEFAULT 'long', status TEXT NOT NULL,
           entry_date TEXT, exit_date TEXT, pnl REAL, pnl_pct REAL, stop_order_id TEXT, broker_stop_price REAL);
         CREATE TABLE portfolio_snapshots (id INTEGER PRIMARY KEY, date TEXT NOT NULL UNIQUE, total_value REAL NOT NULL);
         CREATE TABLE ticker_industry (ticker TEXT PRIMARY KEY, industry TEXT, checked_at TEXT NOT NULL DEFAULT '');
         CREATE TABLE earnings_calendar (
           ticker TEXT NOT NULL, report_date TEXT NOT NULL, hour TEXT, eps_estimate REAL, revenue_estimate REAL,
           fetched_at TEXT NOT NULL DEFAULT (datetime('now')), PRIMARY KEY (ticker, report_date));

         INSERT INTO entities (id, name) VALUES (1, 'Apple Inc'), (2, 'Tesla Inc'), (3, 'Microsoft');
         INSERT INTO entity_tickers (entity_id, ticker) VALUES (1, 'AAPL'), (2, 'TSLA'), (3, 'MSFT');
         INSERT INTO ticker_industry (ticker, industry) VALUES ('AAPL', 'Technology'), ('TSLA', 'Automobiles');

         INSERT INTO briefings (id, date, status, time_label, executive_summary) VALUES
           (1, '2026-10-10', 'complete', 'Evening', 'Older summary.'),
           (2, '2026-10-11', 'complete', 'Morning', 'Chips lead; rates wait for Wednesday.'),
           (3, '2026-10-11', 'pending', NULL, NULL);
         INSERT INTO stories (id, briefing_id, sector, original_title, original_url, source_name, headline, summary, why_it_matters, importance_score, is_hero, display_order) VALUES
           (10, 2, 'ai', 'Orig A', 'https://example.com/a', 'Example Wire', 'Chipmakers rally on demand', 'Orders rose.', 'Supply is tight.', 9, 1, 0),
           (11, 2, 'tech', 'Orig B', 'https://example.com/b', 'Example Daily', 'Rates stay put', 'No change expected.', '', 6, 0, 1),
           (12, 3, 'ai', 'Half done', 'https://example.com/c', 'X', 'Half done', 'Retry in flight.', '', 9, 1, 0);

         INSERT INTO cross_signals (entity_id, ticker, compound_score, convergence_detected, computed_at) VALUES
           (1, NULL, 0.62, 1, '2026-10-11 07:00:00'), (1, NULL, 0.41, 0, '2026-10-11 06:00:00'),
           (3, NULL, 0.35, 0, '2026-10-11 07:00:00'), (2, NULL, 0.9, 1, '2026-10-09 07:00:00');

         INSERT INTO paper_trades (id, ticker, direction, status, entry_date, exit_date, pnl, pnl_pct, stop_order_id, broker_stop_price) VALUES
           (1, 'AAPL', 'long', 'open', '2026-10-01', NULL, NULL, NULL, 'ord-1', 190.0),
           (2, 'TSLA', 'short', 'open', '2026-10-02', NULL, NULL, NULL, 'ord-2', 270.0),
           (3, 'NVDA', 'long', 'closed', '2026-09-01', '2026-09-20', 420.5, 8.4, NULL, NULL),
           (4, 'AMD', 'long', 'stopped_out', '2026-09-05', '2026-09-25', -150.0, -3.0, NULL, NULL);

         INSERT INTO portfolio_snapshots (date, total_value) VALUES
           ('2026-10-06', 99000.0), ('2026-10-07', 101000.0), ('2026-10-08', 100500.0), ('2026-10-09', 99800.0);

         INSERT INTO earnings_calendar (ticker, report_date, hour, eps_estimate, revenue_estimate, fetched_at) VALUES
           ('AAPL', '2026-10-22', 'amc', 1.62, 94000000000.0, '2026-10-11 05:00:00'),
           ('MSFT', '2026-10-20', 'amc', 3.1, 65000000000.0, '2026-10-11 05:00:00'),
           ('TSLA', '2026-12-30', NULL, NULL, NULL, '2026-10-11 05:00:00');",
    )
    .unwrap();
    conn
}

/// 40 calendar days of closes whose daily moves are `scale` times the market's.
fn bars(scale: f64) -> Vec<DailyBar> {
    let start = d("2026-08-31");
    let mut close = 100.0;
    (0..40)
        .map(|i| {
            close *= 1.0 + scale * if i % 2 == 0 { 0.01 } else { -0.005 };
            DailyBar {
                date: (start + chrono::Days::new(i)).to_string(),
                open: close,
                high: close,
                low: close,
                close,
                volume: 1_000_000.0,
            }
        })
        .collect()
}

fn live() -> LiveAccount {
    LiveAccount {
        equity: 100_300.0,
        cash: 98_000.0,
        last_equity: Some(99_800.0),
        positions: vec![
            HeldPosition { symbol: "AAPL".into(), qty: 10.0, price: 205.0, market_value: 2050.0, unrealized_pl: 50.0 },
            HeldPosition { symbol: "TSLA".into(), qty: -5.0, price: 240.0, market_value: -1200.0, unrealized_pl: 100.0 },
        ],
        bars: HashMap::from([("SPY".to_string(), bars(1.0)), ("AAPL".to_string(), bars(1.0)), ("TSLA".to_string(), bars(2.0))]),
        notes: vec![],
    }
}

/// Equal JSON, with numbers compared to a tolerance so the fixture does not
/// depend on the last bit of a float sum.
fn same(a: &Value, b: &Value, path: &str) -> Result<(), String> {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => {
            let (x, y) = (x.as_f64().unwrap(), y.as_f64().unwrap());
            if (x - y).abs() <= 1e-9 * x.abs().max(y.abs()).max(1.0) {
                Ok(())
            } else {
                Err(format!("{path}: {x} != {y}"))
            }
        }
        (Value::Array(x), Value::Array(y)) => {
            if x.len() != y.len() {
                return Err(format!("{path}: {} items != {} items", x.len(), y.len()));
            }
            x.iter().zip(y).enumerate().try_for_each(|(i, (x, y))| same(x, y, &format!("{path}[{i}]")))
        }
        (Value::Object(x), Value::Object(y)) => {
            let keys: HashSet<&String> = x.keys().chain(y.keys()).collect();
            keys.into_iter().try_for_each(|k| match (x.get(k), y.get(k)) {
                (Some(x), Some(y)) => same(x, y, &format!("{path}.{k}")),
                _ => Err(format!("{path}.{k}: present on one side only")),
            })
        }
        _ if a == b => Ok(()),
        _ => Err(format!("{path}: {a} != {b}")),
    }
}

fn fixture_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(FIXTURE)
}

#[test]
fn build_matches_the_fixture_the_phone_decodes() {
    let snap = build(&setup(), d("2026-10-11"), now(), Some(&live())).unwrap();
    let got = serde_json::to_value(&snap).unwrap();
    // `UPDATE_FIXTURE=1 cargo test -p pulse-snapshot` rewrites the fixture after a deliberate contract change.
    if std::env::var_os("UPDATE_FIXTURE").is_some() {
        std::fs::write(fixture_path(), serde_json::to_string_pretty(&got).unwrap() + "\n").unwrap();
    }
    let want: Value = serde_json::from_str(&std::fs::read_to_string(fixture_path()).unwrap()).unwrap();
    if let Err(e) = same(&got, &want, "$") {
        panic!("snapshot differs from {FIXTURE} (UPDATE_FIXTURE=1 rewrites it, then update the Swift side too): {e}");
    }
}

#[test]
fn fixture_round_trips_through_the_structs() {
    // A field the structs drop, or an Option the fixture leaves out, would change the JSON.
    let want: Value = serde_json::from_str(&std::fs::read_to_string(fixture_path()).unwrap()).unwrap();
    let snap: Snapshot = serde_json::from_value(want.clone()).unwrap();
    same(&serde_json::to_value(&snap).unwrap(), &want, "$").unwrap();
    assert_eq!(snap.version, SCHEMA_VERSION);
}

#[test]
fn fixture_fills_every_optional_the_phone_reads() {
    let snap: Snapshot = serde_json::from_str(&std::fs::read_to_string(fixture_path()).unwrap()).unwrap();
    let a = snap.account.as_ref().unwrap();
    assert!(a.day_change.is_some() && a.day_change_pct.is_some());
    assert!(snap.positions.iter().all(|p| p.stop_price.is_some() && p.industry.is_some() && p.unrealized_pl_pct.is_some()));
    let r = snap.risk.as_ref().unwrap();
    assert!(r.beta_exposure_pct.is_some() && r.drawdown.peak_date.is_some() && r.positions.is_empty());
    let b = snap.briefing.as_ref().unwrap();
    assert!(b.time_label.is_some() && b.executive_summary.is_some());
    assert!(b.stories.iter().any(|s| s.why_it_matters.is_some() && s.source_name.is_some() && s.url.is_some()));
    assert!(snap.signals.iter().all(|s| s.name.is_some()));
    assert!(snap.recent_trades.iter().all(|t| t.pnl.is_some() && t.pnl_pct.is_some()));
    assert!(snap.calendar.earnings.iter().any(|e| e.hour.is_some() && e.eps_estimate.is_some() && e.revenue_estimate.is_some() && e.signal_score.is_some()));
    assert!(snap.calendar.earnings_updated_at.is_some());
}

#[test]
fn without_alpaca_the_database_parts_still_show() {
    let snap = build(&setup(), d("2026-10-11"), now(), None).unwrap();
    assert!(snap.account.is_none() && snap.risk.is_none() && snap.positions.is_empty());
    assert!(snap.briefing.is_some());
    assert!(!snap.signals.is_empty());
    assert!(snap.signals.iter().all(|s| !s.held), "nothing is known to be held without the broker");
    assert!(snap.calendar.has_earnings_data);
}

#[test]
fn account_and_positions_come_from_the_broker_with_stops_from_the_database() {
    let snap = build(&setup(), d("2026-10-11"), now(), Some(&live())).unwrap();
    let a = snap.account.unwrap();
    assert_eq!((a.equity, a.cash, a.open_pl), (100_300.0, 98_000.0, 150.0));
    assert_eq!(a.day_change, Some(500.0));
    assert!((a.day_change_pct.unwrap() - 500.0 / 99_800.0 * 100.0).abs() < 1e-9);

    let aapl = snap.positions.iter().find(|p| p.symbol == "AAPL").unwrap();
    assert_eq!((aapl.side.as_str(), aapl.stop_price, aapl.industry.as_deref()), ("long", Some(190.0), Some("Technology")));
    // Cost 2000, up 50.
    assert!((aapl.unrealized_pl_pct.unwrap() - 2.5).abs() < 1e-9);

    let tsla = snap.positions.iter().find(|p| p.symbol == "TSLA").unwrap();
    assert_eq!((tsla.side.as_str(), tsla.stop_price), ("short", Some(270.0)));
    // Short value 1200 plus the 100 gained is the 1300 shorted.
    assert!((tsla.unrealized_pl_pct.unwrap() - 100.0 / 1300.0 * 100.0).abs() < 1e-9);
}

#[test]
fn risk_leaves_the_position_list_to_the_positions_field() {
    let snap = build(&setup(), d("2026-10-11"), now(), Some(&live())).unwrap();
    let r = snap.risk.unwrap();
    assert!(r.positions.is_empty());
    assert_eq!(r.position_count, 2);
    assert!(r.beta_exposure_pct.is_some());
    // Today's live equity is part of the drawdown history.
    assert_eq!(r.drawdown.points.last().map(|p| p.equity), Some(100_300.0));
}

#[test]
fn notes_from_reading_the_account_show_with_the_risk_warnings() {
    let mut l = live();
    l.notes.push("Left out XYZ with unreadable data from Alpaca".into());
    let r = build(&setup(), d("2026-10-11"), now(), Some(&l)).unwrap().risk.unwrap();
    assert!(r.warnings.iter().any(|w| w.contains("XYZ")));
}

#[test]
fn missing_previous_equity_leaves_the_day_change_out() {
    let mut l = live();
    l.last_equity = None;
    let a = build(&setup(), d("2026-10-11"), now(), Some(&l)).unwrap().account.unwrap();
    assert!(a.day_change.is_none() && a.day_change_pct.is_none());
}

#[test]
fn briefing_is_the_newest_complete_one_not_a_retry_in_flight() {
    let b = load_briefing(&setup()).unwrap().unwrap();
    assert_eq!((b.date.as_str(), b.time_label.as_deref()), ("2026-10-11", Some("Morning")));
    assert_eq!(b.stories.len(), 2, "the pending briefing's story is not shown");
    assert!(b.stories[0].is_hero, "hero first");
    assert_eq!(b.stories[0].headline, "Chipmakers rally on demand");
    assert_eq!(b.stories[1].why_it_matters, None, "an empty field is absent, not an empty string");
}

#[test]
fn long_text_is_clipped_for_the_phone() {
    let conn = setup();
    conn.execute("UPDATE stories SET summary = ?1 WHERE id = 10", params!["word ".repeat(400)]).unwrap();
    let story = &load_briefing(&conn).unwrap().unwrap().stories[0];
    assert!(story.summary.chars().count() <= SUMMARY_CHARS + 1 && story.summary.ends_with('…'));
}

#[test]
fn no_briefing_is_none() {
    let conn = setup();
    conn.execute("DELETE FROM briefings", []).unwrap();
    assert!(load_briefing(&conn).unwrap().is_none());
}

#[test]
fn signals_are_the_latest_day_one_row_per_ticker_best_score_first() {
    let held = HashSet::from(["AAPL".to_string()]);
    let sigs = load_signals(&setup(), &held).unwrap();
    let got: Vec<(&str, f64, bool, bool)> = sigs.iter().map(|s| (s.ticker.as_str(), s.score, s.convergence, s.held)).collect();
    // TSLA's 0.9 is from an older day; AAPL's two rows collapse to the best.
    assert_eq!(got, vec![("AAPL", 0.62, true, true), ("MSFT", 0.35, false, false)]);
    assert_eq!(sigs[0].name.as_deref(), Some("Apple Inc"));
}

#[test]
fn recent_trades_are_closed_ones_newest_exit_first() {
    let t = load_recent_trades(&setup()).unwrap();
    assert_eq!(t.iter().map(|t| t.ticker.as_str()).collect::<Vec<_>>(), vec!["AMD", "NVDA"]);
    assert_eq!(t[1].pnl, Some(420.5));
}

#[test]
fn earnings_on_the_phone_are_held_names_or_strong_signals_within_two_weeks() {
    let conn = setup();
    conn.execute_batch(
        "INSERT INTO earnings_calendar (ticker, report_date) VALUES ('NVDA', '2026-10-20');
         INSERT INTO earnings_calendar (ticker, report_date) VALUES ('AMD', '2026-10-21');
         INSERT INTO cross_signals (ticker, compound_score, computed_at) VALUES ('AMD', 0.05, '2026-10-11 07:00:00');",
    )
    .unwrap();
    let tickers = |conn: &Connection| -> Vec<String> {
        build(conn, d("2026-10-11"), now(), Some(&live())).unwrap().calendar.earnings.into_iter().map(|e| e.ticker).collect()
    };
    let got = tickers(&conn);
    assert!(got.contains(&"AAPL".to_string()), "held");
    assert!(got.contains(&"MSFT".to_string()), "signal 0.35 through its entity");
    assert!(!got.contains(&"NVDA".to_string()) && !got.contains(&"AMD".to_string()), "neither held nor a strong signal");
    assert!(!got.contains(&"TSLA".to_string()), "held, but reports in 80 days");

    // Past the phone's two weeks they drop off.
    conn.execute("UPDATE earnings_calendar SET report_date = '2026-10-26' WHERE ticker IN ('AAPL', 'MSFT')", []).unwrap();
    assert!(tickers(&conn).is_empty());
}

#[test]
fn non_finite_numbers_never_reach_the_file() {
    let mut l = live();
    l.equity = f64::INFINITY;
    l.last_equity = Some(f64::NAN);
    l.positions[0].price = f64::NAN;
    l.positions[0].market_value = f64::NAN;
    l.positions[0].unrealized_pl = f64::NAN;
    l.positions[1].unrealized_pl = f64::INFINITY;
    l.bars.get_mut("AAPL").unwrap()[3].close = f64::NAN;
    let conn = setup();
    conn.execute("UPDATE paper_trades SET pnl = 1e999 WHERE id = 3", []).unwrap();
    let snap = build(&conn, d("2026-10-11"), now(), Some(&l)).unwrap();

    // What the phone does: decode every required number.
    let json = serde_json::to_string(&snap).unwrap();
    let back: Snapshot = serde_json::from_str(&json).unwrap_or_else(|e| panic!("phone could not decode: {e}\n{json}"));
    let a = back.account.unwrap();
    assert!(a.equity.is_finite() && a.open_pl.is_finite());
    assert!(back.positions.iter().all(|p| p.price.is_finite() && p.market_value.is_finite() && p.unrealized_pl.is_finite()));
}

#[test]
fn sanitize_turns_required_non_finite_numbers_into_zero_and_optional_ones_into_absent() {
    let mut snap = build(&setup(), d("2026-10-11"), now(), Some(&live())).unwrap();
    snap.account.as_mut().unwrap().equity = f64::NAN;
    snap.account.as_mut().unwrap().day_change = Some(f64::INFINITY);
    snap.risk.as_mut().unwrap().beta_exposure_pct = Some(f64::NAN);
    snap.risk.as_mut().unwrap().drawdown.max_pct = f64::NEG_INFINITY;
    snap.signals[0].score = f64::NAN;
    snap.sanitize();
    assert_eq!(snap.account.as_ref().unwrap().equity, 0.0);
    assert_eq!(snap.account.as_ref().unwrap().day_change, None);
    assert_eq!(snap.risk.as_ref().unwrap().beta_exposure_pct, None);
    assert_eq!(snap.risk.as_ref().unwrap().drawdown.max_pct, 0.0);
    assert_eq!(snap.signals[0].score, 0.0);
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("pulse-snapshot-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

#[test]
fn write_atomic_creates_the_folder_replaces_the_file_and_leaves_no_temp() {
    let dir = scratch("write").join("nested");
    let mut snap = build(&setup(), d("2026-10-11"), now(), None).unwrap();
    assert_eq!(age_seconds(&dir), None);

    let path = write_atomic(&dir, &snap).unwrap();
    assert_eq!(path, dir.join(FILE_NAME));
    assert!(age_seconds(&dir).unwrap() < 60);

    snap.generated_at = "2026-10-11T13:00:00Z".into();
    write_atomic(&dir, &snap).unwrap();
    let back: Snapshot = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(back.generated_at, "2026-10-11T13:00:00Z");
    let files: Vec<String> = std::fs::read_dir(&dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
    assert_eq!(files, vec![FILE_NAME.to_string()]);
    let _ = std::fs::remove_dir_all(dir.parent().unwrap());
}

#[test]
fn output_dir_follows_the_switch_the_override_then_icloud_drive() {
    let os = |s: &str| Some(std::ffi::OsString::from(s));
    assert_eq!(resolve_dir(Some("off"), os("/x"), os("/home/me")), None);
    assert_eq!(resolve_dir(Some(" False "), None, os("/home/me")), None);
    assert_eq!(resolve_dir(Some("on"), os("/x"), os("/home/me")), Some(PathBuf::from("/x")));
    assert_eq!(
        resolve_dir(None, os(""), os("/home/me")),
        Some(PathBuf::from("/home/me/Library/Mobile Documents/com~apple~CloudDocs/Pulse"))
    );
    assert_eq!(resolve_dir(None, None, None), None);
}
