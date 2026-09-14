#!/usr/bin/env bash
set -u

# test-residue-gate.sh — tests OF scripts/lib/residue-gate.py, verify's last line of
# defence for the one-public-repo design (ADR-0011): a gitignore is a promise, this is
# the mechanism. Two halves, in the version-alignment pattern:
#
#   1. FIXTURES — a throwaway git repo where every check has one file that must be
#      refused and the clean files must pass. A gate that refuses nothing and a gate
#      that refuses everything are both useless, so both directions are pinned, and the
#      private denylist is proven to be read (and to be optional).
#   2. THE REAL TREE — the gate runs over this checkout's `git ls-files` AND every commit
#      message reachable from HEAD, with the private denylist when the notebook is
#      present. This is the step that makes it a verify gate rather than a tool nobody
#      runs.
#
# An adversarial probe of the first version found five ways past it (a denylisted word in
# an exempt file, after a NUL byte in a binary, in a file NAME, in a file that could not
# be read, and in a commit message). Section 6 pins each of them.

SCRIPT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
ROOT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
GATE="$SCRIPT_DIR/lib/residue-gate.py"
PASS=0
FAIL=0
t() { local d="$1"; shift; if "$@" >/dev/null 2>&1; then PASS=$((PASS+1)); else FAIL=$((FAIL+1)); echo "  ✗ $d" >&2; fi; }
t_fails() { local d="$1"; shift; if "$@" >/dev/null 2>&1; then FAIL=$((FAIL+1)); echo "  ✗ $d (succeeded and should not have)" >&2; else PASS=$((PASS+1)); fi; }

WORK="$(mktemp -d /tmp/banshee-residue-test.XXXXXX)"
trap 'rm -rf "$WORK"' EXIT
export GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_NOSYSTEM=1
export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@x GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@x

# ---------------------------------------------------------------------------
# A fixture repo. `add <path> <content>` writes and stages one file; the gate reads
# `git ls-files`, so a staged file is a tracked file.
# ---------------------------------------------------------------------------
REPO="$WORK/repo"; git init -q "$REPO" -b main
add() { mkdir -p "$REPO/$(dirname "$1")"; printf '%s\n' "$2" > "$REPO/$1"; git -C "$REPO" add -f "$1"; }
gate() { python3 "$GATE" --root "$REPO" "$@"; }
# Findings go to stderr; the summary to stdout. Capture both, separately, for assertions.
run() { # [args…] — sets OUT (stdout), ERR (stderr), RC
  OUT="$(gate "$@" 2>"$WORK/err")"; RC=$?; ERR="$(cat "$WORK/err")"
}

# --- 1. A clean tree passes: every look-alike the gate must NOT refuse -----------------
add "README.md" "Install from GitHub Releases. Fixtures live under /Users/example/Library and the build user is /Users/builder/repos."
add "docs/runbook.md" "ssh builder@<build-host>; the dev machine pushes with git push mbp main; loopback is 127.0.0.1 and the documentation address is 192.0.2.1."
add "scripts/tools.sh" 'git config user.email "8061019+someone@users.noreply.github.com"; git remote add origin git@github.com:someone/banshee.git'
add "src/lib.rs" '// a phantom process is a corporate fixture; contact team@example.com; see banshee-abc for the bead'
# A clean binary file. Written directly: a shell variable cannot hold a NUL byte, so
# `add` with a $(printf …) would silently make this a TEXT fixture.
mkdir -p "$REPO/assets"; printf 'binary\0with\0nuls and /Users/example inside' > "$REPO/assets/blob.bin"; git -C "$REPO" add -f assets/blob.bin
run --no-denylist
t "a clean tree passes (exit 0)" test "$RC" -eq 0
t "…with no findings on stderr" test -z "$ERR"
t "…and a summary naming the file count" bash -c "printf '%s' \"\$1\" | grep -q 'residue-gate: 5 file(s)'" _ "$OUT"
t "…that says the denylist was skipped" bash -c "printf '%s' \"\$1\" | grep -q 'denylist: none'" _ "$OUT"

