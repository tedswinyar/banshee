#!/usr/bin/env sh
# Install the Banshee pre-push verify gate without clobbering other hook
# blocks (notably the beads-owned block that `bd hooks install` writes).

banshee_error() {
  printf '%s\n' "install-hooks: ERROR: $*" >&2
}

banshee_info() {
  printf '%s\n' "install-hooks: $*"
}

# Marker matchers live in one place so install-hooks.sh and check-hooks.sh
# cannot drift apart.
banshee_lib_dir="$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)/lib"
if [ ! -r "$banshee_lib_dir/hook-markers.sh" ]; then
  printf '%s\n' "install-hooks: ERROR: missing $banshee_lib_dir/hook-markers.sh" >&2
  exit 1
fi
# shellcheck source=lib/hook-markers.sh
. "$banshee_lib_dir/hook-markers.sh"

# The canonical gate block, in ONE place. check-hooks.sh compares the
# installed block against this byte-for-byte (via --print-gate-block), so a
# hook whose body was emptied, truncated, or hand-edited between surviving
# markers is caught — a marker alone proves nothing.
banshee_print_gate_block() {
  cat <<'BANSHEE_BLOCK'
# --- BEGIN BANSHEE VERIFY GATE v1 ---
# This section is managed by scripts/install-hooks.sh. Do not edit by hand.
_banshee_zero_sha=0000000000000000000000000000000000000000
_banshee_ref_file="${TMPDIR:-/tmp}/banshee-pre-push.$$"
_banshee_should_verify=0

if ! cat > "$_banshee_ref_file"; then
  echo >&2 "banshee: failed to read pre-push refs from stdin"
  rm -f "$_banshee_ref_file"
  exit 1
fi

_banshee_repo_root="$(git rev-parse --show-toplevel 2>/dev/null || true)"
if [ -z "$_banshee_repo_root" ]; then
  echo >&2 "banshee: could not resolve the pushing working copy"
  rm -f "$_banshee_ref_file"
  exit 1
fi

echo >&2 "banshee: checking git hook baseline"
if [ ! -x "$_banshee_repo_root/scripts/check-hooks.sh" ]; then
  echo >&2 "banshee: missing executable $_banshee_repo_root/scripts/check-hooks.sh"
  rm -f "$_banshee_ref_file"
  exit 1
fi
"$_banshee_repo_root/scripts/check-hooks.sh"
_banshee_status=$?
if [ "$_banshee_status" -ne 0 ]; then
  echo >&2 "banshee: hook baseline failed (exit $_banshee_status); push blocked before tests"
  rm -f "$_banshee_ref_file"
  exit "$_banshee_status"
fi

while read _banshee_local_ref _banshee_local_sha _banshee_remote_ref _banshee_remote_sha
do
  [ -n "$_banshee_local_ref" ] || continue
  [ "$_banshee_local_sha" = "$_banshee_zero_sha" ] && continue
  [ "$_banshee_local_sha" = "$_banshee_remote_sha" ] && continue

  case "$_banshee_remote_ref" in
    refs/tags/*)
      ;;
    *)
      _banshee_should_verify=1
      ;;
  esac
done < "$_banshee_ref_file"
# The first loop consumes the ref list; the OPE gate below needs to read it
# too, so keep a copy rather than re-reading a stdin that is already drained.
_banshee_ref_copy="${_banshee_ref_file}.ope"
cp "$_banshee_ref_file" "$_banshee_ref_copy" 2>/dev/null || _banshee_ref_copy="$_banshee_ref_file"
rm -f "$_banshee_ref_file"

if [ "$_banshee_should_verify" -ne 1 ]; then
  echo >&2 "banshee: no branch updates to verify; skipping the verify gate"
  rm -f "${_banshee_ref_copy:-}" 2>/dev/null
  unset _banshee_zero_sha _banshee_ref_file _banshee_ref_copy _banshee_should_verify _banshee_repo_root _banshee_status
else
  if [ ! -x "$_banshee_repo_root/scripts/verify.sh" ]; then
    echo >&2 "banshee: missing executable $_banshee_repo_root/scripts/verify.sh"
    echo >&2 "banshee: branch pushes fail closed when the pushing checkout cannot run the canonical verifier"
    rm -f "${_banshee_ref_copy:-}" 2>/dev/null
    unset _banshee_zero_sha _banshee_ref_file _banshee_ref_copy _banshee_should_verify _banshee_repo_root _banshee_status
    exit 1
  fi

  # OPE contract-version gate — runs only when the open-prompt-edition layer
  # is present. Cheap (a git diff), so it runs BEFORE the multi-minute
  # verifier. FAIL CLOSED when the layer exists but the gate cannot run: a
  # silently skipped checker lets a contract change through unversioned.
  if [ -d "$_banshee_repo_root/open-prompt-edition" ]; then
    if [ ! -x "$_banshee_repo_root/scripts/check-ope-version-bump.sh" ]; then
      echo >&2 "banshee: open-prompt-edition/ exists but scripts/check-ope-version-bump.sh is missing or not executable"
      echo >&2 "banshee: the OPE contract-version gate cannot run; branch pushes fail closed"
      rm -f "${_banshee_ref_copy:-}" 2>/dev/null
      exit 1
    fi
    while read _banshee_l_ref _banshee_l_sha _banshee_r_ref _banshee_r_sha
    do
      [ -n "$_banshee_l_ref" ] || continue
      [ "$_banshee_l_sha" = "$_banshee_zero_sha" ] && continue
      case "$_banshee_r_ref" in refs/tags/*) continue ;; esac
      _banshee_base="$_banshee_r_sha"
      if [ "$_banshee_base" = "$_banshee_zero_sha" ]; then
        _banshee_base="$(git -C "$_banshee_repo_root" rev-parse --verify --quiet origin/main || true)"
      fi
      if [ -z "$_banshee_base" ]; then
        echo >&2 "banshee: cannot resolve a base commit for the OPE contract-version gate (new branch and origin/main is missing)"
        echo >&2 "banshee: the gate cannot run; branch pushes fail closed. Fix: git fetch origin main"
        rm -f "${_banshee_ref_copy:-}" 2>/dev/null
        exit 1
      fi
      if ! "$_banshee_repo_root/scripts/check-ope-version-bump.sh" "$_banshee_base" "$_banshee_l_sha"; then
        echo >&2 "banshee: push blocked by the OPE contract-version gate"
        rm -f "${_banshee_ref_copy:-}" 2>/dev/null
        exit 1
      fi
    done < "$_banshee_ref_copy"
  fi

  echo >&2 "banshee: running pre-push verify gate: ./scripts/verify.sh"
  "$_banshee_repo_root/scripts/verify.sh"
  _banshee_status=$?
  if [ "$_banshee_status" -eq 0 ]; then
    echo >&2 "banshee: pre-push verify passed"
  elif [ "$_banshee_status" -eq 75 ]; then
    echo >&2 "banshee: verify lock collision (exit 75); another verify.sh is already running, so this is not a test failure"
    echo >&2 "banshee: push blocked. Wait for the active verifier to finish, then push again."
    exit 75
  else
    echo >&2 "banshee: pre-push verify failed (exit $_banshee_status); see the PASS/FAIL summary above"
    echo >&2 "banshee: push blocked. Fix the failing suite, then push again."
    exit "$_banshee_status"
  fi
  rm -f "${_banshee_ref_copy:-}" 2>/dev/null
  unset _banshee_zero_sha _banshee_ref_file _banshee_ref_copy _banshee_should_verify _banshee_repo_root _banshee_status
fi
# --- END BANSHEE VERIFY GATE v1 ---
BANSHEE_BLOCK
}

# --print-gate-block: emit the canonical gate block and exit. check-hooks.sh
# uses this to detect a stale or hand-edited installed block.
if [ "${1:-}" = "--print-gate-block" ]; then
  banshee_print_gate_block
  exit 0
fi

# Verify a CANDIDATE hook file ($1) before it is allowed to become the live
# hook. This deliberately runs against the temporary file, never the live
# one: replace-then-check leaves a corrupt ACTIVE push gate behind on any
# failure. Fail here and the live hook is still untouched.
banshee_assert_candidate() {
  banshee_candidate="$1"
  banshee_assert_failed=0
  banshee_gate_begin_count="$(banshee_count_marker "$BANSHEE_GATE_BEGIN" "$banshee_candidate")"
  banshee_gate_end_count="$(banshee_count_marker "$BANSHEE_GATE_END" "$banshee_candidate")"

  if [ "$banshee_gate_begin_count" != "1" ]; then
    banshee_error "expected exactly one gate BEGIN marker in the new hook; found $banshee_gate_begin_count"
    banshee_assert_failed=1
  fi
  if [ "$banshee_gate_end_count" != "1" ]; then
    banshee_error "expected exactly one gate END marker in the new hook; found $banshee_gate_end_count"
    banshee_assert_failed=1
  fi

  if [ "$banshee_had_beads_block" = "1" ]; then
    # Compare the BYTES of the beads block, not the marker counts. Counts
    # stay 1/1 while a strip pass deletes lines between surviving markers.
    banshee_beads_after_bytes="$(banshee_beads_block_bytes "$banshee_candidate")"
    if [ "$banshee_beads_after_bytes" != "$banshee_beads_before_bytes" ]; then
      banshee_error "beads-owned block was modified while installing the gate"
      banshee_error "  the beads block must pass through byte-for-byte; it did not"
      banshee_error "  refusing to install; your existing hook is left unchanged"
      banshee_assert_failed=1
    fi
  fi

  banshee_hook_mode="$(ls -ld "$banshee_candidate" 2>/dev/null | awk '{print $1}' | cut -c1-10)"
  if [ "$banshee_hook_mode" != "-rwxr-xr-x" ]; then
    banshee_error "expected the new hook to have mode 755 (-rwxr-xr-x); found ${banshee_hook_mode:-<missing>}"
    banshee_assert_failed=1
  fi

  if [ "$banshee_assert_failed" -ne 0 ]; then
    return 1
  fi
  return 0
}

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
  /*) banshee_hooks_dir="$banshee_git_common_dir/hooks" ;;
  *) banshee_hooks_dir="$banshee_repo_root/$banshee_git_common_dir/hooks" ;;
esac

banshee_hook_file="$banshee_hooks_dir/pre-push"
mkdir -p "$banshee_hooks_dir" || exit 1

# Repair: bd >= 1.2 points local/worktree core.hooksPath at .beads/hooks
# (binary hook shims). That override shadows the system hook chain on machines
# running an endpoint-security agent and — observed 2026-08-19 — the shims can fail with "exec format
# error", blocking every push. The verify gate lives in .git/hooks, so the
# override must go. beads itself still syncs fine without its git hooks
# (bd dolt push/pull and the daemon do the work).
for banshee_scope in --local --worktree; do
  banshee_hp="$(git -C "$banshee_repo_root" config "$banshee_scope" --get core.hooksPath 2>/dev/null || true)"
  case "$banshee_hp" in
    */.beads/hooks)
      banshee_info "removing $banshee_scope core.hooksPath override ($banshee_hp)"
      git -C "$banshee_repo_root" config "$banshee_scope" --unset-all core.hooksPath || exit 1
      ;;
  esac
