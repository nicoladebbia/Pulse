#!/usr/bin/env python3
"""The weekly trading scoreboard: what Pulse bought and sold, measured against
the S&P 500, and what its own learning report says about signals, stops and
filters.

It only reads, and only the team's copy of the database. No session and no AI:
every number is a query or comes from the report Pulse already wrote, so it
can be checked by hand. It changes nothing and proposes nothing.

Standard library only."""

from __future__ import annotations

import datetime as dt
import json
import math
import sqlite3
import statistics
import urllib.parse
from pathlib import Path

# The trades that count, by the same rule as the go-live checklist
# (`load` in src-tauri/src/services/go_live.rs): no merged ledger fragments,
# no trades marked as tests, no close without a price or a booked result.
REAL = """pt.status IN ('closed', 'stopped_out')
  AND COALESCE(pt.exit_price, 0) > 0
  AND COALESCE(pt.pnl, pt.realized_pnl) IS NOT NULL
  AND COALESCE(pt.exit_reason, '') NOT LIKE 'merged_into%'
  AND json_extract(CASE WHEN json_valid(pt.entry_context) THEN pt.entry_context
                   ELSE '{}' END, '$.event.test') IS NULL"""
NOT_A_FRAGMENT = """COALESCE(pt.exit_reason, '') NOT LIKE 'merged_into%'
  AND json_extract(CASE WHEN json_valid(pt.entry_context) THEN pt.entry_context
                   ELSE '{}' END, '$.event.test') IS NULL"""
STOPS = ("trailing_stop", "broker_stop", "hard_stop", "fixed_stop")
# How a filter is said in words (`reason` in trade_decisions).
REASONS = {
    "universe": "not in the tradable universe",
    "earnings": "earnings too close",
    "too_thin": "too little trading volume",
    "too_calm": "the stock moves too little",
    "price": "price under $5",
    "industry": "too much in one industry",
    "max_positions": "the portfolio is full",
    "untracked_holding": "already held outside Pulse's books",
    "no_candidates": "no stock reached a buy grade",
    "no_cash": "no cash left",
    "broker_unreadable": "the broker could not be read",
    "bars_refused": "the price history was refused",
    "bought": "bought",
}
EXITS = {
    "trailing_stop": "trailing stop",
    "broker_stop": "stop at the broker",
    "hard_stop": "hard stop",
    "fixed_stop": "fixed stop",
    "signal_decay": "signal faded",
    "max_hold": "held its maximum time",
    "closed_between_runs": "closed between runs",
    "manual": "closed by hand",
}
LUCK_T = 2.0  # below this a result is not told apart from luck
ENOUGH = 10  # trades before an average is worth a word about the rule
TEXT_LIMIT = 3800  # one Telegram text (its limit is 4096)


def connect(path: Path) -> sqlite3.Connection:
    """The copy, read-only. `immutable` also keeps SQLite from writing its
    side files next to it."""
    where = urllib.parse.quote(str(path))  # a `?` or `#` in a folder name
    return sqlite3.connect(f"file:{where}?mode=ro&immutable=1", uri=True)


def _has(con: sqlite3.Connection, table: str, *columns: str) -> bool:
    """Whether this database has the table, with these columns."""
    have = {r[1] for r in con.execute(f"PRAGMA table_info({table})")}
    return bool(have) and set(columns) <= have


def _rows(con: sqlite3.Connection, sql: str, *args) -> list[tuple]:
    """Rows of an extra (the decisions, the reports), or none when this
    database is older than the table or column asked for. Anything else (a
    damaged file, a locked one) is an error. The trades themselves are never
    read through this: a board that could not read them would say "sold
    nothing" about a database full of trades."""
    try:
        return con.execute(sql, args).fetchall()
    except sqlite3.OperationalError as e:
        if "no such table" in str(e) or "no such column" in str(e):
            return []
        raise


def t_stat(values: list[float]) -> float | None:
    """How far the average is from zero, in standard errors. None under 2 values
    or when they are all the same."""
    if len(values) < 2:
        return None
    sd = statistics.stdev(values)
    return statistics.fmean(values) / (sd / math.sqrt(len(values))) if sd else None


