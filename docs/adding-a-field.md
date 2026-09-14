# Adding a field — the touch-point checklist

The most common whole-system change, enumerated so nobody reconstructs it by
grep (a fresh contributor spent most of their discovery time doing exactly
that, 2026-08-19). Example used throughout: a `pressure_score: f64` on `Pressure` — a
single 0–100 summary number.

**Read this first if the field is DERIVED.** Anything a client could compute from
what it already has still belongs on the wire if it is a REDUCTION — a level, a
band, a glyph, a label, a ranking (ADR-0005). The test is not "could a client
compute it" but "would two clients computing it independently ever disagree".

## Core (rust/banshee-core)

- [ ] `pressure/mod.rs` (or `sample.rs` / `census/mod.rs`) — field on the struct;
      nullable ⇒ `Option`, which is present-as-null on the wire.
- [ ] `pressure/config.rs` — if the field is tunable, it is a `PressureConfig`
      field, not a constant. Thresholds that live as constants get tuned by
      whoever is willing to recompile, and then nobody can see what the daemon is
      actually using.
- [ ] `schema.rs` — a new forward-only migration arm ONLY if the field is
      persisted; bump `CURRENT_VERSION`; a migration test proving existing rows
      survive. `Pressure` is **not** persisted (it is recomputed from history), so
      a field on it needs no migration — which is most of the reason it is cheap
      to add one.
- [ ] `sample_store.rs` — INSERT/SELECT columns and the `row_to_*` mapper, for a
      persisted field. Validate ranges in code, not with SQL CHECK constraints.
      Any `u64` goes through `clamp_i64`.
- [ ] Tests: fixture-decode from RAW BYTES, plus the every-error-branch cases.

## API (rust/banshee-api)

- [ ] `routes.rs` — nothing, for a field on an existing payload: handlers serialize
      whole core types. A new *query parameter* is a `parse_*` helper plus a 400
      path, and is typed as a STRING so every bad value reaches our validator
      instead of a stock text/plain `Query` rejection.
- [ ] `tests/test_pressure_api.rs` — round-trip over real HTTP, plus the
      camelCase-at-depth and present-as-null assertions.

## The client surfaces — ALL of them

Parity is a hard gate. `tests/e2e/run-e2e.sh` iterates one table of
`route | cli subcommand | mcp tool` triples and fails if any surface is missing.

- [ ] CLI — render it in the human view (`rust/banshee-cli/src/main.rs`).
      `--json` needs no change: it relays the server's bytes verbatim.
- [ ] MCP — nothing for a field on an existing payload (tool results are the API's
      bytes unchanged). A new query parameter needs the `inputSchema` property AND
      the `query_from` key list. Put anything an agent could get WRONG in the
      description, not just the schema.
- [ ] Swift — the model struct, the `CodingKeys`, **and the hand-written
      `encode(to:)`** (present-as-null binds the encoder; the synthesized one drops
      nil keys), plus `MockAPIClient`'s stub helper.

## Shared contract surfaces

- [ ] `tests/fixtures/pressure-*.json` — add the field to ALL of them (exact
      bytes). The fully-populated `pressure-shrieking.json` must carry a non-null
      value for it, or the encoder key-set tests cannot see it.
- [ ] `tests/e2e/run-e2e.sh` — a parity assertion, if the field carries an
      invariant worth stating (e.g. "a suffix appears exactly when a source does").
- [ ] `docs/wire-format.md` — the field table, and any new rule the field implies.

## Prove it

- [ ] `./scripts/verify.sh` fully green
- [ ] Mutation pass per the testing standard (see `CONTRIBUTING.md`), via
      `scripts/mutate.sh` — which runs `cargo test` or `swift test` depending on
      the file path, and refuses to report a green run it cannot vouch for.
      Mutate the validator bounds, the serialization, and each client's plumbing
      of the field; every mutation must be caught by a NAMED test.
- [ ] Ask of every new pin: **what other implementation would also pass this?**
      Five false pins in this project have had the same cause — a fixture where
      two properties co-vary, making the thing under test indistinguishable from
      something else.
