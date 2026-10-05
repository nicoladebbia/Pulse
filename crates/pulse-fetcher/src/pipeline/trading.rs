use super::*;

/// What Alpaca actually filled, as opposed to what we asked for.
pub(crate) struct Fill {
    /// `filled_avg_price`. Can be 0.0 if Alpaca reports the order filled but
    /// omits the price — callers must supply their own estimate, never treat
    /// 0.0 as free.
    pub price: f64,
    pub qty: f64,
    /// `filled_at`, converted to local time.
    pub at_local: String,
}

/// Poll an Alpaca order for up to five seconds and return its fill.
///
/// `None` means the order is live but not yet filled — the normal pre-market
/// case, not an error. A caller that treats `None` as executed will invent a
/// position, and one that treats it as failed will lose a real one; both the
/// entry and exit paths depend on the distinction.
///
/// This existed in three near-identical copies (entry, scale-in, exit). They
/// are one function now because a poll that drifts between the buy and sell
/// sides silently desyncs the ledger from the broker.
pub(crate) async fn poll_fill(
    client: &reqwest::Client,
    alpaca_key: &str,
    alpaca_secret: &str,
    order_id: &str,
) -> Option<Fill> {
    for _ in 0..5 {
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        let Ok(resp) = client
            .get(format!("https://paper-api.alpaca.markets/v2/orders/{}", order_id))
            .header("APCA-API-KEY-ID", alpaca_key)
            .header("APCA-API-SECRET-KEY", alpaca_secret)
            .send()
            .await
        else {
            continue;
        };
        let Ok(order) = resp.json::<serde_json::Value>().await else {
            continue;
        };
        if order.get("status").and_then(|v| v.as_str()) != Some("filled") {
            continue;
        }
        return Some(Fill {
            price: order_num(&order, "filled_avg_price"),
            qty: order_num(&order, "filled_qty"),
            at_local: order_filled_at_local(&order),
        });
    }
    None
}

/// A numeric field of an Alpaca order. Alpaca sends numbers as strings.
fn order_num(order: &serde_json::Value, key: &str) -> f64 {
    order
        .get(key)
        .and_then(|v| v.as_str())
        .and_then(|s| s.parse::<f64>().ok())
        .unwrap_or(0.0)
}

/// `filled_at` in local time, or now when Alpaca omits it.
fn order_filled_at_local(order: &serde_json::Value) -> String {
    utc_to_local(order.get("filled_at").and_then(|v| v.as_str()))
}

/// An Alpaca RFC 3339 timestamp in local time, or now when missing.
pub(crate) fn utc_to_local(ts: Option<&str>) -> String {
    ts.and_then(|ft| chrono::DateTime::parse_from_rfc3339(ft).ok())
        .map(|dt| {
            dt.with_timezone(&chrono::Local)
                .format("%Y-%m-%dT%H:%M:%S")
                .to_string()
        })
        .unwrap_or_else(|| chrono::Local::now().format("%Y-%m-%dT%H:%M:%S").to_string())
}

/// What a pending entry order's current Alpaca state means for its row.
#[derive(Debug, PartialEq)]
pub(crate) enum EntrySettlement {
    /// Bought. `price` can be 0.0 when Alpaca omits it; the row then keeps
    /// its estimate.
    Filled { price: f64, qty: f64, at_local: String },
    /// Accepted but not filled yet — the normal state for an order placed
    /// overnight. Leave the row alone and look again next run.
    Pending,
    /// The order ended without buying anything.
    Dead { status: String },
}

/// Classify an entry order from `GET /v2/orders/{id}`.
///
/// A terminal order that filled part of its quantity before it was cancelled
/// or expired still bought shares, so it counts as filled for that part.
pub(crate) fn classify_entry_order(order: &serde_json::Value) -> EntrySettlement {
    let status = order.get("status").and_then(|v| v.as_str()).unwrap_or("");
    let qty = order_num(order, "filled_qty");
    let filled = EntrySettlement::Filled {
        price: order_num(order, "filled_avg_price"),
        qty,
        at_local: order_filled_at_local(order),
    };
    match status {
        "filled" => filled,
        "canceled" | "expired" | "rejected" | "done_for_day" | "replaced" | "stopped" => {
            if qty > 0.0 {
                filled
            } else {
                EntrySettlement::Dead { status: status.to_string() }
            }
        }
        _ => EntrySettlement::Pending,
    }
}

/// Write a settlement onto a pending row. Only touches rows still pending, so
/// running it twice is harmless.
///
/// A fill replaces the estimated entry with the real one: price, time, share
/// count, and a `position_size` of what was actually spent. A dead order
/// becomes an 'expired' row with no P&L, which every analytics query already
/// excludes.
pub(crate) fn apply_entry_settlement(
    conn: &rusqlite::Connection,
    trade_id: i64,
    settlement: &EntrySettlement,
    today: &str,
) -> rusqlite::Result<usize> {
    match settlement {
        EntrySettlement::Filled { price, qty, at_local } => conn.execute(
            "UPDATE paper_trades SET order_status = 'filled', filled_qty = ?1, entry_date = ?2,
                 entry_price     = CASE WHEN ?3 > 0 THEN ?3 ELSE entry_price END,
                 high_water_mark = CASE WHEN ?3 > 0 THEN ?3 ELSE high_water_mark END,
                 position_size   = CASE WHEN ?3 > 0 AND ?1 > 0 THEN ?3 * ?1 ELSE position_size END
             WHERE id = ?4 AND order_status = 'pending'",
            rusqlite::params![qty, at_local, price, trade_id],
        ),
        EntrySettlement::Dead { status } => conn.execute(
            "UPDATE paper_trades SET status = 'expired', exit_date = ?1, exit_reason = ?2
             WHERE id = ?3 AND order_status = 'pending'",
            rusqlite::params![today, format!("entry_order_{status}"), trade_id],
        ),
        EntrySettlement::Pending => Ok(0),
    }
}

/// Close a trade in full and return the P&L booked for the shares sold now.
///
/// The final `pnl` is `realized_pnl` (booked by an earlier half close) plus the
/// shares sold now. The full close used to write only the remaining half, and
/// the half's gain was lost.
pub(crate) fn record_full_close(
    conn: &rusqlite::Connection,
    trade_id: i64,
    entry_price: f64,
    exit_price: f64,
    qty_sold: f64,
    exit_at: &str,
    reason: &str,
) -> rusqlite::Result<f64> {
    let pnl_pct = ((exit_price - entry_price) / entry_price) * 100.0;
    let pnl_now = (exit_price - entry_price) * qty_sold;
    conn.execute(
        "UPDATE paper_trades SET status = ?7, exit_price = ?1, exit_date = ?2,
             pnl = COALESCE(realized_pnl, 0) + ?3, realized_pnl = COALESCE(realized_pnl, 0) + ?3,
             pnl_pct = ?4, exit_reason = ?5, stop_order_id = NULL
         WHERE id = ?6",
        rusqlite::params![exit_price, exit_at, pnl_now, pnl_pct, reason, trade_id, exit_status(reason)],
    )?;
    Ok(pnl_now)
}

/// The row status for a close: a stop of any kind is 'stopped_out', anything
/// else (signal decay, a reconcile) is 'closed'. Every close used to be
/// written 'closed', so stop-outs could not be told apart in the ledger.
pub(crate) fn exit_status(reason: &str) -> &'static str {
    const STOPS: [&str; 4] = ["hard_stop", "fixed_stop", "trailing_stop", "broker_stop"];
    if STOPS.iter().any(|s| reason.starts_with(s)) { "stopped_out" } else { "closed" }
}

/// Book shares the broker stop sold while some of the position is still held
/// (the fractional part a whole-share stop cannot cover, or shares added after
/// the stop was sized). Like a half close, the gain goes to `realized_pnl` and
/// `position_size` shrinks to what is left; the close of the rest follows.
pub(crate) fn book_stop_sale(
    conn: &rusqlite::Connection,
    trade_id: i64,
    entry_price: f64,
    stop_price: f64,
    qty_sold: f64,
    qty_left: f64,
    filled_at: &str,
) -> rusqlite::Result<f64> {
    let pnl_now = (stop_price - entry_price) * qty_sold;
    let left_fraction = if qty_sold + qty_left > 0.0 { qty_left / (qty_sold + qty_left) } else { 0.0 };
    conn.execute(
        "UPDATE paper_trades SET realized_pnl = COALESCE(realized_pnl, 0) + ?1,
             pnl = COALESCE(realized_pnl, 0) + ?1, position_size = position_size * ?2,
             filled_qty = ?3, broker_stop_filled_at = ?4, stop_order_id = NULL
         WHERE id = ?5",
        rusqlite::params![pnl_now, left_fraction, qty_left, filled_at, trade_id],
    )?;
    Ok(pnl_now)
}

/// Open positions whose signal has strengthened enough to add to.
///
/// Extracted from the entry pipeline so the join itself can be tested — it was
/// wrong in two ways that only a query-level test can catch, and both had
/// already cost money on trade 25 (AIRI).
///
/// **It matched any cross_signals row ever written for the ticker.** The join
/// was `ON cs.ticker = pt.ticker` with no recency bound, so a stale peak
/// qualified forever. AIRI's April rows (0.5606) cleared the `orig * 1.2`
/// threshold of a position opened in *July* — a signal from three months before
/// the trade existed. The `entry` query above never had this bug: it bounds
/// candidates to `computed_at >= date('now', '-1 day')`.
///
/// **`LIMIT 3` bounds rows, not rows per trade.** With one open position and
/// three qualifying history rows, the same trade came back three times and the
/// loop scaled into it three times in a single pass. The
/// `COALESCE(scale_in_count, 0) < 1` guard cannot stop that: it is evaluated
/// once, when the query runs, before any increment. AIRI carries
/// `scale_in_count = 3` against a guard that permits one — that count is the
/// bug's signature, not a historical artifact.
///
/// Both are fixed the way the entry query already did it: one row per ticker
/// (the freshest), and that row must be current.
///
/// Returns `(trade_id, ticker, original_score, current_score, entry_price, position_size)`.
fn find_scale_in_candidates(
    conn: &rusqlite::Connection,
) -> Vec<(i64, String, f64, f64, f64, f64)> {
    conn.prepare(
        // The third column is COALESCEd, not raw: `original_compound_score` is
        // nullable, and a NULL there made `row.get::<_, f64>(2)` fail, which
        // `filter_map(Result::ok)` then swallowed — silently dropping a trade
        // that the WHERE clause (which does COALESCE) had already qualified.
        "SELECT pt.id, pt.ticker,
                COALESCE(pt.original_compound_score, pt.confidence),
                cs.compound_score, pt.entry_price, pt.position_size
         FROM paper_trades pt
         JOIN cross_signals cs ON cs.id = (
                 SELECT c2.id FROM cross_signals c2
                 WHERE c2.ticker = pt.ticker
                 ORDER BY c2.computed_at DESC, c2.compound_score DESC
                 LIMIT 1
             )
         WHERE pt.status = 'open'
           AND pt.order_status = 'filled'
           AND pt.pnl_pct > 0.0
           AND COALESCE(pt.scale_in_count, 0) < 1
           AND cs.computed_at >= date('now', '-1 day')
           AND cs.compound_score > COALESCE(pt.original_compound_score, pt.confidence) * 1.2
           AND cs.convergence_detected = 1
         ORDER BY cs.compound_score DESC
         LIMIT 3"
    ).ok()
    .map(|mut stmt| {
        stmt.query_map([], |row| {
            Ok((
                row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?,
                row.get(4)?, row.get(5)?,
            ))
        })
            .ok()
            .map(|rows| rows.filter_map(|r| r.ok()).collect::<Vec<_>>())
            .unwrap_or_default()
    })
    .unwrap_or_default()
}

