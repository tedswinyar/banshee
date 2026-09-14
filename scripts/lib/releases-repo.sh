# releases-repo.sh — WHICH GitHub repository a release is published to, in ONE place.
#
# The DMG and the Sparkle appcast live on the source repository's own Releases page,
# so the release workflow's GITHUB_TOKEN suffices and there is no cross-repo secret
# to mint, rotate or leak. banshee_releases_repo prints owner/name, resolved as:
#   1. RELEASES_REPO (release.conf or the environment) — an explicit override, for
#      publishing somewhere other than the source repository;
#   2. GITHUB_REPOSITORY — set by GitHub Actions to the repository the job runs in;
#   3. the `origin` remote of the checkout given as $1 (default: the current
#      directory), parsed for owner/name — https and ssh forms, with or without .git.
# Exits 1 with nothing printed when none of the three resolves, so a caller's
# `repo="$(banshee_releases_repo)" || die …` reads as intended.

banshee_releases_repo() {
  if [ -n "${RELEASES_REPO:-}" ]; then printf '%s\n' "$RELEASES_REPO"; return 0; fi
  if [ -n "${GITHUB_REPOSITORY:-}" ]; then printf '%s\n' "$GITHUB_REPOSITORY"; return 0; fi
  local url repo
  url="$(git -C "${1:-.}" remote get-url origin 2>/dev/null)" || return 1
  url="${url%/}"
  url="${url%.git}"
  case "$url" in
    https://github.com/*/*)   repo="${url#https://github.com/}" ;;
    git@github.com:*/*)       repo="${url#git@github.com:}" ;;
    ssh://git@github.com/*/*) repo="${url#ssh://git@github.com/}" ;;
    *) return 1 ;;
  esac
  case "$repo" in */*/*|*/|/*) return 1 ;; esac
  printf '%s\n' "$repo"
}
