#!/usr/bin/env bash
set -u

# Tests OF the daemon gate. The headline case is ADR-0004's acceptance criterion:
# check-service.sh must FAIL when the plist points into a build directory.
#
# Everything here runs against a TEMP directory, never the real machine: the
# service scripts take their paths from environment variables
# (scripts/lib/service.sh) precisely so this suite can exercise them without
# registering a LaunchAgent on whatever machine runs the tests. A gate that can
# only be tested by installing a real agent is a gate nobody tests, and this one
# guards an eight-day outage shape.

SCRIPT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
ROOT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
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

# Assert a command FAILS. Stated as its own helper because "the gate refuses bad
# input" is the property under test here, and `t "..." ! cmd` does not work in a
# way that survives `set -u` plus a function call.
t_fails() {
  local desc="$1"
  shift
  if "$@" >/dev/null 2>&1; then
    FAIL=$((FAIL + 1))
    echo "  ✗ $desc (command SUCCEEDED and should not have)" >&2
  else
    PASS=$((PASS + 1))
  fi
}

WORK="$(mktemp -d /tmp/banshee-service-test.XXXXXX)"
trap 'rm -rf "$WORK"' EXIT

export BANSHEE_LABEL="com.tedswinyar.banshee-api-TEST"
export BANSHEE_LAUNCHD_DIR="$WORK/LaunchAgents"
export BANSHEE_INSTALL_DIR="$WORK/install/bin"
export BANSHEE_LOG_DIR="$WORK/Logs"
# A port nothing is listening on, so the health probe fails deterministically
# rather than accidentally reaching a real daemon on 18769.
export BANSHEE_DAEMON_PORT="18991"
# A socket path inside the work dir, so the checks below never touch the real
# daemon's socket — and never pass because of it.
export BANSHEE_API_SOCKET="$WORK/api.sock"
export BANSHEE_KEY_FILE="$WORK/api_key"
PLIST="$BANSHEE_LAUNCHD_DIR/$BANSHEE_LABEL.plist"

mkdir -p "$BANSHEE_LAUNCHD_DIR" "$BANSHEE_INSTALL_DIR" "$BANSHEE_LOG_DIR"

# ---------------------------------------------------------------------------
# 1. Not installed at all → exit 2, and say how to install
# ---------------------------------------------------------------------------
OUT="$("$SCRIPT_DIR/check-service.sh" --no-launchctl 2>&1)"
STATUS=$?
t "missing plist exits 2 (not installed)" [ "$STATUS" -eq 2 ]
t "missing plist names the install script" \
  bash -c "echo \"\$1\" | grep -q 'install-launchd.sh'" _ "$OUT"

# ---------------------------------------------------------------------------
# 2. THE acceptance criterion: a plist pointing into target/ must FAIL
# ---------------------------------------------------------------------------
# Another project shipped exactly this, a benchmark run deleted the binary out from
# under it, and the daemon served from a deleted inode for eight days.
write_plist() {
  # `${3-default}` and NOT `${3:-default}`: the colon form substitutes the default
  # for an EMPTY argument as well as a missing one, so `write_plist bin prod ""`
  # silently wrote a pinned port and the unpinned-port case was never actually
  # unpinned — a test that passed while testing nothing.
  local program="$1" profile="${2:-prod}" port="${3-$BANSHEE_DAEMON_PORT}"
  local ptype="${4:-Standard}"
  cat > "$PLIST" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key><string>$BANSHEE_LABEL</string>
    <key>ProgramArguments</key><array><string>$program</string></array>
    <key>EnvironmentVariables</key><dict>
        <key>BANSHEE_PROFILE</key><string>$profile</string>
        <key>BANSHEE_PORT</key><string>$port</string>
    </dict>
    <key>RunAtLoad</key><true/>
    <key>KeepAlive</key><dict><key>SuccessfulExit</key><false/></dict>
    <key>ThrottleInterval</key><integer>5</integer>
    <key>ProcessType</key><string>$ptype</string>
    <key>StandardOutPath</key><string>$BANSHEE_LOG_DIR/banshee-api.log</string>
    <key>StandardErrorPath</key><string>$BANSHEE_LOG_DIR/banshee-api.err.log</string>
</dict>
</plist>
PLIST
}

