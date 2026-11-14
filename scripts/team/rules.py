"""Decision rules for the night team. Pure functions, no I/O, so every rule has a unit test.

team.py does the running (git, gh, claude); this file decides what is allowed.
"""

from __future__ import annotations

import datetime as dt
import fnmatch
import hashlib
import json
import re

# Files the night team may not change without Nicola. A PR that touches one is
# still checked and reviewed, but the team changes nothing in it, pushes nothing
# and leaves it open with the `needs-you` label. Work of the team's own that
# touches one never leaves this Mac.
#   - CI and the team's own scripts are the gate itself.
#   - launchd/, install and schedule scripts decide what runs on the Mac.
#   - tauri.conf.json / capabilities hold the updater key and app permissions.
#   - go_live.rs holds thresholds that must not be tuned to a result.
#   - .claude/ and .mcp.json hold settings, hooks, skills and connectors that
#     Claude sessions load and run; .cargo/ can change how everything is built.
PROTECTED_GLOBS = [
    ".github/*",
    ".claude/*",
    ".mcp.json",
    ".cargo/*",
    "scripts/team/*",
    "launchd/*",
    "scripts/install-app.sh",
    "scripts/install-launchd.sh",
    "scripts/uninstall-launchd.sh",
    "scripts/scheduled-fetch.sh",
    "scripts/live-signals.sh",
    "src-tauri/tauri.conf.json",
    "src-tauri/capabilities/*",
    "src-tauri/src/services/go_live.rs",
    "rust-toolchain.toml",
    ".env",
    ".env.*",
    "*/.env",
    "*/.env.*",
]

# Orders may only ever go to the paper endpoint. `data.alpaca.markets` (prices)
# and `paper-api.alpaca.markets` are fine; the live trading host is not.
LIVE_ENDPOINT = re.compile(r"(?<![\w-])api\.alpaca\.markets")

CI_PASS = {"SUCCESS", "NEUTRAL", "SKIPPED"}

# Branches the team makes for its own work. A red main is fixed on MAIN_FIX_PREFIX.
MAIN_FIX_PREFIX = "fix/night-main-"
TEAM_PREFIXES = (MAIN_FIX_PREFIX, "feat/night-")

IDEA_LINE = re.compile(r"^(\s*[-*]\s+)(?:\[( |x|X|\?)\]\s+)?(\S.*)$")


def parse_name_status(text: str) -> list[tuple[str, str]]:
    """`git diff --name-status -z` output -> [(status letter, path)]. A rename or
    copy yields both paths: the old one as deleted (D), the new one as added (A).
    (-z, because without it git quotes unusual file names and they match nothing.)"""
    fields = text.split("\0")
    out, i = [], 0
    while i < len(fields):
        status = fields[i].strip()
        if not status:
            i += 1
            continue
        letter = status[0]
        if letter in "RC" and i + 2 < len(fields):
            if letter == "R":
                out.append(("D", fields[i + 1]))
            out.append(("A", fields[i + 2]))
            i += 3
        elif i + 1 < len(fields):
            out.append((letter, fields[i + 1]))
            i += 2
        else:
            break
    return out


def protected_violations(changes: list[tuple[str, str]]) -> list[str]:
    """Plain-language reasons this change needs Nicola. Empty list = fine."""
    reasons = []
    for status, path in changes:
        if any(fnmatch.fnmatch(path, g) for g in PROTECTED_GLOBS):
            reasons.append(f"changes a protected file: {path}")
        elif path.startswith("migrations/") and status != "A":
            # A migration already applied to the live database is frozen.
            reasons.append(f"edits or removes an existing migration: {path}")
    return reasons


def forbidden_content(diff: str) -> list[str]:
    """Scan the ADDED lines of a unified diff for things that must never merge unattended."""
    for line in diff.splitlines():
        if (
            line.startswith("+")
            and not line.startswith("+++")
            and LIVE_ENDPOINT.search(line)
        ):
            return ["adds the live (real money) Alpaca trading address"]
    return []


def script_violations(old_pkg: str, new_pkg: str) -> list[str]:
    """package.json before and after. The checks are `pnpm check` and `pnpm test`,
    and installing runs the lifecycle scripts, so a change to `scripts` can
    switch a check off without touching a protected file."""

    def scripts(text: str):
        if not text.strip():
            return {}
        try:
            data = json.loads(text)
        except ValueError:
            return None
        found = data.get("scripts") if isinstance(data, dict) else None
        return found if isinstance(found, dict) else {}

    old, new = scripts(old_pkg), scripts(new_pkg)
    if old is None or new is None or old != new:
        return ["changes the commands the checks run (the scripts in package.json)"]
    return []


