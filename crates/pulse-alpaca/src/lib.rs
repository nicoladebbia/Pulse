//! Alpaca market data shared by the app and the fetcher.
//!
//! Trading already goes to Alpaca's paper account (`ALPACA_API_KEY` /
//! `ALPACA_SECRET_KEY`); this crate gives both binaries the same free IEX
//! prices from those keys, so Pulse needs no Finnhub key for quotes or the
//! live stream.

pub mod stops;

use serde_json::Value;
use std::collections::HashMap;
use std::time::Duration;

pub const PAPER_URL: &str = "https://paper-api.alpaca.markets/v2";
pub const DATA_URL: &str = "https://data.alpaca.markets/v2";
/// Free plan: IEX trades only, one stream connection per account.
pub const STREAM_URL: &str = "wss://stream.data.alpaca.markets/v2/iex";

/// Symbols per snapshot request; keeps the query string well under URL limits.
const SNAPSHOT_CHUNK: usize = 100;

#[derive(Debug, Clone)]
pub struct Credentials {
    pub key: String,
    pub secret: String,
}

/// Both keys, trimmed, or None if either is missing or blank.
pub fn credentials() -> Option<Credentials> {
    let get = |v: &str| std::env::var(v).ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
    Some(Credentials { key: get("ALPACA_API_KEY")?, secret: get("ALPACA_SECRET_KEY")? })
}

impl Credentials {
    pub fn auth(&self, req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        req.header("APCA-API-KEY-ID", &self.key).header("APCA-API-SECRET-KEY", &self.secret)
    }
}

/// One symbol's latest price and current (or last) session bar.
#[derive(Debug, Clone, PartialEq)]
pub struct Snapshot {
    /// Latest trade, falling back to the session bar's close.
    pub price: f64,
    pub open: Option<f64>,
    pub high: Option<f64>,
    pub low: Option<f64>,
    pub prev_close: Option<f64>,
    pub volume: Option<f64>,
    /// When `price` traded, if known.
    pub trade_time: Option<chrono::DateTime<chrono::Utc>>,
    /// The session the OHLC bar belongs to (`YYYY-MM-DD`, New York date).
    pub bar_date: Option<String>,
}

impl Snapshot {
    /// Percent change against the previous session's close.
    pub fn change_1d(&self) -> Option<f64> {
        self.prev_close.filter(|pc| *pc > 0.0).map(|pc| (self.price - pc) / pc * 100.0)
    }

    /// True when the last trade is older than `max_age` (a halted or delisted
    /// name whose old price must not be stored as today's).
    pub fn is_stale(&self, now: chrono::DateTime<chrono::Utc>, max_age: chrono::Duration) -> bool {
        self.trade_time.is_some_and(|t| now - t > max_age)
    }
}

impl Snapshot {
    /// The bar's open/high/low, but only if the bar is from session `today`.
    /// Before the open (and on weekends) the bar is the previous session's;
    /// stored under today's date it would double-count that range in ATR.
    pub fn ohlc_for(&self, today: &str) -> (Option<f64>, Option<f64>, Option<f64>) {
        if self.bar_date.as_deref() == Some(today) {
            (self.open, self.high, self.low)
        } else {
            (None, None, None)
        }
    }
}

/// Last trades older than this are not stored as today's price.
pub fn max_quote_age() -> chrono::Duration {
    chrono::Duration::days(5)
}

fn text<'a>(v: &'a Value, obj: &str, field: &str) -> Option<&'a str> {
    v.get(obj)?.get(field)?.as_str()
}

fn num(v: &Value, bar: &str, field: &str) -> Option<f64> {
    v.get(bar)?.get(field)?.as_f64().filter(|x| x.is_finite())
}

