#!/usr/bin/env bash
set -u

# Tests OF scripts/lib/entitlements.sh — the paperwork between Banshee.entitlements
# and the provisioning profile that authorises it (banshee-jie).
#
# Why these matter: every failure the library refuses is one codesign ACCEPTS. A
# Developer ID app sealed with the time-sensitive entitlement and a missing, wrong,
# expired or non-Developer-ID profile signs, notarizes, staples, ships — and is killed
# by amfid at launch with no dialog. The only cheap place to catch that is before the
# seal, by reading what the profile grants; these tests hand the pure checks synthetic
# decoded profiles (a real one is a CMS blob only `security` can produce).

SCRIPT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
ROOT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
# shellcheck source=../lib/entitlements.sh
. "$SCRIPT_DIR/lib/entitlements.sh"

# Start from a clean env, not the caller's (CLAUDE.md → Gotchas, 2026-09-14): the
# decoder reads the build keychain's path from the environment, and this suite also
# runs ON the build server, inside a release job that has those variables set and a
# real build keychain in $HOME. Every decoder case below names its own keychain path.
unset BANSHEE_BUILD_KEYCHAIN BANSHEE_BUILD_KEYCHAIN_PASSWORD_FILE SIGNING_KEYCHAIN

PASS=0
FAIL=0
t() {
  local desc="$1"; shift
  if "$@" >/dev/null 2>&1; then PASS=$((PASS + 1)); else FAIL=$((FAIL + 1)); echo "  ✗ $desc" >&2; fi
}
t_fails() {
  local desc="$1"; shift
  if "$@" >/dev/null 2>&1; then FAIL=$((FAIL + 1)); echo "  ✗ $desc (command SUCCEEDED and should not have)" >&2; else PASS=$((PASS + 1)); fi
}

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

# Helpers in THIS shell rather than `bash -c` strings: a sourced function and its
# variables are invisible to a child bash unless exported, and the first draft of
# this file passed two "reason" cases by accident that way (the unexported key made
# every profile fail at the grant step with a message that happened to match).
team_is() { [ "$(banshee_team_id_from_identity "$1")" = "$2" ]; }
# refusal_mentions <plist> <bundle_id> <team> <pattern>: refused AND the reason says why.
refusal_mentions() {
  local out
  if out="$(banshee_check_profile_plist "$1" "$2" "$3" 2>&1)"; then return 1; fi
  printf '%s\n' "$out" | grep -q -- "$4"
}
pb_is() { [ "$(/usr/libexec/PlistBuddy -c "Print :$2" "$1" 2>/dev/null)" = "$3" ]; }
key_count() { [ "$(plutil -convert json -o - "$1" | tr ',' '\n' | grep -c '"com\.')" = "$2" ]; }

# A decoded Developer ID profile, as `security cms -D` would print it. Arguments let
# each test break exactly one thing.
profile() { # profile <out> <all-devices true|false> <grants-ts true|false|absent> <app-id> <expiry>
  local out="$1" all="$2" ts="$3" appid="$4" exp="$5"
  local ts_xml=""
  case "$ts" in
    true) ts_xml="<key>$BANSHEE_TIME_SENSITIVE_KEY</key><true/>" ;;
    false) ts_xml="<key>$BANSHEE_TIME_SENSITIVE_KEY</key><false/>" ;;
    absent) ts_xml="" ;;
  esac
  cat > "$out" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Name</key><string>Banshee Developer ID</string>
  <key>TeamIdentifier</key><array><string>TEAMID1234</string></array>
  <key>ProvisionsAllDevices</key><$all/>
  <key>ExpirationDate</key><date>$exp</date>
  <key>Entitlements</key>
  <dict>
    <key>com.apple.application-identifier</key><string>$appid</string>
    <key>com.apple.developer.team-identifier</key><string>TEAMID1234</string>
    $ts_xml
  </dict>
</dict>
</plist>
PLIST
}
FUTURE="2099-01-01T00:00:00Z"
PAST="2020-01-01T00:00:00Z"
APP="TEAMID1234.com.tedswinyar.banshee"

# ---------------------------------------------------------------------------
# 1. Team ID from the identity string
# ---------------------------------------------------------------------------
t "the Team ID is read from a Developer ID identity" \
  team_is "Developer ID Application: Some Person (ABCDE12345)" ABCDE12345
