#!/usr/bin/env python3
"""The watch: every ten minutes it looks at the installed Pulse and texts
Nicola when something is wrong (`rules.watch_findings` decides what counts).

It only reads: Pulse's logs, its progress file, its schedule stamps, launchd's
list, and one short read-only query on the database. It asks the broker's
clock through the installed fetcher (`--mode market-open`, which takes no lock
and writes nothing), the same call Pulse's own schedule makes. It starts no
session, changes nothing in Pulse and fixes nothing.

A fact that cannot be read is None, and a check without its facts says
nothing.

Standard library only."""

from __future__ import annotations

import contextlib
import datetime as dt
import fcntl
import json
import os
import re
import sqlite3
import subprocess
import sys
import urllib.parse
from pathlib import Path
from zoneinfo import ZoneInfo

sys.path.insert(0, str(Path(__file__).resolve().parent))
import chat  # noqa: E402
import rules  # noqa: E402

HOME = Path(os.environ.get("PULSE_TEAM_HOME") or Path.home() / ".pulse-team")
APP = Path(
    os.environ.get("PULSE_APP_DIR")
    or Path.home() / "Library/Application Support/com.pulse.app"
)
LOGS = Path(os.environ.get("PULSE_LOG_DIR") or Path.home() / "Library/Logs/Pulse")
FETCHER = Path(
    os.environ.get("PULSE_FETCHER")
    or "/Applications/Pulse.app/Contents/MacOS/pulse-fetcher"
)
CRASHES = Path.home() / "Library/Logs/DiagnosticReports"
MEMORY = HOME / "watch.json"  # the watch's notebook between two looks
ET = ZoneInfo("America/New_York")
EVERY_S = 600
LOG_TAIL = 1_500_000  # bytes of the hourly log read at each look: about four days
TICKS_KEPT = 40

# The same rule as the go-live checklist (`load` in
# src-tauri/src/services/go_live.rs), minus the two cases where no stop is
# right: under one whole share, and a stop that already sold.
BARE = """SELECT id, ticker FROM paper_trades
          WHERE status = 'open' AND order_status = 'filled'
            AND COALESCE(stop_order_id, '') = ''
            AND broker_stop_filled_at IS NULL
            AND COALESCE(filled_qty, 1) >= 1"""

ANSI = re.compile(r"\x1b\[[0-9;]*m")
# The fetcher's own lines: UTC.
TRACE = re.compile(
    r"^(\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d)(?:\.\d+)?Z\s+\w+\s+[\w:]+: (.*)$"
)
# The schedule script's lines: `date`, this Mac's clock.
ECHO = re.compile(r"^\w{3} (\w{3}) +(\d{1,2}) (\d\d):(\d\d):(\d\d) \S+ (\d{4}) (.*)$")
MONTHS = {
    m: i
    for i, m in enumerate("Jan Feb Mar Apr May Jun Jul Aug Sep Oct Nov Dec".split(), 1)
}
FAILED = re.compile(r"^(manage-positions|auto-trade) failed \(exit (\d+)\)")
BRIEFING = re.compile(r"^(morning|evening) briefing (done|not made \(exit (\d+)\))")
PANIC = re.compile(r"panicked at ([^\s:]+:\d+)")
CRASH_NAME = re.compile(r"^pulse(-fetcher|-backtest|-eval)?[-_]\d{4}", re.IGNORECASE)


def log(msg: str) -> None:
    print(f"{dt.datetime.now():%Y-%m-%d %H:%M:%S} {msg}", flush=True)


def local(t: dt.datetime) -> dt.datetime:
    """A time with a zone, as this Mac's clock (how the whole team counts)."""
    return t.astimezone().replace(tzinfo=None)


def parse_time(text) -> dt.datetime | None:
    """A stamp from a file, as this Mac's clock; UTC when it names no zone."""
    try:
        t = dt.datetime.fromisoformat(str(text).strip().replace("Z", "+00:00"))
    except ValueError:
        return None
    return local(t if t.tzinfo else t.replace(tzinfo=dt.timezone.utc))


def own_time(text) -> dt.datetime | None:
    """A stamp the team wrote itself: already this Mac's clock."""
    try:
        return dt.datetime.fromisoformat(str(text).strip())
    except ValueError:
        return None


def mtime(path: Path) -> dt.datetime | None:
    try:
        return dt.datetime.fromtimestamp(path.stat().st_mtime)
    except OSError:
        return None


# ------------------------------------------------------------- the facts --


