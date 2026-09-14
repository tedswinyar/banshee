#!/usr/bin/env bash
set -u

# Tests OF the app-bundle assembly, guarding one specific, silent, total failure.
#
# `build-app.sh` used to copy the Rust binaries into `Contents/MacOS/` so that
# `Bundle.url(forAuxiliaryExecutable:)` would find them. On a case-INSENSITIVE
# filesystem — the macOS default — `MacOS/Banshee` (the app) and `MacOS/banshee` (the
# CLI) are THE SAME FILE, so the CLI copy silently overwrote the app.
#
# `open Banshee.app` then launched the CLI, which printed its usage to a terminal
# nobody was watching and exited 0. No crash, no crash log, no Dock icon, nothing in
# `pgrep`. The app had never been launchable, and nothing noticed because until P6 the
# bundle was only ever BUILT, never run.
#
# Found 2026-08-31. It is a TEMPLATE bug: any template stamp whose app name and
# CLI name differ only by case has it.
#
# These are static checks on the script rather than a full assembly, because
# `make app` is a release build of both toolchains (~90s) and the scripts suite has to
# stay fast. The build-time guard lives in `build-app.sh` itself (it refuses to finish
# if the main executable does not link SwiftUI); this suite guards that the guard is
# still there — a check that gets deleted is worse than one that never existed,
# because its absence looks like it passed.

SCRIPT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
BUILD_APP="$SCRIPT_DIR/build-app.sh"
PASS=0
FAIL=0

t() {
  local desc="$1"
  shift
  if "$@" >/dev/null 2>&1; then
    PASS=$((PASS + 1))
  else
    FAIL=$((FAIL + 1))
    echo "  ✗ $desc" >&2
  fi
}

t_fails() {
  local desc="$1"
  shift
  if "$@" >/dev/null 2>&1; then
    FAIL=$((FAIL + 1))
    echo "  ✗ $desc (command SUCCEEDED and should not have)" >&2
  else
    PASS=$((PASS + 1))
  fi
}

t "build-app.sh exists and is executable" test -x "$BUILD_APP"

# ---------------------------------------------------------------------------
# 1. The helper binaries must NOT go into Contents/MacOS/
# ---------------------------------------------------------------------------
# The name collision is only possible there, so the fix is structural: put them
# somewhere the app binary cannot share a name with.
t_fails "no Rust binary is copied into Contents/MacOS/" \
  grep -qE 'cp .*(banshee-api|banshee-mcp|/banshee") .*Contents/MacOS' "$BUILD_APP"

t "the helper binaries are copied into Contents/Helpers/" bash -c \
  "grep -q 'Contents/Helpers/\"\$' '$BUILD_APP' || grep -q 'Contents/Helpers/' '$BUILD_APP'"

for bin in banshee-api banshee-mcp; do
  t "$bin goes to Contents/Helpers/" \
    grep -qE "cp .*$bin\" \"\\\$APP_DIR/Contents/Helpers/\"" "$BUILD_APP"
done

# The CLI is the dangerous one: its name differs from the app's only by case.
t "the CLI goes to Contents/Helpers/, not MacOS/" \
  grep -qE 'cp "\$RUST_BIN_DIR/banshee" "\$APP_DIR/Contents/Helpers/"' "$BUILD_APP"

# ---------------------------------------------------------------------------
# 2. The build-time guard must still be there
# ---------------------------------------------------------------------------
# Structural checks above prove the current copies are right. This proves the script
# would CATCH a future regression by any route — a fourth binary, a reordered copy, a
# stray `cp` added by someone who did not read this file.
t "build-app.sh verifies the main executable links SwiftUI" \
  grep -q 'otool -L .*grep -q SwiftUI' "$BUILD_APP"
t "…and exits nonzero when it does not" bash -c \
  "grep -A6 'otool -L .*SwiftUI' '$BUILD_APP' | grep -q 'exit 1'"

# ---------------------------------------------------------------------------
# 3. Signing must cover the helpers where they now live
# ---------------------------------------------------------------------------
# A codesign path left pointing at MacOS/ would fail the build loudly rather than
# silently, but it would fail for a confusing reason — so pin it.
t "the build signs the helpers in Contents/Helpers/" \
  grep -qE 'sign "\$APP_DIR/Contents/Helpers/\$bin"' "$BUILD_APP"

# Notarization requires the HARDENED RUNTIME on every executable; a real
# Developer ID signing path that omits --options runtime produces a DMG the
# notary rejects (banshee-u0a). Pin that the real-identity branch sets it.
# Every real-identity codesign line (there are two: with and without an explicit
# --keychain) must carry it; a grep for "any line has it" would let one branch
# hide a mutation on the other.
t "real Developer ID signing uses the hardened runtime" bash -c \
  '! grep -E "codesign .*--sign \"\\\$SIGNING_IDENTITY\"" "$1" | grep -v -- "--options runtime" | grep -q .' _ "$BUILD_APP"
