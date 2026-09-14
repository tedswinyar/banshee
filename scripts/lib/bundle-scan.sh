# bundle-scan.sh — refuse a built app bundle that carries the builder's home path.
#
# A pre-publication review of the first published DMG found `/Users/<builder>/…` in
# all four executables: Cargo registry source paths in the Rust helpers (panic
# locations), checkout and `.build` paths in the Swift app. The source-tree residue
# gate cannot see this — release binaries are not tracked files — so the check has to
# run on the PACKAGED BYTES. build-app.sh calls this after assembling a release bundle
# and refuses to finish if it finds anything; the remapping flags it passes to both
# compilers are what make the check pass.
#
#   banshee_scan_bundle_for_paths <bundle-dir> [extra-forbidden-prefix …]
#
# Scans every regular file under the bundle as bytes (`grep -a`), so a path that sits
# after a NUL in a Mach-O is found like any other. Forbidden by default: any
# `/Users/<name>` other than the documented fixture user `example`, plus $HOME and any
# extra prefixes given. Prints each offending file with a sample, returns 1 on a hit,
# 0 when clean, 2 when the bundle is missing.

banshee_scan_bundle_for_paths() {
  local bundle="$1"; shift
  [ -d "$bundle" ] || { printf 'bundle-scan: no bundle at %s\n' "$bundle" >&2; return 2; }
  local rc=0 f hits
  while IFS= read -r -d '' f; do
    hits="$( { grep -a -o '/Users/[A-Za-z0-9._-]*' "$f" 2>/dev/null | grep -v '^/Users/example$' | grep -v '^/Users/$'; } | sort -u | head -3)"
    if [ -n "${HOME:-}" ] && [ -z "$hits" ]; then
      hits="$(grep -a -o -F "$HOME" "$f" 2>/dev/null | head -1)"
    fi
    local extra
    for extra in "$@"; do
      [ -n "$hits" ] && break
      hits="$(grep -a -o -F "$extra" "$f" 2>/dev/null | head -1)"
    done
    if [ -n "$hits" ]; then
      printf 'bundle-scan: %s carries a builder path: %s\n' "${f#"$bundle"/}" "$(printf '%s' "$hits" | paste -sd' ' -)" >&2
      rc=1
    fi
  done < <(find "$bundle" -type f -print0)
  return "$rc"
}
