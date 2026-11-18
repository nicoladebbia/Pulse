#!/usr/bin/env python3
"""Tests for the weekly news board: python3 -m unittest discover -s scripts/team

Every database here is a temporary one with a handful of made-up articles."""

import datetime as dt
import importlib
import itertools
import json
import os
import sqlite3
import sys
import tempfile
import time
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import chat  # noqa: E402
import newsboard  # noqa: E402
import scoreboard  # noqa: E402

TODAY = dt.date(2026, 11, 15)  # a Sunday; the week is 9 to 15 November
SCHEMA = """
CREATE TABLE briefings (id INTEGER PRIMARY KEY, date TEXT,
  briefing_type TEXT DEFAULT 'daily', story_count INTEGER DEFAULT 120);
CREATE TABLE fetch_candidates (id INTEGER PRIMARY KEY, briefing_id INTEGER,
  url_hash TEXT, source_name TEXT, sector TEXT, kept_by_precurate INTEGER,
  story_id INTEGER);
CREATE TABLE pipeline_health (run_date TEXT, stories_fetched INTEGER,
  feeds_failed INTEGER, duration_secs REAL);
CREATE TABLE stories (id INTEGER PRIMARY KEY, briefing_id INTEGER, sector TEXT,
  importance_score REAL, source_type TEXT);
CREATE TABLE engagement_events (occurred_at TEXT, event TEXT, story_id INTEGER);
CREATE TABLE story_feedback (story_id INTEGER PRIMARY KEY, label TEXT,
  updated_at TEXT);
"""
_url = itertools.count(1)
_story = itertools.count(1)


def briefing(con, bid, date, kind="daily", stories=120):
    con.execute("INSERT INTO briefings VALUES (?, ?, ?, ?)", (bid, date, kind, stories))


