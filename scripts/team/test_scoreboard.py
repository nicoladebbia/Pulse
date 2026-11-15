#!/usr/bin/env python3
"""Tests for the weekly trading scoreboard: python3 -m unittest discover -s scripts/team

Every database here is a temporary one with a handful of made-up trades."""

import datetime as dt
import importlib
import json
import os
import sqlite3
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import chat  # noqa: E402
import scoreboard  # noqa: E402

TODAY = dt.date(2026, 11, 15)  # a Sunday; the week is 9 to 15 November
SCHEMA = """
CREATE TABLE paper_trades (id INTEGER PRIMARY KEY, ticker TEXT, direction TEXT,
  entry_date TEXT, exit_date TEXT, exit_price REAL, status TEXT, order_status TEXT,
  pnl REAL, realized_pnl REAL, pnl_pct REAL, exit_reason TEXT, stop_order_id TEXT,
  entry_context TEXT, entry_trigger TEXT);
CREATE TABLE trade_reviews (trade_id INTEGER, excess_pct REAL, exit_kind TEXT,
  after10_pct REAL);
CREATE TABLE trade_decisions (id INTEGER PRIMARY KEY, run_at TEXT, ticker TEXT,
  outcome TEXT, reason TEXT);
CREATE TABLE learning_reports (computed_at TEXT, weights_applied INTEGER, body TEXT);
"""


def trade(
    con,
    tid,
    ticker,
    entered,
    exited=None,
    pnl=None,
    pct=None,
    excess=None,
    kind="signal_decay",
    after10=None,
    **more,
):
    row = {
        "id": tid,
        "ticker": ticker,
        "direction": "long",
        "entry_date": entered,
        "exit_date": exited,
        "exit_price": 10.0 if exited else None,
        "status": "closed" if exited else "open",
        "order_status": "filled",
        "pnl": pnl,
        "pnl_pct": pct,
        "stop_order_id": None if exited else "stop-1",
        "entry_trigger": "convergence",
        **more,
    }
    con.execute(
        f"INSERT INTO paper_trades ({', '.join(row)}) VALUES ({', '.join('?' * len(row))})",
        list(row.values()),
    )
    if excess is not None:
        con.execute(
            "INSERT INTO trade_reviews VALUES (?, ?, ?, ?)",
            (tid, excess, kind, after10),
        )


