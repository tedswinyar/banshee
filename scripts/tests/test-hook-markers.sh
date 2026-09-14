#!/usr/bin/env bash
set -u

# Tests for the hook machinery: the marker lib's classification logic and
# install-hooks.sh's preservation/rollback guarantees. All work happens in a
# sandbox git repo under mktemp — the template repo's own hooks are never
# touched.

SCRIPT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
PASS=0
FAIL=0

t() {
  # t <description> <command...> — command must succeed
  local desc="$1"
  shift
  if "$@" >/dev/null 2>&1; then
    PASS=$((PASS + 1))
  else
    FAIL=$((FAIL + 1))
    echo "  ✗ $desc" >&2
  fi
}

t_eq() {
  # t_eq <description> <expected> <actual>
  local desc="$1" expected="$2" actual="$3"
  if [ "$expected" = "$actual" ]; then
    PASS=$((PASS + 1))
  else
    FAIL=$((FAIL + 1))
    echo "  ✗ $desc (expected: $expected, actual: $actual)" >&2
  fi
}

# shellcheck source=../lib/hook-markers.sh
. "$SCRIPT_DIR/lib/hook-markers.sh"

WORK="$(mktemp -d /tmp/banshee-hook-tests.XXXXXX)"
trap 'rm -rf "$WORK"' EXIT

# ---------------------------------------------------------------------------
# Marker lib: classification
# ---------------------------------------------------------------------------
printf '#!/bin/sh\necho hi\n' > "$WORK/no-markers"
t_eq "no markers classifies absent" "absent" "$(banshee_beads_block_status "$WORK/no-markers")"

cat > "$WORK/ok-block" <<'EOF'
#!/bin/sh
# --- BEGIN BEADS INTEGRATION v1.2.1 ---
bd sync
# --- END BEADS INTEGRATION v1.2.1 ---
EOF
t_eq "matched pair classifies ok" "ok" "$(banshee_beads_block_status "$WORK/ok-block")"
t_eq "begin version extracted" "1.2.1" "$(banshee_beads_begin_version "$WORK/ok-block")"
t_eq "version label" "v1.2.1" "$(banshee_beads_version_label "$WORK/ok-block")"

cat > "$WORK/mismatch" <<'EOF'
# --- BEGIN BEADS INTEGRATION v1.2.1 ---
bd sync
# --- END BEADS INTEGRATION v9.9.9 ---
EOF
t_eq "version disagreement classifies mismatch" "mismatch" "$(banshee_beads_block_status "$WORK/mismatch")"

cat > "$WORK/malformed" <<'EOF'
# --- BEGIN BEADS INTEGRATION v1.2.1 ---
bd sync
EOF
t_eq "begin without end classifies malformed" "malformed" "$(banshee_beads_block_status "$WORK/malformed")"

cat > "$WORK/unversioned" <<'EOF'
# --- BEGIN BEADS INTEGRATION ---
bd sync
# --- END BEADS INTEGRATION ---
EOF
t_eq "unversioned pair classifies ok" "ok" "$(banshee_beads_block_status "$WORK/unversioned")"
t_eq "unversioned label" "unversioned" "$(banshee_beads_version_label "$WORK/unversioned")"

# A tab inside the version token must NOT match ([^[:space:]], not [^ ]).
printf '# --- BEGIN BEADS INTEGRATION v1.2\t1 ---\nx\n# --- END BEADS INTEGRATION v1.2\t1 ---\n' > "$WORK/tab-version"
t_eq "tab in version token rejected" "absent" "$(banshee_beads_block_status "$WORK/tab-version")"

# Block bytes: strip pass that deletes payload between markers must be visible.
BEFORE="$(banshee_beads_block_bytes "$WORK/ok-block")"
sed '/bd sync/d' "$WORK/ok-block" > "$WORK/ok-block-gutted"
AFTER="$(banshee_beads_block_bytes "$WORK/ok-block-gutted")"
t "gutted block bytes differ from original" [ "$BEFORE" != "$AFTER" ]

