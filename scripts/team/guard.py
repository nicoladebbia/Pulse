#!/usr/bin/env python3
"""PreToolUse hook for night-team sessions (it sees every tool call).

Unattended sessions build and review inside one worktree. This hook blocks the
few things they must never do there, whatever the prompt or a web page says:
publish anything (the runner pushes and merges, after its own gate), run the
fetcher on the live database, read secrets, use the mail/calendar/drive
connectors, or touch what runs on this Mac.

It is one of several layers, not the only one: the runner also starts sessions
with an environment in which `git push` and `gh` cannot reach GitHub, with
placeholder API keys and a placeholder `.env` in the worktree (see `night_env`
and `write_placeholder_env` in team.py), a debug build of the fetcher follows
`PULSE_DB_PATH` to the copy, and `PULSE_NO_LIVE_DB` makes every Pulse binary
refuse the live database. A shell command can be spelled in ways no
pattern sees, which is why none of these layers stands alone.

    guard.py <main checkout> <session's worktree>

Exit 2 blocks the tool call and shows the reason to the session.
"""

from __future__ import annotations

import json
import os
import re
import shlex
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
    ("--version", ""),
    ("--help", ""),
}
GH_CALL = re.compile(WORD + r"gh\s+([a-z-]+)(?:\s+([a-z-]+))?")
# `git`, then any global options (`-C dir`, `--no-pager`), then the subcommand.
GIT = WORD + r"git(?:\s+-[cC]\s+\S+|\s+-{1,2}[\w-]+(?:=\S+)?)*\s+"
GIT_PUSH = re.compile(GIT + r"push\b")
# Every worktree shares one stash with Nicola's checkout; looking is fine.
GIT_STASH = re.compile(GIT + r"stash\b(?!\s+(?:list|show)\b)")
# Paths are matched without regard to case: on this Mac's disk `Com.Pulse.App`
# and `.ENV` are the same folder and file as their lower-case spellings.
SECRET_FILE = re.compile(r"(?<![\w])\.env(?![\w])(?!\.example)", re.I)
# The patterns see words, not meaning, so a search or a commit message that only
# mentions one is blocked too. Every Bash refusal says how to get past that.
REWORD = " (if those words are only inside a commit message or a search, reword it or use the Grep tool)"

