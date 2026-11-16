"""Tests for the team's turns by day, its incident jobs and the stop button.
python3 -m unittest discover -s scripts/team"""

import contextlib
import datetime as dt
import importlib
import io
import json
import os
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import chat  # noqa: E402
import page  # noqa: E402
import rules  # noqa: E402
import watch  # noqa: E402

OWNER = "nicola"
NOW = dt.datetime(2026, 11, 16, 14, 30)


def at(text: str) -> dt.datetime:
    return dt.datetime.fromisoformat(text)


def trigger(key="manage", text="Position management failed twice.", when=NOW, **more):
    return {
        "key": key,
        "text": text,
        "evidence": "the log",
        "at": when.isoformat(timespec="seconds"),
        "file": f"{when:%H%M%S}-{key.replace(':', '-')}.json",
        **more,
    }


class WhatAnHourIsFor(unittest.TestCase):
    """launchd wakes the team every hour; `shift_due` says what for."""

    def due(self, when, night="", state=None, **over):
        facts = dict(paused=False, incident=False, active=False, day_budget=120)
        facts.update(over)
        return rules.shift_due(at(when), night, state or {}, **facts)

    def test_the_night_is_the_night_whatever_else_is_true(self):
        self.assertEqual(self.due("2026-11-16T02:30", night="2026-11-16"), "night")
        self.assertEqual(
            self.due(
                "2026-11-16T02:30", night="2026-11-16", incident=True, active=True
            ),
            "night",
        )

    def test_a_serious_problem_is_worked_on_at_any_hour(self):
        self.assertEqual(self.due("2026-11-16T23:30", incident=True), "incident")
        self.assertEqual(
            self.due("2026-11-16T04:30", incident=True, active=True), "incident"
        )

    def test_light_work_only_by_day(self):
        self.assertEqual(self.due("2026-11-16T08:30"), "")
        self.assertEqual(self.due("2026-11-16T09:30"), "day")
        self.assertEqual(self.due("2026-11-16T20:30"), "day")
        self.assertEqual(self.due("2026-11-16T21:30"), "")

    def test_by_day_it_steps_aside_for_nicola(self):
        self.assertEqual(self.due("2026-11-16T10:30", active=True), "")

    def test_the_day_has_a_time_budget(self):
        state = {}
        rules.add_day_minutes(state, dt.date(2026, 11, 16), 70)
        self.assertEqual(self.due("2026-11-16T10:30", state=state), "day")
        rules.add_day_minutes(state, dt.date(2026, 11, 16), 50)
        self.assertEqual(rules.day_used(state, dt.date(2026, 11, 16)), 120)
        self.assertEqual(self.due("2026-11-16T11:30", state=state), "")
        # a new day starts from nothing
        self.assertEqual(self.due("2026-11-17T09:30", state=state), "day")
        self.assertEqual(rules.day_used(state, dt.date(2026, 11, 17)), 0)

    def test_a_budget_that_cannot_be_read_counts_as_unused(self):
        for bad in ({"day": []}, {"day": {"date": "2026-11-16", "minutes": "many"}}):
            self.assertEqual(rules.day_used(bad, dt.date(2026, 11, 16)), 0)

    def test_paused_it_is_for_nothing(self):
        for facts in (
            dict(night="2026-11-16"),
            dict(incident=True),
            dict(),
        ):
            self.assertEqual(self.due("2026-11-16T10:30", paused=True, **facts), "")


class SteppingAside(unittest.TestCase):
    """When the job that is running has to stop and keep its work."""

    def why(self, kind="idea", shift="night", **over):
        facts = dict(paused=False, incident=False, active=False, over_budget=False)
        facts.update(over)
        return rules.yield_reason(kind, shift, **facts)

    def test_nothing_stops_a_job_without_a_reason(self):
        for shift in ("night", "day", "incident"):
            self.assertEqual(self.why(shift=shift), "")

    def test_pause_stops_everything(self):
        for kind in ("idea", "pr", "main", "incident"):
            self.assertEqual(self.why(kind, paused=True), "paused from Telegram")

    def test_a_serious_problem_comes_before_any_other_job(self):
        for shift in ("night", "day"):
            self.assertIn("serious problem", self.why(shift=shift, incident=True))
        # one incident does not interrupt another
        self.assertEqual(self.why("incident", incident=True), "")

    def test_only_the_day_steps_aside_for_nicola_and_for_its_budget(self):
        self.assertIn("Nicola", self.why(shift="day", active=True))
        self.assertIn("used up", self.why(shift="day", over_budget=True))
        self.assertEqual(self.why(shift="night", active=True, over_budget=True), "")
        self.assertEqual(
            self.why("incident", shift="incident", active=True, over_budget=True), ""
        )


class WhoIsWorking(unittest.TestCase):
    """Nicola is active when one of his own session logs changed just now."""

    HOME = "/Users/jdoe"
    OWN = (
        "/Users/jdoe/dev/Pulse/.claude/worktrees/team-job",
        "/Users/jdoe/.pulse-team",
    )
    T = 1_800_000_000.0

    def active(self, *logs, minutes=10):
        return rules.owner_active(list(logs), self.T, self.HOME, self.OWN, minutes)

    def test_the_folder_name_is_the_path_with_dashes(self):
        self.assertEqual(
            rules.project_folder("/Users/jdoe/dev/Pulse/.claude/worktrees/team-job"),
            "-Users-jdoe-dev-Pulse--claude-worktrees-team-job",
        )

    def test_a_session_of_his_own_counts_while_it_is_fresh(self):
        self.assertTrue(self.active(("-Users-jdoe", self.T - 30)))
        self.assertTrue(self.active(("-Users-jdoe-dev-quant", self.T - 9 * 60)))
        self.assertFalse(self.active(("-Users-jdoe-dev-quant", self.T - 11 * 60)))
        self.assertFalse(self.active())

    def test_the_teams_own_sessions_do_not_count(self):
        self.assertFalse(
            self.active(
                ("-Users-jdoe-dev-Pulse--claude-worktrees-team-job", self.T - 5),
                ("-Users-jdoe--pulse-team-page", self.T - 5),
            )
        )
        # the main checkout is his
        self.assertTrue(self.active(("-Users-jdoe-dev-Pulse", self.T - 5)))

    def test_sessions_other_programs_start_do_not_count(self):
        self.assertFalse(
            self.active(
                ("-private-var-folders-yc-T-compass-claude-0b7ytbi1", self.T - 5),
                ("-private-tmp-pulse-web-abc", self.T - 5),
                (
                    "-Users-jdoefriend",
                    self.T - 5,
                ),  # another user whose name starts alike
            )
        )

    def test_a_log_stamped_in_the_future_is_not_activity(self):
        self.assertFalse(self.active(("-Users-jdoe", self.T + 3600)))


