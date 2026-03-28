#!/bin/bash
set -e

PLIST_DEST="$HOME/Library/LaunchAgents/com.pulse.daily-fetch.plist"

echo "Uninstalling Pulse daily fetch agent..."

launchctl unload "$PLIST_DEST" 2>/dev/null || true
rm -f "$PLIST_DEST"

echo "✓ Agent uninstalled"
