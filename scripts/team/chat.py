#!/usr/bin/env python3
"""The team's Telegram line to Nicola: texts out, button presses and a few
fixed commands in.

What is typed in Telegram is data and never runs as a command:
`rules.parse_chat` knows a handful of fixed words, a button press moves one
proposal one step (`rules.proposal_step`), and any other text is at most a note
kept with the proposal it replies to. Two things do reach a builder, as the
request it works on inside the fence and behind the merge gate: an `idea:` line
(it joins the idea list, like `team.py idea`) and such a note. Only the account
that paired is answered; everyone else is dropped unanswered.

The bot token is in ~/.pulse-team/secrets/telegram.json (owner-only). It is
read here and nowhere else, and never goes into the environment, a log or an
error message. Sessions are fenced off from that folder.

Standard library only: this runs from the installed copy under launchd."""

from __future__ import annotations

import contextlib
import datetime as dt
import fcntl
import getpass
import hmac
import http.client
import json
import os
import re
import secrets as random
import subprocess
import sys
import time
import urllib.error
import urllib.request
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import rules  # noqa: E402


def set_home(home: Path) -> None:
    """Point every path at the team's folder (the runner passes its own)."""
    global HOME, SECRETS, PROPOSALS, OUTBOX, INBOX, PAUSED, CHAT_STATE, DIGEST  # noqa: PLW0603
    HOME = Path(home)
    SECRETS = HOME / "secrets/telegram.json"
    PROPOSALS = HOME / "proposals.json"  # what the team asked, what was answered
    OUTBOX = HOME / "outbox"  # texts waiting for the morning, or for Telegram
    INBOX = HOME / "inbox"  # ideas Nicola texted, for the runner to put on the list
    PAUSED = HOME / "paused"  # exists = nothing new starts and nothing merges
    CHAT_STATE = HOME / "chat.json"  # the listener's own: update offset, heartbeat
    DIGEST = HOME / "digest.json"  # the day of the last night report


set_home(Path(os.environ.get("PULSE_TEAM_HOME") or Path.home() / ".pulse-team"))
API = "https://api.telegram.org"
POLL_S = 50  # Telegram holds a getUpdates call open this long
TEXT_MAX = 3900  # Telegram's limit is 4096
PAIR_MIN = 10


class ChatError(Exception):
    def __init__(self, message: str, code: int = 0):
        super().__init__(message)
        self.code = code


def log(msg: str) -> None:
    print(f"{dt.datetime.now():%m-%d %H:%M:%S} {msg}", flush=True)


def _load(path: Path, default):
    try:
        return json.loads(path.read_text())
    except (OSError, ValueError):
        return default


def _save(path: Path, data) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    tmp = path.with_suffix(".tmp")
    tmp.write_text(json.dumps(data, indent=2))
    tmp.replace(path)


def _stamp(when: dt.datetime) -> str:
    return when.isoformat(timespec="seconds")


# ------------------------------------------------------------- Telegram --


class Bot:
    """One bot talking to one chat. The only place the token is used."""

    def __init__(self, token: str, chat_id: int = 0, user_id: int = 0, base: str = ""):
        self._token, self.chat_id, self.user_id = token, chat_id, user_id
        self._base = (base or API).rstrip("/")

    def _clean(self, text: str) -> str:
        return text.replace(self._token, "<token>") if self._token else text

    def call(self, method: str, params: dict | None = None, wait: int = 20):
        req = urllib.request.Request(
            f"{self._base}/bot{self._token}/{method}",
            data=json.dumps(params or {}).encode(),
            headers={"Content-Type": "application/json"},
        )
        try:
            with urllib.request.urlopen(req, timeout=wait) as reply:  # noqa: S310
                body = json.load(reply)
        except urllib.error.HTTPError as e:
            try:
                detail = json.load(e).get("description", "")
            except (OSError, ValueError, AttributeError):
                detail = ""
            finally:
                e.close()
            raise ChatError(
                self._clean(f"{method}: HTTP {e.code} {detail}".strip()), e.code
            ) from None
        except http.client.HTTPException as e:
            # Its text can quote the address, and the token is part of that.
            raise ChatError(f"{method}: {type(e).__name__}") from None
        except (OSError, ValueError) as e:
            raise ChatError(self._clean(f"{method}: {e}")) from None
        if not isinstance(body, dict) or not body.get("ok"):
            raise ChatError(self._clean(f"{method}: refused"))
        return body.get("result")

    def say(self, text: str, buttons=None) -> int:
        params = {
            "chat_id": self.chat_id,
            "text": text[:TEXT_MAX],
            "disable_web_page_preview": True,
        }
        if buttons:
            params["reply_markup"] = _keyboard(buttons)
        return (self.call("sendMessage", params) or {}).get("message_id", 0)


