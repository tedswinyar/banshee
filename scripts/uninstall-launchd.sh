#!/usr/bin/env bash
set -euo pipefail

# uninstall-launchd.sh — remove the LaunchAgent.
#
# Exists because an install step without an uninstall is a trap: the stale plist
# that outlives a deleted checkout is the failure mode ADR-0004 warns about, and
# "just delete the plist" leaves a running process serving from a deleted inode
# until the next reboot.
#
# Usage:
#   ./scripts/uninstall-launchd.sh [--keep-data]
#
# Leaves the DATABASE alone by default — a day of samples is not something to
# discard as a side effect of unregistering a service. `--purge` removes the
# installed binary too; nothing here ever touches banshee.db.

ROOT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
# shellcheck source=lib/service.sh
. "$ROOT_DIR/scripts/lib/service.sh"

PURGE=0
case "${1:-}" in
    --purge) PURGE=1 ;;
    -h|--help) sed -n '3,16p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    "") ;;
    *) echo "uninstall-launchd.sh: unknown option: $1" >&2; exit 2 ;;
esac

say() { echo "banshee-daemon: $*"; }

TARGET="$(banshee_service_target)"

if launchctl print "$TARGET" >/dev/null 2>&1; then
    say "booting out $TARGET"
    launchctl bootout "$TARGET" 2>/dev/null || true
    # bootout is asynchronous. Poll rather than sleep, and say so if it lingers —
    # a plist removed while the process is still up leaves an orphan that nothing
    # will restart and nothing will stop.
    GONE=0
    for _ in $(seq 1 50); do
        if ! launchctl print "$TARGET" >/dev/null 2>&1; then GONE=1; break; fi
        sleep 0.1
    done
    [ "$GONE" = "1" ] || say "WARNING: still registered after 5s; check launchctl print $TARGET"
else
    say "not registered with launchd"
fi

if [ -f "$BANSHEE_PLIST" ]; then
    rm -f "$BANSHEE_PLIST"
    say "removed $BANSHEE_PLIST"
fi

if [ "$PURGE" = "1" ]; then
    for f in "$BANSHEE_INSTALLED_BIN" "$BANSHEE_INSTALLED_CLI" "$BANSHEE_INSTALLED_SHIM"; do
        if [ -e "$f" ]; then
            rm -f "$f"
            say "removed $f"
        fi
    done
fi

say "the database is untouched (a day of samples is not a side effect to discard)."
say "uninstalled."
