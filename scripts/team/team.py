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
import shlex
import shutil
import signal
import subprocess
import sys
import tempfile
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
JOB_MIN = _int("PULSE_TEAM_JOB_MIN", 180)  # one job starts no new pass after this
MAX_FIX_ROUNDS = _int("PULSE_TEAM_FIX_ROUNDS", 2)
MAX_MAIN_MOVES = 2  # times a job starts over because main changed under it
MODEL = os.environ.get("PULSE_TEAM_MODEL", "")  # empty = the default from settings
RUN_AT = (_int("PULSE_TEAM_HOUR", 2), _int("PULSE_TEAM_MINUTE", 30))  # used by install

# Sessions get placeholders for every key Pulse knows, so nothing they build or
# run can trade or spend. (dotenvy never overrides a variable that is already
# set, so the real .env files lose.) ANTHROPIC_API_KEY is left out on purpose:
# `claude` itself would try to log in with it. PULSE_LLM=local and the
# placeholder .env in every worktree (write_placeholder_env) cover that one.
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
    # The fetcher refuses the installed app's database when this is set,
    # whatever build it is and however it was started.
    "PULSE_NO_LIVE_DB": "1",
    "TYPESAFE_API_KEY": "",  # an opt-in service: set but empty means off
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
3. New behaviour without tests, and tests that cannot fail. A change that makes the checks themselves weaker (tests deleted, skipped or ignored, lint or test settings loosened) is a `blocker` unless the job cannot be done any other way.
4. If it edits CLAUDE.md or other instructions for Claude sessions: it may only describe the code. Anything that changes how sessions behave or loosens a rule is a `blocker`.
5. Run the tests that matter and, where it is safe, see the change working for real (the run-pulse skill explains how). List what you actually ran in `verified` and what you could not check in `not_verified`.
6. List new dependencies in `new_dependencies`.