# A real executable inside a fake target/ tree, so the ONLY thing wrong is the
# path. Without this the test would also trip the "does not exist" check and
# could pass for the wrong reason.
mkdir -p "$WORK/repo/rust/target/release"
TARGET_BIN="$WORK/repo/rust/target/release/banshee-api"
printf '#!/bin/sh\nexit 0\n' > "$TARGET_BIN"
chmod +x "$TARGET_BIN"

write_plist "$TARGET_BIN"
OUT="$("$SCRIPT_DIR/check-service.sh" --no-launchctl 2>&1)"
t_fails "a plist pointing into target/ FAILS the check" \
  "$SCRIPT_DIR/check-service.sh" --no-launchctl
t "the failure names the build directory" \
  bash -c "echo \"\$1\" | grep -qi 'BUILD DIRECTORY'" _ "$OUT"
t "the failure explains the deleted-inode consequence" \
  bash -c "echo \"\$1\" | grep -qi 'deleted inode'" _ "$OUT"
t "the failure explains exit 78 is EX_CONFIG, not a bad plist" \
  bash -c "echo \"\$1\" | grep -q '78'" _ "$OUT"
t "the failure names the fix" \
  bash -c "echo \"\$1\" | grep -q 'make daemon-install'" _ "$OUT"

# A debug build directory is just as wrong as a release one.
mkdir -p "$WORK/repo/rust/target/debug"
printf '#!/bin/sh\nexit 0\n' > "$WORK/repo/rust/target/debug/banshee-api"
chmod +x "$WORK/repo/rust/target/debug/banshee-api"
write_plist "$WORK/repo/rust/target/debug/banshee-api"
t_fails "target/debug is refused too" "$SCRIPT_DIR/check-service.sh" --no-launchctl

# ---------------------------------------------------------------------------
# 3. A plist naming a program that does not exist → the exit-78 shape
# ---------------------------------------------------------------------------
write_plist "$BANSHEE_INSTALL_DIR/banshee-api"   # installed path, but absent
OUT="$("$SCRIPT_DIR/check-service.sh" --no-launchctl 2>&1)"
t_fails "a missing program fails the check" \
  "$SCRIPT_DIR/check-service.sh" --no-launchctl
t "a missing program is reported as such" \
  bash -c "echo \"\$1\" | grep -q 'does not exist'" _ "$OUT"

# ---------------------------------------------------------------------------
# 4. Wrong ProcessType and unpinned settings are caught
# ---------------------------------------------------------------------------
GOOD_BIN="$BANSHEE_INSTALL_DIR/banshee-api"
printf '#!/bin/sh\nexit 0\n' > "$GOOD_BIN"
chmod +x "$GOOD_BIN"

write_plist "$GOOD_BIN" prod "$BANSHEE_DAEMON_PORT" "Background"
OUT="$("$SCRIPT_DIR/check-service.sh" --no-launchctl 2>&1)"
t_fails "ProcessType Background is refused" \
  "$SCRIPT_DIR/check-service.sh" --no-launchctl
t "the ProcessType failure explains the I/O throttling" \
  bash -c "echo \"\$1\" | grep -qi 'throttl'" _ "$OUT"

write_plist "$GOOD_BIN" dev "$BANSHEE_DAEMON_PORT"
OUT="$("$SCRIPT_DIR/check-service.sh" --no-launchctl 2>&1)"
t "a non-prod profile is reported" \
  bash -c "echo \"\$1\" | grep -q 'want prod'" _ "$OUT"

# An unpinned port matters because the PROD profile does not climb the ladder:
# without BANSHEE_PORT the daemon's port would depend on the config default alone.
write_plist "$GOOD_BIN" prod ""
OUT="$("$SCRIPT_DIR/check-service.sh" --no-launchctl 2>&1)"
t "an unpinned port is reported" \
  bash -c "echo \"\$1\" | grep -q 'not pinned'" _ "$OUT"

# ---------------------------------------------------------------------------
# 5. A well-formed plist passes every STATIC check
# ---------------------------------------------------------------------------
# It still fails overall, because nothing is answering on the test port — and that
# is correct: "the plist is right" and "the daemon is serving" are different
# claims, and this script exists to keep them apart.
write_plist "$GOOD_BIN"
OUT="$("$SCRIPT_DIR/check-service.sh" --no-launchctl 2>&1)"
t "a good plist passes the build-directory check" \
  bash -c "echo \"\$1\" | grep -q 'not a build directory'" _ "$OUT"
