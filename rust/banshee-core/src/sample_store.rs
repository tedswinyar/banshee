// sample_store — persistence for the pressure time series, and the retention
// sweep that keeps it bounded.
//
// This app's writer is a TIMER, not a person: ~5,760 samples a day. Retention is
// not a later optimisation, it is a correctness property (ADR-0006). A monitor
// that quietly ate disk would be the exact bug it exists to catch.

use std::path::Path;

use chrono::{DateTime, Duration, Utc};
use rusqlite::{Connection, OptionalExtension, Row, params};
use uuid::Uuid;

use crate::census::{
    AgentSession, AppGroup, Census, HelperRollup, MonitorAgent, MonitorTotal, OrphanCensus,
    ProgramCount, TmuxSessionInfo,
};
use crate::pressure::episode::AlertEpisode;
use crate::sample::{
    CensusScalars, Rollup, Sample, VolumeRollup, VolumeSample, bucket_start_of, rollup,
};
use crate::{CoreError, Result, schema, wire_time};

/// How long each tier lives (ADR-0006). Configurable at the call site; these are
/// the defaults the daemon runs with.
///
/// **`raw` has an effective floor of one rollup bucket.** The sweep's cutoff is
/// `bucket_start_of(now - raw)`, so any value below `ROLLUP_BUCKET_SECS` behaves
/// the same as one bucket: samples in the current, still-open bucket are never
/// rolled up or deleted. That is deliberate — summarising a bucket that can
/// still gain samples records a partial average as final — but it does mean a
/// sub-bucket `raw` value is not an error and not honoured either.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Retention {
    pub raw: Duration,
    pub rollups: Duration,
    pub alerts: Duration,
}

impl Default for Retention {
    fn default() -> Self {
        Self {
            raw: Duration::hours(24),
            rollups: Duration::days(30),
            alerts: Duration::days(365),
        }
    }
}

/// What a retention sweep did. Returned rather than logged so a caller — and a
/// test — can assert on it; a `tracing::info!` is visible only in the log
/// (if an operator must see it, it has to travel over the API).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SweepReport {
    /// Aged buckets summarised before their raw samples were deleted.
    pub rollups_written: usize,
    /// CLOSED buckets inside the raw window summarised so `/rollups` is dense
    /// up to the last five-minute boundary, not only older than a day. The
    /// raw samples behind them stay until they age out.
    pub recent_rollups_written: usize,
    pub raw_samples_deleted: usize,
    pub rollups_deleted: usize,
    /// Closed alert episodes past their retention, plus any pre-v10 point events
    /// still in the legacy `alert_events` table.
    pub alerts_deleted: usize,
    pub censuses_deleted: usize,
}

/// Clamp a `u64` into SQLite's signed `INTEGER`.
///
/// SQLite has no unsigned integer type, and `rusqlite` refuses to bind a `u64`
/// above `i64::MAX` with `TryFromIntError(PosOverflow)` — which fails the whole
/// INSERT. For a daemon that is the wrong trade: one absurd reading would stop
/// the time series rather than blemish it.
///
/// Every column this is applied to is a byte or page count from a 64-bit kernel.
/// The largest physically meaningful value is many orders of magnitude below
/// `i64::MAX` (9.2 exabytes), so saturation is unreachable in practice and only
/// exists so a corrupt or synthetic reading degrades one field instead of
/// discarding the sample. Values that hit the clamp are, by construction,
/// already nonsense.
fn clamp_i64(v: u64) -> i64 {
    i64::try_from(v).unwrap_or(i64::MAX)
}

pub struct SampleStore {
    conn: Connection,
}

