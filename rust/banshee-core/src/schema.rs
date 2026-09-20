// Schema versioning: forward-only migrations tracked via SQLite's
// user_version pragma. A database is never migrated backwards; restoring
// an older app version against a newer database refuses loudly instead of
// corrupting silently. (See docs/data-safety.md for the conventions.)

use rusqlite::Connection;

use crate::{CoreError, Result};

/// Bump this when adding a migration below.
pub const CURRENT_VERSION: i64 = 15;

/// Validate an existing database or initialize a fresh one, applying any
/// pending forward migrations. Refuses databases newer than this build.
pub fn validate_or_init(conn: &Connection) -> Result<()> {
    apply_connection_pragmas(conn)?;

    let version: i64 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;

    if version > CURRENT_VERSION {
        return Err(CoreError::Schema(format!(
            "database is schema v{version} but this build supports at most \
             v{CURRENT_VERSION}; refusing to open (upgrade the app, do not \
             downgrade the database)"
        )));
    }

    let mut v = version;
    while v < CURRENT_VERSION {
        apply_migration(conn, v)?;
        v += 1;
    }
    Ok(())
}

/// Per-connection invariants. These are NOT stored in the database file — they
/// are reset to SQLite's defaults on every new connection — so they must be
/// applied by whatever opens the DB, which is why they live here rather than in
/// a store constructor.
///
/// **`foreign_keys` is OFF by default in SQLite.** Without it, the
/// `ON DELETE CASCADE` on `volume_samples` and `volume_rollups` silently does
/// nothing: the retention sweep would delete parent rows and leave orphaned
/// child rows accumulating forever — an unbounded leak in the one table whose
/// whole purpose is to be bounded (ADR-0006). It fails silently and looks fine.
///
/// WAL lets the sample-writing loop and a polling reader coexist without the
/// reader blocking the writer. It is a no-op on `:memory:` databases, which
/// report journal_mode `memory`; that is expected, not a failure.
pub(crate) fn apply_connection_pragmas(conn: &Connection) -> Result<()> {
    conn.execute_batch("PRAGMA journal_mode = WAL; PRAGMA foreign_keys = ON;")?;

    // Verify rather than assume. A pragma that silently failed to take is
    // exactly the class of bug this guards against, and the check is free.
    let fk: i64 = conn.pragma_query_value(None, "foreign_keys", |r| r.get(0))?;
    if fk != 1 {
        return Err(CoreError::Schema(
            "PRAGMA foreign_keys did not take on this connection; ON DELETE \
             CASCADE would silently leak orphaned rows"
                .into(),
        ));
    }
    Ok(())
}

/// Apply one migration step (`from` → `from + 1`) and its `user_version`
/// bump as a SINGLE transaction. `user_version` is transactional in SQLite,
/// so a crash anywhere inside this call rolls back BOTH the DDL and the
/// version bump — the database is left at `from`, cleanly re-runnable. The
/// old shape (DDL auto-commits, then a separate bump) had a window where a
/// crash left the schema ahead of the version, wedging the DB un-openable
/// (`table notes already exists` / `duplicate column`) on the next boot.
pub(crate) fn apply_migration(conn: &Connection, from: i64) -> Result<()> {
    // `unchecked_transaction` gives us a transaction over `&Connection`
    // (we do not own it mutably here); dropped-without-commit == rollback.
    let tx = conn.unchecked_transaction()?;
    migrate_step(&tx, from)?;
    tx.pragma_update(None, "user_version", from + 1)?;
    tx.commit()?;
    Ok(())
}

