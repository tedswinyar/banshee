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

# Not a profile at all: a certificate, an HTML download page, nothing.
printf 'not a plist' > "$WORK/junk.provisionprofile"
t_fails "a file that is not a CMS profile is refused by the decoder" \
  banshee_profile_plist "$WORK/junk.provisionprofile" "$WORK/junk.plist"
t_fails "a missing profile is refused by the decoder" \
  banshee_profile_plist "$WORK/does-not-exist.provisionprofile" "$WORK/none.plist"

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
