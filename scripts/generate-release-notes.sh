#!/usr/bin/env bash
set -euo pipefail

# generate-release-notes.sh — regenerate CHANGELOG.md from git history via
# git-cliff, and mirror it into the website's changelog page when the
# website layer exists. Called by release.sh; safe to run any time.

ROOT_DIR="$(cd "$(dirname "$0")/.." && pwd)"

if ! command -v git-cliff >/dev/null; then
  echo "generate-release-notes: git-cliff not found (brew install git-cliff)" >&2
  exit 1
fi

cd "$ROOT_DIR"
git-cliff --config cliff.toml -o CHANGELOG.md
echo "generate-release-notes: wrote CHANGELOG.md"

if [ -d website/content ]; then
  {
    printf -- '---\ntitle: "Changelog"\nlayout: "changelog"\n---\n\n'
    # Strip the H1 — the page layout supplies the title.
    sed '1,/^All notable changes/d' CHANGELOG.md
  } > website/content/changelog.md
  echo "generate-release-notes: mirrored into website/content/changelog.md"
fi
