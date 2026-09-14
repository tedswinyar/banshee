# build-host.sh — where the build host's name and the repository it serves come
# from, in ONE place. Sourced by the provisioning scripts in this directory.
#
# The build host is a machine on the maintainer's own network, so its name is not
# in the tree. Resolution order for BANSHEE_BUILD_HOST:
#   1. the environment (BANSHEE_BUILD_HOST=… on the command line),
#   2. private/build-server.local at the repo root — a shell fragment kept in the
#      maintainer's notebook (a nested, gitignored repo; see docs/build-server.md),
#   3. this machine's own hostname — these scripts run ON the build host, and the
#      name they print is for the developer's `git remote add`.
#
# BANSHEE_REPO is the GitHub repository (owner/name) the relay forwards to and the
# runner registers with. It defaults to the project's home; a fork sets its own.

banshee_build_host() {
  if [ -n "${BANSHEE_BUILD_HOST:-}" ]; then printf '%s\n' "$BANSHEE_BUILD_HOST"; return 0; fi
  local root local_file
  root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
  local_file="$root/private/build-server.local"
  if [ -r "$local_file" ]; then
    # shellcheck source=/dev/null
    . "$local_file"
    if [ -n "${BANSHEE_BUILD_HOST:-}" ]; then printf '%s\n' "$BANSHEE_BUILD_HOST"; return 0; fi
  fi
  hostname
}

banshee_repo() {
  printf '%s\n' "${BANSHEE_REPO:-tedswinyar/banshee}"
}