t_fails "an ad-hoc identity has no Team ID" banshee_team_id_from_identity "-"
t_fails "an identity without a (TEAMID) suffix has no Team ID" banshee_team_id_from_identity "Developer ID Application: Some Person"
t_fails "an empty identity has no Team ID" banshee_team_id_from_identity ""

# ---------------------------------------------------------------------------
# 2. The profile checks — one good profile, then one defect at a time
# ---------------------------------------------------------------------------
profile "$WORK/good.plist" true true "$APP" "$FUTURE"
t "a Developer ID profile granting the entitlement for this App ID is accepted" \
  banshee_check_profile_plist "$WORK/good.plist" com.tedswinyar.banshee TEAMID1234

profile "$WORK/dev.plist" false true "$APP" "$FUTURE"
t_fails "a profile that does not provision all devices (Development / App Store) is refused" \
  banshee_check_profile_plist "$WORK/dev.plist" com.tedswinyar.banshee TEAMID1234
t "…and the reason names Developer ID" \
  refusal_mentions "$WORK/dev.plist" com.tedswinyar.banshee TEAMID1234 'Developer ID'

profile "$WORK/nots.plist" true absent "$APP" "$FUTURE"
t_fails "a profile without the time-sensitive grant is refused" \
  banshee_check_profile_plist "$WORK/nots.plist" com.tedswinyar.banshee TEAMID1234
t "…and the reason says which capability to enable" \
  refusal_mentions "$WORK/nots.plist" com.tedswinyar.banshee TEAMID1234 'Time Sensitive Notifications'

profile "$WORK/falsets.plist" true false "$APP" "$FUTURE"
t_fails "a profile with the grant set to false is refused" \
  banshee_check_profile_plist "$WORK/falsets.plist" com.tedswinyar.banshee TEAMID1234

profile "$WORK/otherapp.plist" true true "TEAMID1234.com.tedswinyar.other" "$FUTURE"
t_fails "a profile for another App ID is refused" \
  banshee_check_profile_plist "$WORK/otherapp.plist" com.tedswinyar.banshee TEAMID1234
t_fails "a profile for another TEAM is refused (same bundle id)" \
  banshee_check_profile_plist "$WORK/good.plist" com.tedswinyar.banshee OTHERTEAM1
t "…and the reason shows both identifiers" \
  refusal_mentions "$WORK/good.plist" com.tedswinyar.banshee OTHERTEAM1 'OTHERTEAM1.com.tedswinyar.banshee'

profile "$WORK/expired.plist" true true "$APP" "$PAST"
t_fails "an expired profile is refused" \
  banshee_check_profile_plist "$WORK/expired.plist" com.tedswinyar.banshee TEAMID1234
t "…and the reason says expired" \
  refusal_mentions "$WORK/expired.plist" com.tedswinyar.banshee TEAMID1234 expired
# No ExpirationDate at all must read as expired, not as "never expires".
/usr/libexec/PlistBuddy -c 'Delete :ExpirationDate' "$WORK/good.plist" >/dev/null 2>&1
/bin/cp -f "$WORK/good.plist" "$WORK/noexp.plist"
profile "$WORK/good.plist" true true "$APP" "$FUTURE"
t_fails "a profile with no ExpirationDate is refused" \
  banshee_check_profile_plist "$WORK/noexp.plist" com.tedswinyar.banshee TEAMID1234

# Not a profile at all: a certificate, an HTML download page, nothing. These two run
# the REAL `security` and openssl; the keychain path is pinned to one that does not
# exist so the build server's own keychain is never consulted.
export BANSHEE_BUILD_KEYCHAIN="$WORK/no-such.keychain-db"
printf 'not a plist' > "$WORK/junk.provisionprofile"
t_fails "a file that is not a CMS profile is refused by the decoder" \
  banshee_profile_plist "$WORK/junk.provisionprofile" "$WORK/junk.plist"
t_fails "a missing profile is refused by the decoder" \
  banshee_profile_plist "$WORK/does-not-exist.provisionprofile" "$WORK/none.plist"
unset BANSHEE_BUILD_KEYCHAIN

