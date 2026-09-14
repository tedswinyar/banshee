# completion-guard.sh — make "the script finished" distinguishable from
# "the script crashed", on a shell where those look identical.
#
# THE BUG THIS EXISTS FOR
# -----------------------
# macOS ships bash 3.2.57, and on this machine it is the ONLY bash, so
# `#!/usr/bin/env bash` resolves to /bin/bash and every script here runs on 3.2.
# On 3.2, when an EXIT trap is installed, a `set -u` unbound-variable abort
# delivers status 0 to the trap. The script CRASHES AND EXITS 0.
#
#   $ cat /tmp/t.sh
#   #!/usr/bin/env bash
#   set -euo pipefail
#   trap 'echo cleanup' EXIT
#   echo "$UNSET_VAR"          # aborts here
#   $ /bin/bash /tmp/t.sh; echo "exit=$?"
#   /tmp/t.sh: line 4: UNSET_VAR: unbound variable
#   cleanup
#   exit=0                     # <-- reports SUCCESS
#
# Measured 2026-09-03 on bash 3.2.57. The trigger is narrower than it first looks,
# which is exactly why it survives casual testing — it needs `-e` AND `-u` AND a
# trap, and dropping any one of the three makes it behave correctly:
#
#   set flags            trap?   exit on a set -u abort
#   -------------------  -----   ----------------------
#   set -u               no      1   correct
#   set -u               yes     1   correct  <- bare -u is NOT affected
#   set -uo pipefail     yes     1   correct
#   set -eu              no      1   correct
#   set -eu              yes     0   THE BUG
#   set -euo pipefail    no      1   correct
#   set -euo pipefail    yes     0   THE BUG
#
# Other things worth knowing:
#   * a plain `trap cleanup EXIT` is enough — you do NOT need `exit $?` in the trap
#   * removing `exit $?` from the trap does NOT fix it
#   * `set -e` command failures propagate correctly; it is the -u abort that is lost
#
# Scripts here that use bare `set -u` are therefore not exposed to the abort bug.
# They still arm the guard, for two reasons: it also catches a premature `exit 0`
# that bypassed finish(), and adding `set -e` later is an ordinary edit that would
# otherwise silently introduce the bug.
#
# Why it matters here: the test scripts in scripts/tests/ end with
# `[ "$FAIL" -eq 0 ]` as their exit status, and verify.sh reads that status. An
# abort anywhere above that line is reported to verify.sh as a PASS — a gate that
# never ran, recorded as a gate that succeeded.
#
# USAGE
# -----
#   . "$(dirname "$0")/../lib/completion-guard.sh"
#
#   WORK="$(mktemp -d /tmp/example.XXXXXX)"
#   cleanup() { rm -rf "${WORK:-}"; }
#   arm_completion_guard "test-example" cleanup
#
#   ... work ...
#
#   [ "$FAIL" -eq 0 ] && finish 0 || finish 1
#
# Arm the guard as early as the cleanup allows — anything above the trap is
# unprotected, and argument parsing is exactly where an unset variable is most
# likely. Write cleanup functions with `${VAR:-}` so they are safe to run before
# VAR is set.
#
# Every DELIBERATE exit goes through finish(). A bare `exit` is reported as a
# premature exit, which is the point: crashing into an exit and choosing one stay
# distinguishable. PREMATURE_EXIT_CODE is 70 to match scripts/verify.sh, which
# carries its own inline copy of this logic on purpose — it must be armed before
# it sources anything, so it cannot depend on this file existing.
#
# scripts/tests/test-completion-guard.sh pins all of the above, in both
# directions. Deleting it silently restores the bug.

COMPLETED=0
PREMATURE_EXIT_CODE=70

__GUARD_NAME="guarded script"
__GUARD_CLEANUP=""

# finish <status> — the only way to exit successfully-on-purpose.
finish() {
  COMPLETED=1
  exit "${1:-0}"
}

__guard_on_exit() {
  # Capture status FIRST: anything below, including the cleanup, overwrites $?.
  local s=$?

  if [ -n "$__GUARD_CLEANUP" ]; then
    "$__GUARD_CLEANUP" || true
  fi

  if [ "$COMPLETED" != "1" ]; then
    echo "$__GUARD_NAME: exited before finishing (reported status $s) — treating as FAILURE." >&2
    echo "$__GUARD_NAME: see scripts/lib/completion-guard.sh" >&2
    # A real nonzero status is more informative than PREMATURE_EXIT_CODE, so keep it.
    # ("sentinel" is deliberately NOT the word for this — in this repo it means the
    # name placeholder the project template rewrites.)
    [ "$s" != "0" ] && exit "$s"
    exit "$PREMATURE_EXIT_CODE"
  fi

  exit "$s"
}

# arm_completion_guard <name> [cleanup_fn]
arm_completion_guard() {
  __GUARD_NAME="${1:-guarded script}"
  __GUARD_CLEANUP="${2:-}"
  trap __guard_on_exit EXIT
}
