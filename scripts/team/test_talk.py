"""Plain words from Telegram: what a talk session may ask for, and what the
runner makes of it."""

from __future__ import annotations

import importlib
import json
import os
import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import chat  # noqa: E402
import rules  # noqa: E402
from test_chat import ChatCase  # noqa: E402

OK = [{"name": "ci", "status": "COMPLETED", "conclusion": "SUCCESS"}]


def pr(number=7, **over):
    base = {
        "number": number,
        "title": f"Fix number {number}",
        "author": {"login": "owner"},
        "isCrossRepository": False,
        "isDraft": False,
        "baseRefName": "main",
        "headRefOid": f"sha{number}",
        "mergeable": "MERGEABLE",
        "statusCheckRollup": OK,
        "files": [{"path": "src/routes/+page.svelte"}],
    }
    return {**base, **over}


def card(number=7, **over):
    return rules.pr_card(pr(number, **over), "owner", "main")


class Plan(unittest.TestCase):
    def test_a_green_pull_request_of_the_owner_may_be_merged(self):
        self.assertEqual(rules.merge_refusal(card()), "")

    def test_what_is_not_merged_from_a_text(self):
        for over, word in [
            ({"author": {"login": "someone"}}, "someone else"),
            ({"isCrossRepository": True}, "someone else"),
            ({"isDraft": True}, "draft"),
            ({"baseRefName": "feat/x"}, "does not target main"),
            ({"statusCheckRollup": []}, "checks"),
            ({"statusCheckRollup": [{"name": "ci", "status": "IN_PROGRESS"}]}, "checks"),
            ({"statusCheckRollup": [{"name": "ci", "status": "COMPLETED", "conclusion": "FAILURE"}]}, "checks"),
            ({"mergeable": "CONFLICTING"}, "cannot merge"),
            ({"mergeable": None}, "cannot merge"),
            ({"files": [{"path": "crates/pulse-alpaca/src/stops.rs"}]}, "trades"),
        ]:
            self.assertIn(word, rules.merge_refusal(card(**over)), over)
        self.assertIn("not an open", rules.merge_refusal(None))

    def test_a_change_to_the_teams_own_code_is_marked_not_refused(self):
        c = card(files=[{"path": "scripts/team/team.py"}])
        self.assertEqual(rules.merge_refusal(c), "")
        self.assertIn("team's own code", rules.merge_plan_text([c]))
        self.assertNotIn("team's own code", rules.merge_plan_text([card()]))

    def test_the_plan_sorts_what_was_asked(self):
        cards = [card(7), card(8, isDraft=True)]
        plan = rules.talk_plan(
            [
                {"kind": "merge", "pr": 7},
                {"kind": "merge", "pr": 7},  # twice
                {"kind": "merge", "pr": 8},
                {"kind": "merge", "pr": 99},
                {"kind": "merge", "pr": "7"},  # not a number
                {"kind": "merge", "pr": True},
                {"kind": "task", "text": "  build  a chart "},
                {"kind": "task", "text": "Build a chart"},  # the same again
                {"kind": "task", "text": ""},
                {"kind": "idea", "text": "x" * 900},
                {"kind": "pause"},
                {"kind": "pause"},
                {"kind": "release"},  # not on the list
                {"kind": "rm -rf"},
                "merge 7",
                None,
            ],
            cards,
            paused=False,
        )
        self.assertEqual([c["number"] for c in plan["merges"]], [7])
        self.assertEqual(len(plan["refused"]), 2)
        self.assertIn("#8", plan["refused"][0])
        self.assertIn("#99", plan["refused"][1])
        self.assertEqual(
            plan["now"],
            [
                {"kind": "task", "text": "build a chart"},
                {"kind": "idea", "text": "x" * rules.IDEA_MAX},
                {"kind": "pause"},
            ],
        )

    def test_paused_nothing_merges_and_no_job_starts_but_resume_works(self):
        plan = rules.talk_plan(
            [{"kind": "merge", "pr": 7}, {"kind": "task", "text": "a"}, {"kind": "resume"}, {"kind": "idea", "text": "b"}],
            [card()],
            paused=True,
        )
        self.assertEqual(plan["merges"], [])
        self.assertEqual(len(plan["refused"]), 2)
        self.assertEqual(plan["now"], [{"kind": "resume"}, {"kind": "idea", "text": "b"}])

    def test_no_more_than_the_limit_and_nothing_from_nonsense(self):
        many = [{"kind": "idea", "text": f"idea {i}"} for i in range(40)]
        self.assertEqual(len(rules.talk_plan(many, [], False)["now"]), rules.TALK_MAX_ACTIONS)
        for junk in (None, "merge all", {"kind": "merge"}, 5):
            plan = rules.talk_plan(junk, [card()], False)
            self.assertEqual((plan["merges"], plan["now"], plan["refused"]), ([], [], []))