def build(con: sqlite3.Connection, today: dt.date, days: int = 7) -> dict:
    """The scoreboard as plain data, for the `days` up to and including `today`."""
    since = (today - dt.timedelta(days=days - 1)).isoformat()
    until = today.isoformat()
    # The trades are read strictly: a database without the trades table, or
    # too old for one of these columns, is an error and never an empty week.
    # The reviews are an extra. Without them the trades still count and only
    # the comparison with the S&P 500 is missing.
    if _has(con, "trade_reviews", "trade_id", "excess_pct", "exit_kind"):
        excess_of, kind_of = "tr.excess_pct", "COALESCE(tr.exit_kind, '')"
        reviews = "LEFT JOIN trade_reviews tr ON tr.trade_id = pt.id"
    else:
        excess_of, kind_of, reviews = "NULL", "''", ""
    bought = con.execute(
        f"""SELECT pt.ticker, COALESCE(pt.direction, 'long'), COALESCE(pt.entry_trigger, '')
            FROM paper_trades pt
            WHERE substr(pt.entry_date, 1, 10) BETWEEN ? AND ?
              AND pt.status IN ('open', 'closed', 'stopped_out')
              AND COALESCE(pt.order_status, 'filled') = 'filled' AND {NOT_A_FRAGMENT}
            ORDER BY pt.entry_date""",
        (since, until),
    ).fetchall()
    sold = con.execute(
        f"""SELECT pt.ticker, COALESCE(pt.pnl, pt.realized_pnl), pt.pnl_pct, {excess_of},
                   {kind_of}, COALESCE(pt.direction, 'long')
            FROM paper_trades pt {reviews}
            WHERE substr(pt.exit_date, 1, 10) BETWEEN ? AND ? AND {REAL}
            ORDER BY pt.exit_date""",
        (since, until),
    ).fetchall()
    every = con.execute(
        f"""SELECT COALESCE(pt.pnl, pt.realized_pnl), {excess_of}
            FROM paper_trades pt {reviews}
            WHERE {REAL}"""
    ).fetchall()
    excess = [e for _, e in every if e is not None]
    held, bare = con.execute(
        """SELECT COUNT(*), COALESCE(SUM(COALESCE(stop_order_id, '') = ''), 0)
           FROM paper_trades WHERE status = 'open' AND order_status = 'filled'"""
    ).fetchone()
    # Every run writes one row per stock it looked at, and the runs of one day
    # repeat each other: stocks are counted once, runs are counted as runs.
    turned_down = _rows(
        con,
        """SELECT reason, COUNT(DISTINCT ticker) FROM trade_decisions
           WHERE substr(run_at, 1, 10) BETWEEN ? AND ? AND outcome = 'skipped'
           GROUP BY reason ORDER BY 2 DESC, reason""",
        since,
        until,
    )
    ended_early = _rows(
        con,
        """SELECT reason, COUNT(DISTINCT run_at) FROM trade_decisions
           WHERE substr(run_at, 1, 10) BETWEEN ? AND ? AND outcome = 'run_stopped'
           GROUP BY reason ORDER BY 2 DESC, reason""",
        since,
        until,
    )
    runs = _rows(
        con,
        """SELECT COUNT(DISTINCT run_at) FROM trade_decisions
           WHERE substr(run_at, 1, 10) BETWEEN ? AND ?""",
        since,
        until,
    )
    # The ten days after each sale, by exit: the report gives their average
    # but not how many values are in it, nor how much they scatter.
    after: dict[str, list[float]] = {}
    for kind, value in _rows(
        con,
        "SELECT exit_kind, after10_pct FROM trade_reviews WHERE after10_pct IS NOT NULL",
    ):
        after.setdefault(kind, []).append(value)
    report, report_day = {}, ""
    latest = _rows(
        con,
        "SELECT computed_at, body FROM learning_reports ORDER BY computed_at DESC LIMIT 1",
    )
    if latest:
        report_day = str(latest[0][0])[:10]
        try:
            report = json.loads(latest[0][1])
        except (TypeError, ValueError):
            report = {}
    report = report if isinstance(report, dict) else {}
    moved = []
    for day, body in _rows(
        con,
        """SELECT computed_at, body FROM learning_reports
           WHERE weights_applied > 0 AND substr(computed_at, 1, 10) BETWEEN ? AND ?
           ORDER BY computed_at""",
        since,
        until,
    ):
        try:
            changes = json.loads(body).get("weight_changes") or []
        except (TypeError, ValueError, AttributeError):
            changes = []
        moved += [dict(c, day=str(day)[:10]) for c in changes if isinstance(c, dict)]
    return {
        "since": since,
        "until": until,
        "bought": [{"ticker": t, "direction": d, "trigger": g} for t, d, g in bought],
        "sold": [
            {
                "ticker": t,
                "pnl": p,
                "pnl_pct": pct,
                "excess": e,
                "exit": k,
                "direction": d,
            }
            for t, p, pct, e, k, d in sold
        ],
        "open": {"held": held or 0, "without_stop": bare or 0},
        "all": {
            "trades": len(every),
            "pnl": sum(p for p, _ in every),
            "measured": len(excess),
            "beat": sum(1 for e in excess if e > 0),
            "avg_excess": statistics.fmean(excess) if excess else None,
            "t": t_stat(excess),
        },
        "skipped": [{"reason": r, "stocks": n} for r, n in turned_down],
        "ended_early": [{"reason": r, "runs": n} for r, n in ended_early],
        "runs": runs[0][0] if runs else 0,
        "report_day": report_day,
        "headline": [str(h) for h in report.get("headline") or []],
        "exits": [
            dict(
                e,
                after10_n=len(after.get(e.get("exit_kind"), [])),
                after10_t=t_stat(after.get(e.get("exit_kind"), [])),
            )
            for e in report.get("exits") or []
            if isinstance(e, dict)
        ],
        "gates": [g for g in report.get("gates") or [] if isinstance(g, dict)],
        "weights_moved": moved,
    }


