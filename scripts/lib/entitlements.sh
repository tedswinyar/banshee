#!/usr/bin/env bash
# entitlements.sh — the time-sensitive notifications capability and the provisioning
# profile that authorises it (banshee-jie). Sourced by build-app.sh, doctor.sh and the
# build-server import scripts; scripts/tests/test-entitlements.sh pins every function.
#
# Why this exists at all: `com.apple.developer.usernotifications.time-sensitive` is a
# RESTRICTED entitlement (TN3125). A Developer ID app may claim it only when the
# bundle embeds a Developer ID provisioning profile (Contents/embedded.provisionprofile)
# whose Entitlements grant it for this exact App ID; otherwise amfid kills the app at
# launch with no dialog — the symptom is "nothing happens", exit 137 in a terminal. An
# ad-hoc signature cannot be authorised by any profile, so an ad-hoc build must not
# carry the entitlement (probed 2026-10-15: SIGKILL). Everything below is the paperwork
# that keeps a release from shipping an app that cannot start.
#
# Pure functions take PLISTS (XML), so the tests can hand them synthetic ones; only
# banshee_profile_plist touches `security`, and only on a real profile.

BANSHEE_TIME_SENSITIVE_KEY="com.apple.developer.usernotifications.time-sensitive"
# The App ID the profile must be for. build-app.sh spells its own BUNDLE_ID; the
# scripts suite pins that the two agree.
BANSHEE_BUNDLE_ID="com.tedswinyar.banshee"

# banshee_team_id_from_identity "<Developer ID Application: Name (TEAMID1234)>"
# Prints the Team ID, or nothing (exit 1) when the identity has no "(TEAMID)" suffix
# — an ad-hoc "-" or a malformed string.
banshee_team_id_from_identity() {
  local team
  team="$(printf '%s' "${1:-}" | sed -n 's/.*(\([A-Z0-9][A-Z0-9]*\))[[:space:]]*$/\1/p')"
  [ -n "$team" ] || return 1
  printf '%s\n' "$team"
}

# banshee_profile_plist <profile> <out.plist>
# Decodes the CMS-signed .provisionprofile into its XML plist. Exit 1 when the file is
# not a profile (a .cer, a .p12, an HTML download page).
banshee_profile_plist() {
  local profile="$1" out="$2"
  [ -r "$profile" ] || { echo "entitlements: cannot read provisioning profile $profile" >&2; return 1; }
  if ! security cms -D -i "$profile" > "$out" 2>/dev/null || ! plutil -lint "$out" >/dev/null 2>&1; then
    echo "entitlements: $profile is not a CMS-signed provisioning profile (security cms -D failed)" >&2
    return 1
  fi
}

# banshee_check_profile_plist <decoded.plist> <bundle_id> <team_id>
# Exit 0 iff the decoded profile would let a Developer ID build of <bundle_id> by
# <team_id> claim the time-sensitive entitlement TODAY. Every failure prints one
# reason on stderr; a profile that fails here would pass codesign and kill the app.
banshee_check_profile_plist() {
  local plist="$1" bundle_id="$2" team_id="$3"
  local pb=/usr/libexec/PlistBuddy
  local v
  # Developer ID profiles provision ALL devices; a Development or Mac App Store
  # profile does not, and would be refused at launch on every other Mac.
  v="$("$pb" -c 'Print :ProvisionsAllDevices' "$plist" 2>/dev/null || true)"
  if [ "$v" != "true" ]; then
    echo "entitlements: profile is not a Developer ID profile (ProvisionsAllDevices is not true) — in the portal choose Distribution → Developer ID" >&2
    return 1
  fi
  v="$("$pb" -c "Print :Entitlements:$BANSHEE_TIME_SENSITIVE_KEY" "$plist" 2>/dev/null || true)"
  if [ "$v" != "true" ]; then
    echo "entitlements: profile does not grant $BANSHEE_TIME_SENSITIVE_KEY — enable 'Time Sensitive Notifications' on the App ID, then regenerate and re-download the profile" >&2
    return 1
  fi
  v="$("$pb" -c 'Print :Entitlements:com.apple.application-identifier' "$plist" 2>/dev/null || true)"
  if [ "$v" != "$team_id.$bundle_id" ]; then
    echo "entitlements: profile is for application-identifier '${v:-<none>}', the build is '$team_id.$bundle_id' — wrong App ID or wrong team" >&2
    return 1
  fi
  v="$(plutil -extract ExpirationDate raw -o - "$plist" 2>/dev/null || true)"
  local now
  now="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
  # ISO-8601 Zulu strings compare lexically. `[[ ]]`, not `[ ! a \> b ]`: zsh parses
  # the latter as a syntax error and then returns 0, which would PASS an expired
  # profile when this file is sourced interactively (seen writing the tests).
  if [[ -z "$v" || ! "$v" > "$now" ]]; then
    echo "entitlements: profile expired (ExpirationDate ${v:-<none>}, now $now) — regenerate it in the portal" >&2
    return 1
  fi
  return 0
}

# banshee_compose_entitlements <base.entitlements> <team_id> <bundle_id> <out.plist>
# The file the app is actually sealed with: the checked-in capability plus the two
# identifier entitlements Xcode adds for a profile-backed signature, so the claim
# matches the profile's application-identifier exactly.
banshee_compose_entitlements() {
  local base="$1" team_id="$2" bundle_id="$3" out="$4"
  local pb=/usr/libexec/PlistBuddy
  plutil -lint "$base" >/dev/null 2>&1 || { echo "entitlements: $base is not a valid plist" >&2; return 1; }
  /bin/cp -f "$base" "$out"
  # PlistBuddy uses ':' as the key separator, so dotted keys are safe here (plutil
  # -extract would split them).
  "$pb" -c "Add :com.apple.application-identifier string $team_id.$bundle_id" "$out" >/dev/null \
    && "$pb" -c "Add :com.apple.developer.team-identifier string $team_id" "$out" >/dev/null \
    && plutil -lint "$out" >/dev/null
}

# banshee_signed_entitlements_claim <signed-code-path>
# Exit 0 iff the sealed code's entitlements include the time-sensitive key — the
# post-sign assertion that the capability really landed in the bytes that ship.
banshee_signed_entitlements_claim() {
  codesign -d --entitlements - "$1" 2>/dev/null | grep -qF "$BANSHEE_TIME_SENSITIVE_KEY"
}
