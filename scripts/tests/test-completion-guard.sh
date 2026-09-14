#!/usr/bin/env bash
# test-completion-guard.sh — prove the completion guard discriminates.
#
# "the gate passed" is worthless information unless the gate would have failed.
# On macOS bash 3.2, a script with `set -e`, `set -u` AND an EXIT trap exits 0 when
# an unbound variable aborts it, so a crashed gate reports success.
# scripts/lib/completion-guard.sh fixes that; this test is what stops it being
# silently removed or broken.
#
# It builds throwaway scripts in a temp dir and runs the REAL guard from
# scripts/lib/completion-guard.sh against each, asserting BOTH directions:
#
#   guarded + completes normally  -> exit 0        (a guard that always fails is useless)
#   guarded + set -u abort        -> exit nonzero  (a guard that never fires is a lie)
#   guarded + bare exit 0         -> exit nonzero  (an exit that skipped finish() is premature)
#   guarded + finish 3            -> exit 3        (deliberate statuses survive)
#   guarded + command failure     -> exit nonzero
#
# and then the three control cases that pin the trigger matrix documented in
# completion-guard.sh — the bug needs -e AND -u AND a trap, so removing any one of
# them must restore a correct nonzero exit:
#
#   UNGUARDED set -euo pipefail + trap -> exit 0        (the bug, reproduced)
#   UNGUARDED bare set -u      + trap -> exit nonzero  (no -e: not affected)
#   UNGUARDED set -euo pipefail, none -> exit nonzero  (no trap: not affected)
#
# Those controls are the point. If a future bash stops swallowing the abort, the
# first flips and this test fails loudly — the signal to re-measure rather than to
# keep carrying the guard on faith. And if the matrix in completion-guard.sh is
# ever edited to something untrue, the other two catch it.
#
# It sources the real guard rather than restating its logic, so weakening
# completion-guard.sh breaks THIS test. A test carrying its own copy of the guard
# would just be testing its copy.

set -u

SCRIPT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
GUARD="$SCRIPT_DIR/lib/completion-guard.sh"

PASS=0
FAIL=0

ok()   { PASS=$((PASS + 1)); }
bad()  { FAIL=$((FAIL + 1)); echo "  ✗ $*" >&2; }

WORK="$(mktemp -d /tmp/banshee-completion-guard.XXXXXX)"
cleanup() { rm -rf "${WORK:-}"; }
# This test guards itself with the thing it is testing. If the guard is broken
# badly enough to break this script, the failure surfaces here first.
# shellcheck source=../lib/completion-guard.sh
. "$GUARD"
arm_completion_guard "test-completion-guard" cleanup

[ -r "$GUARD" ] || { echo "test-completion-guard: missing $GUARD" >&2; finish 2; }

# write_case <file> <guarded:yes|no> <body> [set-flags]
write_case() {
  local file="$1" guarded="$2" body="$3" flags="${4:-set -euo pipefail}"
  {
    echo '#!/usr/bin/env bash'
    echo "$flags"
    if [ "$guarded" = "yes" ]; then
      echo ". \"$GUARD\""
      echo 'noop_cleanup() { :; }'
      echo 'arm_completion_guard "case" noop_cleanup'
    elif [ "$guarded" = "notrap" ]; then
      : # no guard and no trap: the third leg of the trigger matrix
    else
      echo 'noop_cleanup() { :; }'
      echo 'trap noop_cleanup EXIT'
    fi
    echo "$body"
  } > "$file"
  chmod +x "$file"
}

# expect <desc> <expected-shape> <file>
#   shape: "zero" | "nonzero" | an exact numeric status
expect() {
  local desc="$1" want="$2" file="$3" got
  set +e
  "$file" >/dev/null 2>&1
  got=$?
  set -e
  case "$want" in
    zero)    [ "$got" -eq 0 ] && ok || bad "$desc: expected 0, got $got" ;;
    nonzero) [ "$got" -ne 0 ] && ok || bad "$desc: expected nonzero, got $got" ;;
    *)       [ "$got" -eq "$want" ] && ok || bad "$desc: expected $want, got $got" ;;
  esac
}

# --- the guard must not break the happy path -------------------------------
write_case "$WORK/complete.sh" yes 'echo work; finish 0'
expect "guarded script that completes exits 0" zero "$WORK/complete.sh"

# --- the guard must catch the bug ------------------------------------------
write_case "$WORK/abort.sh" yes 'echo work; echo "v: $UNSET_VARIABLE"; finish 0'
expect "guarded set -u abort exits nonzero" nonzero "$WORK/abort.sh"

# --- an exit that bypassed finish() is premature, even with status 0 -------
write_case "$WORK/bare-exit.sh" yes 'echo work; exit 0'
expect "guarded bare 'exit 0' is reported as premature" nonzero "$WORK/bare-exit.sh"

# --- deliberate nonzero statuses must survive unmangled --------------------
write_case "$WORK/deliberate.sh" yes 'echo work; finish 3'
expect "finish 3 exits 3" 3 "$WORK/deliberate.sh"

# --- a real command failure must still propagate ---------------------------
write_case "$WORK/cmdfail.sh" yes 'echo work; false; finish 0'
expect "set -e failure still exits nonzero" nonzero "$WORK/cmdfail.sh"

# --- THE CONTROL: without the guard, this shell swallows the abort ---------
# If this stops being true, the platform changed and the guard's rationale needs
# re-measuring. Failing here is a prompt to re-read completion-guard.sh, not a bug.
write_case "$WORK/unguarded.sh" no 'echo work; echo "v: $UNSET_VARIABLE"; echo done'
expect "UNGUARDED set -euo pipefail abort exits 0 (bash $BASH_VERSION swallows it)" zero "$WORK/unguarded.sh"

# --- and pin WHY it needs -e: bare set -u is not affected -------------------
# completion-guard.sh documents a trigger matrix; these two cases are what stop
# that matrix from being wrong. Dropping -e, or the trap, restores correct exits.
write_case "$WORK/bare-u.sh" no 'echo work; echo "v: $UNSET_VARIABLE"; echo done' 'set -u'
expect "UNGUARDED bare 'set -u' abort exits nonzero (needs -e to break)" nonzero "$WORK/bare-u.sh"

write_case "$WORK/notrap.sh" notrap 'echo work; echo "v: $UNSET_VARIABLE"; echo done'
expect "set -euo pipefail with NO trap exits nonzero (needs a trap to break)" nonzero "$WORK/notrap.sh"

echo "test-completion-guard: $PASS passed, $FAIL failed"
[ "$FAIL" -eq 0 ] && finish 0 || finish 1
