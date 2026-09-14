#!/usr/bin/env bash
set -u

# Tests OF the perf-scan shim (scripts/perf-scan-shim.sh) — the retirement
# contract for perf-scan (banshee-10w).
#
# Hermetic: BANSHEE_BIN points at a stub that records every invocation and
# nothing real is reaped. The properties under test are the SURFACE mapping
# (which banshee subcommands run, in what order, with --execute or not) and
# the refusals — a shim that silently dry-runs where the original killed, or
# that reaps under a different threshold than the caller asked for, is worse
# than no shim.

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

WORK="$(mktemp -d /tmp/banshee-shim-test.XXXXXX)"
trap 'rm -rf "$WORK"' EXIT

SHIM="$SCRIPT_DIR/perf-scan-shim.sh"
CALLS="$WORK/calls.log"

# The stub records each invocation, one line per call, and exits with
# BANSHEE_STUB_EXIT (default 0) so the failure paths are reachable.
STUB="$WORK/banshee-stub"
cat > "$STUB" <<STUBEOF
#!/usr/bin/env bash
echo "\$*" >> "$CALLS"
exit "\${BANSHEE_STUB_EXIT:-0}"
STUBEOF
chmod +x "$STUB"
export BANSHEE_BIN="$STUB"

run_shim() {
  : > "$CALLS"
  "$SHIM" "$@"
}

# ---------------------------------------------------------------------------
# 1. The read-only surface
# ---------------------------------------------------------------------------
t "default invocation succeeds" run_shim
t "default runs status then census, nothing else" \
  bash -c "[ \"\$(cat '$CALLS')\" = \$'status\ncensus' ]"
t "default never passes --execute" \
  bash -c "! grep -q -- '--execute' '$CALLS'"

run_shim --quiet >/dev/null 2>&1
t "--quiet runs status only" bash -c "[ \"\$(cat '$CALLS')\" = 'status' ]"

# ---------------------------------------------------------------------------
# 2. The destructive surface — the flag IS the consent, exactly as before
# ---------------------------------------------------------------------------
run_shim --reap-orphans >/dev/null 2>&1
t "--reap-orphans executes (not a silent dry-run)" \
  bash -c "grep -q '^reap-orphans --execute\$' '$CALLS'"
t "--reap-orphans still prints the survey first" \
  bash -c "head -1 '$CALLS' | grep -q '^status\$'"

run_shim --reap-tmux >/dev/null 2>&1
t "--reap-tmux executes reap-stale-sessions" \
  bash -c "grep -q '^reap-stale-sessions --execute\$' '$CALLS'"

# Killing a session orphans its MCP servers, so sessions must be reaped
# BEFORE the orphan sweep — the original did it the other way around and then
# told you to run it again.
run_shim --reap-orphans --reap-tmux >/dev/null 2>&1
t "both reaps run sessions before orphans" bash -c "
  grep -n '^reap-stale-sessions --execute\$' '$CALLS' | cut -d: -f1 > '$WORK/s'
  grep -n '^reap-orphans --execute\$' '$CALLS' | cut -d: -f1 > '$WORK/o'
  [ -s '$WORK/s' ] && [ -s '$WORK/o' ] && [ \"\$(cat '$WORK/s')\" -lt \"\$(cat '$WORK/o')\" ]"

# ---------------------------------------------------------------------------
# 3. The refusals
# ---------------------------------------------------------------------------
# A DAYS argument meant "reap at THIS threshold". The daemon reaps at ITS
# configured threshold, which can be lower — i.e. more destructive than the
# caller asked for. Refuse; never reinterpret consent.
ERR="$("$SHIM" --reap-tmux 5 2>&1 >/dev/null)"
STATUS=$?
t "--reap-tmux DAYS is refused with a usage error" [ "$STATUS" -eq 2 ]
t "the refusal names stale_days" \
  bash -c "echo \"\$1\" | grep -q 'stale_days'" _ "$ERR"
# The exit must happen AT the refusal. An implementation that merely warns and
# keeps parsing also exits 2 — but via the unknown-option branch choking on the
# number, which buries the real explanation under "unknown option: 5".
t "the refusal is deliberate, not an unknown-option accident" \
  bash -c "! echo \"\$1\" | grep -q 'unknown option'" _ "$ERR"
: > "$CALLS"
"$SHIM" --reap-tmux 5 >/dev/null 2>&1 || true
t "the refusal reaps nothing" bash -c "! grep -q -- '--execute' '$CALLS'"

"$SHIM" --frobnicate >/dev/null 2>&1
t "an unknown option exits 2, as the original did" [ $? -eq 2 ]

# ---------------------------------------------------------------------------
# 4. Help and the missing-CLI path
# ---------------------------------------------------------------------------
HELP="$("$SHIM" --help 2>&1)"
t "--help exits 0" bash -c "'$SHIM' --help"
t "--help says it is a shim over banshee" \
  bash -c "echo \"\$1\" | grep -qi 'shim' && echo \"\$1\" | grep -q 'banshee'" _ "$HELP"

# No override, an empty PATH slice, and a HOME with no installed CLI: the shim
# must say what happened and where the real thing lives, not stack-trace.
ERR="$(env -u BANSHEE_BIN HOME="$WORK" PATH="/usr/bin:/bin" "$SHIM" 2>&1 >/dev/null)"
STATUS=$?
t "missing CLI exits 4 (cannot reach banshee)" [ "$STATUS" -eq 4 ]
t "missing CLI names daemon-install" \
  bash -c "echo \"\$1\" | grep -q 'daemon-install'" _ "$ERR"

# ---------------------------------------------------------------------------
# 5. Failure propagation — a dead daemon must not read as a healthy survey
# ---------------------------------------------------------------------------
: > "$CALLS"
BANSHEE_STUB_EXIT=4 "$SHIM" >/dev/null 2>&1
STATUS=$?
t "a failing status propagates its exit code" [ "$STATUS" -eq 4 ]
t "a failing status stops before census" \
  bash -c "[ \"\$(cat '$CALLS')\" = 'status' ]"

# ---------------------------------------------------------------------------
echo "test-perf-scan-shim: $PASS passed, $FAIL failed"
[ "$FAIL" -eq 0 ]
