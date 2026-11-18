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
INCIDENT_PREFIX = "fix/night-incident-"
TEAM_PREFIXES = (MAIN_FIX_PREFIX, INCIDENT_PREFIX, "feat/night-")

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


# What decides how Pulse buys and sells. A change here that the team thought
# of by itself (a fix for an incident) is Nicola's to merge. The list errs on
# the wide side: the fetcher's entry point and its pipeline hold the switches
# for trading (which mode runs, dry runs, the stop for a new position) next to
# code that has nothing to do with it.
TRADING_GLOBS = [
    "crates/pulse-fetcher/src/main.rs",
    "crates/pulse-fetcher/src/pipeline/mod.rs",
    "crates/pulse-fetcher/src/pipeline/trading.rs",
    "crates/pulse-fetcher/src/pipeline/signals.rs",
    "crates/pulse-fetcher/src/pipeline/live.rs",
    "crates/pulse-fetcher/src/pipeline/form4.rs",  # which insider buys become trades
    "crates/pulse-fetcher/src/sources/edgar.rs",
    "crates/pulse-fetcher/src/decisions.rs",  # runs inside every buy
    "crates/pulse-fetcher/src/candidates.rs",
    "crates/pulse-fetcher/src/market_prices.rs",
    "crates/pulse-fetcher/src/entry_filters.rs",
    "crates/pulse-fetcher/src/position_management.rs",
    "crates/pulse-fetcher/src/position_sizing.rs",
    "crates/pulse-fetcher/src/event_signals.rs",
    "crates/pulse-fetcher/src/learning.rs",
    "crates/pulse-fetcher/src/calibration.rs",
    "crates/pulse-weights/*",
    "crates/pulse-alpaca/*",
    "crates/pulse-snapshot/src/ledger.rs",
    "crates/pulse-snapshot/src/risk.rs",
    "src-tauri/src/services/paper_trading.rs",
    "src-tauri/src/services/cross_signals.rs",
    "src-tauri/src/services/signals.rs",
    "src-tauri/src/services/risk.rs",
    "src-tauri/src/commands/trading.rs",
]


def trading_violations(changes: list[tuple[str, str]]) -> list[str]:
    """Reasons a change the team started by itself needs Nicola: it touches
    how Pulse trades."""
    return [
        f"changes how Pulse trades: {path}"
        for _status, path in changes
        if any(fnmatch.fnmatch(path, g) for g in TRADING_GLOBS)
    ]


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


def reopen_idea(text: str, key: str) -> str:
    """Set the WAITING line (`- [?] idea — why`) of the idea named `key`
    (`idea_key`) back to open. The idea's own words may hold a dash too, so
    every place the reason could start is tried, longest idea first."""
    lines = text.splitlines()
    for i, line in enumerate(lines):
        m = IDEA_LINE.match(line)
        if not m or (m.group(2) or " ") != "?":
            continue
        parts = m.group(3).strip().split(" — ")
        for n in range(len(parts), 0, -1):
            idea = " — ".join(parts[:n])
            if idea_key(idea) == key:
                lines[i] = f"{m.group(1)}[ ] {idea}"
                return "\n".join(lines) + ("\n" if text.endswith("\n") else "")
    return text


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
    incidents: list[dict] | None = None,
    shift: str = "night",
) -> tuple[list[dict], list[dict]]:
    """This shift's queue, in the agreed order: incidents first, then a broken
    main, then open PRs (oldest first), then ideas from the top of the list.
    A shift started for an incident does that and nothing else. By day only
    the light work is taken: the ideas that are small fixes (`small_fix`), and
    none while main is not known to pass. Returns (jobs, skipped)."""

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
    jobs, skipped = list(incidents or []), []
    if shift == "incident" or (shift == "day" and not main_ok):
        return jobs, skipped
    if not main_ok:
        # Nothing else can pass its checks while main is red. If an earlier
        # night already opened a fix, carry on with that one instead of a second.
        title = f"Fix failing checks on {base}"
        for pr in by_age:
            if (pr.get("headRefName") or "").startswith(MAIN_FIX_PREFIX) and (
                pr.get("author") or {}
            ).get("login") == owner:
                why = pr_why(pr)
                if why:
                    return jobs, [skip_pr(pr, why)]
                return [*jobs, pr_job(pr, label)], []
        why = stuck(state, "main", main_sha) or error_cap(state, "main", main_sha)
        if why:
            return jobs, [{"kind": "main", "title": title, "why": why}]
        main = {"kind": "main", "key": "main", "title": title, "sha": main_sha}
        return [*jobs, main], []
    for pr in by_age:
        if shift == "day":
            break  # checking and reviewing pull requests is night work
        why = pr_why(pr)
        if why:
            skipped.append(skip_pr(pr, why))
        else:
            jobs.append(pr_job(pr, label))
    for idea in ideas:
        if idea["state"] != "open":
            continue
        if shift == "day" and not small_fix(idea["text"]):
            continue  # building a feature is night work
        key = idea_key(idea["text"])
        why = error_cap(state, key, "")
        if why:
            skipped.append({"kind": "idea", "title": idea["text"], "why": why})
        else:
            jobs.append({"kind": "idea", "key": key, "title": idea["text"], "sha": ""})
    return jobs, skipped


# -------------------------------------------------------------- shifts --
# The Mac wakes the team every hour. These rules say what that hour is for,
# and when work that is running has to step aside.

DAY_HOURS = (9, 21)  # light work by day starts from 09:00 and before 21:00
ACTIVE_MIN = 10  # a session log of Nicola's own changed this recently: he is working
INCIDENTS_PER_DAY = 2
INCIDENT_MAX_AGE_H = 24  # an older trigger is about a problem already texted again
HELD_DAYS = 7  # how long a job that stepped aside may go on from where it stopped
ACT_AFTER_DAYS = 7  # the watch only texts for its first week, then it also acts
SMALL_FIX = re.compile(r"fix:\s*\S", re.I)


def small_fix(idea: str) -> bool:
    """An idea line that starts with `fix:` is a small fix: light enough for
    the day."""
    return bool(SMALL_FIX.match(idea.strip()))


def project_folder(path: str) -> str:
    """The name of the folder the Claude CLI keeps a working folder's session
    logs in: the path with everything but letters and digits turned to `-`."""
    return re.sub(r"[^A-Za-z0-9]", "-", path)