done

if [ ! -w "$banshee_hooks_dir" ] || [ ! -x "$banshee_hooks_dir" ]; then
  banshee_error "hooks directory is not writable/searchable: $banshee_hooks_dir"
  exit 1
fi

if [ -e "$banshee_hook_file" ] && [ ! -w "$banshee_hook_file" ]; then
  banshee_error "existing pre-push hook is not writable: $banshee_hook_file"
  exit 1
fi

banshee_had_beads_block=0
banshee_beads_before_bytes=""
if [ -f "$banshee_hook_file" ]; then
  banshee_beads_status="$(banshee_beads_block_status "$banshee_hook_file")"
  case "$banshee_beads_status" in
    ok)
      banshee_had_beads_block=1
      banshee_beads_before_bytes="$(banshee_beads_block_bytes "$banshee_hook_file")"
      banshee_info "preserving beads-owned block ($(banshee_beads_version_label "$banshee_hook_file"))"
      ;;
    mismatch)
      banshee_error "beads hook block markers disagree in $banshee_hook_file"
      banshee_error "  BEGIN says v$(banshee_beads_begin_version "$banshee_hook_file")"
      banshee_error "  END says   v$(banshee_beads_end_version "$banshee_hook_file")"
      banshee_error "refusing to install: the beads block looks corrupt, and rewriting the"
      banshee_error "hook could destroy beads-owned data. Repair it (or re-run bd hooks"
      banshee_error "install), then rerun ./scripts/install-hooks.sh"
      exit 1
      ;;
    malformed)
      banshee_error "beads hook block is malformed in $banshee_hook_file"
      banshee_error "  found BEGIN x$(banshee_beads_begin_count "$banshee_hook_file"), END x$(banshee_beads_end_count "$banshee_hook_file"); expected exactly one of each"
      banshee_error "refusing to install: repair the beads block (or re-run bd hooks install),"
      banshee_error "then rerun ./scripts/install-hooks.sh"
      exit 1
      ;;
  esac
