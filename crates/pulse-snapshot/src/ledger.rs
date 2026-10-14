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
    let sold_before: f64 = conn
        .query_row(
            "SELECT COALESCE(SUM(qty), 0) FROM trade_events
             WHERE trade_id = ?1 AND kind IN ('half_close', 'partial_sale', 'stop_sale')",
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
             CREATE TABLE trade_events (trade_id INTEGER, kind TEXT, qty REAL);",
        )
        .unwrap();
        conn
    }

    #[test]
    fn half_sold_at_a_big_gain_then_the_rest_small_reads_as_the_whole_trade() {
        let conn = db();
        // 100 shares bought at $10: 50 sold at $12.5 (+$125 booked), the last 50 sell at $10.2.
        conn.execute_batch("INSERT INTO paper_trades VALUES (1, 125.0); INSERT INTO trade_events VALUES (1, 'half_close', 50.0);").unwrap();
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
}
