#!/usr/bin/env bash
set -u

# Tests OF the release restore drill (banshee-d8r). The drill is only worth
# anything if it FAILS when the backup is bad — otherwise it is the aspirational
# "restore from the backup that was never taken" it exists to replace. Driven
# with a STUB banshee CLI (BANSHEE_CLI) so no live daemon is needed; the stub
# emits a canned `backup --json` pointing at a sqlite DB we control, and each
# case makes that DB good or bad and asserts the drill's verdict.

SCRIPT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
ROOT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
DRILL="$SCRIPT_DIR/restore-drill.sh"
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

WORK="$(mktemp -d /tmp/banshee-drill-test.XXXXXX)"
trap 'rm -rf "$WORK"' EXIT

# Build a stub `banshee` CLI. `stub backup --json` prints a fixed JSON pointing
# at $STUB_DB with a $STUB_SAMPLES count; anything else exits 1. The DB itself is
# created by each test, so a case can make it valid, missing, or mismatched.
STUB="$WORK/banshee"
cat > "$STUB" <<'STUB_EOF'
#!/usr/bin/env bash
if [ "$1" = "backup" ]; then
  printf '{"path":"%s","samples":%s}\n' "$STUB_DB" "$STUB_SAMPLES"
  exit 0
fi
exit 1
STUB_EOF
chmod +x "$STUB"

make_db() { # path, rowcount, [user_version=9]
  rm -f "$1"
  sqlite3 "$1" "PRAGMA user_version=${3:-9}; CREATE TABLE samples(id TEXT);"
  local n="$2"
  while [ "$n" -gt 0 ]; do sqlite3 "$1" "INSERT INTO samples(id) VALUES('x');"; n=$((n-1)); done
}

# 1. A good snapshot whose row count matches what backup reported → drill passes.
GOOD="$WORK/good.db"; make_db "$GOOD" 3
t "the drill passes on a valid, matching snapshot" \
  env BANSHEE_CLI="$STUB" STUB_DB="$GOOD" STUB_SAMPLES=3 "$DRILL"

# 2. Reported count != actual rows → the copy is not trustworthy → drill FAILS.
t_fails "the drill fails when the row count does not match" \
  env BANSHEE_CLI="$STUB" STUB_DB="$GOOD" STUB_SAMPLES=99 "$DRILL"

# 3. The reported file does not exist → drill FAILS.
t_fails "the drill fails when the snapshot file is missing" \
  env BANSHEE_CLI="$STUB" STUB_DB="$WORK/nope.db" STUB_SAMPLES=3 "$DRILL"

# 4. The snapshot has no samples table (wrong/truncated schema) → drill FAILS.
EMPTY="$WORK/empty.db"; rm -f "$EMPTY"; sqlite3 "$EMPTY" "PRAGMA user_version=9;"
t_fails "the drill fails on a snapshot with no samples table" \
  env BANSHEE_CLI="$STUB" STUB_DB="$EMPTY" STUB_SAMPLES=0 "$DRILL"

# 4b. A snapshot that opens and has a matching row count but NO schema version
#     (user_version=0) is a pre-init or corrupt copy — the version check is the
#     sole guard against restoring it, and this case is what makes that guard a
#     real pin rather than one backstopped by the others.
UNVERSIONED="$WORK/unversioned.db"; make_db "$UNVERSIONED" 2 0
t_fails "the drill fails on a snapshot with no schema version" \
  env BANSHEE_CLI="$STUB" STUB_DB="$UNVERSIONED" STUB_SAMPLES=2 "$DRILL"

# 5. `banshee backup` itself fails (daemon down) → drill FAILS, not skips.
FAILING="$WORK/failing"; printf '#!/usr/bin/env bash\nexit 1\n' > "$FAILING"; chmod +x "$FAILING"
t_fails "the drill fails (not skips) when backup itself fails" \
  env BANSHEE_CLI="$FAILING" "$DRILL"

