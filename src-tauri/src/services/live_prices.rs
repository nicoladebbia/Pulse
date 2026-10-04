use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{Mutex, watch};
use tauri::{AppHandle, Emitter};

/// WebSocket streaming for real-time price updates: Alpaca's IEX trade stream
/// when its keys are set, else Finnhub (free tier: 1 connection, ~50 symbols,
/// separate from the 60 calls/min REST limit).
const WS_URL: &str = "wss://ws.finnhub.io/";

#[derive(Clone)]
enum Feed {
    Alpaca(pulse_alpaca::Credentials),
    Finnhub,
}

type WsMessage = tokio_tungstenite::tungstenite::Message;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct PriceUpdate {
    pub symbol: String,
    pub price: f64,
    pub volume: f64,
    pub timestamp: i64,
}

#[derive(Debug, Deserialize)]
struct FinnhubWsMessage {
    #[serde(rename = "type")]
    msg_type: String,
    data: Option<Vec<FinnhubTrade>>,
}

#[derive(Debug, Deserialize)]
struct FinnhubTrade {
    s: String,  // symbol
    p: f64,     // price
    v: f64,     // volume
    t: i64,     // timestamp (ms)
}

#[derive(Debug, Clone, Serialize)]
pub struct StreamStatus {
    pub connected: bool,
    pub symbols: Vec<String>,
    pub last_update: Option<String>,
}

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

pub struct LivePriceState {
    shutdown_tx: Mutex<Option<watch::Sender<bool>>>,
    status: Mutex<StreamStatus>,
}

impl Default for LivePriceState {
    fn default() -> Self {
        Self::new()
    }
}

