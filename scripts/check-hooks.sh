#!/usr/bin/env sh
# Assert the local git hook baseline that keeps the hook chain (beads where in
# use, and the Banshee verify gate) intact.
#
# The beads block is required only when the repo actually uses beads (a .beads/
# directory exists).

banshee_lib_dir="$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)/lib"
if [ ! -r "$banshee_lib_dir/hook-markers.sh" ]; then
  printf '%s\n' "banshee hooks: ERROR: missing $banshee_lib_dir/hook-markers.sh" >&2
  exit 1
fi
# shellcheck source=lib/hook-markers.sh
. "$banshee_lib_dir/hook-markers.sh"

banshee_error() {
  printf '%s\n' "banshee hooks: ERROR: $*" >&2
}

banshee_info() {
  printf '%s\n' "banshee hooks: $*"
}

banshee_fail=0

banshee_repo_root="$(git rev-parse --show-toplevel 2>/dev/null || true)"
if [ -z "$banshee_repo_root" ]; then
  banshee_error "not inside a git working tree"
  exit 1
fi

banshee_git_common_dir="$(git -C "$banshee_repo_root" rev-parse --git-common-dir 2>/dev/null || true)"
if [ -z "$banshee_git_common_dir" ]; then
  banshee_error "could not resolve git common directory"
  exit 1
fi

case "$banshee_git_common_dir" in
  /*) banshee_hook_file="$banshee_git_common_dir/hooks/pre-push" ;;
  *) banshee_hook_file="$banshee_repo_root/$banshee_git_common_dir/hooks/pre-push" ;;
esac

# --------------------------------------------------------------------------
# .beads/hooks must stay EMPTY: bd's real hook shims belong in .git/hooks, so
# anything here means a core.hooksPath override that would shadow the verify
# gate. (Some endpoint-security agents' daemons write binaries here, which is
# the scar this guards against; the check is generically useful.)
# --------------------------------------------------------------------------
if [ -d "$banshee_repo_root/.beads/hooks" ]; then
  banshee_beads_hooks_files="$(find "$banshee_repo_root/.beads/hooks" -type f 2>/dev/null | head -5)"
  if [ -n "$banshee_beads_hooks_files" ]; then
    banshee_error ".beads/hooks contains files; it must be empty:"
    printf '%s\n' "$banshee_beads_hooks_files" | sed 's/^/banshee hooks:   /' >&2
    banshee_error "fix: rm -f .beads/hooks/*; git config --local core.hooksPath .git/hooks;"
    banshee_error "  bd hooks install; git config --local --unset core.hooksPath; ./scripts/install-hooks.sh"
    banshee_fail=1
  fi
fi

# --------------------------------------------------------------------------
# pre-push hook contents
# --------------------------------------------------------------------------
if [ ! -f "$banshee_hook_file" ]; then
  banshee_error "repo-local pre-push hook is missing: $banshee_hook_file"
  banshee_error "run ./scripts/install-hooks.sh"
  banshee_fail=1
else
  # Beads block: OPTIONAL (bd >= 1.2 does not write text blocks into
  # .git/hooks), but if one exists it must be intact — a corrupt block
  # means something ate beads-owned hook data.
  banshee_beads_status="$(banshee_beads_block_status "$banshee_hook_file")"
  case "$banshee_beads_status" in
    ok|absent)
      ;;
    mismatch)
      banshee_error "beads hook block BEGIN and END markers carry different versions in $banshee_hook_file"
      banshee_error "  BEGIN v$(banshee_beads_begin_version "$banshee_hook_file")"
      banshee_error "  END   v$(banshee_beads_end_version "$banshee_hook_file")"
      banshee_error "  both ends must agree; the block looks corrupt"
      banshee_fail=1
      ;;
    *)
      banshee_error "beads hook block is malformed in $banshee_hook_file"
      banshee_error "  found BEGIN x$(banshee_beads_begin_count "$banshee_hook_file"), END x$(banshee_beads_end_count "$banshee_hook_file"); expected exactly one of each"
      banshee_fail=1
      ;;
  esac

  if ! grep -Fqx "$BANSHEE_GATE_BEGIN" "$banshee_hook_file" || ! grep -Fqx "$BANSHEE_GATE_END" "$banshee_hook_file"; then
    banshee_error "verify gate block is missing or incomplete in $banshee_hook_file"
    banshee_error "run ./scripts/install-hooks.sh (again after bd hooks install rewrites hooks)"
    banshee_fail=1
  else
    # A marker proves only that a marker exists. The BODY between the markers
    # is what actually runs verify.sh; compare it byte-for-byte against the
    # canonical block install-hooks.sh would write today.
    banshee_script_dir="$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)"
    if [ ! -x "$banshee_script_dir/install-hooks.sh" ]; then
      banshee_error "cannot verify the gate body: missing executable $banshee_script_dir/install-hooks.sh"
      banshee_fail=1
    else
      banshee_installed_gate="$(awk -v b="$BANSHEE_GATE_BEGIN" -v e="$BANSHEE_GATE_END" '
        $0 == b { started = 1 }
        started { print }
        started && $0 == e { exit }
      ' "$banshee_hook_file")"
      banshee_canonical_gate="$("$banshee_script_dir/install-hooks.sh" --print-gate-block)"
      if [ "$banshee_installed_gate" != "$banshee_canonical_gate" ]; then
        banshee_error "verify gate block does not match the canonical block in $banshee_hook_file"
        banshee_error "  the markers are present but the body is stale, truncated, or hand-edited —"
        banshee_error "  a marker alone proves nothing about what actually runs"
        banshee_error "  run ./scripts/install-hooks.sh to reinstall the current gate"
        banshee_fail=1
      fi
    fi
  fi
fi

if [ "$banshee_fail" -ne 0 ]; then
  exit 1
fi

banshee_info "git hook baseline OK"
