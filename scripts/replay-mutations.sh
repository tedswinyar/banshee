#!/usr/bin/env bash
set -u

# replay-mutations.sh — re-verify the checked-in mutation manifest (.mutations/).
#
# The durability half of the Testing standard (banshee-jds): a pin recorded only
# in a commit message rots silently when a refactor moves the target text and
# nobody re-runs it. Each .mutations/*.mut names the exact production text, the
# mutation, and the catching test; this replays every one through mutate.sh and
# FAILS if any mutation no longer APPLIES (text moved → mutate.sh exit 3) or is
# no longer CAUGHT (pin rotted → the test passed under mutation → mutate.sh exit
# 1). mutate.sh is the vetted primitive; this is only the loop over it.
#
# NOT a pre-push gate: every entry compiles and runs a test, so a full replay is
# minutes. Run it before a release (docs/release-checklist.md) and quarterly.
#
# Usage:
#   scripts/replay-mutations.sh [manifest_dir]   (default: .mutations/)
#
# Exit 0 iff every entry applied AND was caught. Prints a scripts-suite-style
# "N passed, M failed" summary so it reads like the rest of the gates.

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
MANIFEST_DIR="${1:-$ROOT/.mutations}"
# BANSHEE_MUTATE overrides the primitive, so the self-test can drive replay's
# pass/fail aggregation with a stub instead of a real multi-minute compile — the
# same env-override-for-testability pattern the install scripts use. A checker
# needs its own self-test (mutate.sh's header learned this the hard way).
MUTATE="${BANSHEE_MUTATE:-$ROOT/scripts/mutate.sh}"

[ -x "$MUTATE" ] || { echo "replay-mutations: missing $MUTATE" >&2; exit 2; }

pass=0
fail=0

shopt -s nullglob
muts=("$MANIFEST_DIR"/*.mut)
if [ "${#muts[@]}" -eq 0 ]; then
  # An empty manifest is a misconfiguration, not a clean pass: the whole point
  # is that mutations ARE recorded. Fail loudly rather than report "all green"
  # over nothing.
  echo "replay-mutations: no .mut files in $MANIFEST_DIR" >&2
  echo "replay-mutations: 0 passed, 1 failed"
  exit 1
fi

for m in "${muts[@]}"; do
  name="$(basename "$m")"
  file="$(sed -n 's/^file: *//p' "$m" | head -1)"
  test="$(sed -n 's/^test: *//p' "$m" | head -1)"
  # Everything from the ---OLD--- line onward is exactly mutate.sh's stdin spec.
  spec="$(sed -n '/^---OLD---$/,$p' "$m")"

  if [ -z "$file" ] || [ -z "$test" ] || [ -z "$spec" ]; then
    echo "  ✗ $name: malformed — needs a file:, a test:, and an ---OLD---/---NEW--- block" >&2
    fail=$((fail + 1))
    continue
  fi

  log="$(mktemp)"
  if printf '%s\n' "$spec" | "$MUTATE" "$file" "$test" >"$log" 2>&1; then
    pass=$((pass + 1))
  else
    rc=$?
    fail=$((fail + 1))
    echo "  ✗ $name [$file :: $test] — mutate.sh exit $rc" >&2
    # mutate.sh's last line says which failure it was (exit 1 not caught, 2 no
    # test ran / build broke, 3 text moved); surface it.
    tail -3 "$log" | sed 's/^/      /' >&2
  fi
  rm -f "$log"
done

echo "replay-mutations: $pass passed, $fail failed"
[ "$fail" -eq 0 ]
