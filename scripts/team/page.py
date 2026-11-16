"""The night team's daily page: history (newest first) -> one self-contained HTML page.

Pure functions. Everything that came from a session or from GitHub is escaped.
"""

from __future__ import annotations

import datetime as dt
from html import escape as e

LABELS = {
    "merged": ("Merged", "good"),
    "ready": ("Ready to merge", "good"),
    "needs_user": ("Needs you", "warn"),
    "blocked": ("Stopped", "bad"),
    "error": ("Failed, will retry", "bad"),
    "no_change": ("Nothing to do", "quiet"),
    "yielded": ("Stepped aside, goes on later", "quiet"),
}
# The team's turns that are not the night's: (in a heading, after "Pulse team").
SHIFTS = {
    "day": ("Today, by day", ", by day"),
    "incident": ("A serious problem", ", on a serious problem"),
}

CSS = """
/* Layout: a night-shift log. One narrow column, newest entry on top, what needs you first. */
:root{--bg:#f2f4f8;--card:#ffffff;--ink:#131a26;--soft:#566074;--line:#d9dee8;--accent:#3346c4;
--good:#1c7a4b;--good-bg:#e1f3e9;--warn:#8a5a00;--warn-bg:#fbefd2;--bad:#b3261e;--bad-bg:#fbe4e2;--quiet-bg:#e7eaf0;
--text:"IBM Plex Sans",system-ui,-apple-system,"Segoe UI",sans-serif;--mono:"IBM Plex Mono",ui-monospace,Menlo,monospace}
@media (prefers-color-scheme:dark){:root:not([data-theme="light"]){--bg:#0d121a;--card:#151c28;--ink:#e8ecf4;--soft:#98a3b8;--line:#28324a;--accent:#8e9bff;
--good:#6fd6a0;--good-bg:#12331f;--warn:#f0c268;--warn-bg:#3a2c0c;--bad:#ff9d94;--bad-bg:#3d1714;--quiet-bg:#1e2738;color-scheme:dark}}
:root[data-theme="dark"]{--bg:#0d121a;--card:#151c28;--ink:#e8ecf4;--soft:#98a3b8;--line:#28324a;--accent:#8e9bff;
--good:#6fd6a0;--good-bg:#12331f;--warn:#f0c268;--warn-bg:#3a2c0c;--bad:#ff9d94;--bad-bg:#3d1714;--quiet-bg:#1e2738;color-scheme:dark}
body{background:var(--bg);color:var(--ink);font-family:var(--text);font-size:16px;line-height:1.55}
.nt{max-width:46rem;margin:0 auto;padding-block:2rem 4rem;padding-left:max(16px,env(safe-area-inset-left,0px));padding-right:max(16px,env(safe-area-inset-right,0px));display:flex;flex-direction:column;gap:2.25rem}
.nt h1{font-size:1.9rem;line-height:1.15;margin:0;letter-spacing:-.01em;text-wrap:balance}
.nt h2{font-family:var(--mono);font-size:.78rem;font-weight:500;letter-spacing:.09em;text-transform:uppercase;color:var(--soft);margin:0}
.nt p,.nt ul{margin:0}.nt ul{padding-left:1.1rem}.nt a{color:var(--accent)}
.nt a:focus-visible{outline:2px solid var(--accent);outline-offset:2px}
.nt .stamp{font-family:var(--mono);font-size:.85rem;color:var(--soft);font-variant-numeric:tabular-nums}
.nt header,.nt section{display:flex;flex-direction:column;gap:.85rem}
.nt .entry{background:var(--card);border:1px solid var(--line);border-radius:6px;padding:1rem 1.1rem;display:flex;flex-direction:column;gap:.55rem;min-width:0}
.nt .entry h3{font-size:1.05rem;line-height:1.35;margin:0;font-weight:600;overflow-wrap:anywhere}
.nt .row{display:flex;flex-wrap:wrap;gap:.5rem .75rem;align-items:center}
.nt .pill{font-family:var(--mono);font-size:.74rem;padding:.12rem .5rem;border-radius:3px;white-space:nowrap}
.nt .good{color:var(--good);background:var(--good-bg)}.nt .warn{color:var(--warn);background:var(--warn-bg)}
.nt .bad{color:var(--bad);background:var(--bad-bg)}.nt .quiet{color:var(--soft);background:var(--quiet-bg)}
.nt .sub{font-size:.9rem;color:var(--soft)}.nt .sub b{color:var(--ink);font-weight:600}
.nt .plain{list-style:none;padding:0;display:flex;flex-direction:column;gap:.45rem}
.nt .plain li{border-bottom:1px solid var(--line);padding-bottom:.45rem;overflow-wrap:anywhere}
.nt .scroll{overflow-x:auto}.nt table{border-collapse:collapse;width:100%;font-size:.9rem;font-variant-numeric:tabular-nums}
.nt th{font-family:var(--mono);font-size:.72rem;font-weight:500;letter-spacing:.06em;text-transform:uppercase;color:var(--soft);text-align:left}
.nt th,.nt td{padding:.4rem .9rem .4rem 0;border-bottom:1px solid var(--line);white-space:nowrap}
.nt table.wrap td{white-space:normal;vertical-align:top;overflow-wrap:anywhere}.nt table.wrap td:first-child{white-space:nowrap}
.nt code{font-family:var(--mono);font-size:.88em;background:var(--quiet-bg);padding:.05rem .3rem;border-radius:3px}
"""


