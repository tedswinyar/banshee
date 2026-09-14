# notary.sh — how notarytool authenticates, in ONE place.
#
# Two credential shapes, tried in this order:
#   1. An App Store Connect API key: NOTARY_KEY_PATH (the .p8 file), NOTARY_KEY_ID,
#      NOTARY_ISSUER_ID. A file, 0600, no Apple ID password anywhere. This is the
#      shape a BUILD SERVER needs: it works from launchd and ssh, where the
#      data-protection keychain is locked.
#   2. A keychain profile: NOTARIZE_PROFILE (`xcrun notarytool store-credentials`).
#      Interactive machines only — the profile lives in the data-protection
#      keychain, which a non-interactive shell cannot open (the 0.1.4 release died
#      on exactly this, docs/build-pipeline.md).
#
# banshee_notary_args prints the notarytool auth flags ONE PER LINE (bash 3.2 has
# no safe way to return an array), or exits 1 with nothing printed when neither
# shape is configured. Callers read it into an array:
#
#   NOTARY_ARGS=()
#   while IFS= read -r a; do NOTARY_ARGS+=("$a"); done < <(banshee_notary_args) || true
#   [ "${#NOTARY_ARGS[@]}" -gt 0 ] || SKIP_NOTARIZE=1
#
# A PARTIAL API-key configuration (a path without an id, say) is a hard error,
# not a silent fall-through to the profile: a half-set key on a build server
# would otherwise pick the interactive path and fail minutes later, for the
# wrong reason.

banshee_notary_args() {
  local path="${NOTARY_KEY_PATH:-}" id="${NOTARY_KEY_ID:-}" issuer="${NOTARY_ISSUER_ID:-}"
  if [ -n "$path" ] || [ -n "$id" ] || [ -n "$issuer" ]; then
    if [ -z "$path" ] || [ -z "$id" ] || [ -z "$issuer" ]; then
      printf 'notary: NOTARY_KEY_PATH, NOTARY_KEY_ID and NOTARY_ISSUER_ID must all be set (or none)\n' >&2
      return 2
    fi
    if [ ! -r "$path" ]; then
      printf 'notary: NOTARY_KEY_PATH=%s is not a readable file\n' "$path" >&2
      return 2
    fi
    printf '%s\n' "--key" "$path" "--key-id" "$id" "--issuer" "$issuer"
    return 0
  fi
  if [ -n "${NOTARIZE_PROFILE:-}" ]; then
    printf '%s\n' "--keychain-profile" "$NOTARIZE_PROFILE"
    return 0
  fi
  return 1
}

# banshee_notary_describe — one human line naming which shape is in use.
banshee_notary_describe() {
  if [ -n "${NOTARY_KEY_PATH:-}" ]; then
    printf 'App Store Connect API key %s (%s)\n' "${NOTARY_KEY_ID:-?}" "$NOTARY_KEY_PATH"
  elif [ -n "${NOTARIZE_PROFILE:-}" ]; then
    printf 'keychain profile %s\n' "$NOTARIZE_PROFILE"
  else
    printf 'none configured\n'
  fi
}