t "a good plist passes the program-exists check" \
  bash -c "echo \"\$1\" | grep -q 'program exists'" _ "$OUT"
t "a good plist passes the profile check" \
  bash -c "echo \"\$1\" | grep -q 'profile pinned to prod'" _ "$OUT"
t "a good plist passes the ProcessType check" \
  bash -c "echo \"\$1\" | grep -q 'ProcessType Standard'" _ "$OUT"
t "a dead port is still reported as nothing answering" \
  bash -c "echo \"\$1\" | grep -q 'nothing answering'" _ "$OUT"
t "a missing socket is reported, naming the path" \
  bash -c "echo \"\$1\" | grep -q \"no socket at $BANSHEE_API_SOCKET\"" _ "$OUT"

# ---------------------------------------------------------------------------
# 5b. The socket IS the transport that matters (ADR-0008), so the checker probes it
#     and verifies its mode. Real sockets, made by python3: a socket file nobody
#     serves (what a crashed daemon leaves behind), one that is group-readable, and
#     one with a live HTTP server behind it answering /health and a keyed /stats.
# ---------------------------------------------------------------------------
mk_socket() {
  # $1 = path, $2 = octal mode. The process exits, so nothing is listening.
  python3 - "$1" "$2" <<'PY'
import os, socket, sys
s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
s.bind(sys.argv[1])
os.chmod(sys.argv[1], int(sys.argv[2], 8))
PY
}
rm -f "$BANSHEE_API_SOCKET"
mk_socket "$BANSHEE_API_SOCKET" 660
OUT="$("$SCRIPT_DIR/check-service.sh" --no-launchctl 2>&1)"
t "a group-readable socket is refused with its mode" \
  bash -c "echo \"\$1\" | grep -q 'socket mode is 660, want 600'" _ "$OUT"
t "a socket nobody serves is reported as stale, not as healthy" \
  bash -c "echo \"\$1\" | grep -q 'socket present but nothing answering'" _ "$OUT"

rm -f "$BANSHEE_API_SOCKET"
mk_socket "$BANSHEE_API_SOCKET" 600
OUT="$("$SCRIPT_DIR/check-service.sh" --no-launchctl 2>&1)"
t "an owner-only socket passes the mode check" \
  bash -c "echo \"\$1\" | grep -q 'socket is owner-only (0600)'" _ "$OUT"

# A live server on the socket. The TCP port stays dead, so 'sampling: 7' can only
# have come over the socket — which is the assertion: the keyed read goes over the
# socket and nowhere else.
rm -f "$BANSHEE_API_SOCKET"
printf 'test-key' > "$BANSHEE_KEY_FILE"
python3 - "$BANSHEE_API_SOCKET" <<'PY' &
import http.server, os, socket, sys
class Server(http.server.HTTPServer):
    address_family = socket.AF_UNIX
    def server_bind(self):
        self.socket.bind(self.server_address)
        os.chmod(self.server_address, 0o600)
class H(http.server.BaseHTTPRequestHandler):
    def log_message(self, *a): pass
    def do_GET(self):
        if self.path == "/health":
            body = b'{"status":"ok"}'
        elif self.path == "/stats" and self.headers.get("x-api-key") == "test-key":
            body = b'{"samples":7}'
        else:
            self.send_response(401); self.end_headers(); return
        self.send_response(200)
        self.send_header("content-length", str(len(body))); self.end_headers()
        self.wfile.write(body)
Server(sys.argv[1], H).serve_forever()
PY
SOCK_SERVER=$!
for _ in 1 2 3 4 5 6 7 8 9 10; do [ -S "$BANSHEE_API_SOCKET" ] && break; sleep 0.2; done
OUT="$("$SCRIPT_DIR/check-service.sh" --no-launchctl 2>&1)"
kill "$SOCK_SERVER" 2>/dev/null; wait "$SOCK_SERVER" 2>/dev/null
t "a served socket is reported as answering" \
  bash -c "echo \"\$1\" | grep -q 'answering on the socket'" _ "$OUT"
t "the keyed sampling check goes over the SOCKET (the TCP port is dead)" \
  bash -c "echo \"\$1\" | grep -q 'sampling: 7 samples stored'" _ "$OUT"