def merge_blockers(view: dict, sha: str, base: str, label: str) -> str:
    """The last look at a pull request before merging it: '' when it is still
    the one that was checked (same commit, same target, nobody stepped in) and
    GitHub can merge it, else why not."""
    if view.get("state") != "OPEN":
        return f"the pull request is no longer open ({view.get('state') or 'unknown'})"
    if view.get("headRefOid") != sha:
        return "someone pushed to it while the job ran"
    if view.get("baseRefName") != base:
        return f"it no longer targets {base}"
    if view.get("isDraft"):
        return "it was marked as a draft while the job ran"
    if any(l.get("name") == label for l in view.get("labels") or []):
        return f"the {label} label was added while the job ran"
    if view.get("mergeable") != "MERGEABLE":
        return (
            f"GitHub will not merge it right now ({view.get('mergeable') or 'unknown'})"
        )
    return ""


def pr_skip_reason(
    pr: dict, owner: str, base: str, label: str, state: dict | None = None
) -> str | None:
    """Why the team will not work on this PR, or None if it may."""
    if (pr.get("author") or {}).get("login") != owner:
        return "opened by someone else"
    if pr.get("isCrossRepository"):
        return "comes from a fork"
    if pr.get("isDraft"):
        return "marked as draft"
    if pr.get("baseRefName") != base:
        return f"does not target {base}"
    if pr.get("busy"):
        return f"still being worked on ({pr['busy']})"
    if any(l.get("name") == label for l in pr.get("labels") or []):
        # The label means "stopped, waiting for you". Removing it lifts the stop.
        # So does a new commit, but only when the team put the label there: a
        # label Nicola added by hand keeps the team off until it is removed.
        last = ((state or {}).get("attempts") or {}).get(f"pr-{pr.get('number')}")
        moved = last and last.get("parked") and last.get("sha") != pr.get("headRefOid")
        if not moved:
            return f"waiting for you (label {label})"
    return None


def _last(state: dict, key: str, sha: str) -> dict:
    last = (state.get("attempts") or {}).get(key) or {}
    return last if last and last.get("sha") == sha else {}


def stuck(state: dict, key: str, sha: str) -> str | None:
    """The last attempt stopped (`needs_user` / `blocked`) on this same commit:
    nothing changed, so another night would only repeat it."""
    if _last(state, key, sha).get("result") in ("needs_user", "blocked"):
        return "no change since the last attempt"
    return None


def error_cap(state: dict, key: str, sha: str, max_errors: int = 3) -> str | None:
    """`error` (timeout, usage limit, CI hiccup) is retried, but not forever."""
    last = _last(state, key, sha)
    if last.get("result") == "error" and last.get("tries", 0) >= max_errors:
        return f"failed {max_errors} nights in a row"
    return None


def record_attempt(
    state: dict,
    key: str,
    sha: str,
    result: str,
    parked: bool = False,
    prev_sha: str = "",
) -> None:
    """`parked` = the team itself put the `needs-you` label on this PR.
    `prev_sha` = the commit the job started from. The team's own push (main
    merged in, a fix) moves a PR from `prev_sha` to `sha`; that is the same run
    of attempts, so the count carries on. Only a commit from someone else
    starts it again."""
    attempts = state.setdefault("attempts", {})
    last = attempts.get(key) or {}
    same_run = bool(last) and last.get("sha") in {sha, prev_sha}
    tries = last.get("tries", 0) + 1 if same_run else 1
    attempts[key] = {"sha": sha, "result": result, "tries": tries, "parked": parked}


def clear_attempt(state: dict, key: str) -> None:
    (state.get("attempts") or {}).pop(key, None)


def retry_now(state: dict, key: str) -> None:
    """A job asked for by name: forget its last stop. If the team itself put the
    `needs-you` label on the PR, keep only that fact, so the team's own label
    does not hold it back (one Nicola added by hand still does)."""
    attempts = state.get("attempts") or {}
    last = attempts.pop(key, None) or {}
    if last.get("parked"):
        attempts[key] = {"sha": "", "result": "", "tries": 0, "parked": True}


def night_due(
    now: dt.datetime, start: tuple[int, int], catch_up_min: int, last_night: str
) -> str:
    """The night an hourly wake-up should start, as its date, or "" when it is
    not time. The shift opens at `start` (hour, minute); a wake-up up to
    `catch_up_min` minutes late still starts it (the Mac was busy or asleep);
    each night starts once."""
    opened = now.replace(hour=start[0], minute=start[1], second=0, microsecond=0)
    if opened > now:
        opened -= dt.timedelta(days=1)
    if now - opened >= dt.timedelta(minutes=catch_up_min):
        return ""
    night = opened.date().isoformat()
    return "" if night == last_night else night


