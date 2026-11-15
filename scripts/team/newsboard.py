#!/usr/bin/env python3
"""The weekly news board: how many articles Pulse fetched, how many survived
each cut, which topics and sources fill the briefings, which are fetched and
never used, and what was read in the app.

Like the trading scoreboard it only reads the team's copy of the database,
with plain queries: no session, no AI, nothing proposed and nothing changed.
Every number can be checked by hand.

What the tables mean (migration 034): `pipeline_health.stories_fetched` is
what a run fetched before repeats were dropped; `fetch_candidates` is every
article left after that, with how far it got (`kept_by_precurate`, and
`story_id` set when it made the briefing). A "source" is the publisher's name
on the article, so one Google News search counts as many sources.

A briefing's day is the Mac's own (`briefings.date`); the app stamps what was
read in UTC, so those stamps are turned into the Mac's day before counting.

Standard library only."""

from __future__ import annotations

import datetime as dt
import sqlite3
import statistics

from scoreboard import _day, _more, _rows

TOPICS = {"ai": "AI", "tech": "Tech", "italy": "Italy", "miami": "Miami"}
BURIED_MIN = 100  # offered this often in the week before "almost never used" is said
BURIED_SHARE = 0.02
QUIET_MIN = 80  # articles in the four weeks before, for "gone quiet"
QUIET_DAYS = 28
WINDOW = """FROM fetch_candidates c JOIN briefings b ON b.id = c.briefing_id
            WHERE b.briefing_type = 'daily' AND b.date BETWEEN ? AND ?"""


def build(con: sqlite3.Connection, today: dt.date, days: int = 7) -> dict:
    """The news board as plain data, for the `days` up to and including `today`."""
    first = today - dt.timedelta(days=days - 1)
    since, until = first.isoformat(), today.isoformat()
    week = (since, until)
    # Not through `_rows`: without the briefings table this is not Pulse's
    # database, and that is an error, not a week without news.
    briefings = con.execute(
        "SELECT COUNT(*) FROM briefings WHERE briefing_type = 'daily' AND date BETWEEN ? AND ?",
        week,
    ).fetchone()[0]
    runs = _rows(
        con,
        f"""SELECT COUNT(*), COALESCE(SUM(c.kept_by_precurate), 0), SUM(c.story_id IS NOT NULL)
            {WINDOW} GROUP BY b.id""",
        *week,
    )
    health = _rows(
        con,
        """SELECT stories_fetched, COALESCE(feeds_failed, 0), duration_secs
           FROM pipeline_health WHERE run_date BETWEEN ? AND ?""",
        *week,
    )
    offered = _rows(con, f"SELECT COUNT(*), COUNT(DISTINCT c.url_hash) {WINDOW}", *week)
    total, different = offered[0] if offered else (0, 0)
    topics = _rows(
        con,
        f"""SELECT c.sector, COUNT(*), SUM(c.story_id IS NOT NULL) {WINDOW}
            GROUP BY c.sector ORDER BY 2 DESC""",
        *week,
    )
    importance = dict(
        _rows(
            con,
            """SELECT s.sector, AVG(s.importance_score)
               FROM stories s JOIN briefings b ON b.id = s.briefing_id
               WHERE b.briefing_type = 'daily' AND b.date BETWEEN ? AND ?
                 AND COALESCE(s.source_type, 'news') = 'news'
                 AND s.importance_score IS NOT NULL
               GROUP BY s.sector""",
            *week,
        )
    )
    sources = _rows(
        con,
        f"""SELECT COALESCE(c.source_name, '?'), COUNT(*), SUM(c.story_id IS NOT NULL)
            {WINDOW} GROUP BY 1""",
        *week,
    )
    before = (
        (first - dt.timedelta(days=QUIET_DAYS)).isoformat(),
        (first - dt.timedelta(days=1)).isoformat(),
    )
    quiet = _rows(
        con,
        f"""SELECT c.source_name, COUNT(*) {WINDOW} AND c.source_name IS NOT NULL
              AND c.source_name NOT IN (SELECT COALESCE(c.source_name, '?') {WINDOW})
            GROUP BY 1 HAVING COUNT(*) >= ? ORDER BY 2 DESC, 1""",
        *before,
        *week,
        QUIET_MIN,
    )
    opened = _rows(
        con,
        """SELECT COUNT(DISTINCT CASE WHEN date(occurred_at, 'localtime') BETWEEN ? AND ?
                                      THEN story_id END),
                  COUNT(DISTINCT story_id), date(MAX(occurred_at), 'localtime')
           FROM engagement_events WHERE event = 'story_open'""",
        *week,
    )
    marked = _rows(
        con,
        """SELECT COALESCE(SUM(date(updated_at, 'localtime') BETWEEN ? AND ?), 0), COUNT(*)
           FROM story_feedback""",
        *week,
    )
    used = sorted((s for s in sources if s[2]), key=lambda s: (-s[2], s[0]))
    buried = sorted(
        (s for s in sources if s[1] >= BURIED_MIN and s[2] / s[1] < BURIED_SHARE),
        key=lambda s: (s[2] / s[1], -s[1], s[0]),
    )

    def mean(values: list) -> float | None:
        values = [v for v in values if v is not None]
        return statistics.fmean(values) if values else None

    return {
        "since": since,
        "until": until,
        "briefings": briefings,
        "counted": len(runs),  # briefings whose articles were recorded
        "fetched": mean([h[0] for h in health]),
        "pool": mean([r[0] for r in runs]),
        "kept": mean([r[1] for r in runs]),
        "made": mean([r[2] for r in runs]),
        "minutes": (mean([h[2] for h in health]) or 0) / 60 or None,
        "runs_measured": len(health),
        "runs_with_failures": sum(1 for h in health if h[1]),
        "most_failed": max((h[1] for h in health), default=0),
        "offered": total or 0,
        "offered_again": (total or 0) - (different or 0),
        "topics": [
            {"topic": t, "offered": n, "made": m or 0, "importance": importance.get(t)}
            for t, n, m in topics
        ],
        "sources_offering": len(sources),
        "sources_used": len(used),
        "most_used": [{"source": s, "made": m, "offered": n} for s, n, m in used],
        "buried": [{"source": s, "made": m, "offered": n} for s, n, m in buried],
        "quiet": [{"source": s, "before": n} for s, n in quiet],
        "opened": opened[0][0] if opened else 0,
        "opened_ever": opened[0][1] if opened else 0,
        "last_opened": (opened[0][2] or "") if opened else "",
        "marked": marked[0][0] if marked else 0,
        "marked_ever": marked[0][1] if marked else 0,
    }