# ---------------------------------------------------------------------------
# 2b. Decoding where there is NO DEFAULT KEYCHAIN (banshee-zcp)
# ---------------------------------------------------------------------------
# The build user is ssh-only: `security cms -D` fails there on a valid profile with
# "A default keychain could not be found", and on 2026-11-16 the import reported the
# real profile as "not a CMS-signed provisioning profile". Reproduced with a STUB
# `security` first on PATH — no keychain on this machine is read, created or changed.
# The stub logs every call, says what the real one said for the plain decode, and
# answers the `-k <keychain>` decode as $STUB_K tells it to.
STUB_BIN="$WORK/bin"; mkdir -p "$STUB_BIN"
STUB_LOG="$WORK/security.calls"
cat > "$STUB_BIN/security" <<'STUB_EOF'
#!/bin/bash
printf '%s\n' "$*" >> "$STUB_LOG"
case "$1 $2 $3" in
  "cms -D -i")
    case "${STUB_PLAIN:-no-default}" in
      ok) cat "$STUB_PLIST"; exit 0 ;;
      garbage) echo "exit 0, and this is not a plist"; exit 0 ;;
    esac
    echo "security: cert import failed: A default keychain could not be found." >&2
    echo "security: problem decoding" >&2
    exit 1 ;;
  "cms -D -k")
    [ "$4" = "$STUB_KC" ] || { echo "security: stub: unexpected keychain $4" >&2; exit 1; }
    [ "${STUB_K:-ok}" = ok ] || { echo "security: stub: the keychain is locked" >&2; exit 1; }
    cat "$STUB_PLIST"; exit 0 ;;
esac
echo "security: stub: unexpected call: $*" >&2; exit 64
STUB_EOF
chmod +x "$STUB_BIN/security"
export STUB_LOG STUB_PLIST="$WORK/good.plist"

# A keychain FILE is all the decoder looks for; the stub stands in for its contents.
KC_PRESENT="$WORK/present.keychain-db"; : > "$KC_PRESENT"
KC_ABSENT="$WORK/absent.keychain-db"

# A REAL CMS blob for the openssl path: good.plist signed, DER, by a throwaway
# self-signed certificate made here with the system openssl (files only, in $WORK).
# Without it the openssl decode could only ever be tested against junk.
REAL_CMS="$WORK/real.provisionprofile"
if /usr/bin/openssl req -x509 -newkey rsa:2048 -nodes -days 2 -subj "/CN=banshee-test" \
     -keyout "$WORK/signer.key" -out "$WORK/signer.pem" >/dev/null 2>&1 \
   && /usr/bin/openssl smime -sign -binary -nodetach -outform der \
     -signer "$WORK/signer.pem" -inkey "$WORK/signer.key" \
     -in "$WORK/good.plist" -out "$REAL_CMS" >/dev/null 2>&1; then
  PASS=$((PASS + 1))
else
  FAIL=$((FAIL + 1)); echo "  ✗ could not build a CMS-signed test profile with /usr/bin/openssl" >&2
fi

# decode <keychain-path> <profile> <out>: the decoder with the stub `security` first
# on PATH, a fresh call log, and the keychain path under test. A subshell, so nothing
# leaks into the cases that use the real tools further down.
decode() (
  : > "$STUB_LOG"
  export PATH="$STUB_BIN:$PATH" BANSHEE_BUILD_KEYCHAIN="$1" STUB_KC="$1"
  banshee_profile_plist "$2" "$3"
)
# decodes_to_good …: succeeded AND produced exactly the plist that was signed, which
# the pure checks then accept — the decoders must agree on bytes, not just on "ok".
decodes_to_good() {
  decode "$@" || return 1
  cmp -s "$3" "$WORK/good.plist" && banshee_check_profile_plist "$3" com.tedswinyar.banshee TEAMID1234
}
calls() { [ "$(grep -c -- "$1" "$STUB_LOG")" = "$2" ]; }
# decode_says <keychain> <profile> <text>: refused AND the message contains <text>.
decode_says() {
  local msg
  if msg="$(decode "$1" "$2" "$WORK/says.plist" 2>&1)"; then return 1; fi
  printf '%s\n' "$msg" | grep -qF -- "$3"
}
decode_never_says() {
  local msg
  if msg="$(decode "$1" "$2" "$WORK/says.plist" 2>&1)"; then return 1; fi
  if printf '%s\n' "$msg" | grep -qF -- "$3"; then return 1; fi
}

