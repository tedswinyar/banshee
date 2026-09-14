#!/usr/bin/env bash
# verify.sh — run the repo health checks from one entry point.
#
# Usage:
#   ./scripts/verify.sh [--rust-only|--swift-only|--scripts-only]
#
# Suites are auto-detected by directory presence, so pruned layers (a stamp
# --no-swift etc.) need no script surgery:
#   rust      rust/                 cargo test --workspace
#   swift     swift/                swift test
#   e2e       tests/e2e/            run-e2e.sh parity harness
#   scripts   scripts/tests/        the tests of the gates themselves
#   ope       open-prompt-edition/  an optional prompt-edition layer (absent in
#                                   this project): version guard + conformance
#   website   website/              hugo build (config sanity)
#
# Concurrency: only one verify.sh may run at a time. Two concurrent swift
# builds against the same checkout contend on .build/ and fail confusingly.
# A second invocation exits immediately with code 75 and a clear message.
#
# Logs: each suite tees to .runtime/verify/<suite>.log; the console shows a
# one-line PASS/FAIL per suite and a final summary.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
LOG_DIR="$REPO_ROOT/.runtime/verify"
LOCK_DIR="$REPO_ROOT/.runtime/verify.lock"
LOCK_EXIT_CODE=75

# ---------------------------------------------------------------------------
# Completion guard. On macOS bash 3.2 a set -u abort in a script with an EXIT
# trap exits 0, so this gate could crash and report success while the pre-push
# hook waved the push through. Armed here, before argument parsing, because
# anything above the trap is unprotected. See scripts/lib/completion-guard.sh.
# ---------------------------------------------------------------------------
# Deliberately INLINE rather than sourced from scripts/lib/completion-guard.sh:
# scripts/tests/test-verify-layers.sh copies this script alone into a sandbox, and
# more fundamentally the gate must not depend on another file being present in order
# to report failure correctly. The library carries the same logic for every other
# script in the repo, and scripts/tests/test-completion-guard.sh pins its behaviour.
COMPLETED=0
PREMATURE_EXIT_CODE=70

# Every DELIBERATE exit goes through finish(). A bare `exit` is reported as a
# premature exit, which is the point: crashing into an exit and choosing one are
# then distinguishable. 70 stays distinct from 1 (real suite failure) and 75
# (lock contention).
finish() { COMPLETED=1; exit "$1"; }

# Only release a lock this process owns. The guard is armed before acquire_lock,
# so an early exit (bad option, --help) must not delete another run's lock.
release_lock() {
  if [ -f "$LOCK_DIR/pid" ] && [ "$(cat "$LOCK_DIR/pid" 2>/dev/null)" = "$$" ]; then
    rm -rf "$LOCK_DIR"
  fi
  return 0
}
on_exit() {
  local s=$?
  release_lock
  if [ "$COMPLETED" != "1" ]; then
    echo "verify.sh: exited before finishing (reported status $s) — treating as FAILURE." >&2
    echo "verify.sh: see the completion-guard comment in this script." >&2
    [ "$s" != "0" ] && exit "$s"
    exit "$PREMATURE_EXIT_CODE"
  fi
  exit "$s"
}
trap on_exit EXIT

ONLY=""
case "${1:-}" in
  --rust-only)    ONLY="rust" ;;
  --swift-only)   ONLY="swift" ;;
  --scripts-only) ONLY="scripts" ;;
  -h|--help)
    sed -n '2,20p' "$0" | sed 's/^# \{0,1\}//'
    finish 0
    ;;
  "") ;;
  *) echo "verify.sh: unknown option: $1" >&2; finish 2 ;;
esac

mkdir -p "$LOG_DIR"

# ---------------------------------------------------------------------------
# Lock (exit 75 if another verify.sh is running; reclaim if the holder died)
# ---------------------------------------------------------------------------
acquire_lock() {
  if mkdir "$LOCK_DIR" 2>/dev/null; then
    echo $$ > "$LOCK_DIR/pid"
    return 0
  fi
  local holder
  holder="$(cat "$LOCK_DIR/pid" 2>/dev/null || echo "")"
  if [ -n "$holder" ] && kill -0 "$holder" 2>/dev/null; then
    echo "verify.sh: another verify run (pid $holder) holds the lock; exiting 75." >&2
    echo "verify.sh: do not run cargo/swift test by hand while it runs." >&2
    finish "$LOCK_EXIT_CODE"
  fi
  echo "verify.sh: reclaiming stale lock (holder ${holder:-unknown} is gone)." >&2
  rm -rf "$LOCK_DIR"
  mkdir "$LOCK_DIR" 2>/dev/null || {
    echo "verify.sh: lost the lock race; exiting 75." >&2
    finish "$LOCK_EXIT_CODE"
  }
  echo $$ > "$LOCK_DIR/pid"
}

acquire_lock