# --- 2. Each generic check refuses exactly its file, with path:line ----------------------
add "docs/lan.md" "the runner lives at 10.0.20.30 on the LAN"
run --no-denylist
t "an RFC 1918 address is refused" test "$RC" -eq 1
t "…and the finding names the file and line" bash -c "printf '%s' \"\$1\" | grep -q '^docs/lan.md:1: '" _ "$ERR"
git -C "$REPO" rm -q --cached docs/lan.md

add "docs/home.md" "copy it to /Users/alice/Downloads first"
run --no-denylist; t "a real user's home path is refused" test "$RC" -eq 1; git -C "$REPO" rm -q --cached docs/home.md
add "docs/tilde.md" 'clone into ~/Code/thing'
run --no-denylist; t "a ~/Code path is refused" test "$RC" -eq 1; git -C "$REPO" rm -q --cached docs/tilde.md
add "docs/mail.md" "ask someone@corp-internal.test about it"
run --no-denylist; t "a non-noreply e-mail address is refused" test "$RC" -eq 1; git -C "$REPO" rm -q --cached docs/mail.md

# --- 3. Notebook paths are refused by NAME, whatever they contain --------------------------
for p in "private/notes.md" "HANDOFF.md" ".beads/issues.jsonl" "docs/research/plan.md" "Config" ".template-stamp.toml" "scripts/release.conf"; do
  add "$p" "perfectly innocent text"
  run --no-denylist
  t "a tracked $p is refused" test "$RC" -eq 1
  t "…as a path finding (line 0)" bash -c "printf '%s' \"\$1\" | grep -q '^$p:0: path belongs in the private notebook'" _ "$ERR"
  git -C "$REPO" rm -q --cached "$p"
done

# --- 4. The private denylist: read when named or present, optional otherwise ----------------
add "docs/vendor.md" "the Acme Widgets agent eats a core"
DENY="$WORK/deny.txt"; printf '# vendors\n\\bacme widgets\\b\n\nnot-a-match-either\n' > "$DENY"
run --no-denylist
t "without a denylist the vendor name passes (generic checks only)" test "$RC" -eq 0
run --denylist "$DENY"
t "with --denylist the vendor name is refused" test "$RC" -eq 1
t "…case-insensitively, naming the pattern" bash -c "printf '%s' \"\$1\" | grep -q 'docs/vendor.md:1: denylisted term: matches \\\\bacme widgets\\\\b'" _ "$ERR"
t "…and the summary names the denylist and its size" bash -c "printf '%s' \"\$1\" | grep -q 'denylist: .*deny.txt (2 patterns)'" _ "$OUT"
# The default location is <root>/private/residue-denylist.txt — present ⇒ used, silently.
mkdir -p "$REPO/private"; /bin/cp -f "$DENY" "$REPO/private/residue-denylist.txt"
run
t "private/residue-denylist.txt is picked up by default" test "$RC" -eq 1
t "…and the summary says which file it read" bash -c "printf '%s' \"\$1\" | grep -q 'denylist: private/residue-denylist.txt (2 patterns)'" _ "$OUT"
rm -rf "$REPO/private"
run --denylist "$WORK/missing.txt"
t "a named denylist that does not exist is a usage error (exit 2), not a silent skip" test "$RC" -eq 2
printf '[unclosed\n' > "$WORK/bad.txt"
run --denylist "$WORK/bad.txt"
t "a denylist with a bad regex is an error, not a silent skip" test "$RC" -ne 0
git -C "$REPO" rm -q --cached docs/vendor.md

# --- 5. Explicit file arguments gate only those files ---------------------------------------
add "docs/lan2.md" "10.1.1.1 again"
run --no-denylist README.md
t "an explicit file list is honoured (the dirty file is not looked at)" test "$RC" -eq 0
run --no-denylist docs/lan2.md
t "…and names the dirty file when asked" test "$RC" -eq 1
git -C "$REPO" rm -q --cached docs/lan2.md