def owner_active(
    logs: list[tuple[str, float]],
    now: float,
    home: str,
    own: tuple[str, ...],
    minutes: int = ACTIVE_MIN,
) -> bool:
    """Is Nicola using Claude on this Mac right now? `logs` is every project
    folder with the time its newest session log changed. His own sessions run
    in folders under his home folder (`home`); the team's (`own`) do not count,
    and neither do the ones other programs start in temporary folders."""
    mine = project_folder(home)
    theirs = tuple(project_folder(path) for path in own)
    return any(
        (name == mine or name.startswith(mine + "-"))
        and not name.startswith(theirs)
        and 0 <= now - changed < minutes * 60
        for name, changed in logs
    )


def day_used(state: dict, today: dt.date) -> int:
    """Minutes of day work already done today."""
    day = state.get("day")
    if not isinstance(day, dict) or day.get("date") != today.isoformat():
        return 0
    minutes = day.get("minutes")
    return minutes if isinstance(minutes, int) and minutes > 0 else 0


def add_day_minutes(state: dict, today: dt.date, minutes: int) -> None:
    state["day"] = {
        "date": today.isoformat(),
        "minutes": day_used(state, today) + max(0, minutes),
    }


def shift_due(
    now: dt.datetime,
    night: str,
    state: dict,
    *,
    paused: bool,
    incident: bool,
    active: bool,
    day_budget: int,
    hours: tuple[int, int] = DAY_HOURS,
) -> str:
    """What an hourly wake-up is for: "night" (`night` is the date `night_due`
    gave), "incident" (a serious problem waits, at any hour), "day" (light
    work, inside the day's hours and its time budget, and only while Nicola is
    not using Claude on this Mac), or "" for nothing. Paused, it is nothing."""
    if paused:
        return ""
    if night:
        return "night"  # it takes the incidents first
    if incident:
        return "incident"
    if active or not hours[0] <= now.hour < hours[1]:
        return ""
    return "day" if day_used(state, now.date()) < day_budget else ""


def yield_reason(
    kind: str,
    shift: str,
    *,
    paused: bool,
    incident: bool,
    active: bool,
    over_budget: bool,
) -> str:
    """Why the job that is running has to stop now and keep its work, or ''.
    `pause` stops everything. A serious problem comes before any other job.
    By day the team also steps aside for Nicola and stays inside its budget;
    work on an incident does neither. Kind `check` is the night's own check
    of main, which only `pause` stops."""
    if paused:
        return "paused from Telegram"
    if kind in ("incident", "check"):
        return ""
    if incident:
        return "a serious problem came first"
    if shift == "day" and active:
        return "Nicola started using Claude on this Mac"
    if shift == "day" and over_budget:
        return "the day's time for the team is used up"
    return ""


def waiting(state: dict, key: str, now: dt.datetime) -> dict | None:
    """The note left for a job that stepped aside and has not gone on yet, or
    None. A note older than HELD_DAYS is no longer one: main has moved on, and
    starting over is better than going on."""
    held = state.get("held")
    item = held.get(key) if isinstance(held, dict) else None
    if not isinstance(item, dict):
        return None
    try:
        age = now - dt.datetime.fromisoformat(str(item.get("at")))
    except (TypeError, ValueError):
        return None
    return item if dt.timedelta(0) <= age <= dt.timedelta(days=HELD_DAYS) else None


def hold(state: dict, key: str, branch: str, now: dt.datetime) -> None:
    """A job stepped aside. `branch` holds its unfinished work on this Mac
    ('' when it had none yet); its next turn goes on from there. Notes too old
    to count are thrown away here, so the book never grows."""
    if not isinstance(state.get("held"), dict):
        state["held"] = {}
    held = state["held"]
    for old in [k for k in held if waiting(state, k, now) is None]:
        del held[old]
    held[key] = {"branch": branch, "at": now.isoformat(timespec="seconds")}


def release(state: dict, key: str) -> None:
    held = state.get("held")
    if isinstance(held, dict):
        held.pop(key, None)


def held_branch(state: dict, key: str, now: dt.datetime) -> str:
    """The branch a job that stepped aside goes on from, or ''."""
    item = waiting(state, key, now) or {}
    return item["branch"] if isinstance(item.get("branch"), str) else ""


def incidents_today(state: dict, today: dt.date) -> list[str]:
    """The problems an incident job was started for today."""
    book = state.get("incidents")
    if not isinstance(book, dict) or book.get("date") != today.isoformat():
        return []
    return [k for k in book.get("keys") or [] if isinstance(k, str)]


def record_incident(state: dict, problem: str, today: dt.date) -> None:
    state["incidents"] = {
        "date": today.isoformat(),
        "keys": [*incidents_today(state, today), problem],
    }


def incident_jobs(
    triggers: list[dict], state: dict, now: dt.datetime, per_day: int = INCIDENTS_PER_DAY
) -> tuple[list[dict], list[dict]]:
    """The incident jobs to do for the triggers in the queue, oldest first, and
    the triggers to throw away with the reason: (jobs, dropped). A trigger is
    {file, key, text, evidence, at}, written by the watch or by hand. One job
    per problem per day and `per_day` jobs a day at most; a trigger that is
    malformed or older than a day is dropped. A job that was started and had
    to stop midway (`hold`) is not a second one: it goes on."""
    jobs, dropped = [], []
    done = incidents_today(state, now.date())
    new = 0
    for t in sorted(triggers, key=lambda t: str(t.get("at") or "")):
        problem, text = t.get("key"), t.get("text")
        try:
            at = dt.datetime.fromisoformat(str(t.get("at")))
            if at.tzinfo is not None:  # the runner's clock has no zone
                raise ValueError
        except ValueError:
            at = None
        readable = isinstance(problem, str) and problem and isinstance(text, str) and text
        if not readable or at is None:
            dropped.append(_dropped(t, "it could not be read"))
            continue
        key = f"incident-{slug(problem, 30)}-{at:%m%d%H%M}"
        going = waiting(state, key, now) is not None
        if now - at > dt.timedelta(hours=INCIDENT_MAX_AGE_H):
            why = "it is more than a day old"
        elif any(j["problem"] == problem for j in jobs):
            why = "this problem is in the queue already"
        elif going:
            why = ""
        elif problem in done:
            why = "this problem was already worked on today"
        elif len(done) + new >= per_day:
            why = f"{per_day} incidents a day is the limit"
        else:
            why, new = "", new + 1
        if why:
            dropped.append(_dropped(t, why, key))
            continue
        jobs.append(
            {
                "kind": "incident",
                "key": key,
                "problem": problem,
                "title": " ".join(text.split())[:300],
                "evidence": str(t.get("evidence") or "")[:EVIDENCE_MAX],
                "file": str(t.get("file") or ""),
                "sha": "",
            }
        )
    return jobs, dropped


