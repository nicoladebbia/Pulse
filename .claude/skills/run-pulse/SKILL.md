---
name: run-pulse
description: Launch and drive the Pulse app to see a change actually working — headless Chrome over CDP (real clicks, DOM measurement, console capture) for frontend/layout work, or the real Tauri app when Rust, SQL or live-DB behaviour matters. Use when asked to run the app, screenshot it, or confirm a UI change works for real rather than just compiling.
---

# Running Pulse

## Pick the path first — they verify different things

| You changed | Use | Because |
|---|---|---|
| Svelte layout, CSS, component structure, a client-side sort, an `$effect` | **A: browser + CDP** | Full clicking and DOM measurement. Fast. |
| A Tauri command, SQL in `src-tauri/`, anything reading the real DB | **B: real Tauri app** | The browser **cannot** reach Rust — see below. |

**The trap:** in a browser `isTauri()` is `false`, so every page renders
`src/lib/tauri/mock.ts` fixtures and **no Tauri command is ever invoked**. A green
browser run says nothing about Rust or SQL changes. Conversely path B can't be
clicked (see B's limits), so most frontend work is verified faster in path A.

For SQL changes, the practical combination is: **path B to confirm the page renders +
`sqlite3` against the live DB to confirm the numbers.**

---

## Path A — browser + CDP driver (clicking works here)

```bash
pnpm dev    # serves http://localhost:5173
```

Then drive it with the bundled zero-dependency driver. It launches its own headless
Chrome on a free port, drives it, and tears it down:

```bash
node .claude/skills/run-pulse/cdp.mjs \
  --url http://localhost:5173/freedoms/wealth \
  --ready 'button.block.w-full' \
  --out /tmp/shot.png \
  --eval 'document.querySelectorAll(".border-t.pt-6").length' \
  --click 'button.block.w-full' \
  --eval 'document.querySelectorAll(".border-t.pt-6").length'
```

Flags: `--url` (required), `--ready <sel>` gate before any step, `--click <sel>`,
`--eval <expr>`, `--out <png>`, `--wait <ms>`, `--viewport WxH`, `--timeout <ms>`.
`--click` and `--eval` run **in the order given**, so a before/after pair around a
click measures what you think it does. Exits non-zero if the page logged a console
error or threw. **Always look at the PNG** — a blank frame is a failed launch.

Requires Node ≥22 (global `WebSocket`; no `ws` package). Node 24 confirmed working.

### Verified recipes

Expand a freedoms card and prove the state actually changed:

```bash
node .claude/skills/run-pulse/cdp.mjs --url http://localhost:5173/freedoms/wealth \
  --ready 'button.block.w-full' \
  --eval 'document.querySelector("button.block.w-full").className.split(" ").pop()' \
  --click 'button.block.w-full' \
  --eval 'document.querySelector("button.block.w-full").className.split(" ").pop()'
# collapsed -> "pb-8", expanded -> "pb-6"
```

Unknown-route / runaway-`$effect` check — if an effect loops, the page never settles
and this hangs or reports errors:

```bash
node .claude/skills/run-pulse/cdp.mjs --url http://localhost:5173/freedoms/bogus \
  --wait 3000 --eval 'document.body.innerText.match(/Unknown freedom: \w+/)?.[0]'
# -> "Unknown freedom: bogus", console errors 0
```

### Measuring box geometry correctly

When checking that spacing sits *inside* a card, measure the panel's **last child**
against the card's bottom, not the panel's own rect — `getBoundingClientRect()`
includes padding, so a collapsed-out `margin-bottom` and a correct `padding-bottom`
both read as zero gap. The distinguishing number:

```js
--eval '(() => {
  const card = [...document.querySelectorAll("div")].find(d => d.className.includes("rounded-2xl"));
  const panel = card.querySelector(".border-t"), last = panel?.lastElementChild;
  return last ? Math.round(card.getBoundingClientRect().bottom - last.getBoundingClientRect().bottom) : null;
})()'
# ~32-34 = padding is inside the card (correct).  ~0 = a bottom margin escaped.
```

---

## Path B — the real Tauri app (real DB, but barely drivable)

```bash
pnpm tauri dev      # Rust build + native window, 1200x800
```

Reads the live DB at `~/Library/Application Support/com.pulse.app/pulse.db`
(~324 MB; the 8 AM launchd fetch populates it). Query it directly for ground truth —
plain tables need no sqlite-vec extension:

```bash
sqlite3 "$HOME/Library/Application Support/com.pulse.app/pulse.db" "SELECT ..."
```

### Screenshotting the window

```bash
osascript -e 'tell application "System Events" to tell process "pulse" to set frontmost to true'
osascript -e 'tell application "System Events" to tell process "pulse" to get {position, size} of window 1'
# -> 255, 107, 1200, 800
screencapture -x -o -R255,107,1200,800 /tmp/pulse.png
```

### Its limits — do not rediscover these

- **Not CDP-accessible.** Tauri uses WKWebView on macOS; there is no debugging port,
  so `cdp.mjs` cannot drive it.
- **Clicking does not work.** `System Events … click at {x, y}` fails with error
  `-25208`. `cliclick` was not installed as of 2026-07-25 — `brew install cliclick`
  is the fix if clicking the real app is ever needed.
- **Tab navigation is blind.** The AX tree exposes only `AXWebArea`;
  `AXFocusedUIElement` returns nothing usable inside the webview, so you cannot read
  back what is focused and Tab-counting is guesswork. It *will* silently land on the
  wrong page.
- **`Cmd+R` does not reload** the webview, so it cannot be used to reset focus.
- **What does work:** the app's own shortcuts, from
  `src/lib/components/layout/KeyboardHandler.svelte` — `f` → `/freedoms`,
  `p` → `/ask`, `?` → help, `/` → focus search, and on the home page `j`/`k`,
  `Enter`, `Escape`, `o`. **There is no shortcut for `/trends` or `/signals`**, so
  those pages currently cannot be reached programmatically in the real app.

Because of that last point, verifying a Trends/Signals change in the real app needs a
human click, or `cliclick`.

---

## Gotchas

- Chrome's one-shot `--headless --screenshot=…` often writes the PNG and then never
  exits, hanging the call. `cdp.mjs` manages Chrome's lifecycle instead — prefer it.
- Kill the dev server by pattern, not name: the process is
  `node …/vite/bin/vite.js dev`, so `pkill -f "vite dev"` matches nothing. Use
  `kill $(pgrep -f "vite.js dev")`.
- `pnpm tauri dev` starts its own vite on 5173; stop a standalone `pnpm dev` first.
