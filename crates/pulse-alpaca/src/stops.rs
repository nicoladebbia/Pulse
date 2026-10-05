//! Broker-held stop orders for open positions, shared by the fetcher (which
//! places and raises them) and the app (whose Close button must cancel them).
//!
//! Alpaca facts these helpers are built around:
//! - Fractional orders are day-only, so a GTC stop covers whole shares only.
//! - A sell stop holds its shares: any other sell for them is refused (403
//!   "insufficient qty available") until the stop is cancelled.
//! - A cancel is asynchronous (`pending_cancel`), so callers wait for a final
//!   state before selling — and a stop can fill while the cancel is in flight.
//! - `PATCH /orders/{id}` replaces an order under a new id, but is refused
//!   while the order is `accepted` or `pending_*`; cancel + new is the fallback.

use serde_json::{Value, json};
use std::time::Duration;

use crate::{Credentials, PAPER_URL};

/// `client_order_id` prefix of every stop Pulse places. Anything else open on
/// the account is left alone.
pub const STOP_ID_PREFIX: &str = "pulse-stop-";

#[derive(Debug, Clone, PartialEq)]
pub struct StopOrder {
    pub id: String,
    pub client_order_id: String,
    pub status: String,
    pub qty: f64,
    pub stop_price: f64,
    pub filled_qty: f64,
    pub filled_avg_price: f64,
    /// `filled_at` as sent (RFC 3339, UTC), if any.
    pub filled_at: Option<String>,
}

impl StopOrder {
    /// Alpaca will not change the order any more.
    pub fn is_final(&self) -> bool {
        matches!(self.status.as_str(), "filled" | "canceled" | "expired" | "rejected" | "replaced")
    }

    /// Shares the stop actually sold. A stop cancelled after a partial fill
    /// still sold those shares.
    pub fn sold(&self) -> Option<(f64, f64)> {
        (self.filled_qty > 0.0 && self.filled_avg_price > 0.0).then_some((self.filled_qty, self.filled_avg_price))
    }
}

fn num(v: &Value, key: &str) -> f64 {
    match v.get(key) {
        Some(Value::String(s)) => s.parse().unwrap_or(0.0),
        Some(Value::Number(n)) => n.as_f64().unwrap_or(0.0),
        _ => 0.0,
    }
}

pub fn parse_order(v: &Value) -> Option<StopOrder> {
    let s = |k: &str| v.get(k).and_then(Value::as_str).map(str::to_string);
    Some(StopOrder {
        id: s("id")?,
        client_order_id: s("client_order_id").unwrap_or_default(),
        status: s("status").unwrap_or_default(),
        qty: num(v, "qty"),
        stop_price: num(v, "stop_price"),
        filled_qty: num(v, "filled_qty"),
        filled_avg_price: num(v, "filled_avg_price"),
        filled_at: s("filled_at"),
    })
}

/// A stop price Alpaca accepts, rounded DOWN so rounding never lifts the stop
/// into the market: cents at $1 and above, four decimals below.
pub fn round_stop_price(p: f64) -> f64 {
    if !p.is_finite() || p <= 0.0 {
        return 0.0;
    }
    let scale = if p >= 1.0 { 100.0 } else { 10_000.0 };
    // The epsilon keeps 12.34 (stored as 12.3399999…) from flooring to 12.33.
    ((p * scale) + 1e-6).floor() / scale
}

/// What to do with a position's broker stop.
#[derive(Debug, Clone, PartialEq)]
pub enum StopPlan {
    /// The live stop already covers the shares at a high enough price.
    Keep,
    /// No stop yet: place one.
    Place { qty: u64, stop_price: f64 },
    /// Change the live stop's size and/or raise its price.
    Replace { qty: u64, stop_price: f64 },
    /// Fewer than one whole share is held, so no stop is possible: cancel any.
    Cancel,
}

