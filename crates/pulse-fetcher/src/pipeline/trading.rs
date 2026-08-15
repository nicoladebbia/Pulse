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
        let num = |k: &str| {
            order
                .get(k)
                .and_then(|v| v.as_str())
                .and_then(|s| s.parse::<f64>().ok())
                .unwrap_or(0.0)
        };
        let at_local = order
            .get("filled_at")
            .and_then(|v| v.as_str())
            .and_then(|ft| chrono::DateTime::parse_from_rfc3339(ft).ok())
            .map(|dt| {
                dt.with_timezone(&chrono::Local)
                    .format("%Y-%m-%dT%H:%M:%S")
                    .to_string()
            })
            .unwrap_or_else(|| chrono::Local::now().format("%Y-%m-%dT%H:%M:%S").to_string());
        return Some(Fill {
            price: num("filled_avg_price"),
            qty: num("filled_qty"),
            at_local,
        });
    }
    None
}

/// Auto-execute paper trades when convergence signals are detected.
/// Only trades entities with tickers, not already held, with convergence_detected = true.
///
/// SAFETY GATE: this is hard-disabled by default. Both the entry path and the
/// scale-in path within this function only run when AUTO_TRADE_ENABLED=true is
/// set in the environment. Pulse is a news/intelligence app first — the trading
/// layer is dormant scaffolding that should not place real orders until a
/// 6-month auto-backtest history demonstrates a durable edge.
pub(crate) async fn auto_trade_on_convergence(db_path: &Path) -> anyhow::Result<usize> {
    // Hard kill switch. Default = OFF. Re-enable via `AUTO_TRADE_ENABLED=true`
    // in `.env` once the auto-backtest has shown a positive expectancy across
    // a meaningful window of resolved trades.
    let trading_enabled = std::env::var("AUTO_TRADE_ENABLED")
        .map(|v| v.eq_ignore_ascii_case("true") || v == "1")
        .unwrap_or(false);
    if !trading_enabled {
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
         ORDER BY l.compound_score DESC
         LIMIT 5"
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

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()?;

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
    // human review). 5% is restrictive enough to *bite* against the existing
    // $10k ENTRY_CAP at $100k portfolio sizes.
    const MAX_PER_TICKER_PCT: f64 = 0.05;
    let max_per_ticker_dollars = if portfolio_value > 0.0 {
        portfolio_value * MAX_PER_TICKER_PCT
    } else {
        f64::INFINITY // No portfolio value reported — fall back to ENTRY_CAP only.
    };

    let now = chrono::Local::now();
    let entry_datetime = now.format("%Y-%m-%dT%H:%M:%S").to_string();
    let mut traded = 0;

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

        let notional = match crate::position_sizing::entry_notional(buying_power, *score) {
            Some(n) => n,
            None => {
                tracing::info!("Auto-trade: skipping {} — buying power below entry floor", ticker);
                continue;
            }
        };

        // Concentration check: ask Alpaca for the current market value of any
        // existing position in this ticker. If proposed notional + existing
        // exposure would exceed MAX_PER_TICKER_PCT of portfolio_value, skip.
        let existing_exposure: f64 = match client
            .get(format!("https://paper-api.alpaca.markets/v2/positions/{}", ticker))
            .header("APCA-API-KEY-ID", &alpaca_key)
            .header("APCA-API-SECRET-KEY", &alpaca_secret)
            .send()
            .await
        {
            Ok(r) if r.status().is_success() => {
                r.json::<serde_json::Value>().await.ok()
                    .and_then(|v| v.get("market_value")
                        .and_then(|m| m.as_str())
                        .and_then(|s| s.parse::<f64>().ok()))
                    .unwrap_or(0.0)
            }
            // 404 = no existing position, treat as 0 exposure. Other errors:
            // be conservative and treat as 0 (the trade still gets capped by
            // notional itself, just won't account for hidden exposure).
            _ => 0.0,
        };

        if existing_exposure + notional > max_per_ticker_dollars {
            tracing::warn!(
                "Auto-trade: blocking {} ({}) — would push exposure to ${:.0} (existing ${:.0} + ${:.0}), cap is ${:.0} ({:.0}% of ${:.0} portfolio)",
                name, ticker, existing_exposure + notional, existing_exposure, notional,
                max_per_ticker_dollars, MAX_PER_TICKER_PCT * 100.0, portfolio_value
            );
            continue;
        }

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

        let order = serde_json::json!({
            "symbol": ticker,
            "notional": format!("{:.2}", notional),
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

            // Poll for fill — market orders fill within seconds
            let fill = poll_fill(&client, &alpaca_key, &alpaca_secret, &order_id).await;
            if fill.is_none() {
                tracing::warn!("Auto-trade: order {} not filled after 5s", order_id);
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
                "INSERT INTO paper_trades (entity_id, ticker, direction, entry_price, entry_date, position_size, confidence, signal_profile, alpaca_order_id, status, high_water_mark, original_compound_score)
                 VALUES (?1, ?2, 'long', ?3, ?4, ?5, ?6, ?7, ?8, 'open', ?3, ?6)",
                rusqlite::params![entity_id, ticker, filled_price, fill_time, notional, score, signal_profile.to_string(), order_id],
            ) {
                tracing::warn!("Auto-trade: failed to record trade for {}: {}", ticker, e);
            }

            tracing::info!("Auto-trade: placed order {} for {} (${:.2} @ ${:.2}, qty {:.4})", order_id, ticker, notional, filled_price, filled_qty);
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
    let scale_in_candidates: Vec<(i64, String, f64, f64)> = conn.prepare(
        "SELECT pt.id, pt.ticker, pt.original_compound_score, cs.compound_score
         FROM paper_trades pt
         JOIN cross_signals cs ON cs.ticker = pt.ticker
         WHERE pt.status = 'open'
           AND pt.pnl_pct > 0.0
           AND COALESCE(pt.scale_in_count, 0) < 1
           AND cs.compound_score > COALESCE(pt.original_compound_score, pt.confidence) * 1.2
           AND cs.convergence_detected = 1
         ORDER BY cs.compound_score DESC
         LIMIT 3"
    ).ok()
    .map(|mut stmt| {
        stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)))
            .ok()
            .map(|rows| rows.filter_map(|r| r.ok()).collect::<Vec<_>>())
            .unwrap_or_default()
    })
    .unwrap_or_default();

    for (trade_id, ticker, _old_score, new_score) in &scale_in_candidates {
        let scale_notional = match crate::position_sizing::scale_in_notional(buying_power) {
            Some(n) => n,
            None => {
                tracing::info!("Scale-in: skipping {} — buying power below floor", ticker);
                continue;
            }
        };

        tracing::info!("Scale-in: {} — score increased to {:.2}, adding ${:.2}", ticker, new_score, scale_notional);

        let order = serde_json::json!({
            "symbol": ticker,
            "notional": format!("{:.2}", scale_notional),
            "side": "buy",
            "type": "market",
            "time_in_force": "day"
        });

        let resp = client
            .post("https://paper-api.alpaca.markets/v2/orders")
            .header("APCA-API-KEY-ID", &alpaca_key)
            .header("APCA-API-SECRET-KEY", &alpaca_secret)
            .json(&order)
            .send()
            .await?;

        if resp.status().is_success() {
            conn.execute(
                "UPDATE paper_trades SET scale_in_count = COALESCE(scale_in_count, 0) + 1,
                 position_size = position_size + ?1 WHERE id = ?2",
                rusqlite::params![scale_notional, trade_id],
            ).ok();
            tracing::info!("Scale-in: added ${:.2} to {} position", scale_notional, ticker);
            traded += 1;
        }

        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }

    Ok(traded)
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
///
/// Returns the number of exit ACTIONS executed (orders placed, or in dry-run,
/// actions that WOULD have been placed).
/// Fetch the most recent FILLED sell fill price for a ticker from Alpaca.
/// Used to record real exit P&L when a position closed between runs (e.g. a
/// pre-market `day` order that filled after the open and is gone by next run).
/// Returns (exit_price, filled_qty) or None.
pub(crate) async fn latest_filled_sell(
    client: &reqwest::Client, key: &str, secret: &str, ticker: &str,
) -> Option<(f64, f64)> {
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
    let arr = orders.as_array()?;
    for o in arr {
        if o.get("status").and_then(|v| v.as_str()) == Some("filled") {
            let price = o.get("filled_avg_price")
                .and_then(|v| v.as_str()).and_then(|s| s.parse::<f64>().ok());
            let qty = o.get("filled_qty")
                .and_then(|v| v.as_str()).and_then(|s| s.parse::<f64>().ok());
            if let (Some(p), Some(q)) = (price, qty) {
                if p > 0.0 {
                    return Some((p, q));
                }
            }
        }
    }
    None
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

pub(crate) async fn manage_open_positions(db_path: &Path) -> anyhow::Result<usize> {
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

    let conn = rusqlite::Connection::open(db_path)?;
    // Ensure schema is current (idempotent) — needed when this runs standalone
    // via `--mode manage-positions` without the full pipeline's migration pass.
    crate::db::run_migrations(&conn)?;

    // Pull every open position the DB tracks. entry_price/entry_date drive the
    // exit math; original_compound_score drives signal-decay; half_closed_at
    // gates CloseHalf from re-triggering.
    #[allow(clippy::type_complexity)]
    let open: Vec<(i64, String, f64, String, f64, Option<String>, f64)> = {
        let mut stmt = conn.prepare(
            "SELECT id, ticker, entry_price, entry_date,
                    COALESCE(original_compound_score, confidence, 0.30),
                    half_closed_at, position_size
             FROM paper_trades WHERE status = 'open'"
        )?;
        stmt.query_map([], |row| Ok((
            row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?,
            row.get::<_, f64>(4).unwrap_or(0.30),
            row.get::<_, Option<String>>(5).unwrap_or(None),
            row.get::<_, f64>(6).unwrap_or(0.0),
        )))?
        .filter_map(|r| r.ok())
        .collect()
    };

    if open.is_empty() {
        return Ok(0);
    }

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()?;

    let today = chrono::Local::now().format("%Y-%m-%d").to_string();
    let now_dt = chrono::Local::now().format("%Y-%m-%dT%H:%M:%S").to_string();
    let mut actions = 0usize;

    for (trade_id, ticker, entry_price, entry_date, orig_score, half_closed_at, position_size) in &open {
        // Ground truth from Alpaca: current_price + held qty. If Alpaca has no
        // position (already liquidated elsewhere), reconcile the DB to closed.
        let pos: Option<serde_json::Value> = match client
            .get(format!("https://paper-api.alpaca.markets/v2/positions/{}", ticker))
            .header("APCA-API-KEY-ID", &alpaca_key)
            .header("APCA-API-SECRET-KEY", &alpaca_secret)
            .send()
            .await
        {
            Ok(r) if r.status().is_success() => r.json().await.ok(),
            Ok(r) if r.status().as_u16() == 404 => {
                // Alpaca has no such position but DB says open — it closed between
                // runs (e.g. a pre-market `day` sell that filled after the open).
                // Record REAL P&L from the last filled sell so this exit shows up
                // in analytics — a null-P&L close gets silently dropped by the
                // win-rate / Sharpe queries (analytics.rs filters pnl_pct NOT NULL).
                if !dry_run {
                    if let Some((exit_p, _)) =
                        latest_filled_sell(&client, &alpaca_key, &alpaca_secret, ticker).await
                    {
                        let pnl_pct = ((exit_p - entry_price) / entry_price) * 100.0;
                        // We don't know the exact qty that was held at fill time;
                        // pnl_pct is exact, pnl dollars uses entry notional as a proxy:
                        // implied shares = position_size (dollar notional) / entry_price,
                        // times the per-share gain. Without this, pnl was left NULL/0 and
                        // analytics silently dropped the close (pnl_pct NOT NULL filter
                        // still passed, but win-rate $ aggregates were wrong).
                        let pnl_dollars = (exit_p - entry_price) * (position_size / entry_price);
                        conn.execute(
                            "UPDATE paper_trades SET status='closed', exit_price=?1,
                                exit_date=?2, pnl=?3, pnl_pct=?4 WHERE id=?5",
                            rusqlite::params![exit_p, today, pnl_dollars, pnl_pct, trade_id],
                        ).ok();
                        tracing::warn!(
                            "Position mgmt: {} closed between runs — reconciled @ ${:.2} ({:+.1}%)",
                            ticker, exit_p, pnl_pct
                        );
                    } else {
                        // No fill record found — close without P&L rather than leave
                        // a phantom-open row, but log it loudly for review.
                        conn.execute(
                            "UPDATE paper_trades SET status='closed', exit_date=?1 WHERE id=?2",
                            rusqlite::params![today, trade_id],
                        ).ok();
                        tracing::warn!(
                            "Position mgmt: {} absent on Alpaca, no sell fill found — closed with NULL pnl (review)",
                            ticker
                        );
                    }
                } else {
                    tracing::warn!(
                        "Position mgmt: {} open in DB but absent on Alpaca — would reconcile (dry-run)",
                        ticker
                    );
                }
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
        let held_qty: f64 = pos.get("qty")
            .and_then(|v| v.as_str()).and_then(|s| s.parse().ok()).unwrap_or(0.0);

        if current_price <= 0.0 || held_qty <= 0.0 {
            tracing::warn!("Position mgmt: {} has no valid price/qty from Alpaca, skipping", ticker);
            continue;
        }

        // Signal decay check first — cheapest, and a decayed thesis means exit
        // regardless of price action.
        let decayed = crate::position_management::check_signal_decay(&conn, ticker, *orig_score);

        let action = crate::position_management::evaluate_position(
            &conn, *trade_id, ticker, *entry_price, current_price,
        );

        // Decide final action: signal decay forces a full close (overrides Hold).
        use crate::position_management::PositionAction;
        let pnl_pct = ((current_price - entry_price) / entry_price) * 100.0;
        let (close_qty, reason): (f64, String) = match action {
            PositionAction::CloseAll { reason } => (held_qty, reason),
            PositionAction::CloseHalf { reason } => {
                if half_closed_at.is_some() {
                    // Already took profit on half — don't keep peeling. Hold the rest.
                    tracing::info!("Position mgmt: {} CloseHalf suppressed (already half-closed)", ticker);
                    (0.0, String::new())
                } else {
                    (held_qty / 2.0, reason)
                }
            }
            PositionAction::Hold => {
                if decayed {
                    (held_qty, format!("signal_decay (orig {:.2}, pnl {:.1}%)", orig_score, pnl_pct))
                } else {
                    (0.0, String::new())
                }
            }
        };

        if close_qty <= 0.0 {
            tracing::info!("Position mgmt: {} → HOLD (price ${:.2}, pnl {:.1}%)", ticker, current_price, pnl_pct);
            continue;
        }

        let is_full = (close_qty - held_qty).abs() < 1e-6;
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

        // --- Place the SELL order (live) ---
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
        let resp = client
            .post("https://paper-api.alpaca.markets/v2/orders")
            .header("APCA-API-KEY-ID", &alpaca_key)
            .header("APCA-API-SECRET-KEY", &alpaca_secret)
            .json(&order)
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            tracing::warn!("Position mgmt: sell failed for {} — {} {}", ticker, status, body);
            continue;
        }
        let order_resp: serde_json::Value = resp.json().await?;
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
            let pnl_pct_final = ((exit_price - entry_price) / entry_price) * 100.0;
            let pnl_dollars = (exit_price - entry_price) * held_qty;
            conn.execute(
                "UPDATE paper_trades SET status='closed', exit_price=?1, exit_date=?2,
                    pnl=?3, pnl_pct=?4 WHERE id=?5",
                rusqlite::params![exit_price, now_dt, pnl_dollars, pnl_pct_final, trade_id],
            ).ok();
            // Journal the close. This is the SINGLE exit authority (Phase 13.6),
            // so it owns journal generation — calibration is measure-only and no
            // longer closes trades. Without this, the Portfolio exit-reasons
            // feature stops getting entries once auto-exits arm.
            let reason_status = if reason.starts_with("signal_decay") { "closed" } else { "stopped_out" };
            crate::position_management::generate_trade_journal(
                &conn, *trade_id, ticker, entry_date, &today,
                *entry_price, exit_price, *position_size, pnl_pct_final, pnl_dollars, reason_status,
            );
            tracing::info!("Position mgmt: CLOSED {} @ ${:.2} ({:+.1}%, ${:+.2})",
                ticker, exit_price, pnl_pct_final, pnl_dollars);
        } else {
            // Half close: mark half_closed_at, keep position open. position_size
            // is dollar-notional; halve it to reflect the reduced exposure.
            // Record the realized gain on the SOLD half into pnl (cumulative) so
            // analytics doesn't undercount profit-target winners; the remaining
            // half's P&L is booked at full close.
            let realized_half = (exit_price - entry_price) * close_qty;
            conn.execute(
                "UPDATE paper_trades SET half_closed_at=?1, position_size = position_size / 2.0,
                    pnl = COALESCE(pnl, 0) + ?2 WHERE id=?3",
                rusqlite::params![now_dt, realized_half, trade_id],
            ).ok();
            tracing::info!("Position mgmt: HALF-CLOSED {} @ ${:.2} (realized ${:+.2} on half)",
                ticker, exit_price, realized_half);
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
