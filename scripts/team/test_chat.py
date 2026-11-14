#!/usr/bin/env python3
"""Tests for the team's Telegram line: python3 -m unittest discover -s scripts/team

Telegram is played by a small server on this Mac; nothing here reaches the
real one, and every folder is a temporary one."""

import datetime as dt
import http.server
import importlib
import io
import json
import os
import stat
import sys
import tempfile
import threading
import time
import unittest
from contextlib import redirect_stdout
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import chat  # noqa: E402
import page  # noqa: E402
import rules  # noqa: E402

TOKEN = "12345:not-a-real-token"
OWNER = 4242
DAY = dt.datetime(2026, 11, 16, 10, 0)  # a Monday morning
NIGHT = dt.datetime(2026, 11, 16, 3, 0)


def text(n, words, user=OWNER, chat_id=OWNER, kind="private", reply_to=None):
    message = {
        "message_id": 500 + n,
        "from": {"id": user, "is_bot": False},
        "chat": {"id": chat_id, "type": kind},
        "text": words,
    }
    if reply_to:
        message["reply_to_message"] = {"message_id": reply_to}
    return {"update_id": n, "message": message}


def press(n, data, message_id, user=OWNER, chat_id=OWNER):
    return {
        "update_id": n,
        "callback_query": {
            "id": f"q{n}",
            "from": {"id": user, "is_bot": False},
            "data": data,
            "message": {
                "message_id": message_id,
                "chat": {"id": chat_id, "type": "private"},
            },
        },
    }


class FakeTelegram:
    """Answers the Bot API calls the team makes and remembers them."""

    def __init__(self):
        self.calls, self.paths, self.updates = [], [], []
        self.down = 0  # an HTTP status to answer with instead
        self.texts_left = None  # a number: sendMessage fails once it is used up
        self.message_id = 100
        fake = self

        class Handler(http.server.BaseHTTPRequestHandler):
            def do_POST(self):
                size = int(self.headers.get("Content-Length") or 0)
                params = json.loads(self.rfile.read(size) or b"{}")
                fake.paths.append(self.path)
                status, body = fake.answer(self.path.rsplit("/", 1)[-1], params)
                data = json.dumps(body).encode()
                self.send_response(status)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(data)))
                self.end_headers()
                self.wfile.write(data)

            def log_message(self, *args):
                pass

        self.server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        threading.Thread(target=self.server.serve_forever, daemon=True).start()
        self.base = f"http://127.0.0.1:{self.server.server_address[1]}"

    def answer(self, method, params):
        self.calls.append((method, params))
        if self.down:
            return self.down, {"ok": False, "description": "not now"}
        if method == "getMe":
            return 200, {"ok": True, "result": {"username": "pulse_test_bot"}}
        if method == "sendMessage":
            if self.texts_left is not None:
                if self.texts_left <= 0:
                    return 500, {"ok": False, "description": "not now"}
                self.texts_left -= 1
            self.message_id += 1
            return 200, {"ok": True, "result": {"message_id": self.message_id}}
        if method == "getUpdates":
            # Like Telegram: what is below the offset is confirmed and gone,
            # the rest comes again until it is.
            offset = params.get("offset", 0)
            self.updates = [u for u in self.updates if u["update_id"] >= offset]
            if not self.updates:
                time.sleep(0.02)
            return 200, {"ok": True, "result": list(self.updates)}
        return 200, {"ok": True, "result": True}

    def said(self):
        return [p["text"] for m, p in self.calls if m == "sendMessage"]

    def of(self, method):
        return [p for m, p in self.calls if m == method]

    def close(self):
        self.server.shutdown()
        self.server.server_close()


class ChatCase(unittest.TestCase):
    """A paired bot in a temporary home, talking to the fake Telegram."""

    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.home = Path(self.tmp.name).resolve()
        self.fake = FakeTelegram()
        self.addCleanup(self.fake.close)
        old = (chat.HOME, chat.API, chat.log, chat.banner)
        self.addCleanup(lambda: (chat.set_home(old[0]), self._restore(old)))
        chat.set_home(self.home)
        chat.API = self.fake.base  # a Bot made without an address goes here too
        self.logged, self.banners = [], []
        chat.log = self.logged.append
        chat.banner = self.banners.append
        chat.save_secrets({"token": TOKEN, "chat_id": OWNER, "user_id": OWNER})
        self.line = chat.bot()

    @staticmethod
    def _restore(old):
        chat.API, chat.log, chat.banner = old[1], old[2], old[3]

    def asked(self):
        return json.loads(chat.PROPOSALS.read_text()) if chat.PROPOSALS.exists() else []

    def outbox(self):
        return sorted(chat.OUTBOX.glob("*.json")) if chat.OUTBOX.is_dir() else []

    def ask(self, kind="idea", title="A risk chart", key="k1", job=""):
        """Queue a proposal and text it; returns (its id, its message id)."""
        pid = chat.propose(kind, title, "Because it helps.", key=key, job=job)
        chat.deliver(self.line, DAY)
        return pid, next(p["message_id"] for p in self.asked() if p["id"] == pid)


