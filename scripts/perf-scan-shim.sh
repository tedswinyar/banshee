#!/usr/bin/env bash
# perf-scan-shim.sh — perf-scan, retired to a shim over Banshee (banshee-10w).
#
# The original perf-scan was a 386-line one-shot survey.
# Banshee ported it as a continuously-sampling daemon (docs/signal-collection.md
# is the port spec), and parity was reconciled on one machine-minute on
# 2026-09-01 (the record is in the maintainer's notebook). This shim preserves the perf-scan CLI
# surface and maps it onto the banshee CLI:
#
#   perf-scan                    banshee status + banshee census
#   perf-scan --quiet            banshee status   (verdict + worklist only)
#   perf-scan --reap-tmux        banshee reap-stale-sessions --execute
#   perf-scan --reap-orphans     banshee reap-orphans --execute
#
# What moved, honestly:
#   - The staleness threshold now lives in the DAEMON's census config
#     (`stale_days` in census.local.toml), so `--reap-tmux DAYS` is REFUSED
#     rather than silently reinterpreted: proceeding under the daemon's
#     threshold could reap MORE than the caller asked for, and the reap rails
#     exist to prevent exactly that shape.
#   - When both reaps are asked for, sessions go FIRST: killing a session
#     orphans its MCP servers, so the orphan sweep must come second. (The
#     original reaped in the other order and then told you to run it again.)
#   - History and rates — the reason Banshee exists — are `banshee samples`
#     and `banshee rollups`. Disk hotspots were never perf-scan's and are not
#     Banshee's either (ADR-0003): that is a dedicated disk-space tool's territory.
#
# Requires the banshee daemon (`make daemon-install` in the banshee repo). Exit
# codes are the banshee CLI's (0 ok, 1 server error, 3 no census yet, 4 cannot
# reach the API), plus perf-scan's own 2 for a usage error.

set -u

# Resolve the CLI: an explicit override (used by the shim's own tests), PATH,
# then the installed location — NEVER a build directory (ADR-0004:
# rust/target/ is not an installation).
find_banshee() {
    if [ -n "${BANSHEE_BIN:-}" ]; then
        printf '%s' "$BANSHEE_BIN"
        return 0
    fi
    if command -v banshee >/dev/null 2>&1; then
        printf 'banshee'
        return 0
    fi
    local installed="$HOME/Library/Application Support/banshee/bin/banshee"
    if [ -x "$installed" ]; then
        printf '%s' "$installed"
        return 0
    fi
    return 1
}

usage() {
    sed -n '2,30p' "$0" | sed 's/^# \{0,1\}//'
}

REAP_ORPHANS=0
REAP_TMUX=0
QUIET=0
while [ $# -gt 0 ]; do
    case "$1" in
        --reap-orphans) REAP_ORPHANS=1 ;;
        --reap-tmux)
            REAP_TMUX=1
            case "${2:-}" in
                [0-9]*)
                    echo "perf-scan: --reap-tmux no longer takes DAYS — the threshold is the banshee" >&2
                    echo "daemon's census config (stale_days in census.local.toml). Refusing rather" >&2
                    echo "than reaping under a different threshold than you asked for." >&2
                    exit 2
                    ;;
            esac
            ;;
        --quiet) QUIET=1 ;;
        -h|--help) usage; exit 0 ;;
        *) printf "unknown option: %s (try --help)\n" "$1" >&2; exit 2 ;;
    esac
    shift
done

BANSHEE="$(find_banshee)" || {
    echo "perf-scan: the banshee CLI is not installed. perf-scan retired to a shim" >&2
    echo "over Banshee (2026-09-01); run \`make daemon-install\` in the banshee repo." >&2
    exit 4
}

# One survey note, so nobody wonders where the old sections went.
echo "perf-scan is a shim over Banshee — the daemon samples this continuously."
echo "(history: banshee samples|rollups · hotspots: a dedicated disk tool · perf-scan --help)"
echo

"$BANSHEE" status || exit $?
if [ "$QUIET" != 1 ]; then
    echo
    "$BANSHEE" census || exit $?
fi

# Destructive steps report unconditionally — --quiet trims the survey, never
# the record of what was killed. That was the original's rule and it survives
# the retirement.
STATUS=0
if [ "$REAP_TMUX" = 1 ]; then
    echo
    "$BANSHEE" reap-stale-sessions --execute || STATUS=$?
fi
if [ "$REAP_ORPHANS" = 1 ]; then
    echo
    "$BANSHEE" reap-orphans --execute || STATUS=$?
fi
exit "$STATUS"