# Reasons that carry raw error output. A PR comment names them and leaves the
# output to the night's page.
PRIVATE_REASONS = (
    "runner error",
    "cleanup error",
    "the builder session failed",
    "the review session failed",
    "could not keep the work",
    "push failed",
    "could not open the PR",
)


def public_reasons(reasons: list[str], home: str = "") -> list[str]:
    """The reasons as they go into a pull request comment, which anyone can
    read: no raw error output, no folders or branches of this Mac."""
    out = []
    for reason in reasons:
        kind = reason.split(":", 1)[0]
        if kind in PRIVATE_REASONS:
            reason = f"{kind} (the details are on the night team's page)"
        reason = re.sub(r"(kept on this Mac) as branch \S+", r"\1", reason)
        out.append(reason.replace(home, "~") if home else reason)
    return out


def parse_ideas(text: str) -> list[dict]:
    """IDEAS.md -> [{text, state}] in file order. `- idea` and `- [ ] idea` are
    open, `- [x]` built (merged, or now a pull request), `- [?]` waiting for an answer."""
    ideas = []
    for line in text.splitlines():
        m = IDEA_LINE.match(line)
        if not m:
            continue
        mark = (m.group(2) or " ").lower()
        state = {" ": "open", "x": "done", "?": "waiting"}[mark]
        ideas.append({"text": m.group(3).strip(), "state": state})
    return ideas


def mark_idea(text: str, idea_text: str, mark: str, note: str) -> str:
    """Rewrite the first OPEN line holding `idea_text` as `- [mark] idea — note`."""
    lines = text.splitlines()
    for i, line in enumerate(lines):
        m = IDEA_LINE.match(line)
        if m and (m.group(2) or " ") == " " and m.group(3).strip() == idea_text:
            lines[i] = f"{m.group(1)}[{mark}] {idea_text} — {note}"
            break
    return "\n".join(lines) + ("\n" if text.endswith("\n") else "")


def reopen_idea(text: str, idea_text: str) -> str:
    """Set the first WAITING line of `idea_text` (`- [?] idea — why`) back to open."""
    lines = text.splitlines()
    for i, line in enumerate(lines):
        m = IDEA_LINE.match(line)
        if not m or (m.group(2) or " ") != "?":
            continue
        body = m.group(3).strip()
        if body == idea_text or body.startswith(f"{idea_text} — "):
            lines[i] = f"{m.group(1)}[ ] {idea_text}"
            break
    return "\n".join(lines) + ("\n" if text.endswith("\n") else "")


def add_ideas(text: str, new: list[str]) -> tuple[str, int]:
    """Append each new idea as an open line, unless the list already has it
    (open, built or waiting). Returns the text and how many joined."""
    have = {i["text"].split(" — ")[0].casefold() for i in parse_ideas(text)}
    added = 0
    for idea in new:
        idea = " ".join(idea.split())
        if idea and idea.casefold() not in have:
            have.add(idea.casefold())
            text += (
                "" if not text or text.endswith("\n") else "\n"
            ) + f"- [ ] {idea}\n"
            added += 1
    return text, added


def slug(text: str, limit: int = 40) -> str:
    words = re.findall(r"[a-z0-9]+", text.lower())
    out = ""
    for w in words:
        if len(out) + len(w) + 1 > limit:
            break
        out = f"{out}-{w}" if out else w
    return out or "idea"


def idea_key(text: str) -> str:
    """A stable name for an idea: readable, and unique even when two ideas
    start with the same words."""
    return f"idea-{slug(text, 32)}-{hashlib.sha1(text.encode()).hexdigest()[:6]}"


def ci_state(rollup: list[dict] | None) -> str:
    """GitHub statusCheckRollup -> none | pending | pass | fail."""
    if not rollup:
        return "none"
    pending = False
    for c in rollup:
        if "state" in c and "status" not in c:  # a commit status
            s = c.get("state")
            if s in ("PENDING", "EXPECTED"):
                pending = True
            elif s != "SUCCESS":
                return "fail"
        elif c.get("status") != "COMPLETED":
            pending = True
        elif c.get("conclusion") not in CI_PASS:
            return "fail"
    return "pending" if pending else "pass"


