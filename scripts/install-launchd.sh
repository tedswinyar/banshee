#!/usr/bin/env bash
set -euo pipefail

# install-launchd.sh — install (or update) banshee-api as a launchd LaunchAgent.
#
# This is what makes Banshee a sentinel rather than a command: the sampling loop
# has to be running while nobody is watching, because every interesting signal is
# a rate (ADR-0004). Until this is installed, the app is a nicer `perf-scan`.
#
# **Rebuilding does not update the running service. Reinstalling does.** Every
# contributor learns this once; `make daemon-install` is the answer, and it is
# idempotent — run it after every change you want the daemon to serve.
#
# Usage:
#   ./scripts/install-launchd.sh [--debug] [--no-build] [--force]
#
#   --debug     install the debug binary instead of release (faster iteration)
#   --no-build  install whatever is already built
#   --force     install even from a dirty tree (stamps /health's gitRev +dirty)
#
# Overridable for tests (see scripts/lib/service.sh): BANSHEE_LAUNCHD_DIR,
# BANSHEE_INSTALL_DIR, BANSHEE_LOG_DIR, BANSHEE_LABEL, BANSHEE_DAEMON_PORT.
# `--dry-run` writes the plist and installs the binary but does not talk to
# launchd, which is how the scripts suite exercises this without registering an
# agent on the machine running the tests.

ROOT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
# shellcheck source=lib/service.sh
. "$ROOT_DIR/scripts/lib/service.sh"

PROFILE_BUILD="release"
DO_BUILD=1
DRY_RUN=0
FORCE=0
while [ "$#" -gt 0 ]; do
    case "$1" in
        --debug)    PROFILE_BUILD="debug" ;;
        --no-build) DO_BUILD=0 ;;
        --dry-run)  DRY_RUN=1; DO_BUILD=0 ;;
        --force)    FORCE=1 ;;
        -h|--help)  sed -n '3,24p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
        *) echo "install-launchd.sh: unknown option: $1" >&2; exit 2 ;;
    esac
    shift
done

say()  { echo "banshee-daemon: $*"; }
fail() { echo "banshee-daemon: FAIL: $*" >&2; exit 1; }

# ---------------------------------------------------------------------------
# 0. Refuse to install from a dirty tree (banshee-b2w)
# ---------------------------------------------------------------------------
# The installed daemon must correspond to a committed revision, or /health's
# gitRev is a lie and "is my latest code running?" stays unanswerable — the
# eight-day deleted-inode outage shape (ADR-0004). `--force` is the deliberate
# escape for local iteration; it stamps `+dirty` into the binary either way.
# Skipped outside a git repo (a source tarball is not "dirty", it is untracked).
#
# BANSHEE_REPO_DIR overrides which tree is inspected, so the scripts suite can
# drive both the clean and dirty cases deterministically against a temp repo
# rather than depending on the state of the checkout running the tests — the
# same env-override pattern the rest of this script uses for testability.
REPO_DIR="${BANSHEE_REPO_DIR:-$ROOT_DIR}"
if [ "$FORCE" != "1" ] && git -C "$REPO_DIR" rev-parse --git-dir >/dev/null 2>&1; then
    if [ -n "$(git -C "$REPO_DIR" status --porcelain)" ]; then
        fail "working tree is dirty — the installed daemon would not match any commit.
  Commit your changes, or pass --force to install from a dirty tree anyway
  (/health will report the revision with a +dirty marker)."
    fi
fi

# ---------------------------------------------------------------------------
# 1. Build
# ---------------------------------------------------------------------------
SOURCE_BIN="$ROOT_DIR/rust/target/$PROFILE_BUILD/banshee-api"
SOURCE_CLI="$ROOT_DIR/rust/target/$PROFILE_BUILD/banshee"
SOURCE_SHIM="$ROOT_DIR/scripts/perf-scan-shim.sh"
if [ "$DO_BUILD" = "1" ]; then
    # The CLI builds and installs alongside the daemon: the perf-scan shim
    # calls it, and `make daemon-install` leaving a stale release CLI behind
    # already cost one session a confused hour.
    say "building banshee-api + banshee ($PROFILE_BUILD)..."
    if [ "$PROFILE_BUILD" = "release" ]; then
        (cd "$ROOT_DIR/rust" && cargo build -p banshee-api -p banshee-cli --release --quiet)
    else
        (cd "$ROOT_DIR/rust" && cargo build -p banshee-api -p banshee-cli --quiet)
    fi