class Incidents(unittest.TestCase):
    """The queue of serious problems, and which of them become a job."""

    def jobs(self, triggers, state=None, now=NOW, **kw):
        return rules.incident_jobs(
            triggers, state if state is not None else {}, now, **kw
        )

    def test_a_trigger_becomes_a_job_that_carries_what_was_seen(self):
        (job,), dropped = self.jobs([trigger()])
        self.assertEqual(dropped, [])
        self.assertEqual(job["kind"], "incident")
        self.assertEqual(job["key"], "incident-manage-11161430")
        self.assertEqual(job["problem"], "manage")
        self.assertEqual(job["title"], "Position management failed twice.")
        self.assertEqual(job["evidence"], "the log")
        self.assertTrue(job["file"].endswith(".json"))

    def test_the_oldest_goes_first(self):
        early = trigger("auto", when=NOW - dt.timedelta(hours=1))
        jobs, _ = self.jobs([trigger("manage"), early])
        self.assertEqual([j["problem"] for j in jobs], ["auto", "manage"])

    def test_what_cannot_be_read_is_thrown_away_with_the_reason(self):
        bad = [
            {"file": "a.json"},
            dict(trigger(), key=""),
            dict(trigger(), text=None),
            dict(trigger(), at="yesterday"),
            dict(trigger(), at="2026-11-16T14:30:00+00:00"),
            dict(trigger(), key=["manage"]),
        ]
        jobs, dropped = self.jobs(bad)
        self.assertEqual(jobs, [])
        self.assertEqual([d["why"] for d in dropped], ["it could not be read"] * 6)
        self.assertEqual(
            dropped[0],
            {
                "kind": "incident",
                "key": "",  # it never became a job
                "file": "a.json",
                "title": "a trigger",
                "why": "it could not be read",
            },
        )

    def test_a_trigger_older_than_a_day_is_dropped(self):
        old = trigger(when=NOW - dt.timedelta(hours=25))
        jobs, dropped = self.jobs([old])
        self.assertEqual((jobs, dropped[0]["why"]), ([], "it is more than a day old"))

    def test_one_job_per_problem_per_day(self):
        again = trigger(when=NOW + dt.timedelta(minutes=10))
        jobs, dropped = self.jobs(
            [trigger(), again], now=NOW + dt.timedelta(minutes=20)
        )
        self.assertEqual(len(jobs), 1)
        self.assertEqual(dropped[0]["why"], "this problem is in the queue already")
        state = {}
        rules.record_incident(state, "manage", NOW.date())
        jobs, dropped = self.jobs([trigger()], state)
        self.assertEqual(
            (jobs, dropped[0]["why"]), ([], "this problem was already worked on today")
        )
        # the next day it is a new one
        tomorrow = NOW + dt.timedelta(days=1)
        jobs, _ = self.jobs([trigger(when=tomorrow)], state, now=tomorrow)
        self.assertEqual(len(jobs), 1)

    def test_two_incident_jobs_a_day_at_most(self):
        three = [trigger("manage"), trigger("auto"), trigger("silent")]
        jobs, dropped = self.jobs(three)
        self.assertEqual([j["problem"] for j in jobs], ["manage", "auto"])
        self.assertEqual(dropped[0]["why"], "2 incidents a day is the limit")
        state = {}
        rules.record_incident(state, "stop:7", NOW.date())
        rules.record_incident(state, "auto", NOW.date())
        self.assertEqual(rules.incidents_today(state, NOW.date()), ["stop:7", "auto"])
        jobs, dropped = self.jobs([trigger("manage")], state)
        self.assertEqual(jobs, [])

    def test_a_job_that_stepped_aside_goes_on_and_is_not_a_second_one(self):
        state = {}
        rules.record_incident(state, "manage", NOW.date())
        rules.record_incident(state, "auto", NOW.date())
        rules.hold(state, "incident-manage-11161430", "", NOW)
        jobs, dropped = self.jobs(
            [trigger("manage")], state, now=NOW + dt.timedelta(hours=1)
        )
        self.assertEqual([j["key"] for j in jobs], ["incident-manage-11161430"])
        self.assertEqual(dropped, [])

    def test_the_text_is_one_line_and_the_evidence_is_capped(self):
        long = trigger(text="a\nb   c\n" + "x" * 500, evidence="e" * 20000)
        (job,), _ = self.jobs([long])
        self.assertNotIn("\n", job["title"])
        self.assertTrue(job["title"].startswith("a b c"))
        self.assertLessEqual(len(job["title"]), 300)
        self.assertEqual(len(job["evidence"]), rules.EVIDENCE_MAX)


class Holds(unittest.TestCase):
    """The note kept for a job that stepped aside."""

    def test_a_held_job_goes_on_from_its_branch(self):
        state = {}
        rules.hold(state, "idea-x", "feat/night-x-1116", NOW)
        self.assertEqual(rules.held_branch(state, "idea-x", NOW), "feat/night-x-1116")
        self.assertEqual(rules.held_branch(state, "idea-y", NOW), "")
        rules.release(state, "idea-x")
        self.assertEqual(rules.held_branch(state, "idea-x", NOW), "")
        rules.release(state, "idea-x")  # twice is fine

    def test_a_job_with_nothing_to_keep_still_waits(self):
        state = {}
        rules.hold(state, "incident-a", "", NOW)
        self.assertIsNotNone(rules.waiting(state, "incident-a", NOW))
        self.assertEqual(rules.held_branch(state, "incident-a", NOW), "")

    def test_an_old_note_no_longer_counts_and_is_thrown_away(self):
        state = {}
        rules.hold(state, "idea-old", "feat/night-old", NOW - dt.timedelta(days=8))
        self.assertEqual(rules.held_branch(state, "idea-old", NOW), "")
        rules.hold(state, "idea-new", "feat/night-new", NOW)
        self.assertEqual(list(state["held"]), ["idea-new"])

    def test_a_book_that_cannot_be_read_is_started_again(self):
        for bad in (
            {"held": []},
            {"held": {"idea-x": "branch"}},
            {"held": {"idea-x": {"at": 5}}},
        ):
            self.assertEqual(rules.held_branch(bad, "idea-x", NOW), "")
            self.assertIsNone(rules.waiting(bad, "idea-x", NOW))
        state = {"held": []}
        rules.hold(state, "idea-x", "b", NOW)
        self.assertEqual(rules.held_branch(state, "idea-x", NOW), "b")
        rules.release({"held": []}, "idea-x")


