#!/usr/bin/env python3
"""The Pulse night team: an unattended build -> check -> review -> merge loop.

    team.py run [--only main|pr-<n>|idea] [--max-jobs N] [--no-merge]
    team.py install | uninstall | status
    team.py idea "one line"
    team.py page <artifact url>

What it does and the rules it merges by are in TEAM.md. The decisions live in
rules.py (unit-tested); this file only runs things: git, gh, the checks and the
`claude -p` sessions.
"""

from __future__ import annotations

import argparse
import datetime as dt
import fcntl
import json
import os
import plistlib
import re
import shutil
import signal
import subprocess
import sys
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import page  # noqa: E402

import rules  # noqa: E402

HOME = Path(os.environ.get("PULSE_TEAM_HOME") or Path.home() / ".pulse-team")
REPO = Path(os.environ.get("PULSE_TEAM_REPO") or Path.home() / "dev/Pulse")
LIVE_DB = Path.home() / "Library/Application Support/com.pulse.app/pulse.db"
DB_COPY = HOME / "db/pulse-copy.db"  # tonight's copy of the live database
DB_JOB = HOME / "db/job.db"  # what sessions use; a fresh clone of the copy per job
IDEAS = HOME / "IDEAS.md"
PAGE_URL = HOME / "page-url.txt"  # the artifact the nightly page is published to
BASE = "main"
LABEL = "needs-you"
PLIST_LABEL = "com.pulse.team"
PLIST = Path.home() / "Library/LaunchAgents" / f"{PLIST_LABEL}.plist"


def _int(name: str, default: int) -> int:
    return int(os.environ.get(name) or default)


MAX_JOBS = _int("PULSE_TEAM_MAX_JOBS", 3)
BUDGET_MIN = _int("PULSE_TEAM_BUDGET_MIN", 240)  # no new job starts after this
BUILDER_MIN = _int("PULSE_TEAM_BUILDER_MIN", 75)
REVIEWER_MIN = _int("PULSE_TEAM_REVIEWER_MIN", 40)
CHECKS_MIN = _int("PULSE_TEAM_CHECKS_MIN", 60)
CI_MIN = _int("PULSE_TEAM_CI_MIN", 45)
MAX_FIX_ROUNDS = _int("PULSE_TEAM_FIX_ROUNDS", 2)
MODEL = os.environ.get("PULSE_TEAM_MODEL", "")  # empty = the default from settings
RUN_AT = (_int("PULSE_TEAM_HOUR", 2), _int("PULSE_TEAM_MINUTE", 30))  # used by install

# Sessions get placeholders for every key Pulse knows, so nothing they build or
# run can trade or spend. (dotenvy never overrides a variable that is already
# set, so the real .env files lose.) ANTHROPIC_API_KEY is left out on purpose:
# `claude` itself would try to log in with it. PULSE_LLM=local covers that one.
PLACEHOLDER_KEYS = [
    "ALPACA_API_KEY",
    "ALPACA_SECRET_KEY",
    "CEREBRAS_API_KEY",
    "EIA_API_KEY",
    "FEC_API_KEY",
    "FINNHUB_API_KEY",
    "FRED_API_KEY",
    "GROQ_API_KEY",
    "TAVILY_API_KEY",
    "VOYAGE_API_KEY",
]
# Nothing the team starts can publish, however the command is spelled: gh gets a
# token GitHub rejects, and git rewrites every push to GitHub to a dead address.
# (The runner's own git and gh calls run without this.)
NO_PUBLISH = {
    "GH_TOKEN": "night-team-placeholder",
    "GIT_CONFIG_COUNT": "2",
    "GIT_CONFIG_KEY_0": "url.night-team-no-push://blocked/.pushInsteadOf",
    "GIT_CONFIG_VALUE_0": "https://github.com/",
    "GIT_CONFIG_KEY_1": "url.night-team-no-push://blocked/.pushInsteadOf",
    "GIT_CONFIG_VALUE_1": "git@github.com:",
}
SAFE_ENV = {
    **NO_PUBLISH,
    "AUTO_TRADE_ENABLED": "false",
    "EXIT_DRY_RUN": "true",
    "AUTO_TRADE_PREVIEW": "true",
    "LEARNING_AUTO_APPLY": "false",
    "PULSE_LLM": "local",
    "PULSE_CLOUD_TASKS": "",
    "PULSE_SNAPSHOT": "off",
    # scheduled-fetch.sh / live-signals.sh run the INSTALLED fetcher on the live
    # database; with this they find no fetcher and exit.
    "PULSE_FETCHER": "/nonexistent/night-team-no-fetcher",
    "CLAUDE_TEAM_NIGHT": "1",  # ~/.claude/hooks/notify.sh stays quiet
}
USAGE_LIMIT = re.compile(r"usage limit|hit your limit|limit reached|rate.?limit", re.I)