def _when(iso: str) -> str:
    try:
        return dt.datetime.fromisoformat(iso).strftime("%a %d %b, %H:%M")
    except (TypeError, ValueError):
        return "—"


def _pr(item: dict) -> str:
    if item.get("pr"):
        return f"PR #{item['pr']}: "
    return "Idea: " if item.get("kind") == "idea" else ""


def _items(label: str, items: list) -> str:
    if not items:
        return ""
    lis = "".join(f"<li>{e(str(x))}</li>" for x in items)
    return f'<div class="sub"><b>{label}</b><ul>{lis}</ul></div>'


def _job(job: dict, repo: str) -> str:
    label, tone = LABELS.get(job.get("result"), (job.get("result") or "?", "quiet"))
    pr = job.get("pr")
    link = (
        f'<a href="https://github.com/{e(repo)}/pull/{pr}">PR #{pr}</a>'
        if pr and repo
        else ""
    )
    took = f"{job.get('minutes', 0)} min"
    parts = [
        f'<div class="row"><span class="pill {tone}">{label}</span><span class="stamp">{link} {took}</span></div>',
        f"<h3>{e(job.get('title') or '')}</h3>",
    ]
    if job.get("summary"):
        parts.append(f"<p>{e(job['summary'])}</p>")
    parts.append(_items("Why it stopped", job.get("reasons") or []))
    parts.append(_items("Try it", job.get("try_it") or []))
    parts.append(_items("Not checked", job.get("not_verified") or []))
    parts.append(_items("New dependencies", job.get("new_dependencies") or []))
    return f'<div class="entry">{"".join(parts)}</div>'


def last_night(history: list[dict]) -> dict | None:
    """The newest night's report, when the newest report of all is a turn by
    day or for a serious problem. Such a turn looks at no pull request and
    does not check main, so what that night left for Nicola still waits."""
    if not history or history[0].get("shift") not in SHIFTS:
        return None
    return next((r for r in history[1:] if r.get("shift") not in SHIFTS), None)


def _left(report: dict, handled: frozenset = frozenset()) -> list[str]:
    """What a run's own work left for Nicola: a red main, jobs that stopped,
    pull requests it left alone. `handled` are the pull requests a later turn
    worked on, which says where they stand itself."""
    out = []
    main = report.get("main") or {}
    if (
        main
        and not main.get("ok")
        # while main is red its fix is the only job, so a merge means it went in
        and not any(j.get("result") == "merged" for j in report.get("jobs", []))
    ):
        out.append(
            f"main is failing its checks ({main.get('failed_step') or 'unknown step'}) and the team could not fix it."
        )
    for j in report.get("jobs", []):
        if j.get("result") in ("needs_user", "blocked") and j.get("pr") not in handled:
            where = f"PR #{j['pr']}: " if j.get("pr") else ""
            out.append(
                f"{where}{j.get('title', '')} — {(j.get('reasons') or ['stopped'])[0]}"
            )
    for s in report.get("skipped", []):
        if (
            "waiting for you" in s.get("why", "")
            or "no change" in s.get("why", "")
            or "nights in a row" in s.get("why", "")
        ) and s.get("pr") not in handled:
            out.append(f"{_pr(s)}{s.get('title', '')} — {s['why']}")
    return out