def incident_wanted(new: dict, queued: list[dict], state: dict, now: dt.datetime) -> bool:
    """Would the runner take this trigger, beside what already waits in the
    queue? The watch asks before it queues a problem and says the team is on
    it, so that it never promises a look that the day's limits rule out."""
    mark = "\0the new one"
    jobs, _ = incident_jobs([*queued, {**new, "file": mark}], state, now)
    return any(j["file"] == mark for j in jobs)


def _dropped(trigger: dict, why: str, key: str = "") -> dict:
    return {
        "kind": "incident",
        "key": key,  # of the job it would have been ('' when it cannot be read)
        "file": str(trigger.get("file") or ""),
        "title": " ".join(str(trigger.get("text") or "a trigger").split())[:120],
        "why": why,
    }


# What of the watch's findings the team also works on: the serious ones whose
# cause can be in Pulse's own code. A position without a stop that the next run
# did not repair, trading runs that keep failing, trading that went silent, a
# briefing that keeps failing.
INCIDENT_KEYS = ("stop:", "manage", "auto", "silent", "briefing:")
EVIDENCE_MAX = 6000
LOG_LINE_MAX = 300
ANSI_CODES = re.compile(r"\x1b\[[0-9;]*[A-Za-z]")
# A value that follows a name like these, in an address, a header or a setting:
# `name=value`, `name: value`, `name => value`, `name%3Dvalue`, with the value
# in quotes (all of it, spaces too) or in Rust's `Some(…)`.
# - Names that only ever mean a secret count wherever they stand in a word
#   (`db_password`, `SECRETKEY`, `PHPSESSID`), and so does `token` at a word's
#   end (`mytoken`, `accessToken`).
# - The short ones have to stand as a word of their own (`sig`, `apiKey`,
#   `X-Auth`), so that `signal=0.3`, `tokens: 1532`, `author: x` or `monkey=3`
#   keep their values.
# - A name that ends in what is counted or pointed at (`key_count`,
#   `token_type`, `secret_name`) is no secret either.
_SEP = r"\s*(?:=>|%3[Dd]|[=:])\s*"
_SURE = (
    r"secret(?!ar)|password|passwd|passphrase|credential|private[-_]?key"
    r"|access[-_]?key|api[-_]?key|sess(?:ion)?[-_]?id"
)
_OWN_WORD = r"key|pwd|signature|sig|auth|authorization|bearer|session|sid"
_COUNTED = r"count|len|length|size|total|limit|type|name|path|file|dir"
SECRET_VALUE = re.compile(
    r"(?i)((?:" + _SURE + r"|token(?![a-z])"
    r"|(?:(?<![A-Za-z])|(?-i:(?<=[a-z])(?=[A-Z])))(?:" + _OWN_WORD + r")(?![a-z]))"
    r"(?![\w-]{0,40}(?:" + _COUNTED + r")s?[\"']?" + _SEP + r")"
    r"[\w-]{0,40}[\"']?" + _SEP + r"(?:Some\(\s*)?(?:(?:bearer|basic|token)\s+)?)"
    r"(?:\"[^\"]*\"?|'[^']*'?|[^\s&\"',;]+)"
)
# The same with only a space before the value, as a header is sometimes
# printed. Words stay ("authorization failed", "Basic authentication"): the
# value has to look like a key, with a digit in it or 16 characters long.
SECRET_WORD = re.compile(
    r"(?i)\b((?:(?:proxy-)?authorization[\"']?\s+)?(?:bearer|basic)\s+"
    r"|(?:x-api-key|authorization|password|passwd|passphrase|secret|token|api[-_]?key)"
    r"[\"']?\s+)"
    r"(?:(?=[^\s\"',;]*\d)[^\s\"',;]{6,}|[^\s\"',;]{16,})"
)
# A signed web token: three runs with dots between, the first always `eyJ…`.
WEB_TOKEN = re.compile(r"\beyJ[A-Za-z0-9_\-]{8,}\.[A-Za-z0-9_\-]{4,}(?:\.[A-Za-z0-9_\-]*)?")
COOKIES = re.compile(r"(?i)((?:set-)?cookie[\"']?\s*[=:]\s*).*")
# Keys that say what they are by how they start, with the - and _ inside them.
KNOWN_KEY = re.compile(
    r"(?<![A-Za-z0-9])(?:sk|pk|rk|pa|gsk|ghp|gho|ghs|github_pat|xox[abprs])[-_][A-Za-z0-9_\-]{10,}"
)
# A number, a colon and a long tail: how a bot's token is written into an address.
ID_AND_KEY = re.compile(r"(?<![A-Za-z0-9])[A-Za-z]{0,6}\d{6,}:[A-Za-z0-9_\-]{20,}")
# A run of letters and digits long enough to be a key, whatever it is called:
# 20 or more with both letters and digits in it, or 32 or more of anything.
# Words joined by - or _ (an order's id, a function's name, a UUID) are not one
# run, so what a cause is found with stays readable.
LONG_TOKEN = re.compile(
    r"(?<![A-Za-z0-9])(?:(?=[A-Za-z0-9]*[0-9])(?=[A-Za-z0-9]*[A-Za-z])[A-Za-z0-9]{20,}"
    r"|[A-Za-z0-9]{32,})(?![A-Za-z0-9])"
)
USERINFO = re.compile(r"(://)[^/\s:@]+:[^/\s@]+@")


def incident_worthy(finding: dict) -> bool:
    key = str(finding.get("key") or "")
    return (
        bool(finding.get("serious"))
        and key != "stop:dry"
        and any(key == k or (k.endswith(":") and key.startswith(k)) for k in INCIDENT_KEYS)
    )


def watch_acts(memory: dict, now: dt.datetime, off: bool) -> bool:
    """May the watch start an incident job, or does it only text? Only texts
    while switched off, and for the first week after its first look (kept in
    `memory`), so its alarms can be seen to be true ones first."""
    since = memory.setdefault("since", _stamp(now))
    if off:
        return False
    try:
        return now - dt.datetime.fromisoformat(str(since)) >= dt.timedelta(days=ACT_AFTER_DAYS)
    except ValueError:
        memory["since"] = _stamp(now)
        return False


