// Measure what the sample loop actually costs, so the Mutex-vs-pool decision
// (ADR-0006) is made by measurement rather than by argument.
//
//   cargo run --release --example sample_throughput
//
// Reports: per-sample insert cost, a day's worth of inserts, the retention
// sweep's cost at steady state, and the on-disk size of 24h of raw samples.

use std::time::Instant;

use banshee_core::sample::Sample;
use banshee_core::sample_store::{Retention, SampleStore};
use banshee_core::sys::{RealProbe, SysProbe};
use chrono::{Duration, Utc};

/// 15s cadence for 24h.
const SAMPLES_PER_DAY: usize = 24 * 60 * 60 / 15;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("banshee.db");
    let store = SampleStore::open(&path)?;

    // A real reading, so the row widths are honest.
    let reading = RealProbe.read()?;
    let volumes = RealProbe.volumes(&banshee_core::collector::default_mount_points())?;

    println!("cadence: one sample every 15s => {SAMPLES_PER_DAY} samples/day");
    println!("volumes per sample: {}", volumes.len());
    println!();

    // ---- Insert cost -----------------------------------------------------
    // Timestamps span a real 24 hours so the rollup buckets are realistic, and
    // sit entirely OUTSIDE the 24h raw window so the worst-case sweep below
    // actually has a full day to age out. (Placing them at `now - 24h` made the
    // first version of this example report a sweep of 0 rows, which looked like
    // a bug in the sweep and was a bug in the example.)
    let start_ts = Utc::now() - Duration::hours(72);
    let t0 = Instant::now();
    for i in 0..SAMPLES_PER_DAY {
        let at = start_ts + Duration::seconds(15 * i as i64);
        let mut s = Sample::from_reading(at, &reading, &volumes);
        // Make the monotonic counters actually advance, so rollup deltas are
        // computed rather than short-circuited.
        s.swapins += i as u64;
        s.swapouts += i as u64 * 2;
        store.insert(&s)?;
    }
    let insert_elapsed = t0.elapsed();

    let per_insert_us = insert_elapsed.as_secs_f64() * 1e6 / SAMPLES_PER_DAY as f64;
    println!("24h of inserts ({SAMPLES_PER_DAY} rows + volume rows)");
    println!("  total:      {:>10.3} s", insert_elapsed.as_secs_f64());
    println!("  per sample: {per_insert_us:>10.1} µs");
    println!(
        "  duty cycle: {:>10.5} % of a 15s tick",
        per_insert_us / 15e6 * 100.0
    );
    println!();

    // ---- Read cost -------------------------------------------------------
    let t0 = Instant::now();
    let recent = store.recent_samples(240)?; // one hour at 15s
    let read_1h = t0.elapsed();
    let t0 = Instant::now();
    let all = store.recent_samples(SAMPLES_PER_DAY)?;
    let read_24h = t0.elapsed();
    println!("reads (the UI polls these)");
    println!(
        "  last 1h  ({:>5} rows): {:>8.2} ms",
        recent.len(),
        read_1h.as_secs_f64() * 1e3
    );
    println!(
        "  last 24h ({:>5} rows): {:>8.2} ms",
        all.len(),
        read_24h.as_secs_f64() * 1e3
    );
    println!();

    // ---- Size ------------------------------------------------------------
    let raw_bytes = store.size_bytes()?;
    println!("on-disk size with 24h of raw samples");
    println!(
        "  {:.2} MB  ({:.0} bytes/sample)",
        raw_bytes as f64 / 1e6,
        raw_bytes as f64 / SAMPLES_PER_DAY as f64
    );
    println!();

    // ---- Retention sweep -------------------------------------------------
    // Everything above is now older than the 24h window, so this is the
    // worst-case sweep: roll up a full day and delete all of it.
    let t0 = Instant::now();
    let report = store.sweep(Utc::now(), Retention::default())?;
    let sweep_elapsed = t0.elapsed();
    println!("retention sweep (worst case: a full day aging out at once)");
    println!("  {:>10.3} s", sweep_elapsed.as_secs_f64());
    println!("  rollups written: {}", report.rollups_written);
    println!("  raw deleted:     {}", report.raw_samples_deleted);
    // NOTE: the file does NOT shrink here, and that is expected. SQLite marks
    // the deleted pages free and REUSES them; it does not return them to the OS
    // without an explicit VACUUM (which rewrites the whole file and takes a
    // write lock). So the on-disk size settles at the high-water mark rather
    // than tracking the row count down. That is still bounded — which is what
    // ADR-0006 promises — but "bounded row count" and "shrinking file" are
    // different claims, and only the first one is true.
    println!(
        "  size after:      {:.2} MB  (unchanged: freed pages are reused, not returned)",
        store.size_bytes()? as f64 / 1e6
    );

    // Project the rollup tier honestly, from a store that only ever held
    // rollups, rather than by scaling a file full of reusable free pages.
    let rollup_only = SampleStore::open(&dir.path().join("rollups.db"))?;
    let empty = rollup_only.size_bytes()?;
    let day_of_buckets = 24 * 60 / 5; // 288 five-minute buckets per day
    for b in 0..day_of_buckets {
        let bucket_ts = Utc::now() - Duration::days(40) + Duration::seconds(300 * b);
        let mut s = Sample::from_reading(bucket_ts, &reading, &volumes);
        s.swapins += b as u64;
        rollup_only.insert(&s)?;
    }
    rollup_only.sweep(Utc::now(), Retention::default())?;
    let one_day_of_rollups = rollup_only.size_bytes()?.saturating_sub(empty);
    println!(
        "  rollup tier: {:.0} bytes/day => {:.2} MB for the 30-day window",
        one_day_of_rollups as f64,
        one_day_of_rollups as f64 * 30.0 / 1e6
    );
    println!();

    // Steady state: the sweep normally handles ONE aged bucket per cycle, not a
    // day's worth. That is the number the 15s loop actually pays.
    let store2 = SampleStore::open(&dir.path().join("steady.db"))?;
    let old = Utc::now() - Duration::hours(25);
    for i in 0..20 {
        let mut s = Sample::from_reading(old + Duration::seconds(15 * i), &reading, &volumes);
        s.swapins += i as u64;
        store2.insert(&s)?;
    }
    let t0 = Instant::now();
    store2.sweep(Utc::now(), Retention::default())?;
    println!("retention sweep (steady state: one 5-minute bucket)");
    println!("  {:>10.2} ms", t0.elapsed().as_secs_f64() * 1e3);

    Ok(())
}
