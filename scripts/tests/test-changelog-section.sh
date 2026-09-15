#!/usr/bin/env bash
set -u

# test-changelog-section.sh — tests OF scripts/check-changelog-section.sh, plus the one
# assertion between releases: the REAL changelog has a section for the version the tree
# says it is (so a bump without notes is red in verify, not on release day).
#
# Every negative case is one a sloppier matcher would accept: a longer version that
# starts with the wanted one, the Unreleased heading, the version in body text, a
# deeper heading level.

SCRIPT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
ROOT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
CHECK="$SCRIPT_DIR/check-changelog-section.sh"
PASS=0
FAIL=0
t() {
  local desc="$1"; shift
  if "$@" >/dev/null 2>&1; then PASS=$((PASS + 1)); else
    FAIL=$((FAIL + 1)); echo "  ✗ $desc" >&2; fi
}
t_fails() {
  local desc="$1"; shift
  if "$@" >/dev/null 2>&1; then
    FAIL=$((FAIL + 1)); echo "  ✗ $desc (passed and should not have)" >&2
  else PASS=$((PASS + 1)); fi
}
WORK="$(mktemp -d /tmp/banshee-changelog-test.XXXXXX)"
trap 'rm -rf "$WORK"' EXIT

F="$WORK/CHANGELOG.md"
cat > "$F" <<'MD'
# Changelog

## [Unreleased]

Nothing yet. Version 0.2.0 is planned.

## [0.1.50] — 2026-12-01

### Fixed
- Something in 0.1.5 that regressed.

## [0.1.4] — 2026-09-07
MD

t       "a present section passes"                       "$CHECK" 0.1.4 "$F"
t       "a present section with a dash date passes"      "$CHECK" 0.1.50 "$F"
t_fails "a LONGER version that starts with the wanted one does not count (0.1.5 vs 0.1.50)" "$CHECK" 0.1.5 "$F"
t_fails "the version in body text is not a section"      "$CHECK" 0.2.0 "$F"
t_fails "Unreleased satisfies nothing"                   "$CHECK" Unreleased "$F"
t_fails "no version argument is an error"                "$CHECK" "" "$F"
t_fails "a missing file is an error"                     "$CHECK" 0.1.4 "$WORK/nope.md"

# Heading level and spacing are part of the contract.
printf '### [0.3.0] — 2026-01-01\n' > "$WORK/deep.md"
t_fails "a level-3 heading is not a release section"     "$CHECK" 0.3.0 "$WORK/deep.md"
printf '## [0.3.0]\n' > "$WORK/bare.md"
t       "a bare heading with no date passes"             "$CHECK" 0.3.0 "$WORK/bare.md"
printf '## [0.3.0]-rc1\n' > "$WORK/suffix.md"
t_fails "a suffix glued to the bracket does not count"   "$CHECK" 0.3.0 "$WORK/suffix.md"

# Dots are literal: a matcher using the version as a regex would accept 0x1x4.
printf '## [0x1x4] — 2026-01-01\n' > "$WORK/dots.md"
t_fails "dots in the version are literal, not wildcards" "$CHECK" 0.1.4 "$WORK/dots.md"

# The real tree: the version the tree states must have its notes written.
REAL_VERSION="$(sed -n 's/.*static let marketing = "\(.*\)".*/\1/p' "$ROOT_DIR/swift/Sources/Banshee/Version.swift")"
t "the real CHANGELOG.md has a section for the tree's version ($REAL_VERSION)" "$CHECK" "$REAL_VERSION" "$ROOT_DIR/CHANGELOG.md"

echo "test-changelog-section: $PASS passed, $FAIL failed"
[ "$FAIL" -eq 0 ]