class Board(unittest.TestCase):
    def setUp(self):
        self.con = self.memory()
        self.con.executescript(SCHEMA)

    def memory(self):
        con = sqlite3.connect(":memory:")
        self.addCleanup(con.close)
        return con

    def board(self, days=7):
        return scoreboard.build(self.con, TODAY, days)

    def test_the_week_takes_both_its_first_and_last_day(self):
        trade(self.con, 1, "OLD", "2026-11-08T10:00:00")  # the day before the week
        trade(self.con, 2, "MON", "2026-11-09T10:00:00")
        trade(self.con, 3, "SUN", "2026-11-15T23:59:00")
        trade(
            self.con,
            4,
            "OUT",
            "2026-10-01",
            "2026-11-08T15:00:00",
            pnl=5,
            pct=1,
            excess=1,
        )
        trade(
            self.con,
            5,
            "IN",
            "2026-10-01",
            "2026-11-09T09:35:00",
            pnl=-20,
            pct=-2,
            excess=-3,
        )
        board = self.board()
        self.assertEqual((board["since"], board["until"]), ("2026-11-09", "2026-11-15"))
        self.assertEqual([b["ticker"] for b in board["bought"]], ["MON", "SUN"])
        self.assertEqual([s["ticker"] for s in board["sold"]], ["IN"])
        self.assertEqual(board["all"]["trades"], 2)  # since the start counts both

    def test_only_real_trades_count_like_the_go_live_checklist(self):
        day = "2026-11-10T10:00:00"
        trade(self.con, 1, "REAL", "2026-11-01", day, pnl=100, pct=10, excess=4)
        trade(
            self.con,
            2,
            "PIECE",
            "2026-11-01",
            day,
            pnl=50,
            status="expired",
            exit_reason="merged_into_1",
        )
        trade(
            self.con,
            3,
            "TEST",
            "2026-11-01",
            day,
            pnl=50,
            entry_context=json.dumps({"event": {"test": True}}),
        )
        trade(self.con, 4, "NOPRICE", "2026-11-01", day, pnl=50, exit_price=None)
        trade(self.con, 5, "NORESULT", "2026-11-01", day)
        trade(
            self.con,
            6,
            "STOPPED",
            "2026-11-01",
            day,
            pnl=-30,
            pct=-3,
            status="stopped_out",
            entry_context="not json",
        )
        trade(
            self.con, 7, "FRAGMENT", day, status="expired", exit_reason="merged_into_1"
        )
        board = self.board()
        self.assertEqual([s["ticker"] for s in board["sold"]], ["REAL", "STOPPED"])
        self.assertEqual(board["all"]["trades"], 2)
        self.assertAlmostEqual(board["all"]["pnl"], 70)
        self.assertEqual(board["bought"], [])  # a merged fragment was not a purchase
        self.assertEqual((board["all"]["measured"], board["all"]["beat"]), (1, 1))

    def test_the_result_against_the_market_is_an_average_with_its_t(self):
        for n, excess in enumerate([2.0, -1.0, 3.0, 0.0], start=1):
            trade(
                self.con,
                n,
                f"T{n}",
                "2026-10-01",
                "2026-10-20",
                pnl=excess * 10,
                pct=excess,
                excess=excess,
            )
        every = self.board()["all"]
        self.assertAlmostEqual(every["avg_excess"], 1.0)
        self.assertEqual((every["measured"], every["beat"]), (4, 2))
        # mean 1, sample sd sqrt(10/3), n 4: t = 1 / (1.8257 / 2)
        self.assertAlmostEqual(every["t"], 1.0954, places=3)
        self.assertIsNone(scoreboard.t_stat([1.0]))
        self.assertIsNone(scoreboard.t_stat([2.0, 2.0]))

    def test_open_positions_and_the_ones_without_a_stop(self):
        trade(self.con, 1, "A", "2026-11-01")
        trade(self.con, 2, "B", "2026-11-01", stop_order_id="")
        trade(
            self.con, 3, "C", "2026-11-01", stop_order_id=None, order_status="pending"
        )
        self.assertEqual(self.board()["open"], {"held": 2, "without_stop": 1})
        self.assertIn("1 WITHOUT a stop", scoreboard.as_text(self.board()))

    def test_what_pulse_wrote_itself_is_passed_on(self):
        report = {
            "headline": ["Signals: 60 measured."],
            "exits": [
                {
                    "exit_kind": "trailing_stop",
                    "trades": 12,
                    "avg_return": 1.4,
                    "avg_after10": 2.5,
                },
                {
                    "exit_kind": "hard_stop",
                    "trades": 1,
                    "avg_return": -16.4,
                    "avg_after10": 18.1,
                },
                {
                    "exit_kind": "broker_stop",
                    "trades": 12,
                    "avg_return": -0.4,
                    "avg_after10": 2.1,
                },
                {
                    "exit_kind": "signal_decay",
                    "trades": 25,
                    "avg_return": 0.2,
                    "avg_after10": 1.2,
                },
            ],
            "gates": [
                {
                    "reason": "bought",
                    "outcome": "bought",
                    "signals": 8,
                    "avg_excess": -0.5,
                    "t": -0.2,
                },
                {
                    "reason": "universe",
                    "outcome": "skipped",
                    "signals": 40,
                    "avg_excess": -10.0,
                    "t": -2.4,
                },
                {
                    "reason": "earnings",
                    "outcome": "skipped",
                    "signals": 3,
                    "avg_excess": 0.9,
                    "t": 0.3,
                },
                # A high t on three stocks is still three stocks.
                {
                    "reason": "too_thin",
                    "outcome": "skipped",
                    "signals": 3,
                    "avg_excess": 4.0,
                    "t": 2.5,
                },
                {
                    "reason": "too_calm (short)",
                    "outcome": "skipped",
                    "signals": 12,
                    "avg_excess": -5.2,
                    "t": -2.2,
                },
            ],
            "weight_changes": [
                {"dimension": "news", "from": 0.3, "to": 0.27, "why": "it lagged"}
            ],
        }
        old = dict(
            report,
            weight_changes=[
                {"dimension": "search", "from": 0.2, "to": 0.22, "why": "x"}
            ],
        )
        rows = [
            ("2026-11-01 17:00:00", 1, json.dumps(old)),  # before the week
            ("2026-11-12 17:00:00", 1, json.dumps(report)),
            ("2026-11-13 17:00:00", 0, json.dumps(dict(report, weight_changes=[]))),
        ]
        self.con.executemany("INSERT INTO learning_reports VALUES (?, ?, ?)", rows)
        # The reviews behind the report: every trailing stop has its ten days
        # after the sale, only two of the twelve broker stops do.
        self.con.executemany(
            "INSERT INTO trade_reviews VALUES (?, ?, ?, ?)",
            [(100 + n, 0.0, "trailing_stop", 2.5) for n in range(12)]
            + [(200 + n, 0.0, "broker_stop", 2.1 if n < 2 else None) for n in range(12)]
            + [(300, 0.0, "hard_stop", 18.1)],
        )
        self.con.executemany(
            "INSERT INTO trade_decisions (run_at, ticker, outcome, reason) VALUES (?, ?, ?, ?)",
            # The same stock turned down by three runs is one stock.
            [(f"2026-11-1{d}T10:00:09", "AAA", "skipped", "universe") for d in (0, 1, 2)]
            + [("2026-11-10T10:00:09", "BBB", "skipped", "universe")]
            + [("2026-11-10T10:00:09", "CCC", "skipped", "earnings")]
            + [(f"2026-11-1{d}T13:00:09", "", "run_stopped", "no_candidates") for d in (0, 1)]
            + [("2026-11-10T10:00:09", "DDD", "bought", "bought")]
            + [("2026-11-02T10:00:09", "EEE", "skipped", "earnings")],
        )
        board = self.board()
        self.assertEqual(board["report_day"], "2026-11-13")
        self.assertEqual([c["dimension"] for c in board["weights_moved"]], ["news"])
        self.assertEqual(
            (board["skipped"], board["ended_early"], board["runs"]),
            (
                [{"reason": "universe", "stocks": 2}, {"reason": "earnings", "stocks": 1}],
                [{"reason": "no_candidates", "runs": 2}],
                5,
            ),
        )
        text = scoreboard.as_text(board)
        for part in [
            "• Signals: 60 measured.",
            "Trailing stop: 12 trades, +1.4% on average. Holding on ten more days would "
            "have brought +2.5% on average: on average the stop sold too early.",
            # One trade says nothing about the rule.
            "Hard stop: 1 trade, −16.4% on average. Holding on ten more days would "
            "have brought +18.1% on average.",
            # Nor do two measured trades out of twelve.
            "Stop at the broker: 12 trades, −0.4% on average. Holding on ten more days "
            "would have brought +2.1% on average (known for 2 of them so far).",
            "Not in the tradable universe: 40 skipped, they did −10.0 points (helped).",
            "Earnings too close: 3 skipped, they did +0.9 points (cost money, too few to be sure).",
            "Too little trading volume: 3 skipped, they did +4.0 points "
            "(cost money, too few to be sure).",
            "The stock moves too little (shorts): 12 skipped, shorting them would have "
            "done −5.2 points (helped).",
            "Pulse moved news from 0.30 to 0.27 by itself on 12 Nov: it lagged",
            "• Of its 5 buying runs, these ended before looking at a stock: "
            "no stock reached a buy grade (2).",
            "• Stocks it turned down: not in the tradable universe (2), earnings too close (1).",
        ]:
            self.assertIn(part, text)
        self.assertNotIn("Signal faded:", text)  # not a stop
        self.assertNotIn("Bought: ", text)  # the gates list only what was skipped

    def test_the_text_says_plainly_what_the_numbers_mean(self):
        trade(
            self.con,
            1,
            "WIN",
            "2026-11-09",
            "2026-11-12T10:00:00",
            pnl=120.4,
            pct=6.0,
            excess=4.0,
            kind="trailing_stop",
        )
        trade(
            self.con,
            2,
            "LOSS",
            "2026-11-09",
            "2026-11-13T10:00:00",
            pnl=-300,
            pct=-15.0,
            excess=-12.0,
        )
        trade(self.con, 3, "HELD", "2026-11-10", direction="short")
        text = scoreboard.as_text(self.board())
        for part in [
            "Pulse trading, 9 Nov to 15 Nov (paper money)",
            "• Bought 3: WIN, LOSS, HELD (short)",
            "• Sold 2: −$180 in total. 1 of 2 beat the S&P 500.",
            "   WIN +6.0% (+$120), trailing stop",
            "   LOSS −15.0% (−$300), signal faded",
            "• Open now: 1 position, each with a stop at the broker.",
            "Since the start (2 closed trades)",
            "• Against the S&P 500: −4.0 points per trade on average; 1 of 2 beat it.",
            "cannot be told apart from luck yet (t = −0.5; it takes 2)",
            "• Pulse changed none by itself this week.",
        ]:
            self.assertIn(part, text)
        self.assertLess(len(text), chat.TEXT_MAX)

    def test_a_clear_result_is_called_clear(self):
        for n in range(1, 31):
            trade(
                self.con,
                n,
                f"T{n}",
                "2026-10-01",
                "2026-10-20",
                pnl=10,
                pct=1,
                excess=2.0 + n % 3,
            )
        self.assertIn(
            "better than the market by more than luck usually explains",
            scoreboard.as_text(self.board()),
        )

    def test_an_order_that_never_filled_is_not_a_buy(self):
        trade(self.con, 1, "FILLED", "2026-11-10")
        trade(self.con, 2, "WAITING", "2026-11-10", order_status="pending")
        self.assertIn("• Bought 1: FILLED\n", scoreboard.as_text(self.board()))

    def test_long_lists_say_how_much_is_left_out(self):
        for n in range(1, 16):
            trade(
                self.con,
                n,
                f"T{n}",
                "2026-11-09",
                "2026-11-12",
                pnl=10,
                pct=1,
                excess=1.0,
            )
        text = scoreboard.as_text(self.board())
        self.assertIn("T12, and 3 more", text)
        self.assertIn("   T10 +1.0% (+$10), signal faded\n   and 5 more", text)

    def test_a_board_too_long_for_one_text_loses_whole_sections_and_says_so(self):
        text = "Top\n\n" + "\n\n".join(f"Section {n}\n" + "x" * 500 for n in range(12))
        short = scoreboard.shorten(text, 2000)
        self.assertLessEqual(len(short), 2000)
        self.assertTrue(short.startswith("Top\n\nSection 0\n"))
        self.assertTrue(short.endswith("(Shortened. The whole board: team.py scoreboard)"))
        self.assertTrue(short.split("\n\n")[-2].endswith("x" * 500))  # no section cut in half
        self.assertEqual(scoreboard.shorten("short"), "short")
        # One section longer than a text is cut, still with the note.
        self.assertLessEqual(len(scoreboard.shorten("y" * 5000, 2000)), 2000)

    def test_an_empty_or_older_database_still_gives_a_board(self):
        text = scoreboard.as_text(self.board())
        for part in [
            "• Bought nothing.",
            "• Sold nothing.",
            "• Nothing is open.",
            "(0 closed trades)",
        ]:
            self.assertIn(part, text)
        self.con.execute(
            "INSERT INTO learning_reports VALUES ('2026-11-13', 1, 'not json')"
        )
        self.assertEqual(self.board()["headline"], [])
        older = self.memory()  # only the trades, as Pulse began
        older.executescript(SCHEMA.split("CREATE TABLE trade_reviews")[0])
        self.assertIn(
            "• Sold nothing.", scoreboard.as_text(scoreboard.build(older, TODAY))
        )

    def test_a_database_that_is_not_pulse_is_an_error_not_an_empty_week(self):
        with self.assertRaises(sqlite3.Error):
            scoreboard.build(self.memory(), TODAY)

    def test_one_a_week_on_the_first_night_of_the_week(self):
        monday = dt.date(2026, 11, 16)
        self.assertEqual(scoreboard.week_of(monday), "2026-W47")
        self.assertTrue(scoreboard.due(monday, ""))
        self.assertTrue(scoreboard.due(monday, "2026-W46"))
        self.assertFalse(scoreboard.due(monday + dt.timedelta(days=6), "2026-W47"))
        self.assertTrue(scoreboard.due(monday + dt.timedelta(days=7), "2026-W47"))

    def test_the_board_is_always_the_whole_week_before(self):
        sunday = dt.date(2026, 11, 15)
        # Sent on Monday, or on Tuesday after a Monday night that did nothing,
        # or as late as the next Sunday: the same Monday-to-Sunday week.
        for days in (1, 2, 7):
            self.assertEqual(
                scoreboard.last_week_end(sunday + dt.timedelta(days=days)), sunday
            )
        self.assertEqual(
            scoreboard.last_week_end(sunday + dt.timedelta(days=8)),
            sunday + dt.timedelta(days=7),
        )