# --- 6. The five gaps an adversarial probe found in the first version -----------------------
DENY2="$WORK/deny2.txt"; printf '\\bzorblatt\\b\n' > "$DENY2"
# (a) a denylisted word in an EXEMPT file: exempt from the generic checks only.
mkdir -p "$REPO/scripts/lib"; add "scripts/lib/residue-gate.py" "# a copy of the gate that mentions zorblatt"
run --denylist "$DENY2"
t "the private denylist applies to the gate's own (exempt) files" test "$RC" -eq 1
git -C "$REPO" rm -q --cached scripts/lib/residue-gate.py
# (b) a denylisted word after a NUL byte in a binary.
printf 'PNG\0\0\0 zorblatt \0\0' > "$REPO/assets/dirty.bin"; git -C "$REPO" add -f assets/dirty.bin
run --denylist "$DENY2"
t "binary content is searched byte for byte" test "$RC" -eq 1
t "…and reported as binary content" bash -c "printf '%s' \"\$1\" | grep -q 'assets/dirty.bin:0: binary content: denylisted term'" _ "$ERR"
git -C "$REPO" rm -q --cached assets/dirty.bin
printf 'PNG\0\0\0 10.9.9.9 \0\0' > "$REPO/assets/dirty2.bin"; git -C "$REPO" add -f assets/dirty2.bin
run --no-denylist
t "…for the generic checks too" test "$RC" -eq 1
git -C "$REPO" rm -q --cached assets/dirty2.bin
# (c) a denylisted word in a file NAME.
add "docs/zorblatt-notes.md" "nothing to see in the content"
run --denylist "$DENY2"
t "a denylisted word in a file name is refused" test "$RC" -eq 1
t "…as a path finding" bash -c "printf '%s' \"\$1\" | grep -q '^docs/zorblatt-notes.md:0: file name matches'" _ "$ERR"
git -C "$REPO" rm -q --cached docs/zorblatt-notes.md
# (d) a file the gate cannot read is an ERROR, never clean.
run --no-denylist docs/does-not-exist.md
t "an unreadable explicit file is exit 2, not clean" test "$RC" -eq 2
# (e) commit messages reachable from HEAD.
git -C "$REPO" commit -q -m "clean commit message" --allow-empty
run --no-denylist --messages
t "a clean history passes --messages" test "$RC" -eq 0
t "…and the summary counts the messages" bash -c "printf '%s' \"\$1\" | grep -q '1 commit message(s)'" _ "$OUT"
git -C "$REPO" commit -q -m "this mentions zorblatt and 10.0.20.30 in the body" --allow-empty
run --no-denylist --messages
t "an RFC 1918 address in a commit message is refused" test "$RC" -eq 1
t "…naming the commit and line" bash -c "printf '%s' \"\$1\" | grep -qE '^commit [0-9a-f]{12}:1: private residue'" _ "$ERR"
run --denylist "$DENY2" --messages
t "a denylisted word in a commit message is refused" bash -c "printf '%s' \"\$1\" | grep -q 'denylisted term: matches \\\\bzorblatt\\\\b'" _ "$ERR"
run --no-denylist
t "…and files-only mode does not look at messages" test "$RC" -eq 0

# ---------------------------------------------------------------------------
# 7. THE REAL TREE. This is the gate. private/residue-denylist.txt is read when the
#    notebook is present (the maintainer's machine); a fresh clone or the CI runner
#    gets the generic checks and a summary that says so. Commit messages reachable from
#    HEAD are gated too: a message that recounts what a commit removed publishes it.
# ---------------------------------------------------------------------------
if REAL="$(python3 "$GATE" --root "$ROOT_DIR" --messages 2>"$WORK/real.err")"; then
  PASS=$((PASS+1))
  printf '  %s\n' "$REAL"
else
  FAIL=$((FAIL+1))
  echo "  ✗ the real tree carries private residue:" >&2
  sed 's/^/      /' "$WORK/real.err" >&2
  printf '  %s\n' "$REAL"
fi

echo "test-residue-gate: $PASS passed, $FAIL failed"
[ "$FAIL" -eq 0 ]