# These open the live database unless told otherwise. The copy made for the app
# bundle carries a target suffix (pulse-fetcher-aarch64-apple-darwin).
PULSE_BINARY = re.compile(
    r"^pulse-(?:fetcher|eval|backtest)(?:-(?:aarch64|x86_64|universal)[\w.-]*)?$"
)
SEGMENT = re.compile(r"\|\||&&|[;|&\n()`]|\$\(")
ASSIGNMENT = re.compile(r"^[A-Za-z_]\w*=")
# Words that can stand before the command a shell actually runs (also with a
# path in front: /usr/bin/env), and what can follow them before that command:
# options, an option's value (`timeout -s KILL`, `env -u NAME`, `xargs -I {}`),
# a number or a duration (`timeout 30s`, `nice -n 5`).
BEFORE_COMMAND = {
    "time", "nohup", "exec", "env", "command", "caffeinate", "nice", "timeout",
    "gtimeout", "xargs", "then", "do", "else", "if", "while", "!", "{",
}  # fmt: skip
TAKES_VALUE = {"-s", "--signal", "-u", "--unset", "-I", "-J", "-C", "--chdir", "-a"}
DURATION = re.compile(r"^\d+(?:\.\d+)?[smhd]?$")
SHELLS = {"bash", "sh", "zsh", "dash", "ksh"}
SHELL_SCRIPT_FLAG = re.compile(r"^-[A-Za-z]*c[A-Za-z]*$")  # -c, -lc, -ic
CARGO_RUN = {"run", "r"}
NODE_TOOLS = {"npm", "pnpm", "yarn"}
GLOBAL_FLAGS = {"-g", "--global", "--location=global"}
CARGO_TAKES_VALUE = {"--color", "--config", "-C", "-Z"}
ONLY_PRINTS = {"--help", "-h", "--version", "-V"}
REDIRECT = re.compile(r"^\d*[<>]")
NEVER = [
    (
        re.compile(r"com\.pulse\.app|Mobile\\? Documents", re.I),
        "the installed app's data and the phone snapshot are off limits; use the copy in $PULSE_DB_PATH",
    ),
    (
        re.compile(WORD + r"(?:launchctl|sudo|osascript|crontab)\b"),
        "launchctl, sudo, osascript and crontab are not for unattended sessions",
    ),
    (
        re.compile(
            WORD
            + r"(?:brew\s+(?:install|uninstall|reinstall|upgrade|tap)|cargo\s+install"
            r"|rustup\s+(?:default|update|self|toolchain)|security\s+(?:[a-z]+-[a-z-]+|export|import))\b"
        ),
        "installing onto this Mac, changing its toolchain and the keychain are not for unattended sessions",
    ),
    (
        re.compile(r"install-app\.sh|install-launchd\.sh"),
        "installing onto this Mac needs Nicola",
    ),
    (
        re.compile(
            r"\.ssh/|\.aws/|\.gnupg/|\.tauri/|pulse-updater|\.config/gh|Keychains",
            re.I,
        ),
        "keys and credentials are off limits",
    ),
    (
        re.compile(r"\.claude/settings", re.I),
        "Claude's settings are off limits for unattended sessions",
    ),
]
TMP_ROOTS = ("/tmp/", "/private/tmp/", "/var/folders/", "/private/var/folders/")
SECRET_DIRS = (".ssh", ".aws", ".gnupg", ".tauri", ".config/gh", "Library/Keychains")
# Tools that schedule or start work outside this session.
BLOCKED_TOOLS = {"CronCreate", "CronDelete", "RemoteTrigger"}
WRITE_TOOLS = {"Edit", "Write", "MultiEdit", "NotebookEdit"}
READ_TOOLS = {"Read", "Grep", "Glob"}


def _words(segment: str) -> list[str]:
    try:
        return shlex.split(segment)
    except ValueError:  # an unbalanced quote: the separator sat inside a string
        return [w.strip("'\"") for w in segment.split()]


def _command(words: list[str]) -> list[str]:
    """The words from the command a shell would start: variable assignments,
    the words in BEFORE_COMMAND and what belongs to them are dropped."""
    i = 0
    while i < len(words):
        word = words[i]
        if word == "command" and words[i + 1 : i + 2] in (["-v"], ["-V"]):
            return []  # only looks a name up
        if word in TAKES_VALUE:
            i += 2
        elif (
            ASSIGNMENT.match(word)
            or word.startswith("-")
            or DURATION.match(word)
            or os.path.basename(word) in BEFORE_COMMAND
        ):
            i += 1
        else:
            break
    return words[i:]


def _cargo_runs(args: list[str]) -> bool:
    """`cargo run` / `cargo r`, wherever cargo's own options stand. A `run`
    after another subcommand (`cargo nextest run`) or after `--` (the name of a
    test) is not one."""
    i = 0
    while i < len(args):
        word = args[i]
        if word == "--":
            return False
        if word in CARGO_TAKES_VALUE:
            i += 2
        elif word.startswith(("-", "+")):
            i += 1
        else:
            return word in CARGO_RUN
    return False


def _commands(cmd: str):
    """Every command in a shell line as (name, its arguments, the text it came
    from). The script handed to `bash -c "…"` is read as commands too."""
    for segment in SEGMENT.split(cmd):
        words = _command(_words(segment))
        if not words:
            continue
        first = os.path.basename(words[0])
        rest = [w for w in words[1:] if not REDIRECT.match(w)]
        yield first, rest, segment
        if first in SHELLS:
            for i, word in enumerate(rest[:-1]):
                if SHELL_SCRIPT_FLAG.match(word):
                    yield from _commands(rest[i + 1])
                    break


