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
for tool in cargo-about cargo-cyclonedx; do
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

# Release notes are WRITTEN, not generated: CHANGELOG.md is curated (Keep a Changelog),
# and regenerating it from commit subjects would have replaced the curated file on the
# first pipeline rehearsal. The gate is that the section already exists.
"$SCRIPT_DIR/check-changelog-section.sh" "$VERSION" \
  || die "CHANGELOG.md has no section for $VERSION; write the notes, land them on main, then cut"

info "generating SBOM (CycloneDX) into sbom/"
# cargo-cyclonedx writes <crate>.cdx.json next to each crate's Cargo.toml (gitignored
# there); the tracked copies live in sbom/. The earlier one-liner copied them to
# ../sbom.cdx.json — OUTSIDE the repository — and its `git add` of the missing path
# then failed as a whole, so no notes commit was ever made (first rehearsal, 2026-09-15).
(cd rust && cargo cyclonedx --format json) || die "cargo-cyclonedx failed; a release ships its SBOM"
mkdir -p sbom
sbom_count=0
for f in rust/*/*.cdx.json; do
  [ -f "$f" ] || continue
  /bin/cp -f "$f" "sbom/$(basename "$f")"; sbom_count=$((sbom_count + 1))
done
[ "$sbom_count" -gt 0 ] || die "cargo-cyclonedx produced no rust/*/*.cdx.json"
info "SBOM: $sbom_count crate(s) in sbom/"

# `git add a b c` with ONE missing path adds NOTHING and exits 1; add what exists.
to_add=()
for f in THIRD-PARTY-NOTICES.html sbom; do [ -e "$f" ] && to_add+=("$f"); done
git add -- "${to_add[@]}"
git diff --cached --quiet || git commit -m "release: v$VERSION attribution + SBOM"

# ---------------------------------------------------------------------------
# Tag
# ---------------------------------------------------------------------------
git tag -a "v$VERSION" -m "Banshee v$VERSION"
info "tagged v$VERSION"
info "next: git push && git push --tags, then ./scripts/publish.sh"
