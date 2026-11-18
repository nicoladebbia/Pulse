#!/usr/bin/env python3
"""Tests for the watch: the rules on made-up facts, the reading of files that
look like Pulse's real ones, and one look from end to end.

Run: python3 -m unittest discover -s scripts/team"""

from __future__ import annotations

import datetime as dt
import importlib
import json
import os
import sqlite3
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import chat  # noqa: E402
import rules  # noqa: E402
import watch  # noqa: E402

NOW = dt.datetime(2026, 11, 13, 11, 12)  # a Friday
MIN = dt.timedelta(minutes=1)
HOUR = dt.timedelta(hours=1)


def at(hour: int, minute: int = 0, day: int = 13) -> dt.datetime:
    return dt.datetime(2026, 11, day, hour, minute)


def facts(**over) -> dict:
    """A quiet Friday morning, the Mac awake since 06:00."""
    base = {
        "awake": at(6),
        "events": [],
        "progress": {"stage": "complete", "started": at(8), "updated": at(9, 9)},
        "bare": [],
        "market": "open",
        "session_start": at(10),
        "fetching": False,
        "loaded": dict.fromkeys(rules.SCHEDULERS, True),
        "stamp": at(11),
        "crashes": [],
        "panics": [],
        "team_wake": at(10, 30),
        "team_running": False,
        "paired": True,
        "listener": NOW - MIN,
    }
    return {
        **base,
        "events": [("manage_start", at(11), ""), ("manage_ok", at(11), "")],
        **over,
    }


def keys(found: list[dict]) -> list[str]:
    return [f["key"] for f in found]