impl SampleStore {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(path)?;
        schema::validate_or_init(&conn)?;
        Ok(Self { conn })
    }

    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        schema::validate_or_init(&conn)?;
        Ok(Self { conn })
    }

    /// The underlying connection, for `backup::backup_verified` only.
    ///
    /// `pub(crate)` on purpose: ADR-0001's invariant is that this store is the
    /// only thing that opens the database, and a public handle would let a caller
    /// run arbitrary SQL around the store's validation. The backup path needs a
    /// raw connection because `rusqlite::backup::Backup` takes one.
    pub(crate) fn connection(&self) -> &Connection {
        &self.conn
    }

    /// Insert one sample and its volume rows as a single transaction.
    ///
    /// Atomic because a sample whose volume rows are missing is not a partial
    /// sample, it is a LIE: the disk dimension would read as "no volumes
    /// observed" rather than "not recorded".
    pub fn insert(&self, s: &Sample) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "INSERT INTO samples (id, sampled_at, boot_time_secs, ncpu,
                mem_total_bytes, page_size, load_1m, load_5m, load_15m,
                swap_total_bytes, swap_used_bytes, pages_free, pages_active,
                pages_inactive, pages_wired, pages_speculative,
                pages_compressor, swapins, swapouts, pages_purgeable,
                pages_external, memory_pressure_level, thermal_pressure_level,
                cpu_speed_limit_percent, cpu_ticks_total, cpu_ticks_idle,
                kernel_free_percent, jetsam_kills)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13,
                     ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24,
                     ?25, ?26, ?27, ?28)",
            params![
                s.id.to_string(),
                wire_time::to_wire(&s.sampled_at),
                s.boot_time_secs,
                s.ncpu,
                clamp_i64(s.mem_total_bytes),
                clamp_i64(s.page_size),
                s.load1m,
                s.load5m,
                s.load15m,
                clamp_i64(s.swap_total_bytes),
                clamp_i64(s.swap_used_bytes),
                clamp_i64(s.pages_free),
                clamp_i64(s.pages_active),
                clamp_i64(s.pages_inactive),
                clamp_i64(s.pages_wired),
                clamp_i64(s.pages_speculative),
                clamp_i64(s.pages_compressor),
                clamp_i64(s.swapins),
                clamp_i64(s.swapouts),
                clamp_i64(s.pages_purgeable),
                clamp_i64(s.pages_external),
                s.memory_pressure_level,
                s.thermal_pressure_level,
                s.cpu_speed_limit_percent,
                // Clamp like every other u64 column: a tick count above
                // i64::MAX would fail the WHOLE insert (rusqlite refuses it),
                // stopping the time series over one absurd reading.
                s.cpu_ticks_total.map(clamp_i64),
                s.cpu_ticks_idle.map(clamp_i64),
                s.kernel_free_percent,
                s.jetsam_kills.map(clamp_i64),
            ],
        )?;
        {
            let mut stmt = tx.prepare(
                "INSERT INTO volume_samples (sample_id, mount_point, total_bytes, avail_bytes)
                 VALUES (?1, ?2, ?3, ?4)",
            )?;
            for v in &s.volumes {
                stmt.execute(params![
                    s.id.to_string(),
                    v.mount_point,
                    clamp_i64(v.total_bytes),
                    clamp_i64(v.avail_bytes)
                ])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Samples in `[from, to)`, oldest first.
    ///
    /// Half-open on purpose: adjacent windows tile without double-counting the
    /// boundary sample, which matters for rollups.
    pub fn samples_between(&self, from: DateTime<Utc>, to: DateTime<Utc>) -> Result<Vec<Sample>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, sampled_at, boot_time_secs, ncpu, mem_total_bytes,
                    page_size, load_1m, load_5m, load_15m, swap_total_bytes,
                    swap_used_bytes, pages_free, pages_active, pages_inactive,
                    pages_wired, pages_speculative, pages_compressor,
                    swapins, swapouts, pages_purgeable, pages_external,
                    memory_pressure_level, thermal_pressure_level,
                    cpu_speed_limit_percent, cpu_ticks_total, cpu_ticks_idle,
                    kernel_free_percent, jetsam_kills
             FROM samples
             WHERE sampled_at >= ?1 AND sampled_at < ?2
             ORDER BY sampled_at ASC",
        )?;
        let mut samples = stmt
            .query_map(
                params![wire_time::to_wire(&from), wire_time::to_wire(&to)],
                row_to_sample,
            )?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        self.attach_volumes(&mut samples)?;
        Ok(samples)
    }

    /// The most recent `limit` samples, oldest first (so callers can diff
    /// consecutive pairs without reversing).
    pub fn recent_samples(&self, limit: usize) -> Result<Vec<Sample>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, sampled_at, boot_time_secs, ncpu, mem_total_bytes,
                    page_size, load_1m, load_5m, load_15m, swap_total_bytes,
                    swap_used_bytes, pages_free, pages_active, pages_inactive,
                    pages_wired, pages_speculative, pages_compressor,
                    swapins, swapouts, pages_purgeable, pages_external,
                    memory_pressure_level, thermal_pressure_level,
                    cpu_speed_limit_percent, cpu_ticks_total, cpu_ticks_idle,
                    kernel_free_percent, jetsam_kills
             FROM samples ORDER BY sampled_at DESC LIMIT ?1",
        )?;
        let mut samples = stmt
            .query_map(params![limit as i64], row_to_sample)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        samples.reverse();
        self.attach_volumes(&mut samples)?;
        Ok(samples)
    }

    pub fn latest_sample(&self) -> Result<Option<Sample>> {
        Ok(self.recent_samples(1)?.pop())
    }

    /// Volume rows for a batch of samples, in ONE query.
    ///
    /// Deliberately not a per-sample query: the caller frequently asks for a
    /// whole window, and N+1 queries over a 24-hour window is 5,760 round trips
    /// through the store mutex.
    fn attach_volumes(&self, samples: &mut [Sample]) -> Result<()> {
        if samples.is_empty() {
            return Ok(());
        }
        // Index by id ONCE. The obvious `samples.iter_mut().find(...)` per volume
        // row is O(N²), and with a `to_string()` inside the comparison it also
        // allocates on every probe: measured at 626 ms for a 24-hour window
        // (5,760 samples) before this index, ~19 ms after.
        let mut index: std::collections::HashMap<Uuid, usize> =
            std::collections::HashMap::with_capacity(samples.len());
        for (i, s) in samples.iter().enumerate() {
            index.insert(s.id, i);
        }

        // Chunk the IN-list. SQLite's `SQLITE_MAX_VARIABLE_NUMBER` is 32,766 on
        // current builds but **999 on older ones**, and a 24-hour window is
        // 5,760 ids — comfortably over that older limit. Chunking keeps this
        // correct regardless of which SQLite the crate is linked against.
        const CHUNK: usize = 500;
        let ids: Vec<String> = samples.iter().map(|s| s.id.to_string()).collect();

        for chunk in ids.chunks(CHUNK) {
            let placeholders = vec!["?"; chunk.len()].join(",");
            let sql = format!(
                "SELECT sample_id, mount_point, total_bytes, avail_bytes
                 FROM volume_samples WHERE sample_id IN ({placeholders})
                 ORDER BY mount_point ASC"
            );
            let mut stmt = self.conn.prepare(&sql)?;
            let rows = stmt
                .query_map(rusqlite::params_from_iter(chunk.iter()), |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        VolumeSample {
                            mount_point: r.get(1)?,
                            total_bytes: r.get(2)?,
                            avail_bytes: r.get(3)?,
                        },
                    ))
                })?
                .collect::<std::result::Result<Vec<_>, _>>()?;

            for (sample_id, vol) in rows {
                let Ok(id) = Uuid::parse_str(&sample_id) else {
                    // A row whose id is not a UUID cannot belong to any sample
                    // we asked about; skipping is strictly better than failing
                    // the whole read.
                    continue;
                };
                if let Some(&i) = index.get(&id) {
                    samples[i].volumes.push(vol);
                }
            }
        }
        Ok(())
    }

    pub fn count_samples(&self) -> Result<usize> {
        let n: i64 = self
            .conn
            .query_row("SELECT count(*) FROM samples", [], |r| r.get(0))?;
        Ok(n as usize)
    }

    pub fn count_rollups(&self) -> Result<usize> {
        let n: i64 = self
            .conn
            .query_row("SELECT count(*) FROM sample_rollups", [], |r| r.get(0))?;
        Ok(n as usize)
    }

    /// Bytes the database occupies on disk, so the app can report its own cost.
    /// A monitor that will not say what it costs has no standing to complain
    /// about anything else (ADR-0006).
    pub fn size_bytes(&self) -> Result<u64> {
        let page_count: i64 = self.conn.query_row("PRAGMA page_count", [], |r| r.get(0))?;
        let page_size: i64 = self.conn.query_row("PRAGMA page_size", [], |r| r.get(0))?;
        Ok((page_count.max(0) as u64).saturating_mul(page_size.max(0) as u64))
    }

    pub fn upsert_rollup(&self, r: &Rollup) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        let bucket = wire_time::to_wire(&r.bucket_start);
        // Recomputing a bucket must REPLACE it, not accumulate duplicates —
        // the sweep may legitimately revisit a bucket that gained samples.
        tx.execute(
            "INSERT INTO sample_rollups (bucket_start, sample_count, load_1m_avg,
                load_1m_max, swap_used_avg, swap_used_max, pages_free_min,
                pages_compressor_max, swapins_delta, swapouts_delta,
                stale_sessions_max, orphans_max, monitor_percent_max,
                total_procs_max)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)
             ON CONFLICT(bucket_start) DO UPDATE SET
                sample_count = excluded.sample_count,
                load_1m_avg = excluded.load_1m_avg,
                load_1m_max = excluded.load_1m_max,
                swap_used_avg = excluded.swap_used_avg,
                swap_used_max = excluded.swap_used_max,
                pages_free_min = excluded.pages_free_min,
                pages_compressor_max = excluded.pages_compressor_max,
                swapins_delta = excluded.swapins_delta,
                swapouts_delta = excluded.swapouts_delta,
                stale_sessions_max = excluded.stale_sessions_max,
                orphans_max = excluded.orphans_max,
                monitor_percent_max = excluded.monitor_percent_max,
                total_procs_max = excluded.total_procs_max",
            params![
                bucket,
                r.sample_count,
                r.load1m_avg,
                r.load1m_max,
                clamp_i64(r.swap_used_avg),
                clamp_i64(r.swap_used_max),
                clamp_i64(r.pages_free_min),
                clamp_i64(r.pages_compressor_max),
                r.swapins_delta.map(clamp_i64),
                r.swapouts_delta.map(clamp_i64),
                r.stale_sessions_max,
                r.orphans_max,
                r.monitor_percent_max,
                r.total_procs_max,
            ],
        )?;
        tx.execute(
            "DELETE FROM volume_rollups WHERE bucket_start = ?1",
            params![bucket],
        )?;
        {
            let mut stmt = tx.prepare(
                "INSERT INTO volume_rollups (bucket_start, mount_point, total_bytes,
                    avail_min, avail_avg) VALUES (?1, ?2, ?3, ?4, ?5)",
            )?;
            for v in &r.volumes {
                stmt.execute(params![
                    bucket,
                    v.mount_point,
                    clamp_i64(v.total_bytes),
                    clamp_i64(v.avail_min),
                    clamp_i64(v.avail_avg)
                ])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Rollups in `[from, to)`, oldest first, with their volume rows attached.
    ///
    /// **The per-bucket volume query is N+1 and that is fine here — measured, not
    /// assumed.** 8,640 buckets (the full 30-day retention window) reads in ~24 ms,
    /// because each lookup is an indexed primary-key hit. Contrast `attach_volumes`
    /// on the sample tier, which took 626 ms for a day: that was O(N²), a linear
    /// `find` over the whole vector per volume row *with a `String` allocation
    /// inside the comparison*. The shape that was slow was the scan, not the query
    /// count. Do not "fix" this into a chunked `IN` list without measuring first.
    pub fn rollups_between(&self, from: DateTime<Utc>, to: DateTime<Utc>) -> Result<Vec<Rollup>> {
        let mut stmt = self.conn.prepare(
            "SELECT bucket_start, sample_count, load_1m_avg, load_1m_max,
                    swap_used_avg, swap_used_max, pages_free_min,
                    pages_compressor_max, swapins_delta, swapouts_delta,
                    stale_sessions_max, orphans_max, monitor_percent_max,
                    total_procs_max
             FROM sample_rollups
             WHERE bucket_start >= ?1 AND bucket_start < ?2
             ORDER BY bucket_start ASC",
        )?;
        let mut rollups = stmt
            .query_map(
                params![wire_time::to_wire(&from), wire_time::to_wire(&to)],
                |r| {
                    Ok(Rollup {
                        bucket_start: parse_wire(r, 0)?,
                        sample_count: r.get(1)?,
                        load1m_avg: r.get(2)?,
                        load1m_max: r.get(3)?,
                        swap_used_avg: r.get(4)?,
                        swap_used_max: r.get(5)?,
                        pages_free_min: r.get(6)?,
                        pages_compressor_max: r.get(7)?,
                        swapins_delta: r.get(8)?,
                        swapouts_delta: r.get(9)?,
                        stale_sessions_max: r.get(10)?,
                        orphans_max: r.get(11)?,
                        monitor_percent_max: r.get(12)?,
                        total_procs_max: r.get(13)?,
                        volumes: Vec::new(),
                    })
                },
            )?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        for rl in &mut rollups {
            let mut vs = self.conn.prepare(
                "SELECT mount_point, total_bytes, avail_min, avail_avg
                 FROM volume_rollups WHERE bucket_start = ?1 ORDER BY mount_point ASC",
            )?;
            rl.volumes = vs
                .query_map(params![wire_time::to_wire(&rl.bucket_start)], |r| {
                    Ok(VolumeRollup {
                        mount_point: r.get(0)?,
                        total_bytes: r.get(1)?,
                        avail_min: r.get(2)?,
                        avail_avg: r.get(3)?,
                    })
                })?
                .collect::<std::result::Result<Vec<_>, _>>()?;
        }
        Ok(rollups)
    }

    // ---- census tier ---------------------------------------------------

    /// Insert one census and all of its child rows as a single transaction.
    ///
    /// Atomic for the same reason a sample is: a census whose session list
    /// failed to write is not a partial census, it is a claim that nothing was
    /// running.
    /// The most recent `limit` rollups, oldest first — the same shape as
    /// `recent_samples`, so `/rollups` and `/samples` can offer identical
    /// parameters and a caller has one thing to learn rather than two.
    pub fn recent_rollups(&self, limit: usize) -> Result<Vec<Rollup>> {
        // Newest-first to apply LIMIT, then reversed, exactly as `recent_samples`
        // does. Reading the OLDEST N would be a different (and wrong) answer.
        let buckets: Vec<DateTime<Utc>> = self
            .conn
            .prepare(
                "SELECT bucket_start FROM sample_rollups
                 ORDER BY bucket_start DESC LIMIT ?1",
            )?
            .query_map(params![limit as i64], |r| parse_wire(r, 0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        let Some(oldest) = buckets.last().copied() else {
            return Ok(Vec::new());
        };
        // Half-open upper bound, so the newest bucket is included.
        let newest = buckets[0] + Duration::seconds(1);
        self.rollups_between(oldest, newest)
    }

    pub fn insert_census(&self, c: &Census) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        let id = c.id.to_string();
        tx.execute(
            "INSERT INTO censuses (id, taken_at, total_procs, tmux_available,
                ide_helper_count, ide_helper_rss, orphan_total_count,
                orphan_total_rss, orphan_count, orphan_rss,
                monitor_proc_count, monitor_rss, monitor_cpu_secs,
                monitor_percent, monitor_longest_life)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
            params![
                id,
                wire_time::to_wire(&c.taken_at),
                c.total_procs,
                c.tmux_available as i64,
                c.ide_helpers.count,
                clamp_i64(c.ide_helpers.rss_bytes),
                c.orphans.total_count,
                clamp_i64(c.orphans.total_rss_bytes),
                c.orphans.orphan_count,
                clamp_i64(c.orphans.orphan_rss_bytes),
                c.monitor_total.proc_count,
                clamp_i64(c.monitor_total.rss_bytes),
                c.monitor_total.cpu_secs_total,
                c.monitor_total.percent_of_one_core,
                clamp_i64(c.monitor_total.longest_life_secs),
            ],
        )?;
        {
            let mut st = tx.prepare(
                "INSERT INTO census_agent_sessions (census_id, pid, program, rss_bytes,
                    age_secs, age_days, cpu_secs, tty, is_stale, cwd)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            )?;
            for s in &c.agent_sessions {
                st.execute(params![
                    id,
                    s.pid,
                    s.program,
                    clamp_i64(s.rss_bytes),
                    clamp_i64(s.age_secs),
                    clamp_i64(s.age_days),
                    s.cpu_secs,
                    s.tty,
                    s.is_stale as i64,
                    s.cwd,
                ])?;
            }
        }
        {
            let mut st = tx.prepare(
                "INSERT INTO census_orphan_programs (census_id, program, count, rss_bytes)
                 VALUES (?1, ?2, ?3, ?4)",
            )?;
            for o in &c.orphans.orphans_by_program {
                st.execute(params![id, o.program, o.count, clamp_i64(o.rss_bytes)])?;
            }
        }
        {
            let mut st = tx.prepare(
                "INSERT INTO census_tmux_sessions (census_id, name, pane_command,
                    idle_days, is_busy, is_stale) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            )?;
            for s in &c.tmux_sessions {
                st.execute(params![
                    id,
                    s.name,
                    s.pane_command,
                    s.idle_days,
                    s.is_busy as i64,
                    s.is_stale as i64,
                ])?;
            }
        }
        {
            let mut st = tx.prepare(
                "INSERT INTO census_app_groups (census_id, name, proc_count, rss_bytes)
                 VALUES (?1, ?2, ?3, ?4)",
            )?;
            for g in &c.app_groups {
                st.execute(params![id, g.name, g.proc_count, clamp_i64(g.rss_bytes)])?;
            }
        }
        {
            let mut st = tx.prepare(
                "INSERT INTO census_monitor_agents (census_id, name, proc_count,
                    rss_bytes, cpu_secs_total, percent_of_one_core, longest_life_secs)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            )?;
            for a in &c.monitor_agents {
                st.execute(params![
                    id,
                    a.name,
                    a.proc_count,
                    clamp_i64(a.rss_bytes),
                    a.cpu_secs_total,
                    a.percent_of_one_core,
                    clamp_i64(a.longest_life_secs),
                ])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// The most recent `limit` censuses, OLDEST first, fully hydrated.
    ///
    /// Oldest-first so the pressure model can replay them through the same
    /// hysteresis machine as the samples: census-derived dimensions (stale
    /// sessions, orphans, managed-agent cost) deserve the same protection from
    /// flapping as the syscall-derived ones.
    pub fn recent_censuses(&self, limit: usize) -> Result<Vec<Census>> {
        let ids: Vec<String> = self
            .conn
            .prepare("SELECT id FROM censuses ORDER BY taken_at DESC LIMIT ?1")?
            .query_map(params![limit as i64], |r| r.get(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        let mut out = Vec::with_capacity(ids.len());
        for id in ids.into_iter().rev() {
            if let Some(c) = self.census_by_id(&id)? {
                out.push(c);
            }
        }
        Ok(out)
    }

    /// The scalar counters of every census strictly before `cutoff` — the
    /// projection the sweep folds into rollups before deleting the censuses
    /// themselves (`banshee-yj1`).
    ///
    /// Deliberately NOT full hydration: the stale count is the only child-table
    /// fact needed, and a correlated count is one indexed subquery per row where
    /// `census_by_id` is six.
    fn census_scalars_before(&self, cutoff: DateTime<Utc>) -> Result<Vec<CensusScalars>> {
        self.census_scalars_between(DateTime::<Utc>::MIN_UTC, cutoff)
    }

    /// The scalar counters of every census in `[from, to)`.
    fn census_scalars_between(
        &self,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
    ) -> Result<Vec<CensusScalars>> {
        let mut stmt = self.conn.prepare(
            "SELECT taken_at, total_procs, orphan_count, monitor_percent,
                    (SELECT count(*) FROM census_agent_sessions s
                     WHERE s.census_id = censuses.id AND s.is_stale != 0)
             FROM censuses WHERE taken_at >= ?1 AND taken_at < ?2",
        )?;
        let out = stmt
            .query_map(
                params![wire_time::to_wire(&from), wire_time::to_wire(&to)],
                |r| {
                    Ok(CensusScalars {
                        taken_at: parse_wire(r, 0)?,
                        total_procs: r.get(1)?,
                        orphans: r.get(2)?,
                        monitor_percent: r.get(3)?,
                        stale_sessions: r.get(4)?,
                    })
                },
            )?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(out)
    }

    /// The newest bucket that has a rollup row, if any.
    fn newest_rollup_bucket(&self) -> Result<Option<DateTime<Utc>>> {
        let s: Option<String> =
            self.conn
                .query_row("SELECT max(bucket_start) FROM sample_rollups", [], |r| {
                    r.get(0)
                })?;
        s.map(|s| {
            wire_time::from_wire(&s).map_err(|e| {
                CoreError::InvalidInput(format!(
                    "stored bucket_start {s:?} is not a wire datetime: {e}"
                ))
            })
        })
        .transpose()
    }

    pub fn count_censuses(&self) -> Result<usize> {
        let n: i64 = self
            .conn
            .query_row("SELECT count(*) FROM censuses", [], |r| r.get(0))?;
        Ok(n as usize)
    }

    /// The most recent census, fully hydrated.
    pub fn latest_census(&self) -> Result<Option<Census>> {
        let id: Option<String> = self
            .conn
            .query_row(
                "SELECT id FROM censuses ORDER BY taken_at DESC LIMIT 1",
                [],
                |r| r.get(0),
            )
            .optional()?;
        match id {
            Some(id) => self.census_by_id(&id),
            None => Ok(None),
        }
    }

    /// One census by id, fully hydrated.
    fn census_by_id(&self, want: &str) -> Result<Option<Census>> {
        let row = self
            .conn
            .query_row(
                "SELECT id, taken_at, total_procs, tmux_available, ide_helper_count,
                        ide_helper_rss, orphan_total_count, orphan_total_rss,
                        orphan_count, orphan_rss, monitor_proc_count, monitor_rss,
                        monitor_cpu_secs, monitor_percent, monitor_longest_life
                 FROM censuses WHERE id = ?1",
                params![want],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        parse_wire(r, 1)?,
                        r.get::<_, u32>(2)?,
                        r.get::<_, i64>(3)? != 0,
                        r.get::<_, u32>(4)?,
                        r.get::<_, u64>(5)?,
                        r.get::<_, u32>(6)?,
                        r.get::<_, u64>(7)?,
                        r.get::<_, u32>(8)?,
                        r.get::<_, u64>(9)?,
                        MonitorTotal {
                            proc_count: r.get(10)?,
                            rss_bytes: r.get(11)?,
                            cpu_secs_total: r.get(12)?,
                            percent_of_one_core: r.get(13)?,
                            longest_life_secs: r.get(14)?,
                        },
                    ))
                },
            )
            .optional()?;

        let Some((cid, taken_at, total_procs, tmux_available, hc, hr, otc, otr, oc, or_, mtot)) =
            row
        else {
            return Ok(None);
        };
        let uuid = Uuid::parse_str(&cid)
            .map_err(|e| CoreError::Schema(format!("census id {cid:?} is not a uuid: {e}")))?;

        let agent_sessions = self
            .conn
            .prepare(
                "SELECT pid, program, rss_bytes, age_secs, age_days, cpu_secs, tty,
                        is_stale, cwd
                 FROM census_agent_sessions WHERE census_id = ?1
                 ORDER BY age_secs DESC",
            )?
            .query_map(params![cid], |r| {
                Ok(AgentSession {
                    pid: r.get(0)?,
                    program: r.get(1)?,
                    rss_bytes: r.get(2)?,
                    age_secs: r.get(3)?,
                    age_days: r.get(4)?,
                    cpu_secs: r.get(5)?,
                    tty: r.get(6)?,
                    is_stale: r.get::<_, i64>(7)? != 0,
                    cwd: r.get(8)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        let orphans_by_program = self
            .conn
            .prepare(
                "SELECT program, count, rss_bytes FROM census_orphan_programs
                 WHERE census_id = ?1 ORDER BY count DESC, program ASC",
            )?
            .query_map(params![cid], |r| {
                Ok(ProgramCount {
                    program: r.get(0)?,
                    count: r.get(1)?,
                    rss_bytes: r.get(2)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        let tmux_sessions = self
            .conn
            .prepare(
                "SELECT name, pane_command, idle_days, is_busy, is_stale
                 FROM census_tmux_sessions WHERE census_id = ?1 ORDER BY name ASC",
            )?
            .query_map(params![cid], |r| {
                Ok(TmuxSessionInfo {
                    name: r.get(0)?,
                    pane_command: r.get(1)?,
                    idle_days: r.get(2)?,
                    is_busy: r.get::<_, i64>(3)? != 0,
                    is_stale: r.get::<_, i64>(4)? != 0,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        let app_groups = self
            .conn
            .prepare(
                "SELECT name, proc_count, rss_bytes FROM census_app_groups
                 WHERE census_id = ?1 ORDER BY rss_bytes DESC, name ASC",
            )?
            .query_map(params![cid], |r| {
                Ok(AppGroup {
                    name: r.get(0)?,
                    proc_count: r.get(1)?,
                    rss_bytes: r.get(2)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        let monitor_agents = self
            .conn
            .prepare(
                "SELECT name, proc_count, rss_bytes, cpu_secs_total, percent_of_one_core,
                        longest_life_secs
                 FROM census_monitor_agents WHERE census_id = ?1
                 ORDER BY percent_of_one_core DESC, name ASC",
            )?
            .query_map(params![cid], |r| {
                Ok(MonitorAgent {
                    name: r.get(0)?,
                    proc_count: r.get(1)?,
                    rss_bytes: r.get(2)?,
                    cpu_secs_total: r.get(3)?,
                    percent_of_one_core: r.get(4)?,
                    longest_life_secs: r.get(5)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        Ok(Some(Census {
            id: uuid,
            taken_at,
            total_procs,
            agent_sessions,
            ide_helpers: HelperRollup {
                count: hc,
                rss_bytes: hr,
            },
            orphans: OrphanCensus {
                total_count: otc,
                total_rss_bytes: otr,
                orphan_count: oc,
                orphan_rss_bytes: or_,
                orphans_by_program,
            },
            tmux_sessions,
            app_groups,
            monitor_agents,
            monitor_total: mtot,
            tmux_available,
        }))
    }

    // ---- alert episodes ------------------------------------------------

    /// Write an episode — new, or the stored one advanced (ADR-0009). Kept for a
    /// year (ADR-0006), so the interesting moments survive long after the
    /// samples that produced them are gone.
    pub fn upsert_episode(&self, e: &AlertEpisode) -> Result<()> {
        // `detail` carries the whole episode as JSON. The schema deliberately left
        // it opaque so the shape can grow without a migration; the typed columns
        // are the ones the open-episode query, the newest-first read and the
        // retention sweep need.
        let detail = serde_json::to_string(e)
            .map_err(|err| CoreError::InvalidInput(format!("cannot encode episode: {err}")))?;
        self.conn.execute(
            "INSERT INTO alert_episodes (id, dimension, started_at, ended_at, detail)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(id) DO UPDATE SET
                 dimension  = excluded.dimension,
                 started_at = excluded.started_at,
                 ended_at   = excluded.ended_at,
                 detail     = excluded.detail",
            params![
                e.id.to_string(),
                e.dimension.key(),
                wire_time::to_wire(&e.started_at),
                e.ended_at.map(|t| wire_time::to_wire(&t)),
                detail,
            ],
        )?;
        Ok(())
    }

    /// Every episode still open (`ended_at IS NULL`), oldest first.
    ///
    /// This is the state the lifecycle continues from after a restart: the
    /// reconcile step is a pure function of the verdict and THESE rows, so a
    /// daemon that restarts mid-crisis picks the crisis up rather than
    /// re-announcing it (ADR-0009 decision 9).
    pub fn open_episodes(&self) -> Result<Vec<AlertEpisode>> {
        self.episodes_where(
            "ended_at IS NULL AND started_at >= ?1 ORDER BY started_at ASC",
            DateTime::<Utc>::UNIX_EPOCH,
        )
    }

    /// Episodes active since `since` — everything that STARTED in the window plus
    /// every episode still open, however old — newest first.
    ///
    /// Open episodes are always included because they are the most relevant thing
    /// on the list: a three-day-old episode still firing is exactly what
    /// `/alerts?since=24h` is asking about.
    pub fn episodes_since(&self, since: DateTime<Utc>) -> Result<Vec<AlertEpisode>> {
        self.episodes_where(
            "(started_at >= ?1 OR ended_at IS NULL) ORDER BY started_at DESC",
            since,
        )
    }

    fn episodes_where(&self, clause: &str, since: DateTime<Utc>) -> Result<Vec<AlertEpisode>> {
        let rows: Vec<String> = self
            .conn
            .prepare(&format!("SELECT detail FROM alert_episodes WHERE {clause}"))?
            .query_map(params![wire_time::to_wire(&since)], |r| r.get(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        // A row whose JSON no longer parses is skipped rather than failing the
        // read: losing one historical episode is far better than being unable to
        // continue the lifecycle at all. The corollary lives on the struct —
        // every field added after v10 is `#[serde(default)]`, or this filter
        // silently erases the whole history the moment the daemon upgrades.
        Ok(rows
            .iter()
            .filter_map(|d| serde_json::from_str::<AlertEpisode>(d).ok())
            .collect())
    }

    pub fn count_episodes(&self) -> Result<usize> {
        let n: i64 = self
            .conn
            .query_row("SELECT count(*) FROM alert_episodes", [], |r| r.get(0))?;
        Ok(n as usize)
    }

    /// Roll up everything older than the raw window, then delete what has aged
    /// out of each tier.
    ///
    /// Order is load-bearing: **roll up BEFORE deleting.** Reversed, the sweep
    /// destroys raw samples that were never summarised and the 30-day history
    /// has permanent holes — silently, since nothing downstream knows a bucket
    /// should have existed.
    pub fn sweep(&self, now: DateTime<Utc>, keep: Retention) -> Result<SweepReport> {
        let mut report = SweepReport::default();

        // Only buckets strictly older than the raw cutoff are rolled up. A
        // bucket that is still inside the raw window may still gain samples,
        // and summarising it early would record a partial average as final.
        let raw_cutoff = bucket_start_of(now - keep.raw)?;

        let unrolled = self.samples_between(DateTime::<Utc>::MIN_UTC, raw_cutoff)?;
        if !unrolled.is_empty() {
            // The census scalars for every census about to age out, keyed by
            // bucket — extracted BEFORE the delete below, because after it the
            // month-scale sprawl trend (`banshee-yj1`) exists nowhere else.
            let mut census_by_bucket: std::collections::BTreeMap<
                DateTime<Utc>,
                Vec<CensusScalars>,
            > = std::collections::BTreeMap::new();
            for cs in self.census_scalars_before(raw_cutoff)? {
                census_by_bucket
                    .entry(bucket_start_of(cs.taken_at)?)
                    .or_default()
                    .push(cs);
            }

            let mut by_bucket: std::collections::BTreeMap<DateTime<Utc>, Vec<Sample>> =
                std::collections::BTreeMap::new();
            for s in unrolled {
                by_bucket.entry(s.bucket_start()?).or_default().push(s);
            }
            for (bucket, samples) in by_bucket {
                let censuses = census_by_bucket.remove(&bucket).unwrap_or_default();
                if let Some(r) = rollup(bucket, &samples, &censuses) {
                    self.upsert_rollup(&r)?;
                    report.rollups_written += 1;
                }
            }
        }

        // Continuous roll-up (History fix, 2026-09-08): every CLOSED bucket
        // inside the raw window gets its rollup NOW, not 24 h later. Before
        // this, `/rollups` held nothing younger than a day, so the app's
        // "5 hours" and "24 hours" History windows were empty by construction
        // on every machine, forever. The still-open bucket (the one holding
        // `now`) is excluded for the same reason as ever — a partial average
        // must not be recorded as final. Buckets already summarised are not
        // revisited: samples arrive in time order, so a closed bucket cannot
        // gain one, and the sweep runs once per bucket — the work per tick is
        // one bucket, or the whole backlog after a restart.
        let recent_cutoff = bucket_start_of(now)?;
        let resume_from = match self.newest_rollup_bucket()? {
            Some(newest) if newest >= raw_cutoff => {
                newest + Duration::seconds(crate::sample::ROLLUP_BUCKET_SECS)
            }
            _ => raw_cutoff,
        };
        if resume_from < recent_cutoff {
            let recent = self.samples_between(resume_from, recent_cutoff)?;
            let mut census_by_bucket: std::collections::BTreeMap<
                DateTime<Utc>,
                Vec<CensusScalars>,
            > = std::collections::BTreeMap::new();
            for cs in self.census_scalars_between(resume_from, recent_cutoff)? {
                census_by_bucket
                    .entry(bucket_start_of(cs.taken_at)?)
                    .or_default()
                    .push(cs);
            }
            let mut by_bucket: std::collections::BTreeMap<DateTime<Utc>, Vec<Sample>> =
                std::collections::BTreeMap::new();
            for smp in recent {
                by_bucket.entry(smp.bucket_start()?).or_default().push(smp);
            }
            for (bucket, samples) in by_bucket {
                let censuses = census_by_bucket.remove(&bucket).unwrap_or_default();
                if let Some(r) = rollup(bucket, &samples, &censuses) {
                    self.upsert_rollup(&r)?;
                    report.recent_rollups_written += 1;
                }
            }
        }

        report.raw_samples_deleted = self.conn.execute(
            "DELETE FROM samples WHERE sampled_at < ?1",
            params![wire_time::to_wire(&raw_cutoff)],
        )?;
        report.rollups_deleted = self.conn.execute(
            "DELETE FROM sample_rollups WHERE bucket_start < ?1",
            params![wire_time::to_wire(&(now - keep.rollups))],
        )?;
        // Only CLOSED episodes age out: an open one is live state, not history,
        // and deleting it would make the next evaluation reopen the incident as
        // new. The legacy point-event table (pre-v10) is drained on the same
        // clock so it stays bounded while its rows finish aging out.
        report.alerts_deleted = self.conn.execute(
            "DELETE FROM alert_episodes WHERE started_at < ?1 AND ended_at IS NOT NULL",
            params![wire_time::to_wire(&(now - keep.alerts))],
        )? + self.conn.execute(
            "DELETE FROM alert_events WHERE occurred_at < ?1",
            params![wire_time::to_wire(&(now - keep.alerts))],
        )?;
        // The census shares the raw window: it is a list of named things, not a
        // number, so there is nothing to roll it up INTO (schema v4's comment).
        // It ages out on the same clock as the raw samples.
        report.censuses_deleted = self.conn.execute(
            "DELETE FROM censuses WHERE taken_at < ?1",
            params![wire_time::to_wire(&raw_cutoff)],
        )?;

        Ok(report)
    }
}

fn parse_wire(row: &Row, idx: usize) -> rusqlite::Result<DateTime<Utc>> {
    let s: String = row.get(idx)?;
    wire_time::from_wire(&s).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(idx, rusqlite::types::Type::Text, Box::new(e))
    })
}

fn row_to_sample(row: &Row) -> rusqlite::Result<Sample> {
    let id: String = row.get(0)?;
    Ok(Sample {
        id: Uuid::parse_str(&id).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
        })?,
        sampled_at: parse_wire(row, 1)?,
        boot_time_secs: row.get(2)?,
        ncpu: row.get(3)?,
        mem_total_bytes: row.get(4)?,
        page_size: row.get(5)?,
        load1m: row.get(6)?,
        load5m: row.get(7)?,
        load15m: row.get(8)?,
        swap_total_bytes: row.get(9)?,
        swap_used_bytes: row.get(10)?,
        pages_free: row.get(11)?,
        pages_active: row.get(12)?,
        pages_inactive: row.get(13)?,
        pages_wired: row.get(14)?,
        pages_speculative: row.get(15)?,
        pages_compressor: row.get(16)?,
        swapins: row.get(17)?,
        swapouts: row.get(18)?,
        pages_purgeable: row.get(19)?,
        pages_external: row.get(20)?,
        memory_pressure_level: row.get(21)?,
        thermal_pressure_level: row.get(22)?,
        cpu_speed_limit_percent: row.get(23)?,
        cpu_ticks_total: row.get(24)?,
        cpu_ticks_idle: row.get(25)?,
        kernel_free_percent: row.get(26)?,
        jetsam_kills: row.get(27)?,
        volumes: Vec::new(),
    })
}

impl From<CoreError> for rusqlite::Error {
    fn from(e: CoreError) -> Self {
        rusqlite::Error::ToSqlConversionFailure(Box::new(std::io::Error::other(e.to_string())))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pressure::band::Band;
    use crate::pressure::config::Dimension;
    use crate::pressure::level::Level;
    use crate::sys::DATA_VOLUME;

    fn at(s: &str) -> DateTime<Utc> {
        wire_time::from_wire(s).unwrap()
    }

    fn sample_at(ts: &str, swapins: u64) -> Sample {
        Sample {
            id: Uuid::new_v4(),
            sampled_at: at(ts),
            boot_time_secs: 1_788_122_573,
            ncpu: 8,
            mem_total_bytes: 25_769_803_776,
            page_size: 16384,
            load1m: 1.5,
            load5m: 1.6,
            load15m: 1.7,
            swap_total_bytes: 8_589_934_592,
            swap_used_bytes: 7_078_019_072,
            pages_free: 13_267,
            pages_active: 324_446,
            pages_inactive: 319_599,
            pages_wired: 191_846,
            pages_speculative: 4_289,
            pages_compressor: 685_141,
            pages_purgeable: 5_581,
            pages_external: 321_369,
            swapins,
            swapouts: swapins * 2,
            // Non-zero: the schema DEFAULT is 0, and a fixture that matches the
            // default cannot see a dropped INSERT column.
            memory_pressure_level: 2,
            // Some(...): the v11 columns default to NULL, so a None fixture could
            // not see a dropped INSERT column either.
            thermal_pressure_level: Some(2),
            cpu_speed_limit_percent: Some(85),
            // Some(...): the v12 columns default to NULL, so a None fixture could
            // not see a dropped INSERT column. Distinct total/idle so a swap shows.
            cpu_ticks_total: Some(80_000_000),
            cpu_ticks_idle: Some(60_000_000),
            kernel_free_percent: Some(50),
            jetsam_kills: Some(0),
            volumes: vec![VolumeSample {
                mount_point: DATA_VOLUME.into(),
                total_bytes: 494_384_795_648,
                avail_bytes: 71_000_000_000,
            }],
        }
    }

    #[test]
    fn round_trips_a_sample_with_its_volumes() {
        let st = SampleStore::open_in_memory().unwrap();
        let s = sample_at("2026-08-31T15:00:00.000000Z", 1000);
        st.insert(&s).unwrap();

        let got = st.latest_sample().unwrap().unwrap();
        // WHOLE-STRUCT equality, not a field list. A field list silently stops
        // covering columns added later — which is exactly how an INSERT that
        // dropped `pages_purgeable`/`pages_external` survived the first
        // mutation pass: `DEFAULT 0` filled the gap and no assertion looked.
        // The helper's values for those fields are non-zero for the same
        // reason: a zero fixture is indistinguishable from the default.
        assert_eq!(got, s);
        assert_ne!(
            (s.pages_purgeable, s.pages_external),
            (0, 0),
            "fixture must not co-vary with the schema DEFAULT"
        );
        assert_ne!(
            s.memory_pressure_level, 0,
            "fixture must not co-vary with the schema DEFAULT"
        );
        assert!(
            s.thermal_pressure_level.is_some() && s.cpu_speed_limit_percent.is_some(),
            "fixture must not co-vary with the v11 columns' NULL default"
        );
        assert!(
            s.cpu_ticks_total.is_some() && s.cpu_ticks_idle.is_some(),
            "fixture must not co-vary with the v12 columns' NULL default"
        );
    }

    /// A row stored BEFORE schema v11 has no thermal columns; after the migration
    /// it must read back as None ("not recorded"), never as Some(0) ("nominal").
    /// Walks the real chain to v10, inserts a v10-shaped row by hand, migrates, and
    /// reads it through the store. Mutation-proof: a `row.get(22).unwrap_or(0)`
    /// or a `DEFAULT 0` in the v11 arm both turn this None into Some(0).
    #[test]
    fn a_pre_v11_row_reads_back_with_unrecorded_thermal_fields() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        crate::schema::apply_connection_pragmas(&conn).unwrap();
        for from in 0..10 {
            crate::schema::apply_migration(&conn, from).unwrap();
        }
        conn.pragma_update(None, "user_version", 10).unwrap();
        conn.execute_batch(
            "INSERT INTO samples (id, sampled_at, boot_time_secs, ncpu, mem_total_bytes,
                page_size, load_1m, load_5m, load_15m, swap_total_bytes, swap_used_bytes,
                pages_free, pages_active, pages_inactive, pages_wired, pages_speculative,
                pages_compressor, swapins, swapouts, pages_purgeable, pages_external,
                memory_pressure_level)
             VALUES ('0f0f0f0f-0000-4000-8000-000000000001', '2026-09-08T12:00:00.000000Z',
                1, 8, 1, 16384, 1.0, 1.0, 1.0, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1);",
        )
        .unwrap();
        // Opening the store is what runs the v10 -> v11 step, exactly as the daemon
        // does on a real upgrade.
        crate::schema::validate_or_init(&conn).unwrap();
        let st = SampleStore { conn };
        let got = st
            .latest_sample()
            .unwrap()
            .expect("the v10 row survives the migration");
        assert_eq!(
            got.thermal_pressure_level, None,
            "not recorded is None, not nominal"
        );
        assert_eq!(got.cpu_speed_limit_percent, None);
        // A v10 row also predates v12: the CPU ticks read back as None ("not
        // recorded"), never Some(0) ("idle"). A `row.get(24).unwrap_or(0)` or a
        // `DEFAULT 0` in the v12 arm would turn this None into Some(0).
        assert_eq!(got.cpu_ticks_total, None);
        assert_eq!(got.cpu_ticks_idle, None);
        // …and a fresh v11 write beside it round-trips its Some values.
        let s = sample_at("2026-09-08T12:00:15.000000Z", 1000);
        st.insert(&s).unwrap();
        assert_eq!(
            st.latest_sample().unwrap().unwrap().thermal_pressure_level,
            Some(2)
        );
    }

    /// An absurd reading must blemish one field, NOT discard the sample.
    ///
    /// SQLite's INTEGER is signed, and `rusqlite` rejects a `u64` above
    /// `i64::MAX` with `TryFromIntError(PosOverflow)` — which fails the entire
    /// INSERT. This test found that: for a daemon, losing the whole sample
    /// because one field is nonsense is the wrong failure. Mutation-proof:
    /// remove `clamp_i64` from the insert and this fails with PosOverflow.
    #[test]
    fn an_absurd_byte_count_saturates_instead_of_failing_the_insert() {
        let st = SampleStore::open_in_memory().unwrap();
        let mut s = sample_at("2026-08-31T15:00:00.000000Z", 1);
        s.swap_used_bytes = u64::MAX;
        s.volumes[0].total_bytes = u64::MAX;

        st.insert(&s)
            .expect("an absurd value must not fail the insert");

        let got = st.latest_sample().unwrap().unwrap();
        assert_eq!(
            got.swap_used_bytes,
            i64::MAX as u64,
            "saturated, not wrapped"
        );
        assert_eq!(got.volumes[0].total_bytes, i64::MAX as u64);
        // The rest of the sample is intact — that is the whole point.
        assert_eq!(got.swapins, 1);
        assert_eq!(got.pages_compressor, 685_141);
        assert_eq!(got.volumes[0].mount_point, DATA_VOLUME);
    }

    /// Every realistic value must be exact. The clamp is a backstop for
    /// nonsense, not a lossy encoding of normal readings — a 24 GB machine with
    /// a 494 GB volume must round-trip byte-for-byte.
    #[test]
    fn realistic_byte_counts_round_trip_exactly() {
        let st = SampleStore::open_in_memory().unwrap();
        let s = sample_at("2026-08-31T15:00:00.000000Z", 1_284_918);
        st.insert(&s).unwrap();
        let got = st.latest_sample().unwrap().unwrap();
        assert_eq!(got.mem_total_bytes, 25_769_803_776);
        assert_eq!(got.swap_used_bytes, 7_078_019_072);
        assert_eq!(got.swapins, 1_284_918);
        assert_eq!(got.volumes[0].total_bytes, 494_384_795_648);
        assert_eq!(got.volumes[0].avail_bytes, 71_000_000_000);
    }

    #[test]
    fn recent_samples_returns_oldest_first_so_pairs_can_be_diffed() {
        let st = SampleStore::open_in_memory().unwrap();
        for (i, ts) in [
            "2026-08-31T15:00:00.000000Z",
            "2026-08-31T15:00:15.000000Z",
            "2026-08-31T15:00:30.000000Z",
        ]
        .iter()
        .enumerate()
        {
            st.insert(&sample_at(ts, 1000 + i as u64 * 10)).unwrap();
        }
        let got = st.recent_samples(2).unwrap();
        assert_eq!(got.len(), 2);
        assert!(
            got[0].sampled_at < got[1].sampled_at,
            "must be ascending so consecutive pairs diff correctly"
        );
        // The two MOST RECENT, not the two oldest.
        assert_eq!(got[0].swapins, 1010);
        assert_eq!(got[1].swapins, 1020);
    }

    /// Windows are half-open `[from, to)` so adjacent windows tile without
    /// double-counting the boundary sample. Mutation-proof: make the upper
    /// bound inclusive (`<=`) and this returns 3.
    #[test]
    fn sample_windows_are_half_open() {
        let st = SampleStore::open_in_memory().unwrap();
        for ts in [
            "2026-08-31T15:00:00.000000Z",
            "2026-08-31T15:04:59.000000Z",
            "2026-08-31T15:05:00.000000Z", // exactly the upper bound
        ] {
            st.insert(&sample_at(ts, 1)).unwrap();
        }
        let got = st
            .samples_between(
                at("2026-08-31T15:00:00.000000Z"),
                at("2026-08-31T15:05:00.000000Z"),
            )
            .unwrap();
        assert_eq!(got.len(), 2, "the upper bound must be exclusive");
    }

    #[test]
    fn rollups_round_trip_with_their_volumes() {
        let st = SampleStore::open_in_memory().unwrap();
        let samples = vec![
            sample_at("2026-08-31T15:00:00.000000Z", 100),
            sample_at("2026-08-31T15:02:00.000000Z", 150),
        ];
        // Non-null census scalars, so an INSERT/SELECT that dropped one of the
        // v9 columns cannot hide behind the NULL it would read back as.
        let census = CensusScalars {
            taken_at: at("2026-08-31T15:00:30.000000Z"),
            total_procs: 912,
            stale_sessions: 17,
            orphans: 45,
            monitor_percent: 49.2,
        };
        let r = rollup(at("2026-08-31T15:00:00.000000Z"), &samples, &[census]).unwrap();
        st.upsert_rollup(&r).unwrap();

        let got = st
            .rollups_between(
                at("2026-08-31T15:00:00.000000Z"),
                at("2026-08-31T15:05:00.000000Z"),
            )
            .unwrap();
        assert_eq!(got.len(), 1);
        // WHOLE-STRUCT equality, same rule as the sample round-trip: a field
        // list silently stops covering columns added later.
        assert_eq!(got[0], r);
        assert_eq!(got[0].sample_count, 2);
        assert_eq!(got[0].swapins_delta, Some(50));
        assert_eq!(got[0].stale_sessions_max, Some(17));
        assert_eq!(got[0].volumes.len(), 1);
        assert_eq!(got[0].volumes[0].mount_point, DATA_VOLUME);
    }

    /// Re-rolling a bucket must replace it. Mutation-proof: change the upsert to
    /// a plain INSERT and this fails on a UNIQUE violation; drop the
    /// `DELETE FROM volume_rollups` and the volume rows double.
    #[test]
    fn re_rolling_a_bucket_replaces_rather_than_duplicates() {
        let st = SampleStore::open_in_memory().unwrap();
        let b = at("2026-08-31T15:00:00.000000Z");

        let first = rollup(b, &[sample_at("2026-08-31T15:00:00.000000Z", 100)], &[]).unwrap();
        st.upsert_rollup(&first).unwrap();

        let second = rollup(
            b,
            &[
                sample_at("2026-08-31T15:00:00.000000Z", 100),
                sample_at("2026-08-31T15:03:00.000000Z", 175),
            ],
            &[],
        )
        .unwrap();
        st.upsert_rollup(&second).unwrap();

        assert_eq!(st.count_rollups().unwrap(), 1, "one bucket, not two");
        let got = st
            .rollups_between(b, at("2026-08-31T15:05:00.000000Z"))
            .unwrap();
        assert_eq!(got[0].sample_count, 2, "the newer aggregate wins");
        assert_eq!(got[0].swapins_delta, Some(75));
        assert_eq!(got[0].volumes.len(), 1, "volume rows must not accumulate");
    }

    /// The core retention property: old raw samples become rollups and then go
    /// away, while recent ones are untouched.
    #[test]
    fn sweep_rolls_up_and_then_deletes_aged_raw_samples() {
        let st = SampleStore::open_in_memory().unwrap();
        let now = at("2026-09-01T12:00:00.000000Z");

        // Two samples ~30h old (outside the 24h raw window), same bucket.
        st.insert(&sample_at("2026-08-31T06:00:00.000000Z", 100))
            .unwrap();
        st.insert(&sample_at("2026-08-31T06:03:00.000000Z", 160))
            .unwrap();
        // One recent sample, inside the window.
        st.insert(&sample_at("2026-09-01T11:59:00.000000Z", 500))
            .unwrap();

        assert_eq!(st.count_samples().unwrap(), 3);

        let rep = st.sweep(now, Retention::default()).unwrap();

        assert_eq!(rep.rollups_written, 1, "the aged bucket was summarised");
        assert_eq!(rep.raw_samples_deleted, 2);
        assert_eq!(st.count_samples().unwrap(), 1, "the recent sample survives");
        // Since the History fix the recent sample's bucket (11:55, closed at
        // 12:00) is summarised too — with its raw row kept — so two rollups.
        assert_eq!(rep.recent_rollups_written, 1);
        assert_eq!(st.count_rollups().unwrap(), 2);

        let rl = st
            .rollups_between(
                at("2026-08-31T06:00:00.000000Z"),
                at("2026-08-31T06:05:00.000000Z"),
            )
            .unwrap();
        assert_eq!(rl.len(), 1);
        assert_eq!(rl[0].sample_count, 2);
        assert_eq!(
            rl[0].swapins_delta,
            Some(60),
            "the delta survives the raw rows"
        );
    }

    /// The History fix: a bucket that has CLOSED inside the raw window is rolled
    /// up on the very next sweep, with its raw samples left in place; the bucket
    /// that still holds `now` is not. A second sweep with nothing new writes
    /// nothing (samples arrive in order; a closed bucket cannot grow). Mutation-
    /// proof: drop the continuous step and `recent_rollups_written` is 0 with
    /// `/rollups` empty for the window; roll the open bucket too and the 12:00
    /// bucket appears with one sample; delete raw rows for rolled buckets and
    /// `count_samples` falls.
    #[test]
    fn sweep_rolls_up_closed_buckets_inside_the_raw_window_and_keeps_their_samples() {
        let st = SampleStore::open_in_memory().unwrap();
        // 11:50 bucket: two samples; 11:55 bucket: one; 12:00 bucket: OPEN.
        st.insert(&sample_at("2026-09-01T11:50:30.000000Z", 100))
            .unwrap();
        st.insert(&sample_at("2026-09-01T11:52:00.000000Z", 130))
            .unwrap();
        st.insert(&sample_at("2026-09-01T11:57:00.000000Z", 190))
            .unwrap();
        st.insert(&sample_at("2026-09-01T12:00:10.000000Z", 200))
            .unwrap();
        let now = at("2026-09-01T12:00:30.000000Z");

        let rep = st.sweep(now, Retention::default()).unwrap();
        assert_eq!(
            rep.rollups_written, 0,
            "nothing has aged out of the raw window"
        );
        assert_eq!(rep.recent_rollups_written, 2, "the two CLOSED buckets");
        assert_eq!(rep.raw_samples_deleted, 0, "recent raw samples stay");
        assert_eq!(st.count_samples().unwrap(), 4);

        let rl = st
            .rollups_between(
                at("2026-09-01T11:00:00.000000Z"),
                at("2026-09-01T13:00:00.000000Z"),
            )
            .unwrap();
        let buckets: Vec<(String, u32)> = rl
            .iter()
            .map(|r| (wire_time::to_wire(&r.bucket_start), r.sample_count))
            .collect();
        assert_eq!(
            buckets,
            vec![
                ("2026-09-01T11:50:00.000000Z".to_string(), 2),
                ("2026-09-01T11:55:00.000000Z".to_string(), 1),
            ],
            "closed buckets only; the open 12:00 bucket is not summarised early"
        );
        assert_eq!(
            rl[0].swapins_delta,
            Some(30),
            "a real rollup, deltas included"
        );

        // Nothing new: the second sweep is a no-op for the recent tier.
        let rep = st.sweep(now, Retention::default()).unwrap();
        assert_eq!(
            rep.recent_rollups_written, 0,
            "already summarised buckets are not revisited"
        );

        // Time passes: the 12:00 bucket closes and gets its rollup; still no deletes.
        let later = at("2026-09-01T12:05:01.000000Z");
        let rep = st.sweep(later, Retention::default()).unwrap();
        assert_eq!(rep.recent_rollups_written, 1);
        assert_eq!(rep.raw_samples_deleted, 0);
        assert_eq!(st.count_rollups().unwrap(), 3);
    }

    /// The continuous tier and the aged tier meet without a gap or a double
    /// count: a bucket first rolled up while recent is revisited by the aged
    /// pass with the SAME samples (idempotent upsert), then its raw rows go.
    #[test]
    fn a_bucket_rolled_up_while_recent_is_not_double_counted_when_it_ages_out() {
        let st = SampleStore::open_in_memory().unwrap();
        st.insert(&sample_at("2026-09-01T06:00:10.000000Z", 100))
            .unwrap();
        st.insert(&sample_at("2026-09-01T06:02:00.000000Z", 140))
            .unwrap();
        // Recent: rolled up as a closed bucket.
        let rep = st
            .sweep(at("2026-09-01T06:10:00.000000Z"), Retention::default())
            .unwrap();
        assert_eq!(rep.recent_rollups_written, 1);
        assert_eq!(st.count_rollups().unwrap(), 1);
        // 30 h later it ages out: the aged pass rewrites the same bucket and
        // deletes the raw rows; one rollup row, same numbers.
        let rep = st
            .sweep(at("2026-09-02T12:00:00.000000Z"), Retention::default())
            .unwrap();
        assert_eq!(rep.rollups_written, 1);
        assert_eq!(rep.raw_samples_deleted, 2);
        assert_eq!(st.count_rollups().unwrap(), 1);
        let rl = st
            .rollups_between(
                at("2026-09-01T06:00:00.000000Z"),
                at("2026-09-01T06:05:00.000000Z"),
            )
            .unwrap();
        assert_eq!(rl[0].sample_count, 2);
        assert_eq!(rl[0].swapins_delta, Some(40));
    }

    /// The month-scale sprawl trend (`banshee-yj1`): a census that ages out must
    /// leave its scalar counters in the bucket's rollup, because after the
    /// sweep's DELETE they exist nowhere else. Mutation-proof: pass `&[]`
    /// instead of the bucket's censuses in `sweep` and every scalar reads back
    /// NULL; the census factory's numbers (1 stale of 2 sessions, 3 orphans)
    /// also distinguish "stale count" from "session count".
    #[test]
    fn sweep_folds_census_scalars_into_the_rollup_before_deleting_them() {
        let st = SampleStore::open_in_memory().unwrap();
        let now = at("2026-09-01T12:00:00.000000Z");

        // An aged bucket with samples AND a census in it…
        st.insert(&sample_at("2026-08-31T06:00:00.000000Z", 100))
            .unwrap();
        st.insert(&sample_at("2026-08-31T06:03:00.000000Z", 160))
            .unwrap();
        st.insert_census(&census_at("2026-08-31T06:01:00.000000Z"))
            .unwrap();
        // …and a RECENT census that must neither be deleted nor fold into
        // anything (its bucket is still open).
        st.insert_census(&census_at("2026-09-01T11:58:00.000000Z"))
            .unwrap();

        let rep = st.sweep(now, Retention::default()).unwrap();
        assert_eq!(rep.rollups_written, 1);
        assert_eq!(rep.censuses_deleted, 1, "only the aged census goes");
        assert_eq!(st.count_censuses().unwrap(), 1);

        let rl = st
            .rollups_between(
                at("2026-08-31T06:00:00.000000Z"),
                at("2026-08-31T06:05:00.000000Z"),
            )
            .unwrap();
        assert_eq!(rl.len(), 1);
        assert_eq!(rl[0].stale_sessions_max, Some(1), "1 stale of 2 sessions");
        assert_eq!(rl[0].orphans_max, Some(3));
        assert_eq!(rl[0].monitor_percent_max, Some(18.2));
        assert_eq!(rl[0].total_procs_max, Some(1234));
    }

    /// Order is load-bearing. Mutation-proof: move the raw DELETE above the
    /// roll-up loop and this fails — `rollups_written` drops to 0 and the
    /// 30-day history has a permanent, silent hole.
    #[test]
    fn sweep_summarises_before_deleting_so_history_has_no_holes() {
        let st = SampleStore::open_in_memory().unwrap();
        let now = at("2026-09-01T12:00:00.000000Z");
        for ts in [
            "2026-08-20T01:00:00.000000Z",
            "2026-08-20T01:01:00.000000Z",
            "2026-08-25T09:00:00.000000Z",
        ] {
            st.insert(&sample_at(ts, 10)).unwrap();
        }

        let rep = st.sweep(now, Retention::default()).unwrap();
        assert_eq!(rep.raw_samples_deleted, 3);
        assert_eq!(
            rep.rollups_written, 2,
            "every deleted sample must be represented by a rollup first"
        );
        assert_eq!(st.count_rollups().unwrap(), 2);
    }

    /// A bucket still inside the raw window must NOT be summarised: it can still
    /// gain samples, and recording a partial average as final is a silent lie.
    /// Mutation-proof: use `now` instead of `now - keep.raw` as the cutoff and
    /// this writes a rollup for the live bucket.
    #[test]
    fn sweep_leaves_the_current_window_unrolled() {
        let st = SampleStore::open_in_memory().unwrap();
        let now = at("2026-09-01T12:00:00.000000Z");
        st.insert(&sample_at("2026-09-01T11:58:00.000000Z", 1))
            .unwrap();
        st.insert(&sample_at("2026-09-01T11:59:00.000000Z", 2))
            .unwrap();

        let rep = st.sweep(now, Retention::default()).unwrap();
        assert_eq!(rep.rollups_written, 0);
        assert_eq!(rep.raw_samples_deleted, 0);
        assert_eq!(
            st.count_samples().unwrap(),
            2,
            "recent samples are untouched"
        );
    }

    /// Rollups themselves age out at 30 days.
    #[test]
    fn sweep_deletes_rollups_past_the_rollup_window() {
        let st = SampleStore::open_in_memory().unwrap();
        let now = at("2026-09-01T12:00:00.000000Z");

        // A rollup from 40 days ago, and one from 10 days ago.
        for ts in ["2026-07-23T00:00:00.000000Z", "2026-08-22T00:00:00.000000Z"] {
            let r = rollup(at(ts), &[sample_at(ts, 1)], &[]).unwrap();
            st.upsert_rollup(&r).unwrap();
        }
        assert_eq!(st.count_rollups().unwrap(), 2);

        let rep = st.sweep(now, Retention::default()).unwrap();
        assert_eq!(rep.rollups_deleted, 1);
        assert_eq!(
            st.count_rollups().unwrap(),
            1,
            "only the recent rollup survives"
        );
    }

    /// Deleting a rollup must take its volume rows with it, or the sweep leaks
    /// exactly the way it would for raw samples.
    #[test]
    fn deleting_a_rollup_cascades_to_its_volume_rows() {
        let st = SampleStore::open_in_memory().unwrap();
        let now = at("2026-09-01T12:00:00.000000Z");
        let old = "2026-07-23T00:00:00.000000Z";
        let r = rollup(at(old), &[sample_at(old, 1)], &[]).unwrap();
        st.upsert_rollup(&r).unwrap();

        let before: i64 = st
            .conn
            .query_row("SELECT count(*) FROM volume_rollups", [], |r| r.get(0))
            .unwrap();
        assert_eq!(before, 1);

        st.sweep(now, Retention::default()).unwrap();

        let after: i64 = st
            .conn
            .query_row("SELECT count(*) FROM volume_rollups", [], |r| r.get(0))
            .unwrap();
        assert_eq!(after, 0, "volume rollups must cascade with their bucket");
    }

    /// Retention windows are configuration, not constants (ADR-0006).
    #[test]
    fn retention_windows_are_configurable() {
        let st = SampleStore::open_in_memory().unwrap();
        let now = at("2026-09-01T12:00:00.000000Z");
        st.insert(&sample_at("2026-09-01T11:00:00.000000Z", 1))
            .unwrap();

        // Default 24h keeps it; a 30-minute raw window does not.
        let rep = st
            .sweep(
                now,
                Retention {
                    raw: Duration::minutes(30),
                    ..Retention::default()
                },
            )
            .unwrap();
        assert_eq!(rep.raw_samples_deleted, 1);
        assert_eq!(st.count_samples().unwrap(), 0);
    }

    #[test]
    fn sweep_on_an_empty_store_is_a_no_op() {
        let st = SampleStore::open_in_memory().unwrap();
        let rep = st
            .sweep(at("2026-09-01T12:00:00.000000Z"), Retention::default())
            .unwrap();
        assert_eq!(rep, SweepReport::default());
    }

    #[test]
    fn reports_its_own_size_on_disk() {
        let dir = tempfile::tempdir().unwrap();
        let st = SampleStore::open(&dir.path().join("banshee.db")).unwrap();
        let empty = st.size_bytes().unwrap();
        for i in 0..50 {
            st.insert(&sample_at("2026-08-31T15:00:00.000000Z", i))
                .unwrap();
        }
        assert!(st.size_bytes().unwrap() >= empty, "size must not shrink");
        assert!(empty > 0, "an initialized database occupies pages");
    }

    #[test]
    fn persists_across_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("banshee.db");
        let id = {
            let st = SampleStore::open(&path).unwrap();
            let s = sample_at("2026-08-31T15:00:00.000000Z", 7);
            st.insert(&s).unwrap();
            s.id
        };
        let st = SampleStore::open(&path).unwrap();
        let got = st.latest_sample().unwrap().unwrap();
        assert_eq!(got.id, id);
        assert_eq!(got.volumes.len(), 1, "volume rows persist too");
    }

    /// A batch read must not issue one volume query per sample. Mutation-proof:
    /// this asserts the batch path returns every sample's volumes correctly,
    /// which a naive per-sample loop would also pass — so it is paired with the
    /// single-query implementation rather than proving it. The N+1 concern is
    /// documented on `attach_volumes`.
    #[test]
    fn batch_reads_attach_volumes_to_the_right_samples() {
        let st = SampleStore::open_in_memory().unwrap();
        let mut a = sample_at("2026-08-31T15:00:00.000000Z", 1);
        let mut b = sample_at("2026-08-31T15:00:15.000000Z", 2);
        a.volumes[0].avail_bytes = 111;
        b.volumes[0].avail_bytes = 222;
        b.volumes.push(VolumeSample {
            mount_point: "/Volumes/Backup".into(),
            total_bytes: 900,
            avail_bytes: 333,
        });
        st.insert(&a).unwrap();
        st.insert(&b).unwrap();

        let got = st.recent_samples(10).unwrap();
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].volumes.len(), 1);
        assert_eq!(got[0].volumes[0].avail_bytes, 111);
        assert_eq!(got[1].volumes.len(), 2);
        let backup = got[1]
            .volumes
            .iter()
            .find(|v| v.mount_point == "/Volumes/Backup")
            .expect("the second sample's extra volume");
        assert_eq!(backup.avail_bytes, 333);
    }

    // ---- census tier -----------------------------------------------------

    fn census_at(ts: &str) -> Census {
        Census {
            id: Uuid::new_v4(),
            taken_at: at(ts),
            total_procs: 1234,
            agent_sessions: vec![
                AgentSession {
                    pid: 88120,
                    program: "claude".into(),
                    rss_bytes: 96_256 * 1024,
                    age_secs: 8 * 86_400 + 3600,
                    age_days: 8,
                    cpu_secs: 720.0,
                    tty: "ttys014".into(),
                    is_stale: true,
                    cwd: Some("/Users/example/Code/thing".into()),
                },
                AgentSession {
                    pid: 2833,
                    program: "claude".into(),
                    rss_bytes: 276_480 * 1024,
                    age_secs: 4 * 3600,
                    age_days: 0,
                    cpu_secs: 431.32,
                    tty: "ttys005".into(),
                    is_stale: false,
                    cwd: None,
                },
            ],
            ide_helpers: HelperRollup {
                count: 2,
                rss_bytes: 131_072 * 1024,
            },
            orphans: OrphanCensus {
                total_count: 149,
                total_rss_bytes: 1_900_000_000,
                orphan_count: 3,
                orphan_rss_bytes: 108_544 * 1024,
                orphans_by_program: vec![ProgramCount {
                    program: "example-notes-mcp-server".into(),
                    count: 2,
                    rss_bytes: 86_016 * 1024,
                }],
            },
            tmux_sessions: vec![
                TmuxSessionInfo {
                    name: "example-idle".into(),
                    pane_command: "zsh".into(),
                    idle_days: Some(3.4),
                    is_busy: false,
                    is_stale: true,
                },
                TmuxSessionInfo {
                    name: "example-undated".into(),
                    pane_command: "zsh".into(),
                    idle_days: None,
                    is_busy: false,
                    is_stale: false,
                },
            ],
            app_groups: vec![AppGroup {
                name: "Google Chrome".into(),
                proc_count: 64,
                rss_bytes: 3_800_000_000,
            }],
            monitor_agents: vec![MonitorAgent {
                name: "ExampleEDR".into(),
                proc_count: 2,
                rss_bytes: 512_000 * 1024,
                cpu_secs_total: 14_205.0,
                percent_of_one_core: 18.2,
                longest_life_secs: 78_000,
            }],
            monitor_total: MonitorTotal {
                proc_count: 2,
                rss_bytes: 512_000 * 1024,
                cpu_secs_total: 14_205.0,
                percent_of_one_core: 18.2,
                longest_life_secs: 78_000,
            },
            tmux_available: true,
        }
    }

    #[test]
    fn a_census_round_trips_with_every_child_row() {
        let st = SampleStore::open_in_memory().unwrap();
        let c = census_at("2026-08-31T15:00:00.000000Z");
        st.insert_census(&c).unwrap();

        let got = st.latest_census().unwrap().expect("a census");
        assert_eq!(got.id, c.id);
        assert_eq!(got.taken_at, c.taken_at);
        assert_eq!(got.total_procs, 1234);
        assert!(got.tmux_available);

        assert_eq!(got.agent_sessions.len(), 2);
        assert_eq!(got.agent_sessions[0].pid, 88120, "oldest first");
        assert_eq!(got.agent_sessions[0].age_days, 8);
        assert!(got.agent_sessions[0].is_stale);
        assert_eq!(
            got.agent_sessions[0].cwd.as_deref(),
            Some("/Users/example/Code/thing")
        );

        assert_eq!(got.ide_helpers.count, 2);
        assert_eq!(got.orphans.total_count, 149);
        assert_eq!(got.orphans.orphan_count, 3);
        assert_eq!(got.orphans.orphans_by_program.len(), 1);
        assert_eq!(got.tmux_sessions.len(), 2);
        assert_eq!(got.app_groups[0].name, "Google Chrome");
        assert_eq!(got.monitor_agents[0].name, "ExampleEDR");
        assert!((got.monitor_agents[0].percent_of_one_core - 18.2).abs() < 1e-9);
    }

    /// A non-stale session's NULL cwd means "we did not run lsof", not "no cwd".
    /// It must survive the round trip as NULL rather than becoming an empty
    /// string, which would read as a real answer.
    #[test]
    fn a_null_cwd_survives_the_round_trip_as_null() {
        let st = SampleStore::open_in_memory().unwrap();
        st.insert_census(&census_at("2026-08-31T15:00:00.000000Z"))
            .unwrap();
        let got = st.latest_census().unwrap().unwrap();
        let fresh = got.agent_sessions.iter().find(|s| !s.is_stale).unwrap();
        assert_eq!(fresh.cwd, None, "not looked at, not empty");
    }

    /// Same distinction for an unknown tmux idle age.
    #[test]
    fn an_unknown_tmux_idle_age_survives_as_null() {
        let st = SampleStore::open_in_memory().unwrap();
        st.insert_census(&census_at("2026-08-31T15:00:00.000000Z"))
            .unwrap();
        let got = st.latest_census().unwrap().unwrap();
        let undated = got
            .tmux_sessions
            .iter()
            .find(|t| t.name == "example-undated")
            .unwrap();
        assert_eq!(undated.idle_days, None);
    }

    #[test]
    fn latest_census_returns_the_newest_of_several() {
        let st = SampleStore::open_in_memory().unwrap();
        let old = census_at("2026-08-31T15:00:00.000000Z");
        let mut new = census_at("2026-08-31T15:05:00.000000Z");
        new.total_procs = 4321;
        st.insert_census(&old).unwrap();
        st.insert_census(&new).unwrap();
        assert_eq!(st.count_censuses().unwrap(), 2);
        assert_eq!(st.latest_census().unwrap().unwrap().total_procs, 4321);
    }

    #[test]
    fn latest_census_on_an_empty_store_is_none() {
        let st = SampleStore::open_in_memory().unwrap();
        assert!(st.latest_census().unwrap().is_none());
    }

    /// The census ages out on the raw window, and its child rows must cascade —
    /// otherwise the sweep leaks five child tables per census, 288 times a day.
    /// Mutation-proof: remove the census DELETE from `sweep` and this fails.
    #[test]
    fn sweep_deletes_aged_censuses_and_cascades_their_children() {
        let st = SampleStore::open_in_memory().unwrap();
        let now = at("2026-09-01T12:00:00.000000Z");
        st.insert_census(&census_at("2026-08-31T06:00:00.000000Z"))
            .unwrap(); // 30h old
        st.insert_census(&census_at("2026-09-01T11:55:00.000000Z"))
            .unwrap(); // recent

        let children = |st: &SampleStore| -> i64 {
            st.conn
                .query_row(
                    "SELECT (SELECT count(*) FROM census_agent_sessions)
                          + (SELECT count(*) FROM census_orphan_programs)
                          + (SELECT count(*) FROM census_tmux_sessions)
                          + (SELECT count(*) FROM census_app_groups)
                          + (SELECT count(*) FROM census_monitor_agents)",
                    [],
                    |r| r.get(0),
                )
                .unwrap()
        };
        assert_eq!(children(&st), 14, "2 censuses x 7 child rows");

        let rep = st.sweep(now, Retention::default()).unwrap();
        assert_eq!(rep.censuses_deleted, 1);
        assert_eq!(st.count_censuses().unwrap(), 1, "the recent one survives");
        assert_eq!(children(&st), 7, "the aged census's children cascaded");
    }

    #[test]
    fn sweep_keeps_recent_censuses() {
        let st = SampleStore::open_in_memory().unwrap();
        st.insert_census(&census_at("2026-09-01T11:55:00.000000Z"))
            .unwrap();
        let rep = st
            .sweep(at("2026-09-01T12:00:00.000000Z"), Retention::default())
            .unwrap();
        assert_eq!(rep.censuses_deleted, 0);
        assert_eq!(st.count_censuses().unwrap(), 1);
    }

    // ---- alert episodes --------------------------------------------------

    fn episode(
        dimension: Dimension,
        level: Level,
        started: &str,
        ended: Option<&str>,
    ) -> AlertEpisode {
        use crate::pressure::episode::{EpisodePeak, EpisodeState};
        AlertEpisode {
            id: Uuid::new_v4(),
            dimension,
            started_at: at(started),
            ended_at: ended.map(at),
            peak: EpisodePeak {
                band: Band::Red,
                level,
                severity: 2.5,
                at: at(started),
                message: "something happened".into(),
                who_line: None,
            },
            census_at_peak: None,
            suppressed: 3,
            state: if ended.is_some() {
                EpisodeState::Closed
            } else {
                EpisodeState::Firing
            },
            last_notified_at: Some(at(started)),
            recovering_since: ended.map(at),
        }
    }

    #[test]
    fn an_episode_round_trips_through_the_store() {
        let st = SampleStore::open_in_memory().unwrap();
        let e = episode(
            Dimension::Swap,
            Level::Shrieking,
            "2026-08-31T15:00:00.000000Z",
            Some("2026-08-31T16:00:00.000000Z"),
        );
        st.upsert_episode(&e).unwrap();

        let got = st
            .episodes_since(at("2026-08-31T00:00:00.000000Z"))
            .unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0], e, "the whole episode, not just the typed columns");
        assert_eq!(st.count_episodes().unwrap(), 1);
    }

    /// The upsert ADVANCES a stored episode in place: same id, new state, and the
    /// typed `ended_at` column follows the JSON so the open-episode index stays
    /// truthful. Mutation-proof: make it a plain INSERT and the second write
    /// fails on the primary key; drop `ended_at` from the UPDATE and the closed
    /// episode is still reported open.
    #[test]
    fn upserting_an_episode_advances_it_in_place() {
        use crate::pressure::episode::EpisodeState;
        let st = SampleStore::open_in_memory().unwrap();
        let mut e = episode(
            Dimension::Cpu,
            Level::Wailing,
            "2026-08-31T15:00:00.000000Z",
            None,
        );
        st.upsert_episode(&e).unwrap();
        assert_eq!(st.open_episodes().unwrap().len(), 1, "open");

        e.state = EpisodeState::Closed;
        e.ended_at = Some(at("2026-08-31T15:30:00.000000Z"));
        e.suppressed = 7;
        st.upsert_episode(&e).unwrap();

        assert_eq!(st.count_episodes().unwrap(), 1, "one row, advanced");
        assert!(
            st.open_episodes().unwrap().is_empty(),
            "the typed column followed"
        );
        let got = st
            .episodes_since(at("2026-08-31T00:00:00.000000Z"))
            .unwrap();
        assert_eq!(got[0].suppressed, 7);
        assert_eq!(got[0].state, EpisodeState::Closed);
    }

    /// Newest first, so a capped read drops the oldest.
    #[test]
    fn episodes_are_returned_newest_first() {
        let st = SampleStore::open_in_memory().unwrap();
        for ts in [
            "2026-08-31T15:00:00.000000Z",
            "2026-08-31T16:00:00.000000Z",
            "2026-08-31T14:00:00.000000Z",
        ] {
            st.upsert_episode(&episode(Dimension::Cpu, Level::Wailing, ts, Some(ts)))
                .unwrap();
        }
        let got = st
            .episodes_since(at("2026-08-31T00:00:00.000000Z"))
            .unwrap();
        assert_eq!(got.len(), 3);
        assert!(got[0].started_at > got[1].started_at);
        assert!(got[1].started_at > got[2].started_at);
    }

    /// The window is honoured for CLOSED episodes — and ignored for OPEN ones,
    /// which are the most relevant thing on the list however old they are.
    /// Mutation-proof: drop the `OR ended_at IS NULL` and the three-day-old
    /// firing episode vanishes from a 24h read.
    #[test]
    fn a_window_excludes_old_closed_episodes_but_never_an_open_one() {
        let st = SampleStore::open_in_memory().unwrap();
        st.upsert_episode(&episode(
            Dimension::Cpu,
            Level::Wailing,
            "2026-08-01T00:00:00.000000Z",
            Some("2026-08-01T01:00:00.000000Z"),
        ))
        .unwrap();
        st.upsert_episode(&episode(
            Dimension::Swap,
            Level::Wailing,
            "2026-08-28T00:00:00.000000Z",
            None,
        ))
        .unwrap();
        st.upsert_episode(&episode(
            Dimension::Disk,
            Level::Wailing,
            "2026-08-31T15:00:00.000000Z",
            Some("2026-08-31T15:30:00.000000Z"),
        ))
        .unwrap();
        let got = st
            .episodes_since(at("2026-08-31T00:00:00.000000Z"))
            .unwrap();
        let dims: Vec<Dimension> = got.iter().map(|e| e.dimension).collect();
        assert_eq!(dims, vec![Dimension::Disk, Dimension::Swap]);
    }

    /// An unparseable row must not stop the lifecycle from continuing. Losing one
    /// old episode is far better than re-announcing everything. Mutation-proof:
    /// collect into a Result and this fails.
    #[test]
    fn an_unparseable_episode_row_is_skipped_not_fatal() {
        let st = SampleStore::open_in_memory().unwrap();
        st.upsert_episode(&episode(
            Dimension::Cpu,
            Level::Wailing,
            "2026-08-31T15:00:00.000000Z",
            None,
        ))
        .unwrap();
        st.conn
            .execute(
                "INSERT INTO alert_episodes (id, dimension, started_at, ended_at, detail)
                 VALUES ('bogus', 'cpu', '2026-08-31T15:30:00.000000Z', NULL, 'not json')",
                [],
            )
            .unwrap();

        assert_eq!(
            st.open_episodes().unwrap().len(),
            1,
            "the good row survives"
        );
        assert_eq!(
            st.count_episodes().unwrap(),
            2,
            "both rows are still stored"
        );
    }

    /// Episodes outlive the samples that produced them: a one-year window against
    /// the samples' 24 hours (ADR-0006).
    #[test]
    fn sweep_keeps_episodes_long_after_the_samples_are_gone() {
        let st = SampleStore::open_in_memory().unwrap();
        let now = at("2026-09-01T12:00:00.000000Z");
        st.insert(&sample_at("2026-08-20T00:00:00.000000Z", 1))
            .unwrap();
        st.upsert_episode(&episode(
            Dimension::Cpu,
            Level::Wailing,
            "2026-08-20T00:00:00.000000Z",
            Some("2026-08-20T01:00:00.000000Z"),
        ))
        .unwrap();

        let rep = st.sweep(now, Retention::default()).unwrap();
        assert_eq!(rep.raw_samples_deleted, 1, "the sample aged out");
        assert_eq!(rep.alerts_deleted, 0, "the episode did not");
        assert_eq!(st.count_episodes().unwrap(), 1);
    }

    /// Past a year, CLOSED episodes go and OPEN ones stay: an open episode is live
    /// state, and deleting it would make the next evaluation reopen the incident
    /// as new. Mutation-proof: drop `AND ended_at IS NOT NULL` and the open one
    /// is deleted too.
    #[test]
    fn sweep_deletes_closed_episodes_past_a_year_but_never_open_ones() {
        let st = SampleStore::open_in_memory().unwrap();
        st.upsert_episode(&episode(
            Dimension::Cpu,
            Level::Wailing,
            "2025-01-01T00:00:00.000000Z",
            Some("2025-01-01T02:00:00.000000Z"),
        ))
        .unwrap();
        st.upsert_episode(&episode(
            Dimension::Swap,
            Level::Wailing,
            "2025-01-01T00:00:00.000000Z",
            None,
        ))
        .unwrap();
        let rep = st
            .sweep(at("2026-09-01T12:00:00.000000Z"), Retention::default())
            .unwrap();
        assert_eq!(rep.alerts_deleted, 1);
        let left = st.open_episodes().unwrap();
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].dimension, Dimension::Swap);
    }

    /// The legacy `alert_events` table (pre-v10 point events) keeps draining on
    /// the same clock, so it stays bounded while its rows finish aging out.
    #[test]
    fn sweep_still_drains_the_legacy_alert_events_table() {
        let st = SampleStore::open_in_memory().unwrap();
        st.conn
            .execute(
                "INSERT INTO alert_events (id, occurred_at, dimension, level, detail)
                 VALUES ('old', '2025-01-01T00:00:00.000000Z', 'cpu', 'Wailing', '{}')",
                [],
            )
            .unwrap();
        let rep = st
            .sweep(at("2026-09-01T12:00:00.000000Z"), Retention::default())
            .unwrap();
        assert_eq!(rep.alerts_deleted, 1);
    }
}