fi
[ -x "$SOURCE_BIN" ] || fail "no banshee-api at $SOURCE_BIN (drop --no-build?)"
[ -x "$SOURCE_CLI" ] || fail "no banshee CLI at $SOURCE_CLI (drop --no-build?)"

# ---------------------------------------------------------------------------
# 2. Install the binary — ATOMICALLY, and never point the plist at target/
# ---------------------------------------------------------------------------
# `cp` over a LIVE binary is either ETXTBSY or a corrupt half-write, and the
# daemon may well be running right now (that is the normal case for an update).
# `rename(2)` is atomic and replaces the directory entry without touching the
# inode the running process holds, so the old process keeps serving from the old
# inode until it is restarted — which is exactly what we want between the copy and
# the kickstart below.
mkdir -p "$BANSHEE_INSTALL_DIR" "$BANSHEE_LAUNCHD_DIR" "$BANSHEE_LOG_DIR"

# The daemon's logs must be OWNER-ONLY (banshee-a4i). launchd creates
# StandardOutPath/StandardErrorPath itself when they are absent, and it creates them
# honouring the inherited umask — measured 0644 on this machine, i.e. readable by
# every local user. That is the same trust boundary the 0600 API key file exists to
# hold, so leaving the logs world-readable makes the key file's mode decorative:
# anything the daemon logs about a failed request or a webhook delivery is readable
# by exactly the users the key is meant to exclude.
#
# Pre-creating them 0600 is what makes this stick. launchd APPENDS to an existing
# file rather than recreating it, so the mode we set here survives every restart;
# chmod-ing after the fact would be undone the next time the file was rotated away
# and recreated. Existing logs are repaired too, since this script is the update
# path for machines installed before this change.
for _log in "$BANSHEE_STDOUT_LOG" "$BANSHEE_STDERR_LOG"; do
    [ -e "$_log" ] || : > "$_log"
    # `chmod 600 --` exits 1 on macOS while still applying the mode, so the status
    # is deliberately not checked here; the mode is verified below instead.
    chmod 600 "$_log" 2>/dev/null || true
    _mode="$(stat -f '%Lp' "$_log" 2>/dev/null || echo '???')"
    case "$_mode" in
        600) ;;
        *) fail "could not make $_log owner-only (mode is $_mode)" ;;
    esac
done
unset _log _mode
# /bin/cp -f, not cp: `cp` is aliased to `cp -i` in this shell, and an
# interactive overwrite prompt inside a script or a background job hangs forever
# on a question nobody can answer.
install_atomic() {
    local src="$1" dst="$2"
    local incoming="$dst.incoming"
    /bin/cp -f "$src" "$incoming"
    chmod 755 "$incoming"
    mv -f "$incoming" "$dst"
    say "installed $(basename "$dst") → $BANSHEE_INSTALL_DIR"
}
install_atomic "$SOURCE_BIN" "$BANSHEE_INSTALLED_BIN"
install_atomic "$SOURCE_CLI" "$BANSHEE_INSTALLED_CLI"
install_atomic "$SOURCE_SHIM" "$BANSHEE_INSTALLED_SHIM"

program_path_is_installed "$BANSHEE_INSTALLED_BIN" \
    || fail "refusing to install: $BANSHEE_INSTALLED_BIN looks like a build directory"

# ---------------------------------------------------------------------------
# 3. Write the plist
# ---------------------------------------------------------------------------
# KeepAlive{SuccessfulExit:false} restarts a CRASH but respects a clean exit, so
# `launchctl bootout` actually stops the thing instead of racing a respawn.
#
# ProcessType Standard, and NO LowPriorityIO. Measured on
# an endpoint-security agent's LaunchAgent: Background + LowPriorityIO throttled an
# I/O-bound job so hard it was still running after 20 minutes. A sampler that
# misses its own 15s schedule is not sampling, and the gaps would look like a calm
# machine.
#
# The port is PINNED via BANSHEE_PORT, and the prod profile does not climb the
# ladder (ADR-0004) — a daemon that silently moved to 18770 would leave the MCP
# server and the menu bar asking 18769 and getting nothing.
#
# The Slack escalation webhook is a CREDENTIAL passed through from the installing
# shell's BANSHEE_SLACK_WEBHOOK_URL and written only to the plist under ~/Library,
# never this repo — see banshee_slack_env_block in lib/service.sh (which the
# scripts suite pins). Unset → empty → the block is omitted.
SLACK_ENV_BLOCK="$(banshee_slack_env_block)"
[ -n "$SLACK_ENV_BLOCK" ] \
    && say "Slack escalation webhook will be written to the plist (credential; not in the repo)"

