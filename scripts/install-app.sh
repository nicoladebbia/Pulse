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

# Install a script atomically: cp into the same directory, then rename. Overwriting in place
# reuses the inode, and bash reads a running script by byte offset, so a briefing that was
# mid-run kept reading the NEW file at its OLD position (a real "syntax error near unexpected
# token" killed the evening run on 2026-10-05). A rename leaves the running copy intact.
install_script() {
  cp "$SCRIPT_DIR/$1" "$APP_DATA/.$1.new"
  chmod +x "$APP_DATA/.$1.new"
  mv -f "$APP_DATA/.$1.new" "$APP_DATA/$1"
}

# (Re)load a launch agent. bootout SIGTERMs a job that is running right now (a briefing takes
# 20-27 minutes), so it only happens when the agent file actually changed or isn't loaded.
install_agent() {
  label="$1"; template="$2"; target="$3"
  sed "s#__HOME__#$HOME#g" "$template" > "$target.new"
  if [ -f "$target" ] && cmp -s "$target" "$target.new" \
     && launchctl print "gui/$(id -u)/$label" >/dev/null 2>&1; then
    rm -f "$target.new"
    return
  fi
  mv -f "$target.new" "$target"
  launchctl bootout "gui/$(id -u)/$label" 2>/dev/null || true
  # bootout is asynchronous: wait until the job is really gone before bootstrapping again.
  for _ in 1 2 3 4 5 6 7 8 9 10; do
    launchctl print "gui/$(id -u)/$label" >/dev/null 2>&1 || break
    sleep 1
  done
  launchctl bootstrap "gui/$(id -u)" "$target"
}

install_script scheduled-fetch.sh
install_agent com.pulse.scheduled-fetch "$PROJECT_DIR/launchd/com.pulse.scheduled-fetch.plist" "$AGENT"

install_script live-signals.sh
install_agent com.pulse.live-signals "$PROJECT_DIR/launchd/com.pulse.live-signals.plist" "$LIVE_AGENT"

for old in com.pulse.daily-fetch com.pulse.embedding-backfill; do
  if launchctl print "gui/$(id -u)/$old" >/dev/null 2>&1; then
    echo "! $old (the checkout-based schedule) is also loaded. Two schedulers will"
    echo "  run; remove it with: launchctl bootout gui/$(id -u)/$old"
  fi
done

echo "✓ Pulse installed in /Applications"
echo "✓ Briefings scheduled for 08:00 and 21:00 (log: ~/Library/Logs/Pulse/fetch-stdout.log)"
echo "✓ Live signals every 5 minutes during US market hours (log: ~/Library/Logs/Pulse/live-stdout.log)"
