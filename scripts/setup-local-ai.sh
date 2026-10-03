#!/bin/sh
# One-time setup for running Pulse on local models (PULSE_LLM=local).
# Needs Ollama (https://ollama.com, or `brew install --cask ollama`) and ~24 GB free.
set -e
cd "$(dirname "$0")/.."

if ! curl -sf http://localhost:11434/api/version >/dev/null; then
  echo "Ollama isn't running. Open the Ollama app, then re-run this script." >&2
  exit 1
fi

ollama pull qwen3.6:35b-a3b
ollama pull embeddinggemma
ollama create pulse-local -f ollama/Modelfile

touch .env
grep -q '^PULSE_LLM=' .env || echo 'PULSE_LLM=local' >> .env
echo
echo "Done. Pulse now runs on local models (PULSE_LLM=local in .env)."
echo "If this database already has Voyage search vectors, rebuild them once with:"
echo "  ./target/debug/pulse-fetcher --mode reembed"
