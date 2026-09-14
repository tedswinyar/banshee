#!/usr/bin/env bash
set -euo pipefail

# check-version-alignment.sh — every version the project states must be ONE version.
#
# Usage: scripts/check-version-alignment.sh [<expected>]
#
# The project states its version in two places that nothing ties together:
#   swift/Sources/Banshee/Version.swift   `static let marketing = "X.Y.Z"`  (the app, the DMG)
#   rust/Cargo.toml [workspace.package]   `version = "X.Y.Z"`  (the daemon's /health,
#                                          `banshee health`, MCP serverInfo — via CARGO_PKG_VERSION)
# release.sh gated only the first, so 0.1.1 through 0.1.4 shipped a daemon that
# answered /health with "0.1.0" (banshee-87l.15). This script is the single check:
# both files must agree with each other, and with <expected> when one is given.
# Exit 0 iff aligned; exit 1 with every disagreement named on stderr.
#
# BANSHEE_VERSION_SWIFT / BANSHEE_CARGO_TOML override the file paths so the scripts
# suite can drive both directions against fixtures (test-version-alignment.sh).

ROOT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
SWIFT_FILE="${BANSHEE_VERSION_SWIFT:-$ROOT_DIR/swift/Sources/Banshee/Version.swift}"
CARGO_FILE="${BANSHEE_CARGO_TOML:-$ROOT_DIR/rust/Cargo.toml}"
EXPECTED="${1:-}"

fail() { printf 'version-alignment: %s\n' "$*" >&2; STATUS=1; }
STATUS=0

[ -f "$SWIFT_FILE" ] || { fail "no Version.swift at $SWIFT_FILE"; exit 1; }
[ -f "$CARGO_FILE" ] || { fail "no Cargo.toml at $CARGO_FILE"; exit 1; }

APP_VERSION="$(sed -n 's/.*static let marketing = "\(.*\)".*/\1/p' "$SWIFT_FILE")"
# The workspace version is the `version = "…"` line INSIDE [workspace.package];
# a crate table or a dependency line also says `version =`, so scope by section.
CARGO_VERSION="$(awk '
  /^\[/ { insec = ($0 == "[workspace.package]") ; next }
  insec && /^version[[:space:]]*=/ { gsub(/.*=[[:space:]]*"|".*/, ""); print; exit }
' "$CARGO_FILE")"

[ -n "$APP_VERSION" ]   || fail "Version.swift has no 'static let marketing = \"…\"' line"
[ -n "$CARGO_VERSION" ] || fail "Cargo.toml has no version under [workspace.package]"
[ "$STATUS" = 0 ] || exit 1

if [ "$APP_VERSION" != "$CARGO_VERSION" ]; then
  fail "Version.swift says '$APP_VERSION' but rust/Cargo.toml [workspace.package] says '$CARGO_VERSION' — the daemon would report a version the app does not"
fi
if [ -n "$EXPECTED" ]; then
  [ "$APP_VERSION" = "$EXPECTED" ]   || fail "Version.swift says '$APP_VERSION', release says '$EXPECTED'"
  [ "$CARGO_VERSION" = "$EXPECTED" ] || fail "rust/Cargo.toml says '$CARGO_VERSION', release says '$EXPECTED'"
fi

[ "$STATUS" = 0 ] && printf 'version-alignment: %s (Version.swift = Cargo.toml%s)\n' \
  "$APP_VERSION" "${EXPECTED:+ = release}"
exit "$STATUS"
