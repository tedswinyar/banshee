# ADR-0002: Infrastructure split as pinned dependencies (deferred)

_Template-lineage record, kept for the decision trail; none of the files named below exist in Banshee._

- **Status**: proposed, deferred (captured for future decision)
- **Date**: 2026-08-20

## Context

The project template conflates two very different kinds of code into one copy-forward blob that gets copied into every descendant project:

1. **Reusable infrastructure** (~70% of the codebase) — should be shared and versioned:
   - `wire_time.rs`, `WireDate.swift` (wire format implementation)
   - `config.rs` (profile resolution, prod-dir symlink guard)
   - `auth.rs` (key-file generation)
   - `schema.rs` (the migration runner machinery, not the domain migrations)
   - `backup.rs` (backup verification, restore testing)
   - `APIServerManager.swift` (supervisor, port announcement parser)
   - Port ladder logic
   - The entire `scripts/verify.sh` gate harness
   - Hook installation and verification

2. **Example domain** (~30%) — should diverge per project:
   - `note.rs`, the `/notes` routes
   - Note CLI/MCP verbs
   - Note fixtures and tests

**The problem**: Copy-forward is correct for bucket #2 and wrong for bucket #1. A bug fixed in the symlink-resolving prod-dir guard (which was itself an adversarial finding dated 2026-08-20 — this infra is still discovering bugs) does not propagate to already-instantiated projects. The template's backport protocol is honest ("forward-porting is manual and optional") but manual+optional propagation of security-relevant infra fixes across N projects is exactly how the oldest project ends up running the buggy guard.

**Discovered during**: an architecture review on 2026-08-20

## The tension

At **2–3 personal projects**: manual backport is cheaper than publishing versioned artifacts, and the containment (issue tracking + sentinel-term porting + test-init green bar) holds. The cost of maintaining published crates/packages exceeds the benefit.

At **~8+ projects**: you have material infra drift and no tooling tells you which project is behind. The fix is no longer "backport this one thing" — it's "which of these 8 projects has the v3 guard vs the v2 guard vs the original?"

## Decision

**DEFERRED**. The template currently uses copy-forward for all code (infra + domain). This decision is deferred until the number of descendant projects crosses the threshold where manual backport becomes untenable.

When that threshold is reached (estimated ~5-8 active descendant projects), revisit with the solution described below.

## The deferred solution (for when we revisit)

Split the buckets:

1. **Publish generic Rust infrastructure** as a pinned path or git crate (`core-infra`) containing:
   - `wire_time`, `config`, `auth`, `schema` (runner), `backup`
   - Port ladder, profile resolution, data-safety guards
   - Everything that is generic, not domain-specific

2. **Publish generic Swift infrastructure** as a SwiftPM package containing:
   - `WireDate`, `APIServerManager`, `APIClient` base
   - Design tokens base (if generic)

3. **Copy only**: Domain slice + wiring
   - `note.rs` and its routes, store, tests
   - Domain-specific migrations (which import the runner from the infra crate)
   - CLI/MCP verbs
   - Fixtures

The template can still `init.sh` the wiring; it just stops copying `wire_time`/`config`/`auth`/`APIServerManager` by value. Descendant projects get them as versioned dependencies, and a fix to the prod-dir guard propagates by bumping the dependency version.

## Consequences of the current (deferred) approach

**Accept for now** (N ≤ 3 projects):
- Infra fixes don't propagate automatically
- Each project has a frozen snapshot of infra at the time it was created
- Backports are manual and optional (tracked in the template-lineage notes, `TEMPLATE.md`, kept in the maintainer's notebook)
- The template's own stamping test proves the template ITSELF still instantiates green, but proves nothing about descendants

**Costs at scale** (N ≥ 8 projects):
- No tooling tells you which project has which infra version
- A security fix requires manual application to N projects
- Divergence accumulates (some projects get the fix, some don't)
- The oldest project is most likely to run the buggiest infra

**The question for the maintainer at N=5**: Is maintaining published crates/packages now cheaper than tracking manual backports to 5+ projects?

## Tradeoffs when we DO split (for the record)

**Pros**:
- Infra fixes propagate by version bump (one-line change in Cargo.toml / Package.swift)
- Descendants inherit improvements without manual port
- Audit surface: "which projects are behind?" becomes `grep core-infra Cargo.toml`

**Cons**:
- Maintaining published artifacts (versioning, breaking changes, changelog)
- Coordination cost: updating the infra crate means testing against all descendant projects
- Blessing the infra API: once published, changes become breaking changes
- Still no automatic propagation — projects must opt into version bumps

**Why not do it now?**
- Overhead of maintaining published artifacts exceeds benefit at N=2
- The infra API is still stabilizing (symlink guard was added 2026-08-20)
- Early descendant projects are close enough to manual-track

## References

- Architecture review of 2026-08-20 (finding H1)
- Backport protocol: `TEMPLATE.md` (maintainer's notebook)
- Current infra code identified for future split:
  - Rust: `rust/banshee-core/src/wire_time.rs`, `config.rs`, `auth.rs`, `schema.rs`, `backup.rs`
  - Swift: `swift/Sources/BansheeCore/WireDate.swift`, `APIServerManager.swift`
  - Scripts: `scripts/verify.sh`, `scripts/install-hooks.sh`, `scripts/check-hooks.sh`