class ChatRules(unittest.TestCase):
    def test_only_fixed_commands_mean_something(self):
        for words, want in [
            ("idea: dark mode", ("idea", "dark mode")),
            ("Idea:dark  mode\nplease", ("idea", "dark mode please")),
            ("/idea a thing", ("idea", "a thing")),
            ("idea", ("help", "")),
            ("ideas are cheap", ("note", "ideas are cheap")),
            ("status", ("status", "")),
            ("/status@PulseBot", ("status", "")),
            ("status now", ("note", "status now")),
            ("PAUSE", ("pause", "")),
            ("resume", ("resume", "")),
            ("/start 12345678", ("help", "")),
            ("", ("help", "")),
            ("release it; rm -rf ~", ("note", "release it; rm -rf ~")),
        ]:
            self.assertEqual(rules.parse_chat(words), want, words)
        self.assertEqual(len(rules.parse_chat("idea: " + "x" * 900)[1]), rules.IDEA_MAX)

    def test_a_button_is_an_id_and_one_of_five_words(self):
        self.assertEqual(rules.parse_button("p:0123abcd:yes"), ("0123abcd", "yes"))
        for bad in [
            "p:0123abcd:merge",
            "p:xyz:yes",
            "x:0123abcd:yes",
            "",
            None,
            "p:0123abcd:yes\nmore",
        ]:
            self.assertIsNone(rules.parse_button(bad), bad)

    def test_only_the_paired_account_in_its_private_chat_counts(self):
        self.assertTrue(rules.from_owner(text(1, "hi"), OWNER, OWNER))
        self.assertTrue(rules.from_owner(press(1, "p:0123abcd:yes", 7), OWNER, OWNER))
        for update in [
            text(1, "hi", user=999),  # someone else writes to the bot
            text(1, "hi", user=999, chat_id=999),
            text(1, "hi", kind="group"),  # the owner, but in a group
            press(1, "p:0123abcd:yes", 7, user=999),
            {"update_id": 1, "edited_message": {"text": "pause"}},
            {"update_id": 1},
        ]:
            self.assertFalse(rules.from_owner(update, OWNER, OWNER), update)
        bot = text(1, "hi")
        bot["message"]["from"]["is_bot"] = True
        self.assertFalse(rules.from_owner(bot, OWNER, OWNER))
        self.assertFalse(rules.from_owner(text(1, "hi", user=0, chat_id=0), 0, 0))

    def test_the_first_answer_stands(self):
        idea = {"kind": "idea", "state": "sent"}
        self.assertEqual(rules.proposal_step(idea, "yes")[0], "approved")
        self.assertEqual(rules.proposal_step(idea, "later")[0], "later")
        self.assertEqual(rules.proposal_step(idea, "no")[0], "declined")
        for done in ("approved", "declined", "consumed", "expired", "queued"):
            state, reply = rules.proposal_step({"kind": "idea", "state": done}, "yes")
            self.assertEqual(state, done)
            self.assertIn("Already recorded", reply)

    def test_a_question_has_no_later_and_a_trading_change_is_asked_twice(self):
        question = {"kind": "question", "state": "sent"}
        self.assertEqual(rules.proposal_step(question, "later")[0], "sent")
        trade = {"kind": "trading", "state": "sent"}
        self.assertEqual(rules.proposal_step(trade, "yes")[0], "confirming")
        self.assertEqual(
            rules.proposal_step(trade, "confirm")[0], "sent"
        )  # not yet asked
        asked = {"kind": "trading", "state": "confirming"}
        self.assertEqual(rules.proposal_step(asked, "confirm")[0], "approved")
        self.assertEqual(rules.proposal_step(asked, "cancel")[0], "sent")
        self.assertEqual(rules.proposal_step(asked, "yes")[0], "confirming")

    def test_texts_wait_for_the_morning_unless_serious(self):
        late = dt.datetime(2026, 11, 15, 23, 10)
        self.assertEqual(rules.send_at(DAY, False), DAY)
        self.assertEqual(rules.send_at(NIGHT, False), dt.datetime(2026, 11, 16, 8, 0))
        self.assertEqual(rules.send_at(late, False), dt.datetime(2026, 11, 16, 8, 0))
        self.assertEqual(rules.send_at(NIGHT, True), NIGHT)
        self.assertEqual(
            rules.send_at(dt.datetime(2026, 11, 16, 21, 59), False).hour, 21
        )
        self.assertEqual(rules.send_at(dt.datetime(2026, 11, 16, 22, 0), False).day, 17)

    def test_three_new_feature_ideas_a_week_and_no_limit_on_questions(self):
        def queued(kind, n):
            return {"id": f"{kind}{n}", "kind": kind, "state": "queued"}

        asked = [queued("idea", n) for n in range(5)] + [queued("question", 1)]
        due = rules.due_proposals(asked, DAY)
        self.assertEqual(
            [p["id"] for p in due], ["idea0", "idea1", "idea2", "question1"]
        )
        for p in due:
            p["state"], p["sent_at"] = "sent", DAY.isoformat(timespec="seconds")
        friday = DAY + dt.timedelta(days=4)
        self.assertEqual(rules.due_proposals(asked, friday), [])
        monday = DAY + dt.timedelta(days=7)
        self.assertEqual(
            [p["id"] for p in rules.due_proposals(asked, monday)], ["idea3", "idea4"]
        )

    def test_not_now_comes_back_in_two_weeks_and_counts_again(self):
        p = {
            "id": "a",
            "kind": "idea",
            "state": "sent",
            "message_id": 9,
            "sent_at": DAY.isoformat(timespec="seconds"),
        }
        rules.settle(p, "later", DAY)
        self.assertEqual((p["state"], p["message_id"]), ("queued", None))
        self.assertEqual(rules.due_proposals([p], DAY + dt.timedelta(days=13)), [])
        self.assertEqual(rules.due_proposals([p], DAY + dt.timedelta(days=14)), [p])

    def test_a_text_nobody_answered_closes_after_a_week(self):
        old = {
            "state": "sent",
            "sent_at": (DAY - dt.timedelta(days=8)).isoformat(timespec="seconds"),
        }
        new = {
            "state": "confirming",
            "sent_at": (DAY - dt.timedelta(days=6)).isoformat(timespec="seconds"),
        }
        done = {"state": "approved", "sent_at": "2026-01-01T00:00:00"}
        self.assertEqual(rules.expire([old, new, done], DAY), 1)
        self.assertEqual(
            [old["state"], new["state"], done["state"]],
            ["expired", "confirming", "approved"],
        )

    def test_a_first_press_gives_the_second_its_own_week(self):
        p = {
            "kind": "trading",
            "state": "sent",
            "sent_at": (DAY - dt.timedelta(days=6, hours=23)).isoformat(
                timespec="seconds"
            ),
        }
        rules.settle(p, rules.proposal_step(p, "yes")[0], DAY)
        self.assertEqual(p["state"], "confirming")
        self.assertEqual(rules.expire([p], DAY + dt.timedelta(days=1)), 0)
        self.assertEqual(rules.expire([p], DAY + dt.timedelta(days=8)), 1)

    def test_ideas_join_the_list_once_and_a_waiting_one_reopens(self):
        listed = "# Pulse ideas\n\n- [x] dark mode — merged 2026-11-01 (PR #3)\n- [?] Fix x — it asked\n"
        out, added = rules.add_ideas(
            listed, ["Dark  mode", "a new one", "A new one", ""]
        )
        self.assertEqual(added, 1)
        self.assertTrue(out.endswith("- [ ] a new one\n"))
        self.assertIn(
            "- [ ] Fix x\n", rules.reopen_idea(listed, rules.idea_key("Fix x"))
        )
        self.assertEqual(
            rules.reopen_idea(listed, rules.idea_key("dark mode")), listed
        )  # built: left alone

    def test_a_long_idea_or_one_with_a_dash_reopens_by_its_name(self):
        long = "Show " + "every stop " * 25 + "on one page"  # past a title's 200
        dashed = "Signals page — add a filter"
        listed = (
            f"# Pulse ideas\n\n- [?] {long} — it asked\n"
            f"- [?] {dashed} — it asked — twice\n"
        )
        out = rules.reopen_idea(listed, rules.idea_key(long))
        self.assertIn(f"- [ ] {long}\n", out)
        out = rules.reopen_idea(out, rules.idea_key(dashed))
        self.assertTrue(out.endswith(f"- [ ] {dashed}\n"))
        self.assertEqual(rules.reopen_idea(listed, "idea-unknown-000000"), listed)