/// Auto-execute paper trades when convergence signals are detected.
/// Only trades entities with tickers, not already held, with convergence_detected = true.
///
/// SAFETY GATE: this is hard-disabled by default. Both the entry path and the
/// scale-in path within this function only run when AUTO_TRADE_ENABLED=true is
/// set in the environment. Pulse is a news/intelligence app first — the trading
/// layer is dormant scaffolding that should not place real orders until a
/// 6-month auto-backtest history demonstrates a durable edge.
/// Alpaca's current market value for an open position in `ticker`, or 0.0.
///
/// Market value, not cost basis: the concentration cap is about how much of the
/// portfolio a name occupies *now*. `paper_trades.position_size` would look
/// cheaper than the truth on exactly the positions the scale-in path targets,
/// since it only scales into winners.
///
/// A 404 means no position, which is genuinely 0.0. Any other failure also reads
/// as 0.0, which is the unsafe direction — it lets a trade through that hidden
/// exposure should have trimmed. That was the pre-existing behaviour on the
/// entry path and is left as-is here rather than changed silently alongside a
/// sizing fix; the notional's own cap still bounds the order.
async fn current_exposure(
    client: &reqwest::Client,
    alpaca_key: &str,
    alpaca_secret: &str,
    ticker: &str,
) -> f64 {
    match client
        .get(format!("https://paper-api.alpaca.markets/v2/positions/{ticker}"))
        .header("APCA-API-KEY-ID", alpaca_key)
        .header("APCA-API-SECRET-KEY", alpaca_secret)
        .send()
        .await
    {
        Ok(r) if r.status().is_success() => r
            .json::<serde_json::Value>()
            .await
            .ok()
            .and_then(|v| {
                v.get("market_value")
                    .and_then(|m| m.as_str())
                    .and_then(|s| s.parse::<f64>().ok())
            })
            .unwrap_or(0.0),
        _ => 0.0,
    }
}

/// Summed stop-out risk of every position Alpaca holds, for the heat cap.
///
/// Each position's trailing stop comes from its open row; a position with no
/// row or no stop yet counts at the hard stop. `None` when Alpaca cannot be
/// read — the caller must not size against an unknown book.
async fn open_book_risk(
    client: &reqwest::Client,
    alpaca_key: &str,
    alpaca_secret: &str,
    conn: &rusqlite::Connection,
    params: &pulse_weights::risk_sizing::RiskParams,
) -> Option<f64> {
    let resp = client
        .get("https://paper-api.alpaca.markets/v2/positions")
        .header("APCA-API-KEY-ID", alpaca_key)
        .header("APCA-API-SECRET-KEY", alpaca_secret)
        .send()
        .await
        .ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let positions: Vec<serde_json::Value> = resp.json().await.ok()?;
    let mut total = 0.0;
    for pos in &positions {
        let symbol = pos.get("symbol").and_then(|v| v.as_str()).unwrap_or("");
        let stop: Option<f64> = conn
            .query_row(
                "SELECT trailing_stop FROM paper_trades
                 WHERE ticker = ?1 AND status = 'open' ORDER BY id DESC LIMIT 1",
                [symbol],
                |row| row.get::<_, Option<f64>>(0),
            )
            .ok()
            .flatten();
        total += pulse_weights::risk_sizing::position_risk(
            order_num(pos, "qty"),
            order_num(pos, "current_price"),
            stop,
            params,
        );
    }
    Some(total)
}

/// Whole shares for a dollar budget at `price`, and what they cost. None when
/// not even one share fits.
pub(crate) fn whole_shares(notional: f64, price: f64) -> Option<(u64, f64)> {
    if !(price.is_finite() && notional.is_finite()) || price <= 0.0 {
        return None;
    }
    let qty = (notional / price).floor();
    (qty >= 1.0).then_some((qty as u64, qty * price))
}

/// Size a buy in whole shares from the latest price. Logs and returns None
/// when the price is unavailable or one share costs more than the budget.
async fn whole_share_order(
    client: &reqwest::Client,
    creds: &pulse_alpaca::Credentials,
    ticker: &str,
    notional: f64,
) -> Option<(u64, f64)> {
    let price = match pulse_alpaca::latest_price(client, creds, ticker).await {
        Ok(Some(p)) => p,
        Ok(None) => {
            tracing::warn!("Auto-trade: no latest price for {} — skipping", ticker);
            return None;
        }
        Err(e) => {
            tracing::warn!("Auto-trade: price lookup failed for {} ({}) — skipping", ticker, e);
            return None;
        }
    };
    if price < crate::entry_filters::MIN_PRICE {
        tracing::info!("Auto-trade: skipping {} — ${:.2} is under the ${:.0} price floor", ticker, price, crate::entry_filters::MIN_PRICE);
        return None;
    }
    let sized = whole_shares(notional, price);
    if sized.is_none() {
        tracing::info!("Auto-trade: skipping {} — one share (${:.2}) is over the ${:.0} budget", ticker, price, notional);
    }
    sized
}