class Weekly(unittest.TestCase):
    """What the runner does with it."""

    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.home = Path(self.tmp.name).resolve()
        saved = os.environ.get("PULSE_TEAM_HOME")
        os.environ["PULSE_TEAM_HOME"] = str(self.home)
        old_home = chat.HOME
        self.addCleanup(chat.set_home, old_home)
        self.team = importlib.reload(importlib.import_module("team"))
        if saved is None:
            os.environ.pop("PULSE_TEAM_HOME", None)
        else:
            os.environ["PULSE_TEAM_HOME"] = saved
        self.team.log = lambda msg: None
        self.sent = []
        self.addCleanup(setattr, chat, "send", chat.send)
        chat.send = lambda text, **k: self.sent.append(text) or "sent"
        self.team.DB_COPY.parent.mkdir(parents=True)
        con = sqlite3.connect(self.team.DB_COPY)
        con.executescript(SCHEMA)
        self.sunday = scoreboard.last_week_end(dt.date.today())
        trade(con, 1, "LASTWEEK", self.sunday.isoformat())
        trade(con, 2, "THISWEEK", (self.sunday + dt.timedelta(days=1)).isoformat())
        con.commit()
        con.close()

    def test_the_first_scheduled_night_of_a_week_texts_it_once(self):
        state = {}
        self.team.weekly_scoreboard({"scheduled": True}, state)
        self.assertEqual(len(self.sent), 1)
        # The board is last week's, whichever night of this week sends it.
        self.assertIn("• Bought 1: LASTWEEK", self.sent[0])
        self.assertNotIn("database copy made on", self.sent[0])
        self.assertEqual(state["scoreboard_week"], scoreboard.week_of(dt.date.today()))
        self.assertEqual(
            (self.home / "scoreboard.txt").read_text(), self.sent[0] + "\n"
        )
        self.assertEqual(
            json.loads((self.home / "state.json").read_text())["scoreboard_week"],
            state["scoreboard_week"],
        )
        self.team.weekly_scoreboard({"scheduled": True}, state)
        self.assertEqual(len(self.sent), 1)

    def test_a_run_by_hand_or_a_night_that_did_nothing_sends_none(self):
        for run in [{"scheduled": False}, {"scheduled": True, "nothing_done": True}]:
            state = {}
            self.team.weekly_scoreboard(run, state)
            self.assertEqual((self.sent, state), ([], {}))
        self.team.DB_COPY.unlink()
        self.team.weekly_scoreboard({"scheduled": True}, state)
        self.assertEqual(self.sent, [])

    def test_an_old_copy_of_the_database_is_named(self):
        old = dt.datetime.combine(self.sunday, dt.time(2, 30)).timestamp()
        os.utime(self.team.DB_COPY, (old, old))
        self.team.weekly_scoreboard({"scheduled": True}, {})
        self.assertIn(
            f"database copy made on {self.sunday.day} {self.sunday:%b}: anything after it "
            "is missing.",
            self.sent[0],
        )

    def test_a_long_board_is_texted_short_and_kept_whole(self):
        con = sqlite3.connect(self.team.DB_COPY)
        report = {"headline": ["h" * 1200] * 5}
        con.execute(
            "INSERT INTO learning_reports VALUES (?, 0, ?)",
            (self.sunday.isoformat(), json.dumps(report)),
        )
        con.commit()
        con.close()
        self.team.weekly_scoreboard({"scheduled": True}, {})
        self.assertLessEqual(len(self.sent[0]), chat.TEXT_MAX)
        self.assertIn("(Shortened.", self.sent[0])
        whole = (self.home / "scoreboard.txt").read_text()
        self.assertGreater(len(whole), 6000)
        self.assertIn("Signal weights", whole)

    def test_a_scoreboard_that_fails_never_fails_the_night(self):
        self.team.DB_COPY.write_text("not a database")
        state = {}
        self.team.weekly_scoreboard({"scheduled": True}, state)
        self.assertEqual((self.sent, state), ([], {}))  # tried again the next night

    def test_the_copy_is_only_read(self):
        before = self.team.DB_COPY.read_bytes()
        self.team.scoreboard_text(dt.date.today())
        self.assertEqual(self.team.DB_COPY.read_bytes(), before)
        self.assertEqual(
            sorted(p.name for p in self.team.DB_COPY.parent.iterdir()),
            ["pulse-copy.db"],
        )
        con = scoreboard.connect(self.team.DB_COPY)
        self.addCleanup(con.close)
        with self.assertRaises(sqlite3.OperationalError):
            con.execute("DELETE FROM paper_trades")


if __name__ == "__main__":
    unittest.main()
