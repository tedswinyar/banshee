#!/usr/bin/env bash
set -euo pipefail

# mutate.sh — apply one deliberate source mutation, run a test filter, restore.
#
# The testing standard requires that a "pin" be PROVEN by applying the reverting
# mutation and watching the test fail. This exists because doing that by hand is
# where the mistakes happen:
#
#   1. A string-replace that finds nothing silently does nothing, and a test that
#      "passed" against a mutation that never applied looks exactly like a test
#      that caught it. This happened three times in one sitting before anyone
#      inspected the file. So: **this script exits 3 if the target text is
#      absent**, and never reports a green run it cannot vouch for.
#   2. `cargo fmt` reflows multi-line closures and argument lists, so a target
#      string captured before formatting will not match after. Copy the target out
#      of the CURRENT file.
#   3. `cp` is aliased to `cp -i` in this shell, so a restore inside a background
#      job hangs forever on an unanswerable prompt. This uses /bin/cp -f.
#   4. **"Did it compile?" must be judged from whether tests RAN, not by grepping
#      for "error:".** An XCTest assertion failure prints
#      `…PressureModelTests.swift:44: error: -[…] : XCTAssertEqual failed`, so a
#      grep for `error:` reports a perfectly caught mutation as a broken one — and
#      a broken mutation is indistinguishable from a caught one, which is failure
#      mode (1) again wearing a different hat. This script decides in order:
#      did any test run? then did any fail? (2026-08-31, P4, found by mutating
#      Swift by hand before this dispatch existed.)
#
# The suite is chosen from the FILE PATH, except that the filter `e2e` always
# means the parity harness:
#
#   <any>, filter e2e   tests/e2e/run-e2e.sh          (the real binaries, both transports)
#   swift/…             swift test --filter <filter>
#   scripts/…           scripts/tests/<filter>.sh     (the scripts suite)
#   *                   cargo test <filter>
#
# One entry point, because a project with three suites and a mutation harness that
# only covers one of them will quietly stop mutation-testing the other two. The
# `scripts/` arm was added in P5 for the same reason the `swift/` arm was added in
# P4: the daemon install/check gates are shell, they guard an eight-day outage
# shape, and "we mutation-test our tests" cannot mean "except those". The `e2e`
# arm was added for ADR-0008 Phase 3: the pairing of router flavour to listener in
# `main.rs` is a security decision no unit test can see (the unit tests build the
# routers themselves), and only the harness drives the shipped binary over both
# transports. Without this arm that pin could be recorded nowhere.
#
# Usage:
#   scripts/mutate.sh <file> <test-filter> <<'EOF'
#   ---OLD---
#   the exact text to replace
#   ---NEW---
#   what to replace it with
#   EOF
#
# Exit codes: 0 = the mutation was caught (the test FAILED, which is success
# here); 1 = the mutation was NOT caught (the test passed — the pin is false);
# 2 = no test ran, or the mutated source did not build; 3 = the mutation did not
# apply.

if [ "$#" -lt 2 ]; then
    sed -n '3,32p' "$0" | sed 's/^# \{0,1\}//'
    exit 2
fi

FILE="$1"
FILTER="$2"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
TARGET="$ROOT/$FILE"
[ -f "$TARGET" ] || { echo "mutate: no such file: $TARGET" >&2; exit 2; }

SPEC="$(cat)"
OLD="$(printf '%s' "$SPEC" | sed -n '/^---OLD---$/,/^---NEW---$/p' | sed '1d;$d')"
NEW="$(printf '%s' "$SPEC" | sed -n '/^---NEW---$/,$p' | sed '1d')"
[ -n "$OLD" ] || { echo "mutate: empty ---OLD--- block" >&2; exit 2; }

BACKUP="$(mktemp)"
/bin/cp -f "$TARGET" "$BACKUP"
restore() {
  [ -n "${BACKUP:-}" ] && [ -f "$BACKUP" ] || return 0
  /bin/cp -f "$BACKUP" "$TARGET"; rm -f "$BACKUP"
}
# On macOS bash 3.2 an EXIT trap turns a set -u abort into exit 0, so this
# script could crash and report 'CAUGHT'. See scripts/lib/completion-guard.sh.
# shellcheck source=lib/completion-guard.sh
. "$ROOT/scripts/lib/completion-guard.sh"
arm_completion_guard "mutate" restore

OLD="$OLD" NEW="$NEW" python3 - "$TARGET" <<'PY'
import os, sys, pathlib
p = pathlib.Path(sys.argv[1])
t = p.read_text()
old, new = os.environ["OLD"], os.environ["NEW"]
if old not in t:
    print("mutate: TARGET TEXT NOT FOUND — the mutation did not apply.", file=sys.stderr)
    print("mutate: copy the target out of the CURRENT file (cargo fmt moves it).", file=sys.stderr)
    sys.exit(3)
p.write_text(t.replace(old, new, 1))
PY

