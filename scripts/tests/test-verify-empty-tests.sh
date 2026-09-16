#!/usr/bin/env bash
set -u

# test-verify-empty-tests.sh — an empty scripts/tests/ must FAIL, not pass.
#
# THE BUG THIS EXISTS FOR (upstream-template bead rlm, found 2026-09-02 alongside the completion-guard defect):
# run_scripts iterated `scripts/tests/test-*.sh` and returned 0 when the glob
# matched nothing, so an empty or emptied tests directory reported the
# tests-of-the-gates suite as PASS while running nothing. Same class as vv8:
# not a gate that fails, a gate that is absent and looks satisfied. It matters
# most in the template because init.sh prunes layers by DELETION — but a prune
# deletes the whole directory (reported SKIPPED, which stays legitimate), so a
# directory that EXISTS with no tests means the tests vanished, and that must
# be red. This test pins both sides of that distinction.

SCRIPT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
PASS=0
FAIL=0

t() {
  local desc="$1"
  shift
  if "$@" >/dev/null 2>&1; then
    PASS=$((PASS + 1))
  else
    FAIL=$((FAIL + 1))
    echo "  ✗ $desc" >&2
  fi
}

WORK="$(mktemp -d /tmp/banshee-verify-empty-tests.XXXXXX)"
cleanup() { rm -rf "${WORK:-}"; }
# shellcheck source=../lib/completion-guard.sh
. "$SCRIPT_DIR/lib/completion-guard.sh"
arm_completion_guard "test-verify-empty-tests" cleanup

mkdir -p "$WORK/scripts"
cp "$SCRIPT_DIR/verify.sh" "$WORK/scripts/"
chmod +x "$WORK/scripts/verify.sh"

# 1. scripts/tests exists but is EMPTY: the suite must FAIL and say why.
mkdir -p "$WORK/scripts/tests"
OUT="$("$WORK/scripts/verify.sh" --scripts-only 2>&1)"
STATUS=$?
t "empty scripts/tests exits nonzero" [ "$STATUS" -ne 0 ]
t "failure names the scripts suite" \
  bash -c "echo \"\$1\" | grep -q 'FAILED: scripts'" _ "$OUT"
t "failure explains the empty glob (rlm)" \
  bash -c "echo \"\$1\" | grep -q 'upstream-template bead rlm'" _ "$OUT"

# 2. Deleted directory (the deliberate-prune shape) must stay SKIPPED, exit 0.
rm -rf "$WORK/scripts/tests"
OUT="$("$WORK/scripts/verify.sh" 2>&1)"
STATUS=$?
t "pruned scripts/tests exits 0" [ "$STATUS" -eq 0 ]
t "pruned scripts/tests reports skipped" \
  bash -c "echo \"\$1\" | grep -q 'skipped (layer not present):.*scripts'" _ "$OUT"

# 3. One passing test must still PASS — no false red from the new check.
mkdir -p "$WORK/scripts/tests"
cat > "$WORK/scripts/tests/test-noop.sh" <<'EOF'
#!/bin/sh
exit 0
EOF
chmod +x "$WORK/scripts/tests/test-noop.sh"
OUT="$("$WORK/scripts/verify.sh" --scripts-only 2>&1)"
STATUS=$?
t "one passing test exits 0" [ "$STATUS" -eq 0 ]

# 4. A failing test still fails (the count check must not mask real failures).
cat > "$WORK/scripts/tests/test-red.sh" <<'EOF'
#!/bin/sh
exit 1
EOF
chmod +x "$WORK/scripts/tests/test-red.sh"
"$WORK/scripts/verify.sh" --scripts-only >/dev/null 2>&1
t "a failing test still exits nonzero" [ "$?" -ne 0 ]

echo "test-verify-empty-tests: $PASS passed, $FAIL failed"
[ "$FAIL" -eq 0 ] && finish 0 || finish 1