t "TCP being dead is still reported alongside" \
  bash -c "echo \"\$1\" | grep -q 'nothing answering on http'" _ "$OUT"
rm -f "$BANSHEE_API_SOCKET" "$BANSHEE_KEY_FILE"

# ---------------------------------------------------------------------------
# 6. install-launchd.sh --dry-run: real plist, real atomic install, no launchd
# ---------------------------------------------------------------------------
rm -f "$PLIST" "$GOOD_BIN"
# Build a fake source binary where install-launchd.sh expects one, so this test
# never waits on cargo.
mkdir -p "$ROOT_DIR/rust/target/release"
SOURCE="$ROOT_DIR/rust/target/release/banshee-api"
CREATED_SOURCE=0
if [ ! -e "$SOURCE" ]; then
  printf '#!/bin/sh\nexit 0\n' > "$SOURCE"
  chmod +x "$SOURCE"
  CREATED_SOURCE=1
fi
# The CLI installs alongside the daemon since banshee-10w (the perf-scan shim
# calls it); fake it the same way.
SOURCE_CLI="$ROOT_DIR/rust/target/release/banshee"
CREATED_CLI=0
if [ ! -e "$SOURCE_CLI" ]; then
  printf '#!/bin/sh\nexit 0\n' > "$SOURCE_CLI"
  chmod +x "$SOURCE_CLI"
  CREATED_CLI=1
fi
GOOD_CLI="$BANSHEE_INSTALL_DIR/banshee"
GOOD_SHIM="$BANSHEE_INSTALL_DIR/perf-scan-shim.sh"

# The dirty-tree gate (banshee-b2w) inspects BANSHEE_REPO_DIR. Point it at a
# CLEAN temp repo so these dry-run installs pass regardless of the state of the
# checkout running this suite — otherwise a developer's uncommitted work would
# make the whole section flake.
CLEAN_REPO="$WORK/clean-repo"
mkdir -p "$CLEAN_REPO"
( cd "$CLEAN_REPO" && git init -q && git -c user.email=t@t -c user.name=t commit -q --allow-empty -m init )
export BANSHEE_REPO_DIR="$CLEAN_REPO"

t "install --dry-run succeeds" "$SCRIPT_DIR/install-launchd.sh" --dry-run
t "install --dry-run wrote the plist" test -f "$PLIST"
t "install --dry-run installed the binary" test -x "$GOOD_BIN"
t "install --dry-run installed the CLI" test -x "$GOOD_CLI"
t "install --dry-run installed the perf-scan shim" test -x "$GOOD_SHIM"
t "install --dry-run left no .incoming file" \
  bash -c "[ ! -e '$GOOD_BIN.incoming' ] && [ ! -e '$GOOD_CLI.incoming' ] && [ ! -e '$GOOD_SHIM.incoming' ]"
t "the written plist is valid plist data" plutil -lint "$PLIST"

# The plist may carry the Slack webhook credential in EnvironmentVariables, so
# it must be 0600 — a world-readable LaunchAgent would leak it (banshee-0sa).
t "the installed plist is chmod 600 (credential-bearing)" bash -c \
  "[ \"\$(stat -f '%Lp' '$PLIST')\" = '600' ]"

# The daemon's logs must be owner-only too (banshee-a4i). launchd creates
# StandardOutPath/StandardErrorPath honouring the inherited umask — measured 0644 on
# a real machine — so anything the daemon logs about a failed request or a webhook
# delivery was readable by exactly the local users the 0600 key file exists to
# exclude. Pre-creating them 0600 is what makes it stick, because launchd APPENDS to
# an existing file rather than recreating it.
for _l in "$BANSHEE_LOG_DIR/banshee-api.log" "$BANSHEE_LOG_DIR/banshee-api.err.log"; do
  t "install pre-created $(basename "$_l")" test -f "$_l"
  t "$(basename "$_l") is chmod 600" bash -c \
    "[ \"\$(stat -f '%Lp' '$_l')\" = '600' ]"
done

# …and REPAIRS an already-loose log, since this script is the update path for every
# machine installed before the logs were hardened.
chmod 644 "$BANSHEE_LOG_DIR/banshee-api.err.log"
t "a pre-existing 0644 log is repaired by a re-install" bash -c \
  "'$SCRIPT_DIR/install-launchd.sh' --dry-run >/dev/null 2>&1 && \
   [ \"\$(stat -f '%Lp' '$BANSHEE_LOG_DIR/banshee-api.err.log')\" = '600' ]"
