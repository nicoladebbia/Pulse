//! Ledger arithmetic shared by the fetcher (which books exits) and the app (Close button).

/// The return of the WHOLE trade in percent: all booked dollars over everything the trade
/// held at cost. The last leg's own percentage (what this used to store) made a trade that
/// sold half at +25% and the rest at +2% read +2% next to a large dollar profit, which fed the
/// win rate, the edge used for sizing and the SPY comparison. `None` when the earlier legs are
/// not in `trade_events` (old rows): the caller then keeps the last-leg figure.
pub fn whole_trade_pnl_pct(conn: &rusqlite::Connection, trade_id: i64, entry_price: f64, qty_last: f64, pnl_last: f64) -> Option<f64> {
    let realized_before: f64 = conn
        .query_row("SELECT COALESCE(realized_pnl, 0) FROM paper_trades WHERE id = ?1", [trade_id], |r| r.get(0))
        .ok()?;
    // Shares sold before the last leg. A `resync` rebuilds `realized_pnl` from ALL the broker's
    // sells since entry (its detail lists them), so it replaces every sale event logged before
    // it; sales booked after it add on top. Summing only the sale events would miss a sale the
    // resync found and overstate the percentage.
    let sold_before: f64 = conn
        .query_row(
            "SELECT
                COALESCE((SELECT SUM(json_extract(j.value, '$[0]'))
                          FROM json_each((SELECT CASE WHEN json_valid(detail) THEN detail END
                                          FROM trade_events WHERE trade_id = ?1 AND kind = 'resync'
                                          ORDER BY id DESC LIMIT 1), '$.sells') j), 0)
              + COALESCE((SELECT SUM(qty) FROM trade_events
                          WHERE trade_id = ?1 AND kind IN ('half_close', 'partial_sale', 'stop_sale')
                            AND id > COALESCE((SELECT MAX(id) FROM trade_events
                                               WHERE trade_id = ?1 AND kind = 'resync'), 0)), 0)",
            [trade_id],
            |r| r.get(0),
        )
        .ok()?;
    if sold_before <= 0.0 {
        // No earlier leg recorded (a single-leg trade, or an old row): keep the last-leg figure.
        return None;
    }
    let cost = entry_price * (qty_last + sold_before);
    (cost > 0.0 && entry_price > 0.0).then(|| (realized_before + pnl_last) / cost * 100.0)
}

#[cfg(test)]
mod tests {
    use super::whole_trade_pnl_pct;
    use rusqlite::Connection;

    fn db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE paper_trades (id INTEGER PRIMARY KEY, realized_pnl REAL);
             CREATE TABLE trade_events (id INTEGER PRIMARY KEY AUTOINCREMENT, trade_id INTEGER, kind TEXT, qty REAL, detail TEXT);",
        )
        .unwrap();
        conn
    }

    #[test]
    fn half_sold_at_a_big_gain_then_the_rest_small_reads_as_the_whole_trade() {
        let conn = db();
        // 100 shares bought at $10: 50 sold at $12.5 (+$125 booked), the last 50 sell at $10.2.
        conn.execute_batch("INSERT INTO paper_trades VALUES (1, 125.0); INSERT INTO trade_events (trade_id, kind, qty) VALUES (1, 'half_close', 50.0);").unwrap();
        let pct = whole_trade_pnl_pct(&conn, 1, 10.0, 50.0, 10.0).unwrap();
        assert!((pct - 13.5).abs() < 1e-9, "+$135 on $1,000 is 13.5%, not the last leg's 2%: {pct}");
    }

    #[test]
    fn a_single_leg_trade_or_an_old_row_has_no_whole_trade_figure() {
        let conn = db();
        conn.execute_batch("INSERT INTO paper_trades VALUES (1, NULL);").unwrap();
        assert_eq!(whole_trade_pnl_pct(&conn, 1, 10.0, 100.0, 20.0), None);
        assert_eq!(whole_trade_pnl_pct(&conn, 99, 10.0, 100.0, 20.0), None, "unknown trade id");
    }

    #[test]
    fn a_sale_found_by_a_resync_counts_in_the_cost_of_the_whole_trade() {
        let conn = db();
        // 100 shares at $10. 50 sold at $12.5 (logged), then a resync finds 20 more sold at $11
        // that were never booked: the broker's sells are 50 + 20, realized is 125 + 20.
        conn.execute_batch(
            "INSERT INTO paper_trades VALUES (1, 145.0);
             INSERT INTO trade_events (trade_id, kind, qty) VALUES (1, 'half_close', 50.0);
             INSERT INTO trade_events (trade_id, kind, qty, detail) VALUES (1, 'resync', 30.0, '{\"sells\":[[50.0,12.5],[20.0,11.0]],\"held\":30.0}');",
        )
        .unwrap();
        // The last 30 shares sell at $10: +$0. $145 on 100 shares ($1,000) is 14.5%.
        let pct = whole_trade_pnl_pct(&conn, 1, 10.0, 30.0, 0.0).unwrap();
        assert!((pct - 14.5).abs() < 1e-9, "{pct}");
    }

    #[test]
    fn sales_booked_after_a_resync_add_to_the_ones_it_found() {
        let conn = db();
        // The resync found 20 sold at $11 ($20); later 40 more sold at $12 were booked ($80).
        conn.execute_batch(
            "INSERT INTO paper_trades VALUES (1, 100.0);
             INSERT INTO trade_events (trade_id, kind, qty, detail) VALUES (1, 'resync', 80.0, '{\"sells\":[[20.0,11.0]],\"held\":80.0}');
             INSERT INTO trade_events (trade_id, kind, qty) VALUES (1, 'half_close', 40.0);",
        )
        .unwrap();
        // 40 left sell at $10: +$0 -> $100 on $1,000.
        let pct = whole_trade_pnl_pct(&conn, 1, 10.0, 40.0, 0.0).unwrap();
        assert!((pct - 10.0).abs() < 1e-9, "{pct}");
    }

    #[test]
    fn a_malformed_resync_detail_falls_back_instead_of_failing() {
        let conn = db();
        conn.execute_batch(
            "INSERT INTO paper_trades VALUES (1, 50.0);
             INSERT INTO trade_events (trade_id, kind, qty, detail) VALUES (1, 'resync', 10.0, 'not json');
             INSERT INTO trade_events (trade_id, kind, qty) VALUES (1, 'half_close', 10.0);",
        )
        .unwrap();
        let pct = whole_trade_pnl_pct(&conn, 1, 10.0, 10.0, 0.0).unwrap();
        assert!((pct - 25.0).abs() < 1e-9, "$50 on 20 shares ($200): {pct}");
    }
}
