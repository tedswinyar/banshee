#!/usr/bin/env bash
set -u

# Tests OF the mutation manifest and its replay tool (banshee-jds).
#
# Two things are under test: (1) every checked-in .mutations/*.mut is
# well-formed, so a rotted or half-written entry is caught at gate time rather
# than only when someone runs the slow `make mutations`; and (2) replay's own
# aggregation logic — does it FAIL when an entry fails, PASS when all pass, and
# refuse an empty manifest. (2) runs against a STUB mutate (BANSHEE_MUTATE) so it
# is instant and deterministic: replaying the real manifest compiles code and is
# minutes long, which belongs in `make mutations`, not the pre-push gate.

SCRIPT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
REPLAY="$ROOT/scripts/replay-mutations.sh"
PASS=0
FAIL=0

t() {
  local desc="$1"; shift
  if "$@" >/dev/null 2>&1; then PASS=$((PASS + 1)); else
    FAIL=$((FAIL + 1)); echo "  ✗ $desc" >&2; fi
}
t_fails() {
  local desc="$1"; shift
  if "$@" >/dev/null 2>&1; then
    FAIL=$((FAIL + 1)); echo "  ✗ $desc (succeeded and should not have)" >&2
  else PASS=$((PASS + 1)); fi
}

WORK="$(mktemp -d /tmp/banshee-mut-test.XXXXXX)"
trap 'rm -rf "$WORK"' EXIT

# ---------------------------------------------------------------------------
# 1. Every real .mut is well-formed: file:, test:, and an ---OLD---/---NEW---
#    pair. A malformed entry would be reported by replay as a failure, but that
#    only surfaces on the slow run; catch it cheaply here.
# ---------------------------------------------------------------------------
real_muts=0
for m in "$ROOT"/.mutations/*.mut; do
  [ -e "$m" ] || continue
  real_muts=$((real_muts + 1))
  name="$(basename "$m")"
  t "$name has a file:" bash -c "sed -n 's/^file: *//p' '$m' | grep -q ."
  t "$name has a test:" bash -c "sed -n 's/^test: *//p' '$m' | grep -q ."
  t "$name has an ---OLD--- marker" grep -qx -- '---OLD---' "$m"
  t "$name has a ---NEW--- marker" grep -qx -- '---NEW---' "$m"
  # The referenced file must exist, or the mutation cannot apply — a rotted path
  # is exactly the silent decay this manifest exists to prevent.
  f="$(sed -n 's/^file: *//p' "$m" | head -1)"
  t "$name points at an existing file ($f)" test -f "$ROOT/$f"
done
t "the manifest is non-empty" test "$real_muts" -gt 0

# ---------------------------------------------------------------------------
# 2. replay's aggregation, driven by a STUB mutate (no compile).
# ---------------------------------------------------------------------------
# A stub that exits with the code named in the .mut's `test:` field, so a
# synthetic manifest can script "this entry is caught (0)" vs "not caught (1)".
STUB="$WORK/stub-mutate.sh"
cat > "$STUB" <<'STUB_EOF'
#!/usr/bin/env sh
# args: <file> <filter>; the filter IS the exit code to return.
exit "$2"
STUB_EOF
chmod +x "$STUB"

mk_mut() { # dir, slug, exitcode
  mkdir -p "$1"
  cat > "$1/$2.mut" <<MUT_EOF
# synthetic
file: some/real/path.rs
test: $3
---OLD---
x
---NEW---
y
MUT_EOF
}

# All entries "caught" (stub exits 0) → replay passes.
ALLPASS="$WORK/allpass"; mk_mut "$ALLPASS" a 0; mk_mut "$ALLPASS" b 0
t "replay passes when every entry is caught" \
  env BANSHEE_MUTATE="$STUB" "$REPLAY" "$ALLPASS"

# One entry "not caught" (stub exits 1) → replay fails.
ONEFAIL="$WORK/onefail"; mk_mut "$ONEFAIL" a 0; mk_mut "$ONEFAIL" b 1
t_fails "replay fails when any entry is not caught" \
  env BANSHEE_MUTATE="$STUB" "$REPLAY" "$ONEFAIL"

# The summary line names the real tally, so a caught/uncaught mix is legible and
# mutation-testable like the other suites.
OUT="$(BANSHEE_MUTATE="$STUB" "$REPLAY" "$ONEFAIL" 2>&1)"
t "replay reports the pass/fail tally" \
  bash -c "echo \"\$1\" | grep -q 'replay-mutations: 1 passed, 1 failed'" _ "$OUT"

# A malformed entry (no test:) is a failure, not a skip.
MALFORMED="$WORK/malformed"; mkdir -p "$MALFORMED"
printf 'file: some/path.rs\n---OLD---\nx\n---NEW---\ny\n' > "$MALFORMED/bad.mut"
t_fails "replay fails on a malformed entry" \
  env BANSHEE_MUTATE="$STUB" "$REPLAY" "$MALFORMED"

# An empty manifest is a misconfiguration, not a clean pass.
EMPTY="$WORK/empty"; mkdir -p "$EMPTY"
t_fails "replay refuses an empty manifest" \
  env BANSHEE_MUTATE="$STUB" "$REPLAY" "$EMPTY"

echo "test-mutations-manifest: $PASS passed, $FAIL failed"
[ "$FAIL" -eq 0 ]
