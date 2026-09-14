#!/usr/bin/env bash
set -euo pipefail

# build-dmg.sh — produce dist/Banshee-<version>.dmg. Full flow: build → sign the
# app → notarize + staple the APP → assemble the DMG from the stapled app →
# sign → notarize + staple the DMG. Both artifacts are notarized (banshee-xw0):
# stapling the app needs its hash registered first, and Gatekeeper assesses the
# DMG in its own right on download — so a stapled app inside an unnotarized DMG
# is not enough. Two notary round-trips; each waits a few minutes.
#
# Usage:
#   ./scripts/build-dmg.sh          # full pipeline
#   ./scripts/build-dmg.sh dmg      # DMG + notarize only; reuses last .app
#   SKIP_NOTARIZE=1 ./scripts/build-dmg.sh   # signed-but-unnotarized DMG
#
# Identity comes from scripts/release.conf (gitignored; see
# release.conf.example). Without it: ad-hoc signing, notarization skipped —
# a fresh clone needs zero Apple credentials to produce a local DMG.
#
# Notarization authenticates one of two ways (scripts/lib/notary.sh): an App
# Store Connect API key (NOTARY_KEY_PATH/NOTARY_KEY_ID/NOTARY_ISSUER_ID — a
# file; works headless on the build server) or a keychain profile
# (NOTARIZE_PROFILE — interactive machines only; the data-protection keychain
# can become inaccessible to CLI tools after sleep/iCloud events). Hence:
#   1. a fail-fast pre-flight (seconds) before the multi-minute build, and
#   2. for the profile shape, self-healing when APPLE_ID / APPLE_TEAM_ID are
#      exported — notarytool prompts for the app-specific password, so this is
#      INTERACTIVE only; the build server uses the API-key shape instead.
# docs/build-pipeline.md has the full failure-mode table.
#
# BANSHEE_RELEASE_CONF points at the release.conf to use (default:
# scripts/release.conf). The build server keeps its copy OUTSIDE the checkout,
# since the runner's workspace is re-checked-out per run.

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
ROOT_DIR="$(dirname "$SCRIPT_DIR")"
APP_NAME="Banshee"
APP_DIR="$ROOT_DIR/build/$APP_NAME.app"
DIST_DIR="$ROOT_DIR/dist"

MODE="${1:-full}"

SIGNING_IDENTITY="-"
NOTARIZE_PROFILE=""
RELEASE_CONF="${BANSHEE_RELEASE_CONF:-$SCRIPT_DIR/release.conf}"
if [ -f "$RELEASE_CONF" ]; then
  # shellcheck source=/dev/null
  . "$RELEASE_CONF"
fi
# shellcheck source=lib/notary.sh
. "$SCRIPT_DIR/lib/notary.sh"

# The notarytool auth flags, or SKIP_NOTARIZE when nothing is configured. A
# half-configured API key is a hard error (notary.sh exits 2), not a fall-through.
SKIP_NOTARIZE="${SKIP_NOTARIZE:-}"
NOTARY_ARGS=()
if notary_lines="$(banshee_notary_args)"; then
  while IFS= read -r a; do NOTARY_ARGS+=("$a"); done <<< "$notary_lines"
else
  case $? in
    1) SKIP_NOTARIZE=1 ;;
    *) echo "build-dmg: ERROR: notarization credentials are misconfigured (see above)" >&2; exit 1 ;;
  esac
fi

VERSION="$(sed -n 's/.*static let marketing = "\(.*\)"/\1/p' \
  "$ROOT_DIR/swift/Sources/Banshee/Version.swift")"
[ -n "$VERSION" ] || { echo "build-dmg: cannot read version" >&2; exit 1; }
DMG_PATH="$DIST_DIR/$APP_NAME-$VERSION.dmg"

info() { printf 'build-dmg: %s\n' "$*"; }
die() {
  printf 'build-dmg: ERROR: %s\n' "$*" >&2
  exit 1
}