BUILDER_SCHEMA = {
    "type": "object",
    "properties": {
        "status": {"type": "string", "enum": ["done", "needs_user", "blocked"]},
        "summary": {"type": "string"},
        "question": {"type": "string"},
        "pr_title": {"type": "string"},
        "pr_body": {"type": "string"},
        "try_it": {"type": "array", "items": {"type": "string"}},
    },
    "required": ["status", "summary"],
}
REVIEW_SCHEMA = {
    "type": "object",
    "properties": {
        "verdict": {"type": "string", "enum": ["pass", "fail"]},
        "summary": {"type": "string"},
        "findings": {
            "type": "array",
            "items": {
                "type": "object",
                "properties": {
                    "severity": {
                        "type": "string",
                        "enum": ["blocker", "major", "minor"],
                    },
                    "where": {"type": "string"},
                    "problem": {"type": "string"},
                },
                "required": ["severity", "problem"],
            },
        },
        "verified": {"type": "array", "items": {"type": "string"}},
        "not_verified": {"type": "array", "items": {"type": "string"}},
        "try_it": {"type": "array", "items": {"type": "string"}},
        "new_dependencies": {"type": "array", "items": {"type": "string"}},
    },
    "required": ["verdict", "summary", "findings"],
}

BUILD_IDEA = """## Your job: build this idea

Idea, from Nicola's list: "{title}"

You are on a new branch made from the latest main.
1. Read the code you need. Decide the goal, what "done" looks like, and what you will leave out.
2. If it is too big for one night, build the smallest slice that is useful by itself and say what is left.
3. Build it with tests, run the checks, and see it working for real where you safely can.
4. Commit. Do not push.

Return `done` with `pr_title`, `pr_body` (markdown: what and why, how you verified it, a short "Try it yourself" list), `summary` and `try_it`. Return `needs_user` with one `question` if it needs a decision only Nicola can make, or `blocked` with the reason in `summary`."""

FIX_MAIN = """## Your job: make main pass its checks again

The check "{step}" fails on the latest main. The end of its output:

```
{tail}
```

You are on a new branch made from main. Find the root cause, fix it (do not weaken or skip the check), run the checks, commit. Do not push.
Return `done` with `pr_title`, `pr_body`, `summary` and `try_it`, or `blocked` / `needs_user` as the brief says."""

FIX_CHANGE = """## Your job: fix problems in this change

The change: {request}

It is checked out here with main merged in. Problems to fix:

{problems}

Fix each one at its root, with tests where behaviour changes. If a finding is wrong, leave the code alone and say why in `summary`. Run the checks, commit. Do not push.
Return `done` when everything is fixed and committed, or `blocked` / `needs_user` as the brief says."""

MERGE_MAIN = (
    f"The latest {BASE} is not merged into this branch. Run `git merge origin/{BASE}`, "
    "resolve any conflicts keeping what both sides intended, and commit the merge."
)

REVIEW = """## Your job: review this change. You did not write it.

What it is meant to do: {request}

See it with `git diff origin/{base}...HEAD` and `git log origin/{base}..HEAD`. The CI checks already pass on this commit, so do not repeat the whole suite.

1. Does it do what it is meant to, completely? Does anything it claims not actually work?
2. Look for bugs, crashes (the crash-prevention rules in CLAUDE.md), data loss, anything that could place or change an order wrongly, anything that weakens a safety check.
3. New behaviour without tests, and tests that cannot fail.
4. Run the tests that matter and, where it is safe, see the change working for real (the run-pulse skill explains how). List what you actually ran in `verified` and what you could not check in `not_verified`.
5. List new dependencies in `new_dependencies`.

Do not edit or commit anything. Verdict `fail` if there is any `blocker` or `major` finding; `minor` findings alone pass. Be concrete: file, line, what goes wrong and with which input.
`summary`: two or three plain sentences, for a non-developer, on what this change does. `try_it`: steps Nicola can follow to see it."""


# ---------------------------------------------------------------- plumbing --