def _money(x: float) -> str:
    return f"{'−' if x < 0 else '+'}${abs(x):,.0f}"


def _num(x: float) -> str:
    return f"{'−' if x < 0 else ''}{abs(x):.1f}"


def _pct(x: float | None, unit: str = "%") -> str:
    return "?" if x is None else f"{'−' if x < 0 else '+'}{abs(x):.1f}{unit}"


def _day(iso: str) -> str:
    try:
        d = dt.date.fromisoformat(iso)
    except ValueError:
        return iso
    return f"{d.day} {d:%b}"


def _more(shown: int, total: int) -> str:
    return f", and {total - shown} more" if total > shown else ""


def _reason(reason: str) -> tuple[str, bool]:
    """A filter in words, and whether the row is about shorts."""
    short = reason.endswith(" (short)")
    key = reason.removesuffix(" (short)")
    return REASONS.get(key, key.replace("_", " ")), short


def as_text(board: dict) -> str:
    """The whole scoreboard in plain words (`shorten` fits it into one text)."""
    out = [
        f"Pulse trading, {_day(board['since'])} to {_day(board['until'])} (paper money)"
    ]

    out.append("\nThis week")
    bought, sold = board["bought"], board["sold"]
    if bought:
        names = ", ".join(
            b["ticker"] + (" (short)" if b["direction"] == "short" else "")
            for b in bought[:12]
        )
        out.append(f"• Bought {len(bought)}: {names}{_more(12, len(bought))}")
    else:
        out.append("• Bought nothing.")
    if sold:
        total = sum(s["pnl"] for s in sold)
        measured = [s for s in sold if s["excess"] is not None]
        beat = sum(1 for s in measured if s["excess"] > 0)
        line = f"• Sold {len(sold)}: {_money(total)} in total."
        if measured:
            line += f" {beat} of {len(measured)} beat the S&P 500."
        out.append(line)
        for s in sold[:10]:
            how = EXITS.get(s["exit"], s["exit"].replace("_", " "))
            out.append(
                f"   {s['ticker']} {_pct(s['pnl_pct'])} ({_money(s['pnl'])})"
                + (f", {how}" if how else "")
            )
        if len(sold) > 10:
            out.append(f"   and {len(sold) - 10} more")
    else:
        out.append("• Sold nothing.")
    held, bare = board["open"]["held"], board["open"]["without_stop"]
    if held:
        stops = (
            "each with a stop at the broker"
            if not bare
            else f"{bare} WITHOUT a stop at the broker"
        )
        out.append(f"• Open now: {held} position{'s' if held != 1 else ''}, {stops}.")
    else:
        out.append("• Nothing is open.")

    every = board["all"]
    out.append(f"\nSince the start ({every['trades']} closed trades)")
    if every["trades"]:
        out.append(f"• Result: {_money(every['pnl'])}.")
    if every["measured"]:
        out.append(
            f"• Against the S&P 500: {_pct(every['avg_excess'], ' points')} per trade on "
            f"average; {every['beat']} of {every['measured']} beat it."
        )
        t = every["t"]
        if t is None:
            out.append("• Too few trades to say whether that is skill or luck.")
        elif abs(t) < LUCK_T:
            out.append(
                f"• That cannot be told apart from luck yet (t = {_num(t)}; it takes {LUCK_T:.0f})."
            )
        else:
            word = "better" if t > 0 else "worse"
            out.append(
                f"• That is {word} than the market by more than luck usually explains "
                f"(t = {_num(t)})."
            )

    if board["headline"]:
        out.append(f"\nWhat Pulse learned (its report of {_day(board['report_day'])})")
        out += [f"• {h}" for h in board["headline"][:5]]

    stops = [
        e for e in board["exits"] if e.get("exit_kind") in STOPS and e.get("trades")
    ]
    if stops:
        out.append("\nWhat the stops cost")
        for e in stops:
            name = EXITS.get(e["exit_kind"], e["exit_kind"])
            n = e["trades"]
            line = (
                f"• {name.capitalize()}: {n} trade{'s' if n != 1 else ''}, "
                f"{_pct(e.get('avg_return'))} on average."
            )
            # The report signs this from the position's side (a short included):
            # above zero means holding on would have paid.
            after, known = e.get("avg_after10"), e.get("after10_n") or 0
            if after is not None and known:
                line += f" Holding on ten more days would have brought {_pct(after)} on average"
                if known < n:
                    line += f" (known for {known} of them so far)"
                # A handful of trades says nothing about a rule, and neither
                # does an average its own scatter could explain.
                if known >= ENOUGH:
                    if abs(e.get("after10_t") or 0) >= LUCK_T:
                        line += (
                            ": on average the stop sold too early"
                            if after > 0
                            else ": on average the stop was right"
                        )
                    else:
                        line += ": that could be luck"
                line += "."
            out.append(line)

    gates = [
        g
        for g in board["gates"]
        if g.get("outcome") == "skipped"
        and g.get("signals")
        and g.get("avg_excess") is not None
    ]
    if gates:
        out.append(
            "\nDid the filters help? (what the skipped stocks did next, vs the S&P 500)"
        )

        def sure(g: dict) -> bool:
            return abs(g.get("t") or 0) >= LUCK_T and g["signals"] >= ENOUGH

        # The filters with the most behind them first, whichever way they
        # point: a list sorted by result would cut the ones that cost money.
        for g in sorted(gates, key=lambda g: (not sure(g), -g["signals"]))[:8]:
            name, short = _reason(g["reason"])
            doubt = (
                ""
                if sure(g)
                else ", too few to be sure"
                if g["signals"] < ENOUGH
                else ", could be luck"
            )
            verdict = ("helped" if g["avg_excess"] < 0 else "cost money") + doubt
            # For shorts the report already turns the number around: below zero
            # means the short would have lost against the market.
            did = "shorting them would have done" if short else "they did"
            out.append(
                f"• {name.capitalize()}{' (shorts)' if short else ''}: {g['signals']} skipped, "
                f"{did} {_pct(g['avg_excess'], ' points')} ({verdict})."
            )
        if len(gates) > 8:
            out.append(f"• and {len(gates) - 8} more filters.")

    out.append("\nSignal weights")
    if board["weights_moved"]:
        for c in board["weights_moved"][:8]:
            out.append(
                f"• Pulse moved {c.get('dimension', '?')} from {c.get('from', 0):.2f} to "
                f"{c.get('to', 0):.2f} by itself on {_day(c['day'])}: {c.get('why', '')}".rstrip(
                    ": "
                )
            )
    else:
        out.append("• Pulse changed none by itself this week.")

    if board["skipped"] or board["ended_early"]:
        out.append("\nWhy it did not buy this week")
        if board["ended_early"]:
            out.append(
                f"• Of its {board['runs']} buying run{'s' if board['runs'] != 1 else ''}, "
                "these ended before looking at a stock: "
                + ", ".join(
                    f"{_reason(s['reason'])[0]} ({s['runs']})" for s in board["ended_early"][:4]
                )
                + "."
            )
        if board["skipped"]:
            out.append(
                "• Stocks it turned down: "
                + ", ".join(
                    f"{_reason(s['reason'])[0]} ({s['stocks']})" for s in board["skipped"][:6]
                )
                + _more(6, len(board["skipped"]))
                + "."
            )
    return "\n".join(out)


def shorten(text: str, limit: int = TEXT_LIMIT) -> str:
    """The board cut to one Telegram text: whole sections go, last one first,
    and the text says that something is missing."""
    if len(text) <= limit:
        return text
    note = "\n\n(Shortened. The whole board: team.py scoreboard)"
    parts = text.split("\n\n")
    while len(parts) > 1 and len("\n\n".join(parts)) + len(note) > limit:
        parts.pop()
    return "\n\n".join(parts)[: limit - len(note)] + note


def week_of(day: dt.date) -> str:
    """The name of the week a scoreboard belongs to (ISO year and week)."""
    year, week, _ = day.isocalendar()
    return f"{year}-W{week:02d}"


def last_week_end(today: dt.date) -> dt.date:
    """The Sunday that closed the week before `today`'s. The board always
    covers a whole Monday-to-Sunday week, whichever night of the new week
    sends it, so no day falls between two boards."""
    return today - dt.timedelta(days=today.weekday() + 1)


def due(today: dt.date, last_week: str) -> bool:
    """One scoreboard a week: on the first night of a new week."""
    return week_of(today) != (last_week or "")