pub(crate) async fn auto_trade_on_convergence(db_path: &Path) -> anyhow::Result<usize> {
    // Hard kill switch. Default = OFF. Re-enable via `AUTO_TRADE_ENABLED=true`
    // in `.env` once the auto-backtest has shown a positive expectancy across
    // a meaningful window of resolved trades.
    let trading_enabled = std::env::var("AUTO_TRADE_ENABLED")
        .map(|v| v.eq_ignore_ascii_case("true") || v == "1")
        .unwrap_or(false);
    // Preview: every check and size runs, no order is sent, the market clock
    // is ignored. For testing the entry rules any day of the week.
    let preview = std::env::var("AUTO_TRADE_PREVIEW")
        .map(|v| v.eq_ignore_ascii_case("true") || v == "1")
        .unwrap_or(false);
    if preview {
        tracing::warn!("Auto-trade: PREVIEW mode — no orders will be sent (unset AUTO_TRADE_PREVIEW to trade)");
    }
    if !trading_enabled && !preview {
        tracing::info!("Auto-trade: DISABLED (set AUTO_TRADE_ENABLED=true to re-enable)");
        return Ok(0);
    }

    let alpaca_key = match std::env::var("ALPACA_API_KEY") {
        Ok(k) if !k.is_empty() => k,
        _ => {
            tracing::info!("Auto-trade: skipping (ALPACA_API_KEY not set)");
            return Ok(0);
        }
    };
    let alpaca_secret = std::env::var("ALPACA_SECRET_KEY")
        .map_err(|_| anyhow::anyhow!("ALPACA_SECRET_KEY not set"))?;
    let finnhub_key = std::env::var("FINNHUB_API_KEY").unwrap_or_default();
    let creds = pulse_alpaca::Credentials { key: alpaca_key.clone(), secret: alpaca_secret.clone() };

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()?;

    // Buy only while the market is open. An order placed at 08:00 or 21:00
    // used to sit until the open and fill at its first print, often after an
    // overnight gap — at a price nobody saw when deciding. Fails closed.
    match pulse_alpaca::market_open(&client, &creds).await {
        Ok(true) => {}
        _ if preview => tracing::info!("Auto-trade: PREVIEW — market clock ignored"),
        Ok(false) => {
            tracing::info!("Auto-trade: market closed — no entries this run");
            return Ok(0);
        }
        Err(e) => {
            tracing::warn!("Auto-trade: market clock unreadable ({}) — no entries this run", e);
            return Ok(0);
        }
    }

    let conn = rusqlite::Connection::open(db_path)?;

    // Find convergence signals with tickers, not already in open trades.
    // cross_signals stores 1 row per (entity, day), so the same ticker can have
    // multiple historical rows — we want the freshest row per ticker only,
    // otherwise LIMIT N gets eaten by 5+ stale rows of the same name and the
    // system places one trade per day on whichever ticker has the most history.
    let mut stmt = conn.prepare(
        "WITH latest AS (
             SELECT cs.entity_id, cs.ticker, cs.compound_score,
                    cs.insider_signal, cs.institutional_flow, cs.news_momentum,
                    cs.government_signal, cs.search_trend, cs.patent_signal,
                    cs.supply_chain, cs.political_signal,
                    cs.computed_at,
                    ROW_NUMBER() OVER (PARTITION BY cs.ticker ORDER BY cs.computed_at DESC, cs.compound_score DESC) AS rn
             FROM cross_signals cs
             WHERE cs.convergence_detected = 1
               AND cs.ticker IS NOT NULL
               AND cs.compound_score > 0.3
               AND cs.computed_at >= date('now', '-1 day')
         )
         SELECT l.entity_id, l.ticker, l.compound_score, e.name,
                l.insider_signal, l.institutional_flow, l.news_momentum,
                l.government_signal, l.search_trend, l.patent_signal,
                l.supply_chain, l.political_signal,
                COALESCE((SELECT s.insider_buy_volume FROM signals s
                          WHERE s.topic = LOWER(e.name)
                          ORDER BY s.updated_at DESC LIMIT 1), 0) AS insider_raw
         FROM latest l
         JOIN entities e ON e.id = l.entity_id
         WHERE l.rn = 1
           AND l.ticker NOT IN (
               SELECT ticker FROM paper_trades WHERE status = 'open'
           )
           -- No real-catalyst filter (insider/government/news > 0.3): tested
           -- 2026-10-05 on 145 days of signals, it would have removed ARM, INTC
           -- and META, the three best signals (+44% vs SPY over 20 days).
         ORDER BY l.compound_score DESC
         LIMIT 15"
    )?;

    #[allow(clippy::type_complexity)]
    let candidates: Vec<(i64, String, f64, String, f64, f64, f64, f64, f64, f64, f64, f64, f64)> = stmt
        .query_map([], |row| Ok((
            row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?,
            row.get::<_, f64>(4).unwrap_or(0.0), row.get::<_, f64>(5).unwrap_or(0.0),
            row.get::<_, f64>(6).unwrap_or(0.0), row.get::<_, f64>(7).unwrap_or(0.0),
            row.get::<_, f64>(8).unwrap_or(0.0), row.get::<_, f64>(9).unwrap_or(0.0),
            row.get::<_, f64>(10).unwrap_or(0.0), row.get::<_, f64>(11).unwrap_or(0.0),
            row.get::<_, f64>(12).unwrap_or(0.0),
        )))?
        .filter_map(|r| r.ok())
        .collect();

    if candidates.is_empty() {
        return Ok(0);
    }

    // Get account buying power
    let account: serde_json::Value = client
        .get("https://paper-api.alpaca.markets/v2/account")
        .header("APCA-API-KEY-ID", &alpaca_key)
        .header("APCA-API-SECRET-KEY", &alpaca_secret)
        .send()
        .await?
        .json()
        .await?;

    let buying_power: f64 = account.get("buying_power")
        .and_then(|v| v.as_str())
        .and_then(|s| s.parse().ok())
        .unwrap_or(0.0);

    // portfolio_value is the equity-based denominator for concentration limits.
    // Buying power can swing with leverage and pending orders; portfolio_value
    // is the stable "size of the pie" we want to cap each name against.
    let portfolio_value: f64 = account.get("portfolio_value")
        .and_then(|v| v.as_str())
        .and_then(|s| s.parse().ok())
        .unwrap_or(0.0);

    if buying_power < 100.0 {
        tracing::info!("Auto-trade: insufficient buying power (${:.2})", buying_power);
        return Ok(0);
    }

    // Hard cap on per-ticker exposure. Without this, a bug or repeated signals
    // can stack the same name into 20%+ of the portfolio (META did exactly this
    // — 5x duplicate fills put 23% of equity into a single position before any
    // human review).
    //
    // The percentage now lives in `position_sizing` beside the tiers it has to
    // agree with, and is derived from the top tier rather than written out
    // again. It was a local const here at 0.05 while the tiers were doubled to
    // 2/5/10% in another file, so from 2026-07-23 every top-conviction entry was
    // rejected by this check — see MAX_PER_TICKER_PCT's own doc comment.
    use crate::position_sizing::MAX_PER_TICKER_PCT;
    let max_per_ticker_dollars = crate::position_sizing::ticker_headroom(portfolio_value, 0.0);

    // SIZING_MODEL=risk: size by stop-out risk instead of score tier. Built
    // once per run and updated as orders go in, so later candidates see the
    // heat and cash earlier ones used.
    let mut risk_book = if crate::position_sizing::risk_sizing_enabled() {
        let params = pulse_weights::risk_sizing::RiskParams::default();
        let Some(open_risk) =
            open_book_risk(&client, &alpaca_key, &alpaca_secret, &conn, &params).await
        else {
            tracing::warn!("Auto-trade: risk sizing on but Alpaca positions unreadable — no entries this run");
            return Ok(0);
        };
        let rb = crate::position_sizing::RiskBook {
            book: pulse_weights::risk_sizing::Book {
                equity: portfolio_value,
                buying_power,
                drawdown: crate::position_sizing::current_drawdown(&conn, portfolio_value),
                open_risk,
            },
            edge: crate::position_sizing::load_edge_stats(&conn, 100),
            params,
        };
        tracing::info!(
            "Auto-trade: risk sizing — equity ${:.0}, drawdown {:.1}% (x{:.2}), open risk ${:.0} of ${:.0} heat, edge x{:.2} over {} trades",
            rb.book.equity, rb.book.drawdown * 100.0,
            pulse_weights::risk_sizing::drawdown_multiplier(rb.book.drawdown, &rb.params),
            rb.book.open_risk, rb.book.equity * rb.params.max_heat,
            pulse_weights::risk_sizing::edge_multiplier(&rb.edge, &rb.params), rb.edge.trades
        );
        Some(rb)
    } else {
        None
    };

    let now = chrono::Local::now();
    let entry_datetime = now.format("%Y-%m-%dT%H:%M:%S").to_string();
    let mut traded = 0;

    // Book-level context for the entry filters (see entry_filters.rs).
    use crate::entry_filters as ef;
    let today_date = now.date_naive();
    let mut open_positions: i64 = conn
        .query_row("SELECT COUNT(*) FROM paper_trades WHERE status = 'open'", [], |r| r.get(0))
        .unwrap_or(0);
    let regime = match ef::recent_bars(&client, &creds, "SPY", 100).await {
        Ok(spy) => ef::regime_multiplier(&spy),
        Err(e) => {
            tracing::warn!("Auto-trade: SPY bars unavailable ({}) — full size", e);
            1.0
        }
    };
    if regime < 1.0 {
        tracing::info!("Auto-trade: SPY below its 50-day average — new buys at {:.0}% size", regime * 100.0);
    }
    if finnhub_key.is_empty() {
        tracing::warn!("Auto-trade: no FINNHUB_API_KEY — sector cap and earnings blackout are OFF");
    }
    // Unreadable positions fail closed, like the risk book: the sector cap
    // cannot be checked without them.
    let mut by_industry = match ef::exposure_by_industry(&client, &creds, &conn, &finnhub_key).await {
        Ok(m) => m,
        Err(e) => {
            tracing::warn!("Auto-trade: sector exposure unavailable ({}) — no new buys this run", e);
            return Ok(0);
        }
    };
    // An unreadable calendar does not block: this trims risk, it is not a
    // safety gate.
    let earnings = ef::upcoming_earnings(&client, &finnhub_key, today_date).await.unwrap_or_else(|e| {
        tracing::warn!("Auto-trade: earnings calendar unavailable ({}) — not checked this run", e);
        Default::default()
    });

    // Veto threshold: heavy net insider selling. The $1M scale matches the
    // positive-side normalize_signal scale, so -$1M is roughly the mirror image
    // of what would have shown up as a meaningful BUY signal.
    const INSIDER_VETO_THRESHOLD: f64 = -1_000_000.0;

    for (entity_id, ticker, score, name, insider, inst, news, gov, search, patent, supply, political, insider_raw) in &candidates {
        if *insider_raw < INSIDER_VETO_THRESHOLD {
            tracing::info!(
                "Auto-trade: vetoing {} ({}) — heavy insider selling (net ${:.0})",
                name, ticker, insider_raw
            );
            continue;
        }

        // Universe quality gate: $300M market cap / $1 min price / Alpaca-tradable.
        // Cached 7 days. Fails closed on missing data — see check_ticker_universe_eligibility.
        if !crate::market_prices::check_ticker_universe_eligibility(
            &client, &conn, &finnhub_key, &alpaca_key, &alpaca_secret, ticker,
        ).await {
            tracing::warn!("Auto-trade: skipping {} ({}) — failed universe quality gate", name, ticker);
            continue;
        }

        if open_positions >= ef::MAX_OPEN_POSITIONS {
            tracing::info!("Auto-trade: {} positions open (max {}) — no more buys this run", open_positions, ef::MAX_OPEN_POSITIONS);
            break;
        }

        // No buy just before an earnings report. An unreadable calendar does
        // not block: this trims risk, it is not a safety gate.
        if let Some(dates) = earnings.get(ticker.as_str())
            && ef::in_earnings_blackout(dates, today_date)
        {
            tracing::info!("Auto-trade: skipping {} ({}) — earnings within {} trading days", name, ticker, ef::EARNINGS_BLACKOUT_DAYS);
            continue;
        }

        // Liquidity and price from the consolidated tape. Fails closed.
        match ef::recent_bars(&client, &creds, ticker, 40).await {
            Ok(mut bars) => {
                // Today's bar is partial (half an hour of volume at 10:00).
                let today_str = today_date.to_string();
                let last = bars.last().map(|b| b.close).unwrap_or(0.0);
                bars.retain(|b| b.date < today_str);
                let adv = ef::avg_dollar_volume(&bars, 20);
                if adv.is_none_or(|v| v < ef::MIN_DOLLAR_VOLUME) || last < ef::MIN_PRICE {
                    tracing::info!(
                        "Auto-trade: skipping {} ({}) — too thin or cheap (avg ${:.1}M/day, last ${:.2})",
                        name, ticker, adv.unwrap_or(0.0) / 1e6, last
                    );
                    continue;
                }
                // The signal has only paid on stocks that move (see entry_filters).
                let vol = ef::atr_pct(&bars, ef::ATR_DAYS);
                if vol.is_none_or(|v| v < ef::MIN_ATR_PCT) {
                    tracing::info!(
                        "Auto-trade: skipping {} ({}) — too calm (ATR {:.1}% of price, floor {:.0}%)",
                        name, ticker, vol.unwrap_or(0.0) * 100.0, ef::MIN_ATR_PCT * 100.0
                    );
                    continue;
                }
            }
            // No access to the consolidated feed: every candidate would fail
            // the same way, so stop once instead of once per name.
            Err(e) if e.contains("401") || e.contains("403") => {
                tracing::warn!("Auto-trade: daily bars refused ({}) — no buys this run", e);
                break;
            }
            Err(e) => {
                tracing::warn!("Auto-trade: skipping {} — no daily bars ({})", ticker, e);
                continue;
            }
        }

        let notional = match crate::position_sizing::entry_notional(
            portfolio_value,
            buying_power,
            *score,
        ) {
            Some(n) => n * regime,
            None => {
                tracing::info!("Auto-trade: skipping {} — buying power below entry floor", ticker);
                continue;
            }
        };

        let existing_exposure =
            current_exposure(&client, &alpaca_key, &alpaca_secret, ticker).await;

        // Candidates already exclude tickers with an open or pending row, so
        // shares on Alpaca here are shares no row tracks. Buying again would
        // stack a second position on top of an unmanaged one, which is how 37
        // untracked buys doubled positions in Aug–Sep 2026.
        if existing_exposure > 0.0 {
            tracing::warn!(
                "Auto-trade: skipping {} ({}) — Alpaca holds ${:.0} that no open trade tracks; reconcile the ledger first",
                name, ticker, existing_exposure
            );
            continue;
        }

        // Trim to the room left rather than dropping the order. A name holding
        // 9% of the portfolio has 1% of room, and throwing away the whole signal
        // because it did not fit whole was never the intent — the cap bounds
        // exposure, it does not veto participation. Only a remainder below the
        // entry floor is skipped, because an order that small is not worth the
        // round trip.
        let notional = match crate::position_sizing::clamp_to_ticker_cap(
            portfolio_value,
            existing_exposure,
            notional,
        ) {
            Some(trimmed) => {
                if trimmed < notional {
                    tracing::info!(
                        "Auto-trade: trimming {} ({}) from ${:.0} to ${:.0} — existing ${:.0} against a ${:.0} cap ({:.0}% of ${:.0} portfolio)",
                        name, ticker, notional, trimmed, existing_exposure,
                        max_per_ticker_dollars, MAX_PER_TICKER_PCT * 100.0, portfolio_value
                    );
                }
                trimmed
            }
            None => {
                tracing::warn!(
                    "Auto-trade: blocking {} ({}) — existing exposure ${:.0} leaves no room worth taking under the ${:.0} cap ({:.0}% of ${:.0} portfolio)",
                    name, ticker, existing_exposure,
                    max_per_ticker_dollars, MAX_PER_TICKER_PCT * 100.0, portfolio_value
                );
                continue;
            }
        };

        // Under SIZING_MODEL=risk the tier notional above is replaced. Its cap
        // check cannot have blocked: exposure is zero here (see the skip above).
        let mut sized_risk = None;
        let notional = match risk_book.as_ref() {
            None => notional,
            Some(rb) => match rb.size(&conn, ticker, existing_exposure, regime) {
                Some(s) => {
                    tracing::info!(
                        "Auto-trade: {} ({}) risk-sized ${:.0} — risks ${:.0} to a {:.1}% stop",
                        name, ticker, s.notional, s.risk, s.stop_pct * 100.0
                    );
                    sized_risk = Some(s);
                    s.notional
                }
                None => {
                    tracing::info!("Auto-trade: skipping {} ({}) — no heat, cap room or cash left for its risk", name, ticker);
                    continue;
                }
            },
        };

        tracing::info!("Auto-trade: {} ({}) — score {:.2}, notional ${:.2}", name, ticker, score, notional);

        // Cross-run dedup: if the pipeline runs multiple times in a short window
        // (launchd retry storm), the DB `already_open` guard can't help because the
        // order is placed BEFORE the insert settles — that's how ORCL got 5 fills in
        // 8 seconds on 2026-05-04. Two defenses:
        //   1. Pre-place check: ask Alpaca if an OPEN order for this ticker exists.
        //   2. Deterministic client_order_id keyed on ticker+day — Alpaca rejects a
        //      duplicate client_order_id, so a second same-day order for the ticker
        //      is refused at the source even if checks race.
        let today_key = chrono::Local::now().format("%Y%m%d").to_string();
        let client_order_id = format!("pulse-{}-{}", ticker, today_key);

        let has_open_order = match client
            .get(format!("https://paper-api.alpaca.markets/v2/orders?status=open&symbols={}", ticker))
            .header("APCA-API-KEY-ID", &alpaca_key)
            .header("APCA-API-SECRET-KEY", &alpaca_secret)
            .send()
            .await
        {
            Ok(r) if r.status().is_success() => r.json::<serde_json::Value>().await.ok()
                .and_then(|v| v.as_array().map(|a| !a.is_empty()))
                .unwrap_or(false),
            _ => false, // on error, fall through — client_order_id is the backstop
        };
        if has_open_order {
            tracing::warn!("Auto-trade: skipping {} — open order already exists on Alpaca", ticker);
            continue;
        }

        // Whole shares, so the broker stop (GTC, whole shares only) covers the
        // entire position instead of leaving a fraction unprotected.
        let Some((qty, notional)) = whole_share_order(&client, &creds, ticker, notional).await else {
            continue;
        };

        let industry = ef::industry(&client, &conn, &finnhub_key, ticker).await;
        if industry.is_none() && !finnhub_key.is_empty() {
            tracing::warn!("Auto-trade: industry of {} unknown — sector cap not applied to it", ticker);
        }
        if let Some(ind) = industry.as_deref() {
            let held = by_industry.get(ind).copied().unwrap_or(0.0);
            if !ef::sector_has_room(held, notional, portfolio_value) {
                tracing::info!(
                    "Auto-trade: skipping {} ({}) — {} already ${:.0} of ${:.0} ({:.0}% cap)",
                    name, ticker, ind, held, portfolio_value, ef::MAX_SECTOR_PCT * 100.0
                );
                continue;
            }
        }

        if preview {
            tracing::info!(
                "Auto-trade: PREVIEW — would buy {} {} (~${:.0}, {})",
                qty, ticker, notional, industry.as_deref().unwrap_or("industry unknown")
            );
            if let (Some(rb), Some(s)) = (risk_book.as_mut(), sized_risk.as_ref()) {
                rb.commit(s);
            }
            open_positions += 1;
            if let Some(ind) = industry {
                *by_industry.entry(ind).or_default() += notional;
            }
            traded += 1;
            continue;
        }

        let order = serde_json::json!({
            "symbol": ticker,
            "qty": qty.to_string(),
            "side": "buy",
            "type": "market",
            "time_in_force": "day",
            "client_order_id": client_order_id
        });

        let resp = client
            .post("https://paper-api.alpaca.markets/v2/orders")
            .header("APCA-API-KEY-ID", &alpaca_key)
            .header("APCA-API-SECRET-KEY", &alpaca_secret)
            .json(&order)
            .send()
            .await?;

        if resp.status().is_success() {
            let order_resp: serde_json::Value = resp.json().await?;
            let order_id = order_resp.get("id").and_then(|v| v.as_str()).unwrap_or("unknown").to_string();

            // Poll for fill. During market hours this fills within seconds;
            // overnight it stays open until 9:30 and the row is written as
            // pending, to be settled by order id on the next run.
            let fill = poll_fill(&client, &alpaca_key, &alpaca_secret, &order_id).await;
            let order_status = if fill.is_some() { "filled" } else { "pending" };
            if fill.is_none() {
                tracing::info!("Auto-trade: order {} not filled yet — recording {} as pending", order_id, ticker);
            }
            let mut filled_price = fill.as_ref().map(|f| f.price).unwrap_or(0.0);
            let filled_qty = fill.as_ref().map(|f| f.qty).unwrap_or(0.0);
            let fill_time = fill
                .as_ref()
                .map(|f| f.at_local.clone())
                .unwrap_or_else(|| entry_datetime.clone());

            if filled_price <= 0.0 {
                // Order submitted but fill price unknown — use latest known price as estimate
                // to avoid ghost positions (Alpaca has it, but we don't track it)
                filled_price = conn.query_row(
                    "SELECT close FROM entity_prices WHERE ticker = ?1 ORDER BY date DESC LIMIT 1",
                    [ticker.as_str()], |row| row.get(0),
                ).unwrap_or(0.0);
                tracing::warn!("Auto-trade: fill price unknown for {} ({}), using estimate ${:.2}", ticker, order_id, filled_price);
                if filled_price <= 0.0 {
                    tracing::warn!("Auto-trade: no price data for {}, cancelling order {}", ticker, order_id);
                    client.delete(format!("https://paper-api.alpaca.markets/v2/orders/{}", order_id))
                        .header("APCA-API-KEY-ID", &alpaca_key)
                        .header("APCA-API-SECRET-KEY", &alpaca_secret)
                        .send().await.ok();
                    continue;
                }
            }

            // Capture the recent stories that drove this signal so the trade
            // journal can show "we bought because of these specific headlines".
            // Mentions often land on an alias row of the same company ("Arm" vs
            // "ARM HOLDINGS PLC"), so include every entity mapped to the trade's
            // ticker. Grouping by ticker, NOT entities.canonical_id — canonical
            // groups are polluted (AIRI's contains Fitbit; ARM's canonical is
            // Vertex Pharma), which would attach unrelated headlines as the
            // trade's "reason". 30-day window matches the source_diversity
            // starvation fix. The order has already executed on Alpaca by this
            // point, so a story-lookup failure must never skip recording the
            // trade — every error path degrades to an empty list, never
            // `continue`.
            let story_refs: Vec<(i64, String, String)> = conn
                .prepare(
                    "SELECT s.id, s.headline, s.source_name
                     FROM entity_mentions em
                     JOIN stories s ON s.id = em.story_id
                     WHERE em.entity_id IN (
                           SELECT entity_id FROM entity_tickers WHERE ticker = ?2
                           UNION SELECT ?1
                       )
                       AND em.mentioned_at >= date('now', '-30 days')
                     ORDER BY em.mentioned_at DESC, s.importance_score DESC
                     LIMIT 8"
                )
                .ok()
                .and_then(|mut stmt| {
                    stmt.query_map(
                        rusqlite::params![entity_id, ticker.as_str()],
                        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                    )
                    .ok()
                    .map(|rows| rows.filter_map(|r| r.ok()).collect())
                })
                .unwrap_or_default();

            // Build signal_profile JSON matching calibration keys.
            // Now includes `stories[]` so the trade journal can reference the
            // specific headlines that drove the entry decision.
            let signal_profile = serde_json::json!({
                "insider": insider,
                "institutional": inst,
                "news": news,
                "government": gov,
                "search": search,
                "patent": patent,
                "supply_chain": supply,
                "political": political,
                "stories": story_refs.iter().map(|(id, head, src)| {
                    serde_json::json!({"id": id, "headline": head, "source": src})
                }).collect::<Vec<_>>(),
            });

            // Re-check: the candidate query filtered open positions at fetch time,
            // but Alpaca may have filled this order while another iteration was
            // processing the same ticker. Refuse the second insert.
            let already_open: bool = conn
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM paper_trades WHERE ticker = ?1 AND status = 'open')",
                    [ticker.as_str()],
                    |row| row.get(0),
                )
                .unwrap_or(false);
            if already_open {
                tracing::warn!("Auto-trade: skipping duplicate insert for {} — already open", ticker);
                continue;
            }

            // Record in paper_trades with position management columns
            if let Err(e) = conn.execute(
                "INSERT INTO paper_trades (entity_id, ticker, direction, entry_price, entry_date, position_size, confidence, signal_profile, alpaca_order_id, status, high_water_mark, original_compound_score, order_status, filled_qty)
                 VALUES (?1, ?2, 'long', ?3, ?4, ?5, ?6, ?7, ?8, 'open', ?3, ?6, ?9, ?10)",
                rusqlite::params![
                    entity_id, ticker, filled_price, fill_time, notional, score,
                    signal_profile.to_string(), order_id, order_status,
                    (filled_qty > 0.0).then_some(filled_qty),
                ],
            ) {
                tracing::warn!("Auto-trade: failed to record trade for {}: {}", ticker, e);
            }

            tracing::info!("Auto-trade: placed order {} for {} (${:.2} @ ${:.2}, qty {:.4})", order_id, ticker, notional, filled_price, filled_qty);
            if let (Some(rb), Some(s)) = (risk_book.as_mut(), sized_risk.as_ref()) {
                rb.commit(s);
            }
            open_positions += 1;
            if let Some(ind) = industry {
                *by_industry.entry(ind).or_default() += notional;
            }
            traded += 1;
        } else {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            // A duplicate client_order_id (same ticker, same day) is the dedup guard
            // working as intended during a re-run race — log it as such, not as an error.
            if body.contains("client_order_id") || status.as_u16() == 422 {
                tracing::info!("Auto-trade: dedup blocked duplicate same-day order for {} ({})", ticker, status);
            } else {
                tracing::warn!("Auto-trade: order failed for {} — {} {}", ticker, status, body);
            }
        }

        // Rate limit
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }

    // Scale-in: check if existing positions have strengthening signals
    let scale_in_candidates = find_scale_in_candidates(&conn);

    for (trade_id, ticker, _old_score, new_score, held_entry, held_notional) in &scale_in_candidates {
        let scale_notional = match crate::position_sizing::scale_in_notional(buying_power) {
            Some(n) => n,
            None => {
                tracing::info!("Scale-in: skipping {} — buying power below floor", ticker);
                continue;
            }
        };

        // The concentration cap applied to entries and not to scale-ins, because
        // it was a const local to the entry loop. That left the accumulation hole
        // the cap exists to close: a name already at the ceiling could still take
        // 1% of *buying power* on top, which on a margin account is a bigger
        // number than the cap being bypassed. Only one scale-in per trade is
        // allowed, so the overshoot was bounded — it was not prevented.
        let scale_exposure =
            current_exposure(&client, &alpaca_key, &alpaca_secret, ticker).await;
        let scale_notional = match crate::position_sizing::clamp_to_ticker_cap(
            portfolio_value,
            scale_exposure,
            scale_notional,
        ) {
            Some(trimmed) => {
                if trimmed < scale_notional {
                    tracing::info!(
                        "Scale-in: trimming {} from ${:.0} to ${:.0} — already holding ${:.0} of a ${:.0} cap",
                        ticker, scale_notional, trimmed, scale_exposure, max_per_ticker_dollars
                    );
                }
                trimmed
            }
            None => {
                tracing::info!(
                    "Scale-in: skipping {} — already holding ${:.0} against a ${:.0} cap ({:.0}% of ${:.0} portfolio)",
                    ticker, scale_exposure, max_per_ticker_dollars,
                    MAX_PER_TICKER_PCT * 100.0, portfolio_value
                );
                continue;
            }
        };

        let mut sized_risk = None;
        let scale_notional = match risk_book.as_ref() {
            None => scale_notional,
            Some(rb) => match rb.size(&conn, ticker, scale_exposure, rb.params.scale_in_fraction) {
                Some(s) => {
                    sized_risk = Some(s);
                    s.notional
                }
                None => {
                    tracing::info!("Scale-in: skipping {} — no heat, cap room or cash left for its risk", ticker);
                    continue;
                }
            },
        };

        let Some((qty, scale_notional)) = whole_share_order(&client, &creds, ticker, scale_notional).await else {
            continue;
        };
        tracing::info!("Scale-in: {} — score increased to {:.2}, adding {} sh (~${:.2})", ticker, new_score, qty, scale_notional);

        if preview {
            tracing::info!("Scale-in: PREVIEW — would add {} {}", qty, ticker);
            continue;
        }

        // One add per ticker per day, refused at the source like entries.
        let order = serde_json::json!({
            "symbol": ticker,
            "qty": qty.to_string(),
            "side": "buy",
            "type": "market",
            "time_in_force": "day",
            "client_order_id": format!("pulse-add-{}-{}", ticker, chrono::Local::now().format("%Y%m%d"))
        });

        let resp = client
            .post("https://paper-api.alpaca.markets/v2/orders")
            .header("APCA-API-KEY-ID", &alpaca_key)
            .header("APCA-API-SECRET-KEY", &alpaca_secret)
            .json(&order)
            .send()
            .await?;

        if resp.status().is_success() {
            let order_id = resp
                .json::<serde_json::Value>()
                .await
                .ok()
                .and_then(|v| v.get("id").and_then(|i| i.as_str()).map(String::from))
                .unwrap_or_default();

            // The added shares cost more than the original ones — scale-in only
            // fires on a winner — so the basis has to move with the size. Both
            // columns update together or `pnl_pct`, the -15% hard stop and the
            // profit target all keep measuring against a price we no longer
            // paid.
            let fill = if order_id.is_empty() {
                None
            } else {
                poll_fill(&client, &alpaca_key, &alpaca_secret, &order_id).await
            };
            let add_price = fill.as_ref().map(|f| f.price).filter(|p| *p > 0.0);
            let new_basis = add_price.and_then(|p| {
                crate::position_sizing::blended_entry_price(
                    *held_notional, *held_entry, scale_notional, p,
                )
            });

            match new_basis {
                Some(basis) => {
                    conn.execute(
                        "UPDATE paper_trades SET scale_in_count = COALESCE(scale_in_count, 0) + 1,
                         position_size = position_size + ?1, entry_price = ?2 WHERE id = ?3",
                        rusqlite::params![scale_notional, basis, trade_id],
                    ).ok();
                    tracing::info!(
                        "Scale-in: added ${:.2} to {} at ${:.2} — basis ${:.2} -> ${:.2}",
                        scale_notional, ticker, add_price.unwrap_or(0.0), held_entry, basis
                    );
                }
                None => {
                    // The money is committed either way, so the size must be
                    // recorded — an unrecorded fill is a ghost position. But
                    // without a fill price there is no honest basis to write,
                    // and a stale entity_prices close is exactly the kind of
                    // guess that put a wrong number in this column before.
                    conn.execute(
                        "UPDATE paper_trades SET scale_in_count = COALESCE(scale_in_count, 0) + 1,
                         position_size = position_size + ?1 WHERE id = ?2",
                        rusqlite::params![scale_notional, trade_id],
                    ).ok();
                    tracing::warn!(
                        "Scale-in: added ${:.2} to {} but no fill price (order {}) — \
                         size recorded, basis left at ${:.2} and now understated",
                        scale_notional, ticker, order_id, held_entry
                    );
                }
            }
            if let (Some(rb), Some(s)) = (risk_book.as_mut(), sized_risk.as_ref()) {
                rb.commit(s);
            }
            traded += 1;
        }

        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }

    Ok(traded)
}