class DayQueue(unittest.TestCase):
    """What a turn takes on, by kind of turn."""

    IDEAS = [
        {"text": "a new page for earnings", "state": "open"},
        {"text": "fix: the risk card shows NaN", "state": "open"},
        {"text": "Fix:   a typo in the sidebar", "state": "open"},
        {"text": "fix: done already", "state": "done"},
    ]

    def select(self, shift, main_ok=True, prs=(), incidents=None, state=None):
        return rules.select_jobs(
            main_ok,
            list(prs),
            self.IDEAS,
            state or {},
            OWNER,
            "main",
            "needs-you",
            "m1",
            incidents=incidents,
            shift=shift,
        )

    def pr(self, n):
        return {
            "number": n,
            "title": f"PR {n}",
            "author": {"login": OWNER},
            "isCrossRepository": False,
            "isDraft": False,
            "baseRefName": "main",
            "headRefName": f"feat/{n}",
            "headRefOid": f"sha{n}",
            "labels": [],
            "createdAt": f"2026-10-{n:02d}",
            "body": "",
        }

    def test_what_a_small_fix_is(self):
        self.assertTrue(rules.small_fix("fix: the risk card shows NaN"))
        self.assertTrue(rules.small_fix("  FIX:a typo"))
        for other in (
            "fix the trading engine",
            "prefix: x",
            "a new page; fix: it",
            "fix:",
            "fix:   ",
        ):
            self.assertFalse(rules.small_fix(other), other)

    def test_by_day_only_the_small_fixes(self):
        jobs, skipped = self.select("day", prs=[self.pr(3)])
        self.assertEqual(
            [j["title"] for j in jobs],
            ["fix: the risk card shows NaN", "Fix:   a typo in the sidebar"],
        )
        self.assertEqual(
            skipped, []
        )  # a pull request is not skipped: it waits for the night

    def test_by_night_everything_as_before(self):
        jobs, _ = self.select("night", prs=[self.pr(3)])
        self.assertEqual([j["kind"] for j in jobs], ["pr", "idea", "idea", "idea"])

    def test_by_day_nothing_is_built_on_a_main_not_known_to_pass(self):
        self.assertEqual(self.select("day", main_ok=False), ([], []))

    def test_an_incident_goes_first_also_before_a_red_main(self):
        inc = [{"kind": "incident", "key": "incident-a", "title": "A", "sha": ""}]
        jobs, _ = self.select("night", incidents=inc, prs=[self.pr(3)])
        self.assertEqual([j["kind"] for j in jobs][:2], ["incident", "pr"])
        jobs, _ = self.select("night", main_ok=False, incidents=inc)
        self.assertEqual([j["kind"] for j in jobs], ["incident", "main"])
        jobs, _ = self.select("day", main_ok=False, incidents=inc)
        self.assertEqual([j["kind"] for j in jobs], ["incident"])

    def test_a_turn_for_an_incident_does_that_and_nothing_else(self):
        inc = [{"kind": "incident", "key": "incident-a", "title": "A", "sha": ""}]
        jobs, skipped = self.select("incident", incidents=inc, prs=[self.pr(3)])
        self.assertEqual(([j["key"] for j in jobs], skipped), (["incident-a"], []))
        self.assertEqual(self.select("incident", main_ok=False), ([], []))

    def test_a_red_main_that_is_stuck_does_not_hide_the_incident(self):
        state = {}
        rules.record_attempt(state, "main", "m1", "blocked")
        inc = [{"kind": "incident", "key": "incident-a", "title": "A", "sha": ""}]
        jobs, skipped = self.select("night", main_ok=False, incidents=inc, state=state)
        self.assertEqual([j["kind"] for j in jobs], ["incident"])
        self.assertEqual(skipped[0]["kind"], "main")


class TradingFiles(unittest.TestCase):
    def test_what_decides_how_pulse_trades(self):
        changes = [
            ("M", "crates/pulse-fetcher/src/position_management.rs"),
            ("M", "crates/pulse-fetcher/src/pipeline/trading.rs"),
            ("A", "crates/pulse-alpaca/src/stops.rs"),
            ("M", "crates/pulse-weights/src/lib.rs"),
            ("M", "src-tauri/src/services/paper_trading.rs"),
            ("M", "crates/pulse-snapshot/src/ledger.rs"),
        ]
        out = rules.trading_violations(changes)
        self.assertEqual(len(out), len(changes))
        self.assertEqual(
            out[0],
            "changes how Pulse trades: crates/pulse-fetcher/src/position_management.rs",
        )

    def test_where_the_switches_for_trading_are_counts_as_trading(self):
        # The fetcher's entry point and its pipeline choose which trading runs
        # and whether it is a dry run, next to code that has nothing to do with it.
        changes = [
            ("M", "crates/pulse-fetcher/src/main.rs"),
            ("M", "crates/pulse-fetcher/src/pipeline/mod.rs"),
            ("M", "crates/pulse-fetcher/src/candidates.rs"),
            ("M", "crates/pulse-fetcher/src/market_prices.rs"),
            ("M", "crates/pulse-snapshot/src/risk.rs"),
        ]
        self.assertEqual(len(rules.trading_violations(changes)), len(changes))

    def test_the_rest_of_pulse_is_not_trading(self):
        changes = [
            ("M", "crates/pulse-fetcher/src/article_text.rs"),
            ("M", "crates/pulse-fetcher/src/pipeline/notify.rs"),
            ("M", "crates/pulse-fetcher/src/sources/reddit.rs"),
            ("M", "src/routes/signals/+page.svelte"),
            ("M", "crates/pulse-snapshot/src/explain.rs"),
            ("A", "docs/trading/notes.md"),
        ]
        self.assertEqual(rules.trading_violations(changes), [])

    def test_an_incident_branch_is_one_of_the_teams_own(self):
        self.assertTrue(
            "fix/night-incident-manage-11161430".startswith(rules.TEAM_PREFIXES)
        )
        self.assertFalse(
            "fix/night-incident-x".startswith(rules.MAIN_FIX_PREFIX)
        )  # never mistaken for a fix of main


