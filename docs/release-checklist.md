# Release checklist

`scripts/release.sh <version>` enforces most of this — it is listed here so the
steps are visible and so the ones a script cannot judge are not forgotten.
**Releases are cut on the maintainer's build server** (`docs/build-server.md`):
pushing `release/<version>` there runs everything below, including the
self-contained restore drill. The checklist still applies when a release is cut by
hand. The
restore drill is a REAL gate, not a checkbox to tick: `release.sh` runs
`scripts/restore-drill.sh`, which takes a verified backup and re-reads it, and
the release blocks if it fails. A checkbox in a committed file
would stay ticked and rot; performing the drill cannot.

## Before cutting

- [ ] Decide the bump (feat → minor, fix → patch, breaking → major — VERSIONING.md).
- [ ] Update `swift/Sources/Banshee/Version.swift` `marketing` AND
      `rust/Cargo.toml` `[workspace.package] version` to the new version, then
      `cargo build` so `Cargo.lock` follows. `release.sh` blocks if either
      disagrees with the version argument (`scripts/check-version-alignment.sh`).
- [ ] Working tree clean (`release.sh` blocks on a dirty tree).

## Gates `release.sh` runs for you (fail-closed)

- [ ] **Restore drill** — `scripts/restore-drill.sh` takes a `banshee backup`
      and confirms the snapshot re-opens with its schema version and its full row
      count. This is the answer to "recovery from a bad migration": a verified,
      restorable backup exists as of the moment of release. Needs the daemon
      running (it is, on the release machine); the release fails clearly if not.
- [ ] `./scripts/verify.sh` green (every suite).
- [ ] DMG builds (`build-dmg.sh`) when the swift layer is present.
- [ ] Release tooling present (`git-cliff`, `cargo-about`, `cargo-cyclonedx`).
- [ ] Notes, THIRD-PARTY-NOTICES, and SBOM regenerated and committed.

## Not automated — judgment calls

- [ ] **Replay the mutation manifest**: `make mutations` (minutes; not in the
      per-push gate). A green replay is the proof the pins still hold; a release
      is the right cadence to pay for it.
- [ ] Skim `CHANGELOG.md` — does it read as a human summary, or just commit
      subjects?
- [ ] If any wire field or schema version changed, confirm `docs/wire-format.md`
      and the fixtures match reality.

## After tagging

- [ ] `git push && git push --tags`.
- [ ] `./scripts/publish.sh` — EdDSA-signs the DMG, templates the Sparkle
      appcast, and uploads both to this repository's GitHub Releases
      (`scripts/lib/releases-repo.sh` decides the target; `RELEASES_REPO` in
      release.conf overrides). This is what makes existing installs auto-update; a
      release that skips it strands everyone on their current version.
- [ ] Smoke-test the update path: from the previously-shipped version, confirm
      "Check for Updates…" sees the new one (bump the version and re-run for a
      real offer). Sparkle needs the repository PUBLIC — anonymous GETs 404 on a
      private one; until the flip, rehearse against a local appcast
      (`docs/build-server.md`).
- [ ] Confirm `/health`'s `gitRev` on the shipped daemon matches the tag.