fi

banshee_tmp_file="$(mktemp "$banshee_hooks_dir/pre-push.banshee.XXXXXX")" || exit 1
banshee_stripped_file="$(mktemp "$banshee_hooks_dir/pre-push.banshee-stripped.XXXXXX")" || {
  rm -f "$banshee_tmp_file"
  exit 1
}
banshee_block_file="$(mktemp "$banshee_hooks_dir/pre-push.banshee-block.XXXXXX")" || {
  rm -f "$banshee_tmp_file" "$banshee_stripped_file"
  exit 1
}

banshee_cleanup() {
  rm -f "$banshee_tmp_file" "$banshee_stripped_file" "$banshee_block_file"
}

trap banshee_cleanup EXIT HUP INT TERM

banshee_print_gate_block > "$banshee_block_file" || exit 1

if [ -f "$banshee_hook_file" ]; then
  banshee_backup="$banshee_hook_file.banshee-backup-$(date +%Y%m%d%H%M%S).$$"
  command cp -p "$banshee_hook_file" "$banshee_backup" || exit 1
  banshee_info "backed up existing hook to $banshee_backup"

  if ! awk '
    /^# --- BEGIN BANSHEE VERIFY GATE / {
      if (in_gate_block) {
        print "install-hooks: ERROR: nested verify gate BEGIN marker" > "/dev/stderr"
        exit 2
      }
      in_gate_block = 1
      next
    }
    /^# --- END BANSHEE VERIFY GATE / {
      if (!in_gate_block) {
        print "install-hooks: ERROR: verify gate END marker without BEGIN" > "/dev/stderr"
        exit 2
      }
      in_gate_block = 0
      next
    }
    {
      if (!in_gate_block) {
        print
      }
    }
    END {
      if (in_gate_block) {
        print "install-hooks: ERROR: verify gate BEGIN marker without END; refusing to rewrite hook" > "/dev/stderr"
        exit 2
      }
    }
  ' "$banshee_hook_file" > "$banshee_stripped_file"; then
    banshee_error "hook left unchanged"
    exit 1
  fi
