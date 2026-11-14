"""Decision rules for the night team. Pure functions, no I/O, so every rule has a unit test.

team.py does the running (git, gh, claude); this file decides what is allowed.
"""

from __future__ import annotations

import fnmatch
import hashlib
import re

# Files the night team may not change without Nicola. A PR that touches one is
# finished and reviewed as usual but left open with the `needs-you` label.
#   - CI and the team's own scripts are the gate itself.
#   - launchd/, install and schedule scripts decide what runs on the Mac.
#   - tauri.conf.json / capabilities hold the updater key and app permissions.
#   - go_live.rs holds thresholds that must not be tuned to a result.
#   - .claude/ holds settings, hooks and skills that Claude sessions load and run.
PROTECTED_GLOBS = [
    ".github/*",
    ".claude/*",
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
    state: dict, key: str, sha: str, result: str, parked: bool = False
) -> None:
    """`parked` = the team itself put the `needs-you` label on this PR."""
    attempts = state.setdefault("attempts", {})
    last = attempts.get(key) or {}
    tries = last.get("tries", 0) + 1 if last.get("sha") == sha else 1
    attempts[key] = {"sha": sha, "result": result, "tries": tries, "parked": parked}


def clear_attempt(state: dict, key: str) -> None:
    (state.get("attempts") or {}).pop(key, None)


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


def _pr_job(pr: dict, label: str) -> dict:
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
        return pr_skip_reason(pr, owner, base, label, state) or error_cap(
            state, f"pr-{pr['number']}", pr.get("headRefOid") or ""
        )

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
                return ([], [skip_pr(pr, why)]) if why else ([_pr_job(pr, label)], [])
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
            jobs.append(_pr_job(pr, label))
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
