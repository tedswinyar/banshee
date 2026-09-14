// collector — one cheap-tier cycle: read the kernel, stamp it, store it.
//
// Generic over `SysProbe` so the whole path is exercisable against a fake
// kernel. The clock is injected for the same reason: a sampler that reads
// `Utc::now()` internally cannot be placed on a timeline, and every retention
// and rate test needs exactly that.

use chrono::{DateTime, Utc};

use crate::Result;
use crate::sample::Sample;
use crate::sys::{DATA_VOLUME, SysProbe};

/// Which volumes to sample.
///
/// Defaults to the macOS data volume alone. **Not `/`** — on APFS that is the
/// sealed system snapshot, which is how a disk-usage tool once reported "140Mi avail"
/// against a data volume holding 21Gi.
pub fn default_mount_points() -> Vec<String> {
    vec![DATA_VOLUME.to_string()]
}

pub struct Collector<P: SysProbe> {
    probe: P,
    mount_points: Vec<String>,
}

impl<P: SysProbe> Collector<P> {
    pub fn new(probe: P, mount_points: Vec<String>) -> Self {
        Self {
            probe,
            mount_points,
        }
    }

    pub fn with_defaults(probe: P) -> Self {
        Self::new(probe, default_mount_points())
    }

    /// Take one reading, stamped at `at`.
    ///
    /// The kernel read and the volume reads happen as close together as
    /// possible, but they are NOT atomic — nothing in the kernel offers that.
    /// Both are microseconds, so the skew is far below the 15s cadence and below
    /// the resolution of anything the pressure model asks.
    pub fn collect_at(&self, at: DateTime<Utc>) -> Result<Sample> {
        let reading = self.probe.read()?;
        let volumes = self.probe.volumes(&self.mount_points)?;
        Ok(Sample::from_reading(at, &reading, &volumes))
    }

