// sample — a stored reading, and the 5-minute rollup it collapses into.
//
// These are storage AND wire types: camelCase keys, present-as-null, canonical
// datetimes (docs/wire-format.md). Rates and ratios serialize at full f64
// precision rather than pre-rounded for display — a pre-rounded rate cannot be
// re-aggregated into a rollup.

use chrono::{DateTime, DurationRound, TimeDelta, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::sys::{SysReading, VolumeReading};
use crate::{CoreError, Result};

/// The rollup bucket width. Raw samples are kept 24h, rollups 30d (ADR-0006).
pub const ROLLUP_BUCKET_SECS: i64 = 300;

/// One cheap-tier reading, persisted.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Sample {
    pub id: Uuid,
    #[serde(with = "crate::wire_time")]
    pub sampled_at: DateTime<Utc>,
    /// Carried per-sample, not as global config, because it is the reboot
    /// discriminator: any two samples with different values have no valid
    /// counter delta between them (see `rates::counter_rates`).
    pub boot_time_secs: i64,
    pub ncpu: u32,
    pub mem_total_bytes: u64,
    pub page_size: u64,
    pub load1m: f64,
    pub load5m: f64,
    pub load15m: f64,
    pub swap_total_bytes: u64,
    pub swap_used_bytes: u64,
    pub pages_free: u64,
    pub pages_active: u64,
    pub pages_inactive: u64,
    pub pages_wired: u64,
    pub pages_speculative: u64,
    pub pages_compressor: u64,
    /// App-marked discardable pages. Counts toward availability.
    pub pages_purgeable: u64,
    /// File-backed pages (the page cache). Counts toward availability — banding
    /// on `pages_free` alone was the `banshee-cot` false-positive bug.
    pub pages_external: u64,
    pub swapins: u64,
    pub swapouts: u64,
    /// The kernel's own memory verdict (`kern.memorystatus_vm_pressure_level`):
    /// 1 normal, 2 warn, 4 critical. **0 means "not recorded"** — a pre-v8 row —
    /// and the pressure model skips it rather than reading it as normal.
    pub memory_pressure_level: u32,
    /// The kernel's thermal pressure level (0 nominal … 4 sleeping), schema v11.
    /// **Nullable, and null means "not recorded"** (a pre-v11 row) — unlike
    /// `memory_pressure_level`, 0 here is a real reading (nominal), so the
    /// absent-marker has to be NULL rather than a sentinel value. Present-as-null
    /// on the wire; `#[serde(default)]` so a pre-v11 daemon's JSON still decodes.
    #[serde(default)]
    pub thermal_pressure_level: Option<u32>,
    /// `CPU_Speed_Limit` percent from IOKit — Intel Macs only; null on Apple
    /// silicon where the platform does not publish one (and on pre-v11 rows).
    #[serde(default)]
    pub cpu_speed_limit_percent: Option<u32>,
    /// Cumulative CPU tick counters summed across all cores (schema v12): `total`
    /// is user+system+idle+nice, `idle` the idle state alone. The CPU dimension
    /// bands on the utilization RATE derived from these — `Δ(total − idle)/Δtotal`
    /// between consecutive samples (`banshee-87l.19`) — never on load-per-core,
    /// which cannot tell a saturated machine from an idle one under a churning
    /// run queue. **Nullable, null = "not recorded"** (a pre-v12 row), present-as-
    /// null on the wire; `#[serde(default)]` so a pre-v12 daemon's JSON decodes.
    /// The two move together: both Some or both None.
    #[serde(default)]
    pub cpu_ticks_total: Option<u64>,
    #[serde(default)]
    pub cpu_ticks_idle: Option<u64>,
    /// `kern.memorystatus_level` — the kernel's reclaimability percentage (schema
    /// v13), distinct from `memory_pressure_level`. Drives the advisory KernelFree
    /// dimension. **Nullable, null = "not recorded"** (a pre-v13 row), present-as-
    /// null on the wire; `#[serde(default)]` so a pre-v13 daemon's JSON decodes.
    #[serde(default)]
    pub kernel_free_percent: Option<u32>,
    /// `kern.memorystatus.kill_on_sustained_pressure_count` — lifetime jetsam-kill
    /// counter (schema v13). A positive delta between samples is a kill, which the
    /// Jetsam dimension treats as Shrieking-grade. **Nullable, null = "not
    /// recorded"** (a pre-v13 row), present-as-null on the wire.
    #[serde(default)]
    pub jetsam_kills: Option<u64>,
    pub volumes: Vec<VolumeSample>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VolumeSample {
    pub mount_point: String,
    pub total_bytes: u64,
    pub avail_bytes: u64,
}

impl From<&VolumeReading> for VolumeSample {
    fn from(v: &VolumeReading) -> Self {
        Self {
            mount_point: v.mount_point.clone(),
            total_bytes: v.total_bytes,
            avail_bytes: v.avail_bytes,
        }
    }
}

impl Sample {
    /// Build a sample from a kernel reading at a given instant.
    ///
    /// The timestamp is passed in rather than read from the clock here so that
    /// tests can place samples on an exact timeline, and so a caller can stamp a
    /// whole cycle with one consistent instant.
    pub fn from_reading(
        at: DateTime<Utc>,
        reading: &SysReading,
        volumes: &[VolumeReading],
    ) -> Self {
        Self {
            id: Uuid::new_v4(),
            sampled_at: at,
            boot_time_secs: reading.boot_time_secs,
            ncpu: reading.ncpu,
            mem_total_bytes: reading.mem_total_bytes,
            page_size: reading.page_size,
            load1m: reading.load_1m,
            load5m: reading.load_5m,
            load15m: reading.load_15m,
            swap_total_bytes: reading.swap_total_bytes,
            swap_used_bytes: reading.swap_used_bytes,
            pages_free: reading.pages_free,
            pages_active: reading.pages_active,
            pages_inactive: reading.pages_inactive,
            pages_wired: reading.pages_wired,
            pages_speculative: reading.pages_speculative,
            pages_compressor: reading.pages_compressor,
            pages_purgeable: reading.pages_purgeable,
            pages_external: reading.pages_external,
            swapins: reading.swapins,
            swapouts: reading.swapouts,
            memory_pressure_level: reading.memory_pressure_level,
            thermal_pressure_level: reading.thermal_pressure_level,
            cpu_speed_limit_percent: reading.cpu_speed_limit_percent,
            cpu_ticks_total: reading.cpu_ticks_total,
            cpu_ticks_idle: reading.cpu_ticks_idle,
            kernel_free_percent: reading.kernel_free_percent,
            jetsam_kills: reading.jetsam_kills,
            volumes: volumes.iter().map(VolumeSample::from).collect(),
        }
    }

    /// Recover the kernel reading, so the rate math can consume stored samples
    /// exactly as it consumes live ones. Without this, `rates` would need a
    /// second code path for history — and the two would drift.
    pub fn to_reading(&self) -> SysReading {
        SysReading {
            ncpu: self.ncpu,
            mem_total_bytes: self.mem_total_bytes,
            page_size: self.page_size,
            boot_time_secs: self.boot_time_secs,
            load_1m: self.load1m,
            load_5m: self.load5m,
            load_15m: self.load15m,
            swap_total_bytes: self.swap_total_bytes,
            swap_used_bytes: self.swap_used_bytes,
            pages_free: self.pages_free,
            pages_active: self.pages_active,
            pages_inactive: self.pages_inactive,
            pages_wired: self.pages_wired,
            pages_speculative: self.pages_speculative,
            pages_compressor: self.pages_compressor,
            pages_purgeable: self.pages_purgeable,
            pages_external: self.pages_external,
            swapins: self.swapins,
            swapouts: self.swapouts,
            memory_pressure_level: self.memory_pressure_level,
            thermal_pressure_level: self.thermal_pressure_level,
            cpu_speed_limit_percent: self.cpu_speed_limit_percent,
            cpu_ticks_total: self.cpu_ticks_total,
            cpu_ticks_idle: self.cpu_ticks_idle,
            kernel_free_percent: self.kernel_free_percent,
            jetsam_kills: self.jetsam_kills,
        }
    }

    /// The 5-minute bucket this sample belongs to, floored.
    ///
    /// Computed from the canonical UTC instant. Flooring a local-time rendering
    /// would put bucket boundaries in the wrong place for any non-UTC zone and
    /// produce a duplicated or missing bucket at every DST transition.
    pub fn bucket_start(&self) -> Result<DateTime<Utc>> {
        bucket_start_of(self.sampled_at)
    }
}

/// Floor an instant to its rollup bucket.
pub fn bucket_start_of(at: DateTime<Utc>) -> Result<DateTime<Utc>> {
    let width = TimeDelta::try_seconds(ROLLUP_BUCKET_SECS)
        .ok_or_else(|| CoreError::InvalidInput("rollup bucket width is out of range".into()))?;
    at.duration_trunc(width)
        .map_err(|e| CoreError::InvalidInput(format!("cannot floor {at} to a bucket: {e}")))
}

/// A 5-minute aggregate. What survives past 24 hours.
///
/// The choice of aggregate per field is deliberate: **averages hide the spike
/// that matters.** Load and swap carry a max, free memory carries a min, and the
/// monotonic counters carry a delta (a sum of increments), because an average of
/// a lifetime counter is meaningless.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Rollup {
    #[serde(with = "crate::wire_time")]
    pub bucket_start: DateTime<Utc>,
    pub sample_count: u32,
    pub load1m_avg: f64,
    pub load1m_max: f64,
    pub swap_used_avg: u64,
    pub swap_used_max: u64,
    pub pages_free_min: u64,
    pub pages_compressor_max: u64,
    /// Increase across the bucket. `None` when it could not be computed —
    /// a reboot inside the bucket, or a single sample with nothing to diff.
    pub swapins_delta: Option<u64>,
    pub swapouts_delta: Option<u64>,
    // The census SCALARS (`banshee-yj1`). The census itself is a list of named
    // things and is deliberately never rolled up (schema v4); these are the
    // numbers the pressure model derives from it, which average and max
    // honestly — so month-scale sprawl trends live in the tier that already
    // has 30-day retention. **`None` means no census fell in this bucket
    // (or the rollup pre-dates schema v9) — never zero.**
    pub stale_sessions_max: Option<u32>,
    pub orphans_max: Option<u32>,
    /// `MonitorTotal::percent_of_one_core`, the DEDUPLICATED figure. Raw: the
    /// live model's `corporate_min_life_secs` guard is interpretation and is
    /// not applied here, so a bucket right after a reboot can carry a noisy
    /// ratio from young processes.
    pub monitor_percent_max: Option<f64>,
    pub total_procs_max: Option<u32>,
    pub volumes: Vec<VolumeRollup>,
}

