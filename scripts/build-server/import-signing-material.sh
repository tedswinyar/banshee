#!/usr/bin/env bash
set -euo pipefail
# import-signing-material.sh — one-time, INTERACTIVE, run AS THE BUILD USER on the
# build host. Puts every release credential where the headless jobs expect it and writes
# the release.conf they read (docs/build-server.md).
#
#   import-signing-material.sh \
#       --p12 ~/DeveloperID.p12 \
#       --sparkle-key ~/sparkle_private_key.txt \
#       --sparkle-public 'BASE64…' \
#       --notary-key ~/AuthKey_XXXXXX.p8 --notary-key-id XXXXXX \
#       --notary-issuer 11111111-2222-3333-4444-555555555555 \
#       [--releases-repo owner/name]   # only to publish somewhere other than this repo
#
# What it creates (all under the build user, nothing in any repo):
#   ~/Library/Keychains/banshee-build.keychain-db   Developer ID cert + key
#   ~/.config/banshee-build/keychain-password       0600, read by unlock-build-keychain.sh
#   ~/.config/banshee-build/sparkle_private_key     0600  (NEVER regenerate this key)
#   ~/.config/banshee-build/AuthKey.p8              0600
#   ~/.config/banshee-build/release.conf            what build-app/build-dmg/publish read
# Prompts once for the .p12 password. Ends by running scripts/doctor.sh --release.
ROOT_DIR="$(cd "$(dirname "$0")/../.." && pwd)"
CFG="$HOME/.config/banshee-build"
KC="$HOME/Library/Keychains/banshee-build.keychain-db"
die() { printf 'import-signing-material: ERROR: %s\n' "$*" >&2; exit 1; }
info() { printf 'import-signing-material: %s\n' "$*"; }

P12="" SPARKLE_KEY="" SPARKLE_PUB="" NKEY="" NKEY_ID="" NISSUER="" RELEASES=""
while [ $# -gt 0 ]; do
  case "$1" in
    --p12) P12="$2"; shift 2 ;;
    --sparkle-key) SPARKLE_KEY="$2"; shift 2 ;;
    --sparkle-public) SPARKLE_PUB="$2"; shift 2 ;;
    --notary-key) NKEY="$2"; shift 2 ;;
    --notary-key-id) NKEY_ID="$2"; shift 2 ;;
    --notary-issuer) NISSUER="$2"; shift 2 ;;
    --releases-repo) RELEASES="$2"; shift 2 ;;
    *) die "unknown argument $1" ;;
  esac
done
for v in P12 SPARKLE_KEY SPARKLE_PUB NKEY NKEY_ID NISSUER; do
  [ -n "${!v}" ] || die "--$(printf '%s' "$v" | tr 'A-Z_' 'a-z-') is required"
done
for f in "$P12" "$SPARKLE_KEY" "$NKEY"; do [ -r "$f" ] || die "cannot read $f"; done
[ "$(id -un)" != "root" ] || die "run as the build user, not root"

umask 077
mkdir -p "$CFG"
chmod 700 "$CFG"

# --- the keychain ------------------------------------------------------------
if [ -f "$KC" ]; then
  info "keychain exists: $KC (reusing; delete it to start over)"
  [ -r "$CFG/keychain-password" ] || die "keychain exists but $CFG/keychain-password is missing"
else
  info "creating $KC"
  head -c 32 /dev/urandom | base64 | tr -d '\n=/+' > "$CFG/keychain-password"
  chmod 600 "$CFG/keychain-password"
  security create-keychain -p "$(cat "$CFG/keychain-password")" "$KC"
fi
security unlock-keychain -p "$(cat "$CFG/keychain-password")" "$KC"
security set-keychain-settings "$KC"

# An ssh-only user has an EMPTY user keychain search list (no login keychain was ever
# created for it), and identity validation builds the chain from the SEARCH LIST, not
# from the keychain named on the command line — so `find-identity -v` reported the
# imported Developer ID as invalid even with its intermediate beside it (build
# server, 2026-09-13). Put ours in the list, keeping whatever is already there.
existing=(); while IFS= read -r k; do [ -n "$k" ] && existing+=("$k"); done < <(security list-keychains -d user | sed 's/^[[:space:]]*"//; s/"$//')
case " ${existing[*]-} " in *" $KC "*) ;; *) security list-keychains -d user -s "$KC" "${existing[@]-}" ;; esac

