#!/usr/bin/env python3
"""PreToolUse hook for night-team sessions (it sees every tool call).

Unattended sessions build and review inside one worktree. This hook blocks the
few things they must never do there, whatever the prompt or a web page says:
publish anything (the runner pushes and merges, after its own gate), run the
fetcher on the live database, read secrets, use the mail/calendar/drive
connectors, or touch what runs on this Mac.

It is one of several layers, not the only one: the runner also starts sessions
with an environment in which `git push` and `gh` cannot reach GitHub, and with
placeholder API keys (see `night_env` in team.py).

Exit 2 blocks the tool call and shows the reason to the session.
"""

from __future__ import annotations

import json
import os
import re
import sys

# A command word can follow the start, white space, a shell separator, a quote
# (bash -c "git push") or a slash (/usr/bin/git push).
WORD = r"(?:^|[\s;&|(`'\"/])"

# `gh` is read-only for sessions. Anything not listed here is blocked.
GH_ALLOWED = {
    ("pr", "view"),
    ("pr", "diff"),
    ("pr", "checks"),
    ("pr", "list"),
    ("pr", "status"),
    ("run", "view"),
    ("run", "list"),
    ("run", "watch"),
    ("issue", "view"),
    ("issue", "list"),
}
GH_CALL = re.compile(WORD + r"gh\s+([a-z-]+)(?:\s+([a-z-]+))?")
# `git push` with any global options before it; `git stash push` is local and fine.
GIT_PUSH = re.compile(
    WORD + r"git(?:\s+-[cC]\s+\S+|\s+-{1,2}[\w-]+(?:=\S+)?)*\s+push\b"
)
SECRET_FILE = re.compile(r"(?<![\w])\.env(?![\w])(?!\.example)")
IN_A_MESSAGE = " (if those words are only inside a commit message, reword it)"
NEVER = [
    (
        re.compile(r"com\.pulse\.app|Mobile\\? Documents"),
        "the installed app's data and the phone snapshot are off limits; use the copy in $PULSE_DB_PATH",
    ),
    (
        re.compile(WORD + r"(?:launchctl|sudo|osascript|crontab)\b"),
        "launchctl, sudo, osascript and crontab are not for unattended sessions",
    ),
    (
        re.compile(r"install-app\.sh|install-launchd\.sh"),
        "installing onto this Mac needs Nicola",
    ),
    (
        re.compile(
            r"\.ssh/|\.aws/|\.gnupg/|\.tauri/|pulse-updater|\.config/gh|Keychains"
        ),
        "keys and credentials are off limits",
    ),
    (
        re.compile(r"\.claude/settings"),
        "Claude's settings are off limits for unattended sessions",
    ),
]
TMP_ROOTS = ("/tmp/", "/private/tmp/", "/var/folders/", "/private/var/folders/")
SECRET_DIRS = (".ssh", ".aws", ".gnupg", ".tauri", ".config/gh", "Library/Keychains")
# Tools that schedule or start work outside this session.
BLOCKED_TOOLS = {"CronCreate", "CronDelete", "RemoteTrigger"}
WRITE_TOOLS = {"Edit", "Write", "MultiEdit", "NotebookEdit"}
READ_TOOLS = {"Read", "Grep", "Glob"}


def bash_block_reason(cmd: str, repo: str) -> str | None:
    if GIT_PUSH.search(cmd):
        return "commit only; the runner pushes after its checks" + IN_A_MESSAGE
    for m in GH_CALL.finditer(cmd):
        if (m.group(1), m.group(2) or "") not in GH_ALLOWED:
            call = f"gh {m.group(1)} {m.group(2) or ''}".strip()
            return (
                f"`{call}` is not allowed here; gh is read-only for sessions"
                + IN_A_MESSAGE
            )
    if "pulse-fetcher" in cmd and "--mode" in cmd and "--db-path" not in cmd:
        return 'run the fetcher only with --db-path "$PULSE_DB_PATH" (never the live database)'
    if SECRET_FILE.search(cmd):
        return ".env files hold real keys and are off limits"
    for pattern, reason in NEVER:
        if pattern.search(cmd):
            return reason
    if repo:
        # The main checkout is Nicola's. Sessions stay inside <repo>/.claude/worktrees/.
        home = os.path.expanduser("~")
        forms = {repo}
        if repo.startswith(home):
            forms |= {"~" + repo[len(home) :], "$HOME" + repo[len(home) :]}
        for form in forms:
            if re.search(re.escape(form) + r"(?!/\.claude/worktrees/)(?![\w.-])", cmd):
                return "work only inside your worktree, not the main checkout"
    return None