def needs_you(report: dict, night: dict | None = None) -> list[str]:
    """Plain lines for everything waiting on Nicola. `night` is the last
    night's report when `report` is a later turn (`last_night`): what the
    night left is carried along."""
    out = _left(report)
    if night:
        handled = frozenset(j["pr"] for j in report.get("jobs", []) if j.get("pr"))
        out += [w for w in _left(night, handled) if w not in out]
    for i in report.get("ideas", []):
        if i.get("state") == "waiting":
            out.append(f"Idea: {i['text']}")
    # What the watch saw in the installed app when the night ended.
    for f in report.get("watch") or []:
        out.append(f"{'Serious, in' if f.get('serious') else 'In'} the running app: {f.get('text', '')}")
    # A queued question goes out with the morning text; a queued idea waits
    # for its place in the week and is not waiting for an answer yet.
    asked = [
        a
        for a in report.get("asked") or []
        if a.get("state") != "queued" or a.get("kind") == "question"
    ]
    if asked:
        out.append(
            f"{len(asked)} question{'s' if len(asked) != 1 else ''} wait for your answer in Telegram: "
            + "; ".join(str(a.get("title", ""))[:80] for a in asked[:5])
        )
    merges = (report.get("unreleased") or {}).get("merges", 0)
    if merges:
        tag = (report.get("unreleased") or {}).get("tag") or "the last release"
        out.append(
            f"{merges} merged change{'s' if merges != 1 else ''} since {tag} are not in your installed app yet. "
            'Tell Claude "release Pulse" when you want them.'
        )
    return out


def _team(report: dict, night: str = "") -> str:
    """Who reports: the night team, or the team by day or on an incident."""
    shift = SHIFTS.get(report.get("shift"))
    return f"Pulse team{shift[1]}" if shift else f"Pulse {night}team"


def incident_lines(job: dict) -> list[str]:
    """What became of a serious problem the team looked into, in plain words."""
    pr = f" (PR #{job['pr']})" if job.get("pr") else ""
    why = str((job.get("reasons") or ["no reason was given"])[0])[:600]
    result = job.get("result")
    if result == "merged":
        said = (
            f"Cause found and fixed{pr}. The fix is on main; your installed app gets it "
            "with the next release."
        )
    elif result == "ready":
        said = f"A fix is ready to merge{pr}."
    elif result in ("needs_user", "blocked") and pr:
        said = f"A fix waits for you{pr}: {why}"
    elif result in ("needs_user", "blocked"):
        said = f"Not fixed by the team: {why}"
    elif result == "yielded":
        said = f"The work on it stopped midway and goes on at the next turn ({why})."
    else:
        said = f"The look into it failed: {why}"
    return [f"Problem: {str(job.get('title', ''))[:300]}", said]


def as_text(report: dict, night: dict | None = None) -> str:
    lines = [
        f"{_team(report, 'night ')} — {_when(report.get('started', ''))} ({report.get('minutes', 0)} min)"
    ]
    for j in report.get("jobs", []):
        label = LABELS.get(j.get("result"), (j.get("result"), ""))[0]
        pr = f" (PR #{j['pr']})" if j.get("pr") else ""
        lines.append(f"  [{label}] {j.get('title', '')}{pr}")
        lines += [f"      {r}" for r in (j.get("reasons") or [])[:3]]
    if not report.get("jobs"):
        lines.append("  Nothing to work on.")
    lines += [f"  note: {n}" for n in report.get("notes", [])]
    waiting = needs_you(report, night)
    if waiting:
        lines.append("Needs you:")
        lines += [f"  - {w}" for w in waiting]
    return "\n".join(lines)