# The repair must not have truncated it — logs are evidence.
echo "existing line" >> "$BANSHEE_LOG_DIR/banshee-api.err.log"
"$SCRIPT_DIR/install-launchd.sh" --dry-run >/dev/null 2>&1
t "re-install preserves existing log contents" \
  grep -q "existing line" "$BANSHEE_LOG_DIR/banshee-api.err.log"

# The plist install-launchd.sh writes must PASS its own checker. Two scripts, one
# source of truth (scripts/lib/service.sh) — this is what proves they agree.
OUT="$("$SCRIPT_DIR/check-service.sh" --no-launchctl 2>&1)"
t "the installed plist points at an installed path" \
  bash -c "echo \"\$1\" | grep -q 'not a build directory'" _ "$OUT"
t "the installed plist pins prod" \
  bash -c "echo \"\$1\" | grep -q 'profile pinned to prod'" _ "$OUT"
t "the installed plist pins the port" \
  bash -c "echo \"\$1\" | grep -q \"port pinned to $BANSHEE_DAEMON_PORT\"" _ "$OUT"
t "the installed plist sets ProcessType Standard" \
  bash -c "echo \"\$1\" | grep -q 'ProcessType Standard'" _ "$OUT"

# The values launchd needs but check-service.sh does not assert, read the way
# check-service.sh reads them (PlistBuddy, because launchd rewrites to binary).
t "RunAtLoad is set" bash -c \
  "/usr/libexec/PlistBuddy -c 'Print :RunAtLoad' '$PLIST' | grep -q true"
t "KeepAlive respects a clean exit" bash -c \
  "/usr/libexec/PlistBuddy -c 'Print :KeepAlive:SuccessfulExit' '$PLIST' | grep -q false"
t "ThrottleInterval is 5" bash -c \
  "/usr/libexec/PlistBuddy -c 'Print :ThrottleInterval' '$PLIST' | grep -q 5"
t "no LowPriorityIO key" bash -c \
  "! /usr/libexec/PlistBuddy -c 'Print :LowPriorityIO' '$PLIST' >/dev/null 2>&1"
t "logs are redirected to a file launchd can write" bash -c \
  "/usr/libexec/PlistBuddy -c 'Print :StandardErrorPath' '$PLIST' | grep -q 'banshee-api.err.log'"

# The dirty-tree gate (banshee-b2w): an install from a tree with uncommitted
# changes would leave the daemon reporting a gitRev that matches no commit, so
# it is refused — with --force as the deliberate escape. Driven against a temp
# repo so the case is deterministic, not a function of this checkout's state.
DIRTY_REPO="$WORK/dirty-repo"
mkdir -p "$DIRTY_REPO"
( cd "$DIRTY_REPO" && git init -q && git -c user.email=t@t -c user.name=t commit -q --allow-empty -m init && echo uncommitted > tracked.txt && git add tracked.txt )
t_fails "install refuses a dirty tree" \
  env BANSHEE_REPO_DIR="$DIRTY_REPO" "$SCRIPT_DIR/install-launchd.sh" --dry-run
OUT="$(BANSHEE_REPO_DIR="$DIRTY_REPO" "$SCRIPT_DIR/install-launchd.sh" --dry-run 2>&1)"
t "the dirty-tree refusal explains itself" \
  bash -c "echo \"\$1\" | grep -q 'working tree is dirty'" _ "$OUT"
t "install --force overrides the dirty-tree gate" \
  env BANSHEE_REPO_DIR="$DIRTY_REPO" "$SCRIPT_DIR/install-launchd.sh" --dry-run --force

# Idempotence: installing twice is the documented way to UPDATE, so it must not
# accumulate state or fail on an existing plist.
t "install --dry-run is idempotent" "$SCRIPT_DIR/install-launchd.sh" --dry-run
t "still exactly one plist" bash -c \
  "[ \"\$(find '$BANSHEE_LAUNCHD_DIR' -name '*.plist' | wc -l | tr -d ' ')\" = 1 ]"