# ---------------------------------------------------------------------------
# Notarization pre-flight: fail in seconds, not after a five-minute build.
# ---------------------------------------------------------------------------
preflight_notary() {
  [ -n "$SKIP_NOTARIZE" ] && return 0
  if xcrun notarytool history "${NOTARY_ARGS[@]}" >/dev/null 2>&1; then
    info "notary pre-flight OK ($(banshee_notary_describe))"
    return 0
  fi
  if [ -n "${NOTARY_KEY_PATH:-}" ]; then
    # The API-key shape has nothing to self-heal: the file is there or it is not,
    # and Apple accepted the key or it did not. Say which.
    die "notary pre-flight failed with the API key ($(banshee_notary_describe)).
Check the key id / issuer id against App Store Connect, and that the key has the
Developer role. Or SKIP_NOTARIZE=1."
  fi
  info "notary profile '$NOTARIZE_PROFILE' is not accessible"
  if [ -n "${APPLE_ID:-}" ] && [ -n "${APPLE_TEAM_ID:-}" ]; then
    info "self-healing: re-provisioning the keychain profile"
    # Omit --password: passing it on the command line exposes the app-specific
    # password to any local user via `ps`. notarytool prompts for it interactively.
    xcrun notarytool store-credentials "$NOTARIZE_PROFILE" \
      --apple-id "$APPLE_ID" --team-id "$APPLE_TEAM_ID" --no-validate \
      || die "could not re-provision notary credentials"
    xcrun notarytool history --keychain-profile "$NOTARIZE_PROFILE" >/dev/null 2>&1 \
      || die "profile still inaccessible after re-provisioning"
    info "self-heal succeeded"
    return 0
  fi
  die "notary credentials unavailable. Either run any notarytool command
interactively to unlock the keychain, export APPLE_ID/APPLE_TEAM_ID for interactive
self-healing, or use SKIP_NOTARIZE=1."
}

# Submit one artifact to the notary and --wait, classifying the failure modes
# (each has a different recovery). Factored out because the app and the DMG are
# BOTH notarized now (banshee-xw0): stapling the app requires its hash to be
# registered, which requires notarizing it before it goes into the DMG, and the
# DMG must then be notarized in its own right or Gatekeeper rejects it on
# download. `$2` is a human label; `$3` is the artifact left behind for the
# recovery hint.
notarize_wait() {
  local artifact="$1" label="$2" leftover="$3" out status sub_id
  info "submitting the $label to Apple notary (this can take a few minutes)"
  set +e
  out="$(xcrun notarytool submit "$artifact" \
    "${NOTARY_ARGS[@]}" --wait 2>&1)"
  status=$?
  set -e
  if [ "$status" -eq 0 ] && printf '%s' "$out" | grep -q 'status: Accepted'; then
    return 0
  fi
  printf '%s\n' "$out" >&2
  if printf '%s' "$out" | grep -q 'No Keychain password'; then
    die "credential disappeared mid-build. Re-run; pre-flight self-heals if APPLE_* env vars are set. $leftover"
  elif printf '%s' "$out" | grep -qE 'id: [0-9a-f-]+'; then
    sub_id="$(printf '%s' "$out" | grep -oE 'id: [0-9a-f-]+' | head -1 | cut -d' ' -f2)"
    die "notary rejected the $label. Inspect:
  xcrun notarytool log $sub_id ${NOTARY_ARGS[*]}
$leftover"
  else
    die "network/transient notary failure on the $label. Retry. $leftover"
  fi
}

preflight_notary

