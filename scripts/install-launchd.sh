#!/bin/bash
set -e

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"
PLIST_SRC="$PROJECT_DIR/launchd/com.pulse.daily-fetch.plist"
PLIST_DEST="$HOME/Library/LaunchAgents/com.pulse.daily-fetch.plist"
LOG_DIR="$HOME/Library/Logs/Pulse"
APP_DATA="$HOME/Library/Application Support/com.pulse.app"

echo "Installing Pulse daily fetch agent..."

# Create directories
mkdir -p "$LOG_DIR"
mkdir -p "$APP_DATA"

# Copy plist (API keys are loaded from .env by the fetcher binary at runtime)
cp "$PLIST_SRC" "$PLIST_DEST"

# Unload if already loaded
launchctl unload "$PLIST_DEST" 2>/dev/null || true

# Load the agent
launchctl load "$PLIST_DEST"

echo "✓ Pulse daily fetch agent installed"
echo "  Plist: $PLIST_DEST"
echo "  Logs:  $LOG_DIR/"
echo "  Data:  $APP_DATA/"
echo ""
echo "The fetch will run daily at 8:00 AM."
echo "To test now: launchctl start com.pulse.daily-fetch"
