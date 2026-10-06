#!/bin/bash
# Install Pulse.app into /Applications and schedule the 08:00 / 21:00 briefings.
#
#   scripts/install-app.sh            # download the latest GitHub release
#   scripts/install-app.sh Pulse.app  # install a local build instead
#
# After this, updates come from the "Update" button inside the app.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"
APP_DATA="$HOME/Library/Application Support/com.pulse.app"
AGENT="$HOME/Library/LaunchAgents/com.pulse.scheduled-fetch.plist"
LIVE_AGENT="$HOME/Library/LaunchAgents/com.pulse.live-signals.plist"
REPO="nicoladebbia/Pulse"

mkdir -p "$APP_DATA" "$HOME/Library/Logs/Pulse" "$HOME/Library/LaunchAgents"

if [ $# -ge 1 ]; then
  SRC="$1"
else
  TMP="$(mktemp -d)"
  trap 'rm -rf "$TMP"' EXIT
  echo "Downloading the latest Pulse release..."
  gh release download --repo "$REPO" --pattern '*.app.tar.gz' --dir "$TMP"
  tar -xzf "$TMP"/*.app.tar.gz -C "$TMP"
  SRC="$TMP/Pulse.app"
fi

osascript -e 'quit app "Pulse"' 2>/dev/null || true
# Copy beside the old app first, so a failed copy never leaves you without one.
rm -rf /Applications/.Pulse.app.new
cp -R "$SRC" /Applications/.Pulse.app.new
rm -rf /Applications/Pulse.app
mv /Applications/.Pulse.app.new /Applications/Pulse.app
# Not notarized (no Apple developer account): clear the download quarantine so
# Gatekeeper opens it. Later in-app updates don't get quarantined.
xattr -dr com.apple.quarantine /Applications/Pulse.app 2>/dev/null || true

# Installed builds have no checkout to read .env from; they read it here.
if [ ! -f "$APP_DATA/.env" ] && [ -f "$PROJECT_DIR/.env" ]; then
  cp "$PROJECT_DIR/.env" "$APP_DATA/.env"
  chmod 600 "$APP_DATA/.env"
fi

cp "$SCRIPT_DIR/scheduled-fetch.sh" "$APP_DATA/scheduled-fetch.sh"
sed "s#__HOME__#$HOME#g" "$PROJECT_DIR/launchd/com.pulse.scheduled-fetch.plist" > "$AGENT"
launchctl bootout "gui/$(id -u)/com.pulse.scheduled-fetch" 2>/dev/null || true
launchctl bootstrap "gui/$(id -u)" "$AGENT"

cp "$SCRIPT_DIR/live-signals.sh" "$APP_DATA/live-signals.sh"
sed "s#__HOME__#$HOME#g" "$PROJECT_DIR/launchd/com.pulse.live-signals.plist" > "$LIVE_AGENT"
launchctl bootout "gui/$(id -u)/com.pulse.live-signals" 2>/dev/null || true
launchctl bootstrap "gui/$(id -u)" "$LIVE_AGENT"

for old in com.pulse.daily-fetch com.pulse.embedding-backfill; do
  if launchctl print "gui/$(id -u)/$old" >/dev/null 2>&1; then
    echo "! $old (the checkout-based schedule) is also loaded. Two schedulers will"
    echo "  run; remove it with: launchctl bootout gui/$(id -u)/$old"
  fi
done

echo "✓ Pulse installed in /Applications"
echo "✓ Briefings scheduled for 08:00 and 21:00 (log: ~/Library/Logs/Pulse/fetch-stdout.log)"
echo "✓ Live signals every 5 minutes during US market hours (log: ~/Library/Logs/Pulse/live-stdout.log)"