echo "mutate: applied to $FILE; running \`$FILTER\`"

# Capture to a file rather than piping into grep. `set -o pipefail` plus a
# DELIBERATELY failing command is a trap: cargo exits 101 under a caught
# mutation, so `cargo test | grep -q` returns 101 even when grep matched, and the
# script reported NOT CAUGHT for a perfectly good pin. The harness written to
# prevent silent misreporting was silently misreporting. `|| true` because a
# non-zero cargo exit is the EXPECTED outcome here.
LOG="$(mktemp)"
restore_and_clean() { restore; rm -f "${LOG:-}"; return 0; }
arm_completion_guard "mutate" restore_and_clean

if [ "$FILTER" = "e2e" ]; then
    # The parity harness prints one `e2e: …` progress line per section and ends in
    # `e2e: PASS` or `e2e: FAIL: <why>`. "Did a test run" is the announcement line,
    # which only appears once the mutated source has BUILT and the daemon has come
    # up; a compile error stops the harness at `cargo build failed` and must read as
    # no-test-ran, not as caught.
    "$ROOT/tests/e2e/run-e2e.sh" >"$LOG" 2>&1 && E2E_RC=0 || E2E_RC=$?
    if ! grep -q "^e2e: API at " "$LOG"; then
        echo "mutate: NO TEST RAN — the e2e harness never got a daemon up, so the" >&2
        echo "mutate: mutated source probably did not build. Last lines:" >&2
        tail -15 "$LOG" >&2
        finish 2
    fi
    if [ "$E2E_RC" -ne 0 ] && grep -q "^e2e: FAIL" "$LOG"; then
        echo "mutate: CAUGHT — the e2e harness failed under mutation. The pin is real."
        grep "^e2e: FAIL" "$LOG" | head -3
        finish 0
    fi
    echo "mutate: NOT CAUGHT — e2e passed under mutation. THE PIN IS FALSE." >&2
    finish 1
fi

case "$FILE" in
    scripts/*)
        # FILTER names a scripts-suite test, e.g. `test-service`. Those suites
        # print a summary line ("test-service: 47 passed, 1 failed") and exit
        # non-zero on any failure; the summary is what proves the script RAN rather
        # than dying on line 1, which is the same "did a test run" discipline the
        # other arms apply.
        SUITE="$ROOT/scripts/tests/$FILTER.sh"
        if [ ! -x "$SUITE" ]; then
            echo "mutate: no such scripts suite: $SUITE" >&2
            finish 2
        fi
        "$SUITE" >"$LOG" 2>&1 && SUITE_RC=0 || SUITE_RC=$?
        if ! grep -qE "[0-9]+ passed, [0-9]+ failed" "$LOG"; then
            echo "mutate: NO TEST RAN — $FILTER printed no summary line, so the" >&2
            echo "mutate: mutation may have broken the suite itself. Last lines:" >&2
            tail -15 "$LOG" >&2
            finish 2
        fi
        if [ "$SUITE_RC" -ne 0 ]; then
            echo "mutate: CAUGHT — the test failed under mutation. The pin is real."
            finish 0
        fi
        ;;
    swift/*)
        (cd "$ROOT/swift" && swift test --filter "$FILTER" >"$LOG" 2>&1) || true
        # ORDER MATTERS: "did a test run" before "was there an error", because an
        # XCTest failure line contains the word `error:` (see note 4 above).
        if ! grep -qE "Executed [1-9][0-9]* test" "$LOG"; then
            echo "mutate: NO TEST RAN — the filter \`$FILTER\` matched nothing, or" >&2
            echo "mutate: the mutated source did not build. Last lines:" >&2
            tail -15 "$LOG" >&2
            finish 2
        fi
        if grep -qE "with [1-9][0-9]* failure" "$LOG"; then
            echo "mutate: CAUGHT — the test failed under mutation. The pin is real."
            finish 0
        fi
        ;;
    *)
        (cd "$ROOT/rust" && cargo test "$FILTER" >"$LOG" 2>&1) || true
        # Same ordering discipline: a compile failure means no test ran, which is
        # NOT a caught mutation however red the output looks.
        if ! grep -qE "^test .* \.\.\. (ok|FAILED)" "$LOG"; then
            echo "mutate: NO TEST RAN — the filter \`$FILTER\` matched nothing, or" >&2
            echo "mutate: the mutated source did not build. Last lines:" >&2
            tail -15 "$LOG" >&2
            finish 2
        fi
        if grep -qE "^test result: FAILED" "$LOG"; then
            echo "mutate: CAUGHT — the test failed under mutation. The pin is real."
            finish 0
        fi
        ;;
esac

echo "mutate: NOT CAUGHT — the test passed under mutation. THE PIN IS FALSE." >&2
echo "mutate: ask what OTHER implementation would also pass it, then make the" >&2
echo "mutate: fixture distinguish them. Four such pins have been found here." >&2
finish 1
