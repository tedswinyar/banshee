# Data safety — SQLite conventions

The user's data lives in one SQLite file. These conventions exist because
each of their absences has destroyed someone's data somewhere; Banshee has
followed them since its first migration.

## Schema versioning: forward-only

- The schema version lives in SQLite's `user_version` pragma.
  `schema::validate_or_init` (`rust/banshee-core/src/schema.rs`) applies
  pending migrations on open and **refuses databases newer than the build**.
- Migrations are additive, one version step per function arm, and never
  rewritten once shipped. Downgrading the app against an upgraded database is
  a refusal, not a best-effort — silent corruption is worse than an error.
- Every migration ships with a test proving existing rows survive it
  (`v1_database_migrates_forward_to_v2` is the worked example).

## Backups: not a backup until restored

`backup::backup_verified` (`rust/banshee-core/src/backup.rs`) is the only
approved snapshot path:

1. Uses SQLite's online backup API (consistent even mid-write).
2. **Re-opens the copy out-of-place**, validates its schema, and compares row
   counts before reporting success.
3. Refuses to overwrite an existing backup file.

A copy that has never been opened somewhere else is a hope, not a backup.

**How to take one:** `banshee backup` (or `POST /backup`). The
daemon owns the file — only it opens the DB (ADR-0001) — so it chooses a
timestamped path in a `backups/` directory beside the live database, verifies
the copy, and prints where it landed. There is deliberately **no MCP tool** and
**no caller-supplied path**: snapshotting is operator maintenance, and a path
from the wire would let any key holder make the daemon write anywhere its uid
can. Copy the verified snapshot to external media yourself for off-machine
safety. Nothing prunes `backups/` — snapshots are operator-initiated and rare,
so they do not accumulate on their own.

Restore drills belong in the release checklist: before each release, restore
the latest backup into a temp profile and open it. See
[release-checklist.md](release-checklist.md).

## The API owns the file

Exactly one process opens the database: `banshee-api`. The CLI, MCP
server, and app go through HTTP. This is a data-safety property, not just an
architecture taste — SQLite corruption stories usually start with two
writers.

Test-profile guardrail: `BANSHEE_PROFILE=test` refuses any database path
under the prod data directory (`config.rs`), so a mis-exported env var in a
test run cannot touch real data.

## When you add a persisted field

Keep all three properties: `user_version` migrations (forward-only, tested),
`backup_verified` semantics, single-writer via the API. The shapes are in
place; extend them rather than re-deriving.