class LogForASession(unittest.TestCase):
    """A session is never handed a key: the log's end is cleaned first."""

    def test_keys_in_addresses_and_settings_are_removed(self):
        text = "\n".join(
            [
                "GET https://finnhub.io/api/v1/quote?symbol=AAPL&token=abcd1234 failed",
                "ALPACA_SECRET_KEY=PKabcdefghij0123 was used",
                "Authorization: Bearer abc.def.ghi",
                'header "x-api-key": "sk-live-0001"',
                "https://user:hunter2@example.com/feed",
                "password = swordfish",
            ]
        )
        out = rules.redact_log(text)
        for secret in (
            "abcd1234",
            "PKabcdefghij0123",
            "abc.def.ghi",
            "sk-live-0001",
            "hunter2",
            "swordfish",
        ):
            self.assertNotIn(secret, out)
        self.assertIn("symbol=AAPL", out)
        self.assertIn("token=[removed]", out)

    def test_anything_long_enough_to_be_a_key_is_removed(self):
        out = rules.redact_log("sent with 9f8e7d6c5b4a39281706f5e4d3c2b1a0 ok")
        self.assertEqual(out, "sent with [removed] ok")

    def test_what_a_cause_is_found_with_stays(self):
        line = (
            "thread 'main' panicked at crates/pulse-fetcher/src/position_management.rs:412:17: "
            "byte index 7 is not a char boundary"
        )
        self.assertEqual(rules.redact_log(line), line)
        self.assertEqual(
            rules.redact_log("manage-positions failed (exit 101)"),
            "manage-positions failed (exit 101)",
        )

    def test_colour_codes_the_home_folder_and_control_characters_go(self):
        out = rules.redact_log(
            "\x1b[31mERROR\x1b[0m in /Users/jdoe/dev/Pulse\x07\x00", "/Users/jdoe"
        )
        self.assertEqual(out, "ERROR in ~/dev/Pulse")

    def test_it_is_capped_from_the_end(self):
        out = rules.redact_log("\n".join(f"line {i} " + "y " * 200 for i in range(200)))
        self.assertLessEqual(len(out), rules.EVIDENCE_MAX)
        self.assertIn("line 199", out)
        self.assertTrue(all(len(l) <= rules.LOG_LINE_MAX for l in out.splitlines()))


class WatchActs(unittest.TestCase):
    def test_which_findings_the_team_also_works_on(self):
        for key in (
            "stop:12",
            "manage",
            "auto",
            "silent",
            "briefing:morning:2026-11-16",
        ):
            self.assertTrue(rules.incident_worthy({"key": key, "serious": True}), key)
        for key, serious in (
            ("stop:dry", True),
            ("manage", False),
            ("clock", True),
            ("fetch:stuck", True),
            ("scheduler:stalled", True),
            ("automatic", True),
            ("crash:pulse-2026", True),
        ):
            self.assertFalse(
                rules.incident_worthy({"key": key, "serious": serious}), key
            )

    def test_the_watch_only_texts_for_its_first_week(self):
        memory = {}
        self.assertFalse(rules.watch_acts(memory, NOW, off=False))
        self.assertEqual(memory["since"], "2026-11-16T14:30:00")
        self.assertFalse(
            rules.watch_acts(memory, NOW + dt.timedelta(days=6, hours=23), off=False)
        )
        self.assertTrue(rules.watch_acts(memory, NOW + dt.timedelta(days=7), off=False))

    def test_switched_off_it_never_acts(self):
        memory = {"since": "2026-01-01T00:00:00"}
        self.assertFalse(rules.watch_acts(memory, NOW, off=True))
        self.assertTrue(rules.watch_acts(memory, NOW, off=False))

    def test_a_first_day_that_cannot_be_read_starts_the_week_again(self):
        memory = {"since": "long ago"}
        self.assertFalse(rules.watch_acts(memory, NOW, off=False))
        self.assertEqual(memory["since"], "2026-11-16T14:30:00")

    def test_a_trigger_says_what_was_seen_and_when(self):
        finding = {"key": "manage", "serious": True, "text": "It failed twice."}
        self.assertEqual(
            rules.trigger(finding, "x" * 7000, NOW),
            {
                "key": "manage",
                "text": "It failed twice.",
                "evidence": "x" * rules.EVIDENCE_MAX,
                "at": "2026-11-16T14:30:00",
            },
        )