/// The scalar counters one census contributes to its rollup bucket.
///
/// A projection, not a summary the caller invents: `From<&Census>` is the ONE
/// place these are derived, so the trend and the live pressure model cannot
/// disagree about what "stale session count" means.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CensusScalars {
    pub taken_at: DateTime<Utc>,
    pub total_procs: u32,
    pub stale_sessions: u32,
    pub orphans: u32,
    pub monitor_percent: f64,
}

impl From<&crate::census::Census> for CensusScalars {
    fn from(c: &crate::census::Census) -> Self {
        Self {
            taken_at: c.taken_at,
            total_procs: c.total_procs,
            stale_sessions: c.stale_sessions().count() as u32,
            orphans: c.orphans.orphan_count,
            monitor_percent: c.monitor_total.percent_of_one_core,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VolumeRollup {
    pub mount_point: String,
    pub total_bytes: u64,
    pub avail_min: u64,
    pub avail_avg: u64,
}

/// Collapse the samples of one bucket into a rollup, folding in the scalar
/// counters of any censuses taken inside it.
///
/// Returns `None` for an empty sample slice — including a bucket that has a
/// census but no samples, which therefore loses its census scalars. That is
/// the pre-existing shape (the census was deleted unrecorded before v9), and a
/// census-only bucket cannot honestly fill the NOT NULL sample aggregates.
///
/// Samples need not be sorted; the counter delta uses min/max rather than
/// first/last so an out-of-order batch cannot produce a negative increase.
pub fn rollup(
    bucket_start: DateTime<Utc>,
    samples: &[Sample],
    censuses: &[CensusScalars],
) -> Option<Rollup> {
    if samples.is_empty() {
        return None;
    }
    let n = samples.len();
    let nf = n as f64;

    let load_sum: f64 = samples.iter().map(|s| s.load1m).sum();
    let load_max = samples.iter().map(|s| s.load1m).fold(f64::MIN, f64::max);
    let swap_sum: u128 = samples.iter().map(|s| u128::from(s.swap_used_bytes)).sum();
    let swap_max = samples.iter().map(|s| s.swap_used_bytes).max()?;
    let free_min = samples.iter().map(|s| s.pages_free).min()?;
    let comp_max = samples.iter().map(|s| s.pages_compressor).max()?;

    // A reboot inside the bucket makes the counter delta meaningless: the
    // counters restarted, so max - min spans two different lifetimes.
    let rebooted_within = samples
        .iter()
        .any(|s| s.boot_time_secs != samples[0].boot_time_secs);
    let delta = |pick: fn(&Sample) -> u64| -> Option<u64> {
        if rebooted_within || n < 2 {
            return None;
        }
        let hi = samples.iter().map(pick).max()?;
        let lo = samples.iter().map(pick).min()?;
        Some(hi - lo)
    };

    // Volumes are keyed by mount point; a volume that appeared or vanished
    // mid-bucket simply has fewer samples behind its aggregate.
    let mut mounts: Vec<&str> = samples
        .iter()
        .flat_map(|s| s.volumes.iter().map(|v| v.mount_point.as_str()))
        .collect();
    mounts.sort_unstable();
    mounts.dedup();

    let volumes = mounts
        .into_iter()
        .filter_map(|mp| {
            let obs: Vec<&VolumeSample> = samples
                .iter()
                .flat_map(|s| s.volumes.iter())
                .filter(|v| v.mount_point == mp)
                .collect();
            let cnt = obs.len() as u128;
            if cnt == 0 {
                return None;
            }
            let sum: u128 = obs.iter().map(|v| u128::from(v.avail_bytes)).sum();
            Some(VolumeRollup {
                mount_point: mp.to_string(),
                // Last observed total wins: a volume can legitimately be resized.
                total_bytes: obs.last()?.total_bytes,
                avail_min: obs.iter().map(|v| v.avail_bytes).min()?,
                avail_avg: (sum / cnt) as u64,
            })
        })
        .collect();

    // Census scalars: the MAX, consistent with "averages hide the spike that
    // matters" — and at the 5-minute cadence a bucket usually holds exactly one
    // census, where max IS the value.
    let stale_max = censuses.iter().map(|c| c.stale_sessions).max();
    let orphans_max = censuses.iter().map(|c| c.orphans).max();
    let monitor_max = censuses
        .iter()
        .map(|c| c.monitor_percent)
        .fold(None, |acc: Option<f64>, v| {
            Some(acc.map_or(v, |a| a.max(v)))
        });
    let procs_max = censuses.iter().map(|c| c.total_procs).max();

    Some(Rollup {
        bucket_start,
        sample_count: u32::try_from(n).unwrap_or(u32::MAX),
        load1m_avg: load_sum / nf,
        load1m_max: load_max,
        swap_used_avg: (swap_sum / n as u128) as u64,
        swap_used_max: swap_max,
        pages_free_min: free_min,
        pages_compressor_max: comp_max,
        swapins_delta: delta(|s| s.swapins),
        swapouts_delta: delta(|s| s.swapouts),
        stale_sessions_max: stale_max,
        orphans_max,
        monitor_percent_max: monitor_max,
        total_procs_max: procs_max,
        volumes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sys::DATA_VOLUME;

    fn at(s: &str) -> DateTime<Utc> {
        crate::wire_time::from_wire(s).unwrap()
    }

    fn sample(ts: &str, load: f64, swap_used: u64, free: u64, swapins: u64) -> Sample {
        Sample {
            id: Uuid::new_v4(),
            sampled_at: at(ts),
            boot_time_secs: 1_788_122_573,
            ncpu: 8,
            mem_total_bytes: 25_769_803_776,
            page_size: 16384,
            load1m: load,
            load5m: load,
            load15m: load,
            swap_total_bytes: 8_589_934_592,
            swap_used_bytes: swap_used,
            pages_free: free,
            pages_active: 100,
            pages_inactive: 100,
            pages_wired: 100,
            pages_speculative: 0,
            pages_compressor: 500,
            pages_purgeable: 0,
            pages_external: 0,
            swapins,
            swapouts: swapins * 2,
            // Non-default (0 is the schema DEFAULT and "not recorded"): a zero
            // fixture is indistinguishable from a dropped field.
            memory_pressure_level: 2,
            // Some(...), never None, for the same reason: None is the column's
            // NULL default and a dropped field would hide behind it.
            thermal_pressure_level: Some(2),
            cpu_speed_limit_percent: Some(85),
            // Some(...), never None: None is the column's NULL default, so a
            // dropped field would hide behind it (same reason as the thermal
            // fields above). Distinct from each other so a total/idle swap shows.
            cpu_ticks_total: Some(80_000_000),
            cpu_ticks_idle: Some(60_000_000),
            kernel_free_percent: Some(37),
            jetsam_kills: Some(2),
            volumes: vec![VolumeSample {
                mount_point: DATA_VOLUME.into(),
                total_bytes: 494_384_795_648,
                avail_bytes: 71_000_000_000,
            }],
        }
    }

    #[test]
    fn round_trips_through_a_kernel_reading() {
        let s = sample(
            "2026-08-31T15:00:00.000000Z",
            4.2,
            7_078_019_072,
            13_267,
            1000,
        );
        let r = s.to_reading();
        let back = Sample::from_reading(s.sampled_at, &r, &[]);
        // Everything but the fresh id and the dropped volume list must match.
        assert_eq!(back.load1m, s.load1m);
        assert_eq!(back.swap_used_bytes, s.swap_used_bytes);
        assert_eq!(back.swapins, s.swapins);
        assert_eq!(back.boot_time_secs, s.boot_time_secs);
        assert_eq!(back.page_size, s.page_size);
        // Non-default in the helper, so a from_reading/to_reading arm that
        // dropped the field could not hide behind a zero.
        assert_eq!(back.memory_pressure_level, 2);
        assert_eq!(back.thermal_pressure_level, Some(2));
        assert_eq!(back.cpu_speed_limit_percent, Some(85));
        assert_eq!(back.cpu_ticks_total, Some(80_000_000));
        assert_eq!(back.cpu_ticks_idle, Some(60_000_000));
        assert_eq!(back.kernel_free_percent, Some(37));
        assert_eq!(back.jetsam_kills, Some(2));
    }

    /// The nullable v11/v12/v13 fields are present-as-null (wire contract): a
    /// pre-migration row serialises with the keys PRESENT and null, never absent,
    /// and an old daemon's JSON (keys absent) still decodes to None. Mutation-proof:
    /// `skip_serializing_if` on any field fails the first half; dropping
    /// `#[serde(default)]` fails the second.
    #[test]
    fn thermal_fields_are_present_as_null_and_decode_when_absent() {
        let mut s = sample("2026-08-31T15:00:00.000000Z", 1.0, 0, 400_000, 1000);
        s.thermal_pressure_level = None;
        s.cpu_speed_limit_percent = None;
        s.cpu_ticks_total = None;
        s.cpu_ticks_idle = None;
        s.kernel_free_percent = None;
        s.jetsam_kills = None;
        let v: serde_json::Value = serde_json::to_value(&s).unwrap();
        assert!(v.get("thermalPressureLevel").is_some_and(|x| x.is_null()));
        assert!(v.get("cpuSpeedLimitPercent").is_some_and(|x| x.is_null()));
        assert!(v.get("cpuTicksTotal").is_some_and(|x| x.is_null()));
        assert!(v.get("cpuTicksIdle").is_some_and(|x| x.is_null()));
        assert!(v.get("kernelFreePercent").is_some_and(|x| x.is_null()));
        assert!(v.get("jetsamKills").is_some_and(|x| x.is_null()));

        let mut old = v.clone();
        old.as_object_mut().unwrap().remove("thermalPressureLevel");
        old.as_object_mut().unwrap().remove("cpuSpeedLimitPercent");
        old.as_object_mut().unwrap().remove("cpuTicksTotal");
        old.as_object_mut().unwrap().remove("cpuTicksIdle");
        old.as_object_mut().unwrap().remove("kernelFreePercent");
        old.as_object_mut().unwrap().remove("jetsamKills");
        let back: Sample = serde_json::from_value(old).unwrap();
        assert_eq!(back.thermal_pressure_level, None);
        assert_eq!(back.cpu_speed_limit_percent, None);
        assert_eq!(back.cpu_ticks_total, None);
        assert_eq!(back.cpu_ticks_idle, None);
        assert_eq!(back.kernel_free_percent, None);
        assert_eq!(back.jetsam_kills, None);
    }

    /// Buckets floor to 5-minute boundaries computed from the UTC instant.
    #[test]
    fn buckets_floor_to_five_minute_boundaries() {
        for (input, want) in [
            ("2026-08-31T15:00:00.000000Z", "2026-08-31T15:00:00.000000Z"),
            ("2026-08-31T15:04:59.999999Z", "2026-08-31T15:00:00.000000Z"),
            ("2026-08-31T15:05:00.000000Z", "2026-08-31T15:05:00.000000Z"),
            ("2026-08-31T15:59:59.000000Z", "2026-08-31T15:55:00.000000Z"),
            ("2026-08-31T00:00:01.000000Z", "2026-08-31T00:00:00.000000Z"),
        ] {
            let got = bucket_start_of(at(input)).unwrap();
            assert_eq!(crate::wire_time::to_wire(&got), want, "bucket for {input}");
        }
    }

    /// A bucket boundary must not depend on the machine's time zone. Flooring a
    /// local rendering breaks for any offset that is not a whole multiple of the
    /// bucket, and duplicates or drops a bucket at DST transitions.
    #[test]
    fn bucket_boundaries_are_utc_not_local() {
        // Same instant, expressed with a +05:45 offset (Nepal — a 45-minute
        // offset, which is exactly the case a local-time floor gets wrong).
        let utc = at("2026-08-31T15:05:00.000000Z");
        let offset = at("2026-08-31T20:50:00.000000+05:45");
        assert_eq!(utc, offset, "same instant");
        assert_eq!(
            bucket_start_of(utc).unwrap(),
            bucket_start_of(offset).unwrap()
        );
        assert_eq!(
            crate::wire_time::to_wire(&bucket_start_of(offset).unwrap()),
            "2026-08-31T15:05:00.000000Z"
        );
    }

    #[test]
    fn empty_bucket_produces_no_rollup() {
        assert!(rollup(at("2026-08-31T15:00:00.000000Z"), &[], &[]).is_none());
    }

    fn scalars(ts: &str, procs: u32, stale: u32, orphans: u32, monitor: f64) -> CensusScalars {
        CensusScalars {
            taken_at: at(ts),
            total_procs: procs,
            stale_sessions: stale,
            orphans,
            monitor_percent: monitor,
        }
    }

    /// The census scalars carry the MAX across the bucket, same doctrine as
    /// load — averages hide the spike. The two censuses DISAGREE on every
    /// field, and neither dominates the other (the first wins on orphans and
    /// procs, the second on stale and monitor), so "keep the first", "keep the
    /// last" and any per-census copy all fail against at least one assertion.
    #[test]
    fn census_scalars_keep_the_max_of_each_counter_independently() {
        let b = at("2026-08-31T15:00:00.000000Z");
        let samples = vec![sample("2026-08-31T15:00:00.000000Z", 1.0, 1, 1, 1)];
        let censuses = vec![
            scalars("2026-08-31T15:00:10.000000Z", 900, 2, 45, 39.0),
            scalars("2026-08-31T15:04:50.000000Z", 850, 17, 12, 64.1),
        ];
        let r = rollup(b, &samples, &censuses).unwrap();

        assert_eq!(r.stale_sessions_max, Some(17), "second census wins");
        assert_eq!(r.orphans_max, Some(45), "first census wins");
        assert_eq!(r.monitor_percent_max, Some(64.1));
        assert_eq!(r.total_procs_max, Some(900));
    }

    /// A bucket with no census carries None for every scalar — NOT zero. Zero
    /// orphans is a claim about the machine; None is "no census looked". A
    /// client (or a trend query) treating them alike invents a clean machine,
    /// exactly the voided-delta rule.
    #[test]
    fn a_bucket_without_a_census_has_null_scalars_not_zero() {
        let b = at("2026-08-31T15:00:00.000000Z");
        let r = rollup(
            b,
            &[sample("2026-08-31T15:00:00.000000Z", 1.0, 1, 1, 1)],
            &[],
        )
        .unwrap();
        assert_eq!(r.stale_sessions_max, None);
        assert_eq!(r.orphans_max, None);
        assert_eq!(r.monitor_percent_max, None);
        assert_eq!(r.total_procs_max, None);
    }

    /// `CensusScalars::from(&Census)` counts STALE sessions, not all sessions —
    /// the census below has three sessions and only one stale, so the two
    /// derivations give different numbers and a `agent_sessions.len()` slip is
    /// caught. Also pins which field feeds which scalar (orphan_count, not
    /// total_count; the deduplicated monitor percent).
    #[test]
    fn census_scalars_project_the_pressure_models_numbers() {
        use crate::census::{Census, HelperRollup, MonitorTotal, OrphanCensus};
        let mk_session = |stale: bool| crate::census::AgentSession {
            pid: 1,
            program: "claude".into(),
            rss_bytes: 1,
            age_secs: 1,
            age_days: if stale { 8 } else { 0 },
            cpu_secs: 1.0,
            tty: "ttys000".into(),
            is_stale: stale,
            cwd: None,
        };
        let c = Census {
            id: Uuid::new_v4(),
            taken_at: at("2026-08-31T15:00:10.000000Z"),
            total_procs: 812,
            agent_sessions: vec![mk_session(false), mk_session(true), mk_session(false)],
            ide_helpers: HelperRollup {
                count: 2,
                rss_bytes: 0,
            },
            orphans: OrphanCensus {
                total_count: 150,
                total_rss_bytes: 1,
                orphan_count: 14,
                orphan_rss_bytes: 1,
                orphans_by_program: Vec::new(),
            },
            tmux_sessions: Vec::new(),
            app_groups: Vec::new(),
            monitor_agents: Vec::new(),
            monitor_total: MonitorTotal {
                proc_count: 13,
                rss_bytes: 1,
                cpu_secs_total: 1.0,
                percent_of_one_core: 49.2,
                longest_life_secs: 78_000,
            },
            tmux_available: true,
        };
        let s = CensusScalars::from(&c);
        assert_eq!(s.stale_sessions, 1, "stale sessions, not all sessions");
        assert_eq!(s.orphans, 14, "orphan_count, not total_count");
        assert_eq!(s.total_procs, 812);
        assert_eq!(s.monitor_percent, 49.2);
        assert_eq!(s.taken_at, c.taken_at);
    }

    /// Averages hide spikes, so load and swap keep a max and free memory keeps a
    /// min. Mutation-proof: make `load1m_max` the average and this fails — 12.0
    /// is the number that would trip a band, and the average of the three is 5.0.
    #[test]
    fn keeps_the_extreme_that_matters_not_just_the_average() {
        let b = at("2026-08-31T15:00:00.000000Z");
        let samples = vec![
            sample("2026-08-31T15:00:00.000000Z", 1.0, 100, 9_000, 10),
            sample("2026-08-31T15:01:00.000000Z", 12.0, 900, 500, 20),
            sample("2026-08-31T15:02:00.000000Z", 2.0, 500, 4_000, 30),
        ];
        let r = rollup(b, &samples, &[]).unwrap();

        assert_eq!(r.sample_count, 3);
        assert!((r.load1m_avg - 5.0).abs() < 1e-9);
        assert_eq!(r.load1m_max, 12.0, "the spike must survive the rollup");
        assert_eq!(r.swap_used_max, 900);
        assert_eq!(r.swap_used_avg, 500);
        assert_eq!(r.pages_free_min, 500, "the low-water mark must survive");
        assert_eq!(r.swapins_delta, Some(20)); // 30 - 10
        assert_eq!(r.swapouts_delta, Some(40)); // 60 - 20
    }

    /// A reboot inside the bucket makes max-minus-min span two lifetimes.
    /// Mutation-proof: drop the `rebooted_within` guard and this reports a
    /// delta of 995 from counters that actually restarted.
    #[test]
    fn a_reboot_inside_the_bucket_voids_the_counter_delta() {
        let b = at("2026-08-31T15:00:00.000000Z");
        let mut a1 = sample("2026-08-31T15:00:00.000000Z", 1.0, 100, 900, 1000);
        let mut a2 = sample("2026-08-31T15:01:00.000000Z", 1.0, 100, 900, 5);
        a1.boot_time_secs = 1_000;
        a2.boot_time_secs = 2_000; // rebooted between the two
        let r = rollup(b, &[a1, a2], &[]).unwrap();

        assert_eq!(r.swapins_delta, None, "counters restarted; no honest delta");
        assert_eq!(r.swapouts_delta, None);
        // The non-counter aggregates are still perfectly valid.
        assert_eq!(r.load1m_max, 1.0);
        assert_eq!(r.pages_free_min, 900);
    }

    /// One sample has nothing to diff against. Reporting 0 would read as
    /// "no swap activity", which is a claim we cannot make from one reading.
    #[test]
    fn a_single_sample_bucket_has_no_counter_delta() {
        let b = at("2026-08-31T15:00:00.000000Z");
        let r = rollup(
            b,
            &[sample("2026-08-31T15:00:00.000000Z", 3.0, 7, 8, 42)],
            &[],
        )
        .unwrap();
        assert_eq!(r.sample_count, 1);
        assert_eq!(r.swapins_delta, None);
        assert_eq!(r.load1m_max, 3.0);
    }

    /// Out-of-order samples must not yield a negative (wrapping) delta.
    /// Mutation-proof: use first/last instead of min/max and this underflows.
    #[test]
    fn out_of_order_samples_do_not_produce_a_wrapped_delta() {
        let b = at("2026-08-31T15:00:00.000000Z");
        let samples = vec![
            sample("2026-08-31T15:02:00.000000Z", 1.0, 1, 1, 300), // later first
            sample("2026-08-31T15:00:00.000000Z", 1.0, 1, 1, 100),
        ];
        let r = rollup(b, &samples, &[]).unwrap();
        assert_eq!(r.swapins_delta, Some(200));
    }

    #[test]
    fn aggregates_volumes_by_mount_point() {
        let b = at("2026-08-31T15:00:00.000000Z");
        let mut a1 = sample("2026-08-31T15:00:00.000000Z", 1.0, 1, 1, 10);
        let mut a2 = sample("2026-08-31T15:01:00.000000Z", 1.0, 1, 1, 20);
        a1.volumes[0].avail_bytes = 100;
        a2.volumes[0].avail_bytes = 50;
        a2.volumes.push(VolumeSample {
            mount_point: "/Volumes/Backup".into(),
            total_bytes: 2_000,
            avail_bytes: 700,
        });

        let r = rollup(b, &[a1, a2], &[]).unwrap();
        assert_eq!(r.volumes.len(), 2);
        let data = r
            .volumes
            .iter()
            .find(|v| v.mount_point == DATA_VOLUME)
            .unwrap();
        assert_eq!(data.avail_min, 50, "low-water mark");
        assert_eq!(data.avail_avg, 75);
        // A volume seen in only one sample still gets an aggregate.
        let backup = r
            .volumes
            .iter()
            .find(|v| v.mount_point == "/Volumes/Backup")
            .unwrap();
        assert_eq!(backup.avail_min, 700);
    }

    /// Summing u64 byte counts across a bucket must not overflow. A u64 sum of
    /// 300 samples of near-u64::MAX wraps; the u128 accumulator does not.
    /// Mutation-proof: change the accumulator to u64 and this panics in debug.
    #[test]
    fn byte_averages_do_not_overflow_on_large_values() {
        let b = at("2026-08-31T15:00:00.000000Z");
        let samples: Vec<Sample> = (0..4)
            .map(|i| {
                let mut s = sample("2026-08-31T15:00:00.000000Z", 1.0, u64::MAX, 1, i);
                s.volumes[0].avail_bytes = u64::MAX;
                s
            })
            .collect();
        let r = rollup(b, &samples, &[]).unwrap();
        assert_eq!(r.swap_used_avg, u64::MAX);
        assert_eq!(r.volumes[0].avail_avg, u64::MAX);
    }

    /// The wire contract: camelCase at every depth, and the nested volume
    /// objects are exactly where casing escapes scrutiny (Five Rules, rule 3).
    #[test]
    fn serializes_with_camel_case_keys_at_every_depth() {
        let s = sample("2026-08-31T15:00:00.000000Z", 4.2, 7, 8, 9);
        let v: serde_json::Value = serde_json::to_value(&s).unwrap();

        for key in [
            "id",
            "sampledAt",
            "bootTimeSecs",
            "memTotalBytes",
            "pageSize",
            "load1m",
            "swapUsedBytes",
            "pagesCompressor",
            "swapins",
            "memoryPressureLevel",
            "thermalPressureLevel",
            "cpuSpeedLimitPercent",
            "cpuTicksTotal",
            "cpuTicksIdle",
            "volumes",
        ] {
            assert!(v.get(key).is_some(), "missing top-level key {key}");
        }
        assert!(v.get("sampled_at").is_none(), "snake_case key leaked");

        let vol = &v["volumes"][0];
        for key in ["mountPoint", "totalBytes", "availBytes"] {
            assert!(vol.get(key).is_some(), "missing nested key {key}");
        }
        assert!(vol.get("mount_point").is_none(), "nested snake_case leaked");

        // Datetimes use the canonical 6-digit-micros Z form.
        assert_eq!(v["sampledAt"], "2026-08-31T15:00:00.000000Z");
    }

    /// Nullable fields are present-as-null, never absent — this binds the
    /// ENCODER. Mutation-proof: add `skip_serializing_if = "Option::is_none"`
    /// to the delta fields and this fails.
    #[test]
    fn absent_counter_deltas_serialize_as_explicit_null() {
        let b = at("2026-08-31T15:00:00.000000Z");
        let r = rollup(
            b,
            &[sample("2026-08-31T15:00:00.000000Z", 1.0, 1, 1, 1)],
            &[],
        )
        .unwrap();
        let v: serde_json::Value = serde_json::to_value(&r).unwrap();
        assert!(v.get("swapinsDelta").is_some(), "key must be present");
        assert!(v["swapinsDelta"].is_null(), "and explicitly null");
        assert!(v["swapoutsDelta"].is_null());
        // The census scalars follow the same rule (camelCase, present-as-null).
        for key in [
            "staleSessionsMax",
            "orphansMax",
            "monitorPercentMax",
            "totalProcsMax",
        ] {
            assert!(v.get(key).is_some(), "missing key {key}");
            assert!(v[key].is_null(), "{key} must be explicitly null");
        }
    }

    // ---- the shared fixtures (raw bytes, not our own encoder) ----------------
    //
    // The SAME bytes the Swift suite decodes (`WireFormatTests`). These exist
    // because the e2e harness pins these payloads cross-SURFACE but not
    // cross-LANGUAGE; the fixture is the cross-language pin (P6b, `banshee-bej`).

    const FIXTURE_SAMPLES: &str = include_str!("../../../tests/fixtures/samples.json");
    const FIXTURE_ROLLUPS: &str = include_str!("../../../tests/fixtures/rollups.json");

    #[test]
    fn decodes_the_samples_fixture_at_full_depth() {
        let samples: Vec<Sample> = serde_json::from_str(FIXTURE_SAMPLES).unwrap();
        assert_eq!(samples.len(), 2);

        let a = &samples[0];
        assert_eq!(a.ncpu, 8);
        assert_eq!(a.page_size, 16384);
        assert_eq!(a.swap_used_bytes, 17_616_076_800);
        assert_eq!(a.pages_free, 27_965);
        assert_eq!(a.swapins, 41_859_667);
        assert_eq!(a.volumes.len(), 2);
        assert_eq!(a.volumes[0].mount_point, "/");
        assert_eq!(a.volumes[0].avail_bytes, 61_443_809_280);

        // The availability counters (`banshee-cot`): the first sample is the
        // crisis shape — a page cache with almost nothing in it — so free and
        // available are DISTINGUISHABLE per entry.
        assert_eq!(a.pages_purgeable, 5_581);
        assert_eq!(a.pages_external, 4_000);
        // …and in that shape the kernel's own verdict agrees: critical.
        assert_eq!(a.memory_pressure_level, 4);
        // The thermal fields (schema v11): the first sample is a THROTTLED Intel
        // shape — kernel level 3 (trapping) with the PMU's speed limit at 62% —
        // so both nullable fields are non-null and distinguishable here…
        assert_eq!(a.thermal_pressure_level, Some(3));
        assert_eq!(a.cpu_speed_limit_percent, Some(62));
        // The CPU tick counters (schema v12): the first sample records both, and
        // they are distinguishable (idle < total) so a total/idle swap shows.
        assert_eq!(a.cpu_ticks_total, Some(240_000_000));
        assert_eq!(a.cpu_ticks_idle, Some(96_000_000));
        // The kernel memory signals (schema v13, `banshee-yk8`): the crisis sample
        // records both — reclaimability low (12%, below the red line) and a
        // lifetime jetsam count — and they are non-null and distinguishable here.
        assert_eq!(a.kernel_free_percent, Some(12));
        assert_eq!(a.jetsam_kills, Some(3));

        // The second sample is post-reboot: a DIFFERENT bootTimeSecs, which is
        // the discriminator that voids counter deltas across the pair.
        let b = &samples[1];
        assert_ne!(a.boot_time_secs, b.boot_time_secs);
        assert_eq!(b.boot_time_secs, 1_788_269_185);
        assert_eq!(b.swap_used_bytes, 0);
        assert_eq!(b.pages_external, 355_200);
        assert!(
            b.to_reading().available_bytes() > b.to_reading().free_bytes(),
            "a populated page cache must make available exceed free"
        );
        assert_eq!(b.memory_pressure_level, 1, "post-reboot: kernel normal");
        // …and the second is a pre-v11 row: both NULL, decoded as None, not 0.
        assert_eq!(b.thermal_pressure_level, None);
        assert_eq!(b.cpu_speed_limit_percent, None);
        // …and pre-v12 for the CPU ticks: NULL, decoded as None, not 0.
        assert_eq!(b.cpu_ticks_total, None);
        assert_eq!(b.cpu_ticks_idle, None);
        // …and pre-v13 for the kernel memory signals: NULL, decoded as None, not 0.
        assert_eq!(b.kernel_free_percent, None);
        assert_eq!(b.jetsam_kills, None);

        // The fixture's second id is UPPERCASE on purpose: decoders accept any
        // case (wire contract), and re-encoding emits lowercase.
        let v = serde_json::to_value(b).unwrap();
        assert_eq!(v["id"], "b7c8d9e0-f1a2-4b3c-9d4e-5f6a7b8c9d0e");
    }

    #[test]
    fn decodes_the_rollups_fixture_including_the_voided_deltas() {
        let rollups: Vec<Rollup> = serde_json::from_str(FIXTURE_ROLLUPS).unwrap();
        assert_eq!(rollups.len(), 2);

        // First bucket: a normal five minutes, deltas populated. Avg and max are
        // distinct values — carrying both is the whole reason a rollup exists.
        let a = &rollups[0];
        assert_eq!(a.sample_count, 20);
        assert_ne!(a.load1m_avg, a.load1m_max);
        assert_eq!(a.load1m_max, 12.41);
        assert_eq!(a.swapins_delta, Some(18_220));
        assert_eq!(a.swapouts_delta, Some(4_096));
        assert_eq!(a.volumes[0].avail_min, 61_443_809_280);
        assert_ne!(a.volumes[0].avail_min, a.volumes[0].avail_avg);

        // The census scalars (`banshee-yj1`): the first bucket saw a census —
        // the 2026-08-04 incident's numbers, which are the whole reason the
        // trend exists.
        assert_eq!(a.stale_sessions_max, Some(17));
        assert_eq!(a.orphans_max, Some(45));
        assert_eq!(a.monitor_percent_max, Some(49.2));
        assert_eq!(a.total_procs_max, Some(912));

        // Second bucket: a reboot voided the counters. None, NOT zero — a client
        // rendering this as 0 reports a calm five minutes nobody observed.
        let b = &rollups[1];
        assert_eq!(b.swapins_delta, None);
        assert_eq!(b.swapouts_delta, None);
        // …and no census fell in it: None again, not a claim of zero orphans.
        assert_eq!(b.stale_sessions_max, None);
        assert_eq!(b.orphans_max, None);
        assert_eq!(b.monitor_percent_max, None);
        assert_eq!(b.total_procs_max, None);
    }
}