Do not edit or commit anything. Verdict `fail` if there is any `blocker` or `major` finding; `minor` findings alone pass. Be concrete: file, line, what goes wrong and with which input.
`summary`: two or three plain sentences, for a non-developer, on what this change does. `try_it`: steps Nicola can follow to see it."""


# ---------------------------------------------------------------- plumbing --


def sh(
    cmd: list[str],
    cwd: Path | None = None,
    timeout: int = 600,
    env: dict | None = None,
    reap: bool = False,
) -> tuple[int, str, str]:
    """Run a command. Its output goes to files, not pipes, so something it left
    running in the background (a dev server) cannot hold the call open after the
    command itself has ended. On timeout the whole process group is killed
    (code 124); with `reap` the group is also cleared when the command ends.
    A command that cannot be started at all gives code 127, like in a shell."""
    kw = {"encoding": "utf-8", "errors": "replace"}
    with (
        tempfile.TemporaryFile("w+", **kw) as out,
        tempfile.TemporaryFile("w+", **kw) as err,
    ):
        try:
            p = subprocess.Popen(
                cmd,
                cwd=cwd,
                env=env,
                stdin=subprocess.DEVNULL,
                stdout=out,
                stderr=err,
                start_new_session=True,
            )
        except OSError as e:  # the program or the folder is not there
            return 127, "", f"could not start {cmd[0]}: {e}"
        note = ""
        try:
            code = p.wait(timeout=timeout)
        except subprocess.TimeoutExpired:
            code, note = 124, f"\n[timed out after {timeout // 60} min]"
        if note or reap:
            try:
                os.killpg(p.pid, signal.SIGKILL)
            except (ProcessLookupError, PermissionError):
                pass
            p.wait()
        out.seek(0)
        err.seek(0)
        return code, out.read(), err.read() + note


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


def night_env() -> dict:
    """The environment of every unattended session: no trading, no real keys,
    no way to publish, a database copy instead of the live one."""
    env = dict(os.environ)
    env.update(SAFE_ENV)
    for key in PLACEHOLDER_KEYS:
        env[key] = "night-team-placeholder"
    # The runner's own settings stay with the runner: a session's shell gets no
    # variable that points at the main checkout or the team's folder.
    for key in ("ANTHROPIC_API_KEY", "PULSE_TEAM_HOME", "PULSE_TEAM_REPO"):
        env.pop(key, None)
    env["CARGO_TARGET_DIR"] = str(HOME / "target")
    env["PULSE_TEAM_CHECKS"] = str(HERE / "checks.sh")
    # Always set, even when there is no copy: the dev app and a debug build of
    # the fetcher then open an empty database there, never the live one.
    env["PULSE_DB_PATH"] = str(DB_JOB)
    return env


def brief() -> str:
    text = (HERE / "TEAM.md").read_text()
    return text.split("# Brief for sessions", 1)[1].strip()


def guard_settings(worktree: Path) -> dict:
    """guard.py as a PreToolUse hook on every tool call of a session that may
    write only inside `worktree`."""
    parts = ["python3", str(HERE / "guard.py"), str(REPO), str(worktree)]
    hook = {
        "type": "command",
        "command": " ".join(shlex.quote(part) for part in parts),
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
        json.dumps(guard_settings(cwd)),
        "--strict-mcp-config",  # no connectors: a coding session needs no mail or drive
        "-n",
        name,
    ]
    if MODEL:
        cmd += ["--model", MODEL]
    code, out, err = sh(cmd, cwd=cwd, timeout=minutes * 60, env=night_env(), reap=True)
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


def exists(branch: str) -> bool:
    return git("rev-parse", "-q", "--verify", f"refs/heads/{branch}")[0] == 0


def on_github(branch: str) -> bool:
    """True when every commit on this local branch is on GitHub as well: in
    main, or on the pushed branch of the same name. (Goes by what the last
    fetch or push saw.)"""
    for upstream in (f"origin/{BASE}", f"origin/{branch}"):
        code, out = git("rev-list", "--count", f"{upstream}..refs/heads/{branch}")
        if code == 0 and out == "0":
            return True
    return False


def stamp() -> str:
    return dt.datetime.now().strftime("%m%d-%H%M%S")


def make_worktree(ref: str, branch: str | None) -> Path:
    """A fresh worktree for the job at hand, always at the same path. One job
    runs at a time, and Rust compiles paths into what it builds (where the
    fetcher looks for `.env`, where tests find their fixtures). The build folder
    is shared between jobs, so a path that changed from job to job could leave
    a binary that points into a worktree that is gone."""
    path = REPO / ".claude/worktrees/team-job"
    if path.exists():
        git("worktree", "remove", "--force", str(path))
    git("worktree", "prune")
    if branch and exists(branch) and not on_github(branch):
        # An earlier attempt that was kept on this Mac under the same name (a
        # second run on one day). `-B` below would start over on top of it.
        git("branch", "-M", branch, f"night-kept/{branch}-{stamp()}")
    args = (
        ["worktree", "add", "-B", branch, str(path), ref]
        if branch
        else ["worktree", "add", "--detach", str(path), ref]
    )
    code, out = git(*args)
    if code != 0:
        raise RuntimeError(f"could not create worktree: {out[-300:]}")
    write_placeholder_env(path)
    return path


def write_placeholder_env(wt: Path) -> None:
    """A fetcher or dev app built in a checkout loads the first `.env` it finds,
    and its own checkout's comes before the installed app's (which holds the
    real keys). Give every team worktree one with placeholders only."""
    keys = [*PLACEHOLDER_KEYS, "ANTHROPIC_API_KEY"]
    env = wt / ".env"
    env.write_text(
        "# Written by the night team: placeholders only.\n"
        + "".join(f"{key}=night-team-placeholder\n" for key in keys)
    )
    if git("check-ignore", "-q", ".env", cwd=wt)[0] != 0:
        env.unlink()  # not ignored on this branch: it would count as an uncommitted change


def run_checks(wt: Path, run: dict, name: str) -> tuple[bool, str, str]:
    """checks.sh from the installed copy. Returns (ok, failed step, end of output)."""
    # The checks run the code under review, so they cannot publish either.
    env = dict(
        os.environ,
        CARGO_TARGET_DIR=str(HOME / "target"),
        PULSE_NO_LIVE_DB="1",
        **NO_PUBLISH,
    )
    code, out, err = sh(
        ["bash", str(HERE / "checks.sh")],
        cwd=wt,
        timeout=CHECKS_MIN * 60,
        env=env,
        reap=True,
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
    reasons = rules.public_reasons(res["reasons"], str(Path.home()))
    lines = "\n".join(f"- {r[:600]}" for r in reasons[:12])
    gh(
        "pr",
        "comment",
        str(res["pr"]),
        "--body",
        f"The night team did not merge this:\n\n{lines or '- it stopped without giving a reason'}\n\n"
        f"Remove the `{LABEL}` label or push a new commit and it will try again.",
    )


def merge_ready(pr: int, sha: str) -> str:
    """'' when the PR is still the one that was checked and GitHub will merge
    exactly this commit now, else why not."""
    fields = "mergeable,headRefOid,state,baseRefName,isDraft,labels"
    view = {}
    for _ in range(6):  # GitHub works `mergeable` out lazily: UNKNOWN at first
        try:
            view = gh_json("pr", "view", str(pr), "--json", fields)
        except (RuntimeError, ValueError):
            view = {}
        if view.get("mergeable") in ("MERGEABLE", "CONFLICTING"):
            break
        time.sleep(10)
    return rules.merge_blockers(view, sha, BASE, LABEL)


def fetch_base() -> str:
    """Fetch, then name main by its commit. A job compares against this commit,
    not the branch name, so it always knows which main it tested against and
    can tell when main has moved on."""
    if git("fetch", "origin", "--prune")[0] != 0:
        raise RuntimeError("could not reach GitHub")
    code, sha = git("rev-parse", "--verify", f"origin/{BASE}^{{commit}}")
    if code != 0:
        raise RuntimeError(f"origin/{BASE} was not found")
    return sha


def gate_violations(wt: Path, base: str, tip: str = "HEAD") -> list[str]:
    """What in the change between `base` and `tip` needs Nicola (rules.py
    decides). Raises when git cannot answer, so a failure never reads as
    'nothing found'."""

    def must(*args: str) -> str:
        code, out = git(*args, cwd=wt)
        if code != 0:
            raise RuntimeError(f"git {args[0]} failed: {out[-200:]}")
        return out

    def package(rev: str) -> str:
        code, out = git("show", f"{rev}:package.json", cwd=wt)
        return out if code == 0 else ""

    fork = must("merge-base", base, tip)
    changes = rules.parse_name_status(must("diff", "--name-status", "-z", fork, tip))
    return (
        rules.protected_violations(changes)
        # --text and the two --no- options: a .gitattributes entry or a diff
        # driver in the change must not be able to hide a line from this scan.
        + rules.forbidden_content(
            must("diff", "--text", "--no-ext-diff", "--no-textconv", fork, tip)
        )
        + rules.script_violations(package(fork), package(tip))
    )


def merge_main(wt: Path, base: str, branch: str) -> bool:
    """Merge main into the job's branch without a session. False = it conflicts."""
    message = f"Merge {BASE} into {branch}"
    if git("merge", "--no-edit", "-m", message, base, cwd=wt)[0] == 0:
        return True
    git("merge", "--abort", cwd=wt)
    return False