def _keyboard(buttons) -> dict:
    return {"inline_keyboard": [[{"text": t, "callback_data": d} for t, d in buttons]]}


def secrets() -> dict:
    data = _load(SECRETS, {})
    return data if isinstance(data, dict) else {}


def paired() -> bool:
    s = secrets()
    return bool(s.get("token") and s.get("chat_id") and s.get("user_id"))


def bot() -> Bot | None:
    s = secrets()
    if not paired():
        return None
    return Bot(s["token"], s["chat_id"], s["user_id"])


def save_secrets(data: dict) -> None:
    SECRETS.parent.mkdir(parents=True, exist_ok=True)
    os.chmod(SECRETS.parent, 0o700)
    tmp = SECRETS.with_suffix(".tmp")
    fd = os.open(tmp, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
    with os.fdopen(fd, "w") as f:
        json.dump(data, f)
    os.chmod(tmp, 0o600)
    tmp.replace(SECRETS)


# ------------------------------------------------------------ proposals --


@contextlib.contextmanager
def proposals():
    """The proposal list under a lock: the daemon and the runner both write it.
    Saved when the block ends without an error."""
    HOME.mkdir(parents=True, exist_ok=True)
    with open(HOME / "proposals.lock", "w") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        items = _load(PROPOSALS, [])
        items = items if isinstance(items, list) else []
        yield items
        _save(PROPOSALS, items)


def propose(kind: str, title: str, text: str, key: str, job: str = "") -> str:
    """Queue something to ask Nicola; the daemon texts it when it may. One per
    `key`: asked, waiting or declined is not asked again. Returns the id, or ''
    when it was already there."""
    now = _stamp(dt.datetime.now())
    with proposals() as items:
        if any(
            p.get("key") == key and p.get("state") not in ("consumed", "expired")
            for p in items
        ):
            return ""
        pid = rules.proposal_id(kind, key, now)
        items.append(
            {
                "id": pid,
                "kind": kind,
                "key": key,
                "job": job,
                "title": " ".join(title.split())[:200],
                "text": text.strip()[:1500],
                "state": "queued",
                "created": now,
            }
        )
    return pid


def proposal_text(p: dict) -> str:
    head = {
        "idea": "I was thinking of adding this:",
        "question": "This needs you:",
        "trading": "A trading change, with its proof:",
    }.get(p.get("kind"), "")
    return f"{head}\n{p.get('title', '')}\n\n{p.get('text', '')}".strip()


def proposal_buttons(p: dict):
    options = (
        rules.CONFIRM
        if p.get("state") == "confirming"
        else rules.BUTTONS.get(p.get("kind"), ())
    )
    return [(label, f"p:{p['id']}:{answer}") for label, answer in options]


def deliver(line: Bot, now: dt.datetime) -> int:
    """Text the proposals that are due. They are never urgent, so not at night."""
    if rules.send_at(now, serious=False) > now:
        return 0
    with proposals() as items:
        due = [dict(p) for p in rules.due_proposals(items, now)]
    sent = 0
    for p in due:
        # Telegram is called without the lock, so a slow answer never holds up
        # the runner, and each one is on disk before the next is sent.
        p["state"] = "sent"  # the buttons are built for the state it will have
        try:
            message_id = line.say(proposal_text(p), proposal_buttons(p))
        except ChatError as e:
            log(f"could not send a proposal: {e}")
            break
        with proposals() as items:
            for q in items:
                if q.get("id") == p["id"] and q.get("state") == "queued":
                    q["state"], q["message_id"] = "sent", message_id
                    q["sent_at"] = _stamp(now)
        sent += 1
    return sent


# --------------------------------------------------------------- sending --


def spool(text: str, not_before: dt.datetime, serious: bool = False) -> None:
    OUTBOX.mkdir(parents=True, exist_ok=True)
    name = f"{time.time_ns()}-{random.token_hex(3)}.json"
    _save(
        OUTBOX / name,
        {"text": text, "not_before": _stamp(not_before), "serious": serious},
    )


def banner(text: str) -> None:
    """A notification on the Mac, for a serious text Telegram did not take."""
    script = 'display notification (item 1 of argv) with title "Pulse team" sound name "Basso"'
    with contextlib.suppress(OSError, subprocess.SubprocessError):
        subprocess.run(
            ["/usr/bin/osascript", "-e", f"on run argv\n{script}\nend run", text[:200]],
            capture_output=True,
            timeout=10,
            check=False,
        )


def send(
    text: str,
    serious: bool = False,
    now: dt.datetime | None = None,
    line: Bot | None = None,
) -> str:
    """Text Nicola. By day it goes at once; at night it waits for the morning
    unless it is serious. Returns 'sent', 'queued' (the daemon sends it when it
    can) or 'off' (no bot is paired). Never raises."""
    now = now or dt.datetime.now()
    line = line or bot()
    if not line:
        return "off"
    when = rules.send_at(now, serious)
    if when > now:
        spool(text, when)
        return "queued"
    try:
        line.say(text)
        return "sent"
    except ChatError as e:
        log(f"could not send: {e}")
        spool(text, now, serious)
        if serious:
            banner(text)
        return "queued"


def flush(line: Bot, now: dt.datetime) -> int:
    """Send what waits in the outbox and is due, oldest first. A text that is
    not serious keeps to the day, also when it is late only because Telegram
    was down."""
    sent, quiet = 0, rules.send_at(now, serious=False) > now
    for path in sorted(OUTBOX.glob("*.json")) if OUTBOX.is_dir() else []:
        item = _load(path, None)
        if not isinstance(item, dict) or not item.get("text"):
            path.unlink(missing_ok=True)
            continue
        if (item.get("not_before") or "") > _stamp(now):
            continue
        if quiet and not item.get("serious"):
            continue
        try:
            line.say(item["text"])
        except ChatError as e:
            log(f"outbox: {e}")
            break
        path.unlink(missing_ok=True)
        sent += 1
    return sent


# -------------------------------------------------------------- incoming --


def status_text() -> str:
    latest = HOME / "latest.txt"
    parts = [
        latest.read_text().strip()
        if latest.exists()
        else "The night team has not run yet."
    ]
    if PAUSED.exists():
        parts.append(
            "Paused: nothing new starts and nothing merges. Send resume to go on."
        )
    waiting = [p for p in _load(PROPOSALS, []) if p.get("state") in rules.OPEN_STATES]
    if waiting:
        parts.append(
            f"{len(waiting)} of my questions above still wait for your answer."
        )
    return "\n\n".join(parts)


def on_press(line: Bot, query: dict, now: dt.datetime) -> None:
    parsed = rules.parse_button(query.get("data") or "")
    reply, shown = "I do not know that button.", None
    if parsed:
        pid, answer = parsed
        with proposals() as items:  # the answer is on disk before Telegram hears back
            p = next((p for p in items if p.get("id") == pid), None)
            if p:
                state, reply = rules.proposal_step(p, answer)
                if state != p.get("state"):
                    rules.settle(p, state, now)
                    shown = dict(p, state=state)
    with contextlib.suppress(ChatError):
        line.call(
            "answerCallbackQuery",
            {"callback_query_id": query.get("id"), "text": reply[:190]},
        )
    message = (query.get("message") or {}).get("message_id")
    if shown and message:
        # The message keeps the question, shows the answer, and loses buttons
        # that no longer apply.
        buttons = proposal_buttons(shown) if shown["state"] in rules.OPEN_STATES else []
        edit = {
            "chat_id": line.chat_id,
            "message_id": message,
            "text": (
                proposal_text(shown)
                + (
                    f"\n\n→ {reply}"
                    if not buttons or shown["state"] == "confirming"
                    else ""
                )
            )[:TEXT_MAX],
            "reply_markup": _keyboard(buttons) if buttons else {"inline_keyboard": []},
        }
        with contextlib.suppress(ChatError):
            line.call("editMessageText", edit)


def on_text(line: Bot, message: dict, now: dt.datetime) -> None:
    command, arg = rules.parse_chat(message.get("text") or "")
    if command == "idea":
        INBOX.mkdir(parents=True, exist_ok=True)
        _save(
            INBOX / f"{time.time_ns()}-{random.token_hex(3)}.json",
            {"kind": "idea", "text": arg, "at": _stamp(now)},
        )
        reply = "On the list. The team builds from the top at night."
    elif command == "status":
        reply = status_text()
    elif command == "pause":
        PAUSED.write_text(_stamp(now) + "\n")
        reply = "Paused. Nothing new starts and nothing merges until you send resume."
    elif command == "resume":
        PAUSED.unlink(missing_ok=True)
        reply = "Back to work."
    elif command == "note":
        target = (message.get("reply_to_message") or {}).get("message_id")
        kept = False
        with proposals() as items:
            for p in items:
                if (
                    target
                    and p.get("message_id") == target
                    and p.get("state") in (*rules.OPEN_STATES, "approved")
                ):
                    # Telegram hands an update over again when the listener
                    # died before confirming it: the same text is kept once.
                    seen = p.setdefault("note_ids", [])
                    if message.get("message_id") not in seen:
                        seen.append(message.get("message_id"))
                        p["note"] = f"{p.get('note', '')} {arg}".strip()[:1000]
                    kept = True
        reply = "Noted. The builder will see it." if kept else rules.CHAT_HELP
    else:
        reply = rules.CHAT_HELP
    try:
        line.say(reply)
    except ChatError as e:
        log(f"could not answer: {e}")


def handle(line: Bot, update: dict, now: dt.datetime) -> None:
    if not rules.from_owner(update, line.chat_id, line.user_id):
        return  # not Nicola: no answer, no trace of what was sent
    if "callback_query" in update:
        on_press(line, update["callback_query"], now)
    elif "message" in update:
        on_text(line, update["message"], now)


# ---------------------------------------------------------------- daemon --


def morning(line: Bot, now: dt.datetime, state: dict) -> None:
    """Every day brings one text. A night that ran left its report in the
    outbox; when none did, say so instead of staying silent."""
    today = now.date().isoformat()
    if state.get("morning") == today:
        return
    digest = _load(DIGEST, {})
    digest = digest if isinstance(digest, dict) else {}
    if digest.get("date") == today:
        state["morning"] = today
        return
    team = _load(HOME / "state.json", {})
    started = isinstance(team, dict) and team.get("last_night") == today
    if started and 12 <= now.hour < 22:
        text = "The night team started last night and has not reported yet."
    elif not started and 8 <= now.hour < 12:
        if PAUSED.exists():
            why = "the team is paused. Send resume to go on"
        elif digest.get("tried") == today and digest.get("problem"):
            why = str(digest["problem"])[:300]  # every try of the night failed
        else:
            why = "the night team did not run"
        text = f"No night report today: {why}."
    else:
        return
    with contextlib.suppress(ChatError):
        line.say(text)
        state["morning"] = today


def tick(line: Bot, state: dict, now: dt.datetime) -> None:
    """One round of the daemon: send what is due, then wait for Nicola."""
    flush(line, now)
    deliver(line, now)
    morning(line, now, state)
    with proposals() as items:
        rules.expire(items, now)
    state["heartbeat"] = _stamp(now)
    _save(CHAT_STATE, state)
    updates = line.call(
        "getUpdates",
        {
            "offset": state.get("offset", 0),
            "timeout": POLL_S,
            "allowed_updates": ["message", "callback_query"],
        },
        wait=POLL_S + 15,
    )
    for update in updates or []:
        try:
            handle(line, update, dt.datetime.now())
        except Exception as e:  # noqa: BLE001 - one bad update must not block the rest
            log(f"an update could not be handled: {type(e).__name__}")
        state["offset"] = int(update.get("update_id", 0)) + 1
        _save(CHAT_STATE, state)


def run() -> int:
    line = bot()
    if not line:
        print("No bot is paired. Run: team.py chat pair")
        return 1
    state = _load(CHAT_STATE, {})
    state = state if isinstance(state, dict) else {}
    log("listening")
    while True:
        try:
            tick(line, state, dt.datetime.now())
        except ChatError as e:
            log(str(e))
            if e.code in (401, 409):
                # 401: the token was withdrawn. 409: another program is
                # listening with it. Neither gets better by hammering.
                time.sleep(120)
                return 3
            time.sleep(15)


def pair(base: str = "") -> int:
    """Type the token (hidden), then prove from the phone that the chat is
    yours by sending the bot a one-time code."""
    token = getpass.getpass(
        "Paste the bot token from BotFather (it stays hidden): "
    ).strip()
    if not re.fullmatch(r"\d+:[A-Za-z0-9_-]+", token):
        print("That does not look like a bot token; nothing was saved.")
        return 1
    line = Bot(token, base=base)
    try:
        me = line.call("getMe") or {}
    except ChatError as e:
        print(f"Telegram did not accept that token ({e}); nothing was saved.")
        return 1
    code = f"{random.randbelow(10**8):08d}"
    print(
        f"\nOn your phone open  https://t.me/{me.get('username', '')}?start={code}  and press Start."
        f"\n(Or open the bot and send it:  {code} )\nWaiting up to {PAIR_MIN} minutes..."
    )
    deadline, offset = time.time() + PAIR_MIN * 60, 0
    while time.time() < deadline:
        try:
            updates = line.call(
                "getUpdates",
                {"offset": offset, "timeout": 25, "allowed_updates": ["message"]},
                wait=40,
            )
        except ChatError as e:
            print(f"Telegram: {e}")
            time.sleep(5)
            continue
        for update in updates or []:
            offset = int(update.get("update_id", 0)) + 1
            message = update.get("message") or {}
            words = (message.get("text") or "").split()
            chat, sender = message.get("chat") or {}, message.get("from") or {}
            if (
                chat.get("type") == "private"
                and words
                # As bytes: compare_digest refuses text that is not ASCII.
                and hmac.compare_digest(words[-1].encode(), code.encode())
            ):
                save_secrets(
                    {
                        "token": token,
                        "chat_id": chat.get("id"),
                        "user_id": sender.get("id"),
                        "bot": me.get("username", ""),
                        "paired": _stamp(dt.datetime.now()),
                    }
                )
                _save(CHAT_STATE, {"offset": offset})
                with contextlib.suppress(ChatError):
                    Bot(token, chat.get("id"), sender.get("id"), base).say(
                        "Paired. This chat is now the Pulse team's line to you.\n\n"
                        + rules.CHAT_HELP
                    )
                print("Paired.")
                return 0
    print("No message with the code arrived; nothing was saved.")
    return 1