def _about(x: float) -> str:
    """A count as a round figure: an average of runs is not exact to the unit."""
    return f"{round(x, -1 if x >= 100 else 0):,.0f}"


def _names(items: list[dict], shown: int, form) -> str:
    return ", ".join(form(i) for i in items[:shown]) + _more(shown, len(items))


def as_text(board: dict) -> str:
    """The news board in plain words."""
    out = [f"Pulse news, {_day(board['since'])} to {_day(board['until'])}"]
    n = board["briefings"]
    if not n:
        out.append("\nNo briefing was made in these days.")
        return "\n".join(out)

    out.append("\nWhat came in")
    line = f"• {n} briefing{'s' if n != 1 else ''}."
    if board["fetched"]:
        line += f" Each fetched about {_about(board['fetched'])} articles."
    if board["counted"]:
        line += (
            f" About {_about(board['pool'])} were left once repeats were dropped, the first "
            f"cut kept {_about(board['kept'])}, and {_about(board['made'])} made the briefing."
        )
        if board["counted"] != n:
            line += f" (Counted on {board['counted']} of the {n}.)"
    else:
        line += " What was offered to them was not recorded."
    out.append(line)
    if board["minutes"]:
        out.append(f"• A run took about {board['minutes']:.0f} minutes.")
    if board["runs_measured"]:
        bad = board["runs_with_failures"]
        out.append(
            f"• A source failed in {bad} of {board['runs_measured']} runs "
            f"(at most {board['most_failed']} at a time)."
            if bad
            else "• No source failed in any run."
        )
    if board["offered"]:
        share = 100 * board["offered_again"] / board["offered"]
        out.append(
            f"• {share:.0f}% of what was offered had already been offered to an earlier "
            "briefing of the week."
        )

    if board["topics"]:
        out.append(
            "\nBy topic (articles offered → in the briefings, and the AI's own average "
            "importance score out of 10)"
        )
        for t in board["topics"]:
            name = TOPICS.get(t["topic"], str(t["topic"]).capitalize())
            line = f"• {name}: {t['offered']:,} → {t['made']:,}"
            if t["importance"] is not None:
                line += f", importance {t['importance']:.1f}"
            out.append(line)

    if not board["counted"]:
        return "\n".join(out + _read(board))
    out.append("\nSources")
    out.append(
        f"• {board['sources_used']:,} different sources made the briefings, out of "
        f"{board['sources_offering']:,} that offered something."
    )
    if board["most_used"]:
        out.append(
            "• Most used: "
            + _names(board["most_used"], 6, lambda s: f"{s['source']} ({s['made']})")
            + "."
        )
    if board["buried"]:
        out.append(
            f"• Offered {BURIED_MIN} or more and almost never used: "
            + _names(
                board["buried"],
                6,
                lambda s: f"{s['source']} ({s['made']} of {s['offered']})",
            )
            + "."
        )
    if board["quiet"]:
        out.append(
            f"• Gone quiet ({QUIET_MIN} or more in the four weeks before, nothing in these "
            "days): "
            + _names(board["quiet"], 6, lambda s: f"{s['source']} ({s['before']})")
            + "."
        )

    return "\n".join(out + _read(board))


def _read(board: dict) -> list[str]:
    last = f", the last on {_day(board['last_opened'])}" if board["last_opened"] else ""
    return [
        "\nWhat was read in the Mac app",
        f"• Stories opened: {board['opened']} ({board['opened_ever']} since the start{last}).",
        f"• Stories marked: {board['marked']} ({board['marked_ever']} since the start).",
    ]