def keep_attempt(job: dict, res: dict, wt: Path, branch: str) -> None:
    """An unfinished attempt on someone's pull request is never pushed onto
    their branch. It stays on this Mac under a name of its own, so an earlier
    night's attempt is never overwritten."""
    if head(wt) == git("rev-parse", f"origin/{branch}", cwd=wt)[1]:
        return
    kept = f"night-kept/pr-{job['pr']}-{stamp()}"
    if git("branch", kept, "HEAD", cwd=wt)[0] == 0:
        res["reasons"].append(
            f"the team's unfinished attempt is kept on this Mac as branch {kept}"
        )


def keep_work(
    job: dict,
    res: dict,
    wt: Path,
    branch: str,
    base: str,
    title: str,
    body: str,
    built: bool,
    violations: list[str] | None = None,
) -> None:
    """A job that stops keeps the commits its sessions made, so the work and the
    reason are in one place. The team's own branch gets a pull request, unless
    it holds something that may not leave this Mac (then the branch stays here;
    run_job's cleanup never deletes one that is not on GitHub). For someone's
    pull request see keep_attempt."""
    try:
        ahead = git("rev-list", "--count", f"{base}..HEAD", cwd=wt)[1]
        if not built or ahead in ("", "0"):
            return
        if job["kind"] == "pr":
            keep_attempt(job, res, wt, branch)
            return
        listed = violations is not None  # the caller has already given the reasons
        if violations is None:
            violations = gate_violations(wt, base)
        if violations:
            why = (
                "it was not pushed"
                if listed
                else f"not pushed, because it {violations[0]}"
            )
            res["reasons"].append(
                f"{why}; the work is kept on this Mac as branch {branch}"
            )
            return
        err = publish(wt, branch, res, title, body)
        if err:
            res["reasons"].append(err)
    except RuntimeError as e:
        res["reasons"].append(f"could not keep the work: {e}")