/// Decide how the live stop (if any) should change for `held_qty` shares and a
/// wanted stop of `want_price`. A stop is never lowered: if the wanted price
/// is below the live one, the live price is kept.
pub fn plan_stop(live: Option<&StopOrder>, held_qty: f64, want_price: f64) -> StopPlan {
    let qty = if held_qty.is_finite() && held_qty > 0.0 { (held_qty + 1e-9).floor() as u64 } else { 0 };
    let want = round_stop_price(want_price);
    let live = live.filter(|o| !o.is_final());
    if qty == 0 || want <= 0.0 {
        return if live.is_some() && qty == 0 { StopPlan::Cancel } else { StopPlan::Keep };
    }
    match live {
        None => StopPlan::Place { qty, stop_price: want },
        Some(o) => {
            let price = want.max(o.stop_price);
            let same_qty = (o.qty - qty as f64).abs() < 1e-9;
            if same_qty && price <= o.stop_price + 1e-9 {
                StopPlan::Keep
            } else {
                StopPlan::Replace { qty, stop_price: price }
            }
        }
    }
}

fn price_str(p: f64) -> String {
    if p >= 1.0 { format!("{p:.2}") } else { format!("{p:.4}") }
}

async fn read_order(resp: reqwest::Response, what: &str) -> Result<StopOrder, String> {
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err(format!("{what} returned {status}: {}", body.chars().take(200).collect::<String>()));
    }
    let v: Value = resp.json().await.map_err(|e| format!("{what}: bad JSON: {e}"))?;
    parse_order(&v).ok_or_else(|| format!("{what}: no order id in response"))
}

/// One order by id. `Ok(None)` when Alpaca does not know it.
pub async fn get_order(client: &reqwest::Client, creds: &Credentials, id: &str) -> Result<Option<StopOrder>, String> {
    let resp = creds
        .auth(client.get(format!("{PAPER_URL}/orders/{id}")))
        .send()
        .await
        .map_err(|e| format!("order lookup failed: {e}"))?;
    if resp.status().as_u16() == 404 {
        return Ok(None);
    }
    read_order(resp, "order lookup").await.map(Some)
}

/// Pulse's open stop orders for a symbol.
pub async fn open_stops(client: &reqwest::Client, creds: &Credentials, symbol: &str) -> Result<Vec<StopOrder>, String> {
    let resp = creds
        .auth(client.get(format!("{PAPER_URL}/orders")))
        .query(&[("status", "open"), ("symbols", symbol), ("side", "sell"), ("limit", "100")])
        .send()
        .await
        .map_err(|e| format!("open orders request failed: {e}"))?;
    let status = resp.status();
    if !status.is_success() {
        return Err(format!("open orders returned {status}"));
    }
    let v: Value = resp.json().await.map_err(|e| e.to_string())?;
    Ok(v.as_array()
        .map(|a| a.iter().filter_map(parse_order).filter(|o| o.client_order_id.starts_with(STOP_ID_PREFIX)).collect())
        .unwrap_or_default())
}

/// Place a GTC sell stop for whole shares.
pub async fn place_stop(
    client: &reqwest::Client,
    creds: &Credentials,
    symbol: &str,
    qty: u64,
    stop_price: f64,
    client_order_id: &str,
) -> Result<StopOrder, String> {
    let body = json!({
        "symbol": symbol,
        "qty": qty.to_string(),
        "side": "sell",
        "type": "stop",
        "time_in_force": "gtc",
        "stop_price": price_str(stop_price),
        "client_order_id": client_order_id,
    });
    let resp = creds
        .auth(client.post(format!("{PAPER_URL}/orders")))
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("stop order request failed: {e}"))?;
    read_order(resp, "stop order").await
}

/// Replace a live stop's size and price. Returns the NEW order (new id).
pub async fn replace_stop(
    client: &reqwest::Client,
    creds: &Credentials,
    id: &str,
    qty: u64,
    stop_price: f64,
) -> Result<StopOrder, String> {
    let body = json!({ "qty": qty.to_string(), "stop_price": price_str(stop_price) });
    let resp = creds
        .auth(client.patch(format!("{PAPER_URL}/orders/{id}")))
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("stop replace request failed: {e}"))?;
    read_order(resp, "stop replace").await
}

/// Cancel an order and wait until Alpaca reports it final. Returns the final
/// order, which may show it FILLED (the stop fired while we cancelled) — the
/// caller must book those shares as sold. An error means the order may still
/// be live and its shares still held, so the caller must not sell them.
pub async fn cancel_and_wait(client: &reqwest::Client, creds: &Credentials, id: &str) -> Result<StopOrder, String> {
    let resp = creds
        .auth(client.delete(format!("{PAPER_URL}/orders/{id}")))
        .send()
        .await
        .map_err(|e| format!("cancel request failed: {e}"))?;
    let code = resp.status().as_u16();
    // 204 accepted; 422 not cancelable (already final); 404 unknown.
    if !(resp.status().is_success() || code == 422 || code == 404) {
        return Err(format!("cancel returned {}", resp.status()));
    }
    for _ in 0..20 {
        match get_order(client, creds, id).await? {
            None => return Err(format!("order {id} not found after cancel")),
            Some(o) if o.is_final() => return Ok(o),
            Some(_) => tokio::time::sleep(Duration::from_millis(500)).await,
        }
    }
    Err(format!("order {id} still not cancelled after 10s"))
}

