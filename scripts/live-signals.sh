#!/bin/bash
# Live signals for an installed Pulse.app: every 5 minutes while the US market
# is open, pull new SEC filings and rescore signals (no AI calls). From 10:00 to
# 15:30 New York time a stock that newly turns buy-grade goes straight to
# auto-trade instead of waiting for the next scheduled run.
#
# launchd (com.pulse.live-signals) fires this at minutes 5, 10, ... 50. Minutes 55-0
# are left to scheduled-fetch.sh, whose briefing would otherwise find the shared
# fetcher lock taken. A run takes about 2-3 minutes; launchd never starts a
# second copy while one is still running.
set -uo pipefail

FETCHER="${PULSE_FETCHER:-/Applications/Pulse.app/Contents/MacOS/pulse-fetcher}"
STATE_DIR="$HOME/Library/Application Support/com.pulse.app/schedule"
mkdir -p "$STATE_DIR"

[ -x "$FETCHER" ] || exit 0
# Fetchers before live-signals ran the full daily pipeline for unknown modes:
# do nothing until the app has been updated.
"$FETCHER" --help 2>/dev/null | grep -q -- --live-trade || exit 0

et_weekday="$(TZ=America/New_York date +%u)"
et_hour=$((10#$(TZ=America/New_York date +%H)))
et_minute=$((et_hour * 60 + 10#$(TZ=America/New_York date +%M)))

# Weekdays 09:30-15:59 New York time; the Alpaca clock catches holidays.
[ "$et_weekday" -le 5 ] || exit 0
[ "$et_minute" -ge $((9 * 60 + 30)) ] && [ "$et_minute" -lt $((16 * 60)) ] || exit 0
# Exit 3 means closed; any other failure (no Alpaca keys, network) still
# refreshes signals but doesn't trade.
"$FETCHER" --mode market-open >/dev/null 2>&1
clock=$?
[ "$clock" -eq 3 ] && exit 0

args=(--mode live-signals)
# The slower government and financial sources once an hour.
hour_key="$(TZ=America/New_York date +%F)-$et_hour"
full=0
if [ "$(cat "$STATE_DIR/live-full" 2>/dev/null)" != "$hour_key" ]; then
  args+=(--full-sources)
  full=1
fi
if [ "$clock" -eq 0 ] && [ "$et_minute" -ge $((10 * 60)) ] && [ "$et_minute" -le $((15 * 60 + 30)) ]; then
  args+=(--live-trade)
fi

"$FETCHER" "${args[@]}"
rc=$?
if [ "$rc" -eq 0 ] && [ "$full" -eq 1 ]; then
  echo "$hour_key" > "$STATE_DIR/live-full"
fi
case "$rc" in
  0) ;;
  75) echo "$(date) live-signals skipped: another fetch is running" ;;
  *) echo "$(date) live-signals failed (exit $rc)" ;;
esac