/// The most recent filled sell for a ticker on Alpaca.
pub(crate) struct SellFill {
    pub price: f64,
    pub qty: f64,
    /// When it filled, local time. A close between runs is dated by this, not
    /// by the run that noticed it.
    pub at_local: String,
    pub client_order_id: String,
}

/// Fetch the most recent FILLED sell for a ticker from Alpaca.
/// Used to record real exit P&L when a position closed between runs (e.g. a
/// pre-market `day` order that filled after the open, or the broker stop).
pub(crate) async fn latest_filled_sell(
    client: &reqwest::Client, key: &str, secret: &str, ticker: &str,
) -> Option<SellFill> {
    let resp = client
        .get(format!(
            "https://paper-api.alpaca.markets/v2/orders?status=closed&symbols={}&side=sell&limit=5&direction=desc",
            ticker
        ))
        .header("APCA-API-KEY-ID", key)
        .header("APCA-API-SECRET-KEY", secret)
        .send()
        .await
        .ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let orders: serde_json::Value = resp.json().await.ok()?;
    orders.as_array()?.iter().find_map(|o| {
        let price = order_num(o, "filled_avg_price");
        let qty = order_num(o, "filled_qty");
        (qty > 0.0 && price > 0.0).then(|| SellFill {
            price,
            qty,
            at_local: order_filled_at_local(o),
            client_order_id: o.get("client_order_id").and_then(|v| v.as_str()).unwrap_or("").to_string(),
        })
    })
}