# ---------------------------------------------------------------------------
# Suite runner
# ---------------------------------------------------------------------------
PASSED=()
FAILED=()
SKIPPED=()

run_suite() {
  # run_suite <name> <dir-that-must-exist> <command...>
  local name="$1" gate_dir="$2"
  shift 2
  if [ -n "$ONLY" ] && [ "$ONLY" != "$name" ]; then
    return 0
  fi
  if [ ! -d "$REPO_ROOT/$gate_dir" ]; then
    SKIPPED+=("$name")
    return 0
  fi
  local log="$LOG_DIR/$name.log"
  printf '── %-8s ' "$name"
  local start end
  start=$(date +%s)
  if ( cd "$REPO_ROOT" && "$@" ) >"$log" 2>&1; then
    end=$(date +%s)
    echo "PASS  ($((end - start))s, log: ${log#"$REPO_ROOT"/})"
    PASSED+=("$name")
  else
    end=$(date +%s)
    echo "FAIL  ($((end - start))s)"
    echo "   ↳ tail of ${log#"$REPO_ROOT"/}:"
    tail -n 15 "$log" | sed 's/^/   │ /'
    FAILED+=("$name")
  fi
}

run_rust() {
  # fmt and clippy are BLOCKING, and they are here because the template shipped
  # without them: the stamped workspace already carried rustfmt debt in
  # banshee-api/src/config.rs that no gate had ever looked at, and the first
  # clippy run on new code failed on -D warnings. A lint nobody runs is a lint
  # that only ever fires on whoever happens to look. Same posture as
  # another project, where both are blocking phases. (Filed upstream as a
  # template backport.)
  #
  # --keep-going is deliberate: without it cargo stops at the first failing
  # crate, and the same debt reads as far smaller than it is.
  echo "── rust suite: cargo fmt --all --check"
  (cd rust && cargo fmt --all --check) || return 1
  echo "── rust suite: cargo test --workspace"
  (cd rust && cargo test --workspace) || return 1
  echo "── rust suite: cargo clippy -D warnings"
  (cd rust && cargo clippy --workspace --all-targets --keep-going -- -D warnings) || return 1
  # Advisory + license gate. There is no cloud CI: if this does not run
  # here, it runs nowhere. Missing tool = loud warning, not a brick.
  if command -v cargo-deny >/dev/null; then
    (cd rust && cargo deny check advisories licenses sources)
  else
    echo "WARNING: cargo-deny not installed — advisory/license gate SKIPPED (brew install cargo-deny)" >&2
  fi
}
run_swift()   { (cd swift && swift test); }
run_e2e()     { tests/e2e/run-e2e.sh; }
run_scripts() {
  local rc=0 t
  for t in scripts/tests/test-*.sh; do
    [ -e "$t" ] || continue
    echo "── scripts suite: $t"
    "$t" || rc=1
  done
  # Secret scan over the working tree. There is no cloud CI: if it does not
  # run here it runs nowhere, and .gitleaks.toml would be dead config.
  # Present-or-warn (same posture as cargo-deny) so a fresh clone without the
  # tool still builds — install it and the gate becomes real.
  if [ -f .gitleaks.toml ]; then
    if command -v gitleaks >/dev/null; then
      echo "── scripts suite: gitleaks secret scan"
      gitleaks detect --no-banner --config .gitleaks.toml || rc=1
    else
      echo "WARNING: gitleaks not installed — secret scan SKIPPED (brew install gitleaks)" >&2
    fi
  fi
  return "$rc"
}
# The optional prompt-edition layer (absent in this project) has a PUSH-time
# version-bump guard that runs inside the pre-push hook with real SHAs; here we
# run only its conformance harness, when the directory exists.
run_ope() {
  if [ -x open-prompt-edition/kit/09-conformance/run.sh ]; then
    open-prompt-edition/kit/09-conformance/run.sh
  fi
}
run_website() { (cd website && hugo build --quiet --destination "$REPO_ROOT/.runtime/verify/hugo-out"); }

echo "verify.sh — $(date '+%Y-%m-%d %H:%M:%S')  (logs: .runtime/verify/)"
run_suite rust    rust                 run_rust
run_suite swift   swift                run_swift
run_suite e2e     tests/e2e            run_e2e
run_suite scripts scripts/tests        run_scripts
run_suite ope     open-prompt-edition  run_ope
run_suite website website              run_website

# ---------------------------------------------------------------------------
# Summary
# ---------------------------------------------------------------------------
echo
if [ "${#SKIPPED[@]}" -gt 0 ]; then
  echo "skipped (layer not present): ${SKIPPED[*]}"
fi
if [ "${#FAILED[@]}" -gt 0 ]; then
  echo "FAILED: ${FAILED[*]}"
  finish 1
fi
echo "all suites passed: ${PASSED[*]:-none}"
finish 0
