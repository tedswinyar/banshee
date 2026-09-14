#!/usr/bin/env bash
# service.sh — the ONE place that knows the LaunchAgent's identity.
#
# Sourced by install-launchd.sh, uninstall-launchd.sh and check-service.sh so the
# label, the paths and the port cannot drift between the thing that installs the
# service and the thing that checks it. That drift is not hypothetical: another project
# served on 8766 while its config crate defaulted to 18766 because two places
# independently knew "the port".
#
# Every path is overridable by an environment variable so the scripts suite can
# exercise them against a temp directory instead of the real machine. A gate that
# can only be tested by installing a real LaunchAgent is a gate nobody tests.

# The label is a contract with launchd and appears in `launchctl print`.
BANSHEE_LABEL="${BANSHEE_LABEL:-com.tedswinyar.banshee-api}"

# Where the plist lives. launchd owns this directory; it REWRITES plists here in
# binary form, which is why every reader below uses PlistBuddy and not grep.
BANSHEE_LAUNCHD_DIR="${BANSHEE_LAUNCHD_DIR:-$HOME/Library/LaunchAgents}"
BANSHEE_PLIST="$BANSHEE_LAUNCHD_DIR/$BANSHEE_LABEL.plist"

# Where the INSTALLED binary lives. Deliberately not rust/target/ — see
# check_program_path below for the eight days that cost someone.
BANSHEE_INSTALL_DIR="${BANSHEE_INSTALL_DIR:-$HOME/Library/Application Support/banshee/bin}"
BANSHEE_INSTALLED_BIN="$BANSHEE_INSTALL_DIR/banshee-api"
# The CLI and the perf-scan shim install alongside the daemon (banshee-10w):
# the retired perf-scan wrapper execs the installed shim, which
# needs an installed CLI — a shim that reached into rust/target/ would be
# ADR-0004's eight-day outage with a new name.
BANSHEE_INSTALLED_CLI="$BANSHEE_INSTALL_DIR/banshee"
BANSHEE_INSTALLED_SHIM="$BANSHEE_INSTALL_DIR/perf-scan-shim.sh"

# launchd redirects the daemon's stdout/stderr here. The port announcement lands
# in the stdout log, which is the only place to learn the bound port.
BANSHEE_LOG_DIR="${BANSHEE_LOG_DIR:-$HOME/Library/Logs}"
BANSHEE_STDOUT_LOG="$BANSHEE_LOG_DIR/banshee-api.log"
BANSHEE_STDERR_LOG="$BANSHEE_LOG_DIR/banshee-api.err.log"

# The daemon runs the prod profile, which PINS this port (ADR-0004): a daemon that
# quietly moved to 18770 would leave every client asking 18769 and getting nothing,
# while launchctl reported a healthy service.
BANSHEE_DAEMON_PORT="${BANSHEE_DAEMON_PORT:-18769}"

# The Unix-domain socket every client authenticates over (ADR-0008). The SAME
# variable the daemon and the clients read (`banshee_core::API_SOCKET_ENV`), so the
# checker cannot look somewhere the daemon is not — the drift this file exists to
# prevent, in the transport that carries the key. Beside the key file by default.
BANSHEE_DATA_DIR="${BANSHEE_DATA_DIR:-$HOME/Library/Application Support/banshee}"
BANSHEE_API_SOCKET="${BANSHEE_API_SOCKET:-$BANSHEE_DATA_DIR/api.sock}"

# The GUI domain for this user's agents. `launchctl bootstrap gui/<uid>` is the
# modern spelling; `launchctl load` is deprecated and, worse, silently succeeds on
# a plist launchd then refuses to run.
banshee_domain() { echo "gui/$(id -u)"; }
banshee_service_target() { echo "$(banshee_domain)/$BANSHEE_LABEL"; }

# The optional Slack-webhook block for the plist's EnvironmentVariables dict.
#
# The webhook is a CREDENTIAL and must never be written in this repo, so it is
# passed through from the installing shell's `BANSHEE_SLACK_WEBHOOK_URL`:
#   BANSHEE_SLACK_WEBHOOK_URL=https://hooks.slack.com/... make daemon-install
# lands it only in the plist under ~/Library. Unset (or empty) → EMPTY output, so
# the block is simply omitted and the daemon keeps escalation alerts in-app.
#
# A function so the scripts suite can pin it without running the full installer
# (which bootstraps a real LaunchAgent). Note `${VAR:-}` treats empty and unset
# alike — an empty webhook is not a webhook. XML-escapes `&<>` because a webhook
# URL is opaque input landing inside XML.
banshee_slack_env_block() {
    local url="${BANSHEE_SLACK_WEBHOOK_URL:-}"
    [ -n "$url" ] || return 0
    local esc
    esc="$(printf '%s' "$url" | sed -e 's/&/\&amp;/g' -e 's/</\&lt;/g' -e 's/>/\&gt;/g')"
    printf '\n        <key>BANSHEE_SLACK_WEBHOOK_URL</key>\n        <string>%s</string>' "$esc"
}

# Read one value out of a plist. **PlistBuddy, never grep**: launchd rewrites
# plists in binary form once it has loaded them, so a `grep` that worked on the
# freshly written XML returns nothing an hour later — and "nothing" reads as
# "the key is absent" rather than "I cannot read this file".
plist_value() {
    local key="$1" file="${2:-$BANSHEE_PLIST}"
    /usr/libexec/PlistBuddy -c "Print $key" "$file" 2>/dev/null
}

# The single most important check in P5, and the reason check-service.sh exists.
#
# Another project's plist named a `rust/target/release` binary directly. A
# benchmark run recreated `target/release` WITHOUT that binary, and the already
# running process kept serving from its deleted inode for **eight days** — until a
# reboot exposed it. launchd then reported exit **78**, which is `EX_CONFIG` and
# means "cannot exec the program", NOT "your plist is malformed"; two more days
# went into that misreading.
#
# So: a build directory is not an installation, and this refuses to call one.
# Returns 0 when the path is a legitimate installed location.
program_path_is_installed() {
    local path="$1"
    case "$path" in
        */target/debug/*|*/target/release/*|*/target/*)
            return 1
            ;;
        "")
            return 1
            ;;
    esac
    return 0
}
