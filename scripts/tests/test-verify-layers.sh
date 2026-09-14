#!/usr/bin/env bash
set -u

# verify.sh discovers suites by directory presence so pruned layers need no
# script surgery. This test pins that behavior: a sandbox with only some
# layers must run exactly those suites and report the rest as skipped.

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

WORK="$(mktemp -d /tmp/banshee-verify-layers.XXXXXX)"
trap 'rm -rf "$WORK"' EXIT

mkdir -p "$WORK/scripts"
cp "$SCRIPT_DIR/verify.sh" "$WORK/scripts/"
chmod +x "$WORK/scripts/verify.sh"

# Only the e2e layer exists, with a harness that records it ran.
mkdir -p "$WORK/tests/e2e"
cat > "$WORK/tests/e2e/run-e2e.sh" <<EOF
#!/bin/sh
touch "$WORK/e2e-ran"
exit 0
EOF
chmod +x "$WORK/tests/e2e/run-e2e.sh"

OUT="$("$WORK/scripts/verify.sh" 2>&1)"
STATUS=$?

t "verify exits 0 with one passing layer" [ "$STATUS" -eq 0 ]
t "the present layer actually ran" test -f "$WORK/e2e-ran"
t "absent layers are reported as skipped" \
  bash -c "echo \"\$1\" | grep -q 'skipped (layer not present):.*rust'" _ "$OUT"
t "swift reported skipped" bash -c "echo \"\$1\" | grep -q 'skipped.*swift'" _ "$OUT"

# A failing suite must fail the run and name the suite.
cat > "$WORK/tests/e2e/run-e2e.sh" <<'EOF'
#!/bin/sh
echo "deliberate failure for the layer test"
exit 1
EOF
chmod +x "$WORK/tests/e2e/run-e2e.sh"
OUT="$("$WORK/scripts/verify.sh" 2>&1)"
STATUS=$?
t "verify exits nonzero when a suite fails" [ "$STATUS" -ne 0 ]
t "failure names the suite" bash -c "echo \"\$1\" | grep -q 'FAILED: e2e'" _ "$OUT"
t "failure shows the log tail" \
  bash -c "echo \"\$1\" | grep -q 'deliberate failure'" _ "$OUT"

# --scripts-only must not run other layers even when present.
mkdir -p "$WORK/scripts/tests"
cat > "$WORK/scripts/tests/test-noop.sh" <<'EOF'
#!/bin/sh
exit 0
EOF
chmod +x "$WORK/scripts/tests/test-noop.sh"
rm -f "$WORK/e2e-ran"
"$WORK/scripts/verify.sh" --scripts-only >/dev/null 2>&1
t "--scripts-only skips the e2e layer" bash -c "[ ! -f '$WORK/e2e-ran' ]"

# Lock: a held lock exits 75.
mkdir -p "$WORK/.runtime/verify.lock"
echo 99999999 > "$WORK/.runtime/verify.lock/pid"
# Fake a live holder by using our own PID.
echo $$ > "$WORK/.runtime/verify.lock/pid"
"$WORK/scripts/verify.sh" >/dev/null 2>&1
t "held lock exits 75" [ "$?" -eq 75 ]
rm -rf "$WORK/.runtime/verify.lock"

# Stale lock (dead pid) is reclaimed and the run proceeds.
mkdir -p "$WORK/.runtime/verify.lock"
echo 99999999 > "$WORK/.runtime/verify.lock/pid"
cat > "$WORK/tests/e2e/run-e2e.sh" <<'EOF'
#!/bin/sh
exit 0
EOF
chmod +x "$WORK/tests/e2e/run-e2e.sh"
t "stale lock is reclaimed" "$WORK/scripts/verify.sh"

echo "test-verify-layers: $PASS passed, $FAIL failed"
[ "$FAIL" -eq 0 ]