class TheWatchQueues(unittest.TestCase):
    """A serious alarm is also left for the team, once the watch may act."""

    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        root = Path(self.tmp.name).resolve()
        self.home, self.logs = root / "team", root / "logs"
        for d in (self.home, self.logs, root / "app/schedule", root / "crashes"):
            d.mkdir(parents=True)
        for name, value in {
            "HOME": self.home,
            "APP": root / "app",
            "LOGS": self.logs,
            "CRASHES": root / "crashes",
            "MEMORY": self.home / "watch.json",
            "QUEUE": self.home / "queue",
            "INCIDENTS_OFF": self.home / "incidents-off",
            "FETCHER": root / "pulse-fetcher",
        }.items():
            self.addCleanup(setattr, watch, name, getattr(watch, name))
            setattr(watch, name, value)
        self.addCleanup(chat.set_home, chat.HOME)
        chat.set_home(self.home)
        self.sent, self.ran = [], []
        for name, fake in {
            "send": lambda text, serious=False, now=None: (
                self.sent.append((text, serious)) or "sent"
            ),
            "banner": lambda text: None,
        }.items():
            self.addCleanup(setattr, chat, name, getattr(chat, name))
            setattr(chat, name, fake)
        for name, fake in {
            "log": lambda msg: None,
            "gather": lambda now, memory, run=None: {},
        }.items():
            self.addCleanup(setattr, watch, name, getattr(watch, name))
            setattr(watch, name, fake)
        self.found = [
            {
                "key": "manage",
                "serious": True,
                "text": "Position management failed twice.",
            }
        ]
        self.addCleanup(setattr, rules, "watch_findings", rules.watch_findings)
        rules.watch_findings = lambda facts, memory, now: [dict(f) for f in self.found]
        (self.logs / "fetch-stderr.log").write_text(
            "calling https://api.example.com/v2?apikey=SECRET123\n"
            "thread 'main' panicked at crates/pulse-fetcher/src/position_management.rs:412:17\n"
        )

    def run_cmd(self, cmd, **kw):
        self.ran.append(cmd)
        import subprocess

        return subprocess.CompletedProcess(cmd, 0)

    def look(self, now=NOW):
        return watch.look(now, run=self.run_cmd)

    def queued(self):
        folder = self.home / "queue"
        return (
            [json.loads(p.read_text()) for p in sorted(folder.glob("*.json"))]
            if folder.is_dir()
            else []
        )

    def old_enough(self):
        (self.home / "watch.json").write_text(
            json.dumps({"since": "2026-11-01T00:00:00"})
        )

    def test_in_its_first_week_it_only_texts(self):
        self.look()
        self.assertEqual(len(self.sent), 1)
        self.assertEqual(self.queued(), [])
        self.assertEqual(self.ran, [])
        self.assertNotIn("looking for the cause", self.sent[0][0])

    def test_after_that_a_serious_alarm_is_queued_and_the_runner_is_woken(self):
        self.old_enough()
        self.look()
        (item,) = self.queued()
        self.assertEqual(item["key"], "manage")
        self.assertEqual(item["text"], "Position management failed twice.")
        self.assertEqual(item["at"], "2026-11-16T14:30:00")
        self.assertIn("position_management.rs:412", item["evidence"])
        self.assertNotIn("SECRET123", item["evidence"])
        self.assertEqual(
            self.ran, [["launchctl", "kickstart", f"gui/{os.getuid()}/com.pulse.team"]]
        )
        text, serious = self.sent[0]
        self.assertTrue(serious)
        self.assertTrue(
            text.startswith(
                "Serious problem in Pulse. Position management failed twice."
            )
        )
        self.assertTrue(text.endswith("will text what it finds."))
        # what the rules make of it is a job
        jobs, dropped = rules.incident_jobs(
            [dict(item, file="x.json")], {}, NOW + dt.timedelta(minutes=1)
        )
        self.assertEqual((len(jobs), dropped), (1, []))

    def test_the_same_alarm_is_not_queued_again_while_it_is_not_texted_again(self):
        self.old_enough()
        self.look()
        self.look(NOW + dt.timedelta(minutes=10))
        self.look(NOW + dt.timedelta(minutes=20))
        self.assertEqual(len(self.queued()), 1)
        self.assertEqual(len(self.sent), 1)

    def test_what_is_not_serious_or_not_in_the_code_is_only_texted(self):
        self.old_enough()
        self.found = [
            {"key": "fetch:stuck", "serious": False, "text": "A fetch hangs."},
            {"key": "stop:dry", "serious": False, "text": "Dry run."},
        ]
        self.look()
        self.assertEqual(len(self.sent), 2)
        self.assertEqual(self.queued(), [])
        self.assertEqual(self.ran, [])

    def test_switched_off_it_only_texts(self):
        self.old_enough()
        (self.home / "incidents-off").write_text("off\n")
        self.look()
        self.assertEqual(len(self.sent), 1)
        self.assertEqual(self.queued(), [])

    def test_a_look_that_tells_nothing_queues_nothing(self):
        self.old_enough()
        watch.look(NOW, tell=False, run=self.run_cmd)
        self.assertEqual((self.sent, self.queued(), self.ran), ([], [], []))

    def test_a_queue_that_cannot_be_written_does_not_stop_the_text(self):
        self.old_enough()
        (self.home / "queue").write_text("a file where the folder should be")
        self.look()
        self.assertEqual(len(self.sent), 1)
        self.assertNotIn("looking for the cause", self.sent[0][0])
        self.assertEqual(self.ran, [])

    def test_the_evidence_is_the_cleaned_end_of_both_logs(self):
        (self.logs / "fetch-stdout.log").write_text(
            "x" * 50_000 + "\nmanage-positions failed (exit 101)\n"
        )
        text = watch.evidence()
        self.assertIn("== the end of fetch-stdout.log ==", text)
        self.assertIn("manage-positions failed (exit 101)", text)
        self.assertIn("== the end of fetch-stderr.log ==", text)
        self.assertLessEqual(len(text), rules.EVIDENCE_MAX + 200)
        (self.logs / "fetch-stdout.log").unlink()
        (self.logs / "fetch-stderr.log").unlink()
        self.assertEqual(watch.evidence(), "")


class Words(unittest.TestCase):
    """What Nicola reads about a turn by day or for an incident."""

    def report(self, shift, *jobs, **more):
        return {
            "started": "2026-11-16T14:30",
            "shift": shift,
            "minutes": 12,
            "jobs": list(jobs),
            **more,
        }

    def incident(self, result, **more):
        return {
            "kind": "incident",
            "key": "incident-manage-11161430",
            "title": "Position management failed twice in a row.",
            "result": result,
            "reasons": [],
            "pr": None,
            **more,
        }

    def test_the_night_reads_as_before(self):
        night = {"started": "2026-11-16T02:30", "minutes": 40, "jobs": []}
        self.assertTrue(
            page.as_text(night).startswith(
                "Pulse night team — Mon 16 Nov, 02:30 (40 min)"
            )
        )
        self.assertTrue(
            page.chat_digest(night).startswith("Pulse team — Mon 16 Nov, 02:30")
        )

    def test_a_turn_says_what_kind_it_was(self):
        self.assertTrue(
            page.as_text(self.report("day")).startswith("Pulse team, by day — ")
        )
        self.assertTrue(
            page.chat_digest(self.report("incident")).startswith(
                "Pulse team, on a serious problem — "
            )
        )

    def test_a_fixed_incident_says_the_app_still_needs_a_release(self):
        text = page.chat_digest(self.report("incident", self.incident("merged", pr=81)))
        self.assertIn("Problem: Position management failed twice in a row.", text)
        self.assertIn("Cause found and fixed (PR #81).", text)
        self.assertIn("with the next release", text)
        self.assertNotIn("Merged: 1", text)
        self.assertNotIn("Nothing to work on", text)

    def test_a_cause_outside_the_code_is_told_in_the_builders_words(self):
        job = self.incident(
            "blocked", reasons=["The broker answered 503 from 10:02 to 12:10."]
        )
        text = page.chat_digest(self.report("incident", job))
        self.assertIn(
            "Not fixed by the team: The broker answered 503 from 10:02 to 12:10.", text
        )
        self.assertNotIn("I ask about each one below", text)

    def test_a_fix_that_touches_trading_waits_for_nicola(self):
        job = self.incident(
            "needs_user",
            pr=82,
            reasons=[
                "changes how Pulse trades: crates/pulse-fetcher/src/position_management.rs"
            ],
        )
        text = page.chat_digest(self.report("incident", job))
        self.assertIn("A fix waits for you (PR #82): changes how Pulse trades", text)

    def test_the_other_endings(self):
        for result, words in (
            ("ready", "A fix is ready to merge (PR #83)."),
            ("yielded", "stopped midway and goes on at the next turn"),
            ("error", "The look into it failed: timed out after 75 min"),
        ):
            job = self.incident(
                result,
                pr=83 if result == "ready" else None,
                reasons=["timed out after 75 min"],
            )
            self.assertIn(words, page.chat_digest(self.report("incident", job)))

    def test_a_job_that_stepped_aside_is_counted_and_labelled(self):
        job = {
            "kind": "idea",
            "title": "fix: a typo",
            "result": "yielded",
            "reasons": ["it stepped aside: x"],
        }
        self.assertIn(
            "Stepped aside midway, goes on later: 1",
            page.chat_digest(self.report("day", job)),
        )
        self.assertIn(
            "[Stepped aside, goes on later] fix: a typo",
            page.as_text(self.report("day", job)),
        )

    def test_the_page_names_the_turn_and_lists_it_among_the_runs(self):
        day = self.report(
            "day",
            {"kind": "idea", "title": "fix: <b>", "result": "merged", "pr": 9},
            repo="n/P",
        )
        night = {
            "started": "2026-11-16T02:30",
            "minutes": 40,
            "jobs": [],
            "repo": "n/P",
        }
        html = page.render([day, night])
        self.assertIn("<h2>Today, by day</h2>", html)
        self.assertIn("fix: &lt;b&gt;", html)
        self.assertIn("<td>Mon 16 Nov, 14:30, by day</td>", html)
        self.assertIn("<td>Mon 16 Nov, 02:30</td>", html)
        self.assertNotIn(
            "main is failing", html
        )  # a turn that did not check main says nothing of it
        self.assertIn("<h2>Last night</h2>", page.render([night, day]))
        self.assertIn(
            "<h2>A serious problem</h2>", page.render([self.report("incident")])
        )


