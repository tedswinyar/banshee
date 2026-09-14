# The build server — a self-hosted runner as CI gate and publisher

Decided 2026-09-08. **The development machine pushes to the build server. The build
server verifies, promotes `main`, cuts releases and is the only machine that writes to
GitHub.** Every step is scripted; the human steps are the ones that touch a credential.

Throughout, `<build-host>` is the build server's name on the maintainer's network. It is
deliberately not written down here; the provisioning scripts print it, and the
maintainer's notebook (`private/`, see `AGENTS.md`) records it.

```
 dev machine ──git push mbp──▶ build server bare repo ──post-receive──▶ GitHub <owner>/banshee
                                ~builder/repos/banshee.git                main ➜ arrives as `staging`
                                                                          release/X.Y.Z, tags, branches ➜ same name
                                                                                │
                                                  GitHub Actions on the build server's `banshee` runner
                                                  ci.yml:      verify `staging` ➜ fast-forward `main`
                                                  release.yml: release/X.Y.Z ➜ drill · verify · DMG ·
                                                               notarize · notes+tag ➜ push main+tag ➜
                                                               publish DMG+appcast to THIS repo's Releases
```

## Why this shape

- **One GitHub writer.** The development machine never pushes to GitHub for this project.
  `main` moves only when CI on the build server has verified `staging`, or when a release
  is cut.
- **Repeatable.** The release runs from a clean checkout on a machine with nothing but a
  keychain and four files. `scripts/doctor.sh --release` says whether that machine can
  release, by resolving the identity, listing notary history and reading the key — not
  by looking for files.
- **Isolated.** A dedicated non-admin user, `builder`, owns the runner, the relay and the
  signing keychain. A job cannot reach the admin's login keychain, working clones, or any
  other runner on the same machine.
- **No pull-request trigger.** The runner is a real machine holding the release signing
  material, and a pull request from a fork would run the fork's code on it as `builder`.
  `ci.yml` runs on pushes only; a contributor's branch is verified by a maintainer pushing
  it to the relay. Keep it that way on the public repo. GitHub's "require approval for
  all outside collaborators" gates fork pull-request runs, so with push-only triggers it
  locks nothing today — turn it on anyway as a guard against a PR trigger being added
  later. The lock that exists is repository write access: keep the collaborator list to
  the maintainer.

## What had to change in the repo (2026-09-08)

Three couplings made the old pipeline unrepeatable on any second machine:

| Coupling | Fix |
|---|---|
| The restore drill backed up the operator's LIVE prod daemon | `restore-drill.sh --self-contained` (or `BANSHEE_DRILL_SELF_CONTAINED=1`) boots a throwaway test-profile daemon from the freshly built binaries, waits for the 40-sample window, backs up with the matching CLI, verifies, tears down. The drill proves the artifact. |
| Notarization only through the data-protection keychain profile (unopenable from launchd/ssh — killed 0.1.4) | `scripts/lib/notary.sh`: an App Store Connect API key (`NOTARY_KEY_PATH`/`NOTARY_KEY_ID`/`NOTARY_ISSUER_ID`) is preferred; the profile remains for interactive machines; a half-set key is a hard error, never a fall-through. |
| Sparkle and codesign read the login keychain | `publish.sh` honours `SPARKLE_PRIVATE_KEY_FILE` (`sign_update --ed-key-file`); `build-app.sh` honours `SIGNING_KEYCHAIN` (`codesign --keychain`); all three scripts honour `BANSHEE_RELEASE_CONF` so the config lives outside the checkout. |

Tests: `scripts/tests/test-notary.sh`, `test-relay-hook.sh` (two temp bare repos),
`test-releases-repo.sh` (where `publish.sh` uploads), and the self-contained cases in
`test-release-checklist.sh` (a REAL drill against the debug binaries, asserting the
40-sample window).

## Provisioning the build server, once

