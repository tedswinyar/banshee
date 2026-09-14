#!/usr/bin/env bash
set -euo pipefail

# publish.sh — cut an auto-updating release (banshee-u0a5-sparkle).
#
# Precondition: ./scripts/build-dmg.sh has produced a signed + notarized +
# stapled dist/Banshee-<version>.dmg. This then:
#   1. generates and EdDSA-signs the Sparkle appcast (generate_appcast, using
#      the private key in the keychain — the one whose public key is baked into
#      the app's Info.plist);
#   2. uploads the DMG and appcast.xml to a GitHub release on THIS repository, so
#      Sparkle's SUFeedURL (…/releases/latest/download/appcast.xml) and the
#      enclosure URL both resolve anonymously once the repository is public.
#
# The enclosure URL uses the repo's stable `releases/latest/download/` path, so
# the newest appcast always points at the newest release's DMG — which is all
# Sparkle needs to move any older install forward.
#
# The target repository is the source repository itself (scripts/lib/releases-repo.sh:
# GITHUB_REPOSITORY on the runner, `origin` on a developer machine; RELEASES_REPO in
# release.conf overrides). A private repository 404s Sparkle's anonymous GET — rehearse
# against a local appcast until the flip (docs/build-server.md).

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
ROOT_DIR="$(dirname "$SCRIPT_DIR")"
APP_NAME="Banshee"

die() { printf 'publish: ERROR: %s\n' "$*" >&2; finish 1; }
info() { printf 'publish: %s\n' "$*"; }

RELEASE_CONF="${BANSHEE_RELEASE_CONF:-$SCRIPT_DIR/release.conf}"
[ -f "$RELEASE_CONF" ] && . "$RELEASE_CONF"
# shellcheck source=lib/releases-repo.sh
. "$SCRIPT_DIR/lib/releases-repo.sh"
RELEASES_REPO="$(banshee_releases_repo "$ROOT_DIR")" \
  || die "cannot tell which repository to publish to: set RELEASES_REPO in $RELEASE_CONF, or run where GITHUB_REPOSITORY is set or origin is a github.com URL"

command -v gh >/dev/null || die "gh (GitHub CLI) is required to upload the release"

VERSION="$(sed -n 's/.*static let marketing = "\(.*\)"/\1/p' \
  "$ROOT_DIR/swift/Sources/Banshee/Version.swift")"
[ -n "$VERSION" ] || die "cannot read version from Version.swift"

DMG="$ROOT_DIR/dist/Banshee-$VERSION.dmg"
[ -f "$DMG" ] || die "no DMG at $DMG — run ./scripts/build-dmg.sh first"

# The notarized DMG must be STAPLED, or an offline first launch prompts. Cheap
# to assert here rather than discover after publishing.
xcrun stapler validate "$DMG" >/dev/null 2>&1 \
  || die "$DMG is not stapled; re-run ./scripts/build-dmg.sh"

# Sign the DMG and template a single-item appcast, rather than lean on
# generate_appcast: on this setup generate_appcast silently emitted an UNSIGNED
# enclosure (its keychain-account lookup differs from sign_update's), whereas
# sign_update reads the same key cleanly. A one-version feed is simple XML, and
# templating it keeps the signature guaranteed and every field under our control.
SIGN_UPDATE="$(find "$ROOT_DIR/swift/.build/artifacts" -name sign_update -type f 2>/dev/null | head -1)"
[ -n "$SIGN_UPDATE" ] || die "sign_update not found; run a swift build so SPM fetches Sparkle's tools"

# Version metadata comes from the app build-dmg packaged, so the feed matches
# the shipped artifact exactly (sparkle:version MUST equal its CFBundleVersion,
# or Sparkle's update comparison is wrong).
APP="$ROOT_DIR/build/$APP_NAME.app"
PB=/usr/libexec/PlistBuddy
[ -d "$APP" ] || die "no packaged app at $APP (run ./scripts/build-dmg.sh)"
SHORT_VERSION="$($PB -c 'Print :CFBundleShortVersionString' "$APP/Contents/Info.plist")"
BUILD_VERSION="$($PB -c 'Print :CFBundleVersion' "$APP/Contents/Info.plist")"
MIN_SYSTEM="$($PB -c 'Print :LSMinimumSystemVersion' "$APP/Contents/Info.plist")"

