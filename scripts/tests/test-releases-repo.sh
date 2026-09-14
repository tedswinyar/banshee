#!/usr/bin/env bash
set -u

# test-releases-repo.sh — tests OF scripts/lib/releases-repo.sh, the one place that
# decides which GitHub repository publish.sh uploads a release to. Releases live on
# the source repository itself (no cross-repo token), so the answer is normally
# derived — from GITHUB_REPOSITORY on the runner, from `origin` on a developer
# machine — and RELEASES_REPO is only an override. A wrong derivation would publish
# a signed DMG to the wrong place or fail minutes after notarization, so each
# branch is pinned here, including the refusals.

SCRIPT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
# shellcheck source=../lib/releases-repo.sh
. "$SCRIPT_DIR/lib/releases-repo.sh"
PASS=0
FAIL=0
t() { local d="$1"; shift; if "$@" >/dev/null 2>&1; then PASS=$((PASS+1)); else FAIL=$((FAIL+1)); echo "  ✗ $d" >&2; fi; }
t_fails() { local d="$1"; shift; if "$@" >/dev/null 2>&1; then FAIL=$((FAIL+1)); echo "  ✗ $d (succeeded and should not have)" >&2; else PASS=$((PASS+1)); fi; }

WORK="$(mktemp -d /tmp/banshee-releases-repo-test.XXXXXX)"
trap 'rm -rf "$WORK"' EXIT
export GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_NOSYSTEM=1

# A throwaway checkout whose origin URL each case sets. The function is run in a
# clean environment so the developer's own RELEASES_REPO/GITHUB_REPOSITORY cannot
# leak into a case that is testing their absence.
REPO="$WORK/checkout"; git init -q "$REPO"
resolve() { # [VAR=value …] — prints "rc:<n> repo:<answer>"
  local out rc
  out="$(env -u RELEASES_REPO -u GITHUB_REPOSITORY "$@" bash -c '. "$0/lib/releases-repo.sh"; banshee_releases_repo "$1"' "$SCRIPT_DIR" "$REPO" 2>/dev/null)"; rc=$?
  printf 'rc:%s repo:%s\n' "$rc" "$out"
}
set_origin() { git -C "$REPO" remote remove origin 2>/dev/null; [ -n "$1" ] && git -C "$REPO" remote add origin "$1"; return 0; }

# 1. No origin, nothing set: refuses (exit 1, prints nothing) rather than guessing.
set_origin ""
t "nothing configured → refuses" test "$(resolve)" = "rc:1 repo:"

# 2. The runner's shape: GITHUB_REPOSITORY names the repo the job runs in.
t "GITHUB_REPOSITORY is used on the runner" \
  test "$(resolve GITHUB_REPOSITORY=someone/banshee)" = "rc:0 repo:someone/banshee"

# 3. RELEASES_REPO is an OVERRIDE and beats the runner's own repository.
t "RELEASES_REPO overrides GITHUB_REPOSITORY" \
  test "$(resolve GITHUB_REPOSITORY=someone/banshee RELEASES_REPO=someone/elsewhere)" = "rc:0 repo:someone/elsewhere"

# 4. A developer checkout: origin's URL, in every spelling git accepts for GitHub.
set_origin "https://github.com/someone/banshee.git"
t "https origin with .git" test "$(resolve)" = "rc:0 repo:someone/banshee"
set_origin "https://github.com/someone/banshee/"
t "https origin, no .git, trailing slash" test "$(resolve)" = "rc:0 repo:someone/banshee"
set_origin "git@github.com:someone/banshee.git"
t "scp-style ssh origin" test "$(resolve)" = "rc:0 repo:someone/banshee"
set_origin "ssh://git@github.com/someone/banshee"
t "ssh:// origin" test "$(resolve)" = "rc:0 repo:someone/banshee"

# 5. An origin that is not GitHub cannot be a Releases target: refuse, do not mangle.
set_origin "ssh://builder@build-host/Users/builder/repos/banshee.git"
t "a non-GitHub origin → refuses" test "$(resolve)" = "rc:1 repo:"
set_origin "https://github.com/someone"
t "a GitHub URL without a repo → refuses" test "$(resolve)" = "rc:1 repo:"

# 6. …and the override still wins over a useless origin.
t "RELEASES_REPO wins over a non-GitHub origin" \
  test "$(resolve RELEASES_REPO=someone/banshee)" = "rc:0 repo:someone/banshee"

# 7. The callers actually go through this function (a hand-rolled parse in one of
#    them would silently diverge from these pins).
t "publish.sh resolves the repo through the library" \
  grep -q 'banshee_releases_repo' "$SCRIPT_DIR/publish.sh"
t "doctor.sh resolves the repo through the library" \
  grep -q 'banshee_releases_repo' "$SCRIPT_DIR/doctor.sh"
t "release.yml publishes with the workflow's own token, not a cross-repo secret" \
  bash -c "! grep -q 'RELEASES_TOKEN' '$SCRIPT_DIR/../.github/workflows/release.yml'"

echo "test-releases-repo: $PASS passed, $FAIL failed"
[ "$FAIL" -eq 0 ]