def pr_job(pr: dict, label: str) -> dict:
    return {
        "kind": "pr",
        "key": f"pr-{pr['number']}",
        "pr": pr["number"],
        "title": pr.get("title", ""),
        "head": pr["headRefName"],
        "body": pr.get("body") or "",
        "sha": pr.get("headRefOid") or "",
        "labelled": any(l.get("name") == label for l in pr.get("labels") or []),
    }


def select_jobs(
    main_ok: bool,
    prs: list[dict],
    ideas: list[dict],
    state: dict,
    owner: str,
    base: str,
    label: str,
    main_sha: str = "",
) -> tuple[list[dict], list[dict]]:
    """Tonight's queue, in the agreed order: a broken main first, then open PRs
    (oldest first), then ideas from the top of the list. Returns (jobs, skipped)."""

    def pr_why(pr: dict) -> str | None:
        key, sha = f"pr-{pr['number']}", pr.get("headRefOid") or ""
        why = pr_skip_reason(pr, owner, base, label, state) or error_cap(
            state, key, sha
        )
        if why:
            return why
        # A stop the team could not label (the label call failed) would otherwise
        # be repeated every night. A stop it did label is lifted by removing the
        # label, so that one is tried again.
        if not _last(state, key, sha).get("parked"):
            return stuck(state, key, sha)
        return None

    def skip_pr(pr: dict, why: str) -> dict:
        return {
            "kind": "pr",
            "pr": pr["number"],
            "title": pr.get("title", ""),
            "why": why,
        }

    by_age = sorted(prs, key=lambda p: p.get("createdAt") or "")
    if not main_ok:
        # Nothing else can pass its checks while main is red. If an earlier
        # night already opened a fix, carry on with that one instead of a second.
        title = f"Fix failing checks on {base}"
        for pr in by_age:
            if (pr.get("headRefName") or "").startswith(MAIN_FIX_PREFIX) and (
                pr.get("author") or {}
            ).get("login") == owner:
                why = pr_why(pr)
                return ([], [skip_pr(pr, why)]) if why else ([pr_job(pr, label)], [])
        why = stuck(state, "main", main_sha) or error_cap(state, "main", main_sha)
        if why:
            return [], [{"kind": "main", "title": title, "why": why}]
        return [{"kind": "main", "key": "main", "title": title, "sha": main_sha}], []
    jobs, skipped = [], []
    for pr in by_age:
        why = pr_why(pr)
        if why:
            skipped.append(skip_pr(pr, why))
        else:
            jobs.append(pr_job(pr, label))
    for idea in ideas:
        if idea["state"] != "open":
            continue
        key = idea_key(idea["text"])
        why = error_cap(state, key, "")
        if why:
            skipped.append({"kind": "idea", "title": idea["text"], "why": why})
        else:
            jobs.append({"kind": "idea", "key": key, "title": idea["text"], "sha": ""})
    return jobs, skipped


# ---------------------------------------------------------------- chat --
# Telegram. Everything typed there is data: these functions decide what a text
# or a button press means, and nothing else in the team reads the words.

CHAT_HELP = (
    "I understand:\n"
    "idea: <what you want> — goes on the team's list\n"
    "status — what the team did last and what waits for you\n"
    "pause — nothing new starts and nothing merges\n"
    "resume — back to work\n"
    "To add a note to something I asked, reply to that message."
)
CHAT_COMMANDS = ("status", "pause", "resume", "help")
IDEA_MAX = 300
# What Nicola can press under each kind of proposal: (label, answer).
BUTTONS = {
    "idea": (("Build it", "yes"), ("Not now", "later"), ("No", "no")),
    "question": (("Try again", "yes"), ("Leave it", "no")),
    "trading": (("Apply it", "yes"), ("No", "no")),
}
CONFIRM = (("Yes, apply it", "confirm"), ("Cancel", "cancel"))
APPROVED = {
    "idea": "Build it. It joins the list and the team builds it at night.",
    "question": "Trying again on the next run.",
    "trading": "Applying it on the next run.",
}
DECLINED = {
    "idea": "No. I will not bring it up again.",
    "question": "Left as it is.",
    "trading": "No. Nothing changes.",
}
OPEN_STATES = ("sent", "confirming")  # texted, no final answer yet
IDEAS_PER_WEEK = 3
LATER_DAYS = 14  # "Not now" comes back after this
EXPIRE_DAYS = 7  # a text nobody answered