impl LivePriceState {
    pub fn new() -> Self {
        Self {
            shutdown_tx: Mutex::new(None),
            status: Mutex::new(StreamStatus {
                connected: false,
                symbols: Vec::new(),
                last_update: None,
            }),
        }
    }
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Start streaming prices for the given symbols.
pub async fn start_stream(
    app: AppHandle,
    state: Arc<LivePriceState>,
    symbols: Vec<String>,
) -> Result<(), String> {
    // Stop existing stream if running
    stop_stream(state.clone()).await?;

    if symbols.is_empty() {
        return Ok(());
    }

    let (feed, url) = match pulse_alpaca::credentials() {
        Some(creds) => (Feed::Alpaca(creds), pulse_alpaca::STREAM_URL.to_string()),
        None => {
            let api_key = std::env::var("FINNHUB_API_KEY")
                .map_err(|_| "No price stream: set the Alpaca keys (or FINNHUB_API_KEY) in .env".to_string())?;
            (Feed::Finnhub, format!("{}?token={}", WS_URL, api_key))
        }
    };

    // Create shutdown channel
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    {
        let mut tx = state.shutdown_tx.lock().await;
        *tx = Some(shutdown_tx);
    }

    // Update status
    {
        let mut status = state.status.lock().await;
        status.symbols = symbols.clone();
    }

    let state_clone = state.clone();

    // Spawn the WebSocket task
    tokio::spawn(async move {
        if let Err(e) = run_ws_loop(app, state_clone, &feed, &url, symbols, shutdown_rx).await {
            eprintln!("WebSocket stream error: {}", e);
        }
    });

    Ok(())
}

/// Stop the current price stream.
pub async fn stop_stream(state: Arc<LivePriceState>) -> Result<(), String> {
    let mut tx = state.shutdown_tx.lock().await;
    if let Some(sender) = tx.take() {
        let _ = sender.send(true);
    }
    let mut status = state.status.lock().await;
    status.connected = false;
    status.symbols.clear();
    Ok(())
}

/// Get current stream status.
pub async fn get_status(state: Arc<LivePriceState>) -> StreamStatus {
    state.status.lock().await.clone()
}

// ---------------------------------------------------------------------------
// WebSocket loop with auto-reconnect
// ---------------------------------------------------------------------------

async fn run_ws_loop(
    app: AppHandle,
    state: Arc<LivePriceState>,
    feed: &Feed,
    url: &str,
    symbols: Vec<String>,
    mut shutdown_rx: watch::Receiver<bool>,
) -> Result<(), String> {
    let mut retry_count = 0;
    let max_retries = 5;

    loop {
        if *shutdown_rx.borrow() {
            break;
        }

        match connect_and_stream(&app, &state, feed, url, &symbols, &mut shutdown_rx).await {
            Ok(()) => break, // Clean shutdown
            Err(e) => {
                retry_count += 1;
                if retry_count > max_retries {
                    eprintln!("WebSocket max retries reached: {}", e);
                    let mut status = state.status.lock().await;
                    status.connected = false;
                    break;
                }
                eprintln!("WebSocket disconnected (attempt {}): {}. Reconnecting...", retry_count, e);
                let delay = std::time::Duration::from_secs(2u64.pow(retry_count.min(4)));
                tokio::time::sleep(delay).await;
            }
        }
    }

    Ok(())
}

async fn connect_and_stream(
    app: &AppHandle,
    state: &Arc<LivePriceState>,
    feed: &Feed,
    url: &str,
    symbols: &[String],
    shutdown_rx: &mut watch::Receiver<bool>,
) -> Result<(), String> {
    let (ws_stream, _) = tokio_tungstenite::connect_async(url)
        .await
        .map_err(|e| format!("WebSocket connect failed: {}", e))?;

    let (mut write, mut read) = ws_stream.split();

    // Subscribe to symbols
    match feed {
        Feed::Alpaca(creds) => {
            // Alpaca: authenticate first, then one subscribe message for all symbols.
            write.send(WsMessage::Text(pulse_alpaca::stream_auth_message(creds).into()))
                .await
                .map_err(|e| format!("Auth send failed: {}", e))?;
            let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
            loop {
                let msg = tokio::time::timeout_at(deadline, read.next())
                    .await
                    .map_err(|_| "Alpaca stream: no auth reply".to_string())?;
                match msg {
                    Some(Ok(WsMessage::Text(text))) => {
                        let batch = pulse_alpaca::parse_stream(&text);
                        if let Some(err) = batch.error {
                            return Err(format!("Alpaca stream: {}", err));
                        }
                        if batch.authenticated {
                            break;
                        }
                    }
                    Some(Ok(_)) => {}
                    Some(Err(e)) => return Err(format!("WebSocket error: {}", e)),
                    None => return Err("WebSocket stream ended during auth".into()),
                }
            }
            write.send(WsMessage::Text(pulse_alpaca::stream_subscribe_message(symbols, true).into()))
                .await
                .map_err(|e| format!("Subscribe failed: {}", e))?;
        }
        Feed::Finnhub => {
            for symbol in symbols {
                let msg = serde_json::json!({"type": "subscribe", "symbol": symbol});
                write.send(WsMessage::Text(msg.to_string().into()))
                    .await
                    .map_err(|e| format!("Subscribe failed: {}", e))?;
            }
        }
    }

    // Mark connected
    {
        let mut status = state.status.lock().await;
        status.connected = true;
    }
    let _ = app.emit("stream-status", true);

    // Throttle: batch updates per symbol, emit at most every 2 seconds
    let mut latest_prices: HashMap<String, PriceUpdate> = HashMap::new();
    let mut last_emit = std::time::Instant::now();

    loop {
        tokio::select! {
            _ = shutdown_rx.changed() => {
                if *shutdown_rx.borrow() {
                    // Unsubscribe
                    match feed {
                        Feed::Alpaca(_) => {
                            let msg = pulse_alpaca::stream_subscribe_message(symbols, false);
                            let _ = write.send(WsMessage::Text(msg.into())).await;
                        }
                        Feed::Finnhub => {
                            for symbol in symbols {
                                let msg = serde_json::json!({"type": "unsubscribe", "symbol": symbol});
                                let _ = write.send(WsMessage::Text(msg.to_string().into())).await;
                            }
                        }
                    }
                    let _ = write.send(tokio_tungstenite::tungstenite::Message::Close(None)).await;
                    let mut status = state.status.lock().await;
                    status.connected = false;
                    let _ = app.emit("stream-status", false);
                    return Ok(());
                }
            }
            msg = read.next() => {
                match msg {
                    Some(Ok(tokio_tungstenite::tungstenite::Message::Text(text))) => {
                        if let Feed::Alpaca(_) = feed {
                            let batch = pulse_alpaca::parse_stream(&text);
                            if let Some(err) = batch.error {
                                return Err(format!("Alpaca stream: {}", err));
                            }
                            for t in batch.trades {
                                latest_prices.insert(t.symbol.clone(), PriceUpdate {
                                    symbol: t.symbol,
                                    price: t.price,
                                    volume: t.size,
                                    timestamp: t.timestamp_ms,
                                });
                            }
                        } else if let Ok(parsed) = serde_json::from_str::<FinnhubWsMessage>(&text)
                            && parsed.msg_type == "trade"
                                && let Some(trades) = parsed.data {
                                    for trade in trades {
                                        latest_prices.insert(trade.s.clone(), PriceUpdate {
                                            symbol: trade.s,
                                            price: trade.p,
                                            volume: trade.v,
                                            timestamp: trade.t,
                                        });
                                    }
                                }

                        // Emit batched updates every 2 seconds
                        if last_emit.elapsed() >= std::time::Duration::from_secs(2) && !latest_prices.is_empty() {
                            let updates: Vec<PriceUpdate> = latest_prices.drain().map(|(_, v)| v).collect();
                            let _ = app.emit("price-updates", &updates);

                            let mut status = state.status.lock().await;
                            status.last_update = Some(chrono::Local::now().format("%H:%M:%S").to_string());

                            last_emit = std::time::Instant::now();
                        }
                    }
                    Some(Ok(tokio_tungstenite::tungstenite::Message::Ping(data))) => {
                        let _ = write.send(tokio_tungstenite::tungstenite::Message::Pong(data)).await;
                    }
                    Some(Ok(tokio_tungstenite::tungstenite::Message::Close(_))) => {
                        let mut status = state.status.lock().await;
                        status.connected = false;
                        let _ = app.emit("stream-status", false);
                        return Err("Server closed connection".into());
                    }
                    Some(Err(e)) => {
                        let mut status = state.status.lock().await;
                        status.connected = false;
                        let _ = app.emit("stream-status", false);
                        return Err(format!("WebSocket error: {}", e));
                    }
                    None => {
                        let mut status = state.status.lock().await;
                        status.connected = false;
                        let _ = app.emit("stream-status", false);
                        return Err("WebSocket stream ended".into());
                    }
                    _ => {} // Binary, Frame — ignore
                }
            }
        }
    }
}