def event_of(line: str) -> tuple[str, dt.datetime, str] | None:
    """One line of Pulse's hourly log as (kind, when, detail), or None."""
    line = ANSI.sub("", line).rstrip()
    if m := TRACE.match(line):
        msg = m[2]
        if msg.startswith("Position management complete:"):
            kind = "manage_ok"
        elif msg.startswith("Position mgmt: DRY_RUN") or "DRY-RUN only" in msg:
            kind = "manage_dry"
        elif msg.startswith("Auto-trade complete:"):
            kind = "auto_ok"
        else:
            return None
        when = parse_time(m[1])
        return (kind, when, "") if when else None
    if m := ECHO.match(line):
        if m[1] not in MONTHS:
            return None
        try:
            when = dt.datetime(
                int(m[6]), MONTHS[m[1]], int(m[2]), int(m[3]), int(m[4]), int(m[5])
            )
        except ValueError:
            return None
        msg = m[7]
        if msg.startswith("market open: managing paper positions"):
            return "manage_start", when, ""
        if f := FAILED.match(msg):
            return (
                ("manage_fail" if f[1] == "manage-positions" else "auto_fail"),
                when,
                f[2],
            )
        if b := BRIEFING.match(msg):
            if b[2] == "done":
                return "brief_done", when, b[1]
            return "brief_fail", when, f"{b[1]} {b[3]}"
    return None


def read_events(path: Path, tail: int = LOG_TAIL) -> list[tuple] | None:
    """The trading and briefing lines at the end of Pulse's hourly log, oldest
    first. None when the log cannot be read."""
    try:
        with open(path, "rb") as f:
            size = f.seek(0, os.SEEK_END)
            f.seek(max(0, size - tail))
            lines = f.read().decode("utf-8", "replace").splitlines()
    except OSError:
        return None
    if size > tail:
        lines = lines[1:]  # the first one is cut in half
    return [e for e in map(event_of, lines) if e]


def read_progress(path: Path) -> dict | None:
    try:
        data = json.loads(path.read_text())
    except (OSError, ValueError):
        return None
    if not isinstance(data, dict):
        return None
    return {
        "stage": data.get("stage"),
        "label": data.get("stage_label") or data.get("stage"),
        "started": parse_time(data.get("started_at")),
        "updated": parse_time(data.get("updated_at")),
    }


def read_bare(db: Path) -> list[dict] | None:
    """The held positions with no stop on Pulse's books. Read-only; None when
    the database cannot be read right now."""
    if not db.exists():
        return None
    try:
        con = sqlite3.connect(
            f"file:{urllib.parse.quote(str(db))}?mode=ro", uri=True, timeout=2
        )
        try:
            con.execute("PRAGMA query_only = 1")
            return [{"id": i, "ticker": t} for i, t in con.execute(BARE)]
        finally:
            con.close()
    except sqlite3.Error:
        return None


def market(now: dt.datetime, fetcher: Path, run=subprocess.run):
    """(open | closed | unknown | None, when today's trading began). Asked of
    the broker's clock, so a holiday or an early close is simply `closed`. Only
    asked in the hours Pulse trades (weekdays 10:00 to 16:00 New York)."""
    et = now.astimezone(ET)
    if et.weekday() >= 5 or not 10 <= et.hour < 16 or not fetcher.exists():
        return None, None
    start = local(et.replace(hour=10, minute=0, second=0, microsecond=0))
    try:
        code = run(
            [str(fetcher), "--mode", "market-open"],
            capture_output=True,
            timeout=90,
            check=False,
        ).returncode
    except (OSError, subprocess.SubprocessError):
        return "unknown", start
    return {0: "open", 3: "closed"}.get(code, "unknown"), start


def exit_code(cmd: list[str], run=subprocess.run) -> int | None:
    try:
        return run(cmd, capture_output=True, timeout=20, check=False).returncode
    except (OSError, subprocess.SubprocessError):
        return None


def alive(pgrep_args: list[str], run=subprocess.run) -> bool | None:
    """Whether such a process runs. Never by trying a lock: a job starting at
    that moment would find it taken and skip its turn."""
    return {0: True, 1: False}.get(exit_code(["/usr/bin/pgrep", *pgrep_args], run))


def read_loaded(labels, run=subprocess.run) -> dict:
    domain = f"gui/{os.getuid()}"
    return {
        label: {0: True, 113: False}.get(
            exit_code(["/bin/launchctl", "print", f"{domain}/{label}"], run)
        )
        for label in labels
    }


def read_crashes(folder: Path, now: dt.datetime) -> list[str]:
    try:
        files = list(folder.iterdir())
    except OSError:
        return []
    day = dt.timedelta(hours=24)
    return sorted(
        f.name
        for f in files
        if f.suffix == ".ips"
        and CRASH_NAME.match(f.name)
        and (made := mtime(f))
        and now - made <= day
    )


