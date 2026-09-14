#!/usr/bin/env bash
set -euo pipefail

# restore-drill.sh — the release restore drill (banshee-d8r).
#
# "A copy that has never been opened somewhere else is a hope, not a backup"
# (docs/data-safety.md). This takes a verified snapshot via `banshee backup`
# (whose backup_verified already re-opens the copy out-of-place) and then
# INDEPENDENTLY re-reads it here — schema version and full row count — so the
# release has proof, at the moment of shipping, that the database is restorable.
# It is a real gate in release.sh, not a checkbox: a ticked box in a committed
# file stays ticked and rots; running the drill cannot.
#
# Which `banshee` runs the backup, in order (banshee-xt5):
#   1. BANSHEE_CLI — an explicit override, so the scripts suite can drive this
#      with a stub instead of a live daemon (the same env-override-for-testability
#      pattern the install scripts use);
#   2. the INSTALLED CLI, the one `make daemon-install` put beside the running
#      daemon (`scripts/lib/service.sh` owns that path; nothing here spells it);
#   3. a bare `banshee` on PATH, last.
# The installed copy comes before PATH because the first real release run died
# with `banshee: command not found` (cutting 0.1.3): the install dir is on PATH in
# an interactive shell and not in the shell release.sh was run from, and the copy
# that matches the running daemon is the installed one either way.
#
# `--self-contained` needs none of the above: it boots its own throwaway daemon
# from the freshly built binaries (see below). That is the build-server mode.

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
# shellcheck source=lib/service.sh
. "$SCRIPT_DIR/lib/service.sh"

info() { printf 'restore-drill: %s\n' "$*"; }

# ---------------------------------------------------------------------------
# Self-contained mode (build server): the drill proves the ARTIFACT, not the
# operator's machine. Boots a throwaway test-profile daemon from the binaries
# just built, lets it sample until it has a real series, takes the backup with
# the matching CLI, and verifies the copy exactly as below. No installed
# daemon, no prod data, nothing left behind.
#
#   restore-drill.sh --self-contained            (or BANSHEE_DRILL_SELF_CONTAINED=1)
#   BANSHEE_DRILL_BIN=rust/target/debug          which build to drill (default: release)
#
# release.sh runs this mode on the build server and the installed-daemon mode
# on an operator's machine; both end in the same verification, so a release
# cut either way carries the same proof.
# ---------------------------------------------------------------------------
SELF_CONTAINED="${BANSHEE_DRILL_SELF_CONTAINED:-}"
[ "${1:-}" = "--self-contained" ] && SELF_CONTAINED=1

API_PID=""
WORK=""
drill_cleanup() {
  if [ -n "${API_PID:-}" ]; then
    kill "$API_PID" 2>/dev/null || true
    wait "$API_PID" 2>/dev/null || true
  fi
  [ -n "${WORK:-}" ] && rm -rf "$WORK"
}
# On macOS bash 3.2 an EXIT trap turns a set -u abort into exit 0, so this script
# could crash and report success. See scripts/lib/completion-guard.sh.
# shellcheck source=lib/completion-guard.sh
. "$SCRIPT_DIR/lib/completion-guard.sh"
arm_completion_guard "restore-drill" drill_cleanup
die() { printf 'restore-drill: ERROR: %s\n' "$*" >&2; finish 1; }