def chat_digest(report: dict, url: str = "") -> str:
    """The night in a few lines, for Telegram. Each job that stopped for Nicola
    gets its own message with buttons, so here it is only counted."""
    every = report.get("jobs", [])
    lines = [f"{_team(report)} — {_when(report.get('started', ''))}"]
    for j in every:
        if j.get("kind") == "incident":
            lines += incident_lines(j)
    jobs = [j for j in every if j.get("kind") != "incident"]
    for result, word in (("merged", "Merged"), ("ready", "Ready to merge")):
        done = [j for j in jobs if j.get("result") == result]
        if done:
            lines.append(f"{word}: {len(done)}")
            lines += [
                f"• {j.get('title', '')}" + (f" (PR #{j['pr']})" if j.get("pr") else "")
                for j in done[:8]
            ]
    stopped = [j for j in jobs if j.get("result") in ("needs_user", "blocked")]
    if stopped:
        lines.append(f"Stopped for you: {len(stopped)}. I ask about each one below.")
    retry = sum(1 for j in jobs if j.get("result") == "error")
    if retry:
        lines.append(f"Failed and will be tried again: {retry}")
    aside = sum(1 for j in jobs if j.get("result") == "yielded")
    if aside:
        lines.append(f"Stepped aside midway, goes on later: {aside}")
    if not every:
        lines.append("Nothing to work on.")
    lines += [f"Note: {n}" for n in (report.get("notes") or [])[:4]]
    main = report.get("main") or {}
    if (
        main
        and not main.get("ok")
        and not any(j.get("result") == "merged" for j in every)
    ):
        lines.append(
            f"main is failing its checks ({main.get('failed_step') or 'unknown step'}) and the team could not fix it."
        )
    merges = (report.get("unreleased") or {}).get("merges", 0)
    if merges:
        lines.append(
            f"Not in your app yet: {merges} merged change{'s' if merges != 1 else ''}. "
            'Tell Claude "release Pulse" when you want them.'
        )
    if url:
        lines.append(f"Page: {url}")
    return "\n".join(lines)


def _day(iso) -> str:
    try:
        d = dt.date.fromisoformat(str(iso))
    except ValueError:
        return "—"
    return f"{d.day} {d:%b}"


def market_section(market) -> str:
    """The market scout's map: products like Pulse, what they sell and charge.
    Every word and link in it came from the web through a session, so all of it
    is escaped, and a link is only made of a plain web address."""
    market = market if isinstance(market, dict) else {}
    rows, kind = "", None
    for p in market.get("map") or []:
        if not isinstance(p, dict):
            continue
        url = str(p.get("url") or "")
        if not url.lower().startswith(("https://", "http://")):
            continue
        if p.get("kind") != kind:
            kind = p.get("kind")
            rows += f'<tr><th colspan="3">{e(str(kind or "other"))}</th></tr>'
        if p.get("seen") != market.get("made"):
            stale = f' <span class="sub">(last seen {e(_day(p.get("seen")))})</span>'
        elif p.get("price") and p.get("price_seen") not in (None, "", p.get("seen")):
            stale = f' <span class="sub">(price read {e(_day(p.get("price_seen")))})</span>'
        else:
            stale = ""
        changed = (
            f'<br><span class="sub">Changed: {e(str(p["changed"]))}</span>'
            if p.get("changed")
            else ""
        )
        rows += (
            f'<tr><td><a href="{e(url)}" rel="noreferrer noopener">{e(str(p.get("name") or ""))}</a></td>'
            f"<td>{e(str(p.get('price') or 'price not seen'))}{stale}</td>"
            f"<td>{e(str(p.get('offer') or ''))}{changed}</td></tr>"
        )
    if not rows:
        return ""
    summary = (
        f"<p>{e(str(market['summary']))}</p>" if market.get("summary") else ""
    )
    missing = [str(x) for x in market.get("not_checked") or [] if isinstance(x, str)]
    return (
        f'<section id="market"><h2>Products like Pulse</h2>{summary}'
        '<div class="scroll"><table class="wrap"><thead><tr><th>Product</th><th>Price</th>'
        f"<th>What it sells</th></tr></thead><tbody>{rows}</tbody></table></div>"
        f'<p class="sub">Read from the web by the market scout on {e(_day(market.get("made")))}. '
        "It only reads; an idea from it is built only after your yes.</p>"
        f"{_items('Could not check', missing)}</section>"
    )