class Sending(ChatCase):
    def test_by_day_a_text_goes_at_once(self):
        self.assertEqual(chat.send("hello", now=DAY), "sent")
        self.assertEqual(self.fake.said(), ["hello"])
        self.assertEqual(self.fake.of("sendMessage")[0]["chat_id"], OWNER)
        self.assertEqual(self.outbox(), [])

    def test_at_night_it_waits_for_eight_unless_serious(self):
        self.assertEqual(chat.send("the report", now=NIGHT), "queued")
        self.assertEqual(
            chat.send("a position has no stop", serious=True, now=NIGHT), "sent"
        )
        self.assertEqual(self.fake.said(), ["a position has no stop"])
        self.assertEqual(chat.flush(self.line, NIGHT.replace(hour=7, minute=59)), 0)
        self.assertEqual(chat.flush(self.line, NIGHT.replace(hour=8)), 1)
        self.assertEqual(self.fake.said()[-1], "the report")
        self.assertEqual(self.outbox(), [])

    def test_when_telegram_is_down_the_text_is_kept_and_sent_later_in_order(self):
        self.fake.down = 500
        self.assertEqual(chat.send("first", now=DAY), "queued")
        self.assertEqual(chat.send("second, serious", serious=True, now=DAY), "queued")
        self.assertEqual(
            self.banners, ["second, serious"]
        )  # a serious one also shows on the Mac
        self.assertEqual(chat.flush(self.line, DAY), 0)
        self.assertEqual(len(self.outbox()), 2)
        # Three tries so far, none delivered; the failed flush stopped at the
        # first text instead of jumping ahead to the second.
        self.assertEqual(self.fake.said(), ["first", "second, serious", "first"])
        self.fake.down = 0
        self.fake.calls.clear()
        self.assertEqual(chat.flush(self.line, DAY), 2)
        self.assertEqual(self.fake.said(), ["first", "second, serious"])
        self.assertEqual(self.outbox(), [])

    def test_a_text_held_up_by_telegram_still_keeps_to_the_day(self):
        self.fake.down = 500
        chat.send("an ordinary one", now=DAY)
        chat.send("a serious one", serious=True, now=DAY)
        self.fake.down = 0
        self.fake.calls.clear()
        late = DAY.replace(hour=23, minute=30)
        self.assertEqual(chat.flush(self.line, late), 1)
        self.assertEqual(self.fake.said(), ["a serious one"])
        self.assertEqual(chat.flush(self.line, late + dt.timedelta(hours=9)), 1)
        self.assertEqual(self.fake.said()[-1], "an ordinary one")

    def test_a_broken_address_is_an_error_without_the_token(self):
        line = chat.Bot("12345:bad token\n", OWNER, OWNER, self.fake.base)
        with self.assertRaises(chat.ChatError) as caught:
            line.call("getMe")
        self.assertNotIn("bad token", str(caught.exception))
        self.assertEqual(self.fake.calls, [])

    def test_without_a_paired_bot_nothing_is_sent_or_kept(self):
        chat.SECRETS.unlink()
        self.assertEqual(chat.send("hello", now=DAY), "off")
        self.assertEqual((self.fake.calls, self.outbox()), ([], []))
        self.assertIsNone(chat.bot())

    def test_the_token_stays_out_of_errors_and_logs(self):
        self.fake.down = 500
        with self.assertRaises(chat.ChatError) as caught:
            self.line.call("sendMessage", {"text": "x"})
        self.assertNotIn(TOKEN, str(caught.exception))
        self.assertEqual(caught.exception.code, 500)
        dead = chat.Bot(
            TOKEN, OWNER, OWNER, base="http://127.0.0.1:9"
        )  # nothing listens
        with self.assertRaises(chat.ChatError) as caught:
            dead.call("getMe", wait=2)
        self.assertNotIn(TOKEN, str(caught.exception))
        chat.send("kept", now=DAY)
        self.assertFalse(any(TOKEN in line for line in self.logged))
        self.assertTrue(all(TOKEN in path for path in self.fake.paths))  # it was used

    def test_the_token_file_is_for_its_owner_only(self):
        self.assertEqual(stat.S_IMODE(chat.SECRETS.stat().st_mode), 0o600)
        self.assertEqual(stat.S_IMODE(chat.SECRETS.parent.stat().st_mode), 0o700)

    def test_a_long_text_is_cut_to_what_telegram_takes(self):
        chat.send("x" * 9000, now=DAY)
        self.assertEqual(len(self.fake.said()[0]), chat.TEXT_MAX)


