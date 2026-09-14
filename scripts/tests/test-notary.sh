#!/usr/bin/env bash
set -u

# test-notary.sh — tests OF scripts/lib/notary.sh, the one place notarytool's
# authentication is decided. The build server authenticates with an App Store
# Connect API key (a file; works from launchd), an operator's machine with a
# keychain profile (interactive only). A half-configured key must be a hard error,
# not a silent fall-through to the profile — that fall-through would fail minutes
# later on the build server, for the wrong reason.

SCRIPT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
# shellcheck source=../lib/notary.sh
. "$SCRIPT_DIR/lib/notary.sh"
PASS=0
FAIL=0
t() { local d="$1"; shift; if "$@" >/dev/null 2>&1; then PASS=$((PASS+1)); else FAIL=$((FAIL+1)); echo "  ✗ $d" >&2; fi; }

WORK="$(mktemp -d /tmp/banshee-notary-test.XXXXXX)"
trap 'rm -rf "$WORK"' EXIT
KEY="$WORK/AuthKey_ABC123.p8"; printf 'not really a key\n' > "$KEY"

# The function prints flags one per line; collect them the way callers do.
args() { # env... -> prints "rc:<n> argv:<joined by |>"
  local out rc
  out="$(env "$@" bash -c '. "$0/lib/notary.sh"; banshee_notary_args' "$SCRIPT_DIR" 2>/dev/null)"; rc=$?
  printf 'rc:%s argv:%s\n' "$rc" "$(printf '%s' "$out" | paste -sd'|' -)"
}
EXPECT_KEY="rc:0 argv:--key|$KEY|--key-id|ABC123|--issuer|11111111-2222-3333-4444-555555555555"

# 1. The API-key shape wins, and emits exactly notarytool's three flag pairs.
t "an API key yields --key/--key-id/--issuer" test \
  "$(args NOTARY_KEY_PATH="$KEY" NOTARY_KEY_ID=ABC123 NOTARY_ISSUER_ID=11111111-2222-3333-4444-555555555555)" = "$EXPECT_KEY"

# 2. …and beats a profile when both are set (the build server may carry both).
t "the API key is preferred over a keychain profile" test \
  "$(args NOTARY_KEY_PATH="$KEY" NOTARY_KEY_ID=ABC123 NOTARY_ISSUER_ID=11111111-2222-3333-4444-555555555555 NOTARIZE_PROFILE=banshee-notarize)" = "$EXPECT_KEY"

# 3. Profile alone → --keychain-profile.
t "a profile alone yields --keychain-profile" test \
  "$(args NOTARIZE_PROFILE=banshee-notarize)" = "rc:0 argv:--keychain-profile|banshee-notarize"

# 4. Nothing configured → exit 1, NOTHING printed (callers turn this into SKIP_NOTARIZE).
t "nothing configured is exit 1 with no output" test "$(args)" = "rc:1 argv:"

# 5. A PARTIAL key is exit 2 and prints nothing — never a fall-through to the profile.
for partial in "NOTARY_KEY_PATH=$KEY" "NOTARY_KEY_ID=ABC123" "NOTARY_ISSUER_ID=1-2-3" \
               "NOTARY_KEY_PATH=$KEY NOTARY_KEY_ID=ABC123"; do
  # shellcheck disable=SC2086
  t "a partial key ($partial) is a hard error, even with a profile present" test \
    "$(args $partial NOTARIZE_PROFILE=banshee-notarize)" = "rc:2 argv:"
done

# 6. A key path that does not exist is a hard error too (the file is the credential).
t "an unreadable key file is a hard error" test \
  "$(args NOTARY_KEY_PATH="$WORK/nope.p8" NOTARY_KEY_ID=ABC123 NOTARY_ISSUER_ID=1-2-3)" = "rc:2 argv:"

# 7. build-dmg.sh consumes the library (not its own --keychain-profile literal) for
#    pre-flight, submit and the log hint — three call sites, all through NOTARY_ARGS.
DMG="$SCRIPT_DIR/build-dmg.sh"
t "build-dmg sources notary.sh" grep -q 'lib/notary.sh' "$DMG"
t "build-dmg submits with NOTARY_ARGS" grep -q 'notarytool submit "\$artifact" \\' "$DMG"
t "build-dmg has no hardcoded --keychain-profile submit" bash -c '! grep -q "submit.*--keychain-profile" "$1"' _ "$DMG"
t "build-dmg pre-flights with NOTARY_ARGS" grep -q 'notarytool history "\${NOTARY_ARGS\[@\]}"' "$DMG"
t "build-dmg treats a misconfigured key as fatal, not as skip" grep -q 'notarization credentials are misconfigured' "$DMG"

# 8. The other build-server seams: release.conf location, Sparkle key file, signing keychain.
for f in build-app.sh build-dmg.sh publish.sh; do
  t "$f honours BANSHEE_RELEASE_CONF" grep -q 'BANSHEE_RELEASE_CONF' "$SCRIPT_DIR/$f"
done
t "publish.sh signs with --ed-key-file when SPARKLE_PRIVATE_KEY_FILE is set" \
  grep -q -- '--ed-key-file "\$SPARKLE_PRIVATE_KEY_FILE" "\$DMG"' "$SCRIPT_DIR/publish.sh"
t "publish.sh refuses an unreadable SPARKLE_PRIVATE_KEY_FILE" \
  grep -q 'SPARKLE_PRIVATE_KEY_FILE=\$SPARKLE_PRIVATE_KEY_FILE is not readable' "$SCRIPT_DIR/publish.sh"

echo "test-notary: $PASS passed, $FAIL failed"
[ "$FAIL" -eq 0 ]
