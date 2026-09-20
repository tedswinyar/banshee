#!/usr/bin/env bash
set -u

# test-bundle-scan.sh — tests OF scripts/lib/bundle-scan.sh, the check that a release
# bundle carries no builder home path. The first published DMG did, in all four
# executables (Cargo registry paths in the Rust helpers, checkout paths in the Swift
# app), and no source-tree gate could see it. This drives the scanner on fixture
# "binaries" (bytes with NULs) and then pins that build-app.sh still calls it for a
# release build — a deleted call looks exactly like a pass.

SCRIPT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
# shellcheck source=../lib/bundle-scan.sh
. "$SCRIPT_DIR/lib/bundle-scan.sh"
PASS=0
FAIL=0
t() { local d="$1"; shift; if "$@" >/dev/null 2>&1; then PASS=$((PASS+1)); else FAIL=$((FAIL+1)); echo "  ✗ $d" >&2; fi; }
t_fails() { local d="$1"; shift; if "$@" >/dev/null 2>&1; then FAIL=$((FAIL+1)); echo "  ✗ $d (succeeded and should not have)" >&2; else PASS=$((PASS+1)); fi; }

WORK="$(mktemp -d /tmp/banshee-bundle-scan-test.XXXXXX)"
trap 'rm -rf "$WORK"' EXIT
export HOME="$WORK/home-of-builder"   # so the $HOME check has a value that is not a real path
B="$WORK/Fixture.app/Contents"; mkdir -p "$B/MacOS" "$B/Helpers"

# 1. A clean bundle passes: real-looking bytes, the documented fixture user, no builder path.
printf 'MACH\0\0text /Users/example/Library/x \0 /cargo/registry/src/foo-1.0/lib.rs\0' > "$B/MacOS/App"
printf '<plist>CFBundleIdentifier com.example.app</plist>' > "$B/Info.plist"
t "a clean bundle passes" banshee_scan_bundle_for_paths "$WORK/Fixture.app"

# 2. A /Users/<name> path after a NUL byte is found and named. The name is assembled at
#    run time so this file itself carries no /Users/<name> literal for the residue gate.
FAKE_USER="alice"
printf 'MACH\0\0panicked at /Users/%s/.cargo/registry/src/x/lib.rs:3\0' "$FAKE_USER" > "$B/Helpers/tool"
OUT="$(banshee_scan_bundle_for_paths "$WORK/Fixture.app" 2>&1)"; RC=$?
t "a builder path inside a binary is refused" test "$RC" -eq 1
t "…and the offending file and path are named" bash -c "printf '%s' \"\$1\" | grep -q \"Helpers/tool carries a builder path: /Users/$FAKE_USER\"" _ "$OUT"
rm -f "$B/Helpers/tool"

# 3. $HOME outside /Users (a CI runner, a Linux cross-build) is refused too.
printf 'x\0%s/.cargo/foo\0' "$HOME" > "$B/Helpers/tool2"
t_fails "the builder's \$HOME is refused even when it is not under /Users" banshee_scan_bundle_for_paths "$WORK/Fixture.app"
rm -f "$B/Helpers/tool2"

# 4. Extra forbidden prefixes are honoured.
printf 'x\0/opt/build-farm/job-42/src\0' > "$B/Helpers/tool3"
t "an extra prefix is honoured" bash -c '! banshee_scan_bundle_for_paths "$1" /opt/build-farm' _ "$WORK/Fixture.app"
t "…and without it the same bytes pass" banshee_scan_bundle_for_paths "$WORK/Fixture.app"
rm -f "$B/Helpers/tool3"

# 5. A missing bundle is an error (2), never clean.
banshee_scan_bundle_for_paths "$WORK/nope.app" >/dev/null 2>&1; t "a missing bundle is exit 2" test $? -eq 2

# 6. build-app.sh still remaps both compilers and still runs the scan for a release build.
BA="$SCRIPT_DIR/build-app.sh"
t "build-app.sh remaps Rust paths (Cargo registry and the checkout)" \
  bash -c "grep -q -- '--remap-path-prefix=\$CARGO_HOME_DIR/registry/src=/cargo/registry' '$BA' && grep -q -- '--remap-path-prefix=\$ROOT_DIR=/banshee' '$BA'"
t "build-app.sh remaps Swift paths" grep -q -- '-file-prefix-map -Xswiftc "$ROOT_DIR=/banshee"' "$BA"
t "build-app.sh strips the linker's object-file paths (N_OSO) too" grep -q -- '-Xlinker -oso_prefix -Xlinker "$ROOT_DIR/"' "$BA"
t "build-app.sh names the native build system (swiftbuild embeds absolute .build/out paths)" \
  grep -q -- '--build-system native' "$BA"
t "build-app.sh refuses to finish a release bundle that fails the scan" \
  bash -c "grep -q 'banshee_scan_bundle_for_paths \"\$APP_DIR\"' '$BA' && grep -q 'refusing to finish' '$BA'"

echo "test-bundle-scan: $PASS passed, $FAIL failed"
[ "$FAIL" -eq 0 ]