def land(
    job: dict,
    res: dict,
    run: dict,
    wt: Path,
    branch: str,
    base: str,
    sha: str,
    title: str,
    body: str,
    hands_off: bool,
    built: bool,
) -> str:
    """The checks and the review passed on `sha`, which has `base` merged in.
    Apply the rest of the gate, publish, wait for CI and merge. Returns '' when
    the job is over (`res` says how), or main's new commit when main moved
    meanwhile and the job has to go round again."""
    if git("diff", "--quiet", base, sha, cwd=wt)[0] == 0:
        res["result"] = "blocked"
        res["reasons"].append(
            f"nothing to merge: the branch has no changes compared with {BASE}"
        )
        return ""
    # Looked at before anything is pushed: a pushed CI file runs on GitHub with
    # the repository's secrets whether or not it is ever merged.
    violations = gate_violations(wt, base, sha)
    if hands_off:
        res["result"] = "needs_user"
        res["reasons"].append(
            "the checks pass and the reviewer has no objection, so merging it is your call"
        )
        return ""
    if violations:
        res["result"] = "needs_user"
        res["reasons"] += violations
        keep_work(job, res, wt, branch, base, title, body, built, violations)
        return ""
    now = fetch_base()
    if now != base:
        return now
    err = publish(wt, branch, res, title, body)
    if err:
        res["result"] = "error"
        res["reasons"].append(err)
        if job["kind"] == "pr" and built:
            keep_attempt(job, res, wt, branch)
        return ""
    ci = wait_ci(res["pr"], sha)
    if ci != "pass":
        why = {
            "fail": f"GitHub's checks failed on {sha[:7]}",
            "none": "GitHub's checks never started",
            "timeout": f"GitHub's checks did not finish in {CI_MIN} min",
        }[ci]
        res["result"] = "blocked" if ci == "fail" else "error"
        res["reasons"].append(why)
        return ""
    why = merge_ready(res["pr"], sha)
    if why:
        res["result"] = "error"
        res["reasons"].append(why)
        return ""
    now = fetch_base()  # CI can take a while: is this still the main that was tested?
    if now != base:
        return now
    if run["no_merge"]:
        res["result"] = "ready"
        return ""
    code, out = gh("pr", "merge", str(res["pr"]), "--merge", "--match-head-commit", sha)
    if code != 0:
        res["result"] = "error"
        res["reasons"].append(f"merge failed: {out[-200:]}")
        return ""
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
    return ""