def render(history: list[dict], market: dict | None = None) -> str:
    report = history[0] if history else {}
    night = last_night(history)
    repo = report.get("repo", "")
    main = report.get("main") or (night or {}).get("main") or {}
    main_pill = (
        '<span class="pill good">main passes its checks</span>'
        if main.get("ok")
        else '<span class="pill bad">main is failing</span>'
        if main
        else ""
    )
    waiting = needs_you(report, night)
    jobs = report.get("jobs", [])
    sections = []

    if waiting:
        lis = "".join(f"<li>{e(w)}</li>" for w in waiting)
        sections.append(
            f'<section id="needs-you"><h2>Needs you</h2><ul class="plain">{lis}</ul></section>'
        )
    else:
        sections.append(
            '<section id="needs-you"><h2>Needs you</h2><p>Nothing. Everything from the last run is handled.</p></section>'
        )

    body = "".join(_job(j, repo) for j in jobs) or "<p>Nothing to work on.</p>"
    notes = "".join(f'<p class="sub">{e(n)}</p>' for n in report.get("notes", []))
    what = SHIFTS.get(report.get("shift"), ("Last night",))[0]
    sections.append(
        f'<section id="last-night"><h2>{what}</h2>{body}{notes}</section>'
    )
    if night:
        # Under a turn by day or for a serious problem: the night before it.
        body = "".join(_job(j, night.get("repo", "") or repo) for j in night.get("jobs", []))
        notes = "".join(f'<p class="sub">{e(n)}</p>' for n in night.get("notes", []))
        sections.append(
            f'<section id="the-night"><h2>Last night</h2>{body or "<p>Nothing to work on.</p>"}{notes}</section>'
        )

    skipped = [s for s in report.get("skipped", [])]
    if skipped:
        lis = "".join(
            f'<li>{_pr(s)}{e(s.get("title", ""))} <span class="sub">— {e(s.get("why", ""))}</span></li>'
            for s in skipped
        )
        sections.append(
            f'<section id="skipped"><h2>Left alone</h2><ul class="plain">{lis}</ul></section>'
        )

    open_ideas = [i for i in report.get("ideas", []) if i.get("state") == "open"]
    lis = (
        "".join(f"<li>{e(i['text'])}</li>" for i in open_ideas[:10])
        or "<li>The idea list is empty.</li>"
    )
    more = (
        f'<p class="sub">and {len(open_ideas) - 10} more</p>'
        if len(open_ideas) > 10
        else ""
    )
    sections.append(
        f'<section id="up-next"><h2>Up next</h2><ul class="plain">{lis}</ul>{more}'
        '<p class="sub">To add an idea, tell Claude <code>Pulse idea: …</code> in any session. '
        "The team builds from the top, after open pull requests.</p></section>"
    )

    sections.append(market_section(market))

    if len(history) > 1:
        rows = ""
        for r in history[:14]:
            js = r.get("jobs", [])
            merged = sum(1 for j in js if j.get("result") == "merged")
            stuck = sum(
                1 for j in js if j.get("result") in ("needs_user", "blocked", "error")
            )
            turn = SHIFTS.get(r.get("shift"), ("", ""))[1]
            rows += (
                f"<tr><td>{_when(r.get('started', ''))}{turn}</td><td>{merged}</td><td>{stuck}</td>"
                f"<td>{r.get('minutes', 0)} min</td><td>${r.get('cost', 0):.2f}</td></tr>"
            )
        sections.append(
            '<section id="earlier"><h2>Earlier runs</h2><div class="scroll"><table><thead><tr><th>Run</th>'
            f"<th>Merged</th><th>Stuck</th><th>Took</th><th>Usage</th></tr></thead><tbody>{rows}</tbody></table></div>"
            '<p class="sub">Usage is what the sessions would cost at API prices; on your plan it counts against your limits instead.</p></section>'
        )

    return (
        "<title>Pulse Night Team</title>\n"
        '<meta name="claude:viewport-fit-sides" content="cover">\n'
        '<link rel="stylesheet" href="https://fonts.googleapis.com/css2?family=IBM+Plex+Mono:wght@400;500&family=IBM+Plex+Sans:wght@400;600&display=swap">\n'
        f"<style>{CSS}</style>\n"
        '<main class="nt">\n<header><h1>Pulse Night Team</h1>'
        f'<div class="row"><span class="stamp">Last run {_when(report.get("started", ""))} · {report.get("minutes", 0)} min · '
        f"${report.get('cost', 0):.2f} usage</span>{main_pill}</div></header>\n"
        + "\n".join(s for s in sections if s)
        + "\n</main>\n"
    )
