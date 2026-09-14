### API Patterns

When working on the Rust API (`rust/banshee-api`):

- The API is the ONLY process that opens the database. CLI, MCP, and the app
  all speak HTTP to it. Do not add a second writer.
- All endpoints use camelCase JSON with explicit nulls — the wire format is a
  contract (`docs/wire-format.md`).
- New endpoints need key-file auth (`X-Api-Key`); only `/health` is exempt,
  because process supervisors probe it before the key exists. Routes go in the
  `keyed` router in `routes::router` — that is what gets the key check over the
  socket and the blanket refusal over TCP (ADR-0008 Phase 3). A route added
  outside it is reachable over the port with no key.
- Map `CoreError` variants through `ApiError` (`routes.rs`): NotFound → 404,
  InvalidInput → 400, everything else → 500 with a generic message. Never leak
  internals in error bodies.
- Request DTOs use `#[serde(deny_unknown_fields)]` — a client sending
  snake_case keys must get a loud 422, not silent data loss.
- Datetimes only via `banshee_core::wire_time` (6-digit micros, generous
  decode). `chrono`'s default serde is NOT the wire format.
- New route → integration test in `tests/` driving real HTTP against a temp
  DB, plus fixtures in `tests/fixtures/` if the wire shape is new.