def do_job(job: dict, res: dict, run: dict) -> None:
    kind, name = job["kind"], job["key"]
    began = time.time()
    base = fetch_base()
    reset_job_db()
    title, body = job["title"], ""
    hands_off = built = False
    if kind == "pr":
        branch = job["head"]
        request = f'PR #{job["pr"]} "{job["title"]}"\n\n{job["body"][:3000]}'
        if job.get("labelled"):  # a new commit lifted the stop
            gh("pr", "edit", str(job["pr"]), "--remove-label", LABEL)
        wt = make_worktree(f"origin/{branch}", None)
        res["wt"] = str(wt)
        # A PR that already touches protected files is its owner's to merge. It
        # is still checked and reviewed, but the team changes and pushes nothing.
        res["reasons"] += gate_violations(wt, base)
        hands_off = bool(res["reasons"])
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
        wt = make_worktree(base, branch)
        res["wt"], res["branch"] = str(wt), branch
        b = session(prompt, wt, BUILDER_SCHEMA, BUILDER_MIN, f"{name}-build", run)
        built = True
        title = b["data"].get("pr_title") or title
        body = b["data"].get("pr_body") or ""
        res["summary"] = b["data"].get("summary", "")
        if not finished(b, res):
            keep_work(job, res, wt, branch, base, title, body, built)
            return

    rounds = moved = passes = 0
    while True:
        passes += 1
        if time.time() - began > JOB_MIN * 60:
            res["result"] = "error"
            res["reasons"].append(
                f"took more than {JOB_MIN} min, the limit for one job; it will be tried again"
            )
            keep_work(job, res, wt, branch, base, title, body, built)
            return
        # `problems` goes to the next fix session, `short` on the page.
        if git("status", "--porcelain", cwd=wt)[1]:
            problems = "You left uncommitted changes. Commit what belongs to the change and remove the rest."
            short = "the builder left uncommitted changes"
        elif git("merge-base", "--is-ancestor", base, "HEAD", cwd=wt)[0] != 0:
            # Checked here, not taken on a session's word. The runner merges
            # main in by itself; a session is only needed for conflicts.
            if merge_main(wt, base, branch):
                continue
            problems, short = MERGE_MAIN, f"it conflicts with the latest {BASE}"
        else:
            sha = head(wt)
            ok, step, tail = run_checks(wt, run, f"{name}-{passes}")
            # The checks run the change's own code. Whatever that did to the
            # checkout is dropped: what is reviewed and merged is the commit
            # that was checked.
            git("reset", "--hard", sha, cwd=wt)
            git("clean", "-fd", cwd=wt)
            if not ok:
                problems = f'The check "{step}" fails. The end of its output:\n\n```\n{tail}\n```'
                short = f'the check "{step}" fails'
            else:
                r = session(
                    REVIEW.format(request=request, base=BASE),
                    wt,
                    REVIEW_SCHEMA,
                    REVIEWER_MIN,
                    f"{name}-review{passes}",
                    run,
                )
                # A reviewer must not change what it reviewed.
                git("reset", "--hard", sha, cwd=wt)
                git("clean", "-fd", cwd=wt)
                if not r["ok"]:
                    res["result"] = "error"
                    res["reasons"].append(f"the review session failed: {r['error']}")
                    keep_work(job, res, wt, branch, base, title, body, built)
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
                    new_base = land(
                        job,
                        res,
                        run,
                        wt,
                        branch,
                        base,
                        sha,
                        title,
                        body,
                        hands_off,
                        built,
                    )
                    if not new_base:
                        return
                    moved += 1
                    if moved > MAX_MAIN_MOVES:
                        res["result"] = "error"
                        res["reasons"].append(
                            f"{BASE} kept changing while this job ran; it will be tried again"
                        )
                        keep_work(job, res, wt, branch, base, title, body, built)
                        return
                    base = (
                        new_base  # merge the new main in, then check and review again
                    )
                    continue
                found = bad or [
                    {"problem": review.get("summary", "the reviewer did not pass it")}
                ]
                res["findings"] = [
                    f"{f.get('where', '')} {f['problem']}".strip() for f in found
                ]
                problems = "An independent reviewer found:\n\n" + "\n".join(
                    f"- {x}" for x in res["findings"]
                )
                short = f"the reviewer objects: {res['findings'][0]}"
        more = res["findings"][1:] if short.startswith("the reviewer") else []
        if hands_off:
            res["result"] = "needs_user"
            res["reasons"] += [short, *more]
            return
        if rounds >= MAX_FIX_ROUNDS:
            res["result"] = "blocked"
            res["reasons"] += [
                f"still not right after {rounds} fix rounds: {short}",
                *more,
            ]
            keep_work(job, res, wt, branch, base, title, body, built)
            return
        rounds += 1
        built = True
        fix = FIX_CHANGE.format(request=request, problems=problems)
        if not finished(
            session(fix, wt, BUILDER_SCHEMA, BUILDER_MIN, f"{name}-fix{rounds}", run),
            res,
        ):
            keep_work(job, res, wt, branch, base, title, body, built)
            return


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
        if branch and exists(branch):
            # The team's own local branch goes too, but only when GitHub has
            # every commit on it. An earlier push says nothing about commits
            # made after it.
            git("fetch", "origin", "--prune")  # a push or a merge just changed GitHub
            if on_github(branch):
                git("branch", "-D", branch)
            elif res["result"] != "merged" and not any(
                branch in r for r in res["reasons"]
            ):
                res["reasons"].append(
                    f"the work is kept on this Mac as branch {branch}"
                )
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
                state,
                key,
                sha,
                result,
                parked=bool(res.get("parked")),
                prev_sha=job.get("sha") or "",
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


