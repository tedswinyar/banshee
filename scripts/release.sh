#!/usr/bin/env bash
set -euo pipefail

# release.sh — cut a release: validate version alignment, verify, build the
# DMG, regenerate release notes, tag.
#
# Usage: ./scripts/release.sh <version>       e.g. ./scripts/release.sh 0.2.0
#
# Version alignment (VERSIONING.md): Version.swift, rust/Cargo.toml and
# open-prompt-edition/VERSION must all equal <version>. The release BLOCKS
# on divergence — bump them first, deliberately.

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
ROOT_DIR="$(dirname "$SCRIPT_DIR")"

die() {
  printf 'release: ERROR: %s\n' "$*" >&2
  exit 1
}
info() { printf 'release: %s\n' "$*"; }

[ $# -eq 1 ] || die "usage: ./scripts/release.sh <version>"
VERSION="$1"
printf '%s' "$VERSION" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+$' || die "'$VERSION' is not MAJOR.MINOR.PATCH"

cd "$ROOT_DIR"

# ---------------------------------------------------------------------------
# Alignment gates
# ---------------------------------------------------------------------------
# Version.swift AND rust/Cargo.toml [workspace.package] must both equal <version>
# — the daemon's /health and the MCP serverInfo come from CARGO_PKG_VERSION, and
# gating only the app let four releases ship a daemon that said "0.1.0".
# One checker, also run by the scripts suite at HEAD.
"$SCRIPT_DIR/check-version-alignment.sh" "$VERSION" \
  || die "versions disagree; align swift/Sources/Banshee/Version.swift and rust/Cargo.toml first"

if [ -d open-prompt-edition ]; then
  OPE_VERSION="$(tr -d '[:space:]' < open-prompt-edition/VERSION)"
  [ "$OPE_VERSION" = "$VERSION" ] || die "open-prompt-edition/VERSION says '$OPE_VERSION', release says '$VERSION'. They move together (VERSIONING.md)."
fi

[ -z "$(git status --porcelain)" ] || die "working tree is dirty; commit or stash first"
git rev-parse "v$VERSION" >/dev/null 2>&1 && die "tag v$VERSION already exists"

# ---------------------------------------------------------------------------
# Restore drill (banshee-d8r) — FIRST, so a missing daemon fails fast, before
# the multi-minute verify + DMG. A release must ship with proof that the
# database is restorable; this takes a verified backup and re-reads it. Real
# gate, not a checkbox (docs/release-checklist.md).
# ---------------------------------------------------------------------------
info "running the restore drill"
"$SCRIPT_DIR/restore-drill.sh" || die "restore drill failed; no release without a restorable backup"

# ---------------------------------------------------------------------------
# Gates, artifacts, notes
# ---------------------------------------------------------------------------
info "running verify"
./scripts/verify.sh || die "verify failed; no release from a red suite"

# Release tooling is REQUIRED for a release (unlike the develop-time gates,
# a release is a deliberate act — fail closed rather than ship stale notes /
# missing attribution). Run ./scripts/doctor.sh to install these.
for tool in git-cliff cargo-about cargo-cyclonedx; do
  command -v "$tool" >/dev/null || die "$tool is required to cut a release (see ./scripts/doctor.sh)"
done

# Notices are generated BEFORE the DMG so build-dmg.sh can stage them into the
# distributed image (banshee-4z1): Sparkle carries BSD components whose licenses
# must accompany a binary distribution, and a DMG built first would ship stale
# or missing attribution.
info "regenerating THIRD-PARTY-NOTICES.html"
(cd rust && cargo about generate about.hbs -o ../THIRD-PARTY-NOTICES.html) \
  || die "cargo-about failed; attribution must be current for a release"

if [ -d swift ]; then
  info "building DMG"
  ./scripts/build-dmg.sh || die "DMG build failed"
fi

info "generating release notes"
git-cliff --config cliff.toml --tag "v$VERSION" -o CHANGELOG.md
if [ -d website/content ]; then
  ./scripts/generate-release-notes.sh >/dev/null || true
fi

info "generating SBOM (CycloneDX)"
(cd rust && cargo cyclonedx --format json) \
  && find rust -name '*.cdx.json' -maxdepth 2 -exec cp {} ../sbom.cdx.json \; 2>/dev/null \
  || info "WARNING: SBOM generation produced no output; check cargo-cyclonedx"

git add CHANGELOG.md website/content/changelog.md THIRD-PARTY-NOTICES.html sbom.cdx.json 2>/dev/null || true
git diff --cached --quiet || git commit -m "release: v$VERSION notes + attribution + SBOM"

# ---------------------------------------------------------------------------
# Tag
# ---------------------------------------------------------------------------
git tag -a "v$VERSION" -m "Banshee v$VERSION"
info "tagged v$VERSION"
info "next: git push && git push --tags, then ./scripts/publish.sh"
