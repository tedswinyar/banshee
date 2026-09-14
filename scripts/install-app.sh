#!/usr/bin/env bash
set -euo pipefail
# install-app.sh — put build/Banshee.app into /Applications and relaunch it.
#
# Half of `make install`. The other half is `make daemon-install`. They are ONE
# target on purpose (maintainer decision, 2026-09-13): the daemon and the app only agree when they
# are built from the same commit, and an app left behind by a daemon reinstall shows
# less than the terminal does — the wire format is lenient, so nothing fails; the
# popover just quietly lags. `make app` alone does NOT touch /Applications.
ROOT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
SRC="$ROOT_DIR/build/Banshee.app"
DEST="${BANSHEE_APP_DEST:-/Applications/Banshee.app}"
case "$DEST" in *.app) ;; *) printf 'install-app: ERROR: BANSHEE_APP_DEST must name a .app bundle (got %s)\n' "$DEST" >&2; exit 1 ;; esac
say() { printf 'install-app: %s\n' "$*"; }
die() { printf 'install-app: ERROR: %s\n' "$*" >&2; exit 1; }

[ -d "$SRC" ] || die "no $SRC — run ./scripts/build-app.sh (make app) first"
# Refuse a bundle whose main executable is not the app (the case-insensitive
# Contents/MacOS collision put the CLI there once; build-app.sh guards it, this
# re-checks the artifact we are about to install, not the one we meant to build).
otool -L "$SRC/Contents/MacOS/Banshee" | grep -q SwiftUI || die "$SRC/Contents/MacOS/Banshee does not link SwiftUI — not the app"

# Quit the running app gracefully, then wait for it — a `cp` over a running bundle
# is how you get a half-old, half-new app that crashes on the next click.
if pgrep -qf "$DEST/Contents/MacOS/Banshee"; then
  say "quitting the running app"
  osascript -e 'tell application "Banshee" to quit' >/dev/null 2>&1 || true
  for _ in $(seq 1 20); do pgrep -qf "$DEST/Contents/MacOS/Banshee" || break; sleep 0.5; done
  pgrep -qf "$DEST/Contents/MacOS/Banshee" && { say "still running; killing"; pkill -f "$DEST/Contents/MacOS/Banshee" || true; sleep 1; }
fi

# Stage beside the destination, then swap: the old bundle is never half-replaced.
INCOMING="${DEST%.app}.incoming.app"
rm -rf "$INCOMING"
ditto "$SRC" "$INCOMING"
rm -rf "$DEST"
mv "$INCOMING" "$DEST"
say "installed $DEST ($(/usr/libexec/PlistBuddy -c 'Print :CFBundleShortVersionString' "$DEST/Contents/Info.plist" 2>/dev/null || echo '?'))"

open "$DEST"
for _ in $(seq 1 20); do pgrep -qf "$DEST/Contents/MacOS/Banshee" && break; sleep 0.5; done
pgrep -qf "$DEST/Contents/MacOS/Banshee" || die "the app did not start; try: open $DEST"
say "running: pid $(pgrep -f "$DEST/Contents/MacOS/Banshee" | head -1)"