def team_at(case: unittest.TestCase, home: Path, sessions: Path):
    """team.py loaded against a temporary home folder, with nobody working on
    this Mac (the session logs it looks at are `sessions`)."""
    old_home = os.environ.get("PULSE_TEAM_HOME")
    os.environ["PULSE_TEAM_HOME"] = str(home)
    team = importlib.reload(importlib.import_module("team"))

    def restore():
        if old_home is None:
            os.environ.pop("PULSE_TEAM_HOME", None)
        else:
            os.environ["PULSE_TEAM_HOME"] = old_home
        importlib.reload(team)

    case.addCleanup(restore)
    team.SESSION_LOGS = sessions
    team.log = lambda msg: None
    return team


def nicola_works(sessions: Path) -> None:
    """A session of his own wrote to its log just now."""
    folder = sessions / (rules.project_folder(str(Path.home())) + "-dev-Pulse")
    folder.mkdir(parents=True, exist_ok=True)
    (folder / "one.jsonl").write_text("{}\n")


def queue_problem(team, name="a.json", key="manage") -> Path:
    team.QUEUE.mkdir(parents=True, exist_ok=True)
    finding = {"key": key, "text": "Position management failed twice."}
    path = team.QUEUE / name
    path.write_text(json.dumps(rules.trigger(finding, "the log", dt.datetime.now())))
    return path


class AnHourlyTurn(unittest.TestCase):
    """What the runner does with an hourly wake-up, against a temporary home.
    The work itself and the report are played by the test."""

    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        root = Path(self.tmp.name).resolve()
        self.home, self.sessions = root / "team", root / "sessions"
        self.home.mkdir()
        self.team = team_at(self, self.home, self.sessions)
        self.team.minutes_since = lambda started: 7
        self.turns, self.reported, self.jobs = [], [], []

        def work(args, run, state, report, started):
            self.turns.append(run["shift"])
            report["jobs"] += self.jobs

        self.team.work = work
        self.team.finish = lambda report, run, state, started: (
            self.reported.append(report["shift"]) or 0
        )
        self.addCleanup(setattr, rules, "night_due", rules.night_due)
        rules.night_due = lambda now, start, late, last: ""
        self.args = type("Args", (), {"scheduled": True, "no_merge": True, "only": ""})()

    def turn(self, hour=11):
        started = dt.datetime.combine(dt.date.today(), dt.time(hour, 30))
        return self.team.locked_run(self.args, started, True)

    def a_small_fix_waits(self):
        self.team.IDEAS.write_text("# Pulse ideas\n\n- [ ] fix: the clock shows 24:00\n")

    def state(self):
        return json.loads((self.home / "state.json").read_text())

    def test_by_day_a_small_fix_gets_a_turn(self):
        self.a_small_fix_waits()
        self.assertEqual(self.turn(), 0)
        self.assertEqual(self.turns, ["day"])

    def test_a_feature_on_the_list_is_no_reason_for_a_turn_by_day(self):
        self.team.IDEAS.write_text("# Pulse ideas\n\n- [ ] dark mode\n")
        self.turn()
        self.assertEqual(self.turns, [])
        self.assertFalse((self.home / "runs").exists())  # the hour left no trace

    def test_something_sent_through_telegram_is_brought_in_by_day(self):
        chat.INBOX.mkdir(parents=True)
        (chat.INBOX / "1.json").write_text(json.dumps({"kind": "idea", "text": "x"}))
        self.turn()
        self.assertEqual(self.turns, ["day"])

    def test_not_while_nicola_is_working_on_this_mac(self):
        self.a_small_fix_waits()
        nicola_works(self.sessions)
        self.turn()
        self.assertEqual(self.turns, [])

    def test_not_outside_the_days_hours(self):
        self.a_small_fix_waits()
        for hour in (8, 21, 23, 3):
            self.turn(hour)
        self.assertEqual(self.turns, [])

    def test_a_serious_problem_gets_a_turn_at_any_hour_whoever_is_working(self):
        queue_problem(self.team)
        nicola_works(self.sessions)
        self.turn(3)
        self.assertEqual(self.turns, ["incident"])

    def test_paused_no_hour_is_for_anything(self):
        self.a_small_fix_waits()
        queue_problem(self.team)
        chat.PAUSED.write_text("now\n")
        rules.night_due = lambda now, start, late, last: "2026-11-16"
        self.turn(3)
        self.turn(11)
        self.assertEqual(self.turns, [])
        self.assertFalse((self.home / "state.json").exists())  # the night is not spent

    def test_the_night_is_still_the_night(self):
        rules.night_due = lambda now, start, late, last: "2026-11-16"
        queue_problem(self.team)  # it takes the incident with it
        self.turn(3)
        self.assertEqual((self.turns, self.reported), (["night"], ["night"]))
        self.assertEqual(self.state()["last_night"], "2026-11-16")

    def test_a_run_by_hand_does_the_nights_work_at_any_hour(self):
        self.args.scheduled = False
        started = dt.datetime.combine(dt.date.today(), dt.time(15, 0))
        self.team.locked_run(self.args, started, False)
        self.assertEqual(self.turns, ["night"])

    def test_the_days_minutes_are_counted_and_end_the_day(self):
        self.a_small_fix_waits()
        self.team.DAY_MIN = 10
        self.turn(10)
        self.assertEqual(rules.day_used(self.state(), dt.date.today()), 7)
        self.turn(11)
        self.assertEqual(rules.day_used(self.state(), dt.date.today()), 14)
        self.turn(12)  # used up
        self.assertEqual(self.turns, ["day", "day"])

    def test_a_turn_without_a_job_leaves_no_report(self):
        self.a_small_fix_waits()
        self.turn()
        self.assertEqual(self.reported, [])
        self.assertEqual(list((self.home / "runs").iterdir()), [])

    def test_a_turn_with_a_job_is_reported(self):
        self.a_small_fix_waits()
        self.jobs = [{"kind": "idea", "result": "merged"}]
        self.turn()
        self.assertEqual(self.reported, ["day"])