Everything below lives in `scripts/build-server/`. **Bold** steps need a person at a
keyboard because they touch a credential. Every script takes the repository from
`BANSHEE_REPO` (default `tedswinyar/banshee`; a fork sets its own) and prints the build
host's name from `BANSHEE_BUILD_HOST`, falling back to the machine's own hostname
(`scripts/build-server/build-host.sh`).

1. **Admin, at the build host:** `sudo scripts/build-server/provision-builder.sh` — creates
   the standard user `builder` (choose a password when prompted), a `.zshenv` that puts
   Homebrew and cargo on PATH for non-interactive shells, copies the admin's
   `authorized_keys` so the development machine can ssh as `builder`, and admits `builder`
   to Remote Login.
2. **Admin:** `brew install gh git-cliff cargo-deny gitleaks jq` (brew is owned by the admin
   account; the formulae are shared). Optional: `pipx install dmgbuild`.
3. **As `builder`** (`ssh builder@<build-host>`): `gh auth login` with a fine-grained PAT —
   contents: read+write and workflow scope on the repository — then
   `git clone https://github.com/<owner>/banshee.git ~/banshee` and
   `~/banshee/scripts/build-server/provision-tools.sh` (rustup, cargo-about,
   cargo-cyclonedx, git identity, `doctor.sh`).
4. **Export the signing material on the development machine** — these are the three things
   that must MOVE, and the Sparkle key must never be regenerated (a new key strands every
   shipped install):
   ```
   # Developer ID Application: Keychain Access → My Certificates → right-click →
   # Export → .p12 with a password. (`security export -t identities -f pkcs12` also
   # works but exports EVERY identity in the keychain — prefer the UI.)
   # Sparkle EdDSA private key:
   swift/.build/artifacts/sparkle/Sparkle/bin/generate_keys -x ~/sparkle_private_key.txt
   # App Store Connect API key: appstoreconnect.apple.com → Users and Access →
   # Integrations → Team Keys → Generate (Developer role). Download AuthKey_XXXXXX.p8
   # ONCE; note the Key ID and the Issuer ID.
   scp ~/DeveloperID.p12 ~/sparkle_private_key.txt ~/Downloads/AuthKey_*.p8 builder@<build-host>:
   ```
5. **As `builder`:** `~/banshee/scripts/build-server/import-signing-material.sh --p12
   ~/DeveloperID.p12 --sparkle-key ~/sparkle_private_key.txt --sparkle-public '<the
   SPARKLE_PUBLIC_KEY from the development machine's release.conf>' --notary-key
   ~/AuthKey_XXXXXX.p8 --notary-key-id XXXXXX --notary-issuer <uuid>`. Creates
   `banshee-build.keychain-db` with a generated password (0600 file), imports the identity
   with codesign in its partition list, stores the two key files 0600, writes
   `~/.config/banshee-build/release.conf`, and ends with `doctor.sh --release` — which must
   be all green. Then **delete the exported files from both machines**.
6. **As `builder`:** `~/banshee/scripts/build-server/setup-relay.sh` — the bare repo,
   seeded from GitHub, with `post-receive` installed. It prints the remote to add on the
   development machine: `git remote add mbp ssh://builder@<build-host>/Users/builder/repos/banshee.git`.
7. **As `builder`:** mint a registration token on any authenticated machine (`gh api -X POST
   repos/<owner>/banshee/actions/runners/registration-token -q .token`, valid an hour),
   then `~/banshee/scripts/build-server/setup-runner.sh --token <token>`. It downloads
   runner 2.337.0 (checksum-verified), registers `<hostname>-banshee` with the label
   `banshee` (the repository's name, which is what the workflows' `runs-on` asks for), and
   prints the **one sudo step** that installs it as a LaunchDaemon — a LaunchAgent only
   runs while its user has a GUI session, and `builder` never does.

There is no cross-repo secret to set: releases are published to this repository's own
Releases page with the workflow's `GITHUB_TOKEN`.

