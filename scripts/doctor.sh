#!/usr/bin/env bash
set -uo pipefail

# doctor.sh — check the development environment is ready to build, test, and
# release this project. Run it right after cloning/stamping.
#
# Exit 0 = everything required is present.
# Exit 1 = a REQUIRED tool or the pre-push hook is missing.
#
# Two tiers:
#   required  — needed to build + run the verify gate (incl. the security
#               gates, which verify.sh runs present-or-warn: doctor makes the
#               expectation explicit so "decorative" never happens silently).
#   release   — needed only to cut a release (reported, not fatal here).

ROOT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
SCRIPT_DIR="$ROOT_DIR/scripts"
rc=0
# --release: ALSO check the signing/notarization/publishing credentials, and make
# every release-tier miss FATAL. This is what the build server runs before a
# release, and what an operator runs once after provisioning a build machine.
RELEASE_MODE=""
[ "${1:-}" = "--release" ] && RELEASE_MODE=1

say()  { printf '%s\n' "$*"; }
ok()   { printf '  \033[0;32mok\033[0m   %s\n' "$*"; }
miss() { printf '  \033[0;31mMISS\033[0m %s — %s\n' "$1" "$2"; }

check() { # check <tool> <install hint> <tier: required|release>
  if command -v "$1" >/dev/null 2>&1; then
    ok "$1"
  else
    miss "$1" "$2"
    [ "$3" = required ] && rc=1
    [ "$3" = release ] && [ -n "$RELEASE_MODE" ] && rc=1
  fi
}

say "banshee doctor — checking your environment"
say ""
say "Required (build + verify gate, including the security gates):"
check cargo    "https://rustup.rs" required
check rustc    "https://rustup.rs" required
[ -d "$ROOT_DIR/swift" ] && check swift "xcode-select --install" required
check jq       "brew install jq" required
check cargo-deny "brew install cargo-deny — advisory/license gate (verify skips it with a warning if absent, which makes it decorative; install it)" required
check gitleaks   "brew install gitleaks — secret scan (same: verify warns-and-skips without it)" required
[ -d "$ROOT_DIR/website" ] && check hugo "brew install hugo" required
command -v bd >/dev/null 2>&1 && ok "bd (beads)" || miss "bd" "optional issue tracker — https://github.com/steveyegge/beads"

say ""
say "Release-only (needed to cut a signed release, not to develop):"
check git-cliff      "brew install git-cliff — changelog generation" release
check cargo-about    "cargo install cargo-about — THIRD-PARTY-NOTICES" release
check cargo-cyclonedx "cargo install cargo-cyclonedx — SBOM" release
say "  (optional) pipx install dmgbuild — the styled DMG window; without it the DMG is unstyled but valid"