def redact_log(text: str, home: str = "") -> str:
    """The end of a log, made fit to show a session: no colour codes, the home
    folder as `~`, and nothing that looks like a key or a password. A session
    is never handed a secret, and a failed request can print the address it
    called with the key in it. It is a net, not a proof: a short secret after
    a bare word ("token abc") or under a name of its own passes."""
    lines = []
    for line in ANSI_CODES.sub("", text).splitlines():
        line = "".join(c for c in line if c == "\t" or c.isprintable())
        if home:
            line = line.replace(home, "~")
        line = USERINFO.sub(r"\1[removed]@", line)
        line = COOKIES.sub(r"\1[removed]", line)
        line = SECRET_VALUE.sub(r"\1[removed]", line)
        line = SECRET_WORD.sub(r"\1[removed]", line)
        line = WEB_TOKEN.sub("[removed]", line)
        line = KNOWN_KEY.sub("[removed]", line)
        line = ID_AND_KEY.sub("[removed]", line)
        line = LONG_TOKEN.sub("[removed]", line)
        lines.append(line[:LOG_LINE_MAX])
    out = "\n".join(lines).strip()
    return out[-EVIDENCE_MAX:]


def trigger(finding: dict, evidence: str, now: dt.datetime) -> dict:
    """What the watch leaves in the queue for the runner."""
    return {
        "key": str(finding["key"]),
        "text": str(finding["text"]),
        "evidence": evidence[-EVIDENCE_MAX:],
        "at": now.isoformat(timespec="seconds"),
    }


# ---------------------------------------------------------------- chat --
# Telegram. Everything typed there is data: these functions decide what a text
# or a button press means, and nothing else in the team reads the words.

