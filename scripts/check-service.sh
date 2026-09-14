#!/usr/bin/env bash
set -uo pipefail

# check-service.sh — tell the truth about the daemon's MACHINE state.
#
# Deliberately NOT part of `make verify` (ADR-0004): verify asserts repo state, and
# whether a LaunchAgent is registered on this laptop is a different question with a
# different answer on every machine. Run it when the app says the daemon is
# missing, after an install, or when `banshee status` cannot reach anything.
#
# Exit codes:
#   0  the service is installed, running, and answering
#   1  something is wrong (every check prints what and how to fix it)
#   2  not installed at all
#
# Overridable for tests: BANSHEE_LAUNCHD_DIR, BANSHEE_INSTALL_DIR,
# BANSHEE_LOG_DIR, BANSHEE_LABEL, BANSHEE_DAEMON_PORT, BANSHEE_API_SOCKET
# (scripts/lib/service.sh).
# `--no-launchctl` skips the launchd queries, so the scripts suite can check the
# plist-reading logic without a registered agent.

ROOT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
# shellcheck source=lib/service.sh
. "$ROOT_DIR/scripts/lib/service.sh"

USE_LAUNCHCTL=1
[ "${1:-}" = "--no-launchctl" ] && USE_LAUNCHCTL=0
case "${1:-}" in
    -h|--help) sed -n '3,20p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
esac

PROBLEMS=0
ok()   { echo "  ✓ $*"; }
bad()  { echo "  ✗ $*" >&2; PROBLEMS=$((PROBLEMS + 1)); }
hint() { echo "      → $*" >&2; }

echo "banshee service: $BANSHEE_LABEL"

# ---------------------------------------------------------------------------
# 1. Is it installed at all?
# ---------------------------------------------------------------------------
if [ ! -f "$BANSHEE_PLIST" ]; then
    echo "  ✗ not installed: no plist at $BANSHEE_PLIST" >&2
    hint "./scripts/install-launchd.sh   (or: make daemon-install)"
    exit 2
fi
ok "plist present: $BANSHEE_PLIST"

if ! plutil -lint "$BANSHEE_PLIST" >/dev/null 2>&1; then
    bad "the plist is not valid property-list data"
    hint "reinstall: make daemon-install"
fi

# ---------------------------------------------------------------------------
# 2. THE check: does ProgramArguments[0] point into a build directory?
# ---------------------------------------------------------------------------
# This is the one that earned this script. Another project's plist named
# a rust/target/release binary directly; a benchmark run recreated
# target/release WITHOUT that binary, and the running process served from its
# DELETED INODE for eight days before a reboot exposed it. launchd then reported
# exit 78 — EX_CONFIG, "cannot exec the program", NOT "malformed plist" — and two
# more days went into that misreading.
#
# PlistBuddy, not grep: launchd REWRITES plists in binary form once loaded, so a
# grep that worked on the freshly written XML silently finds nothing later, and
# "nothing" reads as "the key is absent".
PROGRAM="$(plist_value ":ProgramArguments:0")"
if [ -z "$PROGRAM" ]; then
    bad "ProgramArguments[0] is empty or unreadable"
    hint "reinstall: make daemon-install"
else
    if program_path_is_installed "$PROGRAM"; then
        ok "program is an installed path, not a build directory"
    else
        bad "the plist points into a BUILD DIRECTORY: $PROGRAM"
        hint "a build directory is not an installation. \`cargo clean\` or any"
        hint "rebuild that misses this binary leaves the daemon serving from a"
        hint "deleted inode until the next reboot, and launchd then reports exit"
        hint "78 (EX_CONFIG = cannot exec), which reads like a plist problem."
        hint "Fix: make daemon-install"
    fi

    if [ -x "$PROGRAM" ]; then
        ok "program exists and is executable: $PROGRAM"
    else
        bad "the program named in the plist does not exist: $PROGRAM"
        hint "this is the exit-78 shape. make daemon-install"
    fi
fi

# ---------------------------------------------------------------------------
# 3. Environment the plist pins
# ---------------------------------------------------------------------------
PLIST_PROFILE="$(plist_value ":EnvironmentVariables:BANSHEE_PROFILE")"
PLIST_PORT="$(plist_value ":EnvironmentVariables:BANSHEE_PORT")"
[ "$PLIST_PROFILE" = "prod" ] \
    && ok "profile pinned to prod" \
    || bad "BANSHEE_PROFILE is ${PLIST_PROFILE:-unset}, want prod"
# The prod profile does NOT climb the port ladder, so an unpinned port would make
# the daemon's port depend on what else happened to be bound at login.
[ -n "$PLIST_PORT" ] \
    && ok "port pinned to $PLIST_PORT" \
    || bad "BANSHEE_PORT is not pinned in the plist"

PROCESS_TYPE="$(plist_value ":ProcessType")"
if [ "$PROCESS_TYPE" = "Standard" ]; then
    ok "ProcessType Standard"
else
    bad "ProcessType is ${PROCESS_TYPE:-unset}, want Standard"
    hint "Background + LowPriorityIO throttles I/O hard enough that a ps-walking"
    hint "sampler misses its own schedule — measured at >20min on a similar job."
fi

