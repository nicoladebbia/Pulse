use crate::db::DbState;
use crate::services::paper_trading;
use crate::services::risk::{self, HeldPosition, RiskInputs, RiskReport};
use std::collections::HashMap;
use tauri::State;

/// Days of daily bars used to measure each holding's market sensitivity.
const BETA_LOOKBACK_DAYS: i64 = 150;

fn num(s: &str) -> f64 {
    s.parse::<f64>().ok().filter(|v| v.is_finite()).unwrap_or(0.0)
}

fn parse(s: &str) -> Option<f64> {
    s.parse::<f64>().ok().filter(|v| v.is_finite())
}

/// Where the account stands on risk: exposure, concentration, money at stake if
/// every stop fills, sensitivity to the S&P 500 and drawdown. Beta is
/// best-effort: if the price history can't be fetched the rest still shows.
#[tauri::command]
pub async fn get_risk_report(db: State<'_, DbState>) -> Result<RiskReport, String> {
    let (stops, industries, history) = {
        let conn = db.0.lock().map_err(|e| e.to_string())?;
        (
            risk::load_stops(&conn).map_err(|e| e.to_string())?,
            risk::load_industries(&conn).map_err(|e| e.to_string())?,
            crate::services::benchmark::load_equity(&conn).map_err(|e| e.to_string())?,
        )
    };

    let account = paper_trading::get_account().await.map_err(|e| e.to_string())?;
    let mut skipped: Vec<String> = Vec::new();
    let held: Vec<HeldPosition> = paper_trading::get_positions()
        .await
        .map_err(|e| e.to_string())?
        .into_iter()
        .filter_map(|p| match (parse(&p.qty), parse(&p.market_value), parse(&p.current_price)) {
            (Some(qty), Some(market_value), Some(price)) => Some(HeldPosition {
                symbol: p.symbol,
                qty,
                price,
                market_value,
                unrealized_pl: num(&p.unrealized_pl),
            }),
            _ => {
                skipped.push(p.symbol);
                None
            }
        })
        .collect();

    let (bars, bars_error) = match fetch_bars(&held).await {
        Ok(b) => (b, None),
        Err(e) => (HashMap::new(), Some(e)),
    };
    let equity = num(&account.equity);
    let today = chrono::Local::now().format("%Y-%m-%d").to_string();
    let history = risk::with_live_equity(&history, &today, equity);
    let mut report = risk::build_report(&RiskInputs {
        equity,
        cash: num(&account.cash),
        positions: &held,
        stops: &stops,
        industries: &industries,
        bars: &bars,
        equity_history: &history,
    });
    if !skipped.is_empty() {
        report.warnings.push(format!("Left out {} with unreadable data from Alpaca", skipped.join(", ")));
    }
    if let Some(e) = bars_error {
        report.warnings.push(format!("Price history unavailable, so market sensitivity is assumed: {e}"));
    } else if let Ok(conn) = db.0.lock() {
        crate::services::api_usage::log_usage(&conn, "alpaca", "data_api", "risk_bars", 1, 0).ok();
    }
    Ok(report)
}

async fn fetch_bars(held: &[HeldPosition]) -> Result<HashMap<String, Vec<pulse_alpaca::DailyBar>>, String> {
    let creds = pulse_alpaca::credentials().ok_or("Alpaca keys are not set")?;
    let client = pulse_alpaca::client(std::time::Duration::from_secs(15))?;
    let mut symbols: Vec<String> = held.iter().map(|p| p.symbol.clone()).collect();
    symbols.push("SPY".to_string());
    let start = (chrono::Utc::now() - chrono::Duration::days(BETA_LOOKBACK_DAYS)).format("%Y-%m-%d").to_string();
    pulse_alpaca::daily_bars_multi(&client, &creds, &symbols, &start).await
}