CHAT_HELP = (
    "Text me in plain words what you want: a question, or a job like "
    "\"merge the open pull requests and release\", \"stop buying\" or "
    "\"build me a chart of my risk\". Before a merge, a release or a trading "
    "setting changes, I show you the plan with a button.\n"
    "Fixed words that always work:\n"
    "idea: <what you want> — goes on the team's list for the night\n"
    "status — what the team did last and what waits for you\n"
    "pause — everything stops, also the job that is running\n"
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
    "action": (("Do it", "yes"), ("Cancel", "no")),
}
CONFIRM = (("Yes, apply it", "confirm"), ("Cancel", "cancel"))
APPROVED = {
    "idea": "Build it. It joins the list and the team builds it at night.",
    "question": "Trying again on the next run.",
    "trading": "Applying it on the next run.",
    "action": "Doing it now.",
}
DECLINED = {
    "idea": "No. I will not bring it up again.",
    "question": "Left as it is.",
    "trading": "No. Nothing changes.",
    "action": "Cancelled. Nothing was done.",
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


# ---- plain words -----------------------------------------------------------
# A text that is none of the fixed words goes to a session that can only read.
# It answers, and may ask for actions from the list below. The words it sends
# back are data too: `talk_plan` decides what each one comes to. What changes
# main, the installed app or how Pulse trades is only ever a plan: nothing of
# it happens before Nicola pressed the button under it.

TALK_ACTIONS = ("merge", "task", "idea", "pause", "resume", "release", "setting")
TALK_MAX_ACTIONS = 20
PLAN_MAX = 10  # steps in one plan: all of them must fit in the text he sees
PLAN_TITLE = 90
PLAN_FRESH_MIN = 60  # a yes older than this is not carried out any more
PLAN_OPEN_MIN = 120  # a plan shown longer ago than this is too old to say yes to
SWITCH_DEFAULT_ON = frozenset(
    {"SHORTS_ENABLED", "EVENT_TRADES_ENABLED", "LEARNING_AUTO_APPLY", "EXIT_DRY_RUN"}
)
TALK_HISTORY = 8  # earlier turns a session is shown
TALK_TEXT_MAX = 1500  # of one text from Nicola
# The switches a plan may flip in the installed app's settings, and what each
# one does when it is on. All of them are about the paper account.
# SWITCH_DEFAULT_ON: what the fetcher does when a switch is not set (it reads
# these as "on unless false or 0"; the others as "off unless true or 1").
TRADING_SWITCHES = {
    "AUTO_TRADE_ENABLED": "Pulse buys by itself",
    "SHORTS_ENABLED": "Pulse may sell short",
    "NEWS_TRADES_ENABLED": "news surprises are traded",
    "EVENT_TRADES_ENABLED": "event signals are traded",
    "LEARNING_AUTO_APPLY": "what Pulse learned changes its signal weights by itself",
    "EXIT_DRY_RUN": "exits are only logged, nothing is sold",
}


def pr_card(pr: dict, owner: str, base: str, look: dict | None) -> dict:
    """An open pull request as the talk session and `talk_plan` see it.
    `look` is what its change holds, read from git: {"changes": [(status,
    path), ...] (old names of renamed files too), "never": [reasons it must
    not merge from a text], "scripts": bool}; None when it could not be read."""
    look = look if isinstance(look, dict) else None
    changes = list(look.get("changes") or []) if look else []
    return {
        "number": pr.get("number"),
        "title": " ".join(str(pr.get("title") or "").split())[:160],
        "sha": pr.get("headRefOid") or "",
        "checks": ci_state(pr.get("statusCheckRollup")),
        "mergeable": pr.get("mergeable") or "UNKNOWN",
        "draft": bool(pr.get("isDraft")),
        "base": pr.get("baseRefName") or "",
        "own": (pr.get("author") or {}).get("login") == owner
        and not pr.get("isCrossRepository"),
        "to_base": pr.get("baseRefName") == base,
        "files_known": look is not None,
        "never": [str(r) for r in (look.get("never") or [])] if look else [],
        "trading": bool(trading_violations(changes)),
        "team_code": bool(protected_violations(changes)) or bool(look and look.get("scripts")),
        "labels": sorted(
            str(l.get("name") or "") for l in pr.get("labels") or [] if l.get("name")
        ),
    }


def merge_refusal(card: dict | None) -> str:
    """Why a pull request is not merged from a text, or ''."""
    if not card:
        return "it is not an open pull request"
    if not card.get("own"):
        return "someone else opened it"
    if card.get("draft"):
        return "it is a draft"
    if not card.get("to_base"):
        return f"it does not target main (it targets {card.get('base') or 'nothing'})"
    if not card.get("files_known"):
        return "what it changes could not be read"
    if card.get("never"):
        return f"it {card['never'][0]}, and that is never merged from a text"
    if card.get("checks") != "pass":
        return f"its checks have not passed ({card.get('checks')})"
    if card.get("mergeable") != "MERGEABLE":
        return f"GitHub cannot merge it right now ({card.get('mergeable')})"
    return ""


def merge_line(card: dict, label: str) -> str:
    """One pull request in a plan, with what Nicola should know before the tap."""
    marks = []
    if card.get("trading"):
        marks.append("CHANGES HOW PULSE TRADES")
    if card.get("team_code"):
        marks.append("changes the team's own code or settings")
    if label in (card.get("labels") or []):
        marks.append("the team had stopped on it for you")
    title = card["title"][:PLAN_TITLE]
    return f"Merge #{card['number']} {title}" + (f" ({'; '.join(marks)})" if marks else "")


def switch_value(value) -> str:
    """'true' / 'false' for what a session sent as a switch's value, else ''."""
    if isinstance(value, bool):
        return "true" if value else "false"
    word = str(value or "").strip().lower()
    return {"true": "true", "on": "true", "false": "false", "off": "false"}.get(word, "")


def talk_plan(
    actions, cards: list[dict], paused: bool, switches: dict | None = None, label: str = ""
) -> dict:
    """What a talk session's actions come to: `confirm` (the steps of one plan,
    carried out only after a tap: merge, release, setting; each with the `line`
    he is shown; so is resume), `now` (done at once: task, idea, pause) and `refused`
    (what is not done, with the reason). Anything not on the list is dropped."""
    by_number = {c.get("number"): c for c in cards}
    plan = {"confirm": [], "now": [], "refused": []}
    seen = set()

    def confirm(step: dict, name: str) -> None:
        if len(plan["confirm"]) >= PLAN_MAX:
            plan["refused"].append(f"{name}: more than {PLAN_MAX} steps in one plan, ask again for the rest")
        else:
            plan["confirm"].append(step)

    for a in (actions if isinstance(actions, list) else [])[:TALK_MAX_ACTIONS]:
        if not isinstance(a, dict) or a.get("kind") not in TALK_ACTIONS:
            continue
        kind = a["kind"]
        if kind == "merge":
            number = a.get("pr")
            if not isinstance(number, int) or isinstance(number, bool):
                continue
            if ("merge", number) in seen:
                continue
            seen.add(("merge", number))
            card = by_number.get(number)
            why = "the team is paused" if paused else merge_refusal(card)
            if why:
                plan["refused"].append(f"#{number}: {why}")
            else:
                confirm(
                    {"kind": "merge", "pr": number, "sha": card["sha"], "line": merge_line(card, label)},
                    f"#{number}",
                )
        elif kind == "setting":
            name, value = str(a.get("name") or "").strip().upper(), switch_value(a.get("value"))
            if name not in TRADING_SWITCHES or not value or ("setting", name) in seen:
                if name and name not in TRADING_SWITCHES:
                    plan["refused"].append(f"{name[:40]}: not a switch I may change")
                continue
            seen.add(("setting", name))
            if switches is None:
                plan["refused"].append(f"{name}: the installed app's settings could not be read")
                continue
            was = switches.get(name) or switch_state(name, None)
            if was == value:
                plan["refused"].append(f"{name} is already {value}")
                continue
            confirm(
                {
                    "kind": "setting",
                    "name": name,
                    "value": value,
                    "line": f"Set {name} to {value} (now {was}; true means: {TRADING_SWITCHES[name]})",
                },
                name,
            )
        elif kind == "release":
            if ("release",) in seen:
                continue
            seen.add(("release",))
            if paused:
                plan["refused"].append("a release: the team is paused")
            else:
                confirm(
                    {
                        "kind": "release",
                        "line": "Release a new version of Pulse from main and install it on this Mac",
                    },
                    "the release",
                )
        elif kind in ("task", "idea"):
            words = " ".join(str(a.get("text") or "").split())[:IDEA_MAX]
            if not words or (kind, words.casefold()) in seen:
                continue
            seen.add((kind, words.casefold()))
            if kind == "task" and paused:
                plan["refused"].append(f"\"{words[:60]}\": the team is paused")
            else:
                plan["now"].append({"kind": kind, "text": words})
        elif kind == "resume" and (kind,) not in seen:
            # Lifting his own pause is his to confirm: the words that asked
            # for it came through a session.
            seen.add((kind,))
            if paused:
                confirm({"kind": "resume", "line": "Resume the team (it is paused now)"}, "resume")
        elif (kind,) not in seen:
            seen.add((kind,))
            plan["now"].append({"kind": kind})
    # A release comes after the merges it should carry, whatever order was named.
    order = {"resume": 0, "release": 2}
    plan["confirm"].sort(key=lambda s: order.get(s["kind"], 1))
    return plan


def plan_title(steps: list[dict]) -> str:
    kinds = {s["kind"] for s in steps}
    if kinds == {"merge"}:
        n = len(steps)
        return f"Merge {n} pull request{'s' if n != 1 else ''} into main"
    return f"{len(steps)} step{'s' if len(steps) != 1 else ''}, in this order"


def plan_text(steps: list[dict]) -> str:
    return "\n".join(f"{i}. {s['line']}" for i, s in enumerate(steps, 1))


def plan_fresh(p: dict, now: dt.datetime) -> bool:
    """Is the yes on this plan recent enough to act on? Both the yes and the
    plan it was said to: what a plan says ("now true") ages too."""
    try:
        answered = dt.datetime.fromisoformat(p.get("answered_at") or "")
        sent = dt.datetime.fromisoformat(p.get("sent_at") or "")
    except ValueError:
        return False
    return now - answered <= dt.timedelta(minutes=PLAN_FRESH_MIN) and answered - sent <= dt.timedelta(
        minutes=PLAN_OPEN_MIN
    )


def next_version(tags: list[str]) -> str:
    """The tag after the highest `vX.Y.Z` among `tags`, one patch up; '' when
    there is none to count from."""
    found = [tuple(map(int, m.groups())) for m in (re.fullmatch(r"v(\d+)\.(\d+)\.(\d+)", t.strip()) for t in tags) if m]
    if not found:
        return ""
    major, minor, patch = max(found)
    return f"v{major}.{minor}.{patch + 1}"


def checks_state(runs) -> str:
    """GitHub's check runs on a commit: 'pass' (all finished, none failed, at
    least one there), 'fail' (one finished badly), else 'pending' (also when
    none has shown up yet)."""
    runs = [r for r in runs if isinstance(r, dict)] if isinstance(runs, list) else []
    done = [r for r in runs if r.get("status") == "completed"]
    if any(r.get("conclusion") not in ("success", "skipped", "neutral") for r in done):
        return "fail"
    return "pass" if runs and len(done) == len(runs) else "pending"


def _env_lines(env_text: str):
    """(index, name, raw value) of each assignment line, read as the app's
    dotenv reader does: `export` allowed, quotes and a trailing comment off."""
    for i, line in enumerate(env_text.split("\n")):
        m = re.match(r"\s*(?:export\s+)?([A-Za-z_][A-Za-z0-9_]*)\s*=(.*)$", line.rstrip("\r"))
        if not m:
            continue
        value = m.group(2).strip()
        if value[:1] in ("'", '"'):
            end = value.find(value[0], 1)
            value = value[1:end] if end > 0 else value[1:]
        else:
            value = re.split(r"\s+#", value, maxsplit=1)[0].strip()
        yield i, m.group(1), value


def switch_state(name: str, raw: str | None) -> str:
    """'true' / 'false': what the fetcher makes of a switch's value (None =
    not set)."""
    word = (raw or "").lower() if raw is not None else None
    if name in SWITCH_DEFAULT_ON:
        return "false" if word in ("false", "0") else "true"
    return "true" if word in ("true", "1") else "false"


def set_switch(env_text: str, name: str, value: str) -> str:
    """The settings file with `name=value`: written where the name first
    stands (the line the app reads), any later line for it removed, else
    added at the end. Every other line stays as it is."""
    lines = env_text.split("\n")
    hits = [i for i, key, _ in _env_lines(env_text) if key == name]
    if hits:
        lines[hits[0]] = f"{name}={value}" + ("\r" if lines[hits[0]].endswith("\r") else "")
        for i in reversed(hits[1:]):
            del lines[i]
        return "\n".join(lines)
    return env_text + ("" if not env_text or env_text.endswith("\n") else "\n") + f"{name}={value}\n"


def read_switches(env_text: str) -> dict:
    """Every trading switch as the app will read it from this settings file
    ('true' / 'false'): the first line for a name counts, and one that is not
    set has the fetcher's default."""
    raw = {}
    for _, key, value in _env_lines(env_text):
        if key in TRADING_SWITCHES:
            raw.setdefault(key, value)
    return {name: switch_state(name, raw.get(name)) for name in TRADING_SWITCHES}


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
    """A text nobody answered in a week is closed. Returns how many. The week
    counts from the last thing that happened to it, so a first press on a
    trading change gives the second one its own week."""
    limit, closed = _stamp(now - dt.timedelta(days=EXPIRE_DAYS)), 0
    for p in proposals:
        last = max(p.get("sent_at") or "", p.get("answered_at") or "")
        if p.get("state") in OPEN_STATES and last and last < limit:
            p["state"], closed = "expired", closed + 1
    return closed


def proposal_id(kind: str, key: str, created: str) -> str:
    return hashlib.sha1(f"{kind}|{key}|{created}".encode()).hexdigest()[:8]


# ---------------------------------------------------------------- the watch --
#
# `watch.py` looks at the installed Pulse every ten minutes and hands what it
# saw to `watch_findings`. A fact it could not read is None, and a check
# without its facts says nothing: no reading is never read as "all is well"
# or as "something is wrong".

WATCH_GAP_MIN = 25  # two looks further apart than this: the Mac slept between
# Market open and no position run for this long: two hourly turns missed. One
# missed turn is a broker blip, and it is not worth a text at night.
SILENT_MIN = 130
CLOCK_MIN = 70  # the broker's clock silent at every look for this long
# A failure older than this is no longer "happening now".
RECENT_MIN = 90
# Two failed runs closer than this are one turn tried twice, not two turns.
TURN_MIN = 45
STALL_MIN = 130  # the hourly schedule has skipped two turns
STUCK_FETCH_MIN = 180  # a briefing on local AI takes up to about 70 minutes
DEAD_FETCH_MIN = 10
LISTENER_MIN = 15
# Failed tries before a missing briefing is serious. The evening has only the
# turns of 21:00, 22:00 and 23:00, and a try that fails after an hour of work
# takes the next turn with it: two failures are all a slow evening can show.
BRIEFING_TRIES = {"morning": 3, "evening": 2}
# A try that starts at 23:00 can fail after midnight and still belongs to the
# evening before: a briefing's day ends at this hour of the next morning.
BRIEFING_DAY_ENDS_H = 3
# One text per problem: a serious one is repeated once a day while it lasts,
# anything else once a week.
ALERT_AGAIN_H = {True: 24, False: 7 * 24}
# A serious problem that keeps ending and coming back is texted as back this
# many times; after that nothing more is said about it until a day has passed.
FLAP_MAX = 3
ALERT_KEEP_DAYS = 30
SCHEDULERS = {
    "com.pulse.scheduled-fetch": "the hourly schedule (briefings and trading)",
    "com.pulse.live-signals": "the live signals",
}


def _hm(t: dt.datetime, now: dt.datetime) -> str:
    return f"{t:%H:%M}" if t.date() == now.date() else f"{t:%a %H:%M}"


def briefing_day(t: dt.datetime) -> dt.date:
    """The day a briefing try belongs to (see BRIEFING_DAY_ENDS_H)."""
    return (t - dt.timedelta(hours=BRIEFING_DAY_ENDS_H)).date()


def awake_since(
    ticks: list[dt.datetime], now: dt.datetime, gap_min: int = WATCH_GAP_MIN
) -> dt.datetime:
    """Since when the watch has been looking without a break. A Mac that slept
    saw nothing, so nothing is said about the time it was asleep."""
    start = now
    for t in sorted((t for t in ticks if t <= now), reverse=True):
        if start - t > dt.timedelta(minutes=gap_min):
            break
        start = t
    return start


def watch_findings(facts: dict, memory: dict, now: dt.datetime) -> list[dict]:
    """What is wrong right now, as [{key, serious, text}]. `memory` is the
    watch's own notebook between looks (when a position was first seen without
    a stop, since when the broker's clock is silent) and is updated here.

    Serious means: texted at any hour. Everything else waits for the day."""
    found: list[dict] = []
    awake = facts.get("awake") or now
    events = facts.get("events")
    progress = facts.get("progress") or {}

    def add(key: str, serious: bool, text: str, **more) -> None:
        found.append({"key": key, "serious": serious, "text": text, **more})

    def minutes(since: dt.datetime) -> int:
        """How long something has lasted, counting only time the watch was awake."""
        return int((now - max(since, awake)).total_seconds() // 60)

    def hm(t: dt.datetime) -> str:
        return _hm(t, now)

    running = bool(
        progress
        and progress.get("stage") not in ("complete", "failed")
        and progress.get("updated")
        and now - progress["updated"] < dt.timedelta(minutes=DEAD_FETCH_MIN)
    )

    # A held position without a stop at the broker.
    bare, seen = facts.get("bare"), memory.setdefault("bare", {})
    if bare is not None and events is not None:
        ids = {str(b["id"]) for b in bare}
        for gone in [k for k in seen if k not in ids]:
            del seen[gone]
        # Every finished position run, and whether it was a dry one: with
        # EXIT_DRY_RUN it names what it would have done and places nothing.
        # Only the latest run counts, so the alarm is back with the first real
        # run after the exits are armed.
        passes: list[tuple[dt.datetime, bool]] = []
        dry_run = False
        for kind, when, _ in events:
            if kind == "manage_dry":
                dry_run = True
            elif kind in ("manage_start", "manage_fail"):
                dry_run = False
            elif kind == "manage_ok":
                passes.append((when, dry_run))
                dry_run = False
        dry = bool(passes) and passes[-1][1]
        for b in bare:
            first = dt.datetime.fromisoformat(seen.setdefault(str(b["id"]), _stamp(now)))
            since = [p for p in passes if p[0] > first]
            if since and not dry:
                add(
                    f"stop:{b['id']}",
                    True,
                    f"{b['ticker']} has no stop at the broker, and the position run at "
                    f"{hm(since[-1][0])} did not put one back. Nothing limits the loss on it "
                    "until it has one.",
                    name=str(b["ticker"]),
                )
        if dry and bare:
            names = ", ".join(str(b["ticker"]) for b in bare[:10])
            add(
                "stop:dry",
                False,
                f"Exits are in dry-run mode (EXIT_DRY_RUN), so {len(bare)} open "
                f"position{'s have' if len(bare) != 1 else ' has'} no stop at the broker: {names}.",
            )

    def in_a_row(first: dt.datetime, second: dt.datetime) -> bool:
        """Two failed turns of the same day, with the second still fresh. The
        turns need not be an hour apart: a slow briefing takes the turn after
        it, so two failures in a row can be two hours apart."""
        return (
            first.date() == second.date()
            and second - first >= dt.timedelta(minutes=TURN_MIN)
            and now - second <= dt.timedelta(minutes=RECENT_MIN)
        )

    if events is not None:
        # Position management failing twice in a row.
        runs: list[list] = []
        for kind, when, detail in events:
            if kind == "manage_start":
                runs.append([when, "", ""])
            elif kind in ("manage_ok", "manage_fail") and runs and not runs[-1][1]:
                runs[-1][1:] = [kind, detail]
        last = runs[-2:]
        if (
            len(last) == 2
            and all(r[1] == "manage_fail" for r in last)
            and in_a_row(last[0][0], last[1][0])
        ):
            add(
                "manage",
                True,
                f"Position management failed twice in a row (at {hm(last[0][0])} and "
                f"{hm(last[1][0])}, exit {last[1][2] or '?'}). Exits and stop updates are "
                "not happening. The stops already at the broker still hold.",
            )

        # Buying failing twice in a row.
        oks = [w for k, w, _ in events if k == "auto_ok"]
        fails = [(w, d) for k, w, d in events if k == "auto_fail" and (not oks or w > max(oks))]
        if len(fails) >= 2 and in_a_row(fails[-2][0], fails[-1][0]):
            add(
                "auto",
                True,
                f"Buying failed twice in a row (at {hm(fails[-2][0])} and "
                f"{hm(fails[-1][0])}, exit {fails[-1][1] or '?'}). No new positions are "
                "opened. The ones held are not affected.",
            )

        # A briefing that keeps failing. One made by the app or by hand is in
        # the database and not in the log: after it the schedule stops trying,
        # so the failures before it are over.
        day = briefing_day(now)
        today = [(k, w, d) for k, w, d in events if briefing_day(w) == day]
        made = list(facts.get("briefings") or [])  # an earlier day's is before every try
        for slot, tries in BRIEFING_TRIES.items():
            done = [w for k, w, d in today if k == "brief_done" and d == slot] + made
            failed = [
                (w, d.split()[-1])
                for k, w, d in today
                if k == "brief_fail" and d.split()[0] == slot and (not done or w > max(done))
            ]
            if len(failed) >= tries:
                until = " until midnight" if slot == "evening" else ""
                add(
                    f"briefing:{slot}:{day.isoformat()}",
                    True,
                    f"The {slot} briefing failed {len(failed)} times (last try "
                    f"{hm(failed[-1][0])}, exit {failed[-1][1]}). The schedule tries again "
                    f"every hour{until}.",
                )

    # The market is open and nobody looks at the positions.
    market = facts.get("market")
    if events is not None and market == "open" and facts.get("session_start"):
        starts = [w for k, w, _ in events if k == "manage_start"]
        base = max([facts["session_start"], *starts[-1:]])
        idle = minutes(base)
        if idle >= SILENT_MIN:
            why = ""
            if running and progress.get("started"):
                why = (
                    f" A fetch has been running since {hm(progress['started'])} and holds "
                    "up the hourly schedule."
                )
            said = f"last at {hm(starts[-1])}" if starts else "no run in the log"
            add(
                "silent",
                True,
                f"The market is open and Pulse has not looked at its positions for {idle} "
                f"minutes ({said}).{why} The stops at the broker still hold, but exits and "
                "stop updates are not happening.",
            )

    # The broker's clock does not answer, so trading is skipped without a word.
    if facts.get("session_start") and market == "unknown":
        since = dt.datetime.fromisoformat(memory.setdefault("clock_bad", _stamp(now)))
        if minutes(since) >= CLOCK_MIN:
            add(
                "clock",
                False,
                f"The broker's clock has not answered for {minutes(since)} minutes. Pulse "
                "skips trading while it cannot tell whether the market is open. That is an "
                "outage at the broker, the network, or a missing key.",
            )
    else:
        memory.pop("clock_bad", None)

    # A fetch that hangs, or one that died midway.
    if progress and progress.get("stage") not in ("complete", "failed"):
        started, updated = progress.get("started"), progress.get("updated")
        label = progress.get("label") or progress.get("stage") or "?"
        if running and started and minutes(started) >= STUCK_FETCH_MIN:
            add(
                "fetch:stuck",
                False,
                f"A fetch has been running since {hm(started)} ({minutes(started)} minutes, "
                f'now at "{label}"). While it runs the hourly schedule waits, trading included.',
            )
        elif (
            updated
            and facts.get("fetching") is False
            and now - updated >= dt.timedelta(minutes=DEAD_FETCH_MIN)
        ):
            add(
                f"fetch:died:{_stamp(started or updated)}",
                False,
                f"The fetch that started at {hm(started or updated)} stopped midway "
                f'(at "{label}", last sign of life {hm(updated)}). The schedule tries again '
                "at the next hour.",
            )

    # The schedulers.
    for label, what in SCHEDULERS.items():
        if (facts.get("loaded") or {}).get(label) is False:
            add(
                f"scheduler:{label}",
                False,
                f"{what[0].upper()}{what[1:]} is not loaded in launchd ({label}), so it does "
                "not run. Running scripts/install-app.sh puts it back.",
            )
    stamp = facts.get("stamp")
    if (
        stamp
        and stamp.date() == now.date()
        and 9 <= now.hour <= 20
        and not running
        and facts.get("fetching") is not True  # a long run is busy, not stalled
        and minutes(stamp) >= STALL_MIN
    ):
        add(
            "scheduler:stalled",
            False,
            f"The hourly schedule has not run since {hm(stamp)}. Briefings and trading "
            "depend on it.",
        )

    # Crashes.
    for name in facts.get("crashes") or []:
        add(f"crash:{name}", False, f"Pulse crashed (crash report {name}).")
    for where in facts.get("panics") or []:
        add(
            f"panic:{where}:{now.date().isoformat()}",
            False,
            f"The fetcher crashed in its own code ({where}).",
        )

    # The team itself.
    wake = facts.get("team_wake")
    if wake and facts.get("team_running") is False and minutes(wake) >= STALL_MIN:
        add(
            "team",
            False,
            f"The team's own hourly wake-up has not run since {hm(wake)}, so the night "
            "shift would not start.",
        )
    beat = facts.get("listener")
    if facts.get("paired") and beat and minutes(beat) >= LISTENER_MIN:
        add(
            "listener",
            False,
            f"The Telegram listener has been silent since {hm(beat)}: buttons and commands "
            "do nothing until it is back (team.py install restarts it).",
        )
    return found


def watch_over(key: str, alert: dict, facts: dict, now: dt.datetime) -> str | None:
    """What to say when a serious problem has ended, or None while that cannot
    be told. A finding that is merely no longer listed is not the end of it:
    the market may have closed, or the facts may be missing this time.

    "Solved" is said for a problem that went away. A briefing whose day ended
    without it is over too, and that is said as it is."""
    events = facts.get("events")
    if events is None:
        return None
    sent = dt.datetime.fromisoformat(alert["sent"])

    def after(kind: str, detail: str | None = None) -> dt.datetime | None:
        times = [
            w for k, w, d in events if k == kind and w > sent and (detail is None or d == detail)
        ]
        return max(times) if times else None

    if key.startswith("stop:") and key != "stop:dry":
        bare = facts.get("bare")
        if bare is None or key[5:] in {str(b["id"]) for b in bare}:
            return None
        return (
            f"Solved. {alert.get('name') or 'The position'} is no longer open without a "
            "stop: it has one now, or the position was closed."
        )
    if key == "silent" and (t := after("manage_start")):
        return f"Solved. Pulse is looking at its positions again (run at {_hm(t, now)})."
    if key == "manage" and (t := after("manage_ok")):
        return f"Solved. Position management works again (run at {_hm(t, now)})."
    if key == "auto" and (t := after("auto_ok")):
        return f"Solved. Buying works again (run at {_hm(t, now)})."
    if key.startswith("briefing:"):
        _, slot, day = key.split(":")
        # In the log when the schedule made it, only in the database when the
        # app or a hand did.
        rows = facts.get("briefings")
        made = [
            w for k, w, d in events if k == "brief_done" and d == slot and w > sent
        ] + [w for w in rows or [] if w > sent]
        made = [w for w in made if briefing_day(w).isoformat() == day]
        if made:
            return f"Solved. The {slot} briefing was made after all, at {_hm(max(made), now)}."
        # "Not made" is only said with the database read: the log alone does
        # not know a briefing made outside the schedule.
        if rows is not None and briefing_day(now).isoformat() > day:
            when = dt.date.fromisoformat(day)
            return f"The {slot} briefing of {when:%a} {when.day} {when:%b} was not made."
    return None


def watch_alerts(
    found: list[dict], facts: dict, memory: dict, now: dt.datetime
) -> list[tuple[str, bool]]:
    """The texts to send for what the watch found, as [(text, serious)], and
    the watch's book of what it already said (kept in `memory`).

    One text per problem: while it lasts nothing is said twice within a day (a
    week when it is not serious, however often it comes and goes). The end of
    a serious problem is said once, and only when it can be told for sure.
    After that the same problem coming back is a new text: its end was told,
    so its return must be, or the next "Solved" would be about nothing. One
    that keeps coming and going is texted as back FLAP_MAX times; the last of
    those says so, and then it is quiet until a day has passed."""
    book = memory.setdefault("alerts", {})
    out: list[tuple[str, bool]] = []
    for f in found:
        old = book.get(f["key"])
        lasting = bool(old and old.get("open"))
        again = dt.timedelta(hours=ALERT_AGAIN_H[bool(f["serious"])])
        recent = bool(old) and now - dt.datetime.fromisoformat(old["sent"]) < again
        back = bool(old) and bool(f["serious"]) and not lasting
        if recent and not back:
            old["open"] = True
            continue
        flaps = old.get("flaps", 0) + 1 if back and recent else 0
        if flaps > FLAP_MAX:  # said for the last time today
            old["open"] = True
            continue
        first = old["first"] if lasting else _stamp(now)
        book[f["key"]] = {
            "first": first,
            "sent": _stamp(now),
            "serious": bool(f["serious"]),
            "open": True,
            **({"name": f["name"]} if f.get("name") else {}),
            **({"flaps": flaps} if flaps else {}),
        }
        text = f["text"]
        if lasting:
            since = _hm(dt.datetime.fromisoformat(first), now)
            text = f"Still not solved (first seen {since}). {text}"
        elif flaps == FLAP_MAX:
            text = (
                f"It is back again. {text} It keeps coming and going, so I will not "
                "text about it again until a day has passed."
            )
        elif flaps:
            text = f"It is back. {text}"
        out.append((text, bool(f["serious"])))
    listed = {f["key"] for f in found}
    for key, alert in list(book.items()):
        if alert.get("open") and key not in listed:
            if not alert.get("serious"):
                alert["open"] = False
            elif over := watch_over(key, alert, facts, now):
                alert["open"] = False
                if alert.get("flaps", 0) < FLAP_MAX:
                    out.append((over, False))
        age = now - dt.datetime.fromisoformat(alert["sent"])
        if age > dt.timedelta(days=ALERT_KEEP_DAYS):
            del book[key]
    return out