# ---------------------------------------------------------------------------
# 4. launchd's own view
# ---------------------------------------------------------------------------
if [ "$USE_LAUNCHCTL" = "1" ]; then
    TARGET="$(banshee_service_target)"
    if PRINT="$(launchctl print "$TARGET" 2>&1)"; then
        ok "registered with launchd ($TARGET)"
        PID="$(printf '%s' "$PRINT" | awk '/^\tpid = /{print $3; exit}')"
        if [ -n "${PID:-}" ]; then
            ok "running as pid $PID"
        else
            bad "registered but NOT running"
            LAST_EXIT="$(printf '%s' "$PRINT" | awk '/last exit code = /{print $NF; exit}')"
            [ -n "${LAST_EXIT:-}" ] && hint "last exit code: $LAST_EXIT"
            # BOTH spellings. Current macOS prints the SYMBOL (`EX_CONFIG`);
            # older versions print the NUMBER (`78`). Verified against a live agent
            # on 2026-08-31 (macOS 15): the numeric branch alone was dead code, and
            # this hint is the whole reason the check exists — two days went into
            # misreading 78 as "malformed plist" on another project.
            case "${LAST_EXIT:-}" in
                78|EX_CONFIG)
                    hint "EX_CONFIG (78) means launchd could not EXEC the program —"
                    hint "NOT that this plist is malformed. Check that"
                    hint "ProgramArguments[0] above actually exists."
                    ;;
            esac
            hint "logs: tail -20 $BANSHEE_STDERR_LOG"
        fi
    else
        bad "not registered with launchd"
        hint "./scripts/install-launchd.sh"
    fi
else
    echo "  · launchd queries skipped (--no-launchctl)"
fi

# ---------------------------------------------------------------------------
# 5. Is it actually serving? The only checks that prove the thing works.
#
# Two transports (ADR-0008). TCP carries ONLY the open /health (since Phase 3 the
# daemon refuses every credential there), so it answers "is a process there" and
# nothing else — the line below is informational. The SOCKET is what every client
# authenticates over, so it is the one that has to work — a daemon whose socket
# failed to bind, or bound group-readable and was refused, would otherwise look
# healthy here while every client sat locked out. The key therefore travels over
# the socket ONLY, including in this script.
# ---------------------------------------------------------------------------
BASE="http://127.0.0.1:${PLIST_PORT:-$BANSHEE_DAEMON_PORT}"
if HEALTH="$(curl -sf "$BASE/health" 2>/dev/null)"; then
    ok "answering on TCP (/health only; keyed reads are refused there): $HEALTH"
else
    bad "nothing answering on $BASE/health"
    hint "logs: tail -20 $BANSHEE_STDOUT_LOG $BANSHEE_STDERR_LOG"
fi

if [ -S "$BANSHEE_API_SOCKET" ]; then
    # 0600 is the security boundary: bind() creates the socket 0755 and the daemon
    # tightens it afterwards, then refuses to serve if that failed. Verify it from
    # the outside too — a 0755 socket that the daemon nonetheless serves is exactly
    # the port-squatting hole with a different spelling.
    SOCK_MODE="$(stat -f '%Lp' "$BANSHEE_API_SOCKET" 2>/dev/null || echo '???')"
    if [ "$SOCK_MODE" = "600" ]; then
        ok "socket is owner-only (0600): $BANSHEE_API_SOCKET"
    else
        bad "socket mode is $SOCK_MODE, want 600: $BANSHEE_API_SOCKET"
        hint "any local user could connect to it; the daemon should have refused to"
        hint "serve it (see bind_socket in rust/banshee-api/src/main.rs)"
    fi
    if SOCK_HEALTH="$(curl -sf --unix-socket "$BANSHEE_API_SOCKET" http://localhost/health 2>/dev/null)"; then
        ok "answering on the socket: $SOCK_HEALTH"
        # A registered, running, answering daemon that is not SAMPLING is the silent
        # failure this whole app exists to avoid, so ask it — over the socket, which
        # is the only transport the key may travel on.
        # Feed the key to curl via --config on stdin, never as an argv -H value: a
        # process argument is visible to any local user through `ps`.
        KEY_HDR="header = \"x-api-key: $(cat "${BANSHEE_KEY_FILE:-$BANSHEE_DATA_DIR/api_key}" 2>/dev/null || echo)\""
        if STATS="$(printf '%s\n' "$KEY_HDR" | curl -sf --config - --unix-socket "$BANSHEE_API_SOCKET" http://localhost/stats 2>/dev/null)"; then
            SAMPLES="$(printf '%s' "$STATS" | sed -n 's/.*"samples":\([0-9]*\).*/\1/p')"
            if [ "${SAMPLES:-0}" -gt 0 ] 2>/dev/null; then
                ok "sampling: $SAMPLES samples stored"
            else
                bad "answering but has stored NO samples"
                hint "a monitor that serves requests without sampling looks healthy"
                hint "and knows nothing. Check $BANSHEE_STDERR_LOG for collect errors."
            fi
        else
            bad "the socket answers /health but refused the keyed /stats"
            hint "the key file and the daemon's key disagree; a rotation is logged in"
            hint "$BANSHEE_STDERR_LOG, and every client re-reads the file on its own"
        fi
    else
        bad "socket present but nothing answering on it: $BANSHEE_API_SOCKET"
        hint "a stale socket from a daemon that crashed — the next start removes it:"
        hint "launchctl kickstart -k $(banshee_service_target)"
    fi
else
    bad "no socket at $BANSHEE_API_SOCKET"
    hint "every client authenticates over this socket (ADR-0008); a daemon without"
    hint "one predates the transport and every client is locked out. Update it:"
    hint "./scripts/install-launchd.sh"
fi

echo
if [ "$PROBLEMS" -eq 0 ]; then
    echo "banshee service: OK"
    exit 0
fi
echo "banshee service: $PROBLEMS problem(s)" >&2
exit 1