/// Public entry point to run ONLY the position-exit evaluation (Phase 13.6) without
/// the full daily pipeline. Used by `--mode manage-positions` for testing/manual runs.
/// Honors EXIT_DRY_RUN like the in-pipeline call.
pub(crate) async fn run_position_management(db_path: &Path) -> anyhow::Result<usize> {
    manage_open_positions(db_path).await
}

/// Run ONLY the auto-buy phase (Phase 13.5) in isolation — same gate, same
/// candidate query, same Alpaca paper endpoint as the daily pipeline. Exists as a
/// manual buy trigger: the daily run short-circuits at the "already fetched today"
/// guard once the day's first slot succeeds, so later slots never reach Phase 13.5.
/// This lets a fresh convergence set be acted on without a re-fetch. Still hard-gated
/// by AUTO_TRADE_ENABLED; a no-op when disarmed. Paper-only (hardcoded endpoint).
pub(crate) async fn run_auto_trade(db_path: &Path) -> anyhow::Result<usize> {
    auto_trade_on_convergence(db_path).await
}

/// One open row, as `manage_open_positions` reads it.
struct OpenTrade {
    id: i64,
    ticker: String,
    entry_price: f64,
    entry_date: String,
    /// Drives signal decay.
    orig_score: f64,
    /// Gates CloseHalf from re-triggering.
    half_closed_at: Option<String>,
    position_size: f64,
    order_status: String,
    order_id: Option<String>,
    stop_order_id: Option<String>,
    /// Set once the broker stop has sold shares and the rest is still to close.
    stop_filled_at: Option<String>,
}

fn load_open_trades(conn: &rusqlite::Connection) -> rusqlite::Result<Vec<OpenTrade>> {
    let mut stmt = conn.prepare(
        "SELECT id, ticker, entry_price, entry_date,
                COALESCE(original_compound_score, confidence, 0.30),
                half_closed_at, position_size, order_status, alpaca_order_id,
                stop_order_id, broker_stop_filled_at
         FROM paper_trades WHERE status = 'open'",
    )?;
    let rows = stmt
        .query_map([], |row| {
            Ok(OpenTrade {
                id: row.get(0)?,
                ticker: row.get(1)?,
                entry_price: row.get(2)?,
                entry_date: row.get(3)?,
                orig_score: row.get::<_, f64>(4).unwrap_or(0.30),
                half_closed_at: row.get::<_, Option<String>>(5).unwrap_or(None),
                position_size: row.get::<_, f64>(6).unwrap_or(0.0),
                order_status: row.get::<_, String>(7).unwrap_or_else(|_| "filled".to_string()),
                order_id: row.get::<_, Option<String>>(8).unwrap_or(None),
                stop_order_id: row.get::<_, Option<String>>(9).unwrap_or(None),
                stop_filled_at: row.get::<_, Option<String>>(10).unwrap_or(None),
            })
        })?
        .filter_map(|r| r.ok())
        .collect();
    Ok(rows)
}

/// Journal a close. Phase 13.6 is the single exit authority, so it owns
/// journal generation — calibration is measure-only and no longer closes
/// trades. Without this, the Portfolio exit-reasons feature gets no entries.
fn journal_close(conn: &rusqlite::Connection, t: &OpenTrade, exit_at: &str, exit_price: f64, pnl_dollars: f64, reason: &str) {
    let pnl_pct = ((exit_price - t.entry_price) / t.entry_price) * 100.0;
    let exit_day = exit_at.get(..10).unwrap_or(exit_at);
    crate::position_management::generate_trade_journal(
        conn, t.id, &t.ticker, &t.entry_date, exit_day,
        t.entry_price, exit_price, t.position_size, pnl_pct, pnl_dollars, exit_status(reason),
    );
}

/// Alpaca no longer holds a position the DB says is open: it was sold between
/// runs — by the broker stop, by a sell placed last run that filled at the
/// open, or by hand on Alpaca. Record the real exit so it shows up in
/// analytics; a null-P&L close is silently dropped by the win-rate queries.
async fn reconcile_missing_position(
    conn: &rusqlite::Connection,
    client: &reqwest::Client,
    creds: &pulse_alpaca::Credentials,
    t: &OpenTrade,
    today: &str,
    dry_run: bool,
) {
    use pulse_alpaca::stops;
    if dry_run {
        tracing::warn!("Position mgmt: {} open in DB but absent on Alpaca — would reconcile (dry-run)", t.ticker);
        return;
    }

    // The broker stop sold every share.
    if t.stop_filled_at.is_none()
        && let Some(id) = t.stop_order_id.as_deref()
        && let Ok(Some(o)) = stops::get_order(client, creds, id).await
        && let Some((qty, price)) = o.sold()
    {
        let at = utc_to_local(o.filled_at.as_deref());
        let reason = format!("broker_stop (stop ${:.2})", o.stop_price);
        let pnl = record_full_close(conn, t.id, t.entry_price, price, qty, &at, &reason).unwrap_or(0.0);
        journal_close(conn, t, &at, price, pnl, &reason);
        tracing::warn!(
            "Position mgmt: {} stopped out by the broker — {:.0} sh @ ${:.2} ({:+.1}%, ${:+.2})",
            t.ticker, qty, price, ((price - t.entry_price) / t.entry_price) * 100.0, pnl
        );
        return;
    }

    let Some(sell) = latest_filled_sell(client, &creds.key, &creds.secret, &t.ticker).await else {
        // No fill record found — close without P&L rather than leave a
        // phantom-open row, but log it loudly for review.
        conn.execute(
            "UPDATE paper_trades SET status='closed', exit_date=?1,
                exit_reason='reconcile_no_fill', stop_order_id = NULL WHERE id=?2",
            rusqlite::params![today, t.id],
        )
        .ok();
        tracing::warn!("Position mgmt: {} absent on Alpaca, no sell fill found — closed with NULL pnl (review)", t.ticker);
        return;
    };

    let by_stop = sell.client_order_id.starts_with(stops::STOP_ID_PREFIX);
    let (qty, at, reason) = match (&t.stop_filled_at, by_stop) {
        // The latest sell is the stop sale already booked: nothing else sold.
        (Some(stop_at), true) => (0.0, stop_at.clone(), "broker_stop".to_string()),
        // The remainder after a booked stop sale.
        (Some(_), false) => (sell.qty, sell.at_local.clone(), "broker_stop".to_string()),
        (None, true) => (sell.qty, sell.at_local.clone(), "broker_stop".to_string()),
        (None, false) => (sell.qty, sell.at_local.clone(), "closed_between_runs".to_string()),
    };
    // P&L from the shares that sell actually moved, not position_size /
    // entry_price, which was wrong whenever the buy filled away from the estimate.
    let pnl_now = record_full_close(conn, t.id, t.entry_price, sell.price, qty, &at, &reason).unwrap_or(0.0);
    if reason == "broker_stop" {
        let total: f64 = conn
            .query_row("SELECT COALESCE(pnl, 0) FROM paper_trades WHERE id = ?1", [t.id], |r| r.get(0))
            .unwrap_or(pnl_now);
        journal_close(conn, t, &at, sell.price, total, &reason);
    }
    tracing::warn!(
        "Position mgmt: {} closed between runs ({}) — reconciled @ ${:.2} ({:+.1}%, ${:+.2})",
        t.ticker, reason, sell.price, ((sell.price - t.entry_price) / t.entry_price) * 100.0, pnl_now
    );
}

/// Bring the broker stop in line with the wanted level: place it, raise it,
/// resize it, or cancel it when under one share is held. Never lowers it.
/// Failures are logged, not fatal — the stop checked each run still applies.
#[allow(clippy::too_many_arguments)]
async fn sync_broker_stop(
    conn: &rusqlite::Connection,
    client: &reqwest::Client,
    creds: &pulse_alpaca::Credentials,
    trade_id: i64,
    ticker: &str,
    held_qty: f64,
    want_price: f64,
    live: Option<&pulse_alpaca::stops::StopOrder>,
    dry_run: bool,
) {
    use pulse_alpaca::stops::{self, StopPlan};
    let plan = stops::plan_stop(live, held_qty, want_price);
    if plan == StopPlan::Keep {
        return;
    }
    if dry_run {
        tracing::info!("Position mgmt: DRY_RUN — {} broker stop would change: {:?}", ticker, plan);
        return;
    }
    if plan == StopPlan::Cancel {
        if let Some(o) = live {
            match stops::cancel_and_wait(client, creds, &o.id).await {
                Ok(_) => {
                    conn.execute(
                        "UPDATE paper_trades SET stop_order_id = NULL, broker_stop_price = NULL WHERE id = ?1",
                        [trade_id],
                    )
                    .ok();
                    tracing::info!("Position mgmt: {} under one share — broker stop cancelled", ticker);
                }
                Err(e) => tracing::warn!("Position mgmt: {} broker stop not cancelled: {}", ticker, e),
            }
        }
        return;
    }
    let coid = format!("{}{}-{}-{}", stops::STOP_ID_PREFIX, ticker, trade_id, chrono::Utc::now().timestamp());
    let result = match (&plan, live) {
        (StopPlan::Place { qty, stop_price }, _) => stops::place_stop(client, creds, ticker, *qty, *stop_price, &coid).await,
        (StopPlan::Replace { qty, stop_price }, Some(o)) => {
            match stops::replace_stop(client, creds, &o.id, *qty, *stop_price).await {
                Ok(n) => Ok(n),
                // Refused while accepted / pending: cancel and place anew.
                Err(e) => {
                    tracing::info!("Position mgmt: {} stop replace refused ({}), re-placing", ticker, e);
                    match stops::cancel_and_wait(client, creds, &o.id).await {
                        // It fired meanwhile; the next run books the sale.
                        Ok(c) if c.sold().is_some() => Err("stop filled while being replaced".to_string()),
                        Ok(_) => stops::place_stop(client, creds, ticker, *qty, *stop_price, &coid).await,
                        Err(e) => Err(e),
                    }
                }
            }
        }
        _ => return,
    };
    match result {
        Ok(o) => {
            conn.execute(
                "UPDATE paper_trades SET stop_order_id = ?1, broker_stop_price = ?2 WHERE id = ?3",
                rusqlite::params![o.id, o.stop_price, trade_id],
            )
            .ok();
            crate::db::log_api_usage(conn, "alpaca", "trading", "stop_order", 0, 0);
            tracing::info!(
                "Position mgmt: {} broker stop {} — {:.0} sh @ ${:.2}",
                ticker, if live.is_some() { "moved" } else { "placed" }, o.qty, o.stop_price
            );
        }
        Err(e) => tracing::warn!("Position mgmt: {} broker stop not updated: {}", ticker, e),
    }
}