class Incoming(ChatCase):
    def test_nobody_else_gets_an_answer_or_changes_anything(self):
        pid, message = self.ask()
        before = len(self.fake.calls)
        for update in [
            text(1, "pause", user=999, chat_id=999),
            text(2, "idea: free pizza", user=999),
            text(3, "pause", kind="group"),
            press(4, f"p:{pid}:yes", message, user=999),
        ]:
            chat.handle(self.line, update, DAY)
        self.assertEqual(len(self.fake.calls), before)
        self.assertFalse(chat.PAUSED.exists())
        self.assertFalse(chat.INBOX.exists())
        self.assertEqual(self.asked()[0]["state"], "sent")

    def test_an_idea_text_waits_in_the_inbox_for_the_runner(self):
        chat.handle(self.line, text(1, "idea: a chart of my risk"), DAY)
        files = list(chat.INBOX.glob("*.json"))
        self.assertEqual(len(files), 1)
        self.assertEqual(json.loads(files[0].read_text())["text"], "a chart of my risk")
        self.assertIn("On the list", self.fake.said()[-1])

    def test_pause_and_resume(self):
        chat.handle(self.line, text(1, "pause"), DAY)
        self.assertTrue(chat.PAUSED.exists())
        self.assertIn("Paused", self.fake.said()[-1])
        chat.handle(self.line, text(2, "status"), DAY)
        self.assertIn("has not run yet", self.fake.said()[-1])
        self.assertIn("Paused", self.fake.said()[-1])
        chat.handle(self.line, text(3, "/resume"), DAY)
        self.assertFalse(chat.PAUSED.exists())

    def test_other_words_do_nothing_but_explain(self):
        chat.handle(self.line, text(1, "merge everything and release it"), DAY)
        self.assertEqual(self.fake.said(), [rules.CHAT_HELP])
        self.assertEqual(self.asked(), [])
        self.assertFalse(chat.INBOX.exists())

    def test_a_proposal_is_texted_with_buttons_and_a_press_decides_it(self):
        pid, message = self.ask()
        sent = self.fake.of("sendMessage")[0]
        self.assertIn("A risk chart", sent["text"])
        self.assertEqual(
            [b["callback_data"] for b in sent["reply_markup"]["inline_keyboard"][0]],
            [f"p:{pid}:yes", f"p:{pid}:later", f"p:{pid}:no"],
        )
        chat.handle(self.line, press(1, f"p:{pid}:yes", message), DAY)
        self.assertEqual(self.asked()[0]["state"], "approved")
        self.assertIn("Build it", self.fake.of("answerCallbackQuery")[0]["text"])
        edit = self.fake.of("editMessageText")[0]
        self.assertEqual(
            edit["reply_markup"], {"inline_keyboard": []}
        )  # the buttons go
        self.assertIn("→ Build it", edit["text"])
        # A second press, or the other button, changes nothing.
        chat.handle(self.line, press(2, f"p:{pid}:no", message), DAY)
        self.assertEqual(self.asked()[0]["state"], "approved")
        self.assertIn(
            "Already recorded", self.fake.of("answerCallbackQuery")[-1]["text"]
        )

    def test_the_answer_is_on_disk_even_if_telegram_never_hears_back(self):
        pid, message = self.ask()
        self.fake.down = 500
        chat.handle(self.line, press(1, f"p:{pid}:no", message), DAY)
        self.assertEqual(self.asked()[0]["state"], "declined")

    def test_a_trading_change_takes_two_presses(self):
        pid, message = self.ask(kind="trading", title="Raise the ATR floor to 3.5%")
        chat.handle(self.line, press(1, f"p:{pid}:yes", message), DAY)
        self.assertEqual(self.asked()[0]["state"], "confirming")
        edit = self.fake.of("editMessageText")[-1]
        self.assertEqual(
            [b["callback_data"] for b in edit["reply_markup"]["inline_keyboard"][0]],
            [f"p:{pid}:confirm", f"p:{pid}:cancel"],
        )
        chat.handle(self.line, press(2, f"p:{pid}:cancel", message), DAY)
        self.assertEqual(self.asked()[0]["state"], "sent")
        chat.handle(self.line, press(3, f"p:{pid}:yes", message), DAY)
        chat.handle(self.line, press(4, f"p:{pid}:confirm", message), DAY)
        self.assertEqual(self.asked()[0]["state"], "approved")

    def test_not_now_takes_it_off_the_table_for_two_weeks(self):
        pid, message = self.ask()
        chat.handle(self.line, press(1, f"p:{pid}:later", message), DAY)
        self.assertEqual(self.asked()[0]["state"], "queued")
        self.assertEqual(chat.deliver(self.line, DAY + dt.timedelta(days=1)), 0)
        self.assertEqual(chat.deliver(self.line, DAY + dt.timedelta(days=14)), 1)

    def test_a_reply_in_words_becomes_a_note_on_that_proposal(self):
        pid, message = self.ask()
        chat.handle(self.line, text(1, "yes but keep it small", reply_to=message), DAY)
        self.assertEqual(self.asked()[0]["note"], "yes but keep it small")
        self.assertEqual(self.asked()[0]["state"], "sent")  # words are not an answer
        self.assertIn("Noted", self.fake.said()[-1])
        chat.handle(self.line, text(2, "and no new colours", reply_to=message), DAY)
        self.assertEqual(
            self.asked()[0]["note"], "yes but keep it small and no new colours"
        )
        chat.handle(self.line, text(3, "a note about nothing", reply_to=999), DAY)
        self.assertEqual(self.fake.said()[-1], rules.CHAT_HELP)

    def test_a_text_telegram_hands_over_twice_is_one_note(self):
        pid, message = self.ask()
        for _ in range(2):  # the listener died before confirming the update
            chat.handle(self.line, text(1, "keep it small", reply_to=message), DAY)
        self.assertEqual(self.asked()[0]["note"], "keep it small")

    def test_a_made_up_button_is_refused(self):
        pid, message = self.ask()
        for data in [f"p:{pid}:merge", "p:ffffffff:yes", "release"]:
            chat.handle(self.line, press(1, data, message), DAY)
        self.assertEqual(self.asked()[0]["state"], "sent")
        self.assertEqual(self.fake.of("editMessageText"), [])

    def test_the_same_thing_is_not_asked_twice(self):
        self.assertTrue(chat.propose("idea", "A risk chart", "why", key="k1"))
        self.assertEqual(chat.propose("idea", "A risk chart", "why", key="k1"), "")
        pid, message = self.ask(key="k2")
        chat.handle(self.line, press(1, f"p:{pid}:no", message), DAY)
        self.assertEqual(
            chat.propose("idea", "again", "why", key="k2"), ""
        )  # a no stays a no


