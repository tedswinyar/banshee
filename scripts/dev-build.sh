#!/usr/bin/env bash
set -euo pipefail

# dev-build.sh — fast loop for poking at the real app: debug-build the
# bundle, warm the dev database, and launch the app against the dev profile.

ROOT_DIR="$(cd "$(dirname "$0")/.." && pwd)"

"$ROOT_DIR/scripts/build-app.sh" --debug
"$ROOT_DIR/scripts/warm-dev-db.sh" || true

# The app finds the daemon by SOCKET (ADR-0008), and the dev daemon's socket is not
# the default one — without this it would connect to the prod daemon and the "dev
# profile" in the message above would be a lie. `open` does not pass its own
# environment to the app; only `--env` does.
DEV_SOCKET="${BANSHEE_API_SOCKET:-$HOME/Library/Application Support/banshee/api-dev.sock}"
echo "dev-build.sh: launching build/Banshee.app against the dev daemon at $DEV_SOCKET"
open --env "BANSHEE_API_SOCKET=$DEV_SOCKET" "$ROOT_DIR/build/Banshee.app"
