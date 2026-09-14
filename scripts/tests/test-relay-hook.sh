#!/usr/bin/env bash
set -u

# test-relay-hook.sh — the build server's post-receive relay, driven against two
# throwaway bare repos: a "relay" (what the development machine pushes to) and a "github" (what the
# relay forwards to). Pins the ONE rename the topology depends on — main arrives as
# staging, so GitHub's main only moves when CI promotes it — plus pass-through of
# other branches, release/* branches and tags, and that a deletion propagates.

SCRIPT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
HOOK="$SCRIPT_DIR/build-server/post-receive"
PASS=0
FAIL=0
t() { local d="$1"; shift; if "$@" >/dev/null 2>&1; then PASS=$((PASS+1)); else FAIL=$((FAIL+1)); echo "  ✗ $d" >&2; fi; }
t_fails() { local d="$1"; shift; if "$@" >/dev/null 2>&1; then FAIL=$((FAIL+1)); echo "  ✗ $d (succeeded and should not have)" >&2; else PASS=$((PASS+1)); fi; }

WORK="$(mktemp -d /tmp/banshee-relay-test.XXXXXX)"
trap 'rm -rf "$WORK"' EXIT
export GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_NOSYSTEM=1
export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@x GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@x

GITHUB="$WORK/github.git"; git init -q --bare "$GITHUB"
RELAY="$WORK/relay.git";   git init -q --bare "$RELAY"
git -C "$RELAY" remote add github "$GITHUB"
/bin/cp -f "$HOOK" "$RELAY/hooks/post-receive"; chmod +x "$RELAY/hooks/post-receive"

DEV="$WORK/dev"; git init -q "$DEV" -b main
git -C "$DEV" commit -q --allow-empty -m one
git -C "$DEV" remote add mbp "$RELAY"

ref_of() { git -C "$1" rev-parse --verify -q "$2" 2>/dev/null || echo none; }

# 1. main → staging (and NOT main) on github.
t "pushing main to the relay succeeds" git -C "$DEV" push -q mbp main
t "main arrives on github as staging" test "$(ref_of "$GITHUB" refs/heads/staging)" = "$(ref_of "$DEV" HEAD)"
t "github main is NOT created by the relay" test "$(ref_of "$GITHUB" refs/heads/main)" = none

# 2. Other branches keep their names.
git -C "$DEV" checkout -q -b feature/x; git -C "$DEV" commit -q --allow-empty -m two
t "a feature branch pushes" git -C "$DEV" push -q mbp feature/x
t "a feature branch keeps its name on github" test "$(ref_of "$GITHUB" refs/heads/feature/x)" = "$(ref_of "$DEV" feature/x)"

# 3. release/* branches and tags pass through under their own names.
git -C "$DEV" checkout -q main; git -C "$DEV" checkout -q -b release/0.1.4
t "a release branch pushes" git -C "$DEV" push -q mbp release/0.1.4
t "release/0.1.4 keeps its name on github" test "$(ref_of "$GITHUB" refs/heads/release/0.1.4)" = "$(ref_of "$DEV" HEAD)"
git -C "$DEV" tag -a v0.1.4 -m v0.1.4
t "a tag pushes" git -C "$DEV" push -q mbp v0.1.4
t "the tag arrives on github" test "$(ref_of "$GITHUB" 'refs/tags/v0.1.4^{commit}')" = "$(ref_of "$DEV" 'v0.1.4^{commit}')"

# 4. A second main push moves staging (fast-forward).
git -C "$DEV" checkout -q main; git -C "$DEV" commit -q --allow-empty -m three
t "a second main push forwards" git -C "$DEV" push -q mbp main
t "staging moved to the new tip" test "$(ref_of "$GITHUB" refs/heads/staging)" = "$(ref_of "$DEV" main)"

# 5. Deleting a branch on the relay deletes it on github.
t "deleting the release branch on the relay succeeds" git -C "$DEV" push -q mbp --delete release/0.1.4
t "the deletion propagated to github" test "$(ref_of "$GITHUB" refs/heads/release/0.1.4)" = none

# 6. A forward that cannot fast-forward FAILS loudly (the relay push itself lands,
#    the hook reports the failure and exits non-zero). github's staging sits at
#    "three"; a main rewritten to one+diverged does not contain it.
THREE="$(ref_of "$GITHUB" refs/heads/staging)"
git -C "$DEV" reset -q --hard HEAD~1; git -C "$DEV" commit -q --allow-empty -m diverged
git -C "$DEV" push -q -f mbp main 2>"$WORK/err" || true   # -f is for the RELAY; the forward has no -f
t "a non-fast-forward forward is reported as a failure" grep -q "FAILED to forward" "$WORK/err"
t "…and github's staging was NOT force-moved" test "$(ref_of "$GITHUB" refs/heads/staging)" = "$THREE"

# 7. The hook honours BANSHEE_RELAY_REMOTE (how the docs say to point it elsewhere).
OTHER="$WORK/other.git"; git init -q --bare "$OTHER"; git -C "$RELAY" remote add other "$OTHER"
printf '%s %s %s\n' "$(ref_of "$DEV" main)" "$(ref_of "$DEV" main)" refs/heads/feature/y \
  | (cd "$RELAY" && BANSHEE_RELAY_REMOTE=other GIT_DIR="$RELAY" bash hooks/post-receive) 2>/dev/null
t_fails "the remote name is not hardcoded" test "$(ref_of "$OTHER" refs/heads/feature/y)" = none

echo "test-relay-hook: $PASS passed, $FAIL failed"
[ "$FAIL" -eq 0 ]