# ---------------------------------------------------------------------------
# install-hooks.sh in a sandbox repo
# ---------------------------------------------------------------------------
SANDBOX="$WORK/repo"
mkdir -p "$SANDBOX"
git -C "$SANDBOX" init -q
mkdir -p "$SANDBOX/scripts"
cp -R "$SCRIPT_DIR/lib" "$SANDBOX/scripts/"
cp "$SCRIPT_DIR/install-hooks.sh" "$SCRIPT_DIR/check-hooks.sh" "$SANDBOX/scripts/"
# A stub verify.sh so the gate block's target exists.
printf '#!/bin/sh\nexit 0\n' > "$SANDBOX/scripts/verify.sh"
chmod +x "$SANDBOX/scripts/"*.sh
HOOK="$SANDBOX/.git/hooks/pre-push"

# Fresh install into a repo with no existing hook.
t "install into fresh repo succeeds" \
  env -C "$SANDBOX" ./scripts/install-hooks.sh
t "hook file exists and is executable" test -x "$HOOK"
t_eq "gate BEGIN present once" "1" "$(banshee_count_marker "$BANSHEE_GATE_BEGIN" "$HOOK")"

# Idempotence: reinstall must not duplicate the gate.
t "reinstall succeeds" env -C "$SANDBOX" ./scripts/install-hooks.sh
t_eq "gate BEGIN still present once after reinstall" "1" \
  "$(banshee_count_marker "$BANSHEE_GATE_BEGIN" "$HOOK")"

# Beads preservation: plant a beads block, reinstall, block must survive
# byte-for-byte.
cat > "$HOOK" <<'EOF'
#!/usr/bin/env sh
# --- BEGIN BEADS INTEGRATION v1.5.0 ---
bd sync --pre-push
bd flush
# --- END BEADS INTEGRATION v1.5.0 ---
EOF
chmod 755 "$HOOK"
BEADS_BEFORE="$(banshee_beads_block_bytes "$HOOK")"
t "install alongside beads block succeeds" env -C "$SANDBOX" ./scripts/install-hooks.sh
t_eq "beads block preserved byte-for-byte" "$BEADS_BEFORE" "$(banshee_beads_block_bytes "$HOOK")"
t_eq "gate added exactly once alongside beads" "1" \
  "$(banshee_count_marker "$BANSHEE_GATE_BEGIN" "$HOOK")"

# Corrupt beads block: install must refuse and leave the hook unchanged.
cat > "$HOOK" <<'EOF'
#!/usr/bin/env sh
# --- BEGIN BEADS INTEGRATION v1.5.0 ---
bd sync
# --- END BEADS INTEGRATION v9.9.9 ---
EOF
chmod 755 "$HOOK"
CORRUPT_BYTES="$(cat "$HOOK")"
t "install refuses corrupt beads block" \
  bash -c "! (cd '$SANDBOX' && ./scripts/install-hooks.sh)"
t_eq "corrupt hook left unchanged" "$CORRUPT_BYTES" "$(cat "$HOOK")"

# ---------------------------------------------------------------------------
# check-hooks.sh: gate-body tamper detection
# ---------------------------------------------------------------------------
rm -f "$HOOK" "$SANDBOX/.git/hooks/pre-push.banshee-backup-"* 2>/dev/null
( cd "$SANDBOX" && ./scripts/install-hooks.sh >/dev/null 2>&1 )
t "check-hooks passes on a clean install" env -C "$SANDBOX" ./scripts/check-hooks.sh

# Hand-edit the gate body between the markers — markers survive, body lies.
sed -i '' 's|"$_banshee_repo_root/scripts/verify.sh"$|true|' "$HOOK"
t "check-hooks detects a hand-edited gate body" \
  bash -c "! (cd '$SANDBOX' && ./scripts/check-hooks.sh)"

# ---------------------------------------------------------------------------
echo "test-hook-markers: $PASS passed, $FAIL failed"
[ "$FAIL" -eq 0 ]