/// Phase 13.6 — evaluate and act on exits for every open paper position.
///
/// This is the half of the loop that was missing: `position_management::
/// evaluate_position` (ATR trailing stops, profit targets, time/signal decay)
/// existed but was never called at runtime, so positions opened and NEVER
/// closed. This wires it in.
///
/// Safety model:
/// - Runs by default (exits are protective), unlike auto-BUY which is gated off.
/// - `EXIT_DRY_RUN` (default TRUE) logs the decided action WITHOUT placing any
///   sell order. Flip to false only after eyeballing the log.
/// - Current price + sell qty come from Alpaca's own position record (ground
///   truth), never from DB notional (which stores dollars, not shares).
/// - Non-fills (pre-market `day` orders) are NOT marked closed — retried next run.
/// - Each held position keeps a GTC stop at Alpaca for its whole shares, at the
///   same level this checks, so a drop between runs sells at the stop and not
///   at whatever price the next run finds (ARM: -16.4% against a -15% stop).
///
/// Returns the number of exit ACTIONS executed (orders placed, or in dry-run,
/// actions that WOULD have been placed).
pub(crate) async fn manage_open_positions(db_path: &Path) -> anyhow::Result<usize> {
    use pulse_alpaca::stops;

    let dry_run = std::env::var("EXIT_DRY_RUN")
        .map(|v| !(v.eq_ignore_ascii_case("false") || v == "0"))
        .unwrap_or(true); // default: dry-run ON

    let alpaca_key = match std::env::var("ALPACA_API_KEY") {
        Ok(k) if !k.is_empty() => k,
        _ => {
            tracing::info!("Position mgmt: skipping (ALPACA_API_KEY not set)");
            return Ok(0);
        }
    };
    let alpaca_secret = std::env::var("ALPACA_SECRET_KEY")
        .map_err(|_| anyhow::anyhow!("ALPACA_SECRET_KEY not set"))?;
    let creds = pulse_alpaca::Credentials { key: alpaca_key.clone(), secret: alpaca_secret.clone() };

    let conn = rusqlite::Connection::open(db_path)?;
    // Ensure schema is current (idempotent) — needed when this runs standalone
    // via `--mode manage-positions` without the full pipeline's migration pass.
    crate::db::run_migrations(&conn)?;

    let open = load_open_trades(&conn)?;
    if open.is_empty() {
        return Ok(0);
    }

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()?;

    let today = chrono::Local::now().format("%Y-%m-%d").to_string();
    let now_dt = chrono::Local::now().format("%Y-%m-%dT%H:%M:%S").to_string();
    let mut actions = 0usize;

    for t in &open {
        let ticker = &t.ticker;
        let entry_price = &t.entry_price;
        // A pending entry has no position to evaluate yet, and a 404 on the
        // position endpoint means "not filled", not "sold". Settle it from the
        // order itself. This is bookkeeping, not an order, so it runs in
        // dry-run too. Exits are evaluated from the next run, on the real basis.
        if t.order_status == "pending" {
            let Some(order_id) = t.order_id.as_deref().filter(|id| !id.is_empty() && *id != "unknown") else {
                tracing::warn!("Position mgmt: {} is pending with no order id — cannot settle, review", ticker);
                continue;
            };
            let order: Option<serde_json::Value> = match client
                .get(format!("https://paper-api.alpaca.markets/v2/orders/{}", order_id))
                .header("APCA-API-KEY-ID", &alpaca_key)
                .header("APCA-API-SECRET-KEY", &alpaca_secret)
                .send()
                .await
            {
                Ok(r) if r.status().is_success() => r.json().await.ok(),
                _ => None,
            };
            let Some(order) = order else {
                tracing::warn!("Position mgmt: could not fetch entry order {} for {}, leaving pending", order_id, ticker);
                continue;
            };
            let settlement = classify_entry_order(&order);
            if let Err(e) = apply_entry_settlement(&conn, t.id, &settlement, &today) {
                tracing::warn!("Position mgmt: failed to settle {} ({}): {}", ticker, order_id, e);
                continue;
            }
            match settlement {
                EntrySettlement::Filled { price, qty, .. } => tracing::info!(
                    "Position mgmt: {} entry filled — {:.4} sh @ ${:.2}", ticker, qty, price
                ),
                EntrySettlement::Dead { status } => tracing::warn!(
                    "Position mgmt: {} entry order ended {} with no fill — row expired", ticker, status
                ),
                EntrySettlement::Pending => tracing::info!("Position mgmt: {} entry still pending", ticker),
            }
            continue;
        }

        // Ground truth from Alpaca: current_price + held qty. If Alpaca has no
        // position (sold between runs), reconcile the DB to closed.
        let pos: Option<serde_json::Value> = match client
            .get(format!("https://paper-api.alpaca.markets/v2/positions/{}", ticker))
            .header("APCA-API-KEY-ID", &alpaca_key)
            .header("APCA-API-SECRET-KEY", &alpaca_secret)
            .send()
            .await
        {
            Ok(r) if r.status().is_success() => r.json().await.ok(),
            Ok(r) if r.status().as_u16() == 404 => {
                reconcile_missing_position(&conn, &client, &creds, t, &today, dry_run).await;
                continue;
            }
            _ => {
                tracing::warn!("Position mgmt: failed to fetch Alpaca position for {}, skipping", ticker);
                continue;
            }
        };

        let pos = match pos { Some(p) => p, None => continue };
        let current_price: f64 = pos.get("current_price")
            .and_then(|v| v.as_str()).and_then(|s| s.parse().ok()).unwrap_or(0.0);
        let mut held_qty: f64 = pos.get("qty")
            .and_then(|v| v.as_str()).and_then(|s| s.parse().ok()).unwrap_or(0.0);

        if current_price <= 0.0 || held_qty <= 0.0 {
            tracing::warn!("Position mgmt: {} has no valid price/qty from Alpaca, skipping", ticker);
            continue;
        }

        // The broker stop: did it sell since the last run, and what is live now?
        // `stop_known` is false when Alpaca could not be asked — placing a stop
        // blind could stack a second one on the first.
        let mut stop_fired = t.stop_filled_at.is_some();
        // The stop still to cancel before a sell. Cleared once its sale is
        // booked, or cancelling it would find the same fill and book it twice.
        let mut known_stop: Option<&str> = if stop_fired { None } else { t.stop_order_id.as_deref() };
        let mut live_stop: Option<stops::StopOrder> = None;
        let mut stop_known = true;
        if !stop_fired && let Some(id) = t.stop_order_id.as_deref() {
            match stops::get_order(&client, &creds, id).await {
                Ok(Some(o)) if o.is_final() => {
                    if let Some((sold, price)) = o.sold() {
                        // A position is still here, so the stop did not cover
                        // it all: the fractional part, or shares added since.
                        let at = utc_to_local(o.filled_at.as_deref());
                        let booked = book_stop_sale(&conn, t.id, *entry_price, price, sold, held_qty, &at).unwrap_or(0.0);
                        tracing::warn!(
                            "Position mgmt: {} broker stop sold {:.0} sh @ ${:.2} (${:+.2}) — closing the remaining {:.4} sh",
                            ticker, sold, price, booked, held_qty
                        );
                        stop_fired = true;
                        known_stop = None;
                    } else {
                        // Cancelled, expired or replaced without selling.
                        conn.execute("UPDATE paper_trades SET stop_order_id = NULL WHERE id = ?1", [t.id]).ok();
                    }
                }
                Ok(Some(o)) => live_stop = Some(o),
                Ok(None) => {
                    conn.execute("UPDATE paper_trades SET stop_order_id = NULL WHERE id = ?1", [t.id]).ok();
                }
                Err(e) => {
                    tracing::warn!("Position mgmt: {} broker stop unreadable: {}", ticker, e);
                    stop_known = false;
                }
            }
        }
        // No stop on record (first run with broker stops, or a replace that
        // gave it a new id): adopt the one already open on Alpaca, if any.
        if !stop_fired && stop_known && live_stop.is_none() {
            match stops::open_stops(&client, &creds, ticker).await {
                Ok(found) => {
                    live_stop = found.into_iter().max_by(|a, b| {
                        a.stop_price.partial_cmp(&b.stop_price).unwrap_or(std::cmp::Ordering::Equal)
                    });
                    if let Some(o) = &live_stop {
                        conn.execute(
                            "UPDATE paper_trades SET stop_order_id = ?1, broker_stop_price = ?2 WHERE id = ?3",
                            rusqlite::params![o.id, o.stop_price, t.id],
                        )
                        .ok();
                    }
                }
                Err(e) => {
                    tracing::warn!("Position mgmt: {} open stops unreadable: {}", ticker, e);
                    stop_known = false;
                }
            }
        }

        use crate::position_management::PositionAction;
        let pnl_pct = ((current_price - entry_price) / entry_price) * 100.0;
        let (mut close_qty, mut reason): (f64, String) = if stop_fired {
            (held_qty, "broker_stop (remainder after the stop sold)".to_string())
        } else {
            // Signal decay check first — cheapest, and a decayed thesis means
            // exit regardless of price action.
            let decayed = crate::position_management::check_signal_decay(&conn, ticker, t.orig_score);
            let action = crate::position_management::evaluate_position(
                &conn, t.id, ticker, *entry_price, current_price,
            );
            // Decide final action: signal decay forces a full close (overrides Hold).
            match action {
                PositionAction::CloseAll { reason } => (held_qty, reason),
                PositionAction::CloseHalf { reason } => {
                    if t.half_closed_at.is_some() {
                        // Already took profit on half — don't keep peeling. Hold the rest.
                        tracing::info!("Position mgmt: {} CloseHalf suppressed (already half-closed)", ticker);
                        (0.0, String::new())
                    } else {
                        (held_qty / 2.0, reason)
                    }
                }
                PositionAction::Hold => {
                    if decayed {
                        (held_qty, format!("signal_decay (orig {:.2}, pnl {:.1}%)", t.orig_score, pnl_pct))
                    } else {
                        (0.0, String::new())
                    }
                }
            }
        };

        if close_qty <= 0.0 {
            tracing::info!("Position mgmt: {} → HOLD (price ${:.2}, pnl {:.1}%)", ticker, current_price, pnl_pct);
            if stop_known {
                let want = crate::position_management::broker_stop_level(&conn, t.id, ticker, *entry_price);
                sync_broker_stop(&conn, &client, &creds, t.id, ticker, held_qty, want, live_stop.as_ref(), dry_run).await;
            }
            continue;
        }

        let mut is_full = (close_qty - held_qty).abs() < 1e-6;
        tracing::info!(
            "Position mgmt: {} → {} {:.4}/{:.4} sh @ ${:.2} (pnl {:.1}%) — {}",
            ticker, if is_full {"CLOSE"} else {"CLOSE_HALF"}, close_qty, held_qty,
            current_price, pnl_pct, reason
        );

        if dry_run {
            tracing::info!("Position mgmt: DRY_RUN — no order placed for {}", ticker);
            actions += 1;
            continue;
        }

        // A sell stop holds its shares: Alpaca refuses any other sell for them
        // until it is cancelled. If it cannot be cancelled, do not sell this
        // run — the stop is still protecting the position.
        let cleared = match stops::clear_stops(&client, &creds, ticker, known_stop).await {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!("Position mgmt: {} broker stop not cancelled ({}) — no sell this run", ticker, e);
                continue;
            }
        };
        // The stop may have fired while we were deciding or cancelling.
        if let Some(o) = cleared.iter().find(|o| o.sold().is_some()) {
            let (sold, price) = o.sold().unwrap_or((0.0, 0.0));
            let at = utc_to_local(o.filled_at.as_deref());
            let left = (held_qty - sold).max(0.0);
            book_stop_sale(&conn, t.id, *entry_price, price, sold, left, &at).ok();
            reason = format!("broker_stop (stop ${:.2})", o.stop_price);
            if left < 1e-6 {
                record_full_close(&conn, t.id, *entry_price, price, 0.0, &at, &reason).ok();
                let total: f64 = conn
                    .query_row("SELECT COALESCE(pnl, 0) FROM paper_trades WHERE id = ?1", [t.id], |r| r.get(0))
                    .unwrap_or(0.0);
                journal_close(&conn, t, &at, price, total, &reason);
                tracing::warn!("Position mgmt: {} stopped out by the broker @ ${:.2} while closing", ticker, price);
                actions += 1;
                continue;
            }
            held_qty = left;
            close_qty = left;
            is_full = true;
        }

        // --- Place the SELL order (live) ---
        let mut sent_order: Option<serde_json::Value> = None;
        // Deterministic client_order_id keyed on ticker+day+half-vs-full prevents a
        // launchd retry storm from double-selling (same bug class as the buy-side
        // ORCL stacking). A full and a half close on the same day get distinct ids.
        let today_key = chrono::Local::now().format("%Y%m%d").to_string();
        let exit_coid = format!("pulse-exit-{}-{}-{}", ticker, today_key, if is_full {"full"} else {"half"});
        let order = serde_json::json!({
            "symbol": ticker,
            "qty": format!("{:.9}", close_qty),
            "side": "sell",
            "type": "market",
            "time_in_force": "day",
            "client_order_id": exit_coid
        });
        let sent = client
            .post("https://paper-api.alpaca.markets/v2/orders")
            .header("APCA-API-KEY-ID", &alpaca_key)
            .header("APCA-API-SECRET-KEY", &alpaca_secret)
            .json(&order)
            .send()
            .await;
        let failure = match sent {
            Err(e) => Some(e.to_string()),
            Ok(r) if !r.status().is_success() => {
                let status = r.status();
                Some(format!("{} {}", status, r.text().await.unwrap_or_default()))
            }
            Ok(r) => match r.json::<serde_json::Value>().await {
                Ok(v) => {
                    sent_order = Some(v);
                    None
                }
                // Sent but the reply was unreadable: it may be live and
                // holding the shares, so a new stop would be refused anyway.
                Err(e) => {
                    tracing::warn!("Position mgmt: sell for {} sent, reply unreadable ({}) — reconciles next run", ticker, e);
                    actions += 1;
                    continue;
                }
            },
        };
        if let Some(why) = failure {
            tracing::warn!("Position mgmt: sell failed for {} — {}", ticker, why);
            // The stop was cancelled to free the shares; put it back so the
            // position is not left unprotected until the next run.
            if stop_known {
                let want = crate::position_management::broker_stop_level(&conn, t.id, ticker, *entry_price);
                sync_broker_stop(&conn, &client, &creds, t.id, ticker, held_qty, want, None, false).await;
            }
            continue;
        }
        let order_resp = sent_order.take().unwrap_or_default();
        let order_id = order_resp.get("id").and_then(|v| v.as_str()).unwrap_or("unknown").to_string();

        // Poll for fill (day orders may sit unfilled pre-market — that's fine).
        let fill = poll_fill(&client, &alpaca_key, &alpaca_secret, &order_id).await;

        crate::db::log_api_usage(&conn, "alpaca", "trading", "sell_order", 0, 0);

        let Some(fill) = fill else {
            // Order is live but unfilled. Do NOT mark closed — next run sees it
            // gone from Alpaca (or filled) and reconciles. Avoids ghost-closing.
            tracing::warn!(
                "Position mgmt: sell for {} not filled in 5s (likely pre-market) — \
                 order {} placed, will reconcile next run", ticker, order_id
            );
            actions += 1;
            continue;
        };

        // Filled, but Alpaca occasionally omits the average price. Fall back to
        // the mark we decided on rather than booking the exit at $0.
        let exit_price = if fill.price > 0.0 { fill.price } else { current_price };
        if is_full {
            let pnl_dollars = record_full_close(
                &conn, t.id, *entry_price, exit_price, close_qty, &fill.at_local, &reason,
            ).unwrap_or(0.0);
            let total: f64 = conn
                .query_row("SELECT COALESCE(pnl, 0) FROM paper_trades WHERE id = ?1", [t.id], |r| r.get(0))
                .unwrap_or(pnl_dollars);
            journal_close(&conn, t, &fill.at_local, exit_price, total, &reason);
            tracing::info!("Position mgmt: CLOSED {} @ ${:.2} ({:+.1}%, ${:+.2})",
                ticker, exit_price, ((exit_price - entry_price) / entry_price) * 100.0, total);
        } else {
            // Half close: mark half_closed_at, keep position open. position_size
            // is dollar-notional; halve it to reflect the reduced exposure.
            // The sold half's gain goes to realized_pnl, which the daily mark
            // and the final close both add on top of — `pnl` alone is
            // overwritten by calibration's mark every day.
            let realized_half = (exit_price - entry_price) * close_qty;
            conn.execute(
                "UPDATE paper_trades SET half_closed_at=?1, position_size = position_size / 2.0,
                    realized_pnl = COALESCE(realized_pnl, 0) + ?2,
                    pnl = COALESCE(realized_pnl, 0) + ?2, stop_order_id = NULL WHERE id=?3",
                rusqlite::params![now_dt, realized_half, t.id],
            ).ok();
            tracing::info!("Position mgmt: HALF-CLOSED {} @ ${:.2} (realized ${:+.2} on half)",
                ticker, exit_price, realized_half);
            // The stop was cancelled to free the shares; put it back on the half still held.
            let want = crate::position_management::broker_stop_level(&conn, t.id, ticker, *entry_price);
            sync_broker_stop(&conn, &client, &creds, t.id, ticker, held_qty - close_qty, want, None, false).await;
        }
        actions += 1;

        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }

    if dry_run && actions > 0 {
        tracing::warn!(
            "Position mgmt: {} exit action(s) were DRY-RUN only. Set EXIT_DRY_RUN=false to arm.",
            actions
        );
    }

    Ok(actions)
}

