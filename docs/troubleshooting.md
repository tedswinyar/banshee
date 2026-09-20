# Troubleshooting

> **Daemon problems have their own page: [daemon.md](daemon.md).** If the symptom
> is "nothing is answering", "the app says nothing is watching", or "my change did
> not take effect", start there — the answer is almost always that rebuilding does
> not update the running service.

Common failures, their diagnosis, fixes, and prevention. Each entry follows symptom → diagnosis → fix → prevention.

## Build failures

### verify.sh exits with code 75

**Symptom**: `./scripts/verify.sh` exits immediately with code 75, no test output.

**Diagnosis**: Another verify process is running in this working copy. The gate takes a per-working-copy lock because two concurrent Swift builds in one checkout corrupt each other's build state.

**Fix**: Wait for the other verify to complete, then retry. Do not delete `.runtime/verify.lock` manually — that defeats the protection.

**Prevention**: Run verify serially in each checkout. If you need parallel verification, use separate clones or worktrees.

### Push blocked with "cannot execute binary file"

**Symptom**: Push blocked, hook execution fails with "cannot execute binary file: Exec format error" or similar.

**Diagnosis**: Some tool re-pointed `core.hooksPath` at a directory of its own, shadowing the repo's hooks, and a file there is not a runnable executable. Tooling that writes non-executable files into a `hooksPath` directory it owns produces exactly this shape.

**Fix**:
1. `./scripts/install-hooks.sh` (restores the canonical hooks)
2. `git config --unset core.hooksPath` if a tool set it, and check `scripts/check-hooks.sh` passes

**Prevention**: Let `make build` / `make verify` self-heal the hook chain rather than installing hooks by hand; `scripts/check-hooks.sh` compares the gate byte-for-byte on every push.

### SourceKit shows "No such module" / "Cannot find type" errors

**Symptom**: Xcode or editor shows type errors for code that compiles fine.

**Diagnosis**: SourceKit lies before the first build. Fresh checkouts and new files show diagnostics that aren't real.

**Fix**: `swift build` once. Ignore editor errors until the compiler has run.

**Prevention**: Build first; trust the compiler, not the editor overlay.

### build-app.sh: "the release bundle carries a builder path; refusing to finish"