class Listener(ChatCase):
    def test_a_round_sends_what_is_due_and_takes_what_came_in(self):
        chat.spool("the night report", NIGHT.replace(hour=8))
        chat.propose("idea", "A risk chart", "why", key="k1")
        self.fake.updates = [text(7, "pause"), text(8, "idea: more colour")]
        state = {}
        chat.tick(self.line, state, DAY)
        self.assertEqual(self.fake.said()[0], "the night report")
        self.assertEqual(self.asked()[0]["state"], "sent")
        self.assertTrue(chat.PAUSED.exists())
        self.assertEqual(len(list(chat.INBOX.glob("*.json"))), 1)
        self.assertEqual(state["offset"], 9)
        self.assertEqual(json.loads(chat.CHAT_STATE.read_text())["offset"], 9)
        self.assertTrue(state["heartbeat"])
        # The next round confirms them to Telegram and gets nothing twice.
        chat.tick(self.line, state, DAY)
        self.assertEqual(self.fake.of("getUpdates")[-1]["offset"], 9)
        self.assertEqual(len(list(chat.INBOX.glob("*.json"))), 1)

    def test_a_bad_update_does_not_hold_up_the_next(self):
        real = chat.on_text
        self.addCleanup(setattr, chat, "on_text", real)

        def breaks_on_first(line, message, now):
            if message["text"] == "boom":
                raise ValueError("unexpected shape")
            real(line, message, now)

        chat.on_text = breaks_on_first
        self.fake.updates = [text(1, "boom"), text(2, "pause")]
        state = {}
        chat.tick(self.line, state, DAY)
        self.assertTrue(chat.PAUSED.exists())
        self.assertEqual(state["offset"], 3)

    def test_proposals_wait_for_the_day_and_for_their_week(self):
        for n in range(5):
            chat.propose("idea", f"Idea {n}", "why", key=f"k{n}")
        chat.propose(
            "question",
            "PR #5: Thing",
            "it needs a decision",
            key="pr-5@abc",
            job="pr-5",
        )
        self.assertEqual(chat.deliver(self.line, NIGHT), 0)
        self.assertEqual(
            chat.deliver(self.line, DAY), 4
        )  # three ideas and the question
        self.assertEqual(chat.deliver(self.line, DAY + dt.timedelta(days=2)), 0)
        self.assertEqual(chat.deliver(self.line, DAY + dt.timedelta(days=7)), 2)

    def test_a_proposal_telegram_did_not_take_stays_queued(self):
        chat.propose("idea", "A risk chart", "why", key="k1")
        self.fake.down = 500
        self.assertEqual(chat.deliver(self.line, DAY), 0)
        self.assertEqual(self.asked()[0]["state"], "queued")
        self.fake.down = 0
        self.assertEqual(chat.deliver(self.line, DAY), 1)

    def test_each_proposal_is_recorded_as_it_goes(self):
        for n in range(2):
            chat.propose(
                "question", f"PR #{n}: T", "why", key=f"pr-{n}@a", job=f"pr-{n}"
            )
        self.fake.texts_left = 1
        self.assertEqual(chat.deliver(self.line, DAY), 1)
        self.assertEqual([p["state"] for p in self.asked()], ["sent", "queued"])
        self.fake.texts_left = None
        self.assertEqual(chat.deliver(self.line, DAY), 1)
        self.assertEqual(len(self.fake.said()), 3)  # the first was not sent again
        self.assertEqual(len({p["message_id"] for p in self.asked()}), 2)

    def test_a_morning_says_why_there_is_no_report(self):
        today = DAY.date().isoformat()
        chat.DIGEST.write_text(
            json.dumps(
                {"date": "", "tried": today, "problem": "could not reach GitHub"}
            )
        )
        chat.morning(self.line, DAY.replace(hour=9), {})
        chat.PAUSED.write_text("now\n")
        chat.morning(self.line, DAY.replace(hour=9), {})
        self.assertEqual(
            self.fake.said(),
            [
                "No night report today: could not reach GitHub.",
                "No night report today: the team is paused. Send resume to go on.",
            ],
        )

    def test_a_morning_without_a_report_says_so_once(self):
        state = {}
        chat.morning(self.line, DAY.replace(hour=7), state)
        self.assertEqual(self.fake.said(), [])
        chat.morning(self.line, DAY.replace(hour=9), state)
        chat.morning(self.line, DAY.replace(hour=10), state)
        self.assertEqual(
            self.fake.said(), ["No night report today: the night team did not run."]
        )

    def test_a_morning_with_a_report_adds_nothing(self):
        chat.DIGEST.write_text(json.dumps({"date": DAY.date().isoformat()}))
        chat.morning(self.line, DAY, {})
        self.assertEqual(self.fake.said(), [])

    def test_a_night_still_running_is_given_until_noon(self):
        (self.home / "state.json").write_text(
            json.dumps({"last_night": DAY.date().isoformat()})
        )
        state = {}
        chat.morning(self.line, DAY.replace(hour=9), state)
        self.assertEqual(self.fake.said(), [])
        chat.morning(self.line, DAY.replace(hour=12, minute=5), state)
        self.assertIn("has not reported yet", self.fake.said()[0])

    def test_the_listener_stops_when_another_one_holds_the_bot(self):
        naps = []
        real = chat.time.sleep
        self.addCleanup(setattr, chat.time, "sleep", real)
        chat.time.sleep = naps.append
        self.fake.down = 409
        self.assertEqual(chat.run(), 3)
        self.assertEqual(naps, [120])

    def test_without_a_bot_the_listener_does_not_start(self):
        chat.SECRETS.unlink()
        with redirect_stdout(io.StringIO()):
            self.assertEqual(chat.run(), 1)