/// Snapshot the live portfolio state into the `portfolio_snapshots` table.
///
/// Runs once per day (UNIQUE constraint on `date` column makes it idempotent
/// via INSERT OR REPLACE). Pulls equity from Alpaca rather than aggregating
/// `paper_trades.pnl` because the DB can drift from Alpaca and we want the
/// snapshot to reflect ground truth, not our internal accounting.
///
/// MUST run AFTER Phase 14 (calibration) so any closes from this morning's
/// signal-decay or trailing-stop checks are reflected in the recorded equity.
pub(crate) async fn snapshot_portfolio(db_path: &Path) -> anyhow::Result<bool> {
    let alpaca_key = match std::env::var("ALPACA_API_KEY") {
        Ok(k) if !k.is_empty() => k,
        _ => {
            tracing::info!("Snapshot: skipping (ALPACA_API_KEY not set)");
            return Ok(false);
        }
    };
    let alpaca_secret = std::env::var("ALPACA_SECRET_KEY")
        .map_err(|_| anyhow::anyhow!("ALPACA_SECRET_KEY not set"))?;

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()?;

    let account: serde_json::Value = client
        .get("https://paper-api.alpaca.markets/v2/account")
        .header("APCA-API-KEY-ID", &alpaca_key)
        .header("APCA-API-SECRET-KEY", &alpaca_secret)
        .send()
        .await?
        .json()
        .await?;

    let portfolio_value: f64 = account.get("portfolio_value")
        .and_then(|v| v.as_str())
        .and_then(|s| s.parse().ok())
        .unwrap_or(0.0);

    if portfolio_value <= 0.0 {
        tracing::warn!("Snapshot: Alpaca returned zero portfolio_value, skipping");
        return Ok(false);
    }

    // Initial equity is whatever Alpaca says the account started with.
    // Alpaca paper accounts default to $100k; if the user has a different
    // baseline, the equity column would tell us — fall back to 100k.
    const INITIAL_EQUITY: f64 = 100_000.0;
    let total_pnl = portfolio_value - INITIAL_EQUITY;
    let total_pnl_pct = (total_pnl / INITIAL_EQUITY) * 100.0;

    let conn = rusqlite::Connection::open(db_path)?;

    let open_positions: i64 = conn.query_row(
        "SELECT COUNT(*) FROM paper_trades WHERE status = 'open'",
        [],
        |row| row.get(0),
    ).unwrap_or(0);

    // High-water mark = max(previous HWM, today's portfolio_value).
    let prev_hwm: f64 = conn.query_row(
        "SELECT MAX(high_water_mark) FROM portfolio_snapshots",
        [],
        |row| row.get::<_, Option<f64>>(0),
    ).unwrap_or(None).unwrap_or(INITIAL_EQUITY);
    let high_water_mark = prev_hwm.max(portfolio_value);
    let drawdown_pct = if high_water_mark > 0.0 {
        ((high_water_mark - portfolio_value) / high_water_mark) * 100.0
    } else {
        0.0
    };

    let today = chrono::Local::now().format("%Y-%m-%d").to_string();

    conn.execute(
        "INSERT INTO portfolio_snapshots
            (date, total_value, total_pnl, total_pnl_pct, open_positions, high_water_mark, drawdown_pct)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
         ON CONFLICT(date) DO UPDATE SET
             total_value = excluded.total_value,
             total_pnl = excluded.total_pnl,
             total_pnl_pct = excluded.total_pnl_pct,
             open_positions = excluded.open_positions,
             high_water_mark = excluded.high_water_mark,
             drawdown_pct = excluded.drawdown_pct",
        rusqlite::params![today, portfolio_value, total_pnl, total_pnl_pct, open_positions, high_water_mark, drawdown_pct],
    )?;

    tracing::info!(
        "Snapshot: ${:.0} equity, ${:+.0} PnL ({:+.2}%), {} open, HWM ${:.0}, DD {:.2}%",
        portfolio_value, total_pnl, total_pnl_pct, open_positions, high_water_mark, drawdown_pct
    );

    Ok(true)
}

#[cfg(test)]
mod scale_in_tests {
    use super::find_scale_in_candidates;
    use rusqlite::Connection;

    /// The two tables the join touches, with the real UNIQUE index on
    /// cross_signals — without it a fixture can write two rows for the same
    /// entity and day, which the live schema forbids.
    fn db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE paper_trades (
                 id INTEGER PRIMARY KEY AUTOINCREMENT,
                 ticker TEXT NOT NULL, entry_price REAL NOT NULL,
                 position_size REAL NOT NULL, confidence REAL NOT NULL,
                 status TEXT NOT NULL DEFAULT 'open',
                 pnl_pct REAL, original_compound_score REAL,
                 scale_in_count INTEGER DEFAULT 0,
                 order_status TEXT NOT NULL DEFAULT 'filled');
             CREATE TABLE cross_signals (
                 id INTEGER PRIMARY KEY AUTOINCREMENT,
                 entity_id INTEGER, ticker TEXT, compound_score REAL NOT NULL,
                 convergence_detected INTEGER DEFAULT 0,
                 computed_at TEXT DEFAULT (datetime('now')));
             CREATE UNIQUE INDEX idx_cs ON cross_signals(entity_id, date(computed_at));",
        )
        .unwrap();
        conn
    }

    /// One open, profitable, never-scaled position — the shape that qualifies.
    fn open_trade(conn: &Connection, ticker: &str, orig: f64) {
        conn.execute(
            "INSERT INTO paper_trades
               (ticker, entry_price, position_size, confidence, status,
                pnl_pct, original_compound_score, scale_in_count)
             VALUES (?1, 10.0, 1000.0, ?2, 'open', 5.0, ?2, 0)",
            rusqlite::params![ticker, orig],
        )
        .unwrap();
    }

    /// `days_ago` of 0 is today. Each row needs its own entity_id to coexist
    /// with another row for the same ticker on the same day.
    fn signal(conn: &Connection, entity_id: i64, ticker: &str, score: f64, days_ago: i64) {
        conn.execute(
            "INSERT INTO cross_signals
               (entity_id, ticker, compound_score, convergence_detected, computed_at)
             VALUES (?1, ?2, ?3, 1, date('now', ?4))",
            rusqlite::params![entity_id, ticker, score, format!("-{} days", days_ago)],
        )
        .unwrap();
    }

    #[test]
    fn a_fresh_strengthening_signal_still_scales_in() {
        let conn = db();
        open_trade(&conn, "AAA", 0.30);
        signal(&conn, 1, "AAA", 0.50, 0); // 0.50 > 0.30 * 1.2
        let got = find_scale_in_candidates(&conn);
        assert_eq!(got.len(), 1, "the case the feature exists for must still fire");
        assert_eq!(got[0].1, "AAA");
        assert!((got[0].2 - 0.30).abs() < 1e-9, "original score");
        assert!((got[0].3 - 0.50).abs() < 1e-9, "current score");
    }

    /// The AIRI bug, first half: April's peak triggering a July position.
    #[test]
    fn a_stale_peak_cannot_trigger_a_scale_in() {
        let conn = db();
        open_trade(&conn, "AIRI", 0.3408);
        // The real rows that fired it: three months old, far above the
        // 0.3408 * 1.2 = 0.409 threshold.
        signal(&conn, 1, "AIRI", 0.5606, 90);
        signal(&conn, 1, "AIRI", 0.5605, 91);
        assert!(
            find_scale_in_candidates(&conn).is_empty(),
            "a signal older than the position must not qualify it"
        );
    }

    /// A ticker with a stale peak AND a current-but-weak reading must read as
    /// weak. Without the freshest-row-only join, the peak still wins the join
    /// and the recency bound alone would not save it.
    #[test]
    fn the_freshest_reading_wins_even_when_an_older_one_is_stronger() {
        let conn = db();
        open_trade(&conn, "AIRI", 0.3408);
        signal(&conn, 1, "AIRI", 0.5606, 90); // stale peak, would qualify
        signal(&conn, 1, "AIRI", 0.3200, 0); // today, below threshold
        assert!(
            find_scale_in_candidates(&conn).is_empty(),
            "today's weak reading is the truth; the old peak is not"
        );
    }

    /// The AIRI bug, second half: `LIMIT 3` bounds rows, not rows per trade,
    /// so one position consumed all three slots and was bought three times in
    /// a single pass. `scale_in_count < 1` cannot stop it — the guard is read
    /// once, before any increment.
    #[test]
    fn one_trade_cannot_occupy_every_scale_in_slot() {
        let conn = db();
        open_trade(&conn, "AIRI", 0.3408);
        // Three qualifying rows, all current: two aliases today plus
        // yesterday's. Every one clears 0.409.
        signal(&conn, 1, "AIRI", 0.5606, 0);
        signal(&conn, 2, "AIRI", 0.5605, 0);
        signal(&conn, 1, "AIRI", 0.5602, 1);
        let got = find_scale_in_candidates(&conn);
        assert_eq!(
            got.len(),
            1,
            "one open position is one scale-in candidate, not one per signal row"
        );
    }

    /// Distinct positions still compete for the three slots — the dedup is per
    /// trade, not a global cap of one.
    fn three_tickers(conn: &Connection) {
        for (i, t) in ["AAA", "BBB", "CCC"].iter().enumerate() {
            open_trade(conn, t, 0.30);
            signal(conn, i as i64 + 1, t, 0.50, 0);
        }
    }

    #[test]
    fn separate_positions_each_get_a_slot() {
        let conn = db();
        three_tickers(&conn);
        assert_eq!(find_scale_in_candidates(&conn).len(), 3);
    }

    #[test]
    fn a_trade_that_already_scaled_in_is_excluded() {
        let conn = db();
        open_trade(&conn, "AAA", 0.30);
        signal(&conn, 1, "AAA", 0.50, 0);
        conn.execute("UPDATE paper_trades SET scale_in_count = 1", [])
            .unwrap();
        assert!(find_scale_in_candidates(&conn).is_empty());
    }

    #[test]
    fn a_losing_position_is_not_added_to() {
        let conn = db();
        open_trade(&conn, "AAA", 0.30);
        signal(&conn, 1, "AAA", 0.50, 0);
        conn.execute("UPDATE paper_trades SET pnl_pct = -6.96", [])
            .unwrap();
        assert!(find_scale_in_candidates(&conn).is_empty());
    }

    /// `original_compound_score` is nullable. The old projection read it as a
    /// bare f64, so a NULL turned into an Err that `filter_map` discarded —
    /// dropping a trade the WHERE clause had qualified via COALESCE.
    #[test]
    fn a_trade_with_no_stored_original_score_falls_back_to_confidence() {
        let conn = db();
        open_trade(&conn, "AAA", 0.30);
        conn.execute("UPDATE paper_trades SET original_compound_score = NULL", [])
            .unwrap();
        signal(&conn, 1, "AAA", 0.50, 0);
        let got = find_scale_in_candidates(&conn);
        assert_eq!(got.len(), 1, "confidence is the documented fallback");
        assert!((got[0].2 - 0.30).abs() < 1e-9, "fallback value is returned, not NULL");
    }

    /// A pending buy has no shares yet, and its pnl_pct is a mark against an
    /// estimated price. Adding to it would stack a second order on one that
    /// has not filled.
    #[test]
    fn a_pending_entry_is_not_added_to() {
        let conn = db();
        open_trade(&conn, "AAA", 0.30);
        signal(&conn, 1, "AAA", 0.50, 0);
        conn.execute("UPDATE paper_trades SET order_status = 'pending'", [])
            .unwrap();
        assert!(find_scale_in_candidates(&conn).is_empty());
    }
}

