# Build & release pipeline

How `scripts/build-dmg.sh` produces a signed, notarized `.dmg` — with
particular attention to notarization credential management, which is the
part that bites.

## Entry points

```bash
./scripts/build-app.sh                # .app bundle only (ad-hoc unless configured)
./scripts/build-dmg.sh                # full: build → sign app → notarize+staple app → DMG → notarize+staple DMG
./scripts/build-dmg.sh dmg            # DMG + notarize only; reuses last .app
SKIP_NOTARIZE=1 ./scripts/build-dmg.sh   # signed-but-unnotarized DMG
./scripts/release.sh <version>        # alignment gates → verify → DMG → notes → tag
./scripts/publish.sh                  # sign + upload the release artifacts (configure release.conf first)
```

## Identity lives in release.conf, nowhere else

`scripts/release.conf` (gitignored; `release.conf.example` documents it)
carries `SIGNING_IDENTITY`, `NOTARIZE_PROFILE`, and deploy targets. Without
it, everything still works: ad-hoc signing, notarization skipped, DMG usable
locally. This is deliberate — a fresh clone must produce a runnable build
with zero Apple credentials.

## Entitlements and the provisioning profile

The app claims exactly one entitlement, `com.apple.developer.usernotifications.time-sensitive`
(`Banshee.entitlements`, banshee-jie): it is what lets a Shrieking or red-disk banner
be presented immediately and through Focus instead of being silently downgraded to
an auto-dismissing `.active` one. It is a *restricted* entitlement (Apple TN3125),
and that has two consequences `build-app.sh` enforces:

| Signing | Entitlement | Why |
|---|---|---|
| ad-hoc (`make app`, a fresh clone) | **none** | an ad-hoc signature cannot be authorised by any profile; an ad-hoc binary carrying this key is SIGKILLed at launch (probed 2026-10-15, exit 137) |
| Developer ID | **required**, with `PROVISIONING_PROFILE` | the claim must be backed by a Developer ID provisioning profile embedded at `Contents/embedded.provisionprofile`, or amfid kills the app at launch — after codesign, notarization and stapling all passed |
| Developer ID, `SKIP_TIME_SENSITIVE=1` | none (loud warning) | the deliberate way to ship without it; banners then do not break through Focus |

Before anything is signed the profile is decoded (`security cms -D`) and refused
unless it is a Developer ID profile (`ProvisionsAllDevices`), grants the key, is for
`<TEAM>.com.tedswinyar.banshee`, and has not expired (`scripts/lib/entitlements.sh`;
`scripts/tests/test-entitlements.sh` pins each refusal). The entitlements actually
sealed are composed in `build/` from the checked-in file plus the
`com.apple.application-identifier` / `com.apple.developer.team-identifier` the
profile expects, and the sealed app is read back to confirm the claim landed.
`doctor.sh --release` runs the same profile check, so the release job stops at the
credentials step rather than after a twenty-minute build.

**Making the profile** (a person, once a year — profiles expire): developer.apple.com
→ Certificates, Identifiers & Profiles → *Identifiers* → `com.tedswinyar.banshee`
(create it as a macOS App ID with an explicit bundle id if absent) → *Capabilities*
→ tick **Time Sensitive Notifications** → Save. Then *Profiles* → **+** →
*Distribution: Developer ID* → that App ID → the Developer ID Application certificate
→ name it → Generate → Download. On the build server, as `builder`:
`scripts/build-server/import-provisioning-profile.sh ~/Banshee_Developer_ID.provisionprofile`
(copies it 0600 under `~/.config/banshee-build/`, records `PROVISIONING_PROFILE` in
release.conf, ends with `doctor.sh --release`). The app logs at launch whether it was
granted the capability (`Capabilities.swift`) and Settings says the same, so a
downgrade is never silent.

## Notarization credentials — why this is fragile