# ---------------------------------------------------------------------------
# --release: the credentials a signed, notarized, auto-updating release needs.
# Each is checked by DOING the cheap version of the real thing (resolving the
# identity, listing notary history, reading the key), not by looking for a file.
# ---------------------------------------------------------------------------
if [ -n "$RELEASE_MODE" ]; then
  say ""
  say "Release credentials (--release):"
  RELEASE_CONF="${BANSHEE_RELEASE_CONF:-$SCRIPT_DIR/release.conf}"
  if [ -f "$RELEASE_CONF" ]; then
    ok "release.conf at $RELEASE_CONF"
    # shellcheck source=/dev/null
    . "$RELEASE_CONF"
  else
    miss "release.conf" "none at $RELEASE_CONF (copy scripts/release.conf.example; BANSHEE_RELEASE_CONF overrides the path)"; rc=1
  fi
  # shellcheck source=lib/notary.sh
  . "$SCRIPT_DIR/lib/notary.sh"
  # shellcheck source=lib/releases-repo.sh
  . "$SCRIPT_DIR/lib/releases-repo.sh"

  if [ -n "${SIGNING_IDENTITY:-}" ] && [ "$SIGNING_IDENTITY" != "-" ]; then
    # The identity must resolve to a usable cert+key in the keychain codesign will
    # search. With SIGNING_KEYCHAIN set that keychain must be UNLOCKED, or the
    # runner's codesign prompts into the void.
    if security find-identity -v -p codesigning ${SIGNING_KEYCHAIN:+"$SIGNING_KEYCHAIN"} 2>/dev/null \
         | grep -qF "$SIGNING_IDENTITY"; then
      ok "signing identity resolves${SIGNING_KEYCHAIN:+ in $SIGNING_KEYCHAIN}: $SIGNING_IDENTITY"
    else
      miss "signing identity" "'$SIGNING_IDENTITY' not found by security find-identity${SIGNING_KEYCHAIN:+ in $SIGNING_KEYCHAIN} — import the Developer ID .p12 (and unlock the keychain)"; rc=1
    fi
  else
    miss "SIGNING_IDENTITY" "unset or ad-hoc in release.conf — a release must be Developer ID signed"; rc=1
  fi

  if notary_lines="$(banshee_notary_args 2>&1)"; then
    NOTARY_ARGS=(); while IFS= read -r a; do NOTARY_ARGS+=("$a"); done <<< "$notary_lines"
    if xcrun notarytool history "${NOTARY_ARGS[@]}" >/dev/null 2>&1; then
      ok "notary credentials accepted by Apple: $(banshee_notary_describe)"
    else
      miss "notary credentials" "configured ($(banshee_notary_describe)) but 'notarytool history' failed — locked keychain, wrong key id/issuer, or offline"; rc=1
    fi
  else
    miss "notary credentials" "neither NOTARY_KEY_PATH/NOTARY_KEY_ID/NOTARY_ISSUER_ID nor NOTARIZE_PROFILE is usable: $notary_lines"; rc=1
  fi

  if [ -n "${SPARKLE_PUBLIC_KEY:-}" ]; then ok "SPARKLE_PUBLIC_KEY set"; else miss "SPARKLE_PUBLIC_KEY" "unset — the app could not auto-update"; rc=1; fi
  if [ -n "${SPARKLE_PRIVATE_KEY_FILE:-}" ]; then
    if [ -r "$SPARKLE_PRIVATE_KEY_FILE" ]; then ok "Sparkle private key file readable: $SPARKLE_PRIVATE_KEY_FILE"
    else miss "Sparkle private key" "SPARKLE_PRIVATE_KEY_FILE=$SPARKLE_PRIVATE_KEY_FILE is not readable"; rc=1; fi
  elif security find-generic-password -s https://sparkle-project.org >/dev/null 2>&1; then
    ok "Sparkle private key in the keychain"
  else
    miss "Sparkle private key" "no SPARKLE_PRIVATE_KEY_FILE and no keychain item — export it from the machine that has it (generate_keys -x); NEVER regenerate"; rc=1
  fi

  # Releases publish to the source repository itself; RELEASES_REPO is only an override.
  if releases_repo="$(banshee_releases_repo "$ROOT_DIR")"; then
    ok "releases publish to $releases_repo${RELEASES_REPO:+ (RELEASES_REPO override)}"
  else
    miss "releases repo" "no RELEASES_REPO, no GITHUB_REPOSITORY, and origin is not a github.com URL — publish.sh has nowhere to upload"; rc=1
  fi
  if command -v gh >/dev/null 2>&1 && gh auth status >/dev/null 2>&1; then
    ok "gh is authenticated"
  else
    miss "gh auth" "gh missing or not logged in (GH_TOKEN or gh auth login) — publish.sh uploads with it"; rc=1
  fi
fi

say ""
# A CI checkout is not a developer clone: nothing pushes from it through a hook (the
# workflows push with --no-verify, having just run the gate themselves), and a fresh
# actions/checkout never has hooks. Requiring one here failed every job on the build
# server before it reached the gate (found provisioning the build server, 2026-09-13).
if [ -n "${GITHUB_ACTIONS:-}" ]; then
  say "  skip pre-push verify gate — CI checkout (GITHUB_ACTIONS is set), no hook expected"
else
  HOOK="$ROOT_DIR/.git/hooks/pre-push"
  if [ -f "$HOOK" ] && grep -q "VERIFY GATE" "$HOOK" 2>/dev/null; then
    ok "pre-push verify gate installed"
  else
    miss "pre-push hook" "run ./scripts/install-hooks.sh"
    rc=1
  fi
fi

say ""
if [ "$rc" -eq 0 ]; then
  if [ -n "$RELEASE_MODE" ]; then say "Release-ready: tooling and credentials all present."
  else say "All required tooling present. You're ready: ./scripts/verify.sh"; fi
else
  say "Some REQUIRED tooling is missing (see MISS above). Install it, then re-run ./scripts/doctor.sh"
fi
exit "$rc"