class Findings(unittest.TestCase):
    def find(self, memory=None, now=NOW, **over):
        # The team's own heartbeats are fresh unless a test says otherwise.
        fresh = {"listener": now - MIN, "team_wake": now - 42 * MIN}
        return rules.watch_findings(
            facts(**{**fresh, **over}), {} if memory is None else memory, now
        )

    def test_a_quiet_day_finds_nothing(self):
        self.assertEqual(self.find(), [])

    def test_no_facts_is_never_a_finding(self):
        self.assertEqual(rules.watch_findings({}, {}, NOW), [])
        nothing = dict.fromkeys(facts(), None)
        self.assertEqual(rules.watch_findings(nothing, {}, NOW), [])

    def test_a_position_without_a_stop_is_serious_once_a_run_left_it_bare(self):
        memory = {}
        bare = [{"id": 7, "ticker": "DFH"}]
        # First seen: the next position run may still put the stop in place.
        self.assertEqual(self.find(memory, bare=bare), [])
        self.assertEqual(memory["bare"], {"7": "2026-11-13T11:12:00"})
        later = at(12, 2)
        events = [("manage_start", at(12), ""), ("manage_ok", at(12), "")]
        found = self.find(memory, now=later, bare=bare, events=events)
        self.assertEqual(
            found,
            [
                {
                    "key": "stop:7",
                    "serious": True,
                    "text": "DFH has no stop at the broker, and the position run at 12:00 did "
                    "not put one back. Nothing limits the loss on it until it has one.",
                    "name": "DFH",
                }
            ],
        )
        # A run that failed proves nothing about the stop.
        failed = [("manage_start", at(12), ""), ("manage_fail", at(12), "1")]
        self.assertNotIn(
            "stop:7", keys(self.find({}, now=later, bare=bare, events=failed))
        )
        # With the stop in place the watch forgets the position.
        self.find(memory, now=later, bare=[], events=events)
        self.assertEqual(memory["bare"], {})

    def test_when_the_books_cannot_be_read_nothing_is_said_or_forgotten(self):
        memory = {"bare": {"7": "2026-11-13T09:00:00"}}
        self.assertEqual(self.find(memory, bare=None), [])
        self.assertEqual(memory["bare"], {"7": "2026-11-13T09:00:00"})

    def test_dry_run_mode_is_named_once_and_is_not_an_emergency(self):
        bare = [{"id": 7, "ticker": "DFH"}, {"id": 8, "ticker": "TBBK"}]
        memory = {"bare": {"7": "2026-11-13T09:00:00", "8": "2026-11-13T09:00:00"}}
        events = [
            ("manage_start", at(11), ""),
            ("manage_dry", at(11), ""),
            ("manage_ok", at(11), ""),
        ]
        found = self.find(memory, bare=bare, events=events)
        self.assertEqual(keys(found), ["stop:dry"])
        self.assertFalse(found[0]["serious"])
        self.assertIn(
            "2 open positions have no stop at the broker: DFH, TBBK", found[0]["text"]
        )

    def test_the_stop_alarm_is_back_with_the_first_real_run_after_a_dry_one(self):
        bare = [{"id": 7, "ticker": "DFH"}]
        memory = {"bare": {"7": "2026-11-12T13:00:00"}}
        dry = [
            ("manage_start", at(14, day=12), ""),
            ("manage_dry", at(14, day=12), ""),
            ("manage_ok", at(14, day=12), ""),
        ]
        real = [("manage_start", at(11), ""), ("manage_ok", at(11), "")]
        # Exits armed overnight: the dry run of yesterday no longer excuses it.
        found = self.find(memory, bare=bare, events=[*dry, *real])
        self.assertEqual(keys(found), ["stop:7"])
        self.assertTrue(found[0]["serious"])
        # Switched back to dry: the latest run says so, and that is the notice.
        again = [(k, at(11, 5), d) for k, _, d in dry]
        self.assertEqual(
            keys(self.find(memory, bare=bare, events=[*real, *again])), ["stop:dry"]
        )
        # A dry line of a run that failed says nothing about the next one.
        failed = [*dry[:2], ("manage_fail", at(14, day=12), "1"), real[1]]
        self.assertEqual(
            keys(self.find(memory, bare=bare, events=failed)), ["stop:7"]
        )

    def test_position_management_failing_twice_in_a_row_is_serious(self):
        twice = [
            ("manage_start", at(10), ""),
            ("manage_fail", at(10), "1"),
            ("manage_start", at(11), ""),
            ("manage_fail", at(11), "101"),
        ]
        found = self.find(events=twice)
        self.assertEqual(keys(found), ["manage"])
        self.assertTrue(found[0]["serious"])
        self.assertIn("failed twice in a row (at 10:00 and 11:00, exit 101)", found[0]["text"])
        once = [*twice[:2], ("manage_start", at(11), ""), ("manage_ok", at(11), "")]
        self.assertEqual(self.find(events=once), [])
        # A slow briefing took the 11:00 turn: the runs of 10:00 and 12:00 are
        # still two in a row.
        slow = [*twice[:2], ("manage_start", at(12), ""), ("manage_fail", at(12), "1")]
        found = self.find(now=at(12, 10), events=slow)
        self.assertEqual(keys(found), ["manage"])
        self.assertIn("(at 10:00 and 12:00, exit 1)", found[0]["text"])
        # One turn tried twice within minutes is one failed turn.
        retry = [*twice[:2], ("manage_start", at(10, 20), ""), ("manage_fail", at(10, 20), "1")]
        self.assertEqual(self.find(now=at(10, 30), events=retry), [])
        # Yesterday's failures are not happening now.
        old = [(k, w - 24 * HOUR, d) for k, w, d in twice]
        self.assertNotIn("manage", keys(self.find(events=old, market="closed")))
        # Two failures with good hours or a night between them are not a streak.
        apart = [
            ("manage_start", at(15, day=12), ""),
            ("manage_fail", at(15, day=12), "1"),
            *twice[2:],
        ]
        self.assertEqual(self.find(events=apart), [])

    def test_buying_failing_twice_in_a_row_is_serious(self):
        ok = [("manage_start", at(11), ""), ("manage_ok", at(11), "")]
        twice = [*ok, ("auto_fail", at(10), "1"), ("auto_fail", at(11), "1")]
        found = self.find(events=twice)
        self.assertEqual(keys(found), ["auto"])
        self.assertIn("No new positions are opened", found[0]["text"])
        # With a turn lost to a slow briefing between the two.
        slow = [("auto_fail", at(10), "1"), ("auto_fail", at(12), "1")]
        ran = [("manage_start", at(12), ""), ("manage_ok", at(12), "")]
        found = self.find(now=at(12, 10), events=[*slow, *ran])
        self.assertEqual(keys(found), ["auto"])
        self.assertIn("twice in a row (at 10:00 and 12:00", found[0]["text"])
        self.assertEqual(self.find(events=[*twice, ("auto_ok", at(11, 5), "")]), [])
        self.assertEqual(self.find(events=[*ok, ("auto_fail", at(11), "1")]), [])
        # The afternoon slot's last failure and the next morning's first.
        apart = [*ok, ("auto_fail", at(15, day=12), "1"), ("auto_fail", at(11), "1")]
        self.assertEqual(self.find(events=apart), [])

    def test_a_briefing_is_serious_after_its_failed_tries(self):
        ok = [("manage_start", at(11), ""), ("manage_ok", at(11), "")]
        fails = [("brief_fail", at(h), "morning 101") for h in (8, 9, 10)]
        self.assertEqual(self.find(events=[*ok, *fails[:2]]), [])
        found = self.find(events=[*ok, *fails])
        self.assertEqual(keys(found), ["briefing:morning:2026-11-13"])
        self.assertTrue(found[0]["serious"])
        self.assertIn(
            "failed 3 times (last try 10:00, exit 101). The schedule tries again every "
            "hour.",
            found[0]["text"],
        )
        made = [*ok, *fails, ("brief_done", at(11, 9), "morning")]
        self.assertEqual(self.find(events=made), [])
        # One made by the app or by hand is only in the database: the schedule
        # stops trying, and the failures before it are over.
        self.assertEqual(self.find(events=[*ok, *fails], briefings=[at(10, 40)]), [])
        # One made before the failures (or on another day) changes nothing,
        # and neither does a database that could not be read.
        for rows in ([at(7, 30)], [at(10, 40, day=12)], None):
            self.assertEqual(
                keys(self.find(events=[*ok, *fails], briefings=rows)),
                ["briefing:morning:2026-11-13"],
            )
        # The evening has three turns before midnight: two failures are enough.
        quiet = {"market": None, "session_start": None}
        evening = [("brief_fail", at(h), "evening 101") for h in (21, 22)]
        self.assertEqual(self.find(now=at(21, 10), events=evening[:1], **quiet), [])
        found = self.find(now=at(22, 10), events=evening, **quiet)
        self.assertEqual(keys(found), ["briefing:evening:2026-11-13"])
        self.assertIn("tries again every hour until midnight.", found[0]["text"])
        # The morning's briefing is not the evening's.
        found = self.find(now=at(22, 10), events=evening, briefings=[at(8, 28)], **quiet)
        self.assertEqual(keys(found), ["briefing:evening:2026-11-13"])
        # A slow evening: each try works for an hour before it fails, and the
        # last one fails after midnight. It is still Friday's evening briefing.
        slow = [
            ("brief_fail", at(22, 10), "evening 101"),
            ("brief_fail", at(0, 10, day=14), "evening 101"),
        ]
        found = self.find(now=at(0, 12, day=14), events=slow, **quiet)
        self.assertEqual(keys(found), ["briefing:evening:2026-11-13"])
        self.assertIn("failed 2 times (last try 00:10, exit 101)", found[0]["text"])
        # By the next morning that evening is over.
        self.assertEqual(self.find(now=at(8, 12, day=14), events=slow, **quiet), [])
        # Yesterday's failures are yesterday's.
        self.assertEqual(
            self.find(events=[*ok, *[(k, w - 24 * HOUR, d) for k, w, d in fails]]), []
        )

    def test_an_open_market_with_nobody_looking_is_serious(self):
        late = [("manage_start", at(10), ""), ("manage_ok", at(10), "")]
        # One turn missed (the 11:00 one) is a blip, not a text at night.
        self.assertEqual(self.find(events=[]), [])
        self.assertEqual(self.find(events=late), [])
        self.assertEqual(self.find(now=at(12, 9), events=late), [])
        # Two turns missed.
        noon = at(12, 12)
        found = self.find(now=noon, events=[])
        self.assertEqual(keys(found), ["silent"])
        self.assertTrue(found[0]["serious"])
        self.assertIn("for 132 minutes (no run in the log)", found[0]["text"])
        self.assertIn(
            "for 132 minutes (last at 10:00)",
            self.find(now=noon, events=late)[0]["text"],
        )
        # A holiday or an early close: the broker's clock says closed.
        self.assertEqual(self.find(now=noon, events=[], market="closed"), [])
        # Outside trading hours the question is not asked.
        self.assertEqual(
            self.find(now=noon, events=[], market=None, session_start=None), []
        )

    def test_time_the_mac_slept_is_not_counted(self):
        noon = at(12, 12)
        self.assertEqual(self.find(now=noon, events=[], awake=at(10, 40)), [])
        self.assertEqual(
            keys(self.find(now=noon, events=[], awake=at(10, 2))), ["silent"]
        )

    def test_a_long_fetch_is_named_as_the_reason_for_the_silence(self):
        noon = at(12, 12)
        running = {
            "stage": "summarize",
            "label": "Summaries",
            "started": at(10, 30),
            "updated": noon,
        }
        found = self.find(now=noon, events=[], progress=running, fetching=True)
        self.assertIn("silent", keys(found))
        silent = next(f for f in found if f["key"] == "silent")
        self.assertIn("A fetch has been running since 10:30", silent["text"])

    def test_a_silent_broker_clock_is_an_outage_not_an_emergency(self):
        memory = {}
        self.assertEqual(self.find(memory, market="unknown"), [])
        self.assertEqual(memory["clock_bad"], "2026-11-13T11:12:00")
        found = self.find(memory, now=NOW + 71 * MIN, market="unknown", events=[])
        self.assertEqual(keys(found), ["clock"])  # and not "silent": it cannot be told
        self.assertFalse(found[0]["serious"])
        self.find(memory, market="open")
        self.assertNotIn("clock_bad", memory)

    def test_a_fetch_that_hangs_and_one_that_died(self):
        hung = {
            "stage": "summarize",
            "label": "Summaries",
            "started": at(8),
            "updated": NOW,
        }
        found = self.find(progress=hung, fetching=True)
        self.assertEqual(keys(found), ["fetch:stuck"])
        self.assertIn(
            'running since 08:00 (192 minutes, now at "Summaries")', found[0]["text"]
        )
        # A briefing on local AI takes over an hour: that is not stuck.
        normal = dict(hung, started=at(10))
        self.assertEqual(self.find(progress=normal, fetching=True), [])
        dead = {
            "stage": "summarize",
            "label": "Summaries",
            "started": at(8),
            "updated": at(8, 40),
        }
        found = self.find(progress=dead, fetching=False)
        self.assertEqual(keys(found), ["fetch:died:2026-11-13T08:00:00"])
        self.assertIn("stopped midway", found[0]["text"])
        # Without knowing whether a fetcher runs, it is not called dead.
        self.assertEqual(self.find(progress=dead, fetching=None), [])
        self.assertEqual(self.find(progress=None), [])

    def test_a_schedule_that_is_not_loaded_or_stopped_running(self):
        loaded = {"com.pulse.scheduled-fetch": False, "com.pulse.live-signals": None}
        found = self.find(loaded=loaded)
        self.assertEqual(keys(found), ["scheduler:com.pulse.scheduled-fetch"])
        self.assertIn("is not loaded in launchd", found[0]["text"])
        found = self.find(stamp=at(9))
        self.assertEqual(keys(found), ["scheduler:stalled"])
        self.assertIn("has not run since 09:00", found[0]["text"])
        # A long run keeps the schedule busy, and at night it does not touch its stamp.
        self.assertEqual(self.find(stamp=at(9), fetching=True), [])
        self.assertEqual(
            self.find(now=at(23), stamp=at(20), market=None, session_start=None), []
        )
        self.assertEqual(self.find(stamp=at(20, day=12)), [])  # yesterday's stamp

    def test_crashes_and_panics_are_named(self):
        found = self.find(
            crashes=["pulse-2026-11-13-101010.ips"],
            panics=["crates/pulse-fetcher/src/article_text.rs:93"],
        )
        self.assertEqual(
            keys(found),
            [
                "crash:pulse-2026-11-13-101010.ips",
                "panic:crates/pulse-fetcher/src/article_text.rs:93:2026-11-13",
            ],
        )
        self.assertFalse(any(f["serious"] for f in found))

    def test_the_team_watches_itself(self):
        found = self.find(team_wake=at(8, 30))
        self.assertEqual(keys(found), ["team"])
        # A night shift that is still working has not missed its wake-up.
        self.assertEqual(self.find(team_wake=at(2, 30), team_running=True), [])
        self.assertEqual(self.find(team_wake=at(2, 30), team_running=None), [])
        found = self.find(listener=NOW - 20 * MIN)
        self.assertEqual(keys(found), ["listener"])
        self.assertEqual(self.find(listener=NOW - 20 * MIN, paired=False), [])

    def test_awake_since_follows_the_looks_back_to_the_first_gap(self):
        ticks = [at(8), at(8, 10), at(10, 40), at(10, 50), at(11), at(11, 10)]
        self.assertEqual(rules.awake_since(ticks, NOW), at(10, 40))
        self.assertEqual(rules.awake_since([], NOW), NOW)
        self.assertEqual(rules.awake_since([at(9)], NOW), NOW)
        # One missed look is not sleep.
        self.assertEqual(rules.awake_since([at(10, 30), at(10, 52)], NOW), at(10, 30))