/// Apply the single migration from `from` to `from + 1`.
///
/// One function, one version step, and DDL that is additive except where a
/// version deliberately retires a table (v6). The steps that build and then drop
/// the template's `notes` table are all kept: **migration history is not
/// refactorable.** A database created before v6 sits at v1–v5 with real `notes`
/// rows in it, and it can only reach head by replaying the same chain. Editing v1
/// to stop creating the table would leave v6's `DROP` with nothing to drop on a
/// fresh database and everything to drop on an old one — two schemas wearing the
/// same version number, which is the failure `user_version` exists to prevent.
fn migrate_step(conn: &Connection, from: i64) -> Result<()> {
    match from {
        0 => {
            // v1: initial schema.
            conn.execute_batch(
                "CREATE TABLE notes (
                    id         TEXT PRIMARY KEY,
                    title      TEXT NOT NULL,
                    body       TEXT,
                    created_at TEXT NOT NULL
                );",
            )?;
        }
        1 => {
            // v2: notes gain a closed_at state transition.
            conn.execute_batch("ALTER TABLE notes ADD COLUMN closed_at TEXT;")?;
        }
        2 => {
            // v3: the pressure domain — raw samples, 5-minute rollups, and
            // alert events. All THREE retention tiers exist from the first
            // migration that introduces any of them (ADR-0006), because this
            // app's writer is a timer rather than a person: ~5,760 samples a
            // day, and bolting downsampling on later means a migration over
            // millions of rows.
            //
            // Columns stay bare — ranges are validated in code, not by CHECK
            // constraints (docs/adding-a-field.md).
            conn.execute_batch(
                "CREATE TABLE samples (
                    id                TEXT PRIMARY KEY,
                    sampled_at        TEXT    NOT NULL,
                    boot_time_secs    INTEGER NOT NULL,
                    ncpu              INTEGER NOT NULL,
                    mem_total_bytes   INTEGER NOT NULL,
                    page_size         INTEGER NOT NULL,
                    load_1m           REAL    NOT NULL,
                    load_5m           REAL    NOT NULL,
                    load_15m          REAL    NOT NULL,
                    swap_total_bytes  INTEGER NOT NULL,
                    swap_used_bytes   INTEGER NOT NULL,
                    pages_free        INTEGER NOT NULL,
                    pages_active      INTEGER NOT NULL,
                    pages_inactive    INTEGER NOT NULL,
                    pages_wired       INTEGER NOT NULL,
                    pages_speculative INTEGER NOT NULL,
                    pages_compressor  INTEGER NOT NULL,
                    swapins           INTEGER NOT NULL,
                    swapouts          INTEGER NOT NULL
                );
                -- Every query is 'the most recent N' or 'this time window', and
                -- the retention sweep deletes by age. Without this index both
                -- are full scans over a table that grows all day.
                CREATE INDEX samples_sampled_at ON samples (sampled_at);

                CREATE TABLE volume_samples (
                    sample_id   TEXT    NOT NULL REFERENCES samples(id) ON DELETE CASCADE,
                    mount_point TEXT    NOT NULL,
                    total_bytes INTEGER NOT NULL,
                    avail_bytes INTEGER NOT NULL,
                    PRIMARY KEY (sample_id, mount_point)
                );

                CREATE TABLE sample_rollups (
                    bucket_start         TEXT PRIMARY KEY,
                    sample_count         INTEGER NOT NULL,
                    load_1m_avg          REAL    NOT NULL,
                    load_1m_max          REAL    NOT NULL,
                    swap_used_avg        INTEGER NOT NULL,
                    swap_used_max        INTEGER NOT NULL,
                    pages_free_min       INTEGER NOT NULL,
                    pages_compressor_max INTEGER NOT NULL,
                    -- NULL when a reboot inside the bucket voided the delta, or
                    -- a single sample left nothing to diff. Absence of a number
                    -- is a real state here, not a zero.
                    swapins_delta        INTEGER,
                    swapouts_delta       INTEGER
                );

                CREATE TABLE volume_rollups (
                    bucket_start TEXT    NOT NULL REFERENCES sample_rollups(bucket_start) ON DELETE CASCADE,
                    mount_point  TEXT    NOT NULL,
                    total_bytes  INTEGER NOT NULL,
                    avail_min    INTEGER NOT NULL,
                    avail_avg    INTEGER NOT NULL,
                    PRIMARY KEY (bucket_start, mount_point)
                );

                -- Populated by the pressure model, which came later. Created here so the
                -- retention sweep covers all three tiers from day one rather
                -- than being extended (and forgotten) per table.
                CREATE TABLE alert_events (
                    id          TEXT PRIMARY KEY,
                    occurred_at TEXT NOT NULL,
                    dimension   TEXT NOT NULL,
                    level       TEXT NOT NULL,
                    detail      TEXT NOT NULL
                );
                CREATE INDEX alert_events_occurred_at ON alert_events (occurred_at);",
            )?;
        }
        3 => {
            // v4: the census tier (the 5-minute ps/tmux/lsof reading).
            //
            // Deliberately NOT rolled up. A rollup answers "what was the average
            // over five minutes"; a census is a LIST of named things (which
            // sessions, which agents), and there is no meaningful average of a
            // session list. So the census keeps the same 24h raw window as the
            // samples and then goes away. Longer-horizon census trends come
            // from scalar counters that CAN be rolled up — filed here as
            // `banshee-yj1` rather than speculatively built, and built by v9.
            conn.execute_batch(
                "CREATE TABLE censuses (
                    id                 TEXT PRIMARY KEY,
                    taken_at           TEXT    NOT NULL,
                    total_procs        INTEGER NOT NULL,
                    -- 0/1. Distinguished from 'zero sessions' so the UI never
                    -- implies a clean machine when it could not look.
                    tmux_available     INTEGER NOT NULL,
                    ide_helper_count   INTEGER NOT NULL,
                    ide_helper_rss     INTEGER NOT NULL,
                    orphan_total_count INTEGER NOT NULL,
                    orphan_total_rss   INTEGER NOT NULL,
                    orphan_count       INTEGER NOT NULL,
                    orphan_rss         INTEGER NOT NULL
                );
                CREATE INDEX censuses_taken_at ON censuses (taken_at);

                CREATE TABLE census_agent_sessions (
                    census_id TEXT    NOT NULL REFERENCES censuses(id) ON DELETE CASCADE,
                    pid       INTEGER NOT NULL,
                    program   TEXT    NOT NULL,
                    rss_bytes INTEGER NOT NULL,
                    age_secs  INTEGER NOT NULL,
                    age_days  INTEGER NOT NULL,
                    cpu_secs  REAL    NOT NULL,
                    tty       TEXT    NOT NULL,
                    is_stale  INTEGER NOT NULL,
                    -- NULL for non-stale sessions: lsof is not run for them, so
                    -- absence here means 'not looked at', not 'no cwd'.
                    cwd       TEXT,
                    PRIMARY KEY (census_id, pid)
                );

                CREATE TABLE census_orphan_programs (
                    census_id TEXT    NOT NULL REFERENCES censuses(id) ON DELETE CASCADE,
                    program   TEXT    NOT NULL,
                    count     INTEGER NOT NULL,
                    rss_bytes INTEGER NOT NULL,
                    PRIMARY KEY (census_id, program)
                );

                CREATE TABLE census_tmux_sessions (
                    census_id    TEXT    NOT NULL REFERENCES censuses(id) ON DELETE CASCADE,
                    name         TEXT    NOT NULL,
                    pane_command TEXT    NOT NULL,
                    -- NULL when the activity timestamp was unknown, which is NOT
                    -- the same as 'idle forever'.
                    idle_days    REAL,
                    is_busy      INTEGER NOT NULL,
                    is_stale     INTEGER NOT NULL,
                    PRIMARY KEY (census_id, name)
                );

                CREATE TABLE census_app_groups (
                    census_id  TEXT    NOT NULL REFERENCES censuses(id) ON DELETE CASCADE,
                    name       TEXT    NOT NULL,
                    proc_count INTEGER NOT NULL,
                    rss_bytes  INTEGER NOT NULL,
                    PRIMARY KEY (census_id, name)
                );

                CREATE TABLE census_monitor_agents (
                    census_id           TEXT    NOT NULL REFERENCES censuses(id) ON DELETE CASCADE,
                    name                TEXT    NOT NULL,
                    proc_count          INTEGER NOT NULL,
                    rss_bytes           INTEGER NOT NULL,
                    cpu_secs_total      REAL    NOT NULL,
                    percent_of_one_core REAL    NOT NULL,
                    PRIMARY KEY (census_id, name)
                );",
            )?;
        }
        4 => {
            // v5: the monitoring-agent numbers gain the denominator and a
            // DEDUPLICATED total.
            //
            // Both came from reading real output: one pid matched two configured
            // patterns, so summing the per-agent rows over-counted; and a log
            // shipper restarted 109s earlier reported 14.6% off 16s of CPU, so a
            // consumer needs the lifetime to know whether the ratio means
            // anything yet.
            conn.execute_batch(
                "ALTER TABLE census_monitor_agents ADD COLUMN longest_life_secs INTEGER NOT NULL DEFAULT 0;
                 ALTER TABLE censuses ADD COLUMN monitor_proc_count INTEGER NOT NULL DEFAULT 0;
                 ALTER TABLE censuses ADD COLUMN monitor_rss INTEGER NOT NULL DEFAULT 0;
                 ALTER TABLE censuses ADD COLUMN monitor_cpu_secs REAL NOT NULL DEFAULT 0;
                 ALTER TABLE censuses ADD COLUMN monitor_percent REAL NOT NULL DEFAULT 0;
                 ALTER TABLE censuses ADD COLUMN monitor_longest_life INTEGER NOT NULL DEFAULT 0;",
            )?;
        }
        5 => {
            // v6: retire the template's `notes` table.
            //
            // The `Note` entity was the upstream template's demonstration slice — a real,
            // tested, deliberately trivial domain that proved the shape of every
            // layer. The pressure domain replaced it at schema v6, and
            // leaving the table behind would be worse than dropping it: a future
            // reader finds a `notes` table in a system-monitoring database and has
            // to prove to themselves that nothing writes it. `/health` probed it
            // until this commit, so "unused" would not even have been true.
            //
            // The one DESTRUCTIVE migration in the project, and deliberately so.
            // What it destroys is demonstration data — three seeded rows in a dev
            // database, at a point where Banshee has no users but its author. If
            // that ever stops being true, the honest move for a retirement like
            // this is to export first, not to keep the table forever.
            conn.execute_batch("DROP TABLE IF EXISTS notes;")?;
        }
        6 => {
            // v7: the availability counters (`banshee-cot`). The memory
            // dimension banded on `pages_free`, which macOS keeps deliberately
            // tiny — a healthy machine reads 100–500 MB "free" forever, and the
            // dimension sat red while the kernel's own pressure level said
            // NORMAL. Availability needs the purgeable and file-backed counts,
            // which were in the same `host_statistics64` struct all along,
            // just not persisted.
            //
            // DEFAULT 0 makes pre-v7 rows compute available = free +
            // speculative — an UNDERCOUNT, which errs toward the old (alarmist)
            // reading rather than inventing headroom history never recorded.
            conn.execute_batch(
                "ALTER TABLE samples ADD COLUMN pages_purgeable INTEGER NOT NULL DEFAULT 0;
                 ALTER TABLE samples ADD COLUMN pages_external  INTEGER NOT NULL DEFAULT 0;",
            )?;
        }
        7 => {
            // v8: the kernel's own memory verdict (`banshee-0g4`).
            // `kern.memorystatus_vm_pressure_level` — 1 normal, 2 warn,
            // 4 critical — measured at WARN while `available_bytes` was
            // comfortably green (banshee-10w parity run), so it is a signal in
            // its own right, banded as the tenth dimension.
            //
            // DEFAULT 0 means "not recorded", NOT "normal": the kernel never
            // returns 0, and the pressure model SKIPS zero readings rather than
            // letting pre-v8 rows claim the kernel said all-clear.
            conn.execute_batch(
                "ALTER TABLE samples ADD COLUMN memory_pressure_level INTEGER NOT NULL DEFAULT 0;",
            )?;
        }
        8 => {
            // v9: census scalar counters ride the rollup tier (`banshee-yj1`).
            // The census itself stays a 24h list (v4's decision stands — there
            // is no meaningful average of a session list), but the NUMBERS the
            // pressure model derives from it (stale sessions, orphans, the
            // deduplicated monitor percent, total procs) roll up honestly, so
            // "have my orphans been trending up this month?" becomes
            // answerable. The 2026-08-04 incident built over DAYS; the 24h
            // window was structurally blind to it.
            //
            // NULLABLE, no default: NULL means "no census fell in this bucket"
            // (or the rollup pre-dates v9) — a client rendering NULL as 0
            // invents a clean machine, same rule as the voided swap deltas.
            conn.execute_batch(
                "ALTER TABLE sample_rollups ADD COLUMN stale_sessions_max  INTEGER;
                 ALTER TABLE sample_rollups ADD COLUMN orphans_max         INTEGER;
                 ALTER TABLE sample_rollups ADD COLUMN monitor_percent_max REAL;
                 ALTER TABLE sample_rollups ADD COLUMN total_procs_max     INTEGER;",
            )?;
        }
        9 => {
            // v10: alerts become EPISODES, not point events (ADR-0009,
            // `banshee-87l.1`). An episode is one continuous incident on one
            // dimension — start, end (NULL = still open), peak, census-at-peak,
            // suppressed count, lifecycle state — so the record can tell 4
            // storms from 40 (`banshee-nvd`) and status can show the long
            // memory (`banshee-qoz`).
            //
            // The whole episode lives as JSON in `detail`, exactly as
            // `alert_events` stores its whole event — so the episode shape can
            // GROW without a further migration. Only the columns that queries
            // and the retention sweep need are typed: `dimension` (per-source
            // reads), `started_at` (retention cutoff, newest-first ordering),
            // and `ended_at` (the open-episode query is `WHERE ended_at IS
            // NULL`).
            //
            // This arm has RUN ON PROD (installed 2026-09-08; ADR-0009 decision
            // 10 as amended). Append nothing to it: a forward-only migration
            // never re-runs at the same version, so a column added here would
            // exist on fresh databases and be missing on every machine that
            // already migrated. The thermal dimension (v11) and the memory second
            // pass (v13) take their own arms, and the same rule applies to each
            // once shipped.
            conn.execute_batch(
                "CREATE TABLE alert_episodes (
                     id         TEXT PRIMARY KEY,
                     dimension  TEXT NOT NULL,
                     started_at TEXT NOT NULL,
                     ended_at   TEXT,
                     detail     TEXT NOT NULL
                 );
                 CREATE INDEX alert_episodes_started_at
                     ON alert_episodes (started_at);
                 CREATE INDEX alert_episodes_open
                     ON alert_episodes (ended_at) WHERE ended_at IS NULL;",
            )?;
        }
        10 => {
            // v11: the kernel's THERMAL verdict rides the sample (the
            // eleventh dimension). `thermal_pressure_level` is the macOS
            // OSThermalPressureLevel enum (0 nominal … 4 sleeping) read via
            // notify_get_state; `cpu_speed_limit_percent` is IOKit's
            // CPU_Speed_Limit, which Apple silicon does not publish.
            //
            // Both NULLABLE, no DEFAULT: unlike memory_pressure_level (where the
            // kernel never returns 0, so 0 could mean "not recorded"), 0 is a
            // REAL thermal reading — nominal — so the only honest marker for a
            // pre-v11 row is NULL, and the model skips NULL rather than reading it
            // as calm. Its own arm (ADR-0009 dec. 10 as amended): v10 has run on
            // prod, so nothing is appended there.
            conn.execute_batch(
                "ALTER TABLE samples ADD COLUMN thermal_pressure_level INTEGER;
                 ALTER TABLE samples ADD COLUMN cpu_speed_limit_percent INTEGER;",
            )?;
        }
        11 => {
            // v12: per-core CPU tick counters ride the sample (`banshee-87l.19`).
            // The CPU dimension banded on load-average-per-core, which on macOS
            // counts runnable threads — so a machine whose cores sat idle under a
            // churning run queue (measured: a security agent's short blocking
            // threads) read RED "CPU saturation 🔥" while Activity Monitor showed
            // the cores half idle. `cpu_ticks_total` (user+system+idle+nice) and
            // `cpu_ticks_idle` are the cumulative `host_processor_info` counters;
            // the model derives MEASURED utilization from their delta and bands on
            // that instead, so idle cores read green no matter how high the queue.
            //
            // Both NULLABLE, no DEFAULT — same reasoning as the v11 thermal
            // columns: 0 is a real tick count, so the only honest marker for a
            // pre-v12 row is NULL, and the model skips a sample whose utilization
            // cannot be computed rather than reading it as idle. Its own arm: v11
            // has shipped, so nothing is appended there.
            conn.execute_batch(
                "ALTER TABLE samples ADD COLUMN cpu_ticks_total INTEGER;
                 ALTER TABLE samples ADD COLUMN cpu_ticks_idle INTEGER;",
            )?;
        }
        12 => {
            // v13: the memory second pass (`banshee-87l.3`). Two kernel
            // signals the model did not read. `kernel_free_percent`
            // (`kern.memorystatus_level`) is the kernel's OWN reclaimability
            // percentage — a different quantity from both `memory_pressure_level`
            // and `available_bytes` (measured 40% vs 11.5% at the same instant),
            // so it is a signal in its own right, not a restatement.
            // `jetsam_kills` (`kern.memorystatus.kill_on_sustained_pressure_count`)
            // is a lifetime counter of processes the kernel killed under sustained
            // pressure; a positive delta is Shrieking-grade because the machine
            // already lost a process.
            //
            // Both NULLABLE, no DEFAULT — same reasoning as the v11 thermal and
            // v12 tick columns: 0 is a real reading (a healthy machine reports 0
            // jetsam kills), so the only honest marker for a pre-v13 row is NULL.
            //
            // The memory pass ships STANDALONE as v13 (prod installed 2026-09-09,
            // ahead of the limits dimension, to become the daily driver). The
            // earlier plan had the limits dimension share this arm so prod migrated
            // once; that was reversed when the memory pass shipped first. The
            // next migration therefore gets its OWN arm and bumps
            // CURRENT_VERSION — it must NOT append to THIS step, because prod
            // is already at v13 and would never re-run it. (That arm is the
            // `13 =>` CPU-consumers migration below. v15 in the event became
            // the census `pressure_kills` column (`banshee-2dq`), not limits.)
            conn.execute_batch(
                "ALTER TABLE samples ADD COLUMN kernel_free_percent INTEGER;
                 ALTER TABLE samples ADD COLUMN jetsam_kills INTEGER;",
            )?;
        }
        13 => {
            // v14: the CPU/thermal who-line's recent-rate consumers
            // (`banshee-aen`). The cumulative-CPU lens named only agent sessions
            // and the managed-agent rollup, so at 800% busy the finding accounted
            // for 78% of one core and never named Word, clippy or WindowServer —
            // the desktop apps and build tools the census sizes but never timed.
            // The census now differences per-pid `time=` between its two most
            // recent cycles and stores the ranked result here (a rate, not a
            // snapshot — see `census::CpuConsumer`). A child table, cascading with
            // its census like every other census list; empty rows are normal (the
            // first census after a start has no predecessor to difference).
            conn.execute_batch(
                "CREATE TABLE census_cpu_consumers (
                    census_id           TEXT    NOT NULL REFERENCES censuses(id) ON DELETE CASCADE,
                    rank                INTEGER NOT NULL,
                    name                TEXT    NOT NULL,
                    proc_count          INTEGER NOT NULL,
                    percent_of_one_core REAL    NOT NULL,
                    PRIMARY KEY (census_id, rank)
                );",
            )?;
        }
        14 => {
            // v15: the honest jetsam counter (`banshee-2dq`). The memory-kills
            // dimension banded on `samples.jetsam_kills`, a sysctl lifetime
            // counter — but a positive delta there fires on ANY jetsam, including
            // the routine `idle-exit rf:low` reaping that macOS does on a healthy
            // machine, so the dimension was a false-positive generator. The census
            // now parses `/usr/bin/log show` for genuine sustained-pressure kills
            // (excluding idle-exit noise) over the gap since the previous census,
            // and stores that per-window count here. NULLABLE, no DEFAULT — 0 is a
            // real reading (a healthy machine reports 0 pressure kills), so the
            // only honest marker for a pre-v15 census is NULL. The sysctl column
            // stays as a cheap secondary observable but no longer drives the band.
            conn.execute_batch("ALTER TABLE censuses ADD COLUMN pressure_kills INTEGER;")?;
        }
        other => {
            return Err(CoreError::Schema(format!(
                "no migration step defined from v{other}"
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mem() -> Connection {
        Connection::open_in_memory().unwrap()
    }

    #[test]
    fn fresh_database_initializes_to_current_version() {
        let conn = mem();
        validate_or_init(&conn).unwrap();
        let v: i64 = conn
            .pragma_query_value(None, "user_version", |r| r.get(0))
            .unwrap();
        assert_eq!(v, CURRENT_VERSION);
    }

    /// The historical v1→v2 step, exercised as ONE step rather than by running to
    /// head: v6 drops `notes`, so a full `validate_or_init` can no longer observe
    /// the column this step adds. Kept because it is the oldest link in the chain
    /// and a v1 database in the wild must still traverse it.
    #[test]
    fn the_v1_to_v2_step_adds_closed_at_without_losing_rows() {
        let conn = mem();
        // Hand-build a v1 database (no closed_at column).
        conn.execute_batch(
            "CREATE TABLE notes (
                id TEXT PRIMARY KEY, title TEXT NOT NULL,
                body TEXT, created_at TEXT NOT NULL
            );
            PRAGMA user_version = 1;",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO notes (id, title, created_at)
             VALUES ('x', 'pre-migration note', '2026-01-01T00:00:00.000000Z')",
            [],
        )
        .unwrap();

        apply_migration(&conn, 1).unwrap();

        // Existing rows survive and the new column reads as NULL.
        let (title, closed): (String, Option<String>) = conn
            .query_row(
                "SELECT title, closed_at FROM notes WHERE id = 'x'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(title, "pre-migration note");
        assert_eq!(closed, None);
    }

    /// The template's demonstration table is GONE at head, and gone by migration
    /// rather than by never having existed — so a pre-v6 database converges on the
    /// same schema as a fresh one.
    ///
    /// Mutation-proof, verified: delete the v6 arm's `DROP TABLE` and this fails
    /// with `notes` still present. Both halves matter: the fresh case catches a
    /// missing drop, and the v1 case catches a drop that only runs on new
    /// databases.
    #[test]
    fn the_notes_table_is_retired_at_head_from_both_starting_points() {
        let fresh = mem();
        validate_or_init(&fresh).unwrap();
        assert!(
            !table_exists(&fresh, "notes"),
            "fresh database: notes must be gone"
        );

        let legacy = mem();
        legacy
            .execute_batch(
                "CREATE TABLE notes (
                    id TEXT PRIMARY KEY, title TEXT NOT NULL,
                    body TEXT, created_at TEXT NOT NULL
                );
                PRAGMA user_version = 1;",
            )
            .unwrap();
        legacy
            .execute(
                "INSERT INTO notes (id, title, created_at)
                 VALUES ('x', 'demo', '2026-01-01T00:00:00.000000Z')",
                [],
            )
            .unwrap();

        validate_or_init(&legacy).unwrap();
        assert!(
            !table_exists(&legacy, "notes"),
            "a pre-v6 database must converge on the same schema as a fresh one"
        );
    }

    fn table_exists(conn: &Connection, name: &str) -> bool {
        let n: i64 = conn
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE type='table' AND name=?1",
                [name],
                |r| r.get(0),
            )
            .unwrap();
        n == 1
    }

    fn column_exists(conn: &Connection, table: &str, column: &str) -> bool {
        conn.prepare(&format!("PRAGMA table_info({table})"))
            .unwrap()
            .query_map([], |r| r.get::<_, String>(1))
            .unwrap()
            .filter_map(std::result::Result::ok)
            .any(|c| c == column)
    }

    #[test]
    fn refuses_database_from_the_future() {
        let conn = mem();
        conn.pragma_update(None, "user_version", CURRENT_VERSION + 1)
            .unwrap();
        let err = validate_or_init(&conn).unwrap_err();
        assert!(matches!(err, CoreError::Schema(_)));
    }

    #[test]
    fn validate_is_idempotent_on_current_database() {
        let conn = mem();
        validate_or_init(&conn).unwrap();
        validate_or_init(&conn).unwrap();
    }

    /// A v1 database (the template's oldest shape) must reach head, and the
    /// pressure tables must all be there when it arrives.
    #[test]
    fn a_v1_database_migrates_all_the_way_to_the_pressure_schema() {
        let conn = mem();
        conn.execute_batch(
            "CREATE TABLE notes (
                id TEXT PRIMARY KEY, title TEXT NOT NULL,
                body TEXT, created_at TEXT NOT NULL
            );
            PRAGMA user_version = 1;",
        )
        .unwrap();

        validate_or_init(&conn).unwrap();

        let v: i64 = conn
            .pragma_query_value(None, "user_version", |r| r.get(0))
            .unwrap();
        assert_eq!(v, CURRENT_VERSION);

        // All three retention tiers exist (ADR-0006), not just the one v3 needs.
        for table in [
            "samples",
            "volume_samples",
            "sample_rollups",
            "volume_rollups",
            "alert_events",
            "alert_episodes",
            "censuses",
            "census_agent_sessions",
            "census_orphan_programs",
            "census_tmux_sessions",
            "census_app_groups",
            "census_monitor_agents",
            "census_cpu_consumers",
        ] {
            let n: i64 = conn
                .query_row(
                    "SELECT count(*) FROM sqlite_master WHERE type='table' AND name=?1",
                    [table],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(n, 1, "table {table} must exist at head");
        }
    }

    /// Enforcement must come from OUR pragma, not from the SQLite build.
    ///
    /// This test was a FALSE PIN on its first writing: it opened a fresh
    /// connection, and rusqlite's `bundled` SQLite is compiled with
    /// `SQLITE_DEFAULT_FOREIGN_KEYS`, so `foreign_keys` was already 1 and the
    /// cascade fired with our pragma deleted. Confirmed by reading
    /// `PRAGMA compile_options`. So the test starts by turning enforcement OFF
    /// and proves `validate_or_init` turns it back on — which is the actual
    /// mechanism, and which a mutation can actually break.
    ///
    /// The pragma is not redundant despite that default: swapping `bundled` for
    /// a system SQLite silently returns to FK-off, and then the retention sweep
    /// would leak orphaned child rows forever while looking completely healthy.
    #[test]
    fn validate_or_init_turns_foreign_key_enforcement_back_on() {
        let conn = mem();
        conn.execute_batch("PRAGMA foreign_keys = OFF;").unwrap();
        let before: i64 = conn
            .pragma_query_value(None, "foreign_keys", |r| r.get(0))
            .unwrap();
        assert_eq!(before, 0, "precondition: enforcement is off");

        validate_or_init(&conn).unwrap();

        let after: i64 = conn
            .pragma_query_value(None, "foreign_keys", |r| r.get(0))
            .unwrap();
        assert_eq!(after, 1, "validate_or_init must re-enable enforcement");
    }

    /// The cascade itself, on a connection whose enforcement started OFF.
    /// Mutation-proof: remove `PRAGMA foreign_keys = ON` from
    /// `apply_connection_pragmas` and this fails with an orphaned volume row.
    #[test]
    fn deleting_a_sample_cascades_to_its_volume_rows() {
        let conn = mem();
        // Start from the hostile default, so this proves our pragma rather than
        // the bundled build's compile-time default.
        conn.execute_batch("PRAGMA foreign_keys = OFF;").unwrap();
        validate_or_init(&conn).unwrap();
        conn.execute_batch(
            "INSERT INTO samples (id, sampled_at, boot_time_secs, ncpu,
                mem_total_bytes, page_size, load_1m, load_5m, load_15m,
                swap_total_bytes, swap_used_bytes, pages_free, pages_active,
                pages_inactive, pages_wired, pages_speculative,
                pages_compressor, swapins, swapouts)
             VALUES ('s1', '2026-08-31T15:00:00.000000Z', 1, 8, 1, 16384,
                     1.0, 1.0, 1.0, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1);
             INSERT INTO volume_samples (sample_id, mount_point, total_bytes, avail_bytes)
             VALUES ('s1', '/System/Volumes/Data', 100, 50);",
        )
        .unwrap();

        conn.execute("DELETE FROM samples WHERE id = 's1'", [])
            .unwrap();

        let orphans: i64 = conn
            .query_row("SELECT count(*) FROM volume_samples", [], |r| r.get(0))
            .unwrap();
        assert_eq!(
            orphans, 0,
            "volume rows must cascade; {orphans} orphan(s) means the retention \
             sweep leaks rows forever"
        );
    }

    /// The runtime guard in `apply_connection_pragmas` is the thing that would
    /// catch a future switch away from `bundled` SQLite. Prove it actually
    /// fails closed rather than warning into a log nobody reads.
    #[test]
    fn a_connection_that_refuses_foreign_keys_is_rejected() {
        // SQLite refuses `PRAGMA foreign_keys` inside a transaction: it is a
        // silent no-op there. That gives us a real way to simulate the pragma
        // failing to take, without mocking anything.
        let conn = mem();
        validate_or_init(&conn).unwrap();
        conn.execute_batch("PRAGMA foreign_keys = OFF;").unwrap();
        conn.execute_batch("BEGIN;").unwrap();

        let err = validate_or_init(&conn);
        conn.execute_batch("ROLLBACK;").ok();

        let err = err.expect_err("a connection that will not enforce FKs must be refused");
        assert!(
            err.to_string().contains("foreign_keys"),
            "expected an FK-enforcement error, got: {err}"
        );
    }

    /// The retention sweep deletes by age and every read is time-windowed, so a
    /// missing index turns both into full scans of a table that grows every 15
    /// seconds. Mutation-proof: drop the CREATE INDEX and this fails.
    #[test]
    fn samples_are_indexed_by_time() {
        let conn = mem();
        validate_or_init(&conn).unwrap();
        let n: i64 = conn
            .query_row(
                "SELECT count(*) FROM sqlite_master
                 WHERE type='index' AND name='samples_sampled_at'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 1, "samples(sampled_at) must be indexed");
    }

    /// A migration step and its version bump are one transaction: if the DDL
    /// fails, the `user_version` must NOT advance. Mutation-proof — move the
    /// `pragma_update` out of the transaction (or ahead of the DDL) and this
    /// fails, because a failed migration would then leave the version ahead
    /// of the schema: the exact wedge H1 describes.
    #[test]
    fn failed_migration_does_not_advance_user_version() {
        let conn = mem();
        // Build a v1 database whose `closed_at` column ALREADY exists, so the
        // v1→v2 `ALTER TABLE … ADD COLUMN closed_at` will fail as a duplicate.
        conn.execute_batch(
            "CREATE TABLE notes (
                id TEXT PRIMARY KEY, title TEXT NOT NULL,
                body TEXT, created_at TEXT NOT NULL, closed_at TEXT
            );
            PRAGMA user_version = 1;",
        )
        .unwrap();

        let err = apply_migration(&conn, 1);
        assert!(err.is_err(), "duplicate-column ALTER must fail");

        let v: i64 = conn
            .pragma_query_value(None, "user_version", |r| r.get(0))
            .unwrap();
        assert_eq!(v, 1, "version must stay at 1 when the migration rolls back");
    }

    /// An interrupted migration (transaction rolled back, as a crash-in-flight
    /// leaves it under the atomic design) must leave the DB cleanly at the
    /// pre-migration version, and a re-run must complete it. Proves the
    /// "re-running after a partial migration is safe" guarantee.
    ///
    /// Interrupted at the NEWEST step (v9→v10) with real rollup data in the
    /// file, because that is the shape a crash would actually take on this
    /// machine: an old database opened by a new build, with a month of rollups
    /// in it.
    #[test]
    fn interrupted_migration_is_recoverable() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("banshee.db");

        {
            let conn = Connection::open(&path).unwrap();
            // Walk the chain to one step short of head, so the file has the real
            // previous shape rather than a hand-written approximation.
            apply_connection_pragmas(&conn).unwrap();
            for from in 0..(CURRENT_VERSION - 1) {
                apply_migration(&conn, from).unwrap();
            }
            conn.execute_batch(
                "INSERT INTO sample_rollups (bucket_start, sample_count,
                    load_1m_avg, load_1m_max, swap_used_avg, swap_used_max,
                    pages_free_min, pages_compressor_max, swapins_delta,
                    swapouts_delta)
                 VALUES ('2026-08-31T15:00:00.000000Z', 20,
                         1.0, 2.0, 1, 1, 1, 1, 1, 1);",
            )
            .unwrap();

            // Simulate a migration interrupted mid-flight: apply the DDL and
            // the version bump inside a transaction, then roll it back (== a
            // crash before COMMIT under the atomic design). The DDL here must
            // BE the newest arm's (v15, the census `pressure_kills` column), or
            // this test quietly stops covering the step it claims to. Copy it
            // verbatim from the `14 =>` arm after any `cargo fmt`.
            let tx = conn.unchecked_transaction().unwrap();
            tx.execute_batch("ALTER TABLE censuses ADD COLUMN pressure_kills INTEGER;")
                .unwrap();
            tx.pragma_update(None, "user_version", CURRENT_VERSION)
                .unwrap();
            tx.rollback().unwrap();

            // The interrupted migration left NO partial state.
            let v: i64 = conn
                .pragma_query_value(None, "user_version", |r| r.get(0))
                .unwrap();
            assert_eq!(
                v,
                CURRENT_VERSION - 1,
                "rolled-back migration must leave the version where it was"
            );
            assert!(
                !column_exists(&conn, "censuses", "pressure_kills"),
                "the rolled-back ALTER TABLE must not have taken effect"
            );
        }

        // Re-open: validate_or_init must finish the migration cleanly.
        let conn = Connection::open(&path).unwrap();
        validate_or_init(&conn).unwrap();
        let v: i64 = conn
            .pragma_query_value(None, "user_version", |r| r.get(0))
            .unwrap();
        assert_eq!(v, CURRENT_VERSION);
        assert!(
            table_exists(&conn, "census_cpu_consumers"),
            "the retry completes the CREATE TABLE"
        );
        // A fresh v10 table starts empty — episodes are not backfilled from the
        // pre-v10 alert_events rows (ADR-0009 consequences).
        let episodes: i64 = conn
            .query_row("SELECT count(*) FROM alert_episodes", [], |r| r.get(0))
            .unwrap();
        assert_eq!(episodes, 0, "v10 introduces the table empty, no backfill");
        // The pre-v10 rollup row is untouched by v10 (it adds a NEW table, not a
        // column), so real data crosses the migration intact.
        let survivors: i64 = conn
            .query_row(
                "SELECT count(*) FROM sample_rollups
                 WHERE bucket_start = '2026-08-31T15:00:00.000000Z'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(survivors, 1, "real data survives the recovered migration");
    }
}