# The order is: plain security, then openssl, then the build keychain as the LAST
# resort — because `security cms -D -k` imports the profile's signer certificates into
# the keychain it is given, and a decode must not write to the signing keychain when
# anything else can read the profile (ruled 2026-11-17).

# The headless case as it really is: a valid profile, a build keychain that exists
# and WOULD decode it (the stub's -k answers ok). openssl must answer first.
t "no default keychain: /usr/bin/openssl decodes the profile" \
  decodes_to_good "$KC_PRESENT" "$REAL_CMS" "$WORK/o1.plist"
t "…having tried the plain decode first, exactly once" calls '^cms -D -i ' 1
t "…and when openssl succeeds, security -k is never invoked (nothing is written to the signing keychain)" \
  calls '^cms -D -k ' 0

# No build keychain at all (not provisioned yet, or a job that is not given one).
t "no default keychain and no build keychain: /usr/bin/openssl decodes the profile" \
  decodes_to_good "$KC_ABSENT" "$REAL_CMS" "$WORK/o.plist"
t "…without handing security a keychain that does not exist" calls '^cms -D -k ' 0

# The profile here is JUNK on purpose: openssl refuses it, so only the `-k` decode can
# produce a plist, and a pass proves the last resort ran.
t "openssl cannot read it: the build keychain is the last resort, and succeeds" \
  decodes_to_good "$KC_PRESENT" "$WORK/junk.provisionprofile" "$WORK/k.plist"
t "…after the plain decode, exactly once" calls '^cms -D -i ' 1
t "…and the keychain decode exactly once" calls '^cms -D -k ' 1

export STUB_K=locked

# Every path fails: the message must carry each decoder's OWN reason. The words that
# cost the evening must not come back for a failure that is not about the file.
t_fails "when every decoder fails the profile is refused" \
  decode "$KC_PRESENT" "$WORK/junk.provisionprofile" "$WORK/allfail.plist"
t "…and a refused decode leaves no half-written plist behind" test ! -s "$WORK/allfail.plist"
t "…and the reason carries security's own words about the default keychain" \
  decode_says "$KC_PRESENT" "$WORK/junk.provisionprofile" 'A default keychain could not be found'
t "…and why the last-resort keychain decode failed" \
  decode_says "$KC_PRESENT" "$WORK/junk.provisionprofile" "security cms -D -k $KC_PRESENT: security: stub: the keychain is locked"
t "…and why openssl failed" \
  decode_says "$KC_PRESENT" "$WORK/junk.provisionprofile" '/usr/bin/openssl smime -verify: Error reading S/MIME message'
t "…and never the old verdict on the file" \
  decode_never_says "$KC_PRESENT" "$WORK/junk.provisionprofile" 'is not a CMS-signed provisioning profile'
t "…and with no build keychain it says that, by path" \
  decode_says "$KC_ABSENT" "$WORK/junk.provisionprofile" "no build keychain at $KC_ABSENT"
unset STUB_K

# The plist check survives on every path: a decoder that exits 0 having printed
# something else has not decoded a profile.
export STUB_PLAIN=garbage
t_fails "a decoder that succeeds but prints no plist is not believed" \
  decode "$KC_ABSENT" "$WORK/junk.provisionprofile" "$WORK/garbage.plist"
t "…and the reason says so" \
  decode_says "$KC_ABSENT" "$WORK/junk.provisionprofile" 'security cms -D: decoded, but the content is not a property list'
export STUB_PLAIN=ok
t "where the plain decode works (a GUI login session) it is used" \
  decodes_to_good "$KC_PRESENT" "$WORK/junk.provisionprofile" "$WORK/plain.plist"
t "…and nothing else is tried" calls '^cms ' 1
unset STUB_PLAIN

# The DEFAULT keychain path is the one unlock-build-keychain.sh uses. $HOME is moved
# into the work dir for this one case; the stub refuses any other -k path.
FAKE_HOME="$WORK/home"; mkdir -p "$FAKE_HOME/Library/Keychains"
: > "$FAKE_HOME/Library/Keychains/banshee-build.keychain-db"
default_keychain_decode() (
  : > "$STUB_LOG"
  unset BANSHEE_BUILD_KEYCHAIN
  export HOME="$FAKE_HOME" PATH="$STUB_BIN:$PATH" STUB_KC="$FAKE_HOME/Library/Keychains/banshee-build.keychain-db"
  banshee_profile_plist "$WORK/junk.provisionprofile" "$WORK/dk.plist" && cmp -s "$WORK/dk.plist" "$WORK/good.plist"
)
t "with BANSHEE_BUILD_KEYCHAIN unset the retry uses ~/Library/Keychains/banshee-build.keychain-db" \
  default_keychain_decode