class TheStopButton(unittest.TestCase):
    """What a running session, the checks and the wait for CI ask every few
    seconds (team.stepper)."""

    IDEA = {"kind": "idea", "key": "idea-x", "title": "x"}
    INCIDENT = {"kind": "incident", "key": "incident-x", "title": "x"}

    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        root = Path(self.tmp.name).resolve()
        self.home, self.sessions = root / "team", root / "sessions"
        self.home.mkdir()
        self.team = team_at(self, self.home, self.sessions)
        self.run = {"shift": "night", "job": self.IDEA}
        self.state = {}
        self.ask = self.team.stepper(self.run, self.state, dt.datetime.now())

    def test_a_job_goes_on_while_nothing_asks_it_to_stop(self):
        self.assertEqual(self.ask(), "")
        self.assertFalse(self.run.get("yielded"))

    def test_between_jobs_there_is_nothing_to_stop(self):
        self.run["job"] = None
        chat.PAUSED.write_text("now\n")
        self.assertEqual(self.ask(), "")

    def test_pause_stops_the_job_that_is_running(self):
        chat.PAUSED.write_text("now\n")
        self.assertEqual(self.ask(), "paused from Telegram")
        self.assertEqual(self.run["yielded"], "paused from Telegram")
        self.run["job"] = self.INCIDENT
        self.assertEqual(self.ask(), "paused from Telegram")  # also an incident's

    def test_a_serious_problem_that_comes_in_stops_any_other_job(self):
        queue_problem(self.team)
        self.assertEqual(self.ask(), "a serious problem came first")
        self.run["job"] = self.INCIDENT
        self.assertEqual(self.ask(), "")  # the work on a problem is not stopped by one

    def test_a_run_for_one_named_job_does_not_step_aside_for_the_queue(self):
        queue_problem(self.team)
        self.run["only"] = True
        self.assertEqual(self.ask(), "")

    def test_by_day_it_steps_aside_when_nicola_starts_working(self):
        self.run["shift"] = "day"
        self.assertEqual(self.ask(), "")
        nicola_works(self.sessions)
        self.assertEqual(self.ask(), "")  # the logs are read once a minute
        self.team.ACTIVE_EVERY_S = 0
        self.assertEqual(self.ask(), "Nicola started using Claude on this Mac")

    def test_by_night_nicola_working_stops_nothing(self):
        nicola_works(self.sessions)
        self.team.ACTIVE_EVERY_S = 0
        self.assertEqual(self.ask(), "")

    def test_by_day_the_budget_ends_the_job(self):
        self.run["shift"] = "day"
        self.team.DAY_MIN = 0
        self.assertEqual(self.ask(), "the day's time for the team is used up")

    def test_the_teams_own_sessions_are_not_nicola(self):
        own = rules.project_folder(os.path.realpath(self.team.REPO / self.team.WORKTREE))
        (self.sessions / own).mkdir(parents=True)
        (self.sessions / own / "build.jsonl").write_text("{}\n")
        self.assertFalse(self.team.owner_active())
        nicola_works(self.sessions)
        self.assertTrue(self.team.owner_active())


class TheQueue(unittest.TestCase):
    """The folder of serious problems, as the runner reads and empties it."""

    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        root = Path(self.tmp.name).resolve()
        self.home = root / "team"
        self.home.mkdir()
        self.team = team_at(self, self.home, root / "sessions")

    def test_a_trigger_is_a_job_until_it_is_dropped(self):
        path = queue_problem(self.team)
        (job,) = self.team.incidents({})
        self.assertEqual((job["kind"], job["problem"], job["file"]), ("incident", "manage", "a.json"))
        self.team.drop_trigger(job["file"])
        self.assertFalse(path.exists())
        self.assertEqual(self.team.incidents({}), [])

    def test_what_will_not_be_a_job_is_thrown_away_with_the_reason(self):
        self.team.QUEUE.mkdir()
        bad = self.team.QUEUE / "bad.json"
        bad.write_text("not json")
        self.assertEqual(self.team.incidents({}), [])
        self.assertTrue(bad.exists())  # only a run that reports throws away
        report = {"skipped": []}
        self.assertEqual(self.team.incidents({}, report), [])
        self.assertFalse(bad.exists())
        self.assertEqual(report["skipped"][0]["why"], "it could not be read")

    def test_only_a_file_of_the_queue_is_ever_removed(self):
        (self.home / "state.json").write_text("{}")
        self.team.QUEUE.mkdir()
        self.team.drop_trigger("../state.json")
        self.team.drop_trigger("")
        self.assertTrue((self.home / "state.json").exists())

    def test_a_problem_can_be_queued_by_hand(self):
        args = type("Args", (), {"text": ["Reddit", "answers", "403"], "logs": False})()
        with contextlib.redirect_stdout(io.StringIO()):
            self.assertEqual(self.team.cmd_incident(args), 0)
        (job,) = self.team.incidents({})
        self.assertEqual(job["title"], "Reddit answers 403")
        self.assertTrue(job["problem"].startswith("hand:"))

    def test_the_switch_for_the_watch(self):
        said = io.StringIO()
        with contextlib.redirect_stdout(said):
            for action in ("off", "status", "on"):
                self.team.cmd_incidents(type("Args", (), {"action": action})())
                if action != "on":
                    self.assertTrue(self.team.INCIDENTS_OFF.exists())
        self.assertFalse(self.team.INCIDENTS_OFF.exists())
        lines = said.getvalue().splitlines()
        self.assertEqual(lines[:2], ["Off: the watch only texts."] * 2)
        self.assertTrue(lines[2].startswith("On"))