if [ -n "$SELF_CONTAINED" ]; then
  ROOT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
  BIN="${BANSHEE_DRILL_BIN:-$ROOT_DIR/rust/target/release}"
  case "$BIN" in /*) ;; *) BIN="$ROOT_DIR/$BIN" ;; esac
  if [ ! -x "$BIN/banshee-api" ] || [ ! -x "$BIN/banshee" ]; then
    case "$BIN" in
      "$ROOT_DIR/rust/target/release") info "building release binaries for the drill"
        (cd "$ROOT_DIR/rust" && cargo build --release --quiet -p banshee-api -p banshee-cli) \
          || die "cargo build --release failed" ;;
      "$ROOT_DIR/rust/target/debug") info "building debug binaries for the drill"
        (cd "$ROOT_DIR/rust" && cargo build --quiet -p banshee-api -p banshee-cli) \
          || die "cargo build failed" ;;
      *) die "no banshee-api + banshee in $BIN (BANSHEE_DRILL_BIN)" ;;
    esac
  fi
  WORK="$(mktemp -d /tmp/banshee-drill.XXXXXX)"
  # The test profile REFUSES a DB under the prod data dir, so a mis-set env var
  # cannot touch real data; everything lives in $WORK. A fast cadence so the
  # series is real in seconds; census/sweep effectively off.
  export BANSHEE_PROFILE=test
  export BANSHEE_DB_PATH="$WORK/banshee.db"
  export BANSHEE_PORT=0
  export BANSHEE_KEY_FILE="$WORK/api_key"
  export BANSHEE_API_SOCKET="$WORK/api.sock"
  export BANSHEE_SAMPLE_INTERVAL_MS=50
  export BANSHEE_CENSUS_INTERVAL_MS=600000
  export BANSHEE_SWEEP_INTERVAL_MS=600000
  unset BANSHEE_API_URL
  "$BIN/banshee-api" >"$WORK/api.out" 2>"$WORK/api.err" &
  API_PID=$!
  # The configured port is a request; only the announcement is the truth. Wait for
  # the socket line.
  for _ in $(seq 1 100); do
    grep -q 'listening on unix://' "$WORK/api.out" 2>/dev/null && break
    kill -0 "$API_PID" 2>/dev/null || die "banshee-api exited during startup:
$(cat "$WORK/api.err")"
    sleep 0.1
  done
  grep -q 'listening on unix://' "$WORK/api.out" || die "banshee-api never announced its socket:
$(cat "$WORK/api.err")"
  # "Enough to judge" is the model's 40-sample window; a backup of a two-row DB
  # proves little. Wait for a real series (a few seconds at 50 ms).
  WANT=40
  have=0
  for _ in $(seq 1 300); do
    have="$("$BIN/banshee" stats --json 2>/dev/null | python3 -c 'import json,sys; print(json.load(sys.stdin)["samples"])' 2>/dev/null || echo 0)"
    [ "${have:-0}" -ge "$WANT" ] && break
    sleep 0.1
  done
  [ "${have:-0}" -ge "$WANT" ] || die "the throwaway daemon only reached $have samples (wanted $WANT):
$(tail -5 "$WORK/api.err")"
  CLI="$BIN/banshee"
  info "self-contained: throwaway daemon at $have samples; taking the backup with $CLI"
else

if [ -n "${BANSHEE_CLI:-}" ]; then
  CLI="$BANSHEE_CLI"
elif [ -x "$BANSHEE_INSTALLED_CLI" ]; then
  CLI="$BANSHEE_INSTALLED_CLI"
elif command -v banshee >/dev/null 2>&1; then
  CLI="banshee"
else
  die "no banshee CLI: nothing installed at $BANSHEE_INSTALLED_CLI (run \`make daemon-install\`) and none on PATH; BANSHEE_CLI overrides, --self-contained needs neither"
fi
fi

# Take the verified backup. A failure here is usually "the daemon is not
# running" — which on a release machine is a real blocker, not something to skip.
out="$("$CLI" backup --json)" || die "\`$CLI backup\` failed — is the daemon running?"

# Parse {"path": "...", "samples": N} without assuming jq is installed (python3
# is already a scripts-layer dependency; see mutate.sh).
path="$(printf '%s' "$out" | python3 -c 'import json,sys; print(json.load(sys.stdin)["path"])')" \
  || die "backup did not return a JSON path: $out"
reported="$(printf '%s' "$out" | python3 -c 'import json,sys; print(json.load(sys.stdin)["samples"])')" \
  || die "backup did not return a sample count: $out"

[ -f "$path" ] || die "backup reported $path but no file is there"

# The RESTORE half: open the snapshot out-of-place and read it. A backup you
# cannot open and count is not a backup.
version="$(sqlite3 "$path" 'PRAGMA user_version;')" \
  || die "the snapshot at $path will not open in sqlite3"
[ "${version:-0}" -ge 1 ] || die "the snapshot has no schema version (user_version=$version)"

rows="$(sqlite3 "$path" 'SELECT count(*) FROM samples;')" \
  || die "the snapshot has no samples table — a truncated or wrong-schema copy"
[ "$rows" = "$reported" ] \
  || die "restored row count $rows != the $reported the backup reported — the copy is not trustworthy"

[ "$rows" -ge 1 ] || die "the snapshot has no rows — a backup of nothing proves nothing"
info "passed: $path re-opened at schema v$version with $rows samples"
finish 0