    /// Take one reading, stamped now. The daemon's entry point.
    pub fn collect(&self) -> Result<Sample> {
        self.collect_at(Utc::now())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sys::{RealProbe, SysReading, VolumeReading};
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// A fake kernel. This is the ONLY mock in the sampling path — the store,
    /// the rate math and the rollups all run for real against it.
    struct FakeProbe {
        reading: SysReading,
        volumes: Vec<VolumeReading>,
        reads: AtomicUsize,
    }

    impl FakeProbe {
        fn new() -> Self {
            Self {
                reading: SysReading {
                    ncpu: 8,
                    mem_total_bytes: 25_769_803_776,
                    page_size: 16384,
                    boot_time_secs: 1_788_122_573,
                    load_1m: 4.2,
                    load_5m: 10.45,
                    load_15m: 11.0,
                    swap_total_bytes: 8_589_934_592,
                    swap_used_bytes: 7_078_019_072,
                    pages_free: 13_267,
                    pages_active: 324_446,
                    pages_inactive: 319_599,
                    pages_wired: 191_846,
                    pages_speculative: 4_289,
                    pages_compressor: 685_141,
                    pages_purgeable: 0,
                    pages_external: 0,
                    swapins: 1_284_918,
                    swapouts: 1_827_027,
                    memory_pressure_level: 1,
                    thermal_pressure_level: Some(0),
                    cpu_speed_limit_percent: None,
                    cpu_ticks_total: Some(80_000_000),
                    cpu_ticks_idle: Some(60_000_000),
                    kernel_free_percent: Some(50),
                    jetsam_kills: Some(0),
                },
                volumes: vec![VolumeReading {
                    mount_point: DATA_VOLUME.into(),
                    total_bytes: 494_384_795_648,
                    avail_bytes: 71_234_567_890,
                }],
                reads: AtomicUsize::new(0),
            }
        }
    }

    impl SysProbe for FakeProbe {
        fn read(&self) -> Result<SysReading> {
            self.reads.fetch_add(1, Ordering::SeqCst);
            Ok(self.reading)
        }
        fn volumes(&self, mount_points: &[String]) -> Result<Vec<VolumeReading>> {
            Ok(self
                .volumes
                .iter()
                .filter(|v| mount_points.contains(&v.mount_point))
                .cloned()
                .collect())
        }
    }

    struct FailingProbe;
    impl SysProbe for FailingProbe {
        fn read(&self) -> Result<SysReading> {
            Err(crate::CoreError::Schema("kernel read failed".into()))
        }
        fn volumes(&self, _: &[String]) -> Result<Vec<VolumeReading>> {
            Ok(Vec::new())
        }
    }

    fn at(s: &str) -> DateTime<Utc> {
        crate::wire_time::from_wire(s).unwrap()
    }

    #[test]
    fn collects_a_sample_carrying_every_kernel_field() {
        let c = Collector::with_defaults(FakeProbe::new());
        let s = c.collect_at(at("2026-08-31T15:00:00.000000Z")).unwrap();

        assert_eq!(s.sampled_at, at("2026-08-31T15:00:00.000000Z"));
        assert_eq!(s.ncpu, 8);
        assert_eq!(s.load1m, 4.2);
        assert_eq!(s.swap_used_bytes, 7_078_019_072);
        assert_eq!(s.pages_compressor, 685_141);
        assert_eq!(s.swapins, 1_284_918);
        assert_eq!(s.boot_time_secs, 1_788_122_573);
        assert_eq!(s.volumes.len(), 1);
        assert_eq!(s.volumes[0].avail_bytes, 71_234_567_890);
    }

    /// The injected timestamp is used verbatim. Mutation-proof: replace `at`
    /// with `Utc::now()` inside `collect_at` and this fails — and every
    /// retention test built on a fixed timeline would become unpinnable.
    #[test]
    fn the_injected_timestamp_is_used_verbatim() {
        let c = Collector::with_defaults(FakeProbe::new());
        for ts in [
            "2020-01-01T00:00:00.000000Z",
            "2026-08-31T15:00:00.000000Z",
            "2030-12-31T23:59:59.999999Z",
        ] {
            assert_eq!(c.collect_at(at(ts)).unwrap().sampled_at, at(ts));
        }
    }

    /// Only the configured mounts are sampled. Banshee reads volumes and nothing
    /// else about the filesystem (ADR-0003).
    #[test]
    fn samples_only_the_configured_mount_points() {
        let c = Collector::new(FakeProbe::new(), vec!["/not/mounted".into()]);
        let s = c.collect_at(at("2026-08-31T15:00:00.000000Z")).unwrap();
        assert!(
            s.volumes.is_empty(),
            "an unconfigured volume must not appear"
        );
    }

    #[test]
    fn each_sample_gets_a_distinct_id() {
        let c = Collector::with_defaults(FakeProbe::new());
        let a = c.collect_at(at("2026-08-31T15:00:00.000000Z")).unwrap();
        let b = c.collect_at(at("2026-08-31T15:00:15.000000Z")).unwrap();
        assert_ne!(a.id, b.id);
    }

    /// A failing kernel read must surface as an error, not as a zeroed sample.
    /// A sample full of zeros reads as "load 0, no swap, plenty free" — the
    /// most dangerous possible lie for this app to tell.
    #[test]
    fn a_failed_kernel_read_is_an_error_not_a_zeroed_sample() {
        let c = Collector::with_defaults(FailingProbe);
        assert!(c.collect_at(at("2026-08-31T15:00:00.000000Z")).is_err());
    }

    #[test]
    fn one_cycle_reads_the_kernel_exactly_once() {
        let c = Collector::with_defaults(FakeProbe::new());
        c.collect_at(at("2026-08-31T15:00:00.000000Z")).unwrap();
        assert_eq!(c.probe.reads.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn the_default_mount_point_is_the_data_volume_not_root() {
        let mounts = default_mount_points();
        assert_eq!(mounts, vec![DATA_VOLUME.to_string()]);
        assert_ne!(mounts[0], "/", "on APFS / is the sealed system snapshot");
    }

    /// End to end against the real kernel: a live cycle must produce a sample
    /// whose fields are plausible. This is the closest thing to "does the
    /// daemon actually work" that a unit test can be.
    #[test]
    fn a_real_cycle_produces_a_plausible_sample() {
        let c = Collector::with_defaults(RealProbe);
        let s = c.collect().unwrap();
        assert!(s.ncpu >= 1);
        assert!(s.page_size >= 4096);
        assert!(s.load1m >= 0.0 && s.load1m < 1000.0);
        assert!(s.mem_total_bytes > 1 << 30);
        assert_eq!(s.volumes.len(), 1, "the data volume must be readable");
        assert!(s.volumes[0].total_bytes > 0);
    }
}