#[cfg(test)]
mod ledger_tests {
    use super::{apply_entry_settlement, book_stop_sale, classify_entry_order, exit_status, record_full_close, whole_shares, EntrySettlement};
    use rusqlite::Connection;
    use serde_json::json;

    /// The real schema, migrations included, so these tests also prove 036
    /// applies on top of everything before it.
    fn db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::run_migrations(&conn).unwrap();
        conn
    }

    fn insert(conn: &Connection, order_status: &str) -> i64 {
        conn.execute(
            "INSERT INTO paper_trades (ticker, direction, entry_price, entry_date, position_size,
                 confidence, signal_profile, alpaca_order_id, status, high_water_mark, order_status)
             VALUES ('NAVN', 'long', 20.00, '2026-09-28T00:27:46', 1900.0, 0.4, '{}', 'ord-1', 'open', 20.00, ?1)",
            [order_status],
        )
        .unwrap();
        conn.last_insert_rowid()
    }

    fn row(conn: &Connection, id: i64) -> (String, String, f64, f64, Option<f64>, Option<String>, Option<f64>) {
        conn.query_row(
            "SELECT status, order_status, entry_price, position_size, filled_qty, exit_reason, pnl
             FROM paper_trades WHERE id = ?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?)),
        )
        .unwrap()
    }

    #[test]
    fn an_order_waiting_for_the_open_is_still_pending() {
        for status in ["new", "accepted", "pending_new", "partially_filled"] {
            let order = json!({"status": status, "filled_qty": "0"});
            assert_eq!(classify_entry_order(&order), EntrySettlement::Pending, "{status}");
        }
    }

    #[test]
    fn a_filled_order_reports_its_real_price_and_shares() {
        let order = json!({"status": "filled", "filled_qty": "92.5", "filled_avg_price": "20.63",
                           "filled_at": "2026-09-28T13:33:41Z"});
        match classify_entry_order(&order) {
            EntrySettlement::Filled { price, qty, .. } => {
                assert!((price - 20.63).abs() < 1e-9);
                assert!((qty - 92.5).abs() < 1e-9);
            }
            other => panic!("expected Filled, got {other:?}"),
        }
    }

    #[test]
    fn a_cancelled_order_that_bought_nothing_is_dead_but_a_partial_fill_counts() {
        let dead = json!({"status": "canceled", "filled_qty": "0"});
        assert_eq!(classify_entry_order(&dead), EntrySettlement::Dead { status: "canceled".into() });
        let partial = json!({"status": "expired", "filled_qty": "3", "filled_avg_price": "10"});
        assert!(matches!(classify_entry_order(&partial), EntrySettlement::Filled { .. }));
    }

    /// The regression: an overnight buy used to be closed with NULL P&L when
    /// its position 404'd. Now the fill is written onto the same row, which
    /// stays open.
    #[test]
    fn settling_a_fill_keeps_the_trade_open_on_the_real_basis() {
        let conn = db();
        let id = insert(&conn, "pending");
        let fill = EntrySettlement::Filled { price: 20.63, qty: 92.5, at_local: "2026-09-28T09:33:41".into() };
        assert_eq!(apply_entry_settlement(&conn, id, &fill, "2026-09-28").unwrap(), 1);
        let (status, order_status, entry, size, qty, _, _) = row(&conn, id);
        assert_eq!(status, "open");
        assert_eq!(order_status, "filled");
        assert!((entry - 20.63).abs() < 1e-9);
        assert!((size - 20.63 * 92.5).abs() < 1e-6, "position_size is what was spent");
        assert_eq!(qty, Some(92.5));
        assert_eq!(
            apply_entry_settlement(&conn, id, &fill, "2026-09-28").unwrap(),
            0,
            "settling twice changes nothing"
        );
    }

    #[test]
    fn a_fill_without_a_price_keeps_the_estimate() {
        let conn = db();
        let id = insert(&conn, "pending");
        let fill = EntrySettlement::Filled { price: 0.0, qty: 92.5, at_local: "2026-09-28T09:33:41".into() };
        apply_entry_settlement(&conn, id, &fill, "2026-09-28").unwrap();
        let (_, order_status, entry, size, _, _, _) = row(&conn, id);
        assert_eq!(order_status, "filled");
        assert!((entry - 20.0).abs() < 1e-9);
        assert!((size - 1900.0).abs() < 1e-9);
    }

    #[test]
    fn a_dead_order_expires_the_row_without_pnl() {
        let conn = db();
        let id = insert(&conn, "pending");
        apply_entry_settlement(&conn, id, &EntrySettlement::Dead { status: "canceled".into() }, "2026-09-28").unwrap();
        let (status, _, _, _, _, reason, pnl) = row(&conn, id);
        assert_eq!(status, "expired");
        assert_eq!(reason.as_deref(), Some("entry_order_canceled"));
        assert_eq!(pnl, None);
    }

    #[test]
    fn settlement_never_touches_a_filled_row() {
        let conn = db();
        let id = insert(&conn, "filled");
        let dead = EntrySettlement::Dead { status: "canceled".into() };
        assert_eq!(apply_entry_settlement(&conn, id, &dead, "2026-09-28").unwrap(), 0);
        assert_eq!(row(&conn, id).0, "open");
    }

    #[test]
    fn a_full_close_books_the_shares_sold_and_its_reason() {
        let conn = db();
        let id = insert(&conn, "filled");
        let pnl = record_full_close(&conn, id, 20.0, 18.0, 90.0, "2026-09-29T10:00:00", "trailing_stop").unwrap();
        assert!((pnl + 180.0).abs() < 1e-9);
        let (status, _, _, _, _, reason, pnl_col) = row(&conn, id);
        assert_eq!(status, "stopped_out", "a stop is recorded as a stop-out, not a plain close");
        assert_eq!(reason.as_deref(), Some("trailing_stop"));
        assert!((pnl_col.unwrap() + 180.0).abs() < 1e-9);
    }

    /// The half's gain lives in realized_pnl. The final pnl adds the rest to it
    /// and ignores whatever daily mark `pnl` held just before the close.
    #[test]
    fn a_full_close_after_a_half_close_keeps_the_half_s_gain() {
        let conn = db();
        let id = insert(&conn, "filled");
        conn.execute("UPDATE paper_trades SET realized_pnl = 100.0, pnl = 350.0 WHERE id = ?1", [id])
            .unwrap();
        record_full_close(&conn, id, 20.0, 22.0, 50.0, "2026-09-29T10:00:00", "signal_decay").unwrap();
        let (pnl, realized): (f64, f64) = conn
            .query_row("SELECT pnl, realized_pnl FROM paper_trades WHERE id = ?1", [id], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap();
        assert!((pnl - 200.0).abs() < 1e-9, "100 from the half + 100 from the rest, not the stale 350 mark");
        assert!((realized - 200.0).abs() < 1e-9);
    }

    #[test]
    fn stops_are_stopped_out_and_everything_else_is_closed() {
        for r in ["hard_stop_loss (-15.2%)", "fixed_stop_loss (-10.1%, no ATR data)", "trailing_stop (x)", "broker_stop (stop $9.50)"] {
            assert_eq!(exit_status(r), "stopped_out", "{r}");
        }
        for r in ["signal_decay (orig 0.40)", "closed_between_runs", "manual", ""] {
            assert_eq!(exit_status(r), "closed", "{r}");
        }
    }

    /// The stop sells 90 whole shares at $18; the 0.5-share remainder sells
    /// later at $17.90. The trade's P&L is both sales, dated by the stop.
    #[test]
    fn a_broker_stop_sale_then_the_remainder_add_up() {
        let conn = db();
        let id = insert(&conn, "filled");
        let booked = book_stop_sale(&conn, id, 20.0, 18.0, 90.0, 0.5, "2026-10-05T10:31:00").unwrap();
        assert!((booked + 180.0).abs() < 1e-9);
        let (size, left, at): (f64, f64, String) = conn
            .query_row(
                "SELECT position_size, filled_qty, broker_stop_filled_at FROM paper_trades WHERE id = ?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert!((size - 1900.0 * 0.5 / 90.5).abs() < 1e-9, "position_size shrinks to the share left");
        assert!((left - 0.5).abs() < 1e-12);
        assert_eq!(at, "2026-10-05T10:31:00");

        record_full_close(&conn, id, 20.0, 17.9, 0.5, "2026-10-05T11:00:00", "broker_stop").unwrap();
        let (status, _, _, _, _, _, pnl) = row(&conn, id);
        assert_eq!(status, "stopped_out");
        assert!((pnl.unwrap() - (-180.0 - 1.05)).abs() < 1e-9);
    }

    #[test]
    fn a_stop_that_sold_everything_closes_without_double_counting() {
        let conn = db();
        let id = insert(&conn, "filled");
        book_stop_sale(&conn, id, 20.0, 18.0, 95.0, 0.0, "2026-10-05T10:31:00").unwrap();
        record_full_close(&conn, id, 20.0, 18.0, 0.0, "2026-10-05T10:31:00", "broker_stop").unwrap();
        let (status, _, _, _, _, _, pnl) = row(&conn, id);
        assert_eq!(status, "stopped_out");
        assert!((pnl.unwrap() + 190.0).abs() < 1e-9);
    }

    #[test]
    fn entries_buy_whole_shares_within_the_budget() {
        assert_eq!(whole_shares(1_000.0, 30.0), Some((33, 990.0)));
        assert_eq!(whole_shares(1_000.0, 1_000.0), Some((1, 1_000.0)));
        assert_eq!(whole_shares(999.0, 1_000.0), None);
        assert_eq!(whole_shares(1_000.0, 0.0), None);
        assert_eq!(whole_shares(f64::NAN, 10.0), None);
    }
}