PR_FIELDS = (
    "number,title,body,author,isCrossRepository,isDraft,baseRefName,headRefName,"
    "headRefOid,labels,createdAt,state"
)


def pr_now(job: dict, owner: str, state: dict) -> str | None:
    """A last look at a pull request right before its job: the night's list can
    be hours old by then. Returns why the team leaves it alone now, or None
    after bringing the job up to date with what GitHub has."""
    try:
        pr = gh_json("pr", "view", str(job["pr"]), "--json", PR_FIELDS)
    except (RuntimeError, ValueError):
        return "GitHub could not be asked about it"
    if pr.get("state") != "OPEN":
        return f"no longer open ({str(pr.get('state') or 'unknown').lower()})"
    pr["busy"] = local_work(pr.get("headRefName") or "")
    why = rules.pr_skip_reason(pr, owner, BASE, LABEL, state)
    if not why:
        job.update(rules.pr_job(pr, LABEL))
    return why


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
        elif line == f"branch refs/heads/{branch}" and Path(path).is_dir():
            # (a worktree whose folder was deleted is still listed until pruned)
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
    prs = gh_json("pr", "list", "--state", "open", "--limit", "50", "--json", PR_FIELDS)
    for pr in prs:
        pr["busy"] = local_work(pr.get("headRefName") or "")
    ideas = rules.parse_ideas(IDEAS.read_text()) if IDEAS.exists() else []

    main_sha = git("rev-parse", f"origin/{BASE}")[1]
    main_ok = state.get("main_ok_sha") == main_sha
    if not main_ok:
        log(f"checking {BASE} ({main_sha[:7]})")
        wt = make_worktree(f"origin/{BASE}", None)
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

    if re.fullmatch(r"pr-\d+", args.only or ""):
        rules.retry_now(state, args.only)  # asked for by name: forget its last stop
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
        if job["kind"] == "pr":
            why = pr_now(job, owner, state)
            if why:
                log(f"   -> skipped: {why}")
                report["skipped"].append(
                    {"kind": "pr", "pr": job["pr"], "title": job["title"], "why": why}
                )
                continue
        res = run_job(job, run)
        report["jobs"].append(res)
        try:
            after_job(job, res, state)
        except Exception as e:  # noqa: BLE001 - the job is done; its result stays in the report
            report["notes"].append(f"could not record the outcome of {job['key']}: {e}")
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
            json.dumps(
                guard_settings(html.parent)
            ),  # it may write only in the page folder
            "-n",
            "pulse-team-page",
            prompt,
        ],
        cwd=Path.home(),
        timeout=120,
        env=night_env(),
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
