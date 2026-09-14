#!/usr/bin/env bash
set -euo pipefail

# start.sh — run the API server in the dev profile, building if needed.
# The dev profile uses its own database (banshee-dev.db) and port
# (18779), so it never touches prod data.
#
# Usage: ./scripts/start.sh [--prod]

ROOT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
PROFILE="dev"
if [ "${1:-}" = "--prod" ]; then
  PROFILE="prod"
fi

BIN="$ROOT_DIR/rust/target/debug/banshee-api"
if [ ! -x "$BIN" ]; then
  echo "start.sh: building banshee-api..."
  (cd "$ROOT_DIR/rust" && cargo build -p banshee-api)
fi

echo "start.sh: starting banshee-api ($PROFILE profile); Ctrl-C to stop"
exec env BANSHEE_PROFILE="$PROFILE" "$BIN"
