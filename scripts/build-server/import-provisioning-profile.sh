#!/usr/bin/env bash
set -euo pipefail
# import-provisioning-profile.sh — put the Developer ID provisioning profile that
# authorises the time-sensitive notifications entitlement (banshee-jie) where the
# headless release job expects it, after checking it is the RIGHT profile.
#
#   import-provisioning-profile.sh ~/Banshee_Developer_ID.provisionprofile
#   import-provisioning-profile.sh --no-conf <profile> "<signing identity>"   # from import-signing-material.sh
#
# Run AS THE BUILD USER on the build host, once per profile (they expire yearly; the
# release job's doctor --release says when). What it does:
#   ~/.config/banshee-build/Banshee.provisionprofile   0600 copy of the profile
#   ~/.config/banshee-build/release.conf               PROVISIONING_PROFILE= line added/replaced
# It refuses a profile that is not Developer ID, does not grant the entitlement, is
# for another App ID or team, or has expired — each of which codesign accepts and
# amfid then punishes by killing the app at launch (scripts/lib/entitlements.sh).
#
# Making the profile (a person, at developer.apple.com → Certificates, Identifiers &
# Profiles): Identifiers → the App ID (platform macOS, explicit bundle id) →
# Capabilities → tick "Time Sensitive Notifications" → Save. Then Profiles → + →
# Distribution → "Developer ID" → that App ID → the Developer ID Application
# certificate → name it → Generate → Download. scp the .provisionprofile here.
ROOT_DIR="$(cd "$(dirname "$0")/../.." && pwd)"
CFG="$HOME/.config/banshee-build"
# shellcheck source=../lib/entitlements.sh
. "$ROOT_DIR/scripts/lib/entitlements.sh"
die() { printf 'import-provisioning-profile: ERROR: %s\n' "$*" >&2; exit 1; }
info() { printf 'import-provisioning-profile: %s\n' "$*"; }

WRITE_CONF=1
if [ "${1:-}" = "--no-conf" ]; then WRITE_CONF=0; shift; fi
PROFILE="${1:-}"
[ -n "$PROFILE" ] || die "usage: import-provisioning-profile.sh [--no-conf] <file.provisionprofile> [<signing identity>]"
[ -r "$PROFILE" ] || die "cannot read $PROFILE"
[ "$(id -un)" != "root" ] || die "run as the build user, not root"

# The identity names the team the profile must be for: given explicitly (the fresh
# provisioning path), else from the release.conf already on this machine.
IDENTITY="${2:-}"
if [ -z "$IDENTITY" ]; then
  [ -r "$CFG/release.conf" ] || die "no $CFG/release.conf — run import-signing-material.sh first (or pass the signing identity as the second argument)"
  # shellcheck source=/dev/null
  . "$CFG/release.conf"
  IDENTITY="${SIGNING_IDENTITY:-}"
fi
TEAM="$(banshee_team_id_from_identity "$IDENTITY")" || die "no Team ID in signing identity '$IDENTITY'"

tmp="$(mktemp)"
trap 'rm -f "$tmp"' EXIT
banshee_profile_plist "$PROFILE" "$tmp" || die "not a provisioning profile: $PROFILE"
banshee_check_profile_plist "$tmp" "$BANSHEE_BUNDLE_ID" "$TEAM" || die "refusing $PROFILE for $TEAM.$BANSHEE_BUNDLE_ID"
info "profile grants $BANSHEE_TIME_SENSITIVE_KEY to $TEAM.$BANSHEE_BUNDLE_ID, expires $(plutil -extract ExpirationDate raw -o - "$tmp")"

umask 077
mkdir -p "$CFG"; chmod 700 "$CFG"
/bin/cp -f "$PROFILE" "$CFG/Banshee.provisionprofile"; chmod 600 "$CFG/Banshee.provisionprofile"
info "installed $CFG/Banshee.provisionprofile"

if [ "$WRITE_CONF" = 1 ]; then
  [ -w "$CFG/release.conf" ] || die "no writable $CFG/release.conf to record PROVISIONING_PROFILE in"
  # Replace an existing line rather than appending a second one: the LAST assignment
  # would win silently and the file would lie about which profile is used.
  /usr/bin/grep -v '^PROVISIONING_PROFILE=' "$CFG/release.conf" > "$CFG/release.conf.new" || true
  printf 'PROVISIONING_PROFILE="%s"\n' "$CFG/Banshee.provisionprofile" >> "$CFG/release.conf.new"
  /bin/mv -f "$CFG/release.conf.new" "$CFG/release.conf"; chmod 600 "$CFG/release.conf"
  info "recorded PROVISIONING_PROFILE in $CFG/release.conf"
  info "checking everything with doctor --release"
  BANSHEE_RELEASE_CONF="$CFG/release.conf" "$ROOT_DIR/scripts/doctor.sh" --release
fi
