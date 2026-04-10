#!/bin/bash
set -eo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"

echo "Building pulse-fetcher sidecar..."

cd "$PROJECT_DIR"
. "$HOME/.cargo/env"

# Build release binary
cargo build --release -p pulse-fetcher

# Detect target triple dynamically (supports both ARM and Intel Macs)
TARGET_TRIPLE="$(rustc -vV | grep '^host:' | cut -d' ' -f2)"
mkdir -p src-tauri/binaries
cp "target/release/pulse-fetcher" "src-tauri/binaries/pulse-fetcher-${TARGET_TRIPLE}"

echo "✓ Sidecar built and copied to src-tauri/binaries/"
echo "  Binary: src-tauri/binaries/pulse-fetcher-${TARGET_TRIPLE}"
ls -lh "src-tauri/binaries/pulse-fetcher-${TARGET_TRIPLE}"