say "writing $BANSHEE_PLIST"
cat > "$BANSHEE_PLIST" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>$BANSHEE_LABEL</string>

    <key>ProgramArguments</key>
    <array>
        <string>$BANSHEE_INSTALLED_BIN</string>
    </array>

    <key>EnvironmentVariables</key>
    <dict>
        <key>BANSHEE_PROFILE</key>
        <string>prod</string>
        <key>BANSHEE_PORT</key>
        <string>$BANSHEE_DAEMON_PORT</string>$SLACK_ENV_BLOCK
    </dict>

    <key>RunAtLoad</key>
    <true/>

    <key>KeepAlive</key>
    <dict>
        <key>SuccessfulExit</key>
        <false/>
    </dict>

    <key>ThrottleInterval</key>
    <integer>5</integer>

    <key>ProcessType</key>
    <string>Standard</string>

    <key>StandardOutPath</key>
    <string>$BANSHEE_STDOUT_LOG</string>
    <key>StandardErrorPath</key>
    <string>$BANSHEE_STDERR_LOG</string>
</dict>
</plist>
PLIST
plutil -lint "$BANSHEE_PLIST" >/dev/null || fail "the plist we just wrote is invalid"

# 0600, not the umask default (0644): when BANSHEE_SLACK_WEBHOOK_URL is set the
# plist holds a CREDENTIAL in its EnvironmentVariables, and a world-readable
# LaunchAgent plist would leak it to any other local user (banshee-0sa). A user
# LaunchAgent is private to the user, so launchd is happy at 0600 — same posture
# as the 0600 api_key. Tightened unconditionally, so the perms do not depend on
# whether a webhook happened to be configured at this install.
chmod 600 "$BANSHEE_PLIST" || fail "could not restrict permissions on $BANSHEE_PLIST"

if [ "$DRY_RUN" = "1" ]; then
    say "--dry-run: plist written and binary installed; launchd untouched"
    exit 0
fi

# ---------------------------------------------------------------------------
# 4. Register with launchd
# ---------------------------------------------------------------------------
# `bootstrap`/`bootout`/`kickstart`, NOT `load`/`unload`. The legacy verbs are
# deprecated and their failures are quiet: `launchctl load` can return 0 on a
# plist launchd then refuses to run, which is indistinguishable from success until
# you notice nothing is sampling.
DOMAIN="$(banshee_domain)"
TARGET="$(banshee_service_target)"

if launchctl print "$TARGET" >/dev/null 2>&1; then
    say "booting out the existing service"
    launchctl bootout "$TARGET" 2>/dev/null || true
    # bootout is asynchronous; wait for it rather than sleeping a fixed time.
    for _ in $(seq 1 50); do
        launchctl print "$TARGET" >/dev/null 2>&1 || break
        sleep 0.1
    done
fi

say "bootstrapping $TARGET"
launchctl bootstrap "$DOMAIN" "$BANSHEE_PLIST" \
    || fail "launchctl bootstrap failed; check $BANSHEE_STDERR_LOG"

# kickstart -k restarts it if RunAtLoad already started one, so an update takes
# effect now rather than at next login.
launchctl kickstart -k "$TARGET" >/dev/null 2>&1 || true

# ---------------------------------------------------------------------------
# 5. Prove it is actually serving
# ---------------------------------------------------------------------------
# POLL, never sleep. A fixed sleep encodes an assumption about machine speed and
# is wrong in both directions — too short and this flakes on a loaded machine, too
# long and every install pays the worst case.
say "waiting for /health on port $BANSHEE_DAEMON_PORT..."
BASE="http://127.0.0.1:$BANSHEE_DAEMON_PORT"
for _ in $(seq 1 100); do
    if curl -sf "$BASE/health" >/dev/null 2>&1; then
        say "healthy: $(curl -sf "$BASE/health")"
        say "installed. \`./scripts/check-service.sh\` reports machine state;"
        say "\`banshee status\` asks it how the machine is doing."
        exit 0
    fi
    sleep 0.1
done

fail "the service was registered but never answered /health.
  launchctl:  launchctl print $TARGET | head -40
  stdout:     tail -20 $BANSHEE_STDOUT_LOG
  stderr:     tail -20 $BANSHEE_STDERR_LOG
  If launchd reports exit 78, that is EX_CONFIG — it means it could not EXEC the
  program, not that this plist is malformed. Check ProgramArguments[0] exists."
