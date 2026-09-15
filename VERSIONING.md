# Banshee Versioning Policy

## Semantic Versioning

Banshee uses [Semantic Versioning](https://semver.org/): `MAJOR.MINOR.PATCH`

| Bump | Trigger | Example |
|------|---------|---------|
| **Major** | **Breaking** contract changes only: a field removed or retyped, an endpoint's semantics changed, a migration existing readers cannot survive | 1.0.0 → 2.0.0 |
| **Minor** | Additive changes: new features, new endpoints, new nullable fields, **forward-only additive schema migrations** | 0.2.0 → 0.3.0 |
| **Patch** | Bug fixes, security fixes, documentation improvements, conformance test fixes | 0.2.0 → 0.2.1 |

The dividing line is compatibility, not mechanism: an additive nullable
column with a forward-only migration is Minor even though it migrates the
schema. If the table ever points at two rows at once, the change is Minor
unless something existing breaks. (Clarified 2026-08-19.)

## "Is this breaking?" Decision Tree

Edge cases and worked examples:

**Adding validation that tightens what's accepted**
```
Q: Does existing valid data become invalid?
   Yes → Major (breaks stored data)
   No → Minor (tightens input only, DB unaffected)
```
Example: Adding `1 <= priority <= 3` validation when the DB already enforces it → Minor

**Fixing a bug that clients may have depended on**
```
Q: Was the bug documented/intentional behavior?
   Yes → Major (documented behavior changed)
   No → Is the fix in an API response?
      Yes → Could break clients → Minor (document in changelog)
      No → Patch (internal fix)
```
Example: Fixing timestamp rounding that was never specified → Minor + changelog

**Changing HTTP status codes**
```
Q: Is the new status code more correct per HTTP semantics?
   Yes + both are 4xx or both are 5xx → Minor (clarification)
   No or crosses 4xx/5xx boundary → Major (semantics changed)
```
Example: Changing 400 → 422 for validation failures → Minor (both client errors)
Example: Changing 500 → 400 when it was actually a bad request → Minor (fix)
Example: Changing 200 → 201 for creates → Minor (more correct)

**Changing log levels or error messages**
```
Log-level changes → Patch (internal diagnostics)
Error message text (not structure) → Patch (not part of wire contract)
Error message structure (new field, different key) → Minor if additive, Major if breaking
```

**Renaming internal functions, files, or modules**
```
Q: Is it in the public API (HTTP endpoints, MCP tools, CLI flags)?
   Yes → Major (breaks clients)
   No → Patch (internal refactor)
```

**Adding a required field with a server-side default**
```
Additive from client perspective (they don't send it) → Minor
```
Example: Adding `created_at` field populated by the server → Minor

**Changing field types**
```
Wider type (i32 → i64) + wire format stays JSON number → Minor (compatible)
Narrower type (i64 → i32) → Major (could truncate)
Type category change (string → number) → Major (breaks parsing)
```

## Version Alignment

**The app and the daemon share the same version number.**

| File | Purpose |
|------|---------|
| `swift/Sources/Banshee/Version.swift` | App runtime version — the DMG, Sparkle, `CFBundleShortVersionString` |
| `rust/Cargo.toml` `[workspace.package] version` | Daemon, CLI and MCP version — `/health`, `banshee health`, MCP `serverInfo` (via `CARGO_PKG_VERSION`) |
| `scripts/build-app.sh` → `VERSION` | Build script (reads from Version.swift) |

`scripts/check-version-alignment.sh` is the one checker: both files must agree
with each other and with the release argument. `scripts/release.sh` runs it and
blocks the release if anything diverges; the scripts suite runs it at HEAD on
every `verify`, so a bump that touched one file and not the other is red before
release day.

**The Cargo workspace version moves WITH the app** (2026-09-08).
The earlier rule here — "Cargo crate versions are independent of the app release
and are not bumped" — is what let 0.1.1 through 0.1.4 ship a daemon that answered
`/health` with `"version": "0.1.0"`. One product, one number.

## Version Files

### Version.swift (authoritative)
```swift
enum Version {
    static let marketing = "0.1.4"
}
```

### Info.plist (at build time)
- `CFBundleShortVersionString` → marketing version (from Version.swift)
- `CFBundleVersion` → build number (`BUILD_NUMBER` env or 1)

### Git Tags
- Format: `v{MAJOR}.{MINOR}.{PATCH}` (annotated)
- Created by `scripts/release.sh`

## Release Process

1. **Decide bump** — features = minor, fixes = patch, breaking = major
2. **Update both version files** — `Version.marketing` in Version.swift and
   `version` under `[workspace.package]` in `rust/Cargo.toml` (then `cargo build`
   so `Cargo.lock` follows)
3. **Write the notes** — a `## [<version>] — <date>` section in `CHANGELOG.md` (Keep a
   Changelog). The release refuses without it and never generates notes from commits.
4. **Run**: `./scripts/release.sh <version>` (on the build server: push `release/<version>`)
5. **Script validates alignment and the changelog section** → builds → signs → notarizes → tags

## Rules

| Do | Don't |
|----|-------|
| Bump version in both files | Ship a DMG without bumping the version |
| Use semantic versioning | Bump the app without bumping the daemon, or vice versa |
| Create annotated git tags | Skip version numbers (0.2.0 → 0.4.0) |
