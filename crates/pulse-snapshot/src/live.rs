//! What the paper account holds right now, read from Alpaca for the snapshot.

use crate::LiveAccount;
use crate::risk::HeldPosition;
use serde_json::Value;
use std::collections::HashMap;
use std::time::Duration;

/// Days of daily bars used to measure each holding's market sensitivity.
const BETA_LOOKBACK_DAYS: i64 = 150;

/// Alpaca sends numbers as strings.
fn num(v: Option<&Value>) -> Option<f64> {
    match v? {
        Value::String(s) => s.trim().parse::<f64>().ok(),
        Value::Number(n) => n.as_f64(),
        _ => None,
    }
    .filter(|x| x.is_finite())
}

/// (equity, cash, equity at the previous close)
pub fn parse_account(body: &Value) -> Result<(f64, f64, Option<f64>), String> {
    let equity = num(body.get("equity")).ok_or("Alpaca account has no readable equity")?;
    let cash = num(body.get("cash")).ok_or("Alpaca account has no readable cash")?;
    Ok((equity, cash, num(body.get("last_equity"))))
}

/// The positions that could be read, and the symbols that could not.
pub fn parse_positions(body: &Value) -> (Vec<HeldPosition>, Vec<String>) {
    let mut held = Vec::new();
    let mut skipped = Vec::new();
    for p in body.as_array().map(Vec::as_slice).unwrap_or_default() {
        let Some(symbol) = p.get("symbol").and_then(Value::as_str) else { continue };
        match (num(p.get("qty")), num(p.get("market_value")), num(p.get("current_price"))) {
            (Some(qty), Some(market_value), Some(price)) => held.push(HeldPosition {
                symbol: symbol.to_string(),
                qty,
                price,
                market_value,
                unrealized_pl: num(p.get("unrealized_pl")).unwrap_or(0.0),
            }),
            _ => skipped.push(symbol.to_string()),
        }
    }
    (held, skipped)
}

async fn get(client: &reqwest::Client, creds: &pulse_alpaca::Credentials, path: &str) -> Result<Value, String> {
    let resp = creds
        .auth(client.get(format!("{}/{path}", pulse_alpaca::PAPER_URL)))
        .send()
        .await
        .map_err(|e| format!("Alpaca {path} request failed: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("Alpaca {path} returned {}", resp.status()));
    }
    resp.json().await.map_err(|e| format!("Alpaca {path} was not JSON: {e}"))
}

/// Account, positions and price history. Without price history the snapshot
/// still has everything but market sensitivity, so that part is best-effort.
pub async fn fetch() -> Result<LiveAccount, String> {
    let creds = pulse_alpaca::credentials().ok_or("Alpaca keys are not set")?;
    let client = pulse_alpaca::client(Duration::from_secs(15))?;
    let (equity, cash, last_equity) = parse_account(&get(&client, &creds, "account").await?)?;
    let (positions, skipped) = parse_positions(&get(&client, &creds, "positions").await?);

    let mut notes = Vec::new();
    if !skipped.is_empty() {
        notes.push(format!("Left out {} with unreadable data from Alpaca", skipped.join(", ")));
    }
    let mut symbols: Vec<String> = positions.iter().map(|p| p.symbol.clone()).collect();
    symbols.push("SPY".to_string());
    let start = (chrono::Utc::now() - chrono::Duration::days(BETA_LOOKBACK_DAYS)).format("%Y-%m-%d").to_string();
    let bars = match pulse_alpaca::daily_bars_multi(&client, &creds, &symbols, &start).await {
        Ok(b) => b,
        Err(e) => {
            notes.push(format!("Price history unavailable, so market sensitivity is assumed: {e}"));
            HashMap::new()
        }
    };
    Ok(LiveAccount { equity, cash, last_equity, positions, bars, notes })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn account_numbers_arrive_as_strings() {
        let (e, c, l) = parse_account(&json!({"equity": "100300.55", "cash": "98000", "last_equity": "99800"})).unwrap();
        assert_eq!((e, c, l), (100_300.55, 98_000.0, Some(99_800.0)));
    }

    #[test]
    fn account_without_equity_or_cash_is_an_error_and_last_equity_is_optional() {
        assert!(parse_account(&json!({"cash": "1"})).is_err());
        assert!(parse_account(&json!({"equity": "1"})).is_err());
        assert!(parse_account(&json!({"equity": "NaN", "cash": "1"})).is_err());
        assert_eq!(parse_account(&json!({"equity": 5, "cash": 1})).unwrap(), (5.0, 1.0, None));
    }

    #[test]
    fn positions_keep_the_sign_of_a_short_and_skip_unreadable_rows() {
        let body = json!([
            {"symbol": "AAPL", "qty": "10", "market_value": "2050", "current_price": "205", "unrealized_pl": "50"},
            {"symbol": "TSLA", "qty": "-5", "market_value": "-1200", "current_price": "240", "unrealized_pl": "100"},
            {"symbol": "BAD", "qty": "x", "market_value": "1", "current_price": "1"},
            {"symbol": "NOPRICE", "qty": "1", "market_value": "1"},
            {"qty": "1"}
        ]);
        let (held, skipped) = parse_positions(&body);
        assert_eq!(held.iter().map(|p| (p.symbol.as_str(), p.qty, p.market_value)).collect::<Vec<_>>(), vec![("AAPL", 10.0, 2050.0), ("TSLA", -5.0, -1200.0)]);
        assert_eq!(skipped, vec!["BAD", "NOPRICE"]);
    }

    #[test]
    fn a_non_array_body_is_no_positions() {
        assert!(parse_positions(&json!({"message": "forbidden"})).0.is_empty());
    }
}