/// Cancel every Pulse stop on a symbol (plus `known`, if it is not among the
/// open ones) and return their final states. Shares are free to sell only
/// when this returns Ok.
pub async fn clear_stops(
    client: &reqwest::Client,
    creds: &Credentials,
    symbol: &str,
    known: Option<&str>,
) -> Result<Vec<StopOrder>, String> {
    let mut ids: Vec<String> = open_stops(client, creds, symbol).await?.into_iter().map(|o| o.id).collect();
    if let Some(k) = known.filter(|k| !k.is_empty())
        && !ids.iter().any(|i| i == k)
    {
        ids.push(k.to_string());
    }
    let mut out = Vec::with_capacity(ids.len());
    for id in ids {
        out.push(cancel_and_wait(client, creds, &id).await?);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn live(qty: f64, stop: f64) -> StopOrder {
        StopOrder {
            id: "o1".into(),
            client_order_id: "pulse-stop-X-1-1".into(),
            status: "new".into(),
            qty,
            stop_price: stop,
            filled_qty: 0.0,
            filled_avg_price: 0.0,
            filled_at: None,
        }
    }

    #[test]
    fn stop_prices_round_down_to_valid_ticks() {
        assert_eq!(round_stop_price(12.349), 12.34);
        assert_eq!(round_stop_price(12.34), 12.34);
        assert_eq!(round_stop_price(0.56789), 0.5678);
        assert_eq!(round_stop_price(f64::NAN), 0.0);
        assert_eq!(round_stop_price(-1.0), 0.0);
    }

    #[test]
    fn a_new_position_gets_a_stop_for_its_whole_shares() {
        assert_eq!(plan_stop(None, 25.7, 90.129), StopPlan::Place { qty: 25, stop_price: 90.12 });
    }

    #[test]
    fn a_stop_is_raised_but_never_lowered() {
        let o = live(25.0, 90.0);
        assert_eq!(plan_stop(Some(&o), 25.2, 89.0), StopPlan::Keep);
        assert_eq!(plan_stop(Some(&o), 25.2, 90.0), StopPlan::Keep);
        assert_eq!(plan_stop(Some(&o), 25.2, 92.5), StopPlan::Replace { qty: 25, stop_price: 92.5 });
        // A size change keeps the higher live price even when a lower one is wanted.
        assert_eq!(plan_stop(Some(&o), 12.6, 85.0), StopPlan::Replace { qty: 12, stop_price: 90.0 });
    }

    #[test]
    fn under_one_share_cancels_any_stop() {
        assert_eq!(plan_stop(Some(&live(1.0, 9.0)), 0.6, 9.5), StopPlan::Cancel);
        assert_eq!(plan_stop(None, 0.6, 9.5), StopPlan::Keep);
    }

    #[test]
    fn a_final_stop_counts_as_no_stop() {
        let mut o = live(25.0, 90.0);
        o.status = "canceled".into();
        assert_eq!(plan_stop(Some(&o), 25.0, 80.0), StopPlan::Place { qty: 25, stop_price: 80.0 });
    }

    #[test]
    fn a_stop_cancelled_after_a_partial_fill_still_sold_shares() {
        let v = json!({"id": "a", "client_order_id": "pulse-stop-A-1-2", "status": "canceled",
                       "qty": "10", "stop_price": "9.50", "filled_qty": "4", "filled_avg_price": "9.48",
                       "filled_at": "2026-10-05T14:31:00Z"});
        let o = parse_order(&v).unwrap();
        assert!(o.is_final());
        assert_eq!(o.sold(), Some((4.0, 9.48)));
        assert_eq!(o.stop_price, 9.5);
        let untouched = parse_order(&json!({"id": "b", "status": "canceled", "filled_qty": "0"})).unwrap();
        assert_eq!(untouched.sold(), None);
    }
}
