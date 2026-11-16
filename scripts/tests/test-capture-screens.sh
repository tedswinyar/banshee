#!/usr/bin/env bash
set -u

# test-capture-screens.sh — tests OF scripts/capture-screens.sh's own logic: where
# it writes, what it refuses, and what --prune-siblings may delete. The drawing is
# the Swift suite's job (ScreenRenderTests) and is not re-run here; these drive the
# script with --dry-run, or against a copy whose `swift` is a stub, so the scripts
# suite never starts a second build.
#
# The properties that matter:
#   * two runs with no out-dir get DIFFERENT directories — "several runs at once"
#     depends on it;
#   * a run that draws fewer images than expected FAILS, rather than reporting a
#     short capture as a capture;
#   * a failed render is a failure, with the log shown;
#   * --prune-siblings deletes only old directories named like a commit id, and
#     never the one just written.

SCRIPT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
PASS=0
FAIL=0
t() { local d="$1"; shift; if "$@" >/dev/null 2>&1; then PASS=$((PASS+1)); else FAIL=$((FAIL+1)); echo "  ✗ $d" >&2; fi; }

WORK="$(mktemp -d /tmp/banshee-capture-test.XXXXXX)"
trap 'rm -rf "$WORK"' EXIT

CAP="$SCRIPT_DIR/capture-screens.sh"

# 1. No out-dir: a fresh directory each time.
a="$(bash "$CAP" --dry-run | sed -n 's/^out-dir: //p')"
b="$(bash "$CAP" --dry-run | sed -n 's/^out-dir: //p')"
t "a default out-dir is produced" test -n "$a"
t "two default runs do not share an out-dir" test "$a" != "$b"
rmdir "$a" "$b" 2>/dev/null || true

# 2. An explicit out-dir is honoured, and made absolute.
got="$(cd "$WORK" && bash "$CAP" --dry-run shots | sed -n 's/^out-dir: //p')"
t "a relative out-dir is resolved against the caller's directory" test "$got" = "$WORK/shots"

# 3. Bad arguments are refused with exit 2.
bash "$CAP" --dry-run --nope >/dev/null 2>&1; t "an unknown option is refused" test "$?" = 2
bash "$CAP" --dry-run one two >/dev/null 2>&1; t "two out-dirs are refused" test "$?" = 2
bash "$CAP" --dry-run --prune-siblings many >/dev/null 2>&1; t "--prune-siblings needs a number" test "$?" = 2
bash "$CAP" --dry-run --prune-siblings 0 >/dev/null 2>&1; t "--prune-siblings 0 is refused" test "$?" = 2

# 4. The real run, against a copy whose `swift` is a stub that "draws" N images.
REPO="$WORK/repo"
mkdir -p "$REPO/scripts" "$REPO/swift" "$WORK/bin"
/bin/cp -f "$CAP" "$REPO/scripts/capture-screens.sh"
cat > "$WORK/bin/swift" <<'STUB'
#!/usr/bin/env bash
# A stand-in for `swift test`: writes $STUB_IMAGES empty PNGs, exits $STUB_RC.
i=0
while [ "$i" -lt "${STUB_IMAGES:-0}" ]; do
  : > "$BANSHEE_CAPTURE_DIR/screen-$i.png"
  i=$((i + 1))
done
echo "stub swift ran: $*"
exit "${STUB_RC:-0}"
STUB
chmod +x "$WORK/bin/swift"
run() { # env... -> $RC, output in $OUT
  OUT="$(env PATH="$WORK/bin:$PATH" "$@" bash "$REPO/scripts/capture-screens.sh" "$TARGET" 2>&1)"; RC=$?
}

TARGET="$WORK/out-full"
run STUB_IMAGES=22
t "a full capture succeeds" test "$RC" = 0
t "and says how many images it wrote" grep -q '22 images' <<< "$OUT"

TARGET="$WORK/out-short"
run STUB_IMAGES=21
t "a capture one image short FAILS" test "$RC" = 1
t "and says what it expected" grep -q 'expected at least 22' <<< "$OUT"

TARGET="$WORK/out-red"
run STUB_IMAGES=22 STUB_RC=1
t "a failed render is a failure even with every image present" test "$RC" = 1
t "and shows the log" grep -q 'stub swift ran' <<< "$OUT"

# 5. --prune-siblings: only old commit-named directories go.
ROOT="$WORK/captures"
old1="$ROOT/1111111111111111111111111111111111111111"
old2="$ROOT/2222222222222222222222222222222222222222"
new="$ROOT/3333333333333333333333333333333333333333"
mkdir -p "$old1" "$old2" "$ROOT/notes" "$ROOT/abc"
touch -t 202601010000 "$old1"
touch -t 202602010000 "$old2"
touch -t 202601010000 "$ROOT/notes" "$ROOT/abc"
OUT="$(env PATH="$WORK/bin:$PATH" STUB_IMAGES=22 bash "$REPO/scripts/capture-screens.sh" --prune-siblings 2 "$new" 2>&1)"; RC=$?
t "a pruning capture succeeds" test "$RC" = 0
t "the capture just written is kept" test -d "$new"
t "the newer old capture is kept (2 kept)" test -d "$old2"
t "the oldest capture is pruned" test ! -e "$old1"
t "a directory not named like a commit is never touched" test -d "$ROOT/notes"
t "nor a short hex name" test -d "$ROOT/abc"

echo "test-capture-screens: $PASS passed, $FAIL failed"
[ "$FAIL" -eq 0 ]