# 5b. No BANSHEE_CLI: the drill uses the INSTALLED CLI (banshee-xt5), found via
#     service.sh's BANSHEE_INSTALL_DIR, with PATH scrubbed of every `banshee` — the
#     non-interactive shell release.sh actually runs in, where the first real release
#     died with `banshee: command not found`. PATH is scrubbed on purpose: with the
#     interactive PATH (which carries the install dir) this case could pass through
#     PATH and prove nothing about the install-dir branch.
INSTALL="$WORK/install"; mkdir -p "$INSTALL"; /bin/cp -f "$STUB" "$INSTALL/banshee"
NOBANSHEE_PATH="/usr/bin:/bin"
t "with no BANSHEE_CLI the drill finds the installed CLI" \
  env -u BANSHEE_CLI BANSHEE_INSTALL_DIR="$INSTALL" PATH="$NOBANSHEE_PATH" STUB_DB="$GOOD" STUB_SAMPLES=3 "$DRILL"

# 5c. The installed copy beats a `banshee` on PATH: a FAILING banshee first on PATH,
#     the good stub in the install dir. A PATH-first resolution fails this case.
ONPATH="$WORK/onpath"; mkdir -p "$ONPATH"; /bin/cp -f "$FAILING" "$ONPATH/banshee"
t "the installed CLI is preferred over a banshee on PATH" \
  env -u BANSHEE_CLI BANSHEE_INSTALL_DIR="$INSTALL" PATH="$ONPATH:$NOBANSHEE_PATH" STUB_DB="$GOOD" STUB_SAMPLES=3 "$DRILL"

# 5d. Nothing installed, nothing on PATH: the drill FAILS and names the path where
#     the installed CLI should have been, so the fix (make daemon-install) is obvious.
NOINSTALL="$WORK/noinstall"; mkdir -p "$NOINSTALL"
OUT="$(env -u BANSHEE_CLI BANSHEE_INSTALL_DIR="$NOINSTALL" PATH="$NOBANSHEE_PATH" "$DRILL" 2>&1)"; RC=$?
t "with no CLI anywhere the drill fails" test "$RC" -ne 0
t "and names where the installed CLI should have been" bash -c "echo \"\$1\" | grep -q '$NOINSTALL/banshee'" _ "$OUT"

# 5e. PATH is still honoured when nothing is installed (a dev machine that never ran
#     daemon-install but has a symlink on PATH).
t "a banshee on PATH is used when none is installed" \
  env -u BANSHEE_CLI BANSHEE_INSTALL_DIR="$NOINSTALL" PATH="$INSTALL:$NOBANSHEE_PATH" STUB_DB="$GOOD" STUB_SAMPLES=3 "$DRILL"

# 5f. SELF-CONTAINED mode (the build server): no BANSHEE_CLI, nothing installed,
#     nothing on PATH — the drill boots its own throwaway test-profile daemon from
#     the built binaries, waits for a real series, backs up with the matching CLI,
#     and verifies. This is a REAL run against the debug binaries verify.sh has just
#     built (the drill builds them itself if they are absent), because a stub could
#     not prove the daemon boots headless, announces its socket, and is backed up by
#     the CLI over it.
SC_OUT="$(env -u BANSHEE_CLI BANSHEE_INSTALL_DIR="$NOINSTALL" PATH="/usr/bin:/bin:/usr/local/bin:$HOME/.cargo/bin" \
      BANSHEE_DRILL_BIN=rust/target/debug "$DRILL" --self-contained 2>&1)"; SC_RC=$?
t "self-contained: the drill boots a throwaway daemon and verifies its backup" test "$SC_RC" -eq 0
# The drill must wait for a REAL series — the model's 40-sample window — before it
# backs up. A backup of a two-row database would pass every later check and prove
# nothing about the daemon under sustained sampling.
SC_ROWS="$(printf '%s' "$SC_OUT" | sed -n 's/.*with \([0-9]*\) samples$/\1/p' | tail -1)"
t "self-contained: the verified backup holds at least the 40-sample window (got ${SC_ROWS:-?})" \
  test "${SC_ROWS:-0}" -ge 40
