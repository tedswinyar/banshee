#!/usr/bin/env bash
set -euo pipefail
# provision-builder.sh — create the dedicated, non-admin build user on the build
# host. Run ONCE, with sudo, by an admin at that machine (docs/build-server.md):
#
#   sudo scripts/build-server/provision-builder.sh [--user builder] [--ssh-key ~/.ssh/authorized_keys]
#                                                  [--repo owner/name]
#
# Why a separate user: the runner and the release jobs then cannot reach the
# admin's login keychain, working clones, or any other runner on the same machine,
# and the signing material lives in a keychain only this user can open.
# shellcheck source=build-host.sh
. "$(cd "$(dirname "$0")" && pwd)/build-host.sh"
USER_NAME="builder"
SSH_KEYS="${SUDO_USER:+/Users/$SUDO_USER/.ssh/authorized_keys}"
while [ $# -gt 0 ]; do
  case "$1" in
    --user) USER_NAME="$2"; shift 2 ;;
    --ssh-key) SSH_KEYS="$2"; shift 2 ;;
    --repo) BANSHEE_REPO="$2"; shift 2 ;;
    *) echo "unknown argument $1" >&2; exit 1 ;;
  esac
done
[ "$(id -u)" = 0 ] || { echo "provision-builder: run with sudo" >&2; exit 1; }
HOME_DIR="/Users/$USER_NAME"

if id "$USER_NAME" >/dev/null 2>&1; then
  echo "provision-builder: user $USER_NAME exists; leaving the account alone"
else
  echo "provision-builder: creating standard (non-admin) user $USER_NAME — choose a password when prompted"
  sysadminctl -addUser "$USER_NAME" -fullName "Banshee Builder" -home "$HOME_DIR" -shell /bin/zsh -password -
fi
[ -d "$HOME_DIR" ] || createhomedir -c -u "$USER_NAME" >/dev/null

# Non-interactive shells (launchd, ssh) get neither Homebrew nor cargo on PATH on
# this machine (measured 2026-09-05); .zshenv is read by every zsh, so put
# them there once.
cat > "$HOME_DIR/.zshenv" <<'ZSHENV'
export PATH="/opt/homebrew/bin:$HOME/.cargo/bin:/usr/local/bin:$PATH"
[ -f "$HOME/.cargo/env" ] && . "$HOME/.cargo/env"
ZSHENV
mkdir -p "$HOME_DIR/.config/banshee-build" "$HOME_DIR/repos" "$HOME_DIR/.ssh"
chmod 700 "$HOME_DIR/.config/banshee-build" "$HOME_DIR/.ssh"
if [ -n "$SSH_KEYS" ] && [ -r "$SSH_KEYS" ]; then
  /bin/cp -f "$SSH_KEYS" "$HOME_DIR/.ssh/authorized_keys"
  chmod 600 "$HOME_DIR/.ssh/authorized_keys"
  echo "provision-builder: copied ssh authorized_keys from $SSH_KEYS (the development machine can now push to the relay as $USER_NAME)"
fi
chown -R "$USER_NAME:staff" "$HOME_DIR/.zshenv" "$HOME_DIR/.config" "$HOME_DIR/repos" "$HOME_DIR/.ssh"

# Remote Login must admit this user. If it is restricted to a group, add the user
# to it; `dseditgroup` is idempotent.
dseditgroup -o edit -a "$USER_NAME" -t user com.apple.access_ssh 2>/dev/null \
  && echo "provision-builder: added $USER_NAME to the Remote Login access group" || true

cat <<NEXT
provision-builder: done. Next, AS $USER_NAME (ssh $USER_NAME@$(banshee_build_host)):
  git clone https://github.com/$(banshee_repo).git ~/banshee   # after gh auth
  ~/banshee/scripts/build-server/provision-tools.sh
  ~/banshee/scripts/build-server/import-signing-material.sh --p12 … (see docs/build-server.md)
  ~/banshee/scripts/build-server/setup-relay.sh
  ~/banshee/scripts/build-server/setup-runner.sh --token <registration token>   # then sudo for the LaunchDaemon step it prints
NEXT
