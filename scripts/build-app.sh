#!/usr/bin/env bash
set -euo pipefail

# build-app.sh — assemble build/Banshee.app from the SwiftPM executable
# plus the embedded Rust binaries.
#
# Usage:
#   ./scripts/build-app.sh            # release build, ad-hoc signed
#   ./scripts/build-app.sh --debug    # debug build (faster, for local poking)
#
# Signing: ad-hoc ("-") by default so a fresh clone needs no Apple
# credentials. scripts/release.conf (gitignored; see release.conf.example)
# provides SIGNING_IDENTITY for real Developer ID signing — build-dmg.sh
# layers notarization on top of this script's output.

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
ROOT_DIR="$(dirname "$SCRIPT_DIR")"
APP_NAME="Banshee"
BUNDLE_ID="com.tedswinyar.banshee"

CONFIGURATION="release"
if [ "${1:-}" = "--debug" ]; then
  CONFIGURATION="debug"
fi

VERSION="$(sed -n 's/.*static let marketing = "\(.*\)"/\1/p' \
  "$ROOT_DIR/swift/Sources/Banshee/Version.swift")"
if [ -z "$VERSION" ]; then
  echo "build-app.sh: cannot read version from Version.swift" >&2
  exit 1
fi

# CFBundleVersion must be deterministic and monotonic: default it to the git
# commit count so two builds of the same commit produce the same build number
# (was hardcoded 1 — devops review 2026-08-20). Override with BUILD_NUMBER.
if [ -z "${BUILD_NUMBER:-}" ]; then
  BUILD_NUMBER="$(git -C "$ROOT_DIR" rev-list --count HEAD 2>/dev/null || echo 1)"
fi

SIGNING_IDENTITY="-"
RELEASE_CONF="${BANSHEE_RELEASE_CONF:-$SCRIPT_DIR/release.conf}"
if [ -f "$RELEASE_CONF" ]; then
  # shellcheck source=/dev/null
  . "$RELEASE_CONF"
fi

# Sparkle auto-update (banshee-u0a5-sparkle). The framework is ALWAYS embedded
# (the app links it, so a missing framework is a launch-time dyld crash), but
# the Info.plist feed + public key are written ONLY when a key is configured —
# an ad-hoc local build then has no feed and Sparkle stays inert, which is what
# a fresh clone wants. Both are overridable in release.conf.
APPCAST_URL="${APPCAST_URL:-https://github.com/tedswinyar/banshee/releases/latest/download/appcast.xml}"
SPARKLE_PUBLIC_KEY="${SPARKLE_PUBLIC_KEY:-}"

echo "==> Building $APP_NAME $VERSION ($CONFIGURATION)"
# A release build must not carry the builder's home path (scripts/lib/bundle-scan.sh
# explains the incident). Both compilers embed source paths into the binary — Rust in
# panic locations for every crate, including the Cargo registry under $HOME; Swift in
# #filePath literals and module paths — so both get remapped to stable, anonymous
# prefixes. Debug builds are left alone: a debugger wants the real paths.
SWIFT_FLAGS=()
if [ "$CONFIGURATION" = release ]; then
  CARGO_HOME_DIR="${CARGO_HOME:-$HOME/.cargo}"
  export RUSTFLAGS="${RUSTFLAGS:-} --remap-path-prefix=$CARGO_HOME_DIR/registry/src=/cargo/registry --remap-path-prefix=$ROOT_DIR=/banshee --remap-path-prefix=$HOME=/home"
  # -file-prefix-map covers what the COMPILER writes (#filePath, module paths). The
  # LINKER separately records each object file's absolute path in the debug map
  # (N_OSO stabs), which the compiler flag cannot reach; ld64's -oso_prefix strips it.
  SWIFT_FLAGS=(-Xswiftc -file-prefix-map -Xswiftc "$ROOT_DIR=/banshee" -Xswiftc -file-prefix-map -Xswiftc "$HOME=/home"
               -Xlinker -oso_prefix -Xlinker "$ROOT_DIR/")
  # The NATIVE build system, by name. Swift 6.4's SwiftPM switched its default to
  # "swiftbuild", whose link step records ABSOLUTE .build/out/Intermediates.noindex/…
  # object and swiftmodule paths in the binary's symbol table — 34 of them, which
  # neither -file-prefix-map (compiler) nor -oso_prefix (N_OSO only) reaches, so the
  # scan below refused every release bundle on the first Xcode 27 machine
  # (2026-09-20, banshee-vas). Older toolchains default to native and accept the
  # flag, so this is parity, not a workaround; when native is removed this line
  # fails loudly and the follow-up in that bead (strip debug stabs) takes over.
  SWIFT_FLAGS+=(--build-system native)