# ---------------------------------------------------------------------------
# Build the app (unless reusing)
# ---------------------------------------------------------------------------
if [ "$MODE" != "dmg" ]; then
  # A real-identity build without SPARKLE_PUBLIC_KEY produces an app that can
  # never auto-update — the one thing we said we would never ship. Fail closed;
  # SKIP_SPARKLE=1 is the deliberate escape for a throwaway signed build.
  if [ "$SIGNING_IDENTITY" != "-" ] && [ -z "${SPARKLE_PUBLIC_KEY:-}" ] && [ -z "${SKIP_SPARKLE:-}" ]; then
    die "SPARKLE_PUBLIC_KEY is unset: this DMG could not auto-update (set it in release.conf, or SKIP_SPARKLE=1 to override)"
  fi
  "$SCRIPT_DIR/build-app.sh"
fi
[ -d "$APP_DIR" ] || die "no app bundle at $APP_DIR (run without 'dmg' first)"

WORK="$(mktemp -d /tmp/banshee-dmg.XXXXXX)"
cleanup() { rm -rf "${WORK:-}"; }
# On macOS bash 3.2 an EXIT trap turns a set -u abort into exit 0, so this
# script could crash and report success. See scripts/lib/completion-guard.sh.
# shellcheck source=lib/completion-guard.sh
. "$SCRIPT_DIR/lib/completion-guard.sh"
arm_completion_guard "build-dmg" cleanup

# ---------------------------------------------------------------------------
# Notarize + staple the APP, before it goes into the DMG (banshee-xw0)
# ---------------------------------------------------------------------------
# A staple ticket travels inside the bundle, so an app copied out of the DMG and
# first-launched OFFLINE is accepted without an online check. This requires
# stapling the app BEFORE assembling the DMG, which requires notarizing it
# first. Skipped in `dmg` mode (reusing a prior .app that a full run already
# stapled) and when notarization is off.
if [ "$MODE" != "dmg" ] && [ -z "$SKIP_NOTARIZE" ]; then
  APP_ZIP="$WORK/$APP_NAME.zip"
  info "zipping the app for notarization"
  # ditto, not zip: preserves symlinks, resource forks and the code signature
  # the way the notary expects.
  /usr/bin/ditto -c -k --keepParent "$APP_DIR" "$APP_ZIP"
  notarize_wait "$APP_ZIP" "app" "The signed (un-notarized) app is at $APP_DIR"
  info "stapling the app"
  xcrun stapler staple "$APP_DIR" >/dev/null
  xcrun stapler validate "$APP_DIR" >/dev/null || die "app staple validation failed"
fi

# ---------------------------------------------------------------------------
# DMG (built from the now-stapled app)
# ---------------------------------------------------------------------------
mkdir -p "$DIST_DIR"
rm -f "$DMG_PATH"
# License notices travel WITH the distribution (banshee-4z1), staged with their
# distribution names. Rust attribution from cargo-about (release.sh generates it
# before calling us; fallback here for a standalone run); Sparkle is embedded, so
# its own LICENSE (MIT + BSD components) must ride along too.
NOTICES_DIR="$WORK/notices"
mkdir -p "$NOTICES_DIR"
NOTICES="$ROOT_DIR/THIRD-PARTY-NOTICES.html"
if [ ! -f "$NOTICES" ]; then
  if command -v cargo-about >/dev/null; then
    info "generating THIRD-PARTY-NOTICES.html (standalone run)"
    (cd "$ROOT_DIR/rust" && cargo about generate about.hbs -o ../THIRD-PARTY-NOTICES.html) \
      || die "cargo-about failed; a DMG must carry current attribution"
  else
    die "THIRD-PARTY-NOTICES.html is missing and cargo-about is not installed; a DMG must carry attribution (scripts/doctor.sh)"
  fi