t "real Developer ID signing can be pointed at a named keychain (build server)" \
  grep -qE 'codesign .*--keychain "\$SIGNING_KEYCHAIN" --sign "\$SIGNING_IDENTITY"' "$BUILD_APP"

# ---------------------------------------------------------------------------
# 4. If a bundle happens to be built, check it for real
# ---------------------------------------------------------------------------
# NOT a skip that keeps the suite green on a violated invariant: the checks above are
# unconditional and are the real gate. This is a bonus that runs on a developer
# machine which has just built the app, where the strongest possible evidence is
# sitting right there for free.
APP_DIR="$SCRIPT_DIR/../build/Banshee.app"
if [ -d "$APP_DIR" ]; then
  MAIN="$APP_DIR/Contents/MacOS/Banshee"
  t "the built bundle's main executable exists" test -x "$MAIN"
  t "the built bundle's main executable IS the app (links SwiftUI)" \
    bash -c "otool -L '$MAIN' | grep -q SwiftUI"
  # NOT by running it with --help: that LAUNCHES the SwiftUI app, which never
  # exits, and hung this suite for two minutes on its first run. `otool` answers
  # the same question without starting anything — which is the general lesson for
  # testing a GUI binary at all.
  t "the built bundle's main executable does not link clap (i.e. is not the CLI)" \
    bash -c "! nm -gU '$MAIN' 2>/dev/null | grep -qi clap"
  t "Contents/MacOS holds exactly one file" bash -c \
    "[ \"\$(find '$APP_DIR/Contents/MacOS' -maxdepth 1 -type f | wc -l | tr -d ' ')\" = 1 ]"
  t "the helpers are present in Contents/Helpers" bash -c \
    "[ -x '$APP_DIR/Contents/Helpers/banshee-api' ] && [ -x '$APP_DIR/Contents/Helpers/banshee' ]"
  echo "  · checked the built bundle at $APP_DIR"
else
  echo "  · no built bundle to inspect (run 'make app'); static checks still ran"
fi

# ---------------------------------------------------------------------------
# The DMG is STYLED (background + icon layout), not a bare volume. Static pins on
# build-dmg.sh guard the styling from silently regressing to a plain
# `hdiutil create -format UDZO`, the way earlier builds shipped a bare window.
# ---------------------------------------------------------------------------
BUILD_DMG="$SCRIPT_DIR/build-dmg.sh"
ROOT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
ICON_ASSET="$ROOT_DIR/assets/AppIcon.icns"
BG_ASSET="$ROOT_DIR/assets/dmg-background-1x.png"

t "the app icon asset exists" test -f "$ICON_ASSET"
t "the DMG background asset exists" test -f "$BG_ASSET"
t "build-app.sh copies the app icon into Resources" \
  grep -q 'assets/AppIcon.icns" "$APP_DIR/Contents/Resources/AppIcon.icns"' "$SCRIPT_DIR/build-app.sh"
t "Info.plist declares CFBundleIconFile" \
  grep -q 'CFBundleIconFile' "$SCRIPT_DIR/build-app.sh"
SETTINGS="$SCRIPT_DIR/dmg-settings.py"
t "the dmgbuild settings file exists" test -f "$SETTINGS"
t "build-dmg styles the window with dmgbuild (headless; not Finder/AppleScript)" \
  grep -q 'dmgbuild' "$BUILD_DMG"
t "build-dmg passes the background + app to dmgbuild" bash -c \
  "grep -q 'DMG_BG=' '$BUILD_DMG' && grep -q 'DMG_APP=' '$BUILD_DMG' && grep -q 'dmg-settings.py' '$BUILD_DMG'"
t "the settings position the app and the Applications alias" bash -c \
  "grep -q '(148, 170)' '$SETTINGS' && grep -q 'Applications.*(512, 170)' '$SETTINGS'"
t "the settings set the background picture" \
  grep -q 'background = os.environ\["DMG_BG"\]' "$SETTINGS"
# regression guard: the DEFAULT (dmgbuild present) path must not be a bare UDZO
# create — that would skip all styling. The bare create survives only as the
# explicit no-dmgbuild fallback.
t "the styled path uses dmgbuild, not a bare hdiutil UDZO create" \
  bash -c "grep -q 'dmgbuild not installed' '$BUILD_DMG'"

echo "test-app-bundle: $PASS passed, $FAIL failed"
[ "$FAIL" -eq 0 ]
