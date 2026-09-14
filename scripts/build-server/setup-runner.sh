#!/usr/bin/env bash
set -euo pipefail
# setup-runner.sh — as the build user: install and register a GitHub Actions
# runner for the repository, labelled with the repository's name (what the
# workflows' `runs-on` asks for), then print the ONE sudo step that makes it a
# LaunchDaemon (a LaunchAgent only runs while its user is logged in at the GUI, and
# the build user never is).
#
#   setup-runner.sh --token <registration token> [--repo owner/name] [--name <runner name>]
#                   [--version 2.337.0] [--sha256 <hex>]
#
# --repo defaults to BANSHEE_REPO (build-host.sh); --name to <hostname>-<repo name>.
# Mint the token on any authenticated machine (valid one hour):
#   gh api -X POST repos/<owner>/<repo>/actions/runners/registration-token -q .token
export PATH="/opt/homebrew/bin:$HOME/.cargo/bin:/usr/local/bin:$PATH"
# shellcheck source=build-host.sh
. "$(cd "$(dirname "$0")" && pwd)/build-host.sh"
VERSION="2.337.0"
# Published in the runner release notes for actions-runner-osx-arm64-<version>.tar.gz.
SHA256="5a2cd92908a93d7276a194e1de6008099f3e7946f3f8e14aa7a1a7b4a31fdec2"
TOKEN=""
REPO="$(banshee_repo)"
NAME=""
while [ $# -gt 0 ]; do
  case "$1" in
    --token) TOKEN="$2"; shift 2 ;;
    --repo) REPO="$2"; shift 2 ;;
    --name) NAME="$2"; shift 2 ;;
    --version) VERSION="$2"; SHA256=""; shift 2 ;;
    --sha256) SHA256="$2"; shift 2 ;;
    *) echo "unknown argument $1" >&2; exit 1 ;;
  esac
done
die() { printf 'setup-runner: ERROR: %s\n' "$*" >&2; exit 1; }
info() { printf 'setup-runner: %s\n' "$*"; }
[ -n "$TOKEN" ] || die "--token is required"
case "$REPO" in */*) ;; *) die "--repo must be owner/name (got '$REPO')" ;; esac
REPO_URL="https://github.com/$REPO"
REPO_NAME="${REPO##*/}"
# The label is the repository's name — exactly what ci.yml/release.yml `runs-on`
# lists — so a fork that renames the repo gets a matching runner without editing
# either side.
LABEL_TAG="$REPO_NAME"
[ -n "$NAME" ] || NAME="$(hostname -s | tr 'A-Z' 'a-z')-$REPO_NAME"
DIR="$HOME/actions-runner-$REPO_NAME"

mkdir -p "$DIR"; cd "$DIR"
TAR="actions-runner-osx-arm64-$VERSION.tar.gz"
if [ ! -x ./config.sh ]; then
  info "downloading runner $VERSION"
  curl -sSfL -o "$TAR" "https://github.com/actions/runner/releases/download/v$VERSION/$TAR"
  if [ -n "$SHA256" ]; then
    echo "$SHA256  $TAR" | shasum -a 256 -c - || die "checksum mismatch on $TAR"
  else
    die "--version $VERSION was given without --sha256; refusing to run an unverified runner (the checksum is in the runner release notes)"
  fi
  tar xzf "$TAR" && rm -f "$TAR"
fi
if [ -f .runner ]; then
  info "runner already configured: $(sed -n 's/.*"agentName": "\(.*\)",/\1/p' .runner)"
else
  ./config.sh --unattended --url "$REPO_URL" --token "$TOKEN" --name "$NAME" \
    --labels "$LABEL_TAG" --work _work --replace
fi

# The template's ProgramArguments name $DIR/runsvc.sh, but the tarball ships it as
# bin/runsvc.sh; the runner's own svc.sh copies it up on install, and a plist that
# names a missing program is exactly the launchd EX_CONFIG failure the gotchas list
# (caught on the build server before bootstrap, 2026-09-13).
/bin/cp -f bin/runsvc.sh runsvc.sh && chmod +x runsvc.sh

# LaunchDaemon plist from the runner's own template, with UserName = this user. The
# label follows the runner's own svc.sh convention: actions.runner.<owner>-<repo>.<name>.
LABEL="actions.runner.${REPO//\//-}.$NAME"
PLIST="$DIR/$LABEL.plist"
mkdir -p "$HOME/Library/Logs/$LABEL"
sed -e "s|{{SvcName}}|$LABEL|g" -e "s|{{RunnerRoot}}|$DIR|g" -e "s|{{User}}|$(id -un)|g" \
    -e "s|{{UserHome}}|$HOME|g" bin/actions.runner.plist.template > "$PLIST"
cat <<NEXT
setup-runner: registered. Now, as an ADMIN (one time):
  sudo cp "$PLIST" /Library/LaunchDaemons/$LABEL.plist
  sudo chown root:wheel /Library/LaunchDaemons/$LABEL.plist
  sudo launchctl bootstrap system /Library/LaunchDaemons/$LABEL.plist
  sudo launchctl print system/$LABEL | head -20      # state: running
Then: gh api repos/$REPO/actions/runners -q '.runners[] | "\(.name) \(.status)"'
NEXT
