#!/usr/bin/env bash
set -euo pipefail

# capture-screens.sh — draw every screen to a PNG, light and dark, without
# launching the app (banshee-v54.5).
#
# The views are drawn OFFSCREEN by the test suite
# (swift/Tests/BansheeUITests/ScreenRenderTests.swift) from the shared wire
# fixtures. Nothing is launched: no app, no daemon, no fixture server, no socket,
# no menu bar item, no screen. So this runs on a headless build server, and it is
# how a visual change gets LOOKED AT — not by building the app and opening it on
# somebody's desktop.
#
# Usage:
#   scripts/capture-screens.sh [--dry-run] [--prune-siblings N] [out-dir]
#
#   out-dir             where the PNGs go. Default: a fresh directory under
#                       $TMPDIR, so two runs never write to the same place.
#   --dry-run           print the output directory and the command, run nothing.
#   --prune-siblings N  after a successful capture, keep only the newest N
#                       sibling directories of out-dir whose names are 40-hex
#                       commit ids (the CI layout, <root>/<sha>/), and delete
#                       the older ones. Anything not named like a commit is
#                       never touched.
#
# Running several at once: the drawing itself shares nothing — each run is one
# process writing only to its own out-dir. What two runs in the SAME checkout do
# share is SwiftPM's build directory, and two builds there corrupt each other
# (it is why verify.sh takes a lock). Give each concurrent run in one checkout
# its own build directory with BANSHEE_CAPTURE_SCRATCH=<dir>; runs in different
# checkouts (one per CI runner) need nothing.
#
# Exit 0 only if the render tests passed AND the expected images exist.

ROOT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
# Eleven screens, two appearances. Raise this when a screen is added to
# ScreenRenderTests.screens(); a capture that silently drew fewer is a failure.
EXPECTED=22

dry_run=0
prune=""
out=""
while [ $# -gt 0 ]; do
  case "$1" in
    --dry-run) dry_run=1 ;;
    --prune-siblings)
      shift
      [ $# -gt 0 ] || { echo "capture-screens: --prune-siblings needs a number" >&2; exit 2; }
      prune="$1"
      case "$prune" in ''|*[!0-9]*) echo "capture-screens: --prune-siblings needs a number, got '$prune'" >&2; exit 2 ;; esac
      [ "$prune" -ge 1 ] || { echo "capture-screens: --prune-siblings must keep at least 1" >&2; exit 2; }
      ;;
    -h|--help) sed -n '4,34p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    -*) echo "capture-screens: unknown option $1" >&2; exit 2 ;;
    *)
      [ -z "$out" ] || { echo "capture-screens: one out-dir, got '$out' and '$1'" >&2; exit 2; }
      out="$1"
      ;;
  esac
  shift
done

if [ -z "$out" ]; then
  out="$(mktemp -d "${TMPDIR:-/tmp}/banshee-captures.XXXXXX")"
fi
case "$out" in /*) : ;; *) out="$PWD/$out" ;; esac

cmd=(swift test --filter ScreenRenderTests)
if [ -n "${BANSHEE_CAPTURE_SCRATCH:-}" ]; then
  cmd+=(--scratch-path "$BANSHEE_CAPTURE_SCRATCH")
fi

if [ "$dry_run" = 1 ]; then
  echo "out-dir: $out"
  echo "command: (cd $ROOT_DIR/swift && BANSHEE_CAPTURE_DIR=$out ${cmd[*]})"
  exit 0
fi

mkdir -p "$out"
log="$out/capture.log"
# Captured to a file and judged from the exit status and the files on disk — never
# through a pipe, whose status would be the last command's.
rc=0
(cd "$ROOT_DIR/swift" && BANSHEE_CAPTURE_DIR="$out" "${cmd[@]}") > "$log" 2>&1 || rc=$?
if [ "$rc" -ne 0 ]; then
  echo "capture-screens: the render tests FAILED (exit $rc); last lines of $log:" >&2
  tail -20 "$log" >&2
  exit 1
fi

count=0
for f in "$out"/*.png; do
  [ -e "$f" ] && count=$((count + 1))
done
if [ "$count" -lt "$EXPECTED" ]; then
  echo "capture-screens: expected at least $EXPECTED images in $out, found $count" >&2
  exit 1
fi
echo "capture-screens: $count images in $out"

if [ -n "$prune" ]; then
  parent="$(dirname "$out")"
  kept=0
  # Newest first. Only 40-hex directory names are candidates.
  while IFS= read -r dir; do
    name="$(basename "$dir")"
    case "$name" in
      *[!0-9a-f]*) continue ;;
    esac
    [ "${#name}" -eq 40 ] || continue
    kept=$((kept + 1))
    if [ "$kept" -gt "$prune" ]; then
      rm -rf "$dir"
      echo "capture-screens: pruned $name"
    fi
  done < <(ls -1dt "$parent"/*/ 2>/dev/null | sed 's#/$##')
fi