fi
(cd "$ROOT_DIR/swift" && swift build -c "$CONFIGURATION" ${SWIFT_FLAGS[@]+"${SWIFT_FLAGS[@]}"})
(cd "$ROOT_DIR/rust" && cargo build --workspace $([ "$CONFIGURATION" = release ] && echo --release))

APP_BINARY="$ROOT_DIR/swift/.build/$CONFIGURATION/$APP_NAME"
RUST_BIN_DIR="$ROOT_DIR/rust/target/$CONFIGURATION"
for f in "$APP_BINARY" "$RUST_BIN_DIR/banshee-api" "$RUST_BIN_DIR/banshee-mcp" "$RUST_BIN_DIR/banshee"; do
  if [ ! -f "$f" ]; then
    echo "build-app.sh: missing build product: $f" >&2
    exit 1
  fi
done

APP_DIR="$ROOT_DIR/build/$APP_NAME.app"
echo "==> Assembling $APP_DIR"
rm -rf "$APP_DIR"
mkdir -p "$APP_DIR/Contents/MacOS" "$APP_DIR/Contents/Resources" \
         "$APP_DIR/Contents/Helpers"

cp "$APP_BINARY" "$APP_DIR/Contents/MacOS/$APP_NAME"

# App icon (the skull-wail mark). Its absence is why early builds showed the
# blank generic app icon.
if [ -f "$ROOT_DIR/assets/AppIcon.icns" ]; then
  cp "$ROOT_DIR/assets/AppIcon.icns" "$APP_DIR/Contents/Resources/AppIcon.icns"
else
  echo "build-app.sh: WARNING: assets/AppIcon.icns missing — app will have no icon" >&2
fi

# The Rust binaries go in Contents/Helpers/, NOT Contents/MacOS/.
#
# They used to ride in MacOS/ so `Bundle.url(forAuxiliaryExecutable:)` would find
# them — and on a case-INSENSITIVE filesystem (the macOS default) that silently
# destroyed the bundle: `MacOS/Banshee` (the app) and `MacOS/banshee` (the CLI) are
# THE SAME FILE, so the CLI copy overwrote the app. `open Banshee.app` then launched
# the CLI, which printed usage to a terminal nobody was watching and exited 0. No
# crash, no log, no Dock icon — the app simply never appeared, and `pgrep` showed
# nothing.
#
# Found on 2026-08-31, the first time anyone tried to LAUNCH the bundle rather than
# build it (P6). It is a template bug, not one this project introduced: any
# template stamp whose app name differs from its CLI name only by case has it.
# Worth backporting upstream.
#
# Nothing reads them via `forAuxiliaryExecutable:` any more — the app stopped
# supervising the API in P5 (ADR-0004) — but a bundled copy is still a legitimate
# SOURCE for installing the LaunchAgent, so they stay in the bundle.
cp "$RUST_BIN_DIR/banshee-api" "$APP_DIR/Contents/Helpers/"
cp "$RUST_BIN_DIR/banshee-mcp" "$APP_DIR/Contents/Helpers/"
cp "$RUST_BIN_DIR/banshee" "$APP_DIR/Contents/Helpers/"

# Prove the main executable really is the APP. A bundle whose CFBundleExecutable is
# some other program is the failure above, and it is invisible from the outside:
# the only symptom is that nothing happens.
if ! otool -L "$APP_DIR/Contents/MacOS/$APP_NAME" | grep -q SwiftUI; then
  echo "build-app.sh: $APP_DIR/Contents/MacOS/$APP_NAME does not link SwiftUI —" >&2
  echo "build-app.sh: something overwrote the app binary (case-insensitive clash?)" >&2
  exit 1
fi

# ---------------------------------------------------------------------------
# Embed Sparkle.framework (banshee-u0a5-sparkle)
# ---------------------------------------------------------------------------
# The app links @rpath/Sparkle.framework/..., so the framework MUST be in the
# bundle or dyld fails at launch. A SwiftPM build has no Xcode "embed
# frameworks" phase, so copy it in and add the rpath by hand. The nested XPC
# services / Updater.app / Autoupdate are signed inside-out below.
SPARKLE_FRAMEWORK="$ROOT_DIR/swift/.build/$CONFIGURATION/Sparkle.framework"
if [ ! -d "$SPARKLE_FRAMEWORK" ]; then
  echo "build-app.sh: missing $SPARKLE_FRAMEWORK (did swift build resolve Sparkle?)" >&2
  exit 1
fi
mkdir -p "$APP_DIR/Contents/Frameworks"
# /bin/cp -Rp: preserve the framework's version symlinks; cp is aliased to cp -i.
/bin/cp -Rp "$SPARKLE_FRAMEWORK" "$APP_DIR/Contents/Frameworks/"
# The linked executable's rpaths are @loader_path (= Contents/MacOS); add the
# Frameworks dir so @rpath/Sparkle.framework resolves. (Invalidates the ad-hoc
# signature the linker applied — we re-sign below.)
install_name_tool -add_rpath "@loader_path/../Frameworks" \
  "$APP_DIR/Contents/MacOS/$APP_NAME"