def parse_chat(text: str) -> tuple[str, str]:
    """What a text from the owner asks for, as (command, argument): `idea`,
    `status`, `pause`, `resume`, `help`, or `note` for anything else."""
    text = (text or "").strip()
    word, _, rest = text.partition(" ")
    head = word.lower().lstrip("/").split("@", 1)[0]
    if head.startswith("idea:") or head.rstrip(":") == "idea":
        idea = text.split(":", 1)[1] if ":" in word else rest
        idea = " ".join(idea.split())[:IDEA_MAX]
        return ("idea", idea) if idea else ("help", "")
    if head == "start":  # Telegram's own first message to a bot
        return ("help", "")
    if head in CHAT_COMMANDS and not rest.strip():
        return (head, "")
    return ("note", text) if text else ("help", "")


def parse_button(data: str) -> tuple[str, str] | None:
    """A button press as (proposal id, answer), or None for anything else."""
    m = re.fullmatch(r"p:([0-9a-f]{8}):(yes|later|no|confirm|cancel)", data or "")
    return (m.group(1), m.group(2)) if m else None


def from_owner(update: dict, chat_id: int, user_id: int) -> bool:
    """Only the account that paired, in the private chat it paired from."""
    event = update.get("message") or update.get("callback_query") or {}
    message = event if "message" in update else (event.get("message") or {})
    sender, chat = event.get("from") or {}, message.get("chat") or {}
    return bool(
        chat_id
        and user_id
        and chat.get("type") == "private"
        and chat.get("id") == chat_id
        and sender.get("id") == user_id
        and not sender.get("is_bot")
    )


def proposal_step(p: dict, answer: str) -> tuple[str, str]:
    """A button press on a proposal: (its new state, what to tell Nicola). The
    first answer stands; a later press only hears what was recorded. A trading
    change is asked twice."""
    state, kind = p.get("state"), p.get("kind")
    if state == "sent":
        if answer == "yes":
            if kind == "trading":
                return "confirming", "This changes how Pulse trades. Sure?"
            return "approved", APPROVED.get(kind, "Yes.")
        if answer == "later" and kind == "idea":
            return "later", f"Not now. I will bring it up again in {LATER_DAYS} days."
        if answer == "no":
            return "declined", DECLINED.get(kind, "No.")
    elif state == "confirming":
        if answer == "confirm":
            return "approved", APPROVED["trading"]
        if answer == "cancel":
            return "sent", "Cancelled. Nothing changes."
    if state in OPEN_STATES:
        return state, "That button does not fit this message."
    return state, f"Already recorded: {state}."


def send_at(
    now: dt.datetime, serious: bool, first: int = 8, last: int = 22
) -> dt.datetime:
    """When a text may go out: at once for a serious problem or by day,
    otherwise the next morning."""
    if serious or first <= now.hour < last:
        return now
    morning = now.replace(hour=first, minute=0, second=0, microsecond=0)
    return morning if now.hour < first else morning + dt.timedelta(days=1)


def _stamp(when: dt.datetime) -> str:
    return when.isoformat(timespec="seconds")


def due_proposals(
    proposals: list[dict], now: dt.datetime, cap: int = IDEAS_PER_WEEK
) -> list[dict]:
    """The queued proposals to text now, oldest first. Questions and trading
    proposals always go; new-feature ideas stop at `cap` in a calendar week."""
    week = now.isocalendar()[:2]
    sent = sum(
        1
        for p in proposals
        if p.get("kind") == "idea"
        and p.get("sent_at")
        and dt.datetime.fromisoformat(p["sent_at"]).isocalendar()[:2] == week
    )
    due = []
    for p in proposals:
        if p.get("state") != "queued" or (p.get("not_before") or "") > _stamp(now):
            continue
        if p.get("kind") == "idea":
            if sent >= cap:
                continue
            sent += 1
        due.append(p)
    return due


def settle(p: dict, state: str, now: dt.datetime) -> None:
    """Record the state a button press led to. "Not now" goes back in the
    queue for later."""
    p["state"], p["answered_at"] = state, _stamp(now)
    if state == "later":
        p["state"] = "queued"
        p["not_before"] = _stamp(now + dt.timedelta(days=LATER_DAYS))
        p["message_id"] = None


def expire(proposals: list[dict], now: dt.datetime) -> int:
    """A text nobody answered in a week is closed. Returns how many."""
    limit, closed = _stamp(now - dt.timedelta(days=EXPIRE_DAYS)), 0
    for p in proposals:
        if p.get("state") in OPEN_STATES and (p.get("sent_at") or limit) < limit:
            p["state"], closed = "expired", closed + 1
    return closed


def proposal_id(kind: str, key: str, created: str) -> str:
    return hashlib.sha1(f"{kind}|{key}|{created}".encode()).hexdigest()[:8]