class Talker(ChatCase):
    """The runner's side, with the session and GitHub replaced."""

    def setUp(self):
        super().setUp()
        saved = os.environ.get("PULSE_TEAM_HOME")
        os.environ["PULSE_TEAM_HOME"] = str(self.home)
        self.team = importlib.reload(importlib.import_module("team"))
        if saved is None:
            os.environ.pop("PULSE_TEAM_HOME", None)
        else:
            os.environ["PULSE_TEAM_HOME"] = saved
        team = self.team
        team.log = lambda msg: None
        self.cards = [card(7), card(8)]
        team.talk_cards = lambda: (list(self.cards), "")
        self.answer = {"reply": "Sure.", "actions": []}
        self.prompts = []

        def fake_session(text, context, history, run):
            self.prompts.append((text, context, history))
            return {"ok": True, "data": self.answer}

        team.talk_session = fake_session
        self.git, self.gh, self.started = [], [], []
        team.git = lambda *a, **k: (self.git.append(a), (0, ""))[1]
        self.gh_code = 0
        team.gh = lambda *a, **k: (self.gh.append(a), (self.gh_code, "no"))[1]
        team.start_task = lambda text: (self.started.append(text), "")[1]

    def ask_text(self, words):
        chat.ASKS.mkdir(parents=True, exist_ok=True)
        (chat.ASKS / f"{len(list(chat.ASKS.glob('*')))+1:012d}.json").write_text(
            json.dumps({"text": words})
        )

    def test_a_question_is_answered_and_nothing_else_happens(self):
        self.ask_text("what is open?")
        self.team.cmd_talk(None)
        self.assertEqual(self.fake.said(), ["Sure."])
        self.assertEqual(list(chat.ASKS.glob("*.json")), [])
        self.assertEqual(self.gh, [])
        self.assertIn("#7 Fix number 7", self.prompts[0][1])
        self.assertIn("can be merged", self.prompts[0][1])

    def test_a_merge_is_only_a_plan_until_he_taps(self):
        self.answer = {"reply": "I will merge them.", "actions": [{"kind": "merge", "pr": 7}, {"kind": "merge", "pr": 8}, {"kind": "merge", "pr": 9}]}
        self.ask_text("merge all the prs")
        self.team.cmd_talk(None)
        self.assertEqual(self.gh, [])  # nothing merged
        said = self.fake.said()
        self.assertIn("Not done — #9", said[0])
        self.assertIn("#7 Fix number 7", said[1])
        p = self.asked()[0]
        self.assertEqual(p["state"], "sent")
        self.assertEqual(p["acts"], [{"kind": "merge", "pr": 7, "sha": "sha7"}, {"kind": "merge", "pr": 8, "sha": "sha8"}])
        # He taps, and one of the two changed meanwhile.
        self.cards = [card(7), card(8, headRefOid="other")]
        with chat.proposals() as items:
            items[0]["state"] = "approved"
        self.team.cmd_talk(None)
        self.assertEqual(self.gh, [("pr", "merge", "7", "--merge", "--match-head-commit", "sha7")])
        last = self.fake.said()[-1]
        self.assertIn("#7 merged", last)
        self.assertIn("#8: not merged, it changed after", last)
        self.assertEqual(self.asked()[0]["state"], "consumed")
        self.team.cmd_talk(None)  # and never a second time
        self.assertEqual(len(self.gh), 1)

    def test_an_approved_plan_is_checked_again_before_each_merge(self):
        acts = [{"kind": "merge", "pr": 7, "sha": "sha7"}, {"kind": "merge", "pr": 8, "sha": "sha8"}, {"kind": "merge", "pr": 5, "sha": "x"}, {"kind": "merge", "pr": "7", "sha": "sha7"}, {"kind": "merge", "pr": 7}]
        self.cards = [card(7, statusCheckRollup=[]), card(8, files=[{"path": "crates/pulse-alpaca/src/a.rs"}])]
        lines = self.team.merge_approved(acts)
        self.assertEqual(self.gh, [])
        self.assertEqual(len(lines), 3)
        chat.PAUSED.write_text("x")
        self.cards = [card(7)]
        self.assertIn("paused", self.team.merge_approved(acts[:1])[0])
        self.assertEqual(self.gh, [])

    def test_a_merge_github_refuses_is_said_so(self):
        self.gh_code = 1
        self.assertIn("GitHub refused", self.team.merge_approved([{"kind": "merge", "pr": 7, "sha": "sha7"}])[0])

    def test_a_job_starts_now_an_idea_waits_and_pause_pauses(self):
        self.answer = {"reply": "On it.", "actions": [{"kind": "task", "text": "Add a chart"}, {"kind": "idea", "text": "Dark mode"}, {"kind": "pause"}]}
        self.ask_text("build me a chart, and think about dark mode, and stop")
        self.team.cmd_talk(None)
        self.assertEqual(self.started, ["Add a chart"])
        ideas = [json.loads(f.read_text())["text"] for f in chat.INBOX.glob("*.json")]
        self.assertEqual(ideas, ["Dark mode"])
        self.assertTrue(chat.PAUSED.exists())
        self.assertEqual(self.asked(), [])

    def test_a_session_that_fails_says_so_and_does_nothing(self):
        self.team.talk_session = lambda *a: {"ok": False, "data": {}, "error": "boom"}
        self.ask_text("merge everything")
        self.team.cmd_talk(None)
        self.assertIn("could not answer", self.fake.said()[-1])
        self.assertEqual(self.gh, [])
        self.assertEqual(list(chat.ASKS.glob("*.json")), [])

    def test_the_next_text_sees_the_one_before(self):
        self.ask_text("what is open?")
        self.team.cmd_talk(None)
        self.ask_text("and merge the first")
        self.team.cmd_talk(None)
        self.assertIn("He: what is open?\nYou: Sure.", self.prompts[1][2])

    def test_a_run_asked_for_from_telegram_texts_its_outcome_at_night_too(self):
        sent = []
        chat_send = chat.send
        self.addCleanup(lambda: setattr(chat, "send", chat_send))
        chat.send = lambda text, serious=False, **k: sent.append(serious)
        report = {"jobs": [], "skipped": [], "notes": [], "started": "2026-11-17T03:00", "shift": "night"}
        self.team.tell(report, {"tell": True, "scheduled": False, "shift": "night"})
        self.assertEqual(sent, [True])
        self.assertFalse(chat.DIGEST.exists())  # not the night's report
        self.team.tell(report, {"scheduled": False, "shift": "night"})
        self.assertEqual(sent, [True])


if __name__ == "__main__":
    unittest.main()
