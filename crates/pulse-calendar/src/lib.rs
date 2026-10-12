//! Dates that move markets: Fed meetings and the BLS inflation, producer-price
//! and jobs releases, plus the earnings blackout rule shared by the trader and
//! the calendar page.
//!
//! The macro dates are a hand-kept table. Both agencies publish a year or more
//! ahead, but bls.gov refuses automated requests, so the dates are copied from
//! federalreserve.gov/monetarypolicy/fomccalendars.htm and
//! bls.gov/schedule/news_release/{cpi,ppi,empsit}.htm. A test fails when the
//! table runs within 90 days of its end, which is the reminder to extend it.

pub mod view;

use chrono::{Datelike, NaiveDate, Weekday};
use serde::{Deserialize, Serialize};

/// No buys this many trading days before an earnings report.
pub const EARNINGS_BLACKOUT_DAYS: u32 = 3;

/// `today` plus `n` weekdays (holidays are ignored: erring a day long).
pub fn add_trading_days(today: NaiveDate, n: u32) -> NaiveDate {
    let mut d = today;
    let mut left = n;
    while left > 0 {
        d = d.succ_opt().unwrap_or(d);
        if !matches!(d.weekday(), Weekday::Sat | Weekday::Sun) {
            left -= 1;
        }
    }
    d
}

/// Whether a report on `report_date` falls from today through the blackout window.
pub fn in_earnings_blackout(report_date: NaiveDate, today: NaiveDate) -> bool {
    report_date >= today && report_date <= add_trading_days(today, EARNINGS_BLACKOUT_DAYS)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MacroKind {
    Fomc,
    Cpi,
    Ppi,
    Jobs,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MacroEvent {
    /// `YYYY-MM-DD`
    pub date: String,
    pub kind: MacroKind,
    pub title: String,
    /// Release time, US Eastern.
    pub time_et: String,
    /// Extra context: FOMC meeting span and whether projections are published.
    pub note: Option<String>,
}

/// (first day, decision day, publishes projections)
const FOMC: &[(&str, &str, bool)] = &[
    ("2026-10-27", "2026-10-28", false),
    ("2026-12-08", "2026-12-09", true),
    ("2027-01-26", "2027-01-27", false),
    ("2027-03-16", "2027-03-17", true),
    ("2027-04-27", "2027-04-28", false),
    ("2027-06-08", "2027-06-09", true),
    ("2027-07-27", "2027-07-28", false),
    ("2027-09-14", "2027-09-15", true),
    ("2027-10-26", "2027-10-27", false),
    ("2027-12-07", "2027-12-08", true),
];

const CPI: &[&str] = &[
    "2026-10-14", "2026-11-10", "2026-12-10", "2027-01-13", "2027-02-11", "2027-03-10", "2027-04-13",
    "2027-05-12", "2027-06-10", "2027-07-14", "2027-08-11", "2027-09-14", "2027-10-14", "2027-11-10",
    "2027-12-10",
];

const PPI: &[&str] = &[
    "2026-10-15", "2026-11-13", "2026-12-15", "2027-01-14", "2027-02-12", "2027-03-12", "2027-04-14",
    "2027-05-13", "2027-06-11", "2027-07-15", "2027-08-10", "2027-09-10", "2027-10-15", "2027-11-12",
    "2027-12-15",
];

const JOBS: &[&str] = &[
    "2026-11-06", "2026-12-04", "2027-01-08", "2027-02-05", "2027-03-05", "2027-04-02", "2027-05-07",
    "2027-06-04", "2027-07-02", "2027-08-06", "2027-09-03", "2027-10-08", "2027-11-05", "2027-12-03",
];

fn day(label: &str, m: MacroKind, title: &str, time_et: &str, note: Option<String>) -> MacroEvent {
    MacroEvent { date: label.to_string(), kind: m, title: title.to_string(), time_et: time_et.to_string(), note }
}

/// Every macro event, oldest first.
pub fn all_macro_events() -> Vec<MacroEvent> {
    let mut out = Vec::new();
    for (start, decision, sep) in FOMC {
        let span = format!("Two-day meeting starting {start}");
        let note = if *sep { format!("{span}; new projections") } else { span };
        out.push(day(decision, MacroKind::Fomc, "Fed interest-rate decision", "2:00 PM", Some(note)));
    }
    out.extend(CPI.iter().map(|d| day(d, MacroKind::Cpi, "Consumer prices (CPI)", "8:30 AM", None)));
    out.extend(PPI.iter().map(|d| day(d, MacroKind::Ppi, "Producer prices (PPI)", "8:30 AM", None)));
    out.extend(JOBS.iter().map(|d| day(d, MacroKind::Jobs, "Jobs report", "8:30 AM", None)));
    out.sort_by(|a, b| a.date.cmp(&b.date).then_with(|| a.title.cmp(&b.title)));
    out
}

/// Events from `from` through `to`, both inclusive.
pub fn macro_events(from: NaiveDate, to: NaiveDate) -> Vec<MacroEvent> {
    let (from, to) = (from.to_string(), to.to_string());
    all_macro_events().into_iter().filter(|e| e.date >= from && e.date <= to).collect()
}

/// The last date the table knows about.
pub fn last_covered_date() -> Option<NaiveDate> {
    all_macro_events().last().and_then(|e| e.date.parse().ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(s: &str) -> NaiveDate {
        s.parse().unwrap()
    }

    #[test]
    fn every_date_parses_and_falls_on_a_weekday() {
        for e in all_macro_events() {
            let date = e.date.parse::<NaiveDate>().unwrap_or_else(|_| panic!("bad date {}", e.date));
            assert!(!matches!(date.weekday(), Weekday::Sat | Weekday::Sun), "{} {} is a weekend", e.title, e.date);
        }
        for (start, decision, _) in FOMC {
            assert!(d(start) < d(decision), "FOMC {start} does not precede {decision}");
            assert_eq!(d(decision).weekday(), Weekday::Wed, "FOMC decisions land on Wednesdays");
        }
    }

    #[test]
    fn no_event_is_listed_twice() {
        let events = all_macro_events();
        for w in events.windows(2) {
            assert!(!(w[0].date == w[1].date && w[0].kind == w[1].kind), "duplicate {} {}", w[0].title, w[0].date);
        }
    }

    #[test]
    fn range_filter_is_inclusive_and_sorted() {
        let got = macro_events(d("2026-10-14"), d("2026-10-15"));
        assert_eq!(got.iter().map(|e| e.kind).collect::<Vec<_>>(), vec![MacroKind::Cpi, MacroKind::Ppi]);
        assert!(macro_events(d("2030-01-01"), d("2030-12-31")).is_empty());
    }

    #[test]
    fn blackout_runs_today_through_three_trading_days() {
        let today = d("2026-10-07"); // Wednesday
        assert!(in_earnings_blackout(d("2026-10-07"), today), "reports today");
        assert!(in_earnings_blackout(d("2026-10-12"), today), "Monday is 3 trading days out");
        assert!(!in_earnings_blackout(d("2026-10-13"), today));
        assert!(!in_earnings_blackout(d("2026-10-06"), today), "already reported");
    }

    #[test]
    fn the_table_does_not_run_out_within_90_days() {
        // Fails when it is time to add the next year of dates (see the module docs).
        let last = last_covered_date().expect("table is empty");
        let horizon = chrono::Local::now().date_naive() + chrono::Duration::days(90);
        assert!(last >= horizon, "macro calendar ends {last}; extend it from the Fed and BLS schedules");
    }
}