def offer(con, bid, source, n, made=0, kept=None, sector="tech", urls=None):
    """`n` articles from one source offered to a briefing: the first `kept`
    survive the first cut and the first `made` of those are in the briefing."""
    kept = made if kept is None else kept
    for i in range(n):
        con.execute(
            "INSERT INTO fetch_candidates (briefing_id, url_hash, source_name, sector,"
            " kept_by_precurate, story_id) VALUES (?, ?, ?, ?, ?, ?)",
            (
                bid,
                urls[i] if urls else f"u{next(_url)}",
                source,
                sector,
                int(i < kept),
                next(_story) if i < made else None,
            ),
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
        return newsboard.build(self.con, TODAY, days)

    def text(self):
        return newsboard.as_text(self.board())

    def test_the_week_takes_both_its_first_and_last_day(self):
        briefing(self.con, 1, "2026-11-08")  # the day before the week
        briefing(self.con, 2, "2026-11-09")
        briefing(self.con, 3, "2026-11-15")
        briefing(self.con, 4, "2026-11-16")  # the day after
        briefing(self.con, 5, "2026-11-12", "freedoms")  # not a news briefing
        for bid in range(1, 6):
            offer(self.con, bid, f"Source {bid}", 3, made=1)
        board = self.board()
        self.assertEqual((board["since"], board["until"]), ("2026-11-09", "2026-11-15"))
        self.assertEqual((board["briefings"], board["counted"]), (2, 2))
        self.assertEqual(board["offered"], 6)
        self.assertEqual(
            [s["source"] for s in board["most_used"]], ["Source 2", "Source 3"]
        )
        self.assertTrue(self.text().startswith("Pulse news, 9 Nov to 15 Nov\n"))

    def test_how_much_came_in_and_how_much_survived_each_cut(self):
        briefing(self.con, 1, "2026-11-10")
        briefing(self.con, 2, "2026-11-11")
        offer(self.con, 1, "A", 1000, made=100, kept=120)
        offer(self.con, 2, "A", 2000, made=140, kept=160)
        self.con.executemany(
            "INSERT INTO pipeline_health VALUES (?, ?, ?, ?)",
            [
                ("2026-11-10", 2000, 0, 1500.0),
                ("2026-11-11", 3000, 3, 2100.0),
                ("2026-11-08", 9000, 9, 9000.0),  # a run of another week
            ],
        )
        board = self.board()
        self.assertEqual(
            (board["fetched"], board["pool"], board["kept"], board["made"]),
            (2500, 1500, 140, 120),
        )
        self.assertEqual(board["minutes"], 30)
        text = self.text()
        self.assertIn(
            "• 2 briefings. Each fetched about 2,500 articles. About 1,500 were left once "
            "repeats were dropped, the first cut kept 140, and 120 made the briefing.\n",
            text,
        )
        self.assertIn("• A run took about 30 minutes.\n", text)
        self.assertIn("• A source failed in 1 of 2 runs (at most 3 at a time).\n", text)

    def test_a_week_without_a_failed_source_says_so(self):
        briefing(self.con, 1, "2026-11-10")
        offer(self.con, 1, "A", 4, made=1)
        self.con.execute(
            "INSERT INTO pipeline_health VALUES ('2026-11-10', 9, 0, 60.0)"
        )
        text = self.text()
        self.assertIn("• 1 briefing. Each fetched about 9 articles.", text)
        self.assertIn("• No source failed in any run.\n", text)

    def test_an_article_offered_to_a_second_briefing_is_a_repeat(self):
        briefing(self.con, 1, "2026-11-10")
        briefing(self.con, 2, "2026-11-11")
        briefing(
            self.con, 3, "2026-11-08"
        )  # last week's offer is not this week's repeat
        offer(self.con, 3, "A", 1, urls=["x"])
        offer(self.con, 1, "A", 3, made=1, urls=["x", "y", "z"])
        offer(self.con, 2, "A", 1, urls=["y"])
        board = self.board()
        self.assertEqual((board["offered"], board["offered_again"]), (4, 1))
        self.assertIn(
            "• 25% of what was offered had already been offered to an earlier briefing "
            "of these days.\n",
            self.text(),
        )

    def test_topics_carry_what_was_offered_used_and_how_the_ai_scored_it(self):
        briefing(self.con, 1, "2026-11-10")
        briefing(self.con, 2, "2026-11-01")
        offer(self.con, 1, "A", 1200, made=2, sector="italy")
        offer(self.con, 1, "B", 30, made=3, sector="ai")
        offer(self.con, 1, "C", 5, sector="space")
        self.con.executemany(
            "INSERT INTO stories (briefing_id, sector, importance_score, source_type)"
            " VALUES (?, ?, ?, ?)",
            [
                (1, "italy", 4.0, "news"),
                (1, "italy", 5.0, None),  # an older row, before the column: news
                (1, "italy", None, "news"),  # not scored: not a zero
                (1, "italy", 10.0, "financial"),  # a filing, not a news story
                (2, "italy", 10.0, "news"),  # another week
                (1, "ai", 8.25, "news"),
            ],
        )
        self.assertEqual(
            self.board()["topics"],
            [
                {"topic": "italy", "offered": 1200, "made": 2, "importance": 4.5},
                {"topic": "ai", "offered": 30, "made": 3, "importance": 8.25},
                {"topic": "space", "offered": 5, "made": 0, "importance": None},
            ],
        )
        text = self.text()
        self.assertIn("• Italy: 1,200 → 2, importance 4.5\n", text)
        self.assertIn("• AI: 30 → 3, importance 8.2\n", text)
        self.assertIn("• Space: 5 → 0\n", text)

    def test_sources_used_and_sources_fetched_for_nothing(self):
        briefing(self.con, 1, "2026-11-10")
        briefing(self.con, 2, "2026-11-12")
        offer(self.con, 1, "Often", 10, made=6)
        offer(self.con, 2, "Often", 10, made=3)
        offer(self.con, 1, "Once", 5, made=1)
        offer(self.con, 1, "Never", newsboard.BURIED_MIN)  # buried: none used
        offer(self.con, 1, "Rarely", 200, made=3)  # 1.5%: buried
        offer(self.con, 1, "Fair", 200, made=4)  # 2%: used enough
        offer(self.con, 1, "Small", newsboard.BURIED_MIN - 1)  # too few to judge
        offer(self.con, 1, None, 2, made=1)  # an article with no source name
        board = self.board()
        self.assertEqual((board["sources_used"], board["sources_offering"]), (5, 7))
        self.assertEqual(
            board["most_used"][:2],
            [
                {"source": "Often", "made": 9, "offered": 20},
                {"source": "Fair", "made": 4, "offered": 200},
            ],
        )
        self.assertEqual(
            board["buried"],
            [
                {"source": "Never", "made": 0, "offered": 100},
                {"source": "Rarely", "made": 3, "offered": 200},
            ],
        )
        text = self.text()
        self.assertIn(
            "• 5 different sources made the briefings, out of 7 that offered something.\n",
            text,
        )
        self.assertIn("• Most used: Often (9), Fair (4), Rarely (3), ", text)
        self.assertIn(
            "• Offered 100 or more and almost never used: Never (0 of 100), "
            "Rarely (3 of 200).\n",
            text,
        )

    def test_a_long_list_of_sources_is_cut_and_says_how_many_are_left(self):
        briefing(self.con, 1, "2026-11-10")
        for i in range(9):
            offer(self.con, 1, f"S{i}", 2, made=1)
        self.assertIn(
            "• Most used: S0 (1), S1 (1), S2 (1), S3 (1), S4 (1), S5 (1), and 3 more.\n",
            self.text(),
        )

    def test_a_source_that_stopped_offering_is_named(self):
        briefing(self.con, 1, "2026-11-12")  # this week
        briefing(self.con, 2, "2026-11-08")  # the day before the week
        briefing(self.con, 3, "2026-10-12")  # the first of the 28 days before
        briefing(self.con, 4, "2026-10-11")  # one day too old
        offer(self.con, 1, "Still here", 1)
        offer(self.con, 2, "Still here", newsboard.QUIET_MIN)
        offer(self.con, 2, "Gone", 50)
        offer(self.con, 3, "Gone", newsboard.QUIET_MIN - 50)
        offer(self.con, 2, "Too small", newsboard.QUIET_MIN - 1)
        offer(self.con, 4, "Too small", 500)
        offer(self.con, 4, "Long gone", 500)
        self.assertEqual(self.board()["quiet"], [{"source": "Gone", "before": 80}])
        self.assertIn(
            "• Gone quiet (80 or more in the four weeks before, nothing in these days): "
            "Gone (80).\n",
            self.text(),
        )

    def test_nothing_gone_quiet_and_nothing_buried_leave_no_line(self):
        briefing(self.con, 1, "2026-11-12")
        offer(self.con, 1, "A", 3, made=1)
        text = self.text()
        self.assertNotIn("Gone quiet", text)
        self.assertNotIn("almost never used", text)

    def test_what_was_read_counts_a_story_once(self):
        briefing(self.con, 1, "2026-11-12")
        offer(self.con, 1, "A", 3, made=1)
        self.con.executemany(
            "INSERT INTO engagement_events VALUES (?, ?, ?)",
            [
                ("2026-11-12 12:00:00", "story_open", 7),
                ("2026-11-13 12:00:00", "story_open", 7),  # the same story again
                ("2026-11-13 12:00:05", "story_close", 7),
                ("2026-11-14 12:00:00", "story_open", 8),
                ("2026-11-14 12:00:00", "surface_view", None),
                ("2026-10-01 12:00:00", "story_open", 9),  # before the week
            ],
        )
        self.con.executemany(
            "INSERT INTO story_feedback VALUES (?, ?, ?)",
            [
                (7, "mattered", "2026-11-12 12:00:00"),
                (9, "didnt", "2026-10-01 12:00:00"),
            ],
        )
        board = self.board()
        self.assertEqual((board["opened"], board["opened_ever"]), (2, 3))
        self.assertEqual(board["last_opened"], "2026-11-14")
        self.assertEqual((board["marked"], board["marked_ever"]), (1, 2))
        text = self.text()
        self.assertIn("\nWhat was read in the Mac app\n", text)
        self.assertIn(
            "• Stories opened: 2 (3 since the start, the last on 14 Nov).\n", text
        )
        self.assertTrue(text.endswith("• Stories marked: 1 (2 since the start)."))

    def test_nothing_ever_read_has_no_last_day(self):
        briefing(self.con, 1, "2026-11-12")
        offer(self.con, 1, "A", 3, made=1)
        self.assertIn("• Stories opened: 0 (0 since the start).\n", self.text())

    def test_a_late_evening_counts_on_the_macs_day_not_the_utc_one(self):
        saved = os.environ.get("TZ")

        def restore():
            if saved is None:
                os.environ.pop("TZ", None)
            else:
                os.environ["TZ"] = saved
            time.tzset()

        self.addCleanup(restore)
        os.environ["TZ"] = "America/New_York"
        time.tzset()
        briefing(self.con, 1, "2026-11-12")
        offer(self.con, 1, "A", 3, made=1)
        self.con.executemany(
            "INSERT INTO engagement_events VALUES (?, 'story_open', ?)",
            [
                ("2026-11-16 03:00:00", 1),  # Sunday 15 Nov, 22:00 in Miami: in
                ("2026-11-16 03:30:00", 3),  # and a second story the same evening
                ("2026-11-09 03:00:00", 2),  # Sunday 8 Nov, 22:00: the week before
            ],
        )
        self.con.execute(
            "INSERT INTO story_feedback VALUES (1, 'mattered', '2026-11-16 03:00:00')"
        )
        board = self.board()
        self.assertEqual((board["opened"], board["opened_ever"]), (2, 3))
        self.assertEqual(board["last_opened"], "2026-11-15")
        self.assertEqual(board["marked"], 1)

    def test_a_week_without_a_briefing_says_so(self):
        briefing(self.con, 1, "2026-11-01")
        offer(self.con, 1, "A", 3, made=1)
        self.assertEqual(
            self.text(),
            "Pulse news, 9 Nov to 15 Nov\n\nNo briefing was made in these days.",
        )

    def test_a_database_older_than_the_counting_still_gives_the_briefings(self):
        older = self.memory()
        older.execute(
            "CREATE TABLE briefings (id INTEGER PRIMARY KEY, date TEXT, briefing_type TEXT,"
            " story_count INTEGER)"
        )
        older.execute("INSERT INTO briefings VALUES (1, '2026-11-10', 'daily', 120)")
        text = newsboard.as_text(newsboard.build(older, TODAY))
        self.assertIn(
            "• 1 briefing. What was offered to them was not recorded.\n", text
        )
        self.assertNotIn("Sources", text)
        self.assertNotIn("By topic", text)
        self.assertIn("• Stories opened: 0 (0 since the start).", text)

    def test_a_run_that_made_nothing_is_not_a_briefing(self):
        briefing(self.con, 1, "2026-11-10")
        briefing(self.con, 2, "2026-11-11", stories=0)  # the run failed midway
        offer(self.con, 1, "A", 40, made=10)
        offer(self.con, 2, "B", 60)
        board = self.board()
        self.assertEqual(
            (board["briefings"], board["counted"], board["pool"], board["made"]),
            (1, 1, 40, 10),
        )
        self.assertEqual(board["sources_offering"], 1)
        self.assertIn("• 1 briefing.", self.text())
        self.assertNotIn("Counted on", self.text())

    def test_a_board_for_more_than_one_week_takes_all_its_days(self):
        briefing(self.con, 1, "2026-11-03")  # the week before
        briefing(self.con, 2, "2026-11-10")
        offer(self.con, 1, "A", 40, made=10)
        offer(self.con, 2, "A", 20, made=10)
        self.assertEqual(self.board()["briefings"], 1)
        board = newsboard.build(self.con, TODAY, 14)
        self.assertEqual((board["briefings"], board["pool"]), (2, 30))
        self.assertIn("Pulse news, 2 Nov to 15 Nov", newsboard.as_text(board))

    def test_briefings_whose_articles_were_not_recorded_are_not_averaged_in(self):
        briefing(self.con, 1, "2026-11-10")
        briefing(self.con, 2, "2026-11-11")
        offer(self.con, 1, "A", 40, made=10)
        board = self.board()
        self.assertEqual(
            (board["briefings"], board["counted"], board["pool"]), (2, 1, 40)
        )
        self.assertIn("(Counted on 1 of the 2.)", self.text())

    def test_a_database_that_is_not_pulses_is_an_error(self):
        with self.assertRaises(sqlite3.OperationalError):
            newsboard.build(self.memory(), TODAY)

    def test_the_real_week_fits_one_text(self):
        briefing(self.con, 1, "2026-11-10")
        for i in range(40):
            offer(
                self.con,
                1,
                f"A source with a rather long name, number {i}",
                150,
                made=1,
            )
            offer(self.con, 1, f"A buried source with a long name, number {i}", 150)
        self.assertLess(len(self.text()), scoreboard.TEXT_LIMIT)


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
        self.logged = []
        self.team.log = self.logged.append
        self.sent = []
        self.addCleanup(setattr, chat, "send", chat.send)
        chat.send = lambda text, **k: self.sent.append(text) or "sent"
        self.team.DB_COPY.parent.mkdir(parents=True)
        con = sqlite3.connect(self.team.DB_COPY)
        con.executescript(SCHEMA)
        self.sunday = scoreboard.last_week_end(dt.date.today())
        briefing(con, 1, self.sunday.isoformat())
        briefing(con, 2, (self.sunday + dt.timedelta(days=1)).isoformat())
        offer(con, 1, "Last week", 4, made=2)
        offer(con, 2, "This week", 4, made=2)
        con.commit()
        con.close()

    def test_the_first_scheduled_night_of_a_week_texts_it_once(self):
        state = {}
        self.team.weekly_newsboard({"scheduled": True}, state)
        self.assertEqual(len(self.sent), 1)
        # The board is last week's, whichever night of this week sends it.
        self.assertIn("• Most used: Last week (2).", self.sent[0])
        self.assertNotIn("This week", self.sent[0])
        self.assertEqual(state["newsboard_week"], scoreboard.week_of(dt.date.today()))
        self.assertEqual((self.home / "newsboard.txt").read_text(), self.sent[0] + "\n")
        self.assertEqual(
            json.loads((self.home / "state.json").read_text())["newsboard_week"],
            state["newsboard_week"],
        )
        self.team.weekly_newsboard({"scheduled": True}, state)
        self.assertEqual(len(self.sent), 1)

    def test_a_run_by_hand_or_a_night_that_did_nothing_sends_none(self):
        for run in [{"scheduled": False}, {"scheduled": True, "nothing_done": True}]:
            state = {}
            self.team.weekly_newsboard(run, state)
            self.assertEqual((self.sent, state), ([], {}))

    def test_one_board_failing_leaves_the_other_alone(self):
        # This copy has the news tables and none of the trading ones' columns
        # matter to the news board; break the trading side only.
        state = {}
        self.team.scoreboard_text = lambda day, days: 1 / 0
        self.team.weekly_scoreboard({"scheduled": True}, state)
        self.team.weekly_newsboard({"scheduled": True}, state)
        self.assertEqual(len(self.sent), 1)
        self.assertIn("Pulse news", self.sent[0])
        self.assertNotIn("scoreboard_week", state)  # tried again the next night
        self.assertIn("newsboard_week", state)
        self.assertEqual(
            self.logged, ["the weekly scoreboard failed: ZeroDivisionError"]
        )

    def test_a_news_board_that_fails_never_fails_the_night(self):
        self.team.DB_COPY.write_text("not a database")
        state = {}
        self.team.weekly_newsboard({"scheduled": True}, state)
        self.assertEqual((self.sent, state), ([], {}))
        self.assertEqual(self.logged, ["the weekly newsboard failed: DatabaseError"])

    def test_a_long_board_names_its_own_command(self):
        short = scoreboard.shorten(
            "a\n\n" + "b" * 5000, 2000, whole="team.py newsboard"
        )
        self.assertTrue(
            short.endswith("(Shortened. The whole board: team.py newsboard)")
        )

    def test_the_copy_is_only_read(self):
        before = self.team.DB_COPY.read_bytes()
        self.assertIn("Pulse news", self.team.newsboard_text(self.sunday))
        self.assertEqual(self.team.DB_COPY.read_bytes(), before)
        self.assertEqual(
            sorted(p.name for p in self.team.DB_COPY.parent.iterdir()),
            ["pulse-copy.db"],
        )

    def test_the_installed_copy_gets_the_news_board_too(self):
        self.assertIn('"newsboard.py"', (Path(__file__).parent / "team.py").read_text())


if __name__ == "__main__":
    unittest.main()