class Alerts(unittest.TestCase):
    SILENT = {"key": "silent", "serious": True, "text": "Nobody looks."}
    LOADED = {"key": "scheduler:x", "serious": False, "text": "Not loaded."}

    def test_a_problem_is_texted_once_however_often_it_is_seen(self):
        memory = {}
        said = rules.watch_alerts([self.SILENT], {"events": []}, memory, NOW)
        self.assertEqual(said, [("Nobody looks.", True)])
        for n in range(1, 6):
            self.assertEqual(
                rules.watch_alerts(
                    [self.SILENT], {"events": []}, memory, NOW + n * 10 * MIN
                ),
                [],
            )

    def test_a_problem_that_comes_and_goes_is_still_one_text_a_day(self):
        memory = {}
        rules.watch_alerts([self.LOADED], {}, memory, NOW)
        self.assertEqual(rules.watch_alerts([], {}, memory, NOW + 10 * MIN), [])
        self.assertFalse(memory["alerts"]["scheduler:x"]["open"])
        self.assertEqual(
            rules.watch_alerts([self.LOADED], {}, memory, NOW + 20 * MIN), []
        )
        # Not serious: said again after a week, not after a day.
        self.assertEqual(
            rules.watch_alerts([self.LOADED], {}, memory, NOW + 25 * HOUR), []
        )
        self.assertEqual(
            rules.watch_alerts([self.LOADED], {}, memory, NOW + 8 * 24 * HOUR),
            [("Still not solved (first seen Fri 11:12). Not loaded.", False)],
        )

    def test_a_serious_problem_that_was_solved_and_is_back_is_said_again(self):
        memory = {}
        rules.watch_alerts([self.SILENT], {"events": []}, memory, NOW)
        ran = {"events": [("manage_start", NOW + 30 * MIN, "")]}
        self.assertEqual(
            rules.watch_alerts([], ran, memory, NOW + 40 * MIN),
            [("Solved. Pulse is looking at its positions again (run at 11:42).", False)],
        )
        # Three hours later it stops again: muted, the next "Solved" would be
        # about a problem nobody was told of.
        back = NOW + 3 * HOUR
        self.assertEqual(
            rules.watch_alerts([self.SILENT], ran, memory, back),
            [("It is back. Nobody looks.", True)],
        )
        self.assertEqual(
            rules.watch_alerts([self.SILENT], ran, memory, back + 10 * MIN), []
        )
        again = {"events": [*ran["events"], ("manage_start", back + 15 * MIN, "")]}
        self.assertEqual(
            rules.watch_alerts([], again, memory, back + 20 * MIN),
            [("Solved. Pulse is looking at its positions again (run at 14:27).", False)],
        )
        # Days later it is simply a new problem.
        self.assertEqual(
            rules.watch_alerts([self.SILENT], again, memory, NOW + 72 * HOUR),
            [("Nobody looks.", True)],
        )

    def test_a_serious_problem_that_lasts_is_repeated_once_a_day(self):
        memory = {}
        rules.watch_alerts([self.SILENT], {"events": []}, memory, NOW)
        self.assertEqual(
            rules.watch_alerts([self.SILENT], {"events": []}, memory, NOW + 23 * HOUR),
            [],
        )
        self.assertEqual(
            rules.watch_alerts([self.SILENT], {"events": []}, memory, NOW + 24 * HOUR),
            [("Still not solved (first seen Fri 11:12). Nobody looks.", True)],
        )

    def test_the_end_is_said_only_when_it_can_be_told(self):
        memory = {}
        rules.watch_alerts([self.SILENT], {"events": []}, memory, NOW)
        later = NOW + 5 * HOUR
        # No longer listed because the market closed: that is not "solved".
        self.assertEqual(rules.watch_alerts([], {"events": []}, memory, later), [])
        self.assertEqual(rules.watch_alerts([], {"events": None}, memory, later), [])
        self.assertTrue(memory["alerts"]["silent"]["open"])
        # Monday's first run is the end of it.
        monday = at(10, 2, day=16)
        ran = {"events": [("manage_start", at(10, day=16), "")]}
        self.assertEqual(
            rules.watch_alerts([], ran, memory, monday),
            [
                (
                    "Solved. Pulse is looking at its positions again (run at 10:00).",
                    False,
                )
            ],
        )
        self.assertEqual(rules.watch_alerts([], ran, memory, monday + 10 * MIN), [])

    def test_each_serious_problem_has_its_own_ending(self):
        sent = {
            "sent": "2026-11-13T11:12:00",
            "serious": True,
            "open": True,
            "name": "DFH",
        }
        later = at(12, 2)
        events = [
            ("manage_start", at(12), ""),
            ("manage_ok", at(12), ""),
            ("auto_ok", at(12, 1), ""),
            ("brief_done", at(12), "morning"),
        ]
        cases = {
            "stop:7": "Solved. DFH is no longer open without a stop: it has one now, or the "
            "position was closed.",
            "manage": "Solved. Position management works again (run at 12:00).",
            "auto": "Solved. Buying works again (run at 12:01).",
            "briefing:morning:2026-11-13": "Solved. The morning briefing was made after "
            "all, at 12:00.",
        }
        for key, text in cases.items():
            self.assertEqual(
                rules.watch_over(key, sent, {"events": events, "bare": []}, later), text
            )
        # Still bare, or the books unreadable: not over.
        still = {"events": events, "bare": [{"id": 7, "ticker": "DFH"}]}
        self.assertIsNone(rules.watch_over("stop:7", sent, still, later))
        self.assertIsNone(rules.watch_over("stop:7", sent, {"events": events}, later))
        # Things that happened before the text prove nothing.
        before = [(k, w - 2 * HOUR, d) for k, w, d in events]
        self.assertIsNone(rules.watch_over("manage", sent, {"events": before}, later))
        self.assertIsNone(
            rules.watch_over(
                "briefing:evening:2026-11-13", sent, {"events": events}, later
            )
        )

    def test_a_briefing_that_was_never_made_is_closed_as_that(self):
        key = "briefing:evening:2026-11-13"
        sent = {"sent": "2026-11-13T22:10:00", "serious": True, "open": True}
        nothing = {"events": [], "briefings": [at(8, 28)]}  # only the morning's
        # The 23:00 try may still fail after midnight: the evening is not over.
        self.assertIsNone(rules.watch_over(key, sent, nothing, at(1, 0, day=14)))
        self.assertEqual(
            rules.watch_over(key, sent, nothing, at(8, 2, day=14)),
            "The evening briefing of Fri 13 Nov was not made.",
        )
        # The next evening's briefing is not this one made "after all".
        next_day = {
            "events": [("brief_done", at(21, 34, day=14), "evening")],
            "briefings": [at(8, 28), at(8, 31, day=14), at(21, 34, day=14)],
        }
        self.assertEqual(
            rules.watch_over(key, sent, next_day, at(21, 42, day=14)),
            "The evening briefing of Fri 13 Nov was not made.",
        )
        # Made by the 23:00 try, even past midnight: that is the same evening.
        late = {"events": [("brief_done", at(0, 20, day=14), "evening")]}
        self.assertEqual(
            rules.watch_over(key, sent, late, at(8, 2, day=14)),
            "Solved. The evening briefing was made after all, at 00:20.",
        )
        # Made by the app or by hand that night: in the database, not in the log.
        by_hand = {"events": [], "briefings": [at(8, 28), at(23, 40)]}
        self.assertEqual(
            rules.watch_over(key, sent, by_hand, at(8, 2, day=14)),
            "Solved. The evening briefing was made after all, at Fri 23:40.",
        )
        # Without the database nobody knows: nothing is said until it is read.
        self.assertIsNone(rules.watch_over(key, sent, {"events": []}, at(8, 2, day=14)))
        # The whole way: said once in the morning, by day, and then forgotten.
        memory = {"alerts": {key: dict(sent, first=sent["sent"])}}
        self.assertEqual(
            rules.watch_alerts([], nothing, memory, at(8, 2, day=14)),
            [("The evening briefing of Fri 13 Nov was not made.", False)],
        )
        self.assertEqual(rules.watch_alerts([], nothing, memory, at(8, 12, day=14)), [])

    def test_a_problem_that_keeps_coming_and_going_goes_quiet_for_the_day(self):
        memory, said = {}, []
        start = at(10, 0)
        for n in range(6):  # found on the hour, solved half an hour later
            found_at = start + n * HOUR
            said += rules.watch_alerts([self.SILENT], {"events": []}, memory, found_at)
            ran = {"events": [("manage_start", found_at + 20 * MIN, "")]}
            said += rules.watch_alerts([], ran, memory, found_at + 30 * MIN)
        solved = ("Solved. Pulse is looking at its positions again (run at {}).", False)
        self.assertEqual(
            said,
            [
                ("Nobody looks.", True),
                (solved[0].format("10:20"), False),
                ("It is back. Nobody looks.", True),
                (solved[0].format("11:20"), False),
                ("It is back. Nobody looks.", True),
                (solved[0].format("12:20"), False),
                (
                    "It is back again. Nobody looks. It keeps coming and going, so I "
                    "will not text about it again until a day has passed.",
                    True,
                ),
            ],
        )
        self.assertFalse(memory["alerts"]["silent"]["open"])
        # A day after that last text it is a new problem, said plainly.
        next_day = at(13, 0, day=14)
        self.assertEqual(
            rules.watch_alerts([self.SILENT], {"events": []}, memory, next_day),
            [("Nobody looks.", True)],
        )
        self.assertNotIn("flaps", memory["alerts"]["silent"])

    def test_old_entries_leave_the_book(self):
        memory = {}
        rules.watch_alerts([self.LOADED], {}, memory, NOW)
        rules.watch_alerts([], {}, memory, NOW + 31 * 24 * HOUR)
        self.assertEqual(memory["alerts"], {})