fi
cp "$NOTICES" "$NOTICES_DIR/THIRD-PARTY-NOTICES.html"
cp "$ROOT_DIR/LICENSE" "$NOTICES_DIR/LICENSE.txt"
SPARKLE_LICENSE_ARG=""
if [ -d "$APP_DIR/Contents/Frameworks/Sparkle.framework" ]; then
  SPARKLE_LICENSE="$ROOT_DIR/swift/.build/checkouts/Sparkle/LICENSE"
  [ -f "$SPARKLE_LICENSE" ] || die "Sparkle.framework is embedded but its LICENSE was not found at $SPARKLE_LICENSE"
  cp "$SPARKLE_LICENSE" "$NOTICES_DIR/Sparkle-LICENSE.txt"
  SPARKLE_LICENSE_ARG="$NOTICES_DIR/Sparkle-LICENSE.txt"
fi

# Styled DMG window (dark background + icon layout), the conventional shape: the app
# on the left, an arrow, the Applications alias on the right. Built with dmgbuild,
# which writes the .DS_Store DIRECTLY — reproducible, no Finder, works headless/CI
# (the older Finder+AppleScript approach could not run outside a GUI session with
# Automation permission). Falls back to a plain, valid, UNSTYLED DMG when dmgbuild
# is not installed.
VOLNAME="$APP_NAME $VERSION"
# dmgbuild sizes the window to the background's pixel dimensions, so use the
# 1x (660x400) image — a 2x image would make a giant window with the license
# icons and text overlapping. Slight retina softness on the bg is an accepted
# trade for a correctly-sized drag-to-install window.
BG_SRC="$ROOT_DIR/assets/dmg-background-1x.png"
[ -f "$BG_SRC" ] || die "a styled DMG needs assets/dmg-background.png (missing)"

DMGBUILD=""
if command -v dmgbuild >/dev/null; then
  DMGBUILD="dmgbuild"
elif command -v python3 >/dev/null && python3 -c 'import dmgbuild' 2>/dev/null; then
  DMGBUILD="python3 -m dmgbuild"
fi

if [ -n "$DMGBUILD" ]; then
  info "building styled DMG via dmgbuild"
  DMG_APP="$APP_DIR" DMG_BG="$BG_SRC" \
    DMG_NOTICES="$NOTICES_DIR/THIRD-PARTY-NOTICES.html" \
    DMG_LICENSE="$NOTICES_DIR/LICENSE.txt" \
    DMG_SPARKLE_LICENSE="$SPARKLE_LICENSE_ARG" \
    $DMGBUILD -s "$SCRIPT_DIR/dmg-settings.py" "$VOLNAME" "$DMG_PATH" \
    || die "dmgbuild failed"
else
  echo "build-dmg: WARNING: dmgbuild not installed — shipping an UNSTYLED but valid DMG. \`pipx install dmgbuild\` gives the designed window (scripts/doctor.sh)." >&2
  STAGING="$WORK/staging"
  mkdir -p "$STAGING"
  cp -R "$APP_DIR" "$STAGING/"
  ln -s /Applications "$STAGING/Applications"
  cp "$NOTICES_DIR/"* "$STAGING/"
  hdiutil create -volname "$VOLNAME" -srcfolder "$STAGING" -ov -format UDZO "$DMG_PATH" >/dev/null
fi

if [ "$SIGNING_IDENTITY" != "-" ]; then
  info "signing DMG"
  codesign --force --sign "$SIGNING_IDENTITY" "$DMG_PATH"
fi

# ---------------------------------------------------------------------------
# Notarize + staple the DMG (Gatekeeper assesses the DMG itself on download)
# ---------------------------------------------------------------------------
if [ -n "$SKIP_NOTARIZE" ]; then
  info "SKIP_NOTARIZE set (or no profile configured): DMG is NOT notarized."
  info "It works locally; Gatekeeper will warn on other Macs."
else
  notarize_wait "$DMG_PATH" "DMG" "A signed-but-unnotarized DMG is at $DMG_PATH"
  info "notarized; stapling the DMG"
  xcrun stapler staple "$DMG_PATH" >/dev/null
  xcrun stapler validate "$DMG_PATH" >/dev/null || die "DMG staple validation failed"
fi

info "done: $DMG_PATH"
finish 0