# Sparkle feed + public key, only when configured (see the note above). Leading
# newline so it slots under the last static key. SUEnableAutomaticChecks turns
# on the scheduled background check; the user still confirms each install.
SPARKLE_KEYS=""
if [ -n "$SPARKLE_PUBLIC_KEY" ]; then
  SPARKLE_KEYS="
    <key>SUFeedURL</key>                <string>$APPCAST_URL</string>
    <key>SUPublicEDKey</key>            <string>$SPARKLE_PUBLIC_KEY</string>
    <key>SUEnableAutomaticChecks</key>  <true/>"
fi

cat > "$APP_DIR/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleExecutable</key>       <string>$APP_NAME</string>
    <key>CFBundleIdentifier</key>       <string>$BUNDLE_ID</string>
    <key>CFBundleName</key>             <string>Banshee</string>
    <key>CFBundleIconFile</key>         <string>AppIcon</string>
    <key>CFBundlePackageType</key>      <string>APPL</string>
    <key>CFBundleShortVersionString</key><string>$VERSION</string>
    <key>CFBundleVersion</key>          <string>${BUILD_NUMBER:-1}</string>
    <key>LSMinimumSystemVersion</key>   <string>14.0</string>
    <key>NSHighResolutionCapable</key>  <true/>
    <key>LSApplicationCategoryType</key><string>public.app-category.productivity</string>$SPARKLE_KEYS
</dict>
</plist>
PLIST

echo "==> Signing (identity: ${SIGNING_IDENTITY})"
# Real Developer ID signing must use the HARDENED RUNTIME and a secure
# TIMESTAMP, or Apple's notary rejects the submission ("does not have the
# hardened runtime enabled"). Ad-hoc ("-") signing for local use takes neither:
# --options runtime under an ad-hoc identity is pointless and can add friction
# to just running the app on this machine.
#
# A function rather than an array of flags on purpose: stock macOS ships bash
# 3.2, where expanding an empty array under `set -u` is an "unbound variable"
# error — so `codesign "${OPTS[@]}"` with no flags would break a release on a
# clean machine. The app needs no entitlements: it is a localhost HTTP client
# that spawns the helpers as subprocesses (not dlopen), and the helpers exec
# ps/tmux/git/kill, all of which the hardened runtime permits.
# SIGNING_KEYCHAIN (release.conf or env) names the keychain FILE holding the
# Developer ID — the build server keeps it in its own keychain, unlocked by the
# job, rather than in a login keychain nobody is logged in to. Unset, codesign
# searches the default list as before.
sign() {
  if [ "$SIGNING_IDENTITY" = "-" ]; then
    codesign --force --sign "-" "$1"
  elif [ -n "${SIGNING_KEYCHAIN:-}" ]; then
    codesign --force --options runtime --timestamp --keychain "$SIGNING_KEYCHAIN" --sign "$SIGNING_IDENTITY" "$1"
  else
    codesign --force --options runtime --timestamp --sign "$SIGNING_IDENTITY" "$1"
  fi
}
# Sparkle.framework carries nested code — two XPC services, Updater.app, and
# the Autoupdate tool — each of which must be signed with the same identity and
# hardened runtime BEFORE the framework and the app are sealed, or notarization
# rejects the bundle. Sparkle ships them signed by its own team; --force
# re-signs them as ours. Deepest first.
SPK="$APP_DIR/Contents/Frameworks/Sparkle.framework/Versions/B"
if [ -d "$SPK" ]; then
  sign "$SPK/XPCServices/Installer.xpc"
  sign "$SPK/XPCServices/Downloader.xpc"
  sign "$SPK/Autoupdate"
  sign "$SPK/Updater.app"
  sign "$APP_DIR/Contents/Frameworks/Sparkle.framework"
fi

# Inside-out: the nested helper executables first, then the bundle.
for bin in banshee-api banshee-mcp banshee; do
  sign "$APP_DIR/Contents/Helpers/$bin"
done
sign "$APP_DIR"

# The packaged bytes, not the source tree, are what ships: refuse a release bundle that
# still carries the builder's home path anywhere (scripts/lib/bundle-scan.sh).
if [ "$CONFIGURATION" = release ]; then
  # shellcheck source=lib/bundle-scan.sh
  . "$SCRIPT_DIR/lib/bundle-scan.sh"
  banshee_scan_bundle_for_paths "$APP_DIR" \
    || { echo "build-app.sh: the release bundle carries a builder path; refusing to finish (see scripts/lib/bundle-scan.sh)" >&2; exit 1; }
  echo "==> Bundle scan: no builder paths in the packaged bytes"
fi

echo "==> Done: $APP_DIR"
