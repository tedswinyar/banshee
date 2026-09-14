#!/usr/bin/env bash
set -u

# test-doctor.sh — tests OF scripts/doctor.sh's pre-push-hook clause. The hook is a
# developer-clone concern; a CI checkout (actions/checkout on the build server) never
# has one, and requiring it there failed every workflow job at "Toolchain present"
# before the gate ever ran (found provisioning the build server, 2026-09-13). So: without
# GITHUB_ACTIONS a missing hook is a MISS and exit 1; with it, the clause is skipped
# and says so; an installed hook is ok either way. The required-tool checks run for
# real against this machine, exactly as the scripts suite already assumes.

SCRIPT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
PASS=0
FAIL=0
t() { local d="$1"; shift; if "$@" >/dev/null 2>&1; then PASS=$((PASS+1)); else FAIL=$((FAIL+1)); echo "  ✗ $d" >&2; fi; }

WORK="$(mktemp -d /tmp/banshee-doctor-test.XXXXXX)"
trap 'rm -rf "$WORK"' EXIT
mkdir -p "$WORK/scripts"
/bin/cp -f "$SCRIPT_DIR/doctor.sh" "$WORK/scripts/doctor.sh"
/bin/cp -Rf "$SCRIPT_DIR/lib" "$WORK/scripts/lib"
git -C "$WORK" init -q   # a clone with NO hooks, and no swift/ so that check is skipped

run() { # env... -> "rc:<n>" and the output in $OUT
  OUT="$(env "$@" bash "$WORK/scripts/doctor.sh" 2>&1)"; RC=$?
}

# 1. Developer clone, no hook: MISS and exit 1.
run -u GITHUB_ACTIONS
t "no hook, not CI: exit 1" test "$RC" = 1
t "no hook, not CI: names the pre-push hook" grep -Eq 'MISS.*pre-push hook' <<< "$OUT"

# 2. CI checkout, no hook: the clause is skipped, said out loud, and does not fail.
run GITHUB_ACTIONS=true
t "no hook, CI: exit 0" test "$RC" = 0
t "no hook, CI: no MISS for the hook" bash -c '! grep -Eq "MISS.*pre-push" <<< "$1"' _ "$OUT"
t "no hook, CI: says it skipped the hook check" grep -q 'skip pre-push verify gate' <<< "$OUT"

# 3. With the gate installed, ok in both modes (the skip is about absence, not presence).
printf '#!/bin/sh\n# --- BEGIN BANSHEE VERIFY GATE v1 ---\n' > "$WORK/.git/hooks/pre-push"
run -u GITHUB_ACTIONS
t "hook present, not CI: exit 0" test "$RC" = 0
t "hook present, not CI: reports it" grep -q 'pre-push verify gate installed' <<< "$OUT"

echo "test-doctor: $PASS passed, $FAIL failed"
[ "$FAIL" -eq 0 ]