# The same fresh user has none of Apple's intermediates anywhere it can search, and a
# leaf whose chain cannot be built is "not valid" to codesign. Fetch the public
# Developer ID G2 CA (the issuer of every current Developer ID Application cert),
# pinned by SHA-1, and keep it beside the leaf.
G2_SHA1="5B45F61068B29FCC8FFFF1A7E99B78DA9E9C4635"
if ! security find-certificate -c "Developer ID Certification Authority" "$KC" >/dev/null 2>&1; then
  info "importing Apple's Developer ID G2 intermediate"
  tmp="$(mktemp -d)"
  curl -sSfL -o "$tmp/G2.cer" https://www.apple.com/certificateauthority/DeveloperIDG2CA.cer || die "could not download DeveloperIDG2CA.cer"
  got="$(openssl x509 -inform der -in "$tmp/G2.cer" -noout -fingerprint -sha1 | sed 's/.*=//; s/://g')"
  [ "$got" = "$G2_SHA1" ] || die "DeveloperIDG2CA.cer fingerprint $got != expected $G2_SHA1"
  security import "$tmp/G2.cer" -k "$KC" >/dev/null
  rm -rf "$tmp"
fi

if security find-identity -v -p codesigning "$KC" | grep -q '"Developer ID Application: '; then
  info "a valid Developer ID Application identity is already in $KC; skipping the .p12 import (re-run)"
else
  info "importing the Developer ID (you will be asked for the .p12 password)"
  printf 'p12 password: '; read -rs P12_PW; echo
  security import "$P12" -k "$KC" -P "$P12_PW" -T /usr/bin/codesign -T /usr/bin/security -T /usr/bin/productsign
  unset P12_PW
  # Let codesign use the key without a UI prompt — the whole point on a headless box.
  security set-key-partition-list -S 'apple-tool:,apple:,codesign:' -s -k "$(cat "$CFG/keychain-password")" "$KC" >/dev/null
fi
IDENTITY="$(security find-identity -v -p codesigning "$KC" | sed -n 's/.*"\(Developer ID Application: .*\)"/\1/p' | head -1)"
[ -n "$IDENTITY" ] || die "no VALID 'Developer ID Application' identity in $KC after import — an 'Apple Development' cert is the wrong export; 'security verify-cert -c <leaf.pem> -p codeSign -k $KC' names a chain problem"
info "identity: $IDENTITY"

# --- files -------------------------------------------------------------------
/bin/cp -f "$SPARKLE_KEY" "$CFG/sparkle_private_key"; chmod 600 "$CFG/sparkle_private_key"
/bin/cp -f "$NKEY" "$CFG/AuthKey.p8"; chmod 600 "$CFG/AuthKey.p8"

# --- release.conf ------------------------------------------------------------
cat > "$CFG/release.conf" <<CONF
# Written by scripts/build-server/import-signing-material.sh on $(date -u +%Y-%m-%dT%H:%M:%SZ).
# Read by build-app.sh / build-dmg.sh / publish.sh via BANSHEE_RELEASE_CONF.
SIGNING_IDENTITY="$IDENTITY"
SIGNING_KEYCHAIN="$KC"
NOTARY_KEY_PATH="$CFG/AuthKey.p8"
NOTARY_KEY_ID="$NKEY_ID"
NOTARY_ISSUER_ID="$NISSUER"
SPARKLE_PUBLIC_KEY="$SPARKLE_PUB"
SPARKLE_PRIVATE_KEY_FILE="$CFG/sparkle_private_key"
${RELEASES:+RELEASES_REPO="$RELEASES"}
CONF
chmod 600 "$CFG/release.conf"
info "wrote $CFG/release.conf"

info "checking everything with doctor --release"
BANSHEE_RELEASE_CONF="$CFG/release.conf" "$ROOT_DIR/scripts/doctor.sh" --release
