#!/usr/bin/env bash
set -euo pipefail
# unlock-build-keychain.sh — make the build user's signing keychain usable by a
# headless job, or lock it again (docs/build-server.md).
#
#   unlock-build-keychain.sh          unlock + put first in the search list
#   unlock-build-keychain.sh --lock   lock it
#
# The keychain and its password file are created by import-signing-material.sh:
#   ~/Library/Keychains/banshee-build.keychain-db
#   ~/.config/banshee-build/keychain-password   (0600)
# A launchd/ssh session has no unlocked login keychain, which is why codesign and
# sign_update need a keychain of their own that the job can unlock from a file.
KC="${BANSHEE_BUILD_KEYCHAIN:-$HOME/Library/Keychains/banshee-build.keychain-db}"
PW_FILE="${BANSHEE_BUILD_KEYCHAIN_PASSWORD_FILE:-$HOME/.config/banshee-build/keychain-password}"
die() { printf 'unlock-build-keychain: ERROR: %s\n' "$*" >&2; exit 1; }

[ -f "$KC" ] || die "no keychain at $KC — run scripts/build-server/import-signing-material.sh first"
if [ "${1:-}" = "--lock" ]; then
  security lock-keychain "$KC"
  echo "unlock-build-keychain: locked $KC"
  exit 0
fi
[ -r "$PW_FILE" ] || die "no password file at $PW_FILE"
[ "$(stat -f '%Lp' "$PW_FILE")" = "600" ] || die "$PW_FILE must be mode 0600 (is $(stat -f '%Lp' "$PW_FILE"))"
security unlock-keychain -p "$(cat "$PW_FILE")" "$KC"
# No auto-lock during a long notarization wait; the job locks it explicitly.
security set-keychain-settings "$KC"
# First in the user search list so codesign/sign_update find the key without a
# --keychain everywhere; login.keychain stays in the list for everything else.
LOGIN_KC="$HOME/Library/Keychains/login.keychain-db"
if [ -f "$LOGIN_KC" ]; then
  security list-keychains -d user -s "$KC" "$LOGIN_KC"
else
  # An ssh-only build user never gets a login keychain; naming a missing file here
  # would leave the search list in an odd state.
  security list-keychains -d user -s "$KC"
fi
echo "unlock-build-keychain: unlocked $KC; identities:"
security find-identity -v -p codesigning "$KC" | sed 's/^/  /'