class TheRunLoop(unittest.TestCase):
    """team.work from the list of jobs to the report, against a temporary
    home. GitHub and the jobs themselves are played by the test."""

    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        root = Path(self.tmp.name).resolve()
        self.home, self.sessions = root / "team", root / "sessions"
        self.home.mkdir()
        team = self.team = team_at(self, self.home, self.sessions)
        team.fence_problem = lambda: ""
        team.git = lambda *a, **k: (0, "sha")
        team.gh_json = lambda *a: (
            {"owner": {"login": OWNER}, "nameWithOwner": f"{OWNER}/Pulse"}
            if a[0] == "repo"
            else []
        )
        team.copy_database = lambda: ""
        team.weekly_market = lambda run, state, report: None
        self.ran, self.play = [], []
        team.run_job = self.run_job
        self.state = {"main_ok_sha": "sha"}
        self.report = {"jobs": [], "skipped": [], "notes": [], "repo": ""}
        self.run = {
            "dir": root,
            "cost": 0.0,
            "abort": "",
            "scheduled": True,
            "shift": "night",
            "state": self.state,
            "main_step": "",
            "main_tail": "",
        }
        self.args = type("Args", (), {"only": "", "max_jobs": 3, "no_merge": True})()
        team.IDEAS.write_text("# Pulse ideas\n\n- [ ] fix: the clock\n- [ ] dark mode\n")

    def run_job(self, job, run):
        """Each job ends as the next entry of `play` says: a result, or a
        function of the job that gives one. Without an entry it is merged."""
        self.ran.append(job["key"].split("-")[0])
        step = self.play.pop(0) if self.play else "merged"
        res = step(job) if callable(step) else step
        if isinstance(res, str):
            res = {"result": res}
        return {
            "kind": job["kind"],
            "key": job["key"],
            "title": job["title"],
            "pr": 7 if res["result"] == "merged" else None,
            "reasons": [],
            "minutes": 3,
            **res,
        }

    def work(self):
        self.team.work(self.args, self.run, self.state, self.report, dt.datetime.now())
        return [(j["kind"], j["result"]) for j in self.report["jobs"]]

    def test_the_night_does_its_jobs_in_order(self):
        self.assertEqual(self.work(), [("idea", "merged"), ("idea", "merged")])
        self.assertEqual(self.report["notes"], [])

    def test_pause_ends_the_run_and_the_note_counts_what_ended(self):
        def paused(job):
            chat.PAUSED.write_text("now\n")
            return "yielded"

        self.play = [paused]
        self.assertEqual(self.work(), [("idea", "yielded")])
        self.assertEqual(
            self.report["notes"],
            ["paused from Telegram after 0 of 2 jobs; the rest wait"],
        )
        self.assertEqual(self.ran, ["idea"])  # nothing was started after the pause
        self.assertIn("- [ ] fix: the clock", self.team.IDEAS.read_text())

    def test_a_job_that_stepped_aside_for_a_serious_problem_goes_on_in_the_same_run(self):
        def a_problem_comes_in(job):
            self.trigger = queue_problem(self.team)
            return "yielded"

        self.play = [
            a_problem_comes_in,
            {"result": "blocked", "reasons": ["the broker was down"], "minutes": 4},
            {"result": "merged", "minutes": 5},
        ]
        self.assertEqual(
            self.work(),
            [("incident", "blocked"), ("idea", "merged"), ("idea", "merged")],
        )
        self.assertEqual(self.ran, ["idea", "incident", "idea", "idea"])
        # one line for the job that went on, with all its time
        self.assertEqual([j["minutes"] for j in self.report["jobs"]], [4, 8, 3])
        self.assertFalse(self.trigger.exists())  # the problem was looked into: it is gone
        self.assertEqual(rules.incidents_today(self.state, dt.date.today()), ["manage"])
        self.assertEqual(self.state["held"], {})

    def test_a_job_that_keeps_stepping_aside_is_left_for_the_next_turn(self):
        self.play = ["yielded"] * 3
        self.assertEqual(self.work(), [("idea", "yielded"), ("idea", "merged")])
        self.assertEqual(self.ran, ["idea"] * 4)  # three starts for the first, no more
        self.assertEqual(self.report["jobs"][0]["minutes"], 9)
        key = self.report["jobs"][0]["key"]
        self.assertIsNotNone(rules.waiting(self.state, key, dt.datetime.now()))

    def test_by_day_only_the_small_fix_and_not_while_nicola_works(self):
        self.run["shift"] = "day"
        self.assertEqual(self.work(), [("idea", "merged")])
        self.assertIn("- [ ] dark mode", self.team.IDEAS.read_text())
        self.team.IDEAS.write_text("# Pulse ideas\n\n- [ ] fix: the date\n")
        self.report["jobs"], self.ran = [], []
        nicola_works(self.sessions)
        self.assertEqual(self.work(), [])
        self.assertEqual(self.ran, [])
        self.assertEqual(
            self.report["notes"][-1],
            "stopped after 0 of 1 jobs (Nicola started using Claude on this Mac); "
            "the rest wait",
        )

    def test_by_day_nothing_is_built_on_a_main_nobody_checked(self):
        self.run["shift"] = "day"
        self.state["main_ok_sha"] = "an older one"
        self.assertEqual(self.work(), [])
        self.assertNotIn("main", self.report)  # and the day says nothing about main

    def test_a_turn_for_a_serious_problem_does_that_alone(self):
        self.run["shift"] = "incident"
        queue_problem(self.team)
        self.assertEqual(self.work(), [("incident", "merged")])

    def test_a_serious_problem_is_not_held_back_by_the_nights_time_or_job_limit(self):
        queue_problem(self.team)
        self.team.BUDGET_MIN = -1
        self.args.max_jobs = 1
        self.assertEqual(self.work(), [("incident", "merged")])
        self.assertEqual(
            self.report["notes"],
            [
                "1 more wait for the next run: dark mode",
                "ran out of time after 1 of 2 jobs; the rest wait for tomorrow",
            ],
        )

    def test_a_run_for_one_named_job_takes_nothing_from_the_queue(self):
        trigger = queue_problem(self.team)
        self.args.only = "idea"
        self.assertEqual(self.work(), [("idea", "merged"), ("idea", "merged")])
        self.assertTrue(trigger.exists())


if __name__ == "__main__":
    unittest.main()