# ---------------------------------------------------------------------------
# 7. uninstall removes the plist and leaves data alone
# ---------------------------------------------------------------------------
# A fake database, to prove uninstall never touches it: a day of samples is not
# something to discard as a side effect of unregistering a service.
DB="$WORK/install/banshee.db"
echo "precious" > "$DB"
t "uninstall succeeds" "$SCRIPT_DIR/uninstall-launchd.sh"
t "uninstall removed the plist" bash -c "[ ! -f '$PLIST' ]"
t "uninstall left the installed binary" test -x "$GOOD_BIN"
t "uninstall left the database alone" bash -c "grep -q precious '$DB'"
t "uninstall --purge removes the binary" bash -c \
  "'$SCRIPT_DIR/install-launchd.sh' --dry-run >/dev/null 2>&1 && \
   '$SCRIPT_DIR/uninstall-launchd.sh' --purge >/dev/null 2>&1 && [ ! -e '$GOOD_BIN' ]"
t "uninstall --purge removes the CLI and the shim too" \
  bash -c "[ ! -e '$GOOD_CLI' ] && [ ! -e '$GOOD_SHIM' ]"
t "uninstall --purge still left the database alone" bash -c "grep -q precious '$DB'"
t "uninstall is safe when nothing is installed" "$SCRIPT_DIR/uninstall-launchd.sh"

[ "$CREATED_SOURCE" = "1" ] && rm -f "$SOURCE"
[ "$CREATED_CLI" = "1" ] && rm -f "$SOURCE_CLI"

# ---------------------------------------------------------------------------
# 8. The path predicate itself
# ---------------------------------------------------------------------------
# shellcheck source=../lib/service.sh
. "$ROOT_DIR/scripts/lib/service.sh"
t "an installed path is accepted" \
  program_path_is_installed "$HOME/Library/Application Support/banshee/bin/banshee-api"
t_fails "a release build path is refused" \
  program_path_is_installed "/x/rust/target/release/banshee-api"
t_fails "a debug build path is refused" \
  program_path_is_installed "/x/rust/target/debug/banshee-api"
t_fails "any target/ path is refused" \
  program_path_is_installed "/x/target/whatever/banshee-api"
t_fails "an empty path is refused" program_path_is_installed ""

# ---------------------------------------------------------------------------
# 9. The Slack webhook pass-through (P7 chunk 2)
# ---------------------------------------------------------------------------
# The webhook is a credential: it must reach the plist ONLY from the installing
# shell's environment, never from anything in the repo. `banshee_slack_env_block`
# is the seam, pinned here without running the full installer.

# Unset → no block, so the daemon keeps escalation alerts in-app.
unset BANSHEE_SLACK_WEBHOOK_URL
t "no webhook env yields an empty block" \
  bash -c "[ -z \"\$(banshee_slack_env_block)\" ]"

# Empty is not a webhook (the ${VAR:-} empty-vs-unset trap): still no block.
t "an empty webhook env yields an empty block" \
  bash -c "BANSHEE_SLACK_WEBHOOK_URL='' . '$ROOT_DIR/scripts/lib/service.sh'; [ -z \"\$(banshee_slack_env_block)\" ]"

# Set → the key and the URL appear in the block.
export BANSHEE_SLACK_WEBHOOK_URL="https://hooks.slack.com/services/T00/B00/secret"
BLOCK="$(banshee_slack_env_block)"
t "a set webhook names the env key" \
  bash -c "echo \"\$1\" | grep -q '<key>BANSHEE_SLACK_WEBHOOK_URL</key>'" _ "$BLOCK"
t "a set webhook carries the URL" \
  bash -c "echo \"\$1\" | grep -q 'hooks.slack.com/services/T00/B00/secret'" _ "$BLOCK"

# An `&` in the URL is XML-escaped, because the block lands inside a plist's XML.
export BANSHEE_SLACK_WEBHOOK_URL="https://example.com/hook?a=1&b=2"
ESC="$(banshee_slack_env_block)"
t "an ampersand in the URL is XML-escaped" \
  bash -c "echo \"\$1\" | grep -q 'a=1&amp;b=2'" _ "$ESC"
t "the raw ampersand does not leak" \
  bash -c "! echo \"\$1\" | grep -q 'a=1&b=2'" _ "$ESC"
unset BANSHEE_SLACK_WEBHOOK_URL

echo "test-service: $PASS passed, $FAIL failed"
[ "$FAIL" -eq 0 ]
