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
    let held: Vec<HeldPosition> = paper_trading::get_positions()
        .await
        .map_err(|e| e.to_string())?
        .into_iter()
        .map(|p| HeldPosition {
            symbol: p.symbol,
            qty: num(&p.qty),
            price: num(&p.current_price),
            market_value: num(&p.market_value),
            unrealized_pl: num(&p.unrealized_pl),
        })
        .collect();

    let bars = fetch_bars(&held).await.unwrap_or_default();
    let equity = num(&account.equity);
    let report = risk::build_report(&RiskInputs {
        equity,
        cash: num(&account.cash),
        positions: &held,
        stops: &stops,
        industries: &industries,
        bars: &bars,
        equity_history: &history,
    });
    if let Ok(conn) = db.0.lock() {
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