Two ways to authenticate to the notary (`scripts/lib/notary.sh`): an **App Store
Connect API key** (`NOTARY_KEY_PATH`/`NOTARY_KEY_ID`/`NOTARY_ISSUER_ID` — a file;
what the maintainer's build server uses) or the keychain profile below.
`xcrun notarytool` stores profile credentials in the **data-protection keychain**,
not the legacy login keychain. Consequences (inherited knowledge, paid for
repeatedly upstream):

1. `security find-generic-password` cannot see the item. The canonical
   "is it there?" check is
   `xcrun notarytool history --keychain-profile banshee-notarize`.
2. The profile can become inaccessible to CLI tools after wake-from-sleep, a
   Touch ID timeout in a non-interactive shell, or iCloud keychain events —
   failing with `No Keychain password item found for profile: …`.

What the script does about it:

- **Pre-flight, fail fast**: the history check runs BEFORE the multi-minute
  build.
- **Self-healing**: if the pre-flight fails and `APPLE_ID`, `APPLE_TEAM_ID` are
  exported, the profile is re-provisioned via `store-credentials --no-validate`
  (notarytool prompts for the app-specific password — interactive only) and the
  build proceeds.
- **Precise failure messaging**: mid-build failures are classified —
  credential-disappeared (re-run; pre-flight heals), service rejection
  (inspect `notarytool log <id>`), transient (retry `build-dmg.sh dmg`).
  A signed-but-unnotarized DMG is always left on disk so a later retry
  can staple without rebuilding.

## One-time setup on a new machine

```bash
xcrun notarytool store-credentials banshee-notarize \
    --apple-id <apple-id> --team-id <team-id>
xcrun notarytool history --keychain-profile banshee-notarize   # verify
cp scripts/release.conf.example scripts/release.conf              # fill in
```

## Common failure modes

| Symptom | Likely cause | Recovery |
|---|---|---|
| `No Keychain password item found` at submit | data-protection keychain locked | run any `notarytool` command interactively, re-run build; or export the self-heal env vars |
| `status: Invalid` from notary | bundle violates hardened-runtime rules | `xcrun notarytool log <id> --keychain-profile banshee-notarize` |
| DMG works locally, warns on other Macs | not notarized (SKIP_NOTARIZE or no profile) | check `xcrun stapler validate dist/…dmg`; re-run `./scripts/build-dmg.sh dmg` with credentials |
| Release blocked: versions diverge | Version.swift or `rust/Cargo.toml` ≠ release arg | bump BOTH deliberately; `scripts/check-version-alignment.sh` names the odd one out; see `VERSIONING.md` |

## Versioning

`swift/Sources/Banshee/Version.swift` and `rust/Cargo.toml`'s workspace version
move together (the app reports the first, the daemon/CLI/MCP the second);
`build-app.sh` reads Version.swift into Info.plist; `release.sh` refuses to tag
unless both match the requested release. Build
number comes from `BUILD_NUMBER` env (default 1) — set it in CI-like
wrappers if sequential build numbers matter to you.

## The bundle layout, and why helpers are not in `MacOS/`

```
Banshee.app/Contents/
  MacOS/Banshee          the SwiftUI app — and nothing else
  Helpers/banshee-api    the daemon binary (a source for `make daemon-install`)
  Helpers/banshee-mcp
  Helpers/banshee        the CLI
  Info.plist
```

**`Contents/MacOS/` holds exactly one file, on purpose.** It used to hold the Rust
binaries too, so `Bundle.url(forAuxiliaryExecutable:)` could find them — and on the
default (case-insensitive) macOS filesystem `MacOS/Banshee` and `MacOS/banshee` are
the same file, so the CLI copy silently overwrote the app. `open Banshee.app` then
launched the CLI, which printed usage and exited 0: no crash, no log, no Dock icon.
The bundle had never been launchable, and nothing noticed because until the menu
bar app existed it was only ever built, never run.

`build-app.sh` now refuses to finish if the main executable does not link SwiftUI, and
`scripts/tests/test-app-bundle.sh` checks both the layout and that the guard is still
there.

Nothing reads the helpers via `forAuxiliaryExecutable:` any more — the app stopped
supervising the API when the daemon became a LaunchAgent (ADR-0004) — but a bundled copy is a legitimate *source*
for installing the LaunchAgent, so they stay.