def _full(path: str, cwd: str) -> str:
    path = os.path.expanduser(path)
    return os.path.realpath(path if os.path.isabs(path) else os.path.join(cwd, path))


def _is_env_file(full: str) -> bool:
    name = os.path.basename(full)
    return name == ".env" or (name.startswith(".env.") and name != ".env.example")


def _inside(full: str, root: str) -> bool:
    return full == root or full.startswith(root + os.sep)


def read_block_reason(path: str, cwd: str, repo: str) -> str | None:
    """Read, Grep and Glob: no secrets, no live app data, not the main checkout."""
    if not path:
        return None
    full = _full(path, cwd)
    if _is_env_file(full):
        return ".env files hold real keys and are off limits"
    if "/com.pulse.app" in full or "/Mobile Documents" in full:
        return "the installed app's data and the phone snapshot are off limits; use the copy in $PULSE_DB_PATH"
    home = os.path.realpath(os.path.expanduser("~"))
    if any(_inside(full, os.path.join(home, d)) for d in SECRET_DIRS):
        return "keys and credentials are off limits"
    if repo:
        root = os.path.realpath(repo)
        worktrees = os.path.join(root, ".claude", "worktrees")
        if _inside(full, root) and not _inside(full, worktrees):
            return "work only inside your worktree, not the main checkout"
    return None


def write_block_reason(path: str, cwd: str, worktree: str = "") -> str | None:
    """`worktree` is the session's root; `cwd` (where a relative path starts)
    may be a folder inside it."""
    if not path:
        return None
    full = _full(path, cwd)
    if _is_env_file(full):
        return ".env files are off limits"
    root = os.path.realpath(worktree or cwd)
    if _inside(full, os.path.join(root, ".claude")):
        # Settings, hooks and skills here are loaded and run by the next session.
        return "the .claude folder is off limits for unattended sessions"
    if _inside(full, root) or full.startswith(TMP_ROOTS):
        return None
    return f"write only inside your worktree ({root}) or a temp folder"


def decide(event: dict, repo: str, worktree: str = "") -> str | None:
    tool = event.get("tool_name") or ""
    args = event.get("tool_input") or {}
    cwd = event.get("cwd") or os.getcwd()
    if tool.startswith("mcp__"):
        return "connectors (mail, calendar, drive and the rest) are not for unattended sessions"
    if tool in BLOCKED_TOOLS:
        return f"{tool} is not for unattended sessions"
    if tool == "Bash":
        return bash_block_reason(str(args.get("command") or ""), repo)
    if tool in WRITE_TOOLS:
        path = args.get("file_path") or args.get("notebook_path") or ""
        return write_block_reason(str(path), cwd, worktree)
    if tool in READ_TOOLS:
        path = args.get("file_path") or args.get("path") or ""
        return read_block_reason(str(path), cwd, repo)
    return None


def main() -> int:
    try:
        event = json.load(sys.stdin)
    except ValueError:
        print("night-team guard: unreadable hook input", file=sys.stderr)
        return 2  # fail closed
    reason = decide(
        event,
        os.environ.get("PULSE_TEAM_REPO", ""),
        os.environ.get("PULSE_TEAM_WORKTREE", ""),
    )
    if reason:
        print(f"Blocked by the night-team guard: {reason}.", file=sys.stderr)
        return 2
    return 0


if __name__ == "__main__":
    sys.exit(main())