t "the library and unlock-build-keychain.sh spell the default keychain path the same way" \
  grep -qF 'KC="${BANSHEE_BUILD_KEYCHAIN:-$HOME/Library/Keychains/banshee-build.keychain-db}"' "$SCRIPT_DIR/build-server/unlock-build-keychain.sh"

# ---------------------------------------------------------------------------
# 3. The entitlements the app is actually sealed with
# ---------------------------------------------------------------------------
ENT="$ROOT_DIR/Banshee.entitlements"
t "Banshee.entitlements exists at the repo root" test -f "$ENT"
t "Banshee.entitlements is a valid plist" plutil -lint "$ENT"
t "Banshee.entitlements grants time-sensitive notifications" \
  pb_is "$ENT" "$BANSHEE_TIME_SENSITIVE_KEY" true
t "Banshee.entitlements carries exactly one key (identifiers are composed at build time; no critical-alerts)" \
  key_count "$ENT" 1
# By KEY, not by grep: the file's own comment names the ruling.
t_fails "Banshee.entitlements does not claim critical alerts (ruled out 2026-09-22)" \
  /usr/libexec/PlistBuddy -c 'Print :com.apple.developer.usernotifications.critical-alerts' "$ENT"

t "composing succeeds" \
  banshee_compose_entitlements "$ENT" TEAMID1234 com.tedswinyar.banshee "$WORK/signed.plist"
t "…adds the application-identifier the profile expects" \
  pb_is "$WORK/signed.plist" com.apple.application-identifier TEAMID1234.com.tedswinyar.banshee
t "…adds the team-identifier" \
  pb_is "$WORK/signed.plist" com.apple.developer.team-identifier TEAMID1234
t "…keeps the time-sensitive grant" \
  pb_is "$WORK/signed.plist" "$BANSHEE_TIME_SENSITIVE_KEY" true
t "…so the sealed file has exactly three keys" key_count "$WORK/signed.plist" 3
t "composing leaves the checked-in file untouched" key_count "$ENT" 1
printf 'not a plist' > "$WORK/bad.entitlements"
t_fails "composing from a non-plist base fails" \
  banshee_compose_entitlements "$WORK/bad.entitlements" TEAMID1234 com.tedswinyar.banshee "$WORK/bad-signed.plist"

# ---------------------------------------------------------------------------
# 4. The sealed-code claim check, against real signatures
# ---------------------------------------------------------------------------
# Ad-hoc signing a copy of a system tool is enough to put entitlements INTO a seal;
# the check reads them back. (Running the result is not attempted: with this
# entitlement it would be killed — which is the whole point of the library.)
/bin/cp -f /bin/sleep "$WORK/claims"; /bin/cp -f /bin/sleep "$WORK/plain"
banshee_compose_entitlements "$ENT" TEAMID1234 com.tedswinyar.banshee "$WORK/claim.plist" >/dev/null 2>&1
if codesign --force --sign - --entitlements "$WORK/claim.plist" "$WORK/claims" >/dev/null 2>&1 \
   && codesign --force --sign - "$WORK/plain" >/dev/null 2>&1; then
  t "the claim check sees the entitlement in a seal that carries it" banshee_signed_entitlements_claim "$WORK/claims"
  t_fails "the claim check sees nothing in a seal without it" banshee_signed_entitlements_claim "$WORK/plain"
else
  FAIL=$((FAIL + 1)); echo "  ✗ could not ad-hoc sign the probe binaries (codesign unavailable?)" >&2
fi

# ---------------------------------------------------------------------------
# 5. The two spellings of the bundle id must agree
# ---------------------------------------------------------------------------
t "build-app.sh's BUNDLE_ID equals the library's BANSHEE_BUNDLE_ID" \
  grep -qF "BUNDLE_ID=\"$BANSHEE_BUNDLE_ID\"" "$SCRIPT_DIR/build-app.sh"

echo "test-entitlements: $PASS passed, $FAIL failed"
[ "$FAIL" -eq 0 ]