t "self-contained: the same, selected by env for release.sh" \
  env -u BANSHEE_CLI BANSHEE_INSTALL_DIR="$NOINSTALL" PATH="/usr/bin:/bin:/usr/local/bin:$HOME/.cargo/bin" \
      BANSHEE_DRILL_BIN=rust/target/debug BANSHEE_DRILL_SELF_CONTAINED=1 "$DRILL"
# A binary dir that does not exist and is not one the drill knows how to build
# must FAIL — not fall back to the installed CLI.
t_fails "self-contained: an unknown BANSHEE_DRILL_BIN fails rather than falling back" \
  env -u BANSHEE_CLI BANSHEE_INSTALL_DIR="$INSTALL" STUB_DB="$GOOD" STUB_SAMPLES=3 \
      BANSHEE_DRILL_BIN="$WORK/no-such-bin" "$DRILL" --self-contained
# The mode leaves nothing running: no banshee-api from a /tmp/banshee-drill.* dir.
t "self-contained: no throwaway daemon survives the drill" \
  bash -c '! pgrep -f "banshee-drill\." >/dev/null'

# 6. release.sh actually WIRES the drill in as a gate (not just that the script
#    exists). A grep is weak on its own, so pair it with the behavioural tests
#    above — together they cover "the drill works" and "the release runs it".
t "release.sh invokes the restore drill" \
  grep -q 'restore-drill.sh' "$SCRIPT_DIR/release.sh"

# 7. The checklist doc exists and names the drill.
t "the release checklist documents the restore drill" \
  grep -qi 'restore drill' "$ROOT_DIR/docs/release-checklist.md"

# ---------------------------------------------------------------------------
# 8. License notices ride in the DMG (banshee-4z1). Static pins on the wiring,
#    same pattern as test-app-bundle.sh: a deleted guard must FAIL here.
# ---------------------------------------------------------------------------
BUILD_DMG="$SCRIPT_DIR/build-dmg.sh"
t "build-dmg stages THIRD-PARTY-NOTICES into the DMG" \
  grep -q 'cp "\$NOTICES" "\$NOTICES_DIR/THIRD-PARTY-NOTICES.html"' "$BUILD_DMG"
t "build-dmg stages the project LICENSE" \
  grep -q 'cp "\$ROOT_DIR/LICENSE" "\$NOTICES_DIR/LICENSE.txt"' "$BUILD_DMG"
t "build-dmg stages Sparkle's LICENSE when the framework is embedded" \
  grep -q 'Sparkle-LICENSE.txt' "$BUILD_DMG"
t "build-dmg fails a real-identity build without SPARKLE_PUBLIC_KEY" \
  bash -c "grep -q 'SPARKLE_PUBLIC_KEY is unset' '$BUILD_DMG'"
# notices staged BEFORE hdiutil runs — ordering is the bug 4z1 exists to stop
t "notices are prepared before the DMG is assembled" bash -c \
  "grep -n 'NOTICES_DIR/THIRD-PARTY-NOTICES.html\|dmgbuild -s' '$BUILD_DMG' | head -2 | paste -sd: - | awk -F: '{ exit !(\$1 < \$3) }'"
# release.sh generates notices BEFORE it builds the DMG
t "release.sh generates notices before building the DMG" bash -c \
  "grep -n 'cargo about generate\|build-dmg.sh' '$SCRIPT_DIR/release.sh' | head -2 | paste -sd: - | awk -F: '{ exit !(\$1 < \$3) }'"

echo "test-release-checklist: $PASS passed, $FAIL failed"
[ "$FAIL" -eq 0 ]
