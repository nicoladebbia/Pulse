#!/bin/bash
# Morning (08:00) and evening (21:00) briefings for an installed Pulse.app,
# plus paper-trading upkeep during US market hours.
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

# Paper trading while the US market is open (weekdays, 10:00–15:59 New York
# time): settle fills and run stops every hour, look for new buys twice a day
# (once from 10:00, once from 13:00, so a signal found after the morning run
# can still be bought the same day).
# Quick, no AI calls, and first so a long briefing below can't delay it.
et_hour=$((10#$(TZ=America/New_York date +%H)))
et_weekday="$(TZ=America/New_York date +%u)"
# The Alpaca clock catches holidays; it also fails (skipping trading) when no
# Alpaca keys are set.
if [ "$et_weekday" -le 5 ] && [ "$et_hour" -ge 10 ] && [ "$et_hour" -le 15 ] \
  && "$FETCHER" --mode market-open >/dev/null 2>&1; then
  echo "$(date) market open: managing paper positions"
  "$FETCHER" --mode manage-positions
  rc=$?
  [ "$rc" -eq 0 ] || echo "$(date) manage-positions failed (exit $rc)"
  if [ "$et_hour" -lt 13 ]; then slot="$today-am"; else slot="$today-pm"; fi
  if [ "$(cat "$STATE_DIR/auto-trade" 2>/dev/null)" != "$slot" ]; then
    "$FETCHER" --mode auto-trade
    rc=$?
    if [ "$rc" -eq 0 ]; then
      echo "$slot" > "$STATE_DIR/auto-trade"
    else
      echo "$(date) auto-trade failed (exit $rc), next hour retries"
    fi
  fi
fi

# Weekly signal scorecard (which signal types and score ranges beat SPY), once
# per ISO week at the first run that finds it due. A few Alpaca calls, no AI.
week="$(date +%G-W%V)"
if [ "$(cat "$STATE_DIR/scorecard" 2>/dev/null)" != "$week" ]; then
  if "$FETCHER" --mode scorecard; then
    echo "$week" > "$STATE_DIR/scorecard"
  else
    echo "$(date) scorecard failed, next hour retries"
  fi
fi

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
