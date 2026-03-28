#!/bin/bash
set -e

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"

echo "Building pulse-fetcher sidecar..."

cd "$PROJECT_DIR"
. "$HOME/.cargo/env"

# Build release binary
cargo build --release -p pulse-fetcher

# Copy to sidecar location with target triple
TARGET_TRIPLE="aarch64-apple-darwin"
cp "target/release/pulse-fetcher" "src-tauri/binaries/pulse-fetcher-${TARGET_TRIPLE}"

echo "✓ Sidecar built and copied to src-tauri/binaries/"
echo "  Binary: src-tauri/binaries/pulse-fetcher-${TARGET_TRIPLE}"
ls -lh "src-tauri/binaries/pulse-fetcher-${TARGET_TRIPLE}"
