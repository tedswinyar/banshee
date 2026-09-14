# Shared marker matching for the git pre-push hook chain.
# Sourced by scripts/install-hooks.sh and scripts/check-hooks.sh; not
# executable on its own.
#
# The beads block is matched BY PREFIX, never by an exact version. `bd hooks
# install` writes "# --- BEGIN BEADS INTEGRATION v<version> ---" using
# whatever bd version is installed, so pinning one version makes check-hooks
# report a missing block after any bd upgrade (learned in another project).
#
# The gate markers stay exact strings: install-hooks.sh owns that block.
#
# A version token is one or more NON-WHITESPACE characters ([^[:space:]], not
# [^ ] — a tab inside a corrupted marker must not match). The ERE matchers
# and the BRE sed extractors below are a matched pair on purpose: mixing `+`
# in grep with `*` in sed once made them accept different marker sets.

# ERE, for grep -E and awk. Version segment optional so a future bd that
# drops it still matches.
BANSHEE_BEADS_BEGIN_RE='^# --- BEGIN BEADS INTEGRATION( v[^[:space:]]+)? ---$'
BANSHEE_BEADS_END_RE='^# --- END BEADS INTEGRATION( v[^[:space:]]+)? ---$'
BANSHEE_GATE_BEGIN="# --- BEGIN BANSHEE VERIFY GATE v1 ---"
BANSHEE_GATE_END="# --- END BANSHEE VERIFY GATE v1 ---"

# Human-readable form of what the beads matchers accept, for error messages.
BANSHEE_BEADS_BEGIN_DESC="# --- BEGIN BEADS INTEGRATION v<version> ---"
BANSHEE_BEADS_END_DESC="# --- END BEADS INTEGRATION v<version> ---"

# Count whole lines in $2 exactly equal to the fixed string $1.
# Prints 0 for a missing or unreadable file.
banshee_count_marker() {
  banshee_count_result="$(grep -Fxc "$1" "$2" 2>/dev/null)" || banshee_count_result=0
  printf '%s\n' "$banshee_count_result"
}

# Count lines in $2 matching the extended regex $1.
banshee_count_marker_re() {
  banshee_count_result="$(grep -Ec "$1" "$2" 2>/dev/null)" || banshee_count_result=0
  printf '%s\n' "$banshee_count_result"
}

banshee_beads_begin_count() {
  banshee_count_marker_re "$BANSHEE_BEADS_BEGIN_RE" "$1"
}

banshee_beads_end_count() {
  banshee_count_marker_re "$BANSHEE_BEADS_END_RE" "$1"
}

# Print the version carried by the beads BEGIN marker in $1, or the empty
# string when the marker is unversioned or absent. BRE twin of the ERE above:
# `\{1,\}` matches the ERE `+`, so the invalid empty form `v ---` is rejected
# by both.
banshee_beads_begin_version() {
  sed -n 's/^# --- BEGIN BEADS INTEGRATION v\([^[:space:]]\{1,\}\) ---$/\1/p' "$1" 2>/dev/null
}

banshee_beads_end_version() {
  sed -n 's/^# --- END BEADS INTEGRATION v\([^[:space:]]\{1,\}\) ---$/\1/p' "$1" 2>/dev/null
}

# Print a display label for the beads block in $1: "v<version>", or the bare
# word "unversioned". Never fabricates a "v" prefix for a version that does
# not exist.
banshee_beads_version_label() {
  banshee_label_version="$(banshee_beads_begin_version "$1")"
  if [ -n "$banshee_label_version" ]; then
    printf 'v%s\n' "$banshee_label_version"
  elif [ "$(banshee_beads_begin_count "$1")" != "0" ]; then
    printf 'unversioned\n'
  fi
}

# Print the bytes of the beads block in $1, BEGIN through END inclusive.
# Prints nothing when there is no BEGIN. When a BEGIN has no matching END the
# remainder of the file is printed — the conservative choice: a caller
# comparing before/after still notices any edit inside that span.
#
# This is what a preservation assertion must compare. Marker COUNTS are not
# sufficient: a strip pass can delete lines between two surviving markers, so
# counts stay 1/1 while the payload is gone.
banshee_beads_block_bytes() {
  awk -v begin_re="$BANSHEE_BEADS_BEGIN_RE" -v end_re="$BANSHEE_BEADS_END_RE" '
    !started && $0 ~ begin_re { started = 1; print; next }
    started && !finished && $0 ~ end_re { finished = 1; print; next }
    started && !finished { print }
  ' "$1" 2>/dev/null
}

# Classify the beads block in $1. Prints exactly one of:
#   absent    - no beads markers at all
#   ok        - exactly one BEGIN and one END, carrying the same version
#   mismatch  - one BEGIN and one END, but their versions disagree
#   malformed - any other count (BEGIN without END, duplicates, ...)
banshee_beads_block_status() {
  banshee_status_begin_n="$(banshee_beads_begin_count "$1")"
  banshee_status_end_n="$(banshee_beads_end_count "$1")"

  if [ "$banshee_status_begin_n" = "0" ] && [ "$banshee_status_end_n" = "0" ]; then
    printf 'absent\n'
    return 0
  fi
  if [ "$banshee_status_begin_n" != "1" ] || [ "$banshee_status_end_n" != "1" ]; then
    printf 'malformed\n'
    return 0
  fi
  if [ "$(banshee_beads_begin_version "$1")" != "$(banshee_beads_end_version "$1")" ]; then
    printf 'mismatch\n'
    return 0
  fi
  printf 'ok\n'
}