def read_panics(logs: list[Path], memory: dict) -> list[str]:
    """Where the fetcher panicked since the last look. Its error log has no
    times, so the watch remembers how far it has read; the first look only
    notes where each log ends."""
    seen = memory.setdefault("stderr", {})
    out: set[str] = set()
    for path in logs:
        try:
            size = path.stat().st_size
        except OSError:
            continue
        pos = seen.get(path.name)
        seen[path.name] = size
        if pos is None or size == pos:
            continue
        try:
            with open(path, "rb") as f:
                f.seek(pos if size > pos else 0)  # smaller: a new file
                out.update(PANIC.findall(f.read(1_000_000).decode("utf-8", "replace")))
        except OSError:
            continue
    return sorted(out)


def gather(now: dt.datetime, memory: dict, run=subprocess.run) -> dict:
    """Everything the watch can see right now."""
    ticks = [t for t in map(own_time, memory.get("ticks", [])) if t]
    memory["ticks"] = [
        t.isoformat(timespec="seconds") for t in [*ticks, now][-TICKS_KEPT:]
    ]
    state, session = market(now, FETCHER, run)
    wake = None
    with contextlib.suppress(OSError):
        wake = own_time((HOME / "wake.txt").read_text())
    beat = (chat._load(chat.CHAT_STATE, {}) or {}).get("heartbeat")
    return {
        "awake": rules.awake_since(ticks, now),
        "events": read_events(LOGS / "fetch-stdout.log"),
        "progress": read_progress(APP / "fetch-progress.json"),
        "bare": read_bare(APP / "pulse.db"),
        "market": state,
        "session_start": session,
        "fetching": alive(["-x", "pulse-fetcher"], run),
        "loaded": read_loaded(rules.SCHEDULERS, run),
        "stamp": mtime(APP / "schedule/morning"),
        "crashes": read_crashes(CRASHES, now),
        "panics": read_panics(
            [LOGS / "fetch-stderr.log", LOGS / "live-stderr.log"], memory
        ),
        "team_wake": wake,
        "team_running": alive(["-f", "team.py run"], run),
        "paired": chat.paired(),
        "listener": own_time(beat) if beat else None,
    }


# -------------------------------------------------------------- the look --


def load() -> dict:
    data = chat._load(MEMORY, {})
    return data if isinstance(data, dict) else {}


def look(
    now: dt.datetime | None = None, tell: bool = True, run=subprocess.run
) -> list[dict]:
    """One look. With `tell` it texts what is new and remembers it; without,
    it only reports and leaves the notebook as it was."""
    now = now or dt.datetime.now()
    memory = load()
    facts = gather(now, memory, run)
    found = rules.watch_findings(facts, memory, now)
    if not tell:
        return found
    texts = rules.watch_alerts(found, facts, memory, now)
    memory["looked"] = now.isoformat(timespec="seconds")
    memory["found"] = found
    chat._save(MEMORY, memory)  # before the texts: a crash must not send them twice
    for text, serious in texts:
        text = ("Serious problem in Pulse. " if serious else "Pulse watch: ") + text
        result = chat.send(text, serious=serious, now=now)
        if result == "off" and serious:
            chat.banner(text)  # no bot yet: at least a notification on the Mac
        log(f"[{result}] {text}")
    return found


def as_text(memory: dict) -> str:
    """What the watch last saw, for `team.py status`."""
    looked = own_time(memory.get("looked", ""))
    if not looked:
        return "The watch has not looked yet."
    found = [f for f in memory.get("found") or [] if isinstance(f, dict)]
    if not found:
        return f"The watch looked at {looked:%H:%M on %d %b}: nothing wrong."
    lines = [f"The watch looked at {looked:%H:%M on %d %b} and found:"]
    lines += [
        f"- {'SERIOUS: ' if f.get('serious') else ''}{f.get('text', '')}" for f in found
    ]
    return "\n".join(lines)


def main(argv: list[str] | None = None) -> int:
    argv = sys.argv[1:] if argv is None else argv
    dry = "--dry" in argv
    HOME.mkdir(parents=True, exist_ok=True)
    with open(HOME / "watch.lock", "w") as lock:
        try:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except OSError:
            return 0  # another look is running
        try:
            found = look(tell=not dry)
        except Exception as e:  # noqa: BLE001
            log(f"the look failed: {type(e).__name__}: {e}")
            return 1
    if dry or sys.stdout.isatty():
        for f in found:
            print(f"{'SERIOUS: ' if f['serious'] else ''}{f['text']}")
        if not found:
            print("Nothing wrong.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