class Pairing(ChatCase):
    CODE = "00012345"

    def setUp(self):
        super().setUp()
        chat.SECRETS.unlink()
        real = (chat.getpass.getpass, chat.random.randbelow, chat.PAIR_MIN)
        self.addCleanup(self._unpatch, real)
        chat.getpass.getpass = lambda prompt="": f"  {TOKEN}\n"
        chat.random.randbelow = lambda n: 12345

    @staticmethod
    def _unpatch(real):
        chat.getpass.getpass, chat.random.randbelow, chat.PAIR_MIN = real

    def pair(self):
        out = io.StringIO()
        with redirect_stdout(out):
            code = chat.pair()
        return code, out.getvalue()

    def test_the_chat_that_sends_the_code_becomes_the_line(self):
        self.fake.updates = [
            text(0, "ciao è 🙂", user=999, chat_id=999),  # not ASCII: no crash
            text(1, "00099999", user=999, chat_id=999),  # a stranger guessing
            text(2, self.CODE, kind="group"),  # the code, but in a group
            text(3, f"/start {self.CODE}", user=777, chat_id=777),
        ]
        code, shown = self.pair()
        self.assertEqual(code, 0)
        self.assertIn(f"https://t.me/pulse_test_bot?start={self.CODE}", shown)
        self.assertNotIn(TOKEN, shown)
        saved = chat.secrets()
        self.assertEqual(
            (saved["chat_id"], saved["user_id"], saved["token"]), (777, 777, TOKEN)
        )
        self.assertEqual(stat.S_IMODE(chat.SECRETS.stat().st_mode), 0o600)
        self.assertEqual(len(self.fake.said()), 1)  # only the new owner hears back
        self.assertEqual(self.fake.of("sendMessage")[0]["chat_id"], 777)
        self.assertEqual(json.loads(chat.CHAT_STATE.read_text())["offset"], 4)

    def test_without_the_code_nothing_is_saved(self):
        chat.PAIR_MIN = 0.005
        self.fake.updates = [text(1, "hello"), text(2, "00099999")]
        code, shown = self.pair()
        self.assertEqual(code, 1)
        self.assertFalse(chat.SECRETS.exists())
        self.assertEqual(self.fake.said(), [])

    def test_something_that_is_not_a_token_is_not_tried(self):
        for typed in ["", "hello there", "12345:abc def", "12345:abc\x7f"]:
            chat.getpass.getpass = lambda prompt="", typed=typed: typed
            code, shown = self.pair()
            self.assertEqual(code, 1, typed)
        self.assertEqual(self.fake.calls, [])
        self.assertFalse(chat.SECRETS.exists())

    def test_a_token_telegram_rejects_is_not_saved(self):
        self.fake.down = 401
        code, shown = self.pair()
        self.assertEqual(code, 1)
        self.assertFalse(chat.SECRETS.exists())
        self.assertNotIn(TOKEN, shown)