`scripts/lib/bundle-scan.sh` found `/Users/<you>/…` in a release binary. If the sample
it prints is `…/swift/.build/out/Intermediates.noindex/…`, the Swift build ran under
SwiftPM's newer `swiftbuild` build system (the default from Swift 6.4 / Xcode 27),
which records absolute intermediate paths that the compiler's `-file-prefix-map` and
the linker's `-oso_prefix` do not reach. `build-app.sh` passes `--build-system
native` for release builds for exactly this reason; check that line is still there
and that `swift build --help` still lists `native`. If it no longer does, the fix is
to strip debug stabs from the release app binary before signing (see the bead named
in `build-app.sh`). The scan is correct to refuse: the first published DMG shipped
2,163 home-path strings before it existed.

### Lock file mismatch or "cargo build" fails with dependency errors

**Symptom**: Rust build fails with version conflicts, lock file out of date, or missing dependencies.

**Diagnosis**: Cargo.lock is stale or someone force-pushed without regenerating it.

**Fix**: `cargo update` or `cargo build --locked` to see the actual mismatch. Commit the regenerated Cargo.lock.

**Prevention**: Never gitignore Cargo.lock in application crates (libraries are different). The pre-push hook runs verify, which will catch lock-file drift before it reaches main.

## Runtime failures

### App shows "Nothing is watching"

**Symptom**: The app launches but reports that no daemon is answering. The app never starts one itself (a second `banshee-api` would be a second writer on the database).

**Diagnosis**: The daemon is not installed, not running, or cannot serve. Its error is in its stderr log, not surfaced in the UI.

**Fix**: Run `make daemon-status` (or `scripts/check-service.sh`) and read `~/Library/Logs/banshee-api.err.log`. Common causes:
- The pinned prod port 18769 is already bound by something else
- Database file is locked by another process
- Permissions issue on the data directory
- Key file is missing or unreadable (should be auto-generated)

**Prevention**: Check the log file first. If the app must surface diagnostics, they have to travel over the API — a `/health` response, a status field, or a stored record. Stderr is invisible to the UI by design.

**Reference**: `adr/0001-constellation-architecture.md`

### Port already in use / API fails to bind

**Symptom**: `./scripts/start.sh` or the app fails to start; log shows "Address already in use" or "Failed to bind".

**Diagnosis**: Another banshee-api process is running, or the port (18769 for prod, pinned; 18779–18788 for dev, which climbs the ladder) is occupied by something else.

**Fix**:
1. Find the occupying process: `lsof -i :18769` (or the relevant port)
2. Kill it if it's a stale banshee-api: `kill <pid>`
3. If it's another service, change `BANSHEE_PORT` to a free range

**Prevention**: The dev and test profiles climb a port ladder (port, port+1, ..., port+9) to find an open port; prod pins its port and fails loudly instead, because every client promises to find it there (see `daemon.md`). Check `lsof` before assuming the ladder is broken.

### Wrong architecture binary / "Exec format error"

**Symptom**: Launching a binary fails with "cannot execute binary file" or "bad CPU type".

**Diagnosis**: The binary was built for a different architecture (arm64 vs x86_64) or is corrupted (see the hooks entry above).

**Fix**:
1. Check: `file target/debug/banshee-api` (should match your Mac's arch)
2. Rebuild: `cargo clean && cargo build`
3. If it persists, see "cannot execute binary file" above

**Prevention**: Use `cargo build` without cross-compilation unless explicitly needed.

### Key file permission issues / 401 Unauthorized

**Symptom**: CLI or MCP server fails with 401 when the API is running.

**Diagnosis**: The key file (`~/Library/Application Support/banshee/api_key`) is missing, unreadable, or has the wrong permissions. Or the client is reading a different key file than the server generated.

**Fix**:
1. Check the key file exists and is readable: `ls -le ~/Library/Application\ Support/banshee/api_key`
2. Permissions should be 0600 (user read/write only)
3. If missing, the server generates it on first start — delete and restart the server
4. Check `BANSHEE_KEY_FILE` env var isn't pointing elsewhere

**Prevention**: The server generates the key file on first run with 0600 permissions. Clients read the same file by default. Don't manually edit or chmod it.

**Reference**: `SECURITY.md`, `threat-model.md`

## Test failures

### Tests fail with "Database is locked" or version mismatch

**Symptom**: Rust or integration tests fail with "database is locked" or "schema version mismatch".

**Diagnosis**: Either a prod/dev database leaked into test (should be impossible — test profile refuses prod paths) or a previous test didn't clean up.

**Fix**:
1. Check `BANSHEE_PROFILE=test` and `BANSHEE_DB_PATH` points to a temp file
2. Ensure test profile is active: the test suite should set `BANSHEE_PROFILE=test` and pass a temp DB path
3. If a temp DB is reused between tests without cleanup, add explicit cleanup or use unique temp files per test

**Prevention**: Test profile refuses any DB path under the prod data dir (symlink-resolved). This guard exists specifically to prevent test-vs-prod collisions.

**Reference**: `data-safety.md`; the profile rules in `rust/banshee-api/src/config.rs`

### e2e parity test fails: CLI vs HTTP vs MCP output differs

**Symptom**: `tests/e2e/run-e2e.sh` reports byte-for-byte mismatch between CLI JSON, raw HTTP JSON, and MCP response.

**Diagnosis**: A wire format divergence — usually a datetime encoding difference (microseconds vs milliseconds) or a null-handling issue (omitted vs present-as-null).

**Fix**:
1. Capture the actual outputs: the e2e script logs each
2. Diff them: `jq . cli.json > cli-pretty.json; jq . http.json > http-pretty.json; diff -u cli-pretty.json http-pretty.json`
3. Trace to the encoder: check `wire_time.rs` (Rust) and `WireDate.swift` against the Five Rules

**Prevention**: Changes to wire format must update `tests/fixtures/` and `docs/wire-format.md` in the same push, and the parity harness must stay green.

**Reference**: `docs/wire-format.md`

### Test passes before and after a fix (false pin)

**Symptom**: You wrote a test to pin a bug, but it passes even before applying the fix.

**Diagnosis**: The test is mocking the wrong layer, or it's round-tripping through your own encoder/decoder (which makes the bug invisible).

**Fix**:
1. Apply the REVERTING mutation (reintroduce the bug) and watch the test fail
2. If it doesn't fail, the test is not pinning what you think it is
3. Drive the real layer: parser tests start from raw fixture bytes, not already-decoded values

**Prevention**: Mutation-proof is the bar. Make 2+ deliberate single-line production mutations per file and report which test caught each (`scripts/mutate.sh`).

## Hook and gate failures

### Pre-push hook fails: "check-hooks.sh failed"

**Symptom**: `git push` blocked with "scripts/check-hooks.sh failed".

**Diagnosis**: The canonical gate block in `.git/hooks/pre-push` doesn't match the reference in `scripts/gates/pre-push-canonical.sh`, OR hooks are disabled/shadowed.

**Fix**:
1. Run `./scripts/check-hooks.sh` directly to see the specific mismatch
2. `./scripts/install-hooks.sh` to restore canonical hooks
3. If a tool rewrote the hooks (see "cannot execute binary file" above), follow that fix first

**Prevention**: Never manually edit `.git/hooks/pre-push`. If it needs to change, update `scripts/gates/pre-push-canonical.sh` and re-run install-hooks.sh. The byte-for-byte check exists because a marker comment proves nothing.

## Migration and data failures

### Migration fails: "user_version mismatch"

**Symptom**: App or API refuses to start; log shows "schema version X but app expects Y" or "Refusing to downgrade schema".

**Diagnosis**: The database has a newer schema version than the app knows. This happens when:
- You rolled back to an older app version
- The database was touched by a newer build

**Fix**:
1. Restore from a backup taken before the newer migration ran (see `data-safety.md` → forward-only migrations)
2. OR upgrade the app to the version that knows the schema

**Prevention**: Migrations are forward-only. If you need to roll back the app, you must restore the database from a backup. The schema `user_version` pragma enforces this — finding a newer version is a loud refusal, not a silent corruption.

**Reference**: `data-safety.md`

### Backup verification fails: row count mismatch

**Symptom**: `backup verify` command (or the automated backup job) reports row count mismatch between source and backup.

**Diagnosis**: The backup file is incomplete or corrupt, or a write happened between backup and verification.

**Fix**:
1. Retry the backup immediately
2. If it persists, check disk space and file permissions
3. Manually verify: `sqlite3 banshee.db "SELECT COUNT(*) FROM samples"; sqlite3 banshee-backup.db "SELECT COUNT(*) FROM samples"`

**Prevention**: Backups are verified by restoration (open, query, compare row counts). A backup isn't a backup until it's been restored and validated. Automate verification; don't trust the file existence alone.

**Reference**: `data-safety.md`

## Release and notarization failures

### Notarization fails: credential issues

**Symptom**: `./scripts/release.sh` fails during notarization with authentication error.

**Diagnosis**: `scripts/release.conf` is missing or has wrong credentials, or the password is expired.

**Fix**:
1. Check `scripts/release.conf` (or `BANSHEE_RELEASE_CONF`) configures one of the two
   shapes `scripts/lib/notary.sh` accepts: an App Store Connect API key
   (`NOTARY_KEY_PATH`/`NOTARY_KEY_ID`/`NOTARY_ISSUER_ID`) or a keychain profile
   (`NOTARIZE_PROFILE`, created interactively with `xcrun notarytool store-credentials`).
   Never put a password in the file.
2. Profile shape: run any `notarytool` command interactively once to unlock the keychain.
   Key shape: check the key is not revoked in App Store Connect.
3. Re-run release.sh

**Prevention**: `release.conf` is gitignored (it contains secrets). Copy `release.conf.example` and fill it in. Credentials are per-developer, not per-project.

**Reference**: `build-pipeline.md`

### DMG signing fails: identity not found

**Symptom**: Release script fails with "identity not found" or "certificate not found".

**Diagnosis**: The Developer ID Application certificate isn't in your keychain, or it's expired.

**Fix**:
1. Check: `security find-identity -v -p codesigning`
2. If missing: download from Apple Developer portal and install
3. If expired: renew and re-download

**Prevention**: Code signing identities expire annually. Set a calendar reminder or check during release.

**Reference**: `build-pipeline.md`

## See also

- [lessons/](lessons/) — earned findings, each with what it cost
- [data-safety.md](data-safety.md) → forward-only migrations, backup verification
- [build-pipeline.md](build-pipeline.md) → release process and notarization
- [getting-started.md](getting-started.md) → initial setup troubleshooting
