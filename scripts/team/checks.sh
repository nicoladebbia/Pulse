#!/bin/bash
# The same checks as .github/workflows/ci.yml, run in the current directory.
# The night team's runner uses its INSTALLED copy of this file, so a change
# under review cannot weaken the checks it is judged by. Keep in step with CI.
set -uo pipefail

# CI has none of these set, so the tests must not see them here either. (Night
# sessions carry them to keep the fetcher and the app from trading or spending.)
unset AUTO_TRADE_ENABLED EXIT_DRY_RUN AUTO_TRADE_PREVIEW LEARNING_AUTO_APPLY PULSE_LLM \
    PULSE_CLOUD_TASKS PULSE_SNAPSHOT PULSE_DB_PATH ALPACA_API_KEY ALPACA_SECRET_KEY \
    CEREBRAS_API_KEY EIA_API_KEY FEC_API_KEY FINNHUB_API_KEY FRED_API_KEY GROQ_API_KEY \
    TAVILY_API_KEY VOYAGE_API_KEY PULSE_FETCHER

run() {
    local name="$1"; shift
    echo "== $name"
    if ! "$@"; then
        echo "CHECK FAILED: $name"
        exit 1
    fi
}

run "frontend install" pnpm install --frozen-lockfile
run "type check" pnpm check
run "frontend tests" pnpm test

# tauri-build wants the frontend dist folder and the sidecar for this machine
# to exist (see the comments in ci.yml).
mkdir -p build src-tauri/binaries
run "build fetcher" cargo build -p pulse-fetcher --locked
run "sidecar copy" cp "${CARGO_TARGET_DIR:-target}/debug/pulse-fetcher" \
    "src-tauri/binaries/pulse-fetcher-$(rustc -vV | awk '/^host:/{print $2}')"

run "clippy" cargo clippy --workspace --all-targets --locked -- -D warnings
run "rust tests" cargo test --workspace --locked
echo "ALL CHECKS PASSED"