def runs_on_live_db(cmd: str) -> bool:
    """True when the command starts a Pulse binary (itself, or through `cargo
    run`) without --db-path. Without the flag they open the live database, and
    the fetcher's default mode is a full daily run. Building, testing, copying
    or searching for a binary is fine, and so is asking one for its help text."""
    for first, rest, segment in _commands(cmd):
        if "--db-path" in segment:
            continue
        if PULSE_BINARY.match(first):
            if not (rest and all(w in ONLY_PRINTS for w in rest)):
                return True
        elif first == "cargo" and _cargo_runs(rest):
            return True
    return False


def installs_globally(cmd: str) -> bool:
    """npm, pnpm or yarn asked to install onto the Mac instead of into the
    project, wherever the flag stands. What follows `--` belongs to the script
    being run (`pnpm test -- -g name`), not to the tool."""
    for first, rest, _ in _commands(cmd):
        if first not in NODE_TOOLS:
            continue
        own = rest[: rest.index("--")] if "--" in rest else rest
        plain = [w for w in own if not w.startswith("-")]
        if GLOBAL_FLAGS & set(own) or (first == "yarn" and plain[:1] == ["global"]):
            return True
    return False


def bash_block_reason(cmd: str, repo: str) -> str | None:
    reason = _bash_block_reason(cmd, repo)
    return reason + REWORD if reason else None


def _bash_block_reason(cmd: str, repo: str) -> str | None:
    if GIT_PUSH.search(cmd):
        return "commit only; the runner pushes after its checks"
    if GIT_STASH.search(cmd):
        return "the stash is shared with Nicola's checkout; set work aside with a commit instead"
    for m in GH_CALL.finditer(cmd):
        if (m.group(1), m.group(2) or "") not in GH_ALLOWED:
            call = f"gh {m.group(1)} {m.group(2) or ''}".strip()
            return f"`{call}` is not allowed here; gh is read-only for sessions"
    if runs_on_live_db(cmd):
        return (
            'run Pulse binaries (and `cargo run`) only with --db-path "$PULSE_DB_PATH", never on the '
            "live database; the dev app (`pnpm tauri dev`) takes it from the environment"
        )
    if SECRET_FILE.search(cmd):
        return ".env files hold real keys and are off limits"
    if installs_globally(cmd):
        return NEVER[2][1]
    for pattern, reason in NEVER:
        if pattern.search(cmd):
            return reason
    if repo:
        # The main checkout is Nicola's. Sessions stay inside <repo>/.claude/worktrees/.
        home = os.path.expanduser("~")
        forms = {repo}
        if repo.startswith(home):
            rest = repo[len(home) :]
            forms |= {"~" + rest, "$HOME" + rest, "${HOME}" + rest}
        for form in forms:
            if re.search(re.escape(form) + r"(?!/\.claude/worktrees/)(?![\w.-])", cmd):
                return "work only inside your worktree, not the main checkout"
    return None


def _full(path: str, cwd: str) -> str:
    path = os.path.expanduser(path)
    return os.path.realpath(path if os.path.isabs(path) else os.path.join(cwd, path))


def _is_env_file(full: str) -> bool:
    name = os.path.basename(full).lower()
    return name == ".env" or (name.startswith(".env.") and name != ".env.example")


def _inside(full: str, root: str) -> bool:
    full, root = full.lower(), root.lower()
    return full == root or full.startswith(root + os.sep)


def read_block_reason(path: str, cwd: str, repo: str) -> str | None:
    """Read, Grep and Glob: no secrets, no live app data, not the main checkout."""
    if not path:
        return None
    full = _full(path, cwd)
    if _is_env_file(full):
        return ".env files hold real keys and are off limits"
    if "/com.pulse.app" in full.lower() or "/mobile documents" in full.lower():
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
    # The runner passes both paths on the command line, so a session's shell
    # never has a variable that points at the main checkout.
    args = sys.argv[1:] + ["", ""]
    reason = decide(event, args[0], args[1])
    if reason:
        print(f"Blocked by the night-team guard: {reason}.", file=sys.stderr)
        return 2
    return 0


if __name__ == "__main__":
    sys.exit(main())