else
  printf '%s\n' '#!/usr/bin/env sh' > "$banshee_stripped_file" || exit 1
fi

cat "$banshee_stripped_file" "$banshee_block_file" > "$banshee_tmp_file" || exit 1
command chmod 755 "$banshee_tmp_file" || exit 1

# Verify the candidate BEFORE it becomes the live hook. mv-then-check leaves
# the user's ACTIVE pre-push hook corrupt on any assertion failure. A tool
# whose failure mode is "your push gate is now broken" is worse than the bug
# it was written to fix.
if ! banshee_assert_candidate "$banshee_tmp_file"; then
  banshee_error "refusing to install the new hook; verification of the candidate failed"
  if [ -n "${banshee_backup:-}" ]; then
    banshee_error "your existing hook at $banshee_hook_file is UNCHANGED (backup: $banshee_backup)"
  else
    banshee_error "no hook was installed at $banshee_hook_file"
  fi
  exit 1
fi

command mv -f "$banshee_tmp_file" "$banshee_hook_file" || exit 1

# Belt and braces: re-assert against the live file (a concurrent writer could
# have raced the mv) and roll back to the backup if it does not hold.
if ! banshee_assert_candidate "$banshee_hook_file"; then
  banshee_error "post-install verification failed against the live hook"
  if [ -n "${banshee_backup:-}" ] && [ -f "$banshee_backup" ]; then
    if command cp -p "$banshee_backup" "$banshee_hook_file"; then
      banshee_error "restored your previous hook from $banshee_backup"
    else
      banshee_error "FAILED to restore from $banshee_backup -- restore it by hand:"
      banshee_error "  cp -p $banshee_backup $banshee_hook_file"
    fi
  else
    banshee_error "there was no previous hook to restore; removing the incomplete hook"
    rm -f "$banshee_hook_file"
  fi
  exit 1
fi

trap - EXIT HUP INT TERM
banshee_cleanup

banshee_info "installed verify gate in $banshee_hook_file"