class Runner(ChatCase):
    """What the night runner does with the chat."""

    def setUp(self):
        super().setUp()
        saved = os.environ.get("PULSE_TEAM_HOME")
        os.environ["PULSE_TEAM_HOME"] = str(self.home)
        self.team = importlib.reload(importlib.import_module("team"))
        if saved is None:
            os.environ.pop("PULSE_TEAM_HOME", None)
        else:
            os.environ["PULSE_TEAM_HOME"] = saved
        self.team.log = lambda msg: None

    def test_what_came_from_telegram_joins_the_list_once(self):
        chat.handle(self.line, text(1, "idea: dark mode"), DAY)
        idea, m1 = self.ask(title="A risk chart", key="k1")
        retry, m2 = self.ask(
            kind="question", title="PR #5: Thing", key="pr-5@abc", job="pr-5"
        )
        fix_key = rules.idea_key("Fix x")
        again, m3 = self.ask(
            kind="question", title="Fix x", key=f"{fix_key}@", job=fix_key
        )
        long = "Show " + "every stop " * 25 + "on one page"
        long_key = rules.idea_key(long)
        more, m6 = self.ask(
            kind="question", title=long, key=f"{long_key}@", job=long_key
        )
        red, m7 = self.ask(
            kind="question", title="Fix failing checks on main", key="main@", job="main"
        )
        no, m4 = self.ask(title="Confetti", key="k2")
        trade, m5 = self.ask(kind="trading", title="Raise the floor", key="t1")
        chat.handle(self.line, text(2, "keep it small", reply_to=m1), DAY)
        chat.handle(self.line, text(3, "use the old API", reply_to=m2), DAY)
        for n, (pid, message, answer) in enumerate(
            [
                (idea, m1, "yes"),
                (retry, m2, "yes"),
                (again, m3, "yes"),
                (more, m6, "yes"),
                (red, m7, "yes"),
                (no, m4, "no"),
                (trade, m5, "yes"),
                (trade, m5, "confirm"),
            ]
        ):
            chat.handle(self.line, press(10 + n, f"p:{pid}:{answer}", message), DAY)
        self.team.IDEAS.write_text(
            "# Pulse ideas\n\n- [?] Fix x — it asked a question\n"
            f"- [?] {long} — it asked too\n"
        )
        parked = {"sha": "abc", "result": "needs_user", "tries": 1, "parked": True}
        stuck = {"sha": "def", "result": "needs_user", "tries": 1}
        state = {"attempts": {"pr-5": dict(parked), "main": dict(stuck)}}
        report = {"notes": []}

        self.team.from_chat(state, report)

        listed = self.team.IDEAS.read_text()
        for line in [
            "- [ ] Fix x\n",
            f"- [ ] {long}\n",  # longer than the title a text keeps
            "- [ ] dark mode\n",
            "- [ ] A risk chart\n",
        ]:
            self.assertIn(line, listed)
        self.assertNotIn("main", state["attempts"])  # a red main is tried again
        self.assertNotIn("Confetti", listed)
        self.assertNotIn("Raise the floor", listed)
        self.assertEqual(list(chat.INBOX.glob("*.json")), [])
        states = {p["title"]: p["state"] for p in self.asked()}
        self.assertEqual(
            states,
            {
                "A risk chart": "consumed",
                "PR #5: Thing": "consumed",
                "Fix x": "consumed",
                long[:200]: "consumed",
                "Fix failing checks on main": "consumed",
                "Confetti": "declined",
                "Raise the floor": "approved",
            },
        )
        self.assertEqual(
            state["notes"],
            {
                rules.idea_key("A risk chart"): "keep it small",
                "pr-5": "use the old API",
            },
        )
        self.assertEqual(state["attempts"]["pr-5"]["tries"], 0)  # the stop is forgotten
        self.assertEqual(report["notes"], ["2 ideas from Telegram joined the list"])
        self.assertEqual(
            json.loads((self.home / "state.json").read_text())["notes"], state["notes"]
        )
        # A second run finds nothing new.
        self.team.from_chat(state, report)
        self.assertEqual(self.team.IDEAS.read_text(), listed)

    def test_the_night_is_texted_and_each_stop_becomes_a_question(self):
        report = {
            "started": "2026-11-16T02:30",
            "jobs": [
                {
                    "kind": "idea",
                    "key": "idea-a",
                    "title": "Add a",
                    "pr": 12,
                    "result": "merged",
                },
                {
                    "kind": "pr",
                    "key": "pr-5",
                    "title": "Thing",
                    "pr": 5,
                    "result": "needs_user",
                    "pushed": "abc",
                    "reasons": ["it touches .github/workflows/ci.yml"],
                },
                {  # an idea that became a pull request and then stopped
                    "kind": "idea",
                    "key": "idea-b",
                    "title": "Add b",
                    "pr": 13,
                    "result": "blocked",
                    "pushed": "def",
                },
                {
                    "kind": "idea",
                    "key": "idea-c",
                    "title": "Add c",
                    "result": "needs_user",
                },
                {
                    "kind": "main",
                    "key": "main",
                    "title": "Fix main",
                    "result": "blocked",
                },
            ],
            "notes": [],
            "unreleased": {"tag": "v0.8.7", "merges": 2},
        }
        self.team.ask(report, {"scheduled": True})
        self.team.tell(report, {"scheduled": True})
        asked = self.asked()
        self.assertEqual(
            [(p["kind"], p["title"], p["key"], p["job"], p["state"]) for p in asked],
            [
                ("question", "PR #5: Thing", "pr-5@abc", "pr-5", "queued"),
                # "Try again" has to name the job that will run: the PR.
                ("question", "PR #13: Add b", "pr-13@def", "pr-13", "queued"),
                ("question", "Add c", "idea-c@", "idea-c", "queued"),
                ("question", "Fix main", "main@", "main", "queued"),
            ],
        )
        self.assertIn("ci.yml", asked[0]["text"])
        self.assertEqual(
            json.loads(chat.DIGEST.read_text())["date"], dt.date.today().isoformat()
        )
        went = self.fake.said() + [
            json.loads(p.read_text())["text"] for p in self.outbox()
        ]
        self.assertEqual(len(went), 1)  # by day at once, at night for the morning
        for part in [
            "Merged: 1",
            "Add a (PR #12)",
            "Stopped for you: 4",
            "Not in your app yet: 2",
        ]:
            self.assertIn(part, went[0])
        # Asking again (the same stop, the same commit) asks nothing new.
        self.team.ask(report, {"scheduled": True})
        self.assertEqual(len(self.asked()), 4)

    def test_a_night_that_will_be_tried_again_sends_no_report_yet(self):
        report = {
            "started": "2026-11-16T02:30",
            "jobs": [],
            "notes": ["could not reach GitHub; nothing was done"],
        }
        for _ in range(3):  # three hourly tries that got nowhere
            self.team.tell(report, {"scheduled": True, "nothing_done": True})
        self.assertEqual((self.fake.said(), self.outbox()), ([], []))
        digest = json.loads(chat.DIGEST.read_text())
        self.assertEqual(
            (digest["date"], digest["tried"], digest["problem"]),
            ("", dt.date.today().isoformat(), report["notes"][0]),
        )
        self.team.tell(dict(report, notes=[]), {"scheduled": True})  # the one that ran
        self.assertEqual(len(self.fake.said() + self.outbox()), 1)
        self.assertEqual(
            json.loads(chat.DIGEST.read_text())["date"], dt.date.today().isoformat()
        )

    def test_a_run_started_by_hand_texts_nobody(self):
        report = {
            "started": "2026-11-16T15:00",
            "jobs": [
                {
                    "kind": "pr",
                    "key": "pr-5",
                    "title": "Thing",
                    "pr": 5,
                    "result": "blocked",
                }
            ],
        }
        self.team.ask(report, {"scheduled": False})
        self.team.tell(report, {"scheduled": False})
        self.assertEqual((self.fake.calls, self.asked(), self.outbox()), ([], [], []))

    def test_a_chat_that_is_down_never_fails_the_night(self):
        self.fake.down = 500
        self.team.tell({"started": "2026-11-16T02:30", "jobs": []}, {"scheduled": True})
        chat.PROPOSALS.write_text("not json")
        stopped = {
            "started": "2026-11-16T02:30",
            "jobs": [
                {
                    "kind": "pr",
                    "key": "pr-5",
                    "title": "T",
                    "pr": 5,
                    "result": "blocked",
                }
            ],
        }
        self.team.ask(stopped, {"scheduled": True})
        self.team.tell(stopped, {"scheduled": True})
        # A list that cannot be written must not cost the night either.
        real = chat.propose
        self.addCleanup(setattr, chat, "propose", real)
        chat.propose = lambda *a, **k: (_ for _ in ()).throw(OSError("disk"))
        self.team.ask(stopped, {"scheduled": True})

    def test_a_paused_team_lets_the_night_pass_without_spending_it(self):
        worked = []
        self.team.work = lambda *a: worked.append(1)
        self.team.finish = lambda *a: 0
        self.addCleanup(setattr, rules, "night_due", rules.night_due)
        rules.night_due = lambda now, start, late, last: "2026-11-16"
        args = type("Args", (), {"scheduled": True, "no_merge": False, "only": ""})()
        chat.PAUSED.write_text("now\n")
        self.assertEqual(self.team.cmd_run(args), 0)
        self.assertEqual(worked, [])
        self.assertFalse((self.home / "state.json").exists())
        chat.PAUSED.unlink()  # resumed before the morning: the next wake-up starts it
        self.team.cmd_run(args)
        self.assertEqual(worked, [1])

    def test_the_listener_runs_from_the_installed_copy_and_holds_no_secret(self):
        job = self.team.chat_job("/usr/bin/python3", self.home / "bin")
        self.assertEqual(
            job["ProgramArguments"][1:], [str(self.home / "bin/team.py"), "chat", "run"]
        )
        self.assertTrue(job["KeepAlive"])
        self.assertNotIn(TOKEN, json.dumps(job))
        self.assertEqual(
            set(job["EnvironmentVariables"]), {"PATH", "HOME", "PULSE_TEAM_HOME"}
        )

    def test_sessions_never_get_the_token(self):
        env = self.team.night_env()
        self.assertFalse(any(TOKEN in str(v) for v in env.values()))
        self.assertFalse(any("TELEGRAM" in k.upper() for k in env))

    def test_the_page_lists_what_waits_in_telegram(self):
        report = {
            "jobs": [],
            "asked": [
                {"kind": "question", "title": "PR #5: Thing", "state": "sent"},
                {"kind": "question", "title": "PR #6: New", "state": "queued"},
                {"kind": "idea", "title": "Later one", "state": "queued"},
            ],
        }
        waiting = page.needs_you(report)
        self.assertEqual(len(waiting), 1)
        self.assertIn("2 questions wait", waiting[0])
        self.assertIn("PR #6: New", waiting[0])  # last night's, out with the morning
        self.assertIn("PR #5: Thing", waiting[0])
        self.assertNotIn("Later one", waiting[0])

    def test_the_digest_says_when_there_was_nothing_to_do(self):
        words = page.chat_digest(
            {"started": "2026-11-16T02:30", "jobs": []}, "https://x/page"
        )
        self.assertIn("Nothing to work on.", words)
        self.assertTrue(words.endswith("Page: https://x/page"))


if __name__ == "__main__":
    unittest.main()