def sh(
    cmd: list[str], cwd: Path | None = None, timeout: int = 600, env: dict | None = None
) -> tuple[int, str, str]:
    """Run a command. On timeout the whole process group is killed (code 124)."""
    p = subprocess.Popen(
        cmd,
        cwd=cwd,
        env=env,
        stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        start_new_session=True,
    )
    try:
        out, err = p.communicate(timeout=timeout)
        return p.returncode, out or "", err or ""
    except subprocess.TimeoutExpired:
        try:
            os.killpg(p.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        try:
            out, err = p.communicate(timeout=30)
        except subprocess.TimeoutExpired:  # something escaped the group and holds the pipe
            out, err = "", ""
        return 124, out or "", (err or "") + f"\n[timed out after {timeout // 60} min]"


def git(*args: str, cwd: Path | None = None, timeout: int = 300) -> tuple[int, str]:
    code, out, err = sh(["git", *args], cwd=cwd or REPO, timeout=timeout)
    return code, (out if code == 0 else out + err).strip()


def gh(*args: str, timeout: int = 120) -> tuple[int, str]:
    code, out, err = sh(["gh", *args], cwd=REPO, timeout=timeout)
    return code, (out if code == 0 else out + err).strip()


def gh_json(*args: str):
    code, out = gh(*args)
    if code != 0:
        raise RuntimeError(f"gh {' '.join(args[:3])} failed: {out[-300:]}")
    return json.loads(out)


def log(msg: str) -> None:
    print(f"{dt.datetime.now():%H:%M:%S} {msg}", flush=True)


def load_json(path: Path, default):
    try:
        return json.loads(path.read_text())
    except (OSError, ValueError):
        return default


def save_json(path: Path, data) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    tmp = path.with_suffix(".tmp")
    tmp.write_text(json.dumps(data, indent=2))
    tmp.replace(path)


def night_env(worktree: Path | None = None) -> dict:
    """The environment of every unattended session: no trading, no real keys,
    no way to publish, a database copy instead of the live one."""
    env = dict(os.environ)
    env.update(SAFE_ENV)
    for key in PLACEHOLDER_KEYS:
        env[key] = "night-team-placeholder"
    env.pop("ANTHROPIC_API_KEY", None)
    env["PULSE_TEAM_HOME"] = str(HOME)
    env["PULSE_TEAM_REPO"] = str(REPO)
    env["CARGO_TARGET_DIR"] = str(HOME / "target")
    env["PULSE_TEAM_CHECKS"] = str(HERE / "checks.sh")
    # Always set, even when there is no copy: the dev app then opens an empty
    # database there, never the live one.
    env["PULSE_DB_PATH"] = str(DB_JOB)
    if worktree:
        env["PULSE_TEAM_WORKTREE"] = str(worktree)
    return env


def brief() -> str:
    text = (HERE / "TEAM.md").read_text()
    return text.split("# Brief for sessions", 1)[1].strip()


def guard_settings() -> dict:
    """guard.py as a PreToolUse hook on every tool call of a session."""
    hook = {
        "type": "command",
        "command": f"python3 '{HERE / 'guard.py'}'",
        "timeout": 10,
    }
    return {"hooks": {"PreToolUse": [{"matcher": "*", "hooks": [hook]}]}}


def session(
    prompt: str, cwd: Path, schema: dict, minutes: int, name: str, run: dict
) -> dict:
    """One unattended `claude -p` session. Returns {ok, data, error}."""
    cmd = [
        "claude",
        "-p",
        f"{brief()}\n\n{prompt}",
        "--output-format",
        "json",
        "--json-schema",
        json.dumps(schema),
        "--permission-mode",
        "auto",
        "--permission-prompts",
        "none",
        "--settings",
        json.dumps(guard_settings()),
        "--strict-mcp-config",  # no connectors: a coding session needs no mail or drive
        "-n",
        name,
    ]
    if MODEL:
        cmd += ["--model", MODEL]
    code, out, err = sh(cmd, cwd=cwd, timeout=minutes * 60, env=night_env(cwd))
    (run["dir"] / f"{name}.json").write_text(out or err)
    try:
        res = json.loads(out)
    except ValueError:
        res = {}
    run["cost"] += res.get("total_cost_usd") or 0
    data = res.get("structured_output")
    if code == 0 and not res.get("is_error") and isinstance(data, dict):
        return {"ok": True, "data": data}
    error = str(res.get("result") or err or out or "no output")[-400:].strip()
    if code == 124:
        error = f"timed out after {minutes} min"
    if res.get("is_error") and USAGE_LIMIT.search(error):
        run["abort"] = "Claude's usage limit was reached"
    return {"ok": False, "data": {}, "error": error}


def head(wt: Path) -> str:
    return git("rev-parse", "HEAD", cwd=wt)[1]


def make_worktree(name: str, ref: str, branch: str | None) -> Path:
    """A fresh worktree under .claude/worktrees/team-<name>. Only ever replaces
    the team's own leftovers (the team- prefix)."""
    path = REPO / ".claude/worktrees" / f"team-{name}"
    if path.exists():
        git("worktree", "remove", "--force", str(path))
    git("worktree", "prune")
    args = (
        ["worktree", "add", "-B", branch, str(path), ref]
        if branch
        else ["worktree", "add", "--detach", str(path), ref]
    )
    code, out = git(*args)
    if code != 0:
        raise RuntimeError(f"could not create worktree: {out[-300:]}")
    return path


def run_checks(wt: Path, run: dict, name: str) -> tuple[bool, str, str]:
    """checks.sh from the installed copy. Returns (ok, failed step, end of output)."""
    # The checks run the code under review, so they cannot publish either.
    env = dict(os.environ, CARGO_TARGET_DIR=str(HOME / "target"), **NO_PUBLISH)
    code, out, err = sh(
        ["bash", str(HERE / "checks.sh")], cwd=wt, timeout=CHECKS_MIN * 60, env=env
    )
    text = out + err
    (run["dir"] / f"{name}-checks.log").write_text(text)
    if code == 0:
        return True, "", ""
    failed = [
        line.split("CHECK FAILED: ", 1)[1]
        for line in text.splitlines()
        if "CHECK FAILED: " in line
    ]
    step = failed[-1] if failed else ("timed out" if code == 124 else "checks")
    return False, step, text[-3000:]


def reset_job_db() -> None:
    """Give the next job an untouched database: a clone of tonight's copy (on
    APFS `cp -c` takes no time and no space), so one job's migrations or test
    data never reach the next."""
    for suffix in ("", "-wal", "-shm"):
        Path(f"{DB_JOB}{suffix}").unlink(missing_ok=True)
    if DB_COPY.exists():
        if sh(["cp", "-c", str(DB_COPY), str(DB_JOB)], timeout=600)[0] != 0:
            shutil.copy2(DB_COPY, DB_JOB)


def copy_database() -> None:
    """A consistent copy of the live database for sessions to run against."""
    if not LIVE_DB.exists():
        return
    DB_COPY.parent.mkdir(parents=True, exist_ok=True)
    tmp = DB_COPY.with_suffix(".tmp")
    tmp.unlink(missing_ok=True)
    code, _, err = sh(
        ["sqlite3", "-readonly", str(LIVE_DB), f".backup '{tmp}'"], timeout=900
    )
    if code == 0 and tmp.exists():
        tmp.replace(DB_COPY)
    else:
        log(f"database copy failed: {err[-200:]}")


def wait_ci(pr: int, sha: str) -> str:
    """Wait for GitHub's checks on this exact commit: pass | fail | none | timeout."""
    deadline = time.time() + CI_MIN * 60
    none_until = time.time() + 5 * 60
    while time.time() < deadline:
        try:
            view = gh_json(
                "pr", "view", str(pr), "--json", "statusCheckRollup,headRefOid"
            )
        except (RuntimeError, ValueError):
            view = {}
        if view.get("headRefOid") == sha:
            state = rules.ci_state(view.get("statusCheckRollup"))
            if state in ("pass", "fail"):
                return state
            if state == "none" and time.time() > none_until:
                return "none"
        time.sleep(30)
    return "timeout"


# -------------------------------------------------------------------- jobs --


def finished(b: dict, res: dict) -> bool:
    """Fold a builder session's outcome into the job result. True = carry on."""
    if not b["ok"]:
        res["result"] = "error"
        res["reasons"].append(f"the builder session failed: {b['error']}")
        return False
    data = b["data"]
    if data.get("status") == "done":
        return True
    res["result"] = "needs_user" if data.get("status") == "needs_user" else "blocked"
    res["reasons"].append(
        data.get("question") or data.get("summary") or "the builder stopped"
    )
    return False


def publish(wt: Path, branch: str, res: dict, title: str, body: str) -> str:
    """Push the branch and make sure it has a PR. The only place anything leaves
    this Mac. Returns '' or what went wrong."""
    code, out = git("push", "origin", f"HEAD:refs/heads/{branch}", cwd=wt)
    if code != 0:
        return f"push failed: {out[-200:]}"
    res["pushed"] = head(wt)
    if res.get("pr"):
        return ""
    body_file = wt.parent / f"{wt.name}-pr-body.md"
    body_file.write_text(
        (body or res["summary"] or title)
        + "\n\n_Built overnight by the Pulse night team._\n"
    )
    code, out = gh(
        "pr",
        "create",
        "--base",
        BASE,
        "--head",
        branch,
        "--title",
        title[:120],
        "--body-file",
        str(body_file),
    )
    body_file.unlink(missing_ok=True)
    if code != 0:
        return f"could not open the PR: {out[-200:]}"
    res["pr"] = int(out.strip().rstrip("/").rsplit("/", 1)[-1])
    return ""


def park(res: dict) -> None:
    """Leave the PR open for its owner: label it and say why in a comment."""
    gh(
        "label",
        "create",
        LABEL,
        "--color",
        "D93F0B",
        "--force",
        "--description",
        "The night team stopped here and needs the owner",
    )
    if gh("pr", "edit", str(res["pr"]), "--add-label", LABEL)[0] == 0:
        res["parked"] = True
    lines = "\n".join(f"- {r[:600]}" for r in res["reasons"][:12])
    gh(
        "pr",
        "comment",
        str(res["pr"]),
        "--body",
        f"The night team did not merge this:\n\n{lines or '- it stopped without giving a reason'}\n\n"
        f"Remove the `{LABEL}` label or push a new commit and it will try again.",
    )


def merge_ready(pr: int, sha: str) -> str:
    """'' when GitHub will merge exactly this commit now, else why not."""
    view = {}
    for _ in range(6):  # GitHub works `mergeable` out lazily: UNKNOWN at first
        try:
            view = gh_json(
                "pr", "view", str(pr), "--json", "mergeable,headRefOid,state"
            )
        except (RuntimeError, ValueError):
            view = {}
        if view.get("mergeable") in ("MERGEABLE", "CONFLICTING"):
            break
        time.sleep(10)
    if (
        view.get("state") == "OPEN"
        and view.get("headRefOid") == sha
        and view.get("mergeable") == "MERGEABLE"
    ):
        return ""
    return f"GitHub will not merge it right now ({view.get('mergeable')}, {view.get('state')})"


def do_job(job: dict, res: dict, run: dict) -> None:
    kind, name = job["kind"], job["key"]
    git("fetch", "origin", "--prune")
    reset_job_db()
    title = body = ""
    if kind == "pr":
        branch = job["head"]
        request = f'PR #{job["pr"]} "{job["title"]}"\n\n{job["body"][:3000]}'
        if job.get("labelled"):  # a new commit lifted the stop
            gh("pr", "edit", str(job["pr"]), "--remove-label", LABEL)
        wt = make_worktree(name, f"origin/{branch}", None)
        res["wt"] = str(wt)
        if git("merge", "--no-edit", f"origin/{BASE}", cwd=wt)[0] != 0:
            git("merge", "--abort", cwd=wt)
            prompt = FIX_CHANGE.format(request=request, problems=MERGE_MAIN)
            if not finished(
                session(prompt, wt, BUILDER_SCHEMA, BUILDER_MIN, f"{name}-merge", run),
                res,
            ):
                return
    else:
        stamp = dt.date.today().strftime("%m%d")
        if kind == "main":
            branch = f"{rules.MAIN_FIX_PREFIX}{stamp}"
            request = f"Make {BASE} pass its checks again"
            prompt = FIX_MAIN.format(step=run["main_step"], tail=run["main_tail"])
        else:
            branch = f"feat/night-{name.removeprefix('idea-')}-{stamp}"
            request = job["title"]
            prompt = BUILD_IDEA.format(title=job["title"])
        wt = make_worktree(name, f"origin/{BASE}", branch)
        res["wt"], res["branch"] = str(wt), branch
        b = session(prompt, wt, BUILDER_SCHEMA, BUILDER_MIN, f"{name}-build", run)
        if not finished(b, res):
            return
        title = b["data"].get("pr_title") or job["title"]
        body = b["data"].get("pr_body") or ""
        res["summary"] = b["data"].get("summary", "")

    def keep_work() -> None:
        """A job that stops with commits still gets its PR, so the work and the
        reason are in one place and nothing is lost."""
        ahead = git("rev-list", "--count", f"origin/{BASE}..HEAD", cwd=wt)[1]
        if ahead not in ("", "0"):
            err = publish(wt, branch, res, title or job["title"], body)
            if err:
                res["reasons"].append(err)

    rounds = 0
    while True:
        # `problems` goes to the next fix session, `short` on the page.
        if git("status", "--porcelain", cwd=wt)[1]:
            problems = "You left uncommitted changes. Commit what belongs to the change and remove the rest."
            short = "the builder left uncommitted changes"
        elif (
            git("merge-base", "--is-ancestor", f"origin/{BASE}", "HEAD", cwd=wt)[0] != 0
        ):
            problems = MERGE_MAIN  # checked here, not taken on a session's word
            short = f"the latest {BASE} is still not merged in"
        else:
            ok, step, tail = run_checks(wt, run, f"{name}-r{rounds}")
            if not ok:
                problems = f'The check "{step}" fails. The end of its output:\n\n```\n{tail}\n```'
                short = f'the check "{step}" still fails'
            else:
                sha = head(wt)
                r = session(
                    REVIEW.format(request=request, base=BASE),
                    wt,
                    REVIEW_SCHEMA,
                    REVIEWER_MIN,
                    f"{name}-review{rounds}",
                    run,
                )
                # A reviewer must not change what it reviewed.
                git("reset", "--hard", sha, cwd=wt)
                git("clean", "-fd", cwd=wt)
                if not r["ok"]:
                    res["result"] = "error"
                    res["reasons"].append(f"the review session failed: {r['error']}")
                    return
                review = r["data"]
                for field in ("summary", "try_it", "not_verified", "new_dependencies"):
                    if review.get(field):
                        res[field] = review[field]
                bad = [
                    f
                    for f in review.get("findings") or []
                    if f.get("severity") in ("blocker", "major")
                ]
                if review.get("verdict") == "pass" and not bad:
                    break
                found = bad or [
                    {"problem": review.get("summary", "the reviewer did not pass it")}
                ]
                res["findings"] = [
                    f"{f.get('where', '')} {f['problem']}".strip() for f in found
                ]
                problems = "An independent reviewer found:\n\n" + "\n".join(
                    f"- {x}" for x in res["findings"]
                )
                short = f"the reviewer still objects: {res['findings'][0]}"
        if rounds >= MAX_FIX_ROUNDS:
            res["result"] = "blocked"
            res["reasons"].append(f"still not right after {rounds} fix rounds: {short}")
            if short.startswith("the reviewer"):
                res["reasons"] += res["findings"][1:]
            keep_work()
            return
        rounds += 1
        fix = FIX_CHANGE.format(request=request, problems=problems)
        if not finished(
            session(fix, wt, BUILDER_SCHEMA, BUILDER_MIN, f"{name}-fix{rounds}", run),
            res,
        ):
            keep_work()
            return

    # Checks and review passed on `sha`, with main merged in.
    changes = rules.parse_name_status(
        git("diff", "--name-status", "-z", f"origin/{BASE}...HEAD", cwd=wt)[1]
    )
    if not changes:
        res["result"] = "blocked"
        res["reasons"].append(
            f"nothing to merge: the branch has no changes compared with {BASE}"
        )
        return
    diff = git("diff", f"origin/{BASE}...HEAD", cwd=wt)[1]
    violations = rules.protected_violations(changes) + rules.forbidden_content(diff)
    err = publish(wt, branch, res, title or job["title"], body)
    if err:
        res["result"] = "error"
        res["reasons"].append(err)
        return
    if violations:
        res["result"] = "needs_user"
        res["reasons"] += violations
        return
    ci = wait_ci(res["pr"], sha)
    if ci != "pass":
        why = {
            "fail": f"GitHub's checks failed on {sha[:7]}",
            "none": "GitHub's checks never started",
            "timeout": f"GitHub's checks did not finish in {CI_MIN} min",
        }[ci]
        res["result"] = "blocked" if ci == "fail" else "error"
        res["reasons"].append(why)
        return
    why = merge_ready(res["pr"], sha)
    if why:
        res["result"] = "error"
        res["reasons"].append(why)
        return
    if run["no_merge"]:
        res["result"] = "ready"
        return
    code, out = gh("pr", "merge", str(res["pr"]), "--merge", "--match-head-commit", sha)
    if code != 0:
        res["result"] = "error"
        res["reasons"].append(f"merge failed: {out[-200:]}")
        return
    res["result"] = "merged"
    extra = "".join(f"\n- Not checked: {x}" for x in res.get("not_verified") or [])
    gh(
        "pr",
        "comment",
        str(res["pr"]),
        "--body",
        "Merged by the night team: local checks, an independent review and CI all passed on "
        f"`{sha[:7]}`.\n\n{res.get('summary', '')}{extra}",
    )
    if branch.startswith(rules.TEAM_PREFIXES):  # the team's own branch: tidy up
        git("push", "origin", "--delete", branch)


def run_job(job: dict, run: dict) -> dict:
    started = time.time()
    res = {
        "kind": job["kind"],
        "title": job["title"],
        "pr": job.get("pr"),
        "result": "error",
        "summary": "",
        "try_it": [],
        "not_verified": [],
        "new_dependencies": [],
        "reasons": [],
    }
    try:
        do_job(job, res, run)
    except Exception as e:  # noqa: BLE001 - one bad job must not end the night
        res["result"] = "error"
        res["reasons"].append(f"runner error: {e}")
    try:
        if res.get("pr") and res["result"] in ("needs_user", "blocked"):
            park(res)
        wt = res.pop("wt", None)
        if wt:  # never leave a finished worktree behind
            git("worktree", "remove", "--force", wt)
        branch = res.pop("branch", None)
        # The team's own local branch goes too, unless it holds commits that
        # never reached GitHub.
        if branch and (
            res.get("pushed")
            or git("rev-list", "--count", f"origin/{BASE}..refs/heads/{branch}")[1]
            in ("", "0")
        ):
            git("branch", "-D", branch)
    except Exception as e:  # noqa: BLE001
        res["reasons"].append(f"cleanup error: {e}")
    res["minutes"] = round((time.time() - started) / 60)
    log(
        f"   -> {res['result']} ({res['minutes']} min) {'; '.join(res['reasons'])[:200]}"
    )
    return res


def after_job(job: dict, res: dict, state: dict) -> None:
    """Remember the outcome so tomorrow does not repeat it, and tick the idea list."""
    result = res["result"]
    stopped = result in ("needs_user", "blocked", "error")
    if res.get("pr"):  # from here on the pull request is the unit of work
        key = f"pr-{res['pr']}"
        if stopped:
            sha = res.get("pushed") or job.get("sha") or ""
            rules.record_attempt(
                state, key, sha, result, parked=bool(res.get("parked"))
            )
        else:
            rules.clear_attempt(state, key)
    elif stopped and job["kind"] != "idea":
        rules.record_attempt(state, job["key"], job.get("sha") or "", result)
    if job["kind"] != "idea" or not IDEAS.exists():
        return
    text, title = IDEAS.read_text(), job["title"]
    today = dt.date.today().isoformat()
    why = (res["reasons"] or ["stopped"])[0].replace("\n", " ")[:300]
    if result == "merged":
        text = rules.mark_idea(text, title, "x", f"merged {today} (PR #{res['pr']})")
    elif res.get("pr"):
        text = rules.mark_idea(text, title, "x", f"now PR #{res['pr']} ({today})")
    elif result in ("needs_user", "blocked"):
        text = rules.mark_idea(text, title, "?", why)
    elif result == "error":
        rules.record_attempt(state, job["key"], "", "error")
        cap = rules.error_cap(state, job["key"], "")
        if cap:  # stop retrying; setting the line back to [ ] starts over
            rules.clear_attempt(state, job["key"])
            text = rules.mark_idea(text, title, "?", f"{cap}: {why}")
    IDEAS.write_text(text)


# --------------------------------------------------------------------- run --


def local_work(branch: str) -> str:
    """Why a PR's branch looks unfinished on this Mac ('' when it does not):
    commits never pushed, or edits not yet committed in a checkout of it."""
    if not branch:
        return ""
    code, out = git("rev-list", "--count", f"origin/{branch}..refs/heads/{branch}")
    if code == 0 and out not in ("", "0"):
        return "it has commits on this Mac that are not pushed yet"
    path = ""
    for line in git("worktree", "list", "--porcelain")[1].splitlines():
        if line.startswith("worktree "):
            path = line[len("worktree ") :]
        elif line == f"branch refs/heads/{branch}" and path:
            dirty = git("status", "--porcelain", "--untracked-files=no", cwd=Path(path))
            if dirty[0] == 0 and dirty[1]:
                return "it has uncommitted changes in a checkout on this Mac"
    return ""


def cmd_run(args) -> int:
    HOME.mkdir(parents=True, exist_ok=True)
    lock = open(HOME / "lock", "w")
    try:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
    except OSError:
        log("another run is in progress")
        return 0
    started = dt.datetime.now()
    run = {
        "dir": HOME / "runs" / started.strftime("%Y-%m-%d_%H%M"),
        "cost": 0.0,
        "abort": "",
        "no_merge": args.no_merge,
        "main_step": "",
        "main_tail": "",
    }
    run["dir"].mkdir(parents=True, exist_ok=True)
    state = load_json(HOME / "state.json", {})
    report = {
        "started": started.isoformat(timespec="minutes"),
        "jobs": [],
        "skipped": [],
        "notes": [],
        "repo": "",
    }
    try:
        work(args, run, state, report, started)
    except Exception as e:  # noqa: BLE001 - whatever happens, leave a report behind
        report["notes"].append(f"the run stopped on an error: {str(e)[-300:]}")
    return finish(report, run, state, started)


def work(args, run: dict, state: dict, report: dict, started: dt.datetime) -> None:
    log("fetching")
    if git("fetch", "origin", "--prune")[0] != 0:
        report["notes"].append("could not reach GitHub; nothing was done")
        return
    repo_info = gh_json("repo", "view", "--json", "owner,nameWithOwner")
    owner, report["repo"] = repo_info["owner"]["login"], repo_info["nameWithOwner"]
    prs = gh_json(
        "pr",
        "list",
        "--state",
        "open",
        "--limit",
        "50",
        "--json",
        "number,title,body,author,isCrossRepository,isDraft,baseRefName,headRefName,headRefOid,labels,createdAt",
    )
    for pr in prs:
        pr["busy"] = local_work(pr.get("headRefName") or "")
    ideas = rules.parse_ideas(IDEAS.read_text()) if IDEAS.exists() else []

    main_sha = git("rev-parse", f"origin/{BASE}")[1]
    main_ok = state.get("main_ok_sha") == main_sha
    if not main_ok:
        log(f"checking {BASE} ({main_sha[:7]})")
        wt = make_worktree("main-check", f"origin/{BASE}", None)
        try:
            main_ok, run["main_step"], run["main_tail"] = run_checks(wt, run, "main")
        finally:
            git("worktree", "remove", "--force", str(wt))
        if main_ok:
            state["main_ok_sha"] = main_sha
    report["main"] = {
        "sha": main_sha[:7],
        "ok": main_ok,
        "failed_step": run["main_step"],
    }

    jobs, report["skipped"] = rules.select_jobs(
        main_ok, prs, ideas, state, owner, BASE, LABEL, main_sha
    )
    if args.only:
        jobs = [j for j in jobs if j["key"] == args.only or j["kind"] == args.only]
    later, jobs = jobs[args.max_jobs :], jobs[: args.max_jobs]
    if later:
        report["notes"].append(
            f"{len(later)} more wait for the next run: "
            + "; ".join(j["title"][:70] for j in later[:5])
        )
    if jobs:
        log("copying the database for sessions")
        copy_database()
    for i, job in enumerate(jobs):
        if run["abort"]:
            report["notes"].append(f"stopped early: {run['abort']}")
            break
        if (time.time() - started.timestamp()) / 60 > BUDGET_MIN:
            report["notes"].append(
                f"ran out of time after {i} of {len(jobs)} jobs; the rest wait for tomorrow"
            )
            break
        log(f"job {i + 1}/{len(jobs)}: {job['key']} — {job['title'][:80]}")
        res = run_job(job, run)
        after_job(job, res, state)
        report["jobs"].append(res)
        save_json(HOME / "state.json", state)


def finish(report: dict, run: dict, state: dict, started: dt.datetime) -> int:
    git("fetch", "origin", "--prune", "--tags")
    tag = git("describe", "--tags", "--abbrev=0", f"origin/{BASE}")[1]
    count = (
        git("rev-list", "--count", "--merges", f"{tag}..origin/{BASE}")[1]
        if tag
        else "0"
    )
    report["unreleased"] = {"tag": tag, "merges": int(count) if count.isdigit() else 0}
    report["ideas"] = rules.parse_ideas(IDEAS.read_text()) if IDEAS.exists() else []
    report["cost"] = round(run["cost"], 2)
    report["minutes"] = round((dt.datetime.now() - started).total_seconds() / 60)
    report["finished"] = dt.datetime.now().isoformat(timespec="minutes")
    save_json(run["dir"] / "report.json", report)
    history = ([report] + load_json(HOME / "history.json", []))[:30]
    save_json(HOME / "history.json", history)
    save_json(HOME / "state.json", state)
    (HOME / "latest.txt").write_text(page.as_text(report))
    html = HOME / "page/index.html"
    html.parent.mkdir(parents=True, exist_ok=True)
    html.write_text(page.render(history))
    log(page.as_text(report))
    url = PAGE_URL.read_text().strip() if PAGE_URL.exists() else ""
    if url:
        log(
            "page published"
            if publish_page(html, url)
            else "page publish FAILED (local copy is current)"
        )
    return 0


def publish_page(html: Path, url: str) -> bool:
    """Republish the daily page. Only a background session has the Artifact
    tool, so start one, wait for its marker file, then remove it."""
    marker = HOME / "page/published.txt"
    marker.unlink(missing_ok=True)
    prompt = (
        f"Publish the file {html} to the existing artifact {url} with the Artifact tool, changing "
        "nothing in the file. If the publish is refused because the artifact has not been read in "
        "this conversation, read it (action read) and publish again. When it has succeeded write "
        f"the single word OK to {marker}; if it cannot be done write FAILED and the reason there. Then stop."
    )
    code, out, err = sh(
        [
            "claude",
            "--bg",
            "--model",
            "sonnet",
            "--permission-mode",
            "auto",
            "--settings",
            json.dumps(guard_settings()),
            "-n",
            "pulse-team-page",
            prompt,
        ],
        cwd=Path.home(),
        timeout=120,
        env=night_env(html.parent),  # the guard keeps its writes inside the page folder
    )
    parts = [p.strip() for p in out.split("·")]
    if code != 0 or len(parts) < 2:
        log(f"could not start the publish session: {(out + err)[-200:]}")
        return False
    sid = parts[1]
    deadline = time.time() + 8 * 60
    while time.time() < deadline and not marker.exists():
        time.sleep(5)
    ok = marker.exists() and marker.read_text().strip().upper().startswith("OK")
    sh(["claude", "stop", sid], timeout=60)
    sh(["claude", "rm", sid], timeout=60)
    return ok


# ---------------------------------------------------------------- commands --


def cmd_install(_args) -> int:
    bin_dir = HOME / "bin"
    bin_dir.mkdir(parents=True, exist_ok=True)
    for f in ("team.py", "rules.py", "page.py", "guard.py", "checks.sh", "TEAM.md"):
        if (HERE / f).resolve() != (bin_dir / f).resolve():
            shutil.copy2(HERE / f, bin_dir / f)
    if not IDEAS.exists():
        IDEAS.write_text(
            "# Pulse ideas\n\nOne line each. The night team builds from the top.\n"
            "`[x]` = built (merged, or waiting in the pull request named on the line), "
            "`[?]` = it stopped and says why; fix the line and set it back to `[ ]`.\n\n"
        )
    tools = [
        "claude",
        "gh",
        "pnpm",
        "node",
        "cargo",
        "rustc",
        "git",
        "sqlite3",
        "python3",
    ]
    missing = [t for t in tools if not shutil.which(t)]
    if missing:
        print(f"missing tools: {', '.join(missing)}")
        return 1
    dirs = list(
        dict.fromkeys(
            [str(Path(shutil.which(t)).parent) for t in tools]
            + ["/usr/bin", "/bin", "/usr/sbin", "/sbin"]
        )
    )
    plist = {
        "Label": PLIST_LABEL,
        "ProgramArguments": [shutil.which("python3"), str(bin_dir / "team.py"), "run"],
        "StartCalendarInterval": {"Hour": RUN_AT[0], "Minute": RUN_AT[1]},
        "StandardOutPath": str(HOME / "launchd.log"),
        "StandardErrorPath": str(HOME / "launchd.log"),
        "EnvironmentVariables": {
            "PATH": ":".join(dirs),
            "HOME": str(Path.home()),
            "PULSE_TEAM_HOME": str(HOME),
            "PULSE_TEAM_REPO": str(REPO),
        },
    }
    PLIST.parent.mkdir(parents=True, exist_ok=True)
    PLIST.write_bytes(plistlib.dumps(plist))
    domain = f"gui/{os.getuid()}"
    sh(["launchctl", "bootout", f"{domain}/{PLIST_LABEL}"])
    code, out, err = sh(["launchctl", "bootstrap", domain, str(PLIST)])
    if code != 0:
        print(f"launchctl failed: {(out + err).strip()}")
        return 1
    print(
        f"Installed. Runs every night at {RUN_AT[0]:02d}:{RUN_AT[1]:02d} from {bin_dir}. Ideas: {IDEAS}"
    )
    return 0


def cmd_uninstall(_args) -> int:
    sh(["launchctl", "bootout", f"gui/{os.getuid()}/{PLIST_LABEL}"])
    PLIST.unlink(missing_ok=True)
    print(f"Removed the nightly job. {HOME} (ideas, reports) is left in place.")
    return 0


def cmd_status(_args) -> int:
    latest = HOME / "latest.txt"
    print(latest.read_text() if latest.exists() else "The night team has not run yet.")
    return 0


def cmd_idea(args) -> int:
    HOME.mkdir(parents=True, exist_ok=True)
    text = " ".join(args.text).strip()
    with open(IDEAS, "a") as f:
        f.write(f"- [ ] {text}\n")
    print(f"Added to {IDEAS}")
    return 0


def cmd_page(args) -> int:
    """Remember the artifact the nightly page is published to."""
    HOME.mkdir(parents=True, exist_ok=True)
    PAGE_URL.write_text(args.url.strip() + "\n")
    print(f"The nightly page will be published to {args.url}")
    return 0


def main() -> int:
    ap = argparse.ArgumentParser(description="The Pulse night team")
    sub = ap.add_subparsers(dest="cmd", required=True)
    r = sub.add_parser("run")
    r.add_argument("--only", default="", help="main | pr-<n> | idea")
    r.add_argument("--max-jobs", type=int, default=MAX_JOBS)
    r.add_argument(
        "--no-merge", action="store_true", help="do everything except the final merge"
    )
    r.set_defaults(fn=cmd_run)
    for name, fn in (
        ("install", cmd_install),
        ("uninstall", cmd_uninstall),
        ("status", cmd_status),
    ):
        sub.add_parser(name).set_defaults(fn=fn)
    i = sub.add_parser("idea")
    i.add_argument("text", nargs="+")
    i.set_defaults(fn=cmd_idea)
    g = sub.add_parser("page")
    g.add_argument("url")
    g.set_defaults(fn=cmd_page)
    args = ap.parse_args()
    return args.fn(args)


if __name__ == "__main__":
    sys.exit(main())
