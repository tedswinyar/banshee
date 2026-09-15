#!/usr/bin/env bash
set -euo pipefail
# check-changelog-section.sh — a release must have its notes WRITTEN before it is cut.
#
# Usage: scripts/check-changelog-section.sh <version> [<CHANGELOG.md>]
#
# CHANGELOG.md is curated by hand (Keep a Changelog). release.sh used to regenerate it
# with git-cliff, which on the first pipeline rehearsal (2026-09-15) would have replaced
# the 111-line curated file with 33 lines of commit subjects — the only reason it did not
# is that the surrounding `git add` failed for an unrelated reason. Now the release gate
# is the other way round: the section for <version> must already exist, and the tool
# writes nothing.
#
# The heading must be exactly `## [<version>]` followed by a space or end of line —
# `## [0.1.50]` does not satisfy 0.1.5, and `## [Unreleased]` satisfies nothing.
# Exit 0 iff present; exit 1 with the reason on stderr.
VERSION="${1:-}"
FILE="${2:-$(cd "$(dirname "$0")/.." && pwd)/CHANGELOG.md}"
fail() { printf 'changelog-section: %s\n' "$*" >&2; exit 1; }
[ -n "$VERSION" ] || fail "usage: check-changelog-section.sh <version> [CHANGELOG.md]"
printf '%s' "$VERSION" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+$' \
  || fail "'$VERSION' is not MAJOR.MINOR.PATCH — only a release version has a section (Unreleased is not one)"
[ -f "$FILE" ] || fail "no changelog at $FILE"
# Literal match: the version's dots must not be regex wildcards.
ESCAPED="$(printf '%s' "$VERSION" | sed 's/[.[\*^$]/\\&/g')"
if grep -Eq "^## \[$ESCAPED\]( |$)" "$FILE"; then
  echo "changelog-section: $FILE has a section for $VERSION"
else
  fail "$FILE has no '## [$VERSION]' section — write the release notes first (Keep a Changelog); the release will not write them for you"
fi