/// Parse a `/stocks/snapshots` response: `{ "AAPL": { latestTrade, dailyBar, prevDailyBar, .. }, .. }`.
/// Unknown symbols come back null or missing and are skipped, as are zero prices.
pub fn parse_snapshots(body: &Value) -> HashMap<String, Snapshot> {
    // Older responses nest the map under "snapshots".
    let map = body.get("snapshots").unwrap_or(body);
    let Some(obj) = map.as_object() else { return HashMap::new() };
    obj.iter()
        .filter_map(|(sym, s)| {
            let price = num(s, "latestTrade", "p").or_else(|| num(s, "dailyBar", "c")).filter(|p| *p > 0.0)?;
            Some((
                sym.clone(),
                Snapshot {
                    price,
                    open: num(s, "dailyBar", "o"),
                    high: num(s, "dailyBar", "h"),
                    low: num(s, "dailyBar", "l"),
                    prev_close: num(s, "prevDailyBar", "c"),
                    volume: num(s, "dailyBar", "v"),
                    trade_time: text(s, "latestTrade", "t")
                        .and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok())
                        .map(|t| t.with_timezone(&chrono::Utc)),
                    // Daily bars are stamped at New York midnight (04:00Z or
                    // 05:00Z), so the UTC date prefix is the session date.
                    bar_date: text(s, "dailyBar", "t").map(|t| t.chars().take(10).collect()),
                },
            ))
        })
        .collect()
}

pub fn client(timeout: Duration) -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .timeout(timeout)
        .connect_timeout(Duration::from_secs(10))
        .build()
        .map_err(|e| e.to_string())
}

/// Latest prices for many symbols, a hundred per request. A failed chunk is
/// skipped so one bad batch doesn't lose the rest; only when every chunk fails
/// is it an error.
pub async fn snapshots(
    client: &reqwest::Client,
    creds: &Credentials,
    symbols: &[String],
) -> Result<HashMap<String, Snapshot>, String> {
    let mut out = HashMap::new();
    let mut first_error = None;
    for chunk in symbols.chunks(SNAPSHOT_CHUNK) {
        match snapshot_chunk(client, creds, chunk).await {
            Ok(map) => out.extend(map),
            Err(e) => {
                first_error.get_or_insert(e);
            }
        }
    }
    match first_error {
        Some(e) if out.is_empty() => Err(e),
        _ => Ok(out),
    }
}

async fn snapshot_chunk(
    client: &reqwest::Client,
    creds: &Credentials,
    chunk: &[String],
) -> Result<HashMap<String, Snapshot>, String> {
    let url = format!("{DATA_URL}/stocks/snapshots");
    let resp = creds
        .auth(client.get(&url))
        .query(&[("symbols", chunk.join(",")), ("feed", "iex".to_string())])
        .send()
        .await
        .map_err(|e| format!("Alpaca snapshots request failed: {e}"))?;
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err(format!("Alpaca snapshots returned {status}: {}", body.chars().take(200).collect::<String>()));
    }
    let body: Value = resp.json().await.map_err(|e| format!("Alpaca snapshots: bad JSON: {e}"))?;
    Ok(parse_snapshots(&body))
}

/// Latest price for one symbol.
pub async fn latest_price(client: &reqwest::Client, creds: &Credentials, symbol: &str) -> Result<Option<f64>, String> {
    Ok(snapshots(client, creds, &[symbol.to_string()]).await?.get(symbol).map(|s| s.price))
}

/// One daily bar from the consolidated (SIP) feed.
#[derive(Debug, Clone, PartialEq)]
pub struct DailyBar {
    /// `YYYY-MM-DD`
    pub date: String,
    pub close: f64,
    pub volume: f64,
}