def trace_line(when: dt.datetime, message: str, module: str = "pulse_fetcher") -> str:
    """A line as the fetcher writes it: UTC, with the terminal's colour codes."""
    utc = when.astimezone(dt.timezone.utc)
    return (
        f"\x1b[2m{utc:%Y-%m-%dT%H:%M:%S}.917140Z\x1b[0m \x1b[32m INFO\x1b[0m "
        f"\x1b[2m{module}\x1b[0m\x1b[2m:\x1b[0m {message}"
    )


def echo_line(when: dt.datetime, message: str) -> str:
    """A line as scheduled-fetch.sh writes it: `date`, this Mac's clock."""
    return f"{when:%a %b} {when.day:2d} {when:%H:%M:%S} EST {when.year} {message}"


class Reading(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.dir = Path(self.tmp.name).resolve()

    def test_the_lines_of_the_hourly_log_become_events(self):
        t = at(11)
        lines = [
            echo_line(t, "market open: managing paper positions"),
            trace_line(t, "Pulse fetcher starting in manage-positions mode"),
            trace_line(
                t, "Position mgmt: DRY_RUN — DFH broker stop would change: Place"
            ),
            trace_line(t, "Position management complete: 0 action(s)"),
            echo_line(t, "manage-positions failed (exit 101)"),
            trace_line(t, "Auto-trade complete: 0 buy(s) (previewed only if on)"),
            trace_line(
                t,
                "Auto-trade: market closed — no entries this run",
                "pulse_fetcher::pipeline",
            ),
            echo_line(t, "auto-trade failed (exit 1), next hour retries"),
            echo_line(at(9, 9, day=9), "morning briefing done"),
            echo_line(t, "evening briefing not made (exit 101), next hour retries"),
            echo_line(t, "starting morning briefing"),
            "thread 'main' panicked at somewhere",
            "",
        ]
        self.assertEqual(
            [e for e in map(watch.event_of, lines) if e],
            [
                ("manage_start", t, ""),
                ("manage_dry", t, ""),
                ("manage_ok", t, ""),
                ("manage_fail", t, "101"),
                ("auto_ok", t, ""),
                ("auto_fail", t, "1"),
                ("brief_done", at(9, 9, day=9), "morning"),
                ("brief_fail", t, "evening 101"),
            ],
        )

    def test_only_the_end_of_a_long_log_is_read(self):
        log = self.dir / "fetch-stdout.log"
        self.assertIsNone(watch.read_events(log))
        old = echo_line(at(10, day=1), "market open: managing paper positions")
        new = echo_line(at(11), "market open: managing paper positions")
        log.write_text(old + "\n" + "x" * 5000 + "\n" + new + "\n")
        self.assertEqual(len(watch.read_events(log)), 2)
        self.assertEqual(
            watch.read_events(log, tail=200), [("manage_start", at(11), "")]
        )
        # A tail that starts in the middle of a line drops that line.
        self.assertEqual(watch.read_events(log, tail=len(new) - 3), [])

    def test_the_progress_file_is_read_in_this_macs_time(self):
        path = self.dir / "fetch-progress.json"
        self.assertIsNone(watch.read_progress(path))
        started = at(8).astimezone(dt.timezone.utc)
        path.write_text(
            json.dumps(
                {
                    "stage": "summarize",
                    "stage_label": "Summaries",
                    "started_at": started.isoformat(),
                    "updated_at": (started + dt.timedelta(minutes=30)).isoformat(),
                }
            )
        )
        self.assertEqual(
            watch.read_progress(path),
            {
                "stage": "summarize",
                "label": "Summaries",
                "started": at(8),
                "updated": at(8, 30),
            },
        )
        # A failed run has no start.
        path.write_text(
            json.dumps({"stage": "failed", "updated_at": started.isoformat()})
        )
        self.assertIsNone(watch.read_progress(path)["started"])
        path.write_text("not json")
        self.assertIsNone(watch.read_progress(path))

    def test_positions_without_a_stop_are_read_and_the_books_only_read(self):
        db = self.dir / "pulse.db"
        self.assertIsNone(watch.read_bare(db))
        con = sqlite3.connect(db)
        con.executescript(
            """CREATE TABLE paper_trades (id INTEGER PRIMARY KEY, ticker TEXT, status TEXT,
                 order_status TEXT, stop_order_id TEXT, broker_stop_filled_at TEXT,
                 filled_qty REAL);
               INSERT INTO paper_trades VALUES
                 (1, 'SAFE', 'open', 'filled', 'stop-1', NULL, 10),
                 (2, 'BARE', 'open', 'filled', NULL, NULL, 10),
                 (3, 'EMPTY', 'open', 'filled', '', NULL, NULL),
                 (4, 'WAITING', 'open', 'pending', NULL, NULL, NULL),
                 (5, 'CLOSED', 'closed', 'filled', NULL, NULL, 10),
                 (6, 'STOPSOLD', 'open', 'filled', NULL, '2026-11-13', 10),
                 (7, 'TINY', 'open', 'filled', NULL, NULL, 0.4);"""
        )
        con.commit()
        con.close()
        before = db.read_bytes()
        self.assertEqual(
            watch.read_bare(db),
            [{"id": 2, "ticker": "BARE"}, {"id": 3, "ticker": "EMPTY"}],
        )
        self.assertEqual(db.read_bytes(), before)
        db.write_text("not a database")
        self.assertIsNone(watch.read_bare(db))

    def test_the_briefings_in_the_books_are_read_with_their_time_on_this_mac(self):
        db = self.dir / "pulse.db"
        now = at(11, 12)
        self.assertIsNone(watch.read_briefings(db, now))
        con = sqlite3.connect(db)
        utc = "%Y-%m-%d %H:%M:%S"
        made = [now - 3 * HOUR, now - 14 * HOUR, now - 5 * 24 * HOUR]
        stamps = [f"{t.astimezone(dt.timezone.utc):{utc}}" for t in made]
        con.executescript(
            """CREATE TABLE briefings (id INTEGER PRIMARY KEY, date TEXT, created_at TEXT,
                 story_count INTEGER, briefing_type TEXT);"""
        )
        con.executemany(
            "INSERT INTO briefings (created_at, story_count, briefing_type) VALUES (?, ?, ?)",
            [
                (stamps[0], 120, "daily"),
                (stamps[1], 140, "daily"),
                (stamps[2], 120, "daily"),  # older than the watch looks back
                (stamps[0], 0, "daily"),  # a run that made nothing
                (stamps[0], 9, "freedoms"),  # not a daily briefing
            ],
        )
        con.commit()
        con.close()
        before = db.read_bytes()
        self.assertEqual(watch.read_briefings(db, now), [made[1], made[0]])
        self.assertEqual(db.read_bytes(), before)
        self.assertEqual([p.name for p in self.dir.iterdir()], ["pulse.db"])
        # A database from before the briefings table: unknown, not "none made".
        db.unlink()
        sqlite3.connect(db).close()
        self.assertIsNone(watch.read_briefings(db, now))

    def test_reading_the_books_leaves_no_file_next_to_them(self):
        """Pulse's database uses a write-ahead log, as the installed one does."""
        db = self.dir / "pulse.db"
        con = sqlite3.connect(db)
        self.addCleanup(con.close)
        con.execute("PRAGMA journal_mode = WAL")
        con.executescript(
            """CREATE TABLE paper_trades (id INTEGER PRIMARY KEY, ticker TEXT, status TEXT,
                 order_status TEXT, stop_order_id TEXT, broker_stop_filled_at TEXT,
                 filled_qty REAL);
               INSERT INTO paper_trades VALUES (1, 'BARE', 'open', 'filled', NULL, NULL, 10);"""
        )
        con.commit()
        # Pulse has it open and wrote something that is still only in its log.
        con.execute(
            "INSERT INTO paper_trades VALUES (2, 'NEW', 'open', 'filled', NULL, NULL, 5)"
        )
        con.commit()
        self.assertTrue((self.dir / "pulse.db-wal").exists())
        self.assertEqual(
            watch.read_bare(db),
            [{"id": 1, "ticker": "BARE"}, {"id": 2, "ticker": "NEW"}],
        )
        # Pulse closed it: the log is gone, and the watch does not bring it back.
        con.close()
        self.assertEqual([p.name for p in self.dir.iterdir()], ["pulse.db"])
        self.assertEqual(len(watch.read_bare(db)), 2)
        self.assertEqual([p.name for p in self.dir.iterdir()], ["pulse.db"])

    def fetcher(self, code: int) -> Path:
        path = self.dir / "pulse-fetcher"
        path.write_text(
            f'#!/bin/sh\n[ "$1 $2" = "--mode market-open" ] || exit 9\nexit {code}\n'
        )
        path.chmod(0o755)
        return path

    def test_the_market_is_asked_of_the_broker_only_in_trading_hours(self):
        def et(hour, minute=0, day=13):
            return watch.local(
                dt.datetime(2026, 11, day, hour, minute, tzinfo=watch.ET)
            )

        for code, state in ((0, "open"), (3, "closed"), (1, "unknown")):
            self.assertEqual(
                watch.market(et(11, 10), self.fetcher(code)), (state, et(10))
            )
        asked = []

        def run(cmd, **kw):
            asked.append(cmd)
            return subprocess.CompletedProcess(cmd, 0)

        for when in (
            et(9, 59),
            et(16),
            et(11, day=14),
            et(11, day=15),
        ):  # early, late, weekend
            self.assertEqual(watch.market(when, self.fetcher(0), run), (None, None))
        self.assertEqual(watch.market(et(11), self.dir / "missing", run), (None, None))
        self.assertEqual(asked, [])

        def hangs(cmd, **kw):
            raise subprocess.TimeoutExpired(cmd, 90)

        self.assertEqual(
            watch.market(et(11), self.fetcher(0), hangs), ("unknown", et(10))
        )

    def test_launchd_and_processes_are_asked_without_guessing(self):
        codes = {"com.pulse.scheduled-fetch": 0, "com.pulse.live-signals": 113}

        def run(cmd, **kw):
            return subprocess.CompletedProcess(cmd, codes[cmd[-1].rsplit("/", 1)[-1]])

        self.assertEqual(
            watch.read_loaded(rules.SCHEDULERS, run),
            {"com.pulse.scheduled-fetch": True, "com.pulse.live-signals": False},
        )
        codes["com.pulse.live-signals"] = 5  # launchctl itself failed
        self.assertIsNone(
            watch.read_loaded(rules.SCHEDULERS, run)["com.pulse.live-signals"]
        )

        def pgrep(code):
            return lambda cmd, **kw: subprocess.CompletedProcess(cmd, code)

        self.assertIs(watch.alive(["-x", "pulse-fetcher"], pgrep(0)), True)
        self.assertIs(watch.alive(["-x", "pulse-fetcher"], pgrep(1)), False)
        self.assertIsNone(watch.alive(["-x", "pulse-fetcher"], pgrep(2)))

        def broken(cmd, **kw):
            raise OSError("no pgrep")

        self.assertIsNone(watch.alive(["-x", "pulse-fetcher"], broken))

    def test_a_panic_is_reported_once_and_old_ones_never(self):
        log = self.dir / "fetch-stderr.log"
        panic = (
            "\nthread 'main' (97020649) panicked at crates/pulse-fetcher/src/article_text.rs:93:28:\n"
            "start byte index 172532 is not a char boundary\n"
        )
        log.write_text(panic)
        memory = {}
        missing = self.dir / "live-stderr.log"
        self.assertEqual(watch.read_panics([log, missing], memory), [])  # history
        self.assertEqual(watch.read_panics([log, missing], memory), [])
        with open(log, "a") as f:
            f.write(panic + panic.replace("article_text.rs:93", "main.rs:7"))
        self.assertEqual(
            watch.read_panics([log, missing], memory),
            [
                "crates/pulse-fetcher/src/article_text.rs:93",
                "crates/pulse-fetcher/src/main.rs:7",
            ],
        )
        self.assertEqual(watch.read_panics([log, missing], memory), [])
        log.write_text(panic)  # a new, shorter file is read from its start
        self.assertEqual(
            watch.read_panics([log, missing], memory),
            ["crates/pulse-fetcher/src/article_text.rs:93"],
        )

    def test_only_fresh_crash_reports_of_pulse_count(self):
        folder = self.dir / "DiagnosticReports"
        self.assertEqual(watch.read_crashes(folder, NOW), [])
        folder.mkdir()
        names = [
            "pulse-2026-11-13-101010.ips",
            "pulse-fetcher-2026-11-13-101011.ips",
            "pulse-2026-11-01-101010.ips",
            "pulseaudio-2026-11-13-101010.ips",
            "Safari-2026-11-13-101010.ips",
            "pulse-2026-11-13-101010.txt",
        ]
        for name in names:
            (folder / name).write_text("x")
        old = (NOW - 3 * 24 * HOUR).timestamp()
        os.utime(folder / names[2], (old, old))
        fresh = (NOW - HOUR).timestamp()
        for name in (*names[:2], *names[3:]):
            os.utime(folder / name, (fresh, fresh))
        self.assertEqual(watch.read_crashes(folder, NOW), names[:2])


class Look(unittest.TestCase):
    """One look from end to end, on a made-up install."""

    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        root = Path(self.tmp.name).resolve()
        self.home, self.app, self.logs = root / "team", root / "app", root / "logs"
        for d in (self.home, self.app / "schedule", self.logs, root / "crashes"):
            d.mkdir(parents=True)
        for name, value in {
            "HOME": self.home,
            "APP": self.app,
            "LOGS": self.logs,
            "CRASHES": root / "crashes",
            "MEMORY": self.home / "watch.json",
            "QUEUE": self.home / "queue",
            "INCIDENTS_OFF": self.home / "incidents-off",
            "FETCHER": root / "pulse-fetcher",
        }.items():
            self.addCleanup(setattr, watch, name, getattr(watch, name))
            setattr(watch, name, value)
        watch.FETCHER.write_text("#!/bin/sh\nexit 0\n")
        watch.FETCHER.chmod(0o755)
        self.addCleanup(chat.set_home, chat.HOME)
        chat.set_home(self.home)
        self.sent, self.banners, self.reply = [], [], "sent"
        for name, fake in {
            "send": lambda text, serious=False, now=None: (
                self.sent.append((text, serious)) or self.reply
            ),
            "banner": lambda text: self.banners.append(text),
        }.items():
            self.addCleanup(setattr, chat, name, getattr(chat, name))
            setattr(chat, name, fake)
        self.addCleanup(setattr, watch, "log", watch.log)
        watch.log = lambda msg: None
        self.log = self.logs / "fetch-stdout.log"
        self.log.write_text("")

    @staticmethod
    def et(hour, minute=0):
        return watch.local(dt.datetime(2026, 11, 13, hour, minute, tzinfo=watch.ET))

    @staticmethod
    def fake_run(cmd, **kw):
        """launchctl says loaded, pgrep says nothing runs, the market is open."""
        code = 1 if cmd[0].endswith("pgrep") else 0
        return subprocess.CompletedProcess(cmd, code)

    def write(self, *lines):
        with open(self.log, "a") as f:
            f.write("\n".join(lines) + "\n")

    def looks(self, start, end, step=10):
        when = start
        while when <= end:
            found = watch.look(when, run=self.fake_run)
            when += dt.timedelta(minutes=step)
        return found

    def test_a_morning_without_position_runs_is_texted_once_and_then_solved(self):
        self.write(
            echo_line(self.et(10), "market open: managing paper positions"),
            trace_line(self.et(10), "Position management complete: 0 action(s)"),
        )
        self.assertEqual(self.looks(self.et(10, 2), self.et(12, 2)), [])
        self.assertEqual(self.sent, [])
        found = watch.look(self.et(12, 12), run=self.fake_run)
        self.assertEqual(keys(found), ["silent"])
        self.assertEqual(len(self.sent), 1)
        text, serious = self.sent[0]
        self.assertTrue(serious)
        self.assertTrue(
            text.startswith(
                "Serious problem in Pulse. The market is open and Pulse has not looked at "
                "its positions for 130 minutes (last at "
            ),
            text,
        )
        self.looks(self.et(12, 22), self.et(12, 52))
        self.assertEqual(len(self.sent), 1)
        self.write(echo_line(self.et(13), "market open: managing paper positions"))
        self.assertEqual(watch.look(self.et(13, 2), run=self.fake_run), [])
        self.assertEqual(len(self.sent), 2)
        self.assertEqual(self.sent[1][1], False)
        self.assertTrue(
            self.sent[1][0].startswith(
                "Pulse watch: Solved. Pulse is looking at its positions again (run at "
            )
        )
        self.assertEqual(self.banners, [])
        memory = json.loads(watch.MEMORY.read_text())
        self.assertEqual(memory["found"], [])
        self.assertIn("nothing wrong", watch.as_text(memory))

    def test_a_mac_that_just_woke_up_alarms_about_nothing(self):
        self.assertEqual(watch.look(self.et(13, 30), run=self.fake_run), [])
        self.assertEqual(self.sent, [])

    def test_without_a_bot_a_serious_problem_is_a_notification_on_the_mac(self):
        self.reply = "off"
        self.looks(self.et(10, 2), self.et(12, 12))
        self.assertEqual(len(self.banners), 1)
        self.assertIn("Serious problem in Pulse.", self.banners[0])
        self.assertIn("SERIOUS: The market is open", watch.as_text(watch.load()))

    def test_a_dry_look_texts_nothing_and_leaves_the_notebook_alone(self):
        self.looks(self.et(10, 2), self.et(12, 2))
        before = watch.MEMORY.read_text()
        found = watch.look(self.et(12, 12), tell=False, run=self.fake_run)
        self.assertEqual(keys(found), ["silent"])
        self.assertEqual((self.sent, watch.MEMORY.read_text()), ([], before))

    def test_what_the_watch_cannot_read_is_not_a_problem(self):
        self.log.unlink()
        watch.FETCHER.unlink()

        def broken(cmd, **kw):
            raise OSError("nothing works")

        when = self.et(10, 2)
        while when <= self.et(13):
            self.assertEqual(watch.look(when, run=broken), [])
            when += dt.timedelta(minutes=10)
        self.assertEqual(self.sent, [])

    def test_two_looks_never_run_at_once(self):
        import fcntl

        looked = []
        self.addCleanup(setattr, watch, "look", watch.look)
        watch.look = lambda **kw: looked.append(kw) or []
        with open(self.home / "watch.lock", "w") as lock:
            fcntl.flock(lock, fcntl.LOCK_EX)
            self.assertEqual(watch.main([]), 0)
            self.assertEqual(looked, [])
        self.assertEqual(watch.main([]), 0)
        self.assertEqual(looked, [{"tell": True}])

    def test_a_look_that_breaks_is_one_line_in_the_log_not_a_crash(self):
        said = []
        watch.log = said.append
        self.addCleanup(setattr, watch, "look", watch.look)

        def breaks(**kw):
            raise ValueError("boom")

        watch.look = breaks
        self.assertEqual(watch.main([]), 1)
        self.assertEqual(said, ["the look failed: ValueError: boom"])


class Wiring(unittest.TestCase):
    """How the runner installs and shows the watch."""

    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.home = Path(self.tmp.name).resolve()
        saved = os.environ.get("PULSE_TEAM_HOME")
        os.environ["PULSE_TEAM_HOME"] = str(self.home)
        self.addCleanup(chat.set_home, chat.HOME)
        self.team = importlib.reload(importlib.import_module("team"))
        if saved is None:
            os.environ.pop("PULSE_TEAM_HOME", None)
        else:
            os.environ["PULSE_TEAM_HOME"] = saved

    def test_the_watch_runs_every_ten_minutes_from_the_installed_copy(self):
        job = self.team.watch_job("/usr/bin/python3", self.home / "bin")
        self.assertEqual(job["Label"], "com.pulse.team.watch")
        self.assertEqual(
            job["ProgramArguments"],
            ["/usr/bin/python3", str(self.home / "bin/watch.py")],
        )
        self.assertEqual(job["StartInterval"], 600)
        self.assertEqual(
            set(job["EnvironmentVariables"]), {"PATH", "HOME", "PULSE_TEAM_HOME"}
        )

    def test_the_night_page_lists_what_the_watch_sees(self):
        import page

        report = {
            "watch": [
                {"serious": True, "text": "DFH has no stop at the broker."},
                {"serious": False, "text": "The live signals is not loaded."},
            ]
        }
        self.assertEqual(
            page.needs_you(report),
            [
                "Serious, in the running app: DFH has no stop at the broker.",
                "In the running app: The live signals is not loaded.",
            ],
        )


if __name__ == "__main__":
    unittest.main()