## Day-to-day

```
git push mbp main                 # verify on the build server; main fast-forwards on green
git push mbp feature/x            # verified, never promoted
git push origin …                 # FAILS — origin's push URL is disabled on the dev machine
make install                      # ship the same commit to THIS machine: daemon + app, together
```

The development machine's `origin` remote is fetch-only: `git remote set-url --push origin
DISABLED-push-goes-through-the-build-server`. Undo with `git remote set-url --push origin
https://github.com/<owner>/banshee.git` — but do not.

To cut a release: bump BOTH version files (`Version.swift`, `rust/Cargo.toml`; the scripts
suite is red on a half-bump), land that on `main` via `staging`, then:

```
git fetch mbp && git push mbp origin/main:release/0.1.4
```

The workflow refuses unless the branch is exactly `origin/main`'s tip, unlocks the
keychain, runs `doctor.sh --release`, then `release.sh 0.1.4` (self-contained drill,
verify, notices, signed + notarized + stapled DMG, release-notes commit, tag), pushes
`main` and `v0.1.4`, publishes the DMG and appcast to the repository's Releases, deletes
`release/0.1.4`, locks the keychain. Watch it under the repository's Actions tab.

## Rehearsing before the repository is public

Sparkle fetches the appcast anonymously, and an anonymous GET on a private repository's
release asset 404s. For a rehearsal while the repository is still private, serve a feed
locally: on the build host, `python3 -m http.server 8000` in a directory holding the DMG
and `appcast.xml` (publish.sh writes the appcast to a temp dir; copy it), and build the
test app with `APPCAST_URL=http://<build-host>:8000/appcast.xml` so its `SUFeedURL` points
there. Install that build, bump, publish again to the local directory, and watch "Check for
Updates…" offer it. The real feed URL is the default and goes back in for the first public
release.

## Failure modes worth knowing

| Symptom | Cause | Fix |
|---|---|---|
| Job queued forever | runner offline (LaunchDaemon not running, build host asleep/away) | `sudo launchctl print system/actions.runner.<owner>-banshee.<runner name>`; `gh api repos/<owner>/banshee/actions/runners` |
| `errSecInternalComponent` / codesign hangs | keychain locked or key not in codesign's partition list | `unlock-build-keychain.sh`; re-run `import-signing-material.sh` (it re-applies the partition list) |
| `import-signing-material` says no valid Developer ID identity, but `find-identity` (without `-v`) lists one | the chain cannot be built: a fresh non-admin user has no Apple intermediates, and an ssh-only user has an EMPTY keychain search list | the script imports the Developer ID G2 CA (SHA-1 pinned) and adds the build keychain to the search list; `security verify-cert -c leaf.pem -p codeSign -k <kc>` names the problem |
| `import-signing-material` says no valid Developer ID identity and `find-identity` shows "Apple Development: …" | the .p12 was exported from the wrong row in Keychain Access (a development certificate, not the Developer ID) | `security delete-keychain` the build keychain, `rm -rf ~/.config/banshee-build`, re-export the "Developer ID Application" row |
| `notarytool` `HTTP 401` with the API key | wrong key id / issuer id, or key revoked | check App Store Connect; `doctor.sh --release` reproduces it in seconds |
| `relay: FAILED to forward` on `git push mbp` | GitHub's ref is not a fast-forward of yours | the relay never forces; look at what moved on GitHub |
| `release/X` refused: "is not origin/main" | `main` moved after you branched, or the branch carries extra commits | `git push mbp origin/main:release/X` after `staging` promoted |
| Homebrew/cargo tools "missing" over ssh | non-interactive PATH lacks `/opt/homebrew/bin` | every script here exports it; `.zshenv` covers ad-hoc shells |
| Sparkle's "Check for Updates…" finds nothing after a publish | the repository is still private (anonymous GET 404s) | rehearse against a local appcast (above); flip visibility for the real thing |
