#!/usr/bin/env bash
set -euo pipefail

# warm-dev-db.sh — give the dev database enough history for the app to show a real
# verdict, then report what it found.
#
# This replaced `seed-dev-db.sh`, and the rename is the point: **there is nothing
# for a client to seed.** Banshee's writer is a timer, not a person — the API is
# read-only, the sampler owns every insert — so the only way to get data is to let
# the daemon run. What this script does is wait for it, with the cadence turned up
# so waiting takes a second instead of half a minute.
#
# Boots a dev-profile API if one is not already running, and leaves the server in
# whatever state it found it.

ROOT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
BIN="$ROOT_DIR/rust/target/debug"
DEV_URL="${BANSHEE_API_URL:-http://127.0.0.1:18779}"
# The dev daemon's socket (ADR-0008). TCP is /health only — the daemon refuses the
# key there — so the CLI below must go over the socket, and BANSHEE_API_URL must NOT
# be exported to it (an explicit URL wins every client's resolution and would put it
# back on TCP, correctly locked out).
DEV_SOCKET="${BANSHEE_API_SOCKET:-$HOME/Library/Application Support/banshee/api-dev.sock}"

if [ ! -x "$BIN/banshee" ]; then
  echo "warm-dev-db.sh: building..."
  (cd "$ROOT_DIR/rust" && cargo build --workspace)
fi
command -v jq >/dev/null || { echo "warm-dev-db.sh: jq is required" >&2; exit 1; }

STARTED_PID=""
cleanup() {
  if [ -n "${STARTED_PID:-}" ]; then
    kill "$STARTED_PID" 2>/dev/null || true
    wait "$STARTED_PID" 2>/dev/null || true
  fi
}
# On macOS bash 3.2 an EXIT trap turns a set -u abort into exit 0, so this
# script could crash and report success. See scripts/lib/completion-guard.sh.
# shellcheck source=lib/completion-guard.sh
. "$ROOT_DIR/scripts/lib/completion-guard.sh"
arm_completion_guard "warm-dev-db" cleanup

if ! curl -sf "$DEV_URL/health" >/dev/null 2>&1; then
  echo "warm-dev-db.sh: starting a dev API..."
  # A fast cheap tier so the verdict leaves `Checking` in about a second; the
  # census still costs subprocesses, so it keeps its own slower cadence.
  BANSHEE_PROFILE=dev \
  BANSHEE_SAMPLE_INTERVAL_MS=200 \
  BANSHEE_CENSUS_INTERVAL_MS=5000 \
    "$BIN/banshee-api" >/dev/null 2>&1 &
  STARTED_PID=$!
  for _ in $(seq 1 50); do
    curl -sf "$DEV_URL/health" >/dev/null 2>&1 && break
    sleep 0.2
  done
fi

export BANSHEE_API_SOCKET="$DEV_SOCKET"
unset BANSHEE_API_URL
for _ in $(seq 1 50); do
  [ -S "$DEV_SOCKET" ] && break
  sleep 0.2
done
[ -S "$DEV_SOCKET" ] || { echo "warm-dev-db.sh: the dev daemon answers /health but has no socket at $DEV_SOCKET" >&2; finish 1; }

# Poll for a verdict rather than sleeping: a fixed sleep encodes an assumption
# about machine speed and is wrong in both directions.
echo "warm-dev-db.sh: waiting for the model to leave Checking..."
for _ in $(seq 1 100); do
  LEVEL="$("$BIN/banshee" status --json 2>/dev/null | jq -r .level 2>/dev/null || echo "")"
  [ -n "$LEVEL" ] && [ "$LEVEL" != "checking" ] && break
  sleep 0.2
done

echo
"$BIN/banshee" status --all
echo
"$BIN/banshee" stats
finish 0