pub fn parse_bars(body: &Value) -> Vec<DailyBar> {
    body.get("bars")
        .and_then(Value::as_array)
        .map(|bars| {
            bars.iter()
                .filter_map(|b| {
                    let close = b.get("c").and_then(Value::as_f64).filter(|c| *c > 0.0)?;
                    Some(DailyBar {
                        date: b.get("t").and_then(Value::as_str)?.get(..10)?.to_string(),
                        close,
                        volume: b.get("v").and_then(Value::as_f64).unwrap_or(0.0),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Daily bars since `start` (`YYYY-MM-DD`), oldest first.
///
/// From the consolidated SIP feed, not IEX: IEX carries a few percent of the
/// volume, so liquidity read from it would be off by ~30x. The free plan
/// serves SIP only up to 15 minutes ago, hence the `end`.
pub async fn daily_bars(
    client: &reqwest::Client,
    creds: &Credentials,
    symbol: &str,
    start: &str,
) -> Result<Vec<DailyBar>, String> {
    let end = (chrono::Utc::now() - chrono::Duration::minutes(16)).format("%Y-%m-%dT%H:%M:%SZ").to_string();
    let resp = creds
        .auth(client.get(format!("{DATA_URL}/stocks/{symbol}/bars")))
        .query(&[
            ("timeframe", "1Day"),
            ("feed", "sip"),
            ("adjustment", "split"),
            ("limit", "1000"),
            ("start", start),
            ("end", end.as_str()),
        ])
        .send()
        .await
        .map_err(|e| format!("Alpaca bars request failed: {e}"))?;
    let status = resp.status();
    if !status.is_success() {
        return Err(format!("Alpaca bars returned {status}"));
    }
    let body: Value = resp.json().await.map_err(|e| format!("Alpaca bars: bad JSON: {e}"))?;
    Ok(parse_bars(&body))
}

/// Whether the US market is open right now (`/v2/clock`).
pub async fn market_open(client: &reqwest::Client, creds: &Credentials) -> Result<bool, String> {
    let resp = creds
        .auth(client.get(format!("{PAPER_URL}/clock")))
        .send()
        .await
        .map_err(|e| format!("Alpaca clock request failed: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("Alpaca clock returned {}", resp.status()));
    }
    let body: Value = resp.json().await.map_err(|e| e.to_string())?;
    Ok(body.get("is_open").and_then(Value::as_bool).unwrap_or(false))
}

// ---------------------------------------------------------------------------
// Streaming (wss://stream.data.alpaca.markets/v2/iex)
// ---------------------------------------------------------------------------

pub fn stream_auth_message(creds: &Credentials) -> String {
    serde_json::json!({ "action": "auth", "key": creds.key, "secret": creds.secret }).to_string()
}

pub fn stream_subscribe_message(symbols: &[String], subscribe: bool) -> String {
    let action = if subscribe { "subscribe" } else { "unsubscribe" };
    serde_json::json!({ "action": action, "trades": symbols }).to_string()
}

#[derive(Debug, Clone, PartialEq)]
pub struct StreamTrade {
    pub symbol: String,
    pub price: f64,
    pub size: f64,
    /// Milliseconds since the epoch.
    pub timestamp_ms: i64,
}

#[derive(Debug, Default, PartialEq)]
pub struct StreamBatch {
    pub trades: Vec<StreamTrade>,
    pub authenticated: bool,
    /// Server-reported error, e.g. "auth failed" or "connection limit exceeded".
    pub error: Option<String>,
}

/// Parse one stream frame. Alpaca sends JSON arrays of messages tagged by `T`:
/// `t` trade, `success`, `error`, `subscription`.
pub fn parse_stream(text: &str) -> StreamBatch {
    let mut batch = StreamBatch::default();
    let Ok(Value::Array(msgs)) = serde_json::from_str::<Value>(text) else { return batch };
    for m in &msgs {
        match m.get("T").and_then(Value::as_str) {
            Some("t") => {
                let (Some(symbol), Some(price)) = (m.get("S").and_then(Value::as_str), m.get("p").and_then(Value::as_f64))
                else {
                    continue;
                };
                let timestamp_ms = m
                    .get("t")
                    .and_then(Value::as_str)
                    .and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok())
                    .map(|t| t.timestamp_millis())
                    .unwrap_or_else(|| chrono::Utc::now().timestamp_millis());
                batch.trades.push(StreamTrade {
                    symbol: symbol.to_string(),
                    price,
                    size: m.get("s").and_then(Value::as_f64).unwrap_or(0.0),
                    timestamp_ms,
                });
            }
            Some("success") if m.get("msg").and_then(Value::as_str) == Some("authenticated") => {
                batch.authenticated = true;
            }
            Some("error") => {
                let msg = m.get("msg").and_then(Value::as_str).unwrap_or("unknown error");
                let code = m.get("code").and_then(Value::as_i64).map(|c| format!(" ({c})")).unwrap_or_default();
                batch.error = Some(format!("{msg}{code}"));
            }
            _ => {}
        }
    }
    batch
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn snapshots_use_latest_trade_and_session_bar() {
        let body = json!({
            "AAPL": {
                "latestTrade": {"p": 231.5, "t": "2026-10-02T19:59:59.5Z"},
                "dailyBar": {"t": "2026-10-02T04:00:00Z", "o": 228.0, "h": 232.0, "l": 227.5, "c": 231.0, "v": 1200.0},
                "prevDailyBar": {"c": 225.0}
            },
            "NOPE": null,
            "ZERO": {"latestTrade": {"p": 0.0}},
            "BARONLY": {"dailyBar": {"c": 10.0}}
        });
        let s = parse_snapshots(&body);
        assert_eq!(s.len(), 2);
        let a = &s["AAPL"];
        assert_eq!(a.price, 231.5);
        assert_eq!(a.high, Some(232.0));
        assert!((a.change_1d().unwrap() - 2.888_888).abs() < 1e-3);
        assert_eq!(s["BARONLY"].price, 10.0);
        assert_eq!(s["BARONLY"].change_1d(), None);
        assert_eq!(a.bar_date.as_deref(), Some("2026-10-02"));

        let now = chrono::DateTime::parse_from_rfc3339("2026-10-05T15:00:00Z").unwrap().with_timezone(&chrono::Utc);
        assert!(!a.is_stale(now, chrono::Duration::days(5)));
        assert!(a.is_stale(now, chrono::Duration::days(2)));
        assert!(!s["BARONLY"].is_stale(now, chrono::Duration::days(1)), "unknown time is not stale");
        assert_eq!(a.ohlc_for("2026-10-02"), (Some(228.0), Some(232.0), Some(227.5)));
        assert_eq!(a.ohlc_for("2026-10-05"), (None, None, None));
    }

    #[test]
    fn snapshots_accept_nested_shape() {
        let body = json!({"snapshots": {"MSFT": {"latestTrade": {"p": 400.0}}}});
        assert_eq!(parse_snapshots(&body)["MSFT"].price, 400.0);
        assert!(parse_snapshots(&json!([])).is_empty());
    }

    #[test]
    fn stream_parses_trades_auth_and_errors() {
        let b = parse_stream(r#"[{"T":"success","msg":"authenticated"}]"#);
        assert!(b.authenticated);

        let b = parse_stream(
            r#"[{"T":"t","S":"NVDA","p":120.25,"s":100,"t":"2026-10-02T14:30:00.123Z"},{"T":"q","S":"NVDA"}]"#,
        );
        assert_eq!(b.trades.len(), 1);
        assert_eq!(b.trades[0].symbol, "NVDA");
        assert_eq!(b.trades[0].size, 100.0);
        assert_eq!(b.trades[0].timestamp_ms, 1_790_951_400_123);

        let b = parse_stream(r#"[{"T":"error","code":402,"msg":"auth failed"}]"#);
        assert_eq!(b.error.as_deref(), Some("auth failed (402)"));

        assert_eq!(parse_stream("not json"), StreamBatch::default());
    }

    #[test]
    fn messages_have_alpaca_shape() {
        let c = Credentials { key: "k".into(), secret: "s".into() };
        let auth: Value = serde_json::from_str(&stream_auth_message(&c)).unwrap();
        assert_eq!(auth, json!({"action": "auth", "key": "k", "secret": "s"}));
        let sub: Value = serde_json::from_str(&stream_subscribe_message(&["A".into()], false)).unwrap();
        assert_eq!(sub, json!({"action": "unsubscribe", "trades": ["A"]}));
    }

    #[test]
    fn bars_keep_date_close_and_volume_and_drop_bad_rows() {
        let body = json!({"bars": [
            {"t": "2026-09-25T04:00:00Z", "c": 771.35, "v": 36735822.0},
            {"t": "2026-09-28T04:00:00Z", "c": 0.0, "v": 1.0},
            {"c": 5.0, "v": 1.0}
        ]});
        assert_eq!(parse_bars(&body), vec![DailyBar { date: "2026-09-25".into(), close: 771.35, volume: 36735822.0 }]);
        assert!(parse_bars(&json!({"bars": null})).is_empty());
    }
}
