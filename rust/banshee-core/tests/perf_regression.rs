// A coarse regression detector for the attach_volumes optimization.
// `attach_volumes` (sample_store.rs) once did a linear
// `iter().find(|s| s.id.to_string() == …)` per volume row — O(N²) with a String
// allocation on every probe, measured at 626 ms for a 24-hour window (5,760
// samples) before it became a HashMap index (~19 ms after). Nothing guarded
// against reintroducing the pathological shape.
//
// This seeds a realistic window and asserts the read that runs attach_volumes
// completes well under a GENEROUS ceiling. It is a smoke check, not a benchmark:
// the architect's review explicitly rejected Criterion / percentile targets as
// over-engineering — the regression that matters here is order-of-magnitude
// (hundreds of ms vs tens), so a coarse wall-clock ceiling with ~30x headroom
// catches it without tripping on a loaded machine's jitter.
//
// It lives as a banshee-core integration test rather than in the e2e HTTP path
// deliberately: the O(N²) was purely in attach_volumes (the HTTP layer adds
// nothing to that cost), and seeding thousands of rows here — instead of into
// the shared e2e temp DB on every push — is both faithful and far less flaky.

use std::time::Instant;

use banshee_core::SampleStore;
use banshee_core::sample::Sample;
use banshee_core::sys::{SysReading, VolumeReading};
use chrono::{Duration, Utc};

// Big enough that O(N²) with a per-row String allocation is unmistakably slow
// (N² = 4,000,000 probes, hundreds of ms), while O(N) stays in the low tens of
// ms — and small enough that seeding it is a second or two.
const N: usize = 2_000;
// ~30x over the O(N) figure scaled to N, and far below any O(N²) time here.
const CEILING_MS: u128 = 500;

fn reading() -> SysReading {
    SysReading {
        ncpu: 8,
        mem_total_bytes: 25_769_803_776,
        page_size: 16384,
        boot_time_secs: 1_788_100_000,
        load_1m: 1.0,
        load_5m: 1.0,
        load_15m: 1.0,
        swap_total_bytes: 8_589_934_592,
        swap_used_bytes: 1_000_000_000,
        pages_free: 400_000,
        pages_active: 200_000,
        pages_inactive: 200_000,
        pages_wired: 100_000,
        pages_speculative: 0,
        pages_compressor: 10_000,
        pages_purgeable: 0,
        pages_external: 0,
        swapins: 1_000,
        swapouts: 2_000,
        memory_pressure_level: 1,
        thermal_pressure_level: Some(0),
        cpu_speed_limit_percent: None,
        cpu_ticks_total: Some(80_000_000),
        cpu_ticks_idle: Some(60_000_000),
        kernel_free_percent: Some(50),
        jetsam_kills: Some(0),
    }
}

#[test]
fn attaching_volumes_over_a_full_window_stays_well_under_a_generous_ceiling() {
    let dir = tempfile::tempdir().unwrap();
    let store = SampleStore::open(&dir.path().join("perf.db")).unwrap();

    // Two volumes per sample — the real shape (data volume + one more), so the
    // volume-row count that attach_volumes joins against is 2N, as in production.
    let base = Utc::now() - Duration::seconds(15 * N as i64);
    let volumes = [
        VolumeReading {
            mount_point: "/System/Volumes/Data".into(),
            total_bytes: 494_384_795_648,
            avail_bytes: 100_000_000_000,
        },
        VolumeReading {
            mount_point: "/".into(),
            total_bytes: 494_384_795_648,
            avail_bytes: 100_000_000_000,
        },
    ];
    for i in 0..N {
        let at = base + Duration::seconds(15 * i as i64);
        store
            .insert(&Sample::from_reading(at, &reading(), &volumes))
            .unwrap();
    }

    // Time ONLY the read (attach_volumes runs inside it), not the seeding.
    let start = Instant::now();
    let samples = store.recent_samples(N).unwrap();
    let elapsed = start.elapsed().as_millis();

    assert_eq!(samples.len(), N, "the whole window must come back");
    assert!(
        samples.iter().all(|s| s.volumes.len() == 2),
        "every sample's volume rows must be attached"
    );
    assert!(
        elapsed < CEILING_MS,
        "reading {N} samples with volumes took {elapsed} ms (ceiling {CEILING_MS} ms) — \
         attach_volumes may have regressed to the O(N²) per-row scan (banshee-t32)"
    );
}
