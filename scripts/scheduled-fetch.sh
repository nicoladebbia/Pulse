#!/bin/bash
# Morning (08:00) and evening (21:00) briefings for an installed Pulse.app.
#
# launchd fires this every hour (a LaunchAgent with an `Hour` key never fired
# on this setup, see com.pulse.embedding-backfill.plist), and the script decides
# whether a slot is due. A slot missed while the Mac slept runs at the next wake:
# morning any time from 08:00, evening any time from 21:00, each once a day.
set -uo pipefail

FETCHER="${PULSE_FETCHER:-/Applications/Pulse.app/Contents/MacOS/pulse-fetcher}"
DB="$HOME/Library/Application Support/com.pulse.app/pulse.db"
STATE_DIR="$HOME/Library/Application Support/com.pulse.app/schedule"
mkdir -p "$STATE_DIR"

[ -x "$FETCHER" ] || { echo "$(date) pulse-fetcher not found at $FETCHER"; exit 0; }

today="$(date +%F)"
hour=$((10#$(date +%H)))

# Today's daily briefings that actually hold stories. The fetcher exits 0 for
# skips too (lock held, already fetched, provider unreachable, zero news), so a
# slot only counts as done when this number goes up.
briefings_today() {
  sqlite3 "$DB" "SELECT count(*) FROM briefings WHERE date = '$today'
    AND briefing_type = 'daily' AND story_count > 0" 2>/dev/null || echo 0
}

run_slot() { # name, extra args...
  local name="$1"; shift
  local stamp="$STATE_DIR/$name"
  [ "$(cat "$stamp" 2>/dev/null)" = "$today" ] && return 0
  echo "$(date) starting $name briefing"
  local before after
  before="$(briefings_today)"
  "$FETCHER" --mode daily "$@"
  local code=$?
  after="$(briefings_today)"
  if [ "$after" -gt "$before" ]; then
    echo "$today" > "$stamp"
    echo "$(date) $name briefing done"
  else
    echo "$(date) $name briefing not made (exit $code), next hour retries"
  fi
}

if [ "$hour" -ge 21 ]; then
  # The morning one already exists by now, so the evening run needs --force
  # (multiple briefings per day are allowed; each gets its own time label).
  if [ "$(cat "$STATE_DIR/morning" 2>/dev/null)" = "$today" ]; then
    run_slot evening --force
  else
    run_slot morning
  fi
elif [ "$hour" -ge 8 ]; then
  run_slot morning
fi
