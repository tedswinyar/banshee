#!/usr/bin/env bash
set -euo pipefail
# setup-relay.sh — as the build user: the bare repo the development machine pushes to, with the
# post-receive relay that forwards to GitHub (docs/build-server.md).
#
#   setup-relay.sh [--github https://github.com/<owner>/<repo>.git]
#
# The GitHub URL defaults to BANSHEE_REPO (build-host.sh); the name printed for the
# developer's remote comes from BANSHEE_BUILD_HOST, or this machine's hostname.
#
# Requires `gh auth login` (or GH_TOKEN) as this user first: the hook pushes over
# HTTPS using gh's credential helper (`gh auth setup-git`), so no PAT sits in a
# remote URL.
export PATH="/opt/homebrew/bin:$HOME/.cargo/bin:/usr/local/bin:$PATH"
ROOT_DIR="$(cd "$(dirname "$0")/../.." && pwd)"
# shellcheck source=build-host.sh
. "$ROOT_DIR/scripts/build-server/build-host.sh"
GITHUB_URL="https://github.com/$(banshee_repo).git"
[ "${1:-}" = "--github" ] && GITHUB_URL="$2"
RELAY="$HOME/repos/banshee.git"
info() { printf 'setup-relay: %s\n' "$*"; }
die() { printf 'setup-relay: ERROR: %s\n' "$*" >&2; exit 1; }

gh auth status >/dev/null 2>&1 || die "gh is not authenticated as $(id -un); run: gh auth login (scopes: repo, workflow)"
gh auth setup-git >/dev/null

mkdir -p "$HOME/repos"
if [ -d "$RELAY" ]; then info "bare repo exists: $RELAY"; else git init -q --bare "$RELAY"; info "created $RELAY"; fi
if git -C "$RELAY" remote get-url github >/dev/null 2>&1; then
  git -C "$RELAY" remote set-url github "$GITHUB_URL"
else
  git -C "$RELAY" remote add github "$GITHUB_URL"
fi
/bin/cp -f "$ROOT_DIR/scripts/build-server/post-receive" "$RELAY/hooks/post-receive"
chmod +x "$RELAY/hooks/post-receive"
# Seed the relay from GitHub so the first forward is a fast-forward, not a fresh history.
git -C "$RELAY" fetch -q github '+refs/heads/*:refs/heads/*' '+refs/tags/*:refs/tags/*' || die "could not fetch $GITHUB_URL — check gh auth"
info "relay seeded from $GITHUB_URL; hook installed"
cat <<NEXT
setup-relay: done. On the development machine, in the banshee checkout:
  git remote add mbp ssh://$(id -un)@$(banshee_build_host)$RELAY
  git push mbp main          # arrives on GitHub as 'staging'; CI promotes it to main
  git push mbp release/0.1.4 # cuts a release on the build server
NEXT