# The EdDSA private key comes from a FILE when SPARKLE_PRIVATE_KEY_FILE is set
# (release.conf or env) — the build server's shape, since its keychain is not
# the one the key was generated into — and from the keychain otherwise. Same
# key either way: `generate_keys -x` exported it once. NEVER regenerate it; a
# new key strands every shipped install. (Provisioning: docs/build-server.md.)
if [ -n "${SPARKLE_PRIVATE_KEY_FILE:-}" ]; then
  [ -r "$SPARKLE_PRIVATE_KEY_FILE" ] || die "SPARKLE_PRIVATE_KEY_FILE=$SPARKLE_PRIVATE_KEY_FILE is not readable"
  info "signing the DMG (EdDSA key from $SPARKLE_PRIVATE_KEY_FILE)"
  SIG_ATTRS="$("$SIGN_UPDATE" --ed-key-file "$SPARKLE_PRIVATE_KEY_FILE" "$DMG")"
else
  info "signing the DMG (EdDSA key from the keychain)"
  # sign_update prints:  sparkle:edSignature="..." length="..."
  SIG_ATTRS="$("$SIGN_UPDATE" "$DMG")"
fi
printf '%s' "$SIG_ATTRS" | grep -q 'edSignature=' \
  || die "sign_update produced no EdDSA signature: $SIG_ATTRS"

WORK="$(mktemp -d /tmp/banshee-publish.XXXXXX)"
cleanup() { rm -rf "${WORK:-}"; }
# On macOS bash 3.2 an EXIT trap turns a set -u abort into exit 0, so this
# script could crash and report success. See scripts/lib/completion-guard.sh.
# shellcheck source=lib/completion-guard.sh
. "$SCRIPT_DIR/lib/completion-guard.sh"
arm_completion_guard "publish" cleanup
APPCAST="$WORK/appcast.xml"
DMG_URL="https://github.com/$RELEASES_REPO/releases/latest/download/$(basename "$DMG")"

info "writing the appcast"
cat > "$APPCAST" <<XML
<?xml version="1.0" standalone="yes"?>
<rss xmlns:sparkle="http://www.andymatuschak.org/xml-namespaces/sparkle" version="2.0">
    <channel>
        <title>Banshee</title>
        <item>
            <title>$SHORT_VERSION</title>
            <sparkle:version>$BUILD_VERSION</sparkle:version>
            <sparkle:shortVersionString>$SHORT_VERSION</sparkle:shortVersionString>
            <sparkle:minimumSystemVersion>$MIN_SYSTEM</sparkle:minimumSystemVersion>
            <enclosure url="$DMG_URL" $SIG_ATTRS type="application/octet-stream"/>
        </item>
    </channel>
</rss>
XML

TAG="v$VERSION"
# Provenance: the tag must name the commit the DMG was built from, and that commit must
# already be on the remote (a tag on a commit GitHub does not have is an error, and a
# tag on "whatever main is right now" was how a shipped app once disagreed with its own
# tag). A dirty tree has no single commit to name.
REV="$(git -C "$ROOT_DIR" rev-parse HEAD)"
git -C "$ROOT_DIR" diff --quiet && git -C "$ROOT_DIR" diff --cached --quiet \
  || die "the working tree is dirty; a release names one commit, so commit or stash first"
git -C "$ROOT_DIR" fetch -q origin 2>/dev/null || true
git -C "$ROOT_DIR" merge-base --is-ancestor "$REV" origin/main 2>/dev/null \
  || die "HEAD ${REV:0:7} is not on origin/main; push it first so the tag can point at it"
info "publishing $TAG to $RELEASES_REPO (DMG + appcast.xml) for commit ${REV:0:7}"
if gh release view "$TAG" --repo "$RELEASES_REPO" >/dev/null 2>&1; then
  # Re-publishing the same version: replace the assets rather than fail. The tag keeps
  # its original target; if the source moved, delete the release and tag first.
  gh release upload "$TAG" "$DMG" "$APPCAST" --repo "$RELEASES_REPO" --clobber
else
  gh release create "$TAG" "$DMG" "$APPCAST" \
    --repo "$RELEASES_REPO" \
    --target "$REV" \
    --title "Banshee $VERSION" \
    --notes "Banshee $VERSION, built from commit ${REV:0:7}. Signed, notarized, and auto-updating via Sparkle."
fi

info "done. Sparkle feed: https://github.com/$RELEASES_REPO/releases/latest/download/appcast.xml"
finish 0
