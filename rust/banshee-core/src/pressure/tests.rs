use super::*;
use crate::census::{HelperRollup, MonitorTotal, OrphanCensus};
use crate::sample::VolumeSample;
use crate::sys::DATA_VOLUME;
use uuid::Uuid;

const BOOT: i64 = 1_788_100_000;

fn at(secs: i64) -> DateTime<Utc> {
    DateTime::from_timestamp(BOOT + secs, 0).unwrap()
}

/// A healthy sample, `t` seconds after boot. CPU reads a flat 20% busy — green,
/// and the same at any spacing (see `cpu_ticks_for`).
fn sample(t: i64) -> Sample {
    let (cpu_ticks_total, cpu_ticks_idle) = cpu_ticks_for(t, 0.20);
    Sample {
        id: Uuid::new_v4(),
        sampled_at: at(t),
        boot_time_secs: BOOT,
        ncpu: 8,
        mem_total_bytes: 25_769_803_776,
        page_size: 16384,
        load1m: 4.0, // 0.5 per core
        load5m: 4.0,
        load15m: 4.0,
        swap_total_bytes: 8_589_934_592,
        swap_used_bytes: 1_000_000_000,
        pages_free: 400_000, // 6.5 GB free
        pages_active: 200_000,
        pages_inactive: 200_000,
        pages_wired: 100_000,
        pages_speculative: 0,
        pages_compressor: 10_000,
        pages_purgeable: 0,
        pages_external: 0,
        swapins: 1_000,
        swapouts: 2_000,
        memory_pressure_level: 1,        // healthy: the kernel agrees
        thermal_pressure_level: Some(0), // nominal
        cpu_speed_limit_percent: None,   // Apple silicon: not published
        cpu_ticks_total,
        cpu_ticks_idle,
        kernel_free_percent: Some(50), // healthy reclaimability
        jetsam_kills: Some(0),         // no kills
        volumes: vec![VolumeSample {
            mount_point: DATA_VOLUME.into(),
            total_bytes: 494_384_795_648,
            avail_bytes: 100_000_000_000, // 100 GB free
        }],
    }
}

/// `n` healthy samples, 15s apart, ending `end_t` seconds after boot.
fn healthy(n: usize, end_t: i64) -> Vec<Sample> {
    (0..n)
        .map(|i| sample(end_t - ((n - 1 - i) as i64 * 15)))
        .collect()
}

/// Ticks the whole machine accrues per second, arbitrary — chosen large enough that
/// common utilizations land on integer idle-tick rates.
const CPU_RATE: u64 = 10_000;

/// Cumulative CPU ticks at `t` seconds after boot for a machine that has held a
/// CONSTANT `util` busy since boot. Both counters are linear in `t`, so EVERY
/// interval — whatever its spacing, and whether or not a gap sits inside it — reads
/// exactly `util`. This is the per-pair analog of a flat load, and it is what lets a
/// gap-spanning interval read the same utilization as a contiguous one: the held/gap
/// machinery keys on timestamps, so the value must not change across a hole.
fn cpu_ticks_for(t: i64, util: f64) -> (Option<u64>, Option<u64>) {
    let t = t.max(0) as u64;
    let total = t * CPU_RATE;
    let idle = (t as f64 * CPU_RATE as f64 * (1.0 - util)).round() as u64;
    (Some(total), Some(idle))
}

/// Set every sample to a CONSTANT measured utilization, from its own timestamp.
fn set_cpu(samples: &mut [Sample], util: f64) {
    for s in samples.iter_mut() {
        let (total, idle) = cpu_ticks_for(s.sampled_at.timestamp() - BOOT, util);
        s.cpu_ticks_total = total;
        s.cpu_ticks_idle = idle;
    }
}

/// Set the per-INTERVAL utilization (one entry per interval, so `samples.len() - 1`)
/// by accumulating tick counters — the per-pair analog of `with_thrash_rates`. Each
/// interval reads exactly its utilization, independent of the sample spacing.
fn with_cpu_utils(samples: &mut [Sample], utils: &[f64]) {
    assert_eq!(
        utils.len(),
        samples.len() - 1,
        "one utilization per interval, not per sample"
    );
    const BUDGET: u64 = 100_000;
    let (mut total, mut idle) = (0u64, 0u64);
    samples[0].cpu_ticks_total = Some(total);
    samples[0].cpu_ticks_idle = Some(idle);
    for (i, &u) in utils.iter().enumerate() {
        let busy = (BUDGET as f64 * u).round() as u64;
        total += BUDGET;
        idle += BUDGET - busy;
        samples[i + 1].cpu_ticks_total = Some(total);
        samples[i + 1].cpu_ticks_idle = Some(idle);
    }
}

fn census(t: i64, stale: u32, orphans: u32, managed_agents: f64, life: u64) -> Census {
    let sessions = (0..stale)
        .map(|i| crate::census::AgentSession {
            pid: 1000 + i,
            program: "claude".into(),
            rss_bytes: 100 * 1024 * 1024,
            age_secs: 8 * 86_400,
            age_days: 8,
            cpu_secs: 100.0,
            tty: format!("ttys{i:03}"),
            is_stale: true,
            cwd: None,
        })
        .collect();
    Census {
        id: Uuid::new_v4(),
        taken_at: at(t),
        total_procs: 900,
        agent_sessions: sessions,
        ide_helpers: HelperRollup {
            count: 2,
            rss_bytes: 0,
        },
        orphans: OrphanCensus {
            total_count: 150,
            total_rss_bytes: 1_000_000_000,
            orphan_count: orphans,
            orphan_rss_bytes: 10_000_000,
            orphans_by_program: Vec::new(),
        },
        tmux_sessions: Vec::new(),
        app_groups: Vec::new(),
        monitor_agents: Vec::new(),
        monitor_total: MonitorTotal {
            proc_count: 13,
            rss_bytes: 577_000_000,
            cpu_secs_total: 1000.0,
            percent_of_one_core: managed_agents,
            longest_life_secs: life,
        },
        tmux_available: true,
        cpu_consumers: Vec::new(),
    }
}

fn healthy_census(t: i64) -> Census {
    census(t, 0, 2, 49.2, 78_000)
}

fn cfg() -> PressureConfig {
    PressureConfig::default()
}

// ---- data sufficiency ---------------------------------------------------

/// No samples is Checking, NOT Quiet. Mutation-proof: return a Quiet verdict for
/// an empty history and the menu bar claims calm before it has looked.
#[test]
fn an_empty_history_is_checking() {
    let p = evaluate(at(0), &[], &[], &cfg());
    assert_eq!(p.level, Level::Checking);
    assert_eq!(p.glyph, "🫧");
    assert!(p.findings.is_empty(), "no findings without data");
    assert_eq!(p.sample_count, 0);
}

/// One sample is below `min_samples`, so still Checking — a rate needs two
/// points and half the model is rates. Mutation-proof: drop the `min_samples`
/// check and this reports Quiet off a single reading.
#[test]
fn a_single_sample_is_still_checking() {
    let p = evaluate(at(15), &healthy(1, 15), &[], &cfg());
    assert_eq!(p.level, Level::Checking);
    assert!(p.findings.is_empty());
}

#[test]
fn two_healthy_samples_are_enough_for_a_verdict() {
    let p = evaluate(at(15), &healthy(2, 15), &[], &cfg());
    assert_eq!(p.level, Level::Quiet);
    assert_eq!(p.glyph, "😴");
    assert!(p.findings.is_empty());
}

/// Samples without a census still yield a verdict on the dimensions that do not
/// need one. A daemon started 20 seconds ago has samples but no census yet.
#[test]
fn samples_without_a_census_still_produce_a_verdict() {
    let p = evaluate(at(60), &healthy(5, 60), &[], &cfg());
    assert_ne!(p.level, Level::Checking);
    assert!(p.reading(Dimension::Cpu).is_some());
    assert!(
        p.reading(Dimension::Agents).is_none(),
        "no census, no census-derived dimensions"
    );
    assert_eq!(p.census_count, 0);
}

/// The window is bounded: a very long history is trimmed to `window_samples`, so
/// the model reacts to now rather than averaging over a day.
#[test]
fn the_window_is_trimmed_to_the_configured_length() {
    let c = PressureConfig {
        window_samples: 5,
        ..cfg()
    };
    let p = evaluate(at(1500), &healthy(100, 1500), &[], &c);
    assert_eq!(p.sample_count, 5);
}

// ---- dimensions ---------------------------------------------------------

#[test]
fn a_healthy_machine_reports_every_dimension_green() {
    let p = evaluate(at(300), &healthy(21, 300), &[healthy_census(300)], &cfg());
    assert_eq!(p.level, Level::Quiet);
    for r in &p.dimensions {
        assert_eq!(
            r.band,
            Band::Green,
            "{:?} should be green: {r:?}",
            r.dimension
        );
    }
    // All fourteen, since both tiers have data.
    assert_eq!(p.dimensions.len(), 14, "{:?}", p.dimensions);
}

/// CPU bands on MEASURED utilization from the tick counters, never on load (`banshee-87l.19`).
/// The headline is busy-time; load-per-core is the labeled
/// secondary. Mutation-proof: wire `series_for(Cpu)` back to `load_per_core_1m()`
/// and a machine 20% busy at a load of 0.5 per core reports its run queue instead
/// of its idleness.
#[test]
fn cpu_is_banded_on_measured_utilization_not_load() {
    let p = evaluate(at(300), &healthy(21, 300), &[], &cfg());
    let cpu = p.reading(Dimension::Cpu).unwrap();
    assert_eq!(cpu.band, Band::Green);
    assert!(
        (cpu.value - 0.20).abs() < 1e-9,
        "20% busy from the tick counters"
    );
    assert!(cpu.detail.contains("20% busy"), "got {:?}", cpu.detail);
    // The run queue is still shown, as the labeled secondary (4.0 load ÷ 8 cores).
    assert!(cpu.detail.contains("0.5× per core"), "got {:?}", cpu.detail);
}

/// **The `banshee-87l.19` acceptance pin, and the co-variance breaker.** A deep run
/// queue over IDLE cores — the 3.26× incident, where a churning security agent's
/// runnable threads read saturated while the cores were half idle — is NOT a red
/// compute alarm. It is scheduling contention, named as such, calm-faced, no fire.
/// The SAME load over BUSY cores is genuine saturation: red, fire, CPU as the
/// source. Load-per-core is IDENTICAL in both fixtures; only measured utilization
/// comes apart, so an implementation that dropped the utilization cross-check and
/// banded on load could not tell them apart. Mutation-proof: revert the Cpu series
/// to `load_per_core_1m()` and the idle case flips to a red 🔥.
#[test]
fn a_deep_run_queue_over_idle_cores_is_contention_not_saturation() {
    // load 28 on 8 cores = 3.5× per core — the shape of the incident's 3.26×.
    let mut idle = healthy(21, 300);
    for s in &mut idle {
        s.load1m = 28.0;
    }
    set_cpu(&mut idle, 0.20); // cores 20% busy: idle
    let p = evaluate(at(300), &idle, &[], &cfg());
    let cpu = p.reading(Dimension::Cpu).unwrap();
    assert_eq!(
        cpu.band,
        Band::Green,
        "idle cores are not saturated, whatever the run queue: {cpu:?}"
    );
    assert!(
        cpu.detail.contains("scheduling contention"),
        "the words name contention, not saturation: {:?}",
        cpu.detail
    );
    assert!(
        cpu.detail.contains("3.5× per core"),
        "and still name the run queue: {:?}",
        cpu.detail
    );
    assert_ne!(p.source, Some(Source::Cpu), "CPU is not the story");
    assert!(
        !p.glyph.contains('🔥'),
        "no fire over idle cores: {:?}",
        p.glyph
    );
    assert!(
        p.findings.iter().all(|f| f.dimension != Dimension::Cpu),
        "a green CPU produces no finding, so no saturation alarm: {:?}",
        p.findings
    );

    // The SAME load, cores now genuinely pegged.
    let mut busy = healthy(21, 300);
    for s in &mut busy {
        s.load1m = 28.0;
    }
    set_cpu(&mut busy, 0.98);
    let p = evaluate(at(300), &busy, &[], &cfg());
    let cpu = p.reading(Dimension::Cpu).unwrap();
    assert_eq!(cpu.band, Band::Red, "98% busy IS saturation: {cpu:?}");
    assert!(cpu.detail.contains("98% busy"), "got {:?}", cpu.detail);
    let f = p
        .findings
        .iter()
        .find(|f| f.dimension == Dimension::Cpu)
        .expect("a red cpu dimension is a finding");
    assert!(
        f.message.contains("saturated"),
        "the finding earns the word: {:?}",
        f.message
    );
    assert_eq!(p.source, Some(Source::Cpu));
    assert!(
        p.glyph.contains('🔥'),
        "fire over pegged cores: {:?}",
        p.glyph
    );
}

/// A saturated machine reports CPU red and escalates — measured 98% busy.
#[test]
fn a_saturated_machine_reports_cpu_red() {
    let mut samples = healthy(21, 300);
    set_cpu(&mut samples, 0.98);
    let p = evaluate(at(300), &samples, &[], &cfg());
    assert_eq!(p.reading(Dimension::Cpu).unwrap().band, Band::Red);
    assert!(p.level >= Level::Wailing, "sustained red: {:?}", p.level);
}

/// Memory bands on AVAILABLE bytes — free + speculative + purgeable + file
/// cache — and gets worse as they fall. The two cases below are the
/// `banshee-cot` distinguishers: the free list is IDENTICAL (tiny) in both,
/// and only the reclaimable pools differ. An implementation banding on
/// `free_bytes` calls both red; availability calls one green.
#[test]
fn memory_is_banded_on_available_bytes_falling() {
    // Healthy: 84 MB on the free list, ~9 GB reclaimable behind it — the
    // measured 2026-09-01 state where the kernel's own pressure level said
    // NORMAL while the old metric sat red.
    let mut samples = healthy(21, 300);
    for s in &mut samples {
        s.pages_free = 5_127; // 84 MB
        s.pages_speculative = 1_250;
        s.pages_purgeable = 5_581;
        s.pages_external = 550_000; // ~9 GB of page cache
    }
    let p = evaluate(at(300), &samples, &[], &cfg());
    let mem = p.reading(Dimension::Memory).unwrap();
    assert_eq!(
        mem.band,
        Band::Green,
        "a tiny free list with a full page cache is a HEALTHY machine"
    );

    // Genuine pressure: the same tiny free list with nothing reclaimable
    // behind it — the 2026-08-31 swap-crisis shape.
    let mut samples = healthy(21, 300);
    for s in &mut samples {
        s.pages_free = 5_127;
        s.pages_speculative = 0;
        s.pages_purgeable = 0;
        s.pages_external = 4_000; // 66 MB of cache left
    }
    let p = evaluate(at(300), &samples, &[], &cfg());
    let mem = p.reading(Dimension::Memory).unwrap();
    assert_eq!(mem.band, Band::Red);
    assert!(mem.detail.contains("MB available"), "got {:?}", mem.detail);
}

/// The kernel-pressure dimension bands on the kernel's own level, and the
/// fixture is built at the point where the two memory signals COME APART:
/// availability comfortably green — pages identical to the
/// healthy helper — while the kernel says WARN, which is the state the
/// banshee-10w parity run measured (swap was carrying the pressure; the
/// verdict was blind to the kernel's own claim). An implementation deriving
/// this dimension from `available_bytes` reports green here and fails.
#[test]
fn kernel_pressure_is_banded_on_the_kernels_level_not_availability() {
    let mut samples = healthy(21, 300);
    for s in &mut samples {
        s.memory_pressure_level = 2; // WARN, with ~6.5 GB available
    }
    let p = evaluate(at(300), &samples, &[], &cfg());

    let kernel = p.reading(Dimension::KernelPressure).unwrap();
    assert_eq!(kernel.band, Band::Yellow, "WARN is yellow: {kernel:?}");
    assert!(kernel.detail.contains("warn"), "got {:?}", kernel.detail);
    let mem = p.reading(Dimension::Memory).unwrap();
    assert_eq!(
        mem.band,
        Band::Green,
        "availability stays green — the kernel's verdict is its OWN dimension, \
         not an override of this one"
    );
    // One yellow is Stirring — the point is that the verdict is no longer
    // BLIND to the kernel's claim, not that a lone WARN is an emergency.
    assert_eq!(p.level, Level::Stirring, "the verdict is no longer blind");
}

/// …and the reverse direction: availability collapsing while the kernel still
/// says NORMAL must not drag the kernel dimension along. The two fixtures
/// TOGETHER are what break the co-variance — either alone would pass with the
/// two dimensions wired to the same series.
#[test]
fn a_normal_kernel_level_does_not_follow_availability_down() {
    let mut samples = healthy(21, 300);
    for s in &mut samples {
        // The 2026-08-31 crisis shape: nothing reclaimable…
        s.pages_free = 5_127;
        s.pages_speculative = 0;
        s.pages_purgeable = 0;
        s.pages_external = 4_000;
        // …but the kernel (hypothetically) still calm.
        s.memory_pressure_level = 1;
    }
    let p = evaluate(at(300), &samples, &[], &cfg());
    assert_eq!(p.reading(Dimension::Memory).unwrap().band, Band::Red);
    assert_eq!(
        p.reading(Dimension::KernelPressure).unwrap().band,
        Band::Green,
        "the kernel dimension reports what the KERNEL said"
    );
}

/// CRITICAL (4) is red, and the finding says whose claim it is.
#[test]
fn a_critical_kernel_level_is_red_with_the_kernels_own_words() {
    let mut samples = healthy(21, 300);
    for s in &mut samples {
        s.memory_pressure_level = 4;
    }
    let p = evaluate(at(300), &samples, &[], &cfg());
    let kernel = p.reading(Dimension::KernelPressure).unwrap();
    assert_eq!(kernel.band, Band::Red);
    let f = p
        .findings
        .iter()
        .find(|f| f.dimension == Dimension::KernelPressure)
        .expect("a red kernel level is a finding");
    assert!(
        f.message.contains("kernel itself reports critical"),
        "got {:?}",
        f.message
    );
    assert_eq!(f.action, Action::RelaunchApps, "same lever as memory/swap");
}

/// A pre-v8 sample stores 0 ("not recorded"), and the model must SKIP it, not
/// band on it. Mutation-proof both ways: treating 0 as a reading would (a)
/// report a green "kernel reports level 0" row here instead of NO row, and
/// (b) dilute a real WARN run's hysteresis with spurious greens.
#[test]
fn an_unrecorded_kernel_level_produces_no_reading_not_a_green_one() {
    let mut samples = healthy(21, 300);
    for s in &mut samples {
        s.memory_pressure_level = 0; // every row pre-dates schema v8
    }
    let p = evaluate(at(300), &samples, &[], &cfg());
    assert!(
        p.reading(Dimension::KernelPressure).is_none(),
        "no recorded level, no claim: {:?}",
        p.reading(Dimension::KernelPressure)
    );
    // The other dimensions are untouched by the skip.
    assert!(p.reading(Dimension::Memory).is_some());
    assert_ne!(p.level, Level::Checking);
}

// ---- thermal (schema v11) --------------------------------------------------

/// HEAVY (2) — Apple's "serious": performance impacted — is red, the finding is
/// in the kernel's words and says what red MEANS for every other number on the
/// screen, and there is no process to reap for heat.
#[test]
fn a_heavy_thermal_level_is_red_with_the_kernels_words_and_no_action() {
    let mut samples = healthy(21, 300);
    for s in &mut samples {
        s.thermal_pressure_level = Some(2);
    }
    let p = evaluate(at(300), &samples, &[], &cfg());
    let t = p.reading(Dimension::Thermal).unwrap();
    assert_eq!(t.band, Band::Red);
    assert_eq!(t.detail, "kernel reports heavy");
    let f = p
        .findings
        .iter()
        .find(|f| f.dimension == Dimension::Thermal)
        .expect("a red thermal level is a finding");
    assert!(
        f.message.contains("heavy thermal pressure"),
        "got {:?}",
        f.message
    );
    assert!(
        f.message.contains("throttled"),
        "red must say the CPU is being slowed: {:?}",
        f.message
    );
    assert_eq!(f.action, Action::None, "heat has no process to reap");
}

/// MODERATE (1) — Apple's "fair": fans audible — is yellow, and the yellow finding
/// does NOT claim throttling (nothing is being slowed yet). Nominal (0) is green.
#[test]
fn a_moderate_thermal_level_is_yellow_without_the_throttling_claim() {
    let mut samples = healthy(21, 300);
    for s in &mut samples {
        s.thermal_pressure_level = Some(1);
    }
    let p = evaluate(at(300), &samples, &[], &cfg());
    let t = p.reading(Dimension::Thermal).unwrap();
    assert_eq!(t.band, Band::Yellow);
    assert_eq!(t.detail, "kernel reports moderate");
    let f = p
        .findings
        .iter()
        .find(|f| f.dimension == Dimension::Thermal)
        .unwrap();
    assert!(
        f.message.contains("moderate thermal pressure"),
        "got {:?}",
        f.message
    );
    assert!(
        !f.message.contains("throttled"),
        "yellow must not claim throttling: {:?}",
        f.message
    );

    let calm = evaluate(at(300), &healthy(21, 300), &[], &cfg());
    assert_eq!(calm.reading(Dimension::Thermal).unwrap().band, Band::Green);
    assert_eq!(
        calm.reading(Dimension::Thermal).unwrap().detail,
        "kernel reports nominal"
    );
}

/// A pre-v11 row stores NULL, and the model must SKIP it, not read it as 0 —
/// because 0 IS a real reading (nominal). Mutation-proof: `unwrap_or(0)` in the
/// series builder would produce a green "kernel reports nominal" row here instead
/// of NO row, inventing a calm kernel from history that never looked.
#[test]
fn an_unrecorded_thermal_level_produces_no_reading_not_a_nominal_one() {
    let mut samples = healthy(21, 300);
    for s in &mut samples {
        s.thermal_pressure_level = None;
    }
    let p = evaluate(at(300), &samples, &[], &cfg());
    assert!(
        p.reading(Dimension::Thermal).is_none(),
        "no recorded level, no claim: {:?}",
        p.reading(Dimension::Thermal)
    );
    assert!(
        p.reading(Dimension::Cpu).is_some(),
        "the other dimensions are untouched"
    );
    assert_ne!(p.level, Level::Checking);
}

/// The co-variance pair. (a) A THROTTLED but IDLE machine — trapping (3) with
/// load well under one per core — is a sustained thermal red and a CPU green,
/// and the verdict names THERMAL as the source (the 🌡️ suffix), not CPU.
/// Either half alone would pass with the thermal series wired to the load
/// series; together they cannot.
#[test]
fn a_throttled_idle_machine_is_a_thermal_red_not_a_cpu_red() {
    let mut samples = healthy(21, 300);
    for s in &mut samples {
        s.thermal_pressure_level = Some(3);
        s.load1m = 2.0; // 0.25 per core on 8 cores
    }
    let p = evaluate(at(300), &samples, &[], &cfg());
    assert_eq!(p.reading(Dimension::Thermal).unwrap().band, Band::Red);
    assert_eq!(p.reading(Dimension::Cpu).unwrap().band, Band::Green);
    assert_eq!(
        p.level,
        Level::Wailing,
        "one sustained red past the 120 s grace"
    );
    assert_eq!(p.source, Some(Source::Thermal));
    assert!(
        p.glyph.contains("🌡️"),
        "the menu bar names heat: {:?}",
        p.glyph
    );
}

/// (b) …and a SATURATED but COOL machine is a CPU red with a thermal green.
#[test]
fn a_saturated_cool_machine_is_a_cpu_red_not_a_thermal_red() {
    let mut samples = healthy(21, 300);
    for s in &mut samples {
        s.thermal_pressure_level = Some(0);
    }
    set_cpu(&mut samples, 0.98); // cores genuinely pegged
    let p = evaluate(at(300), &samples, &[], &cfg());
    assert_eq!(p.reading(Dimension::Cpu).unwrap().band, Band::Red);
    assert_eq!(p.reading(Dimension::Thermal).unwrap().band, Band::Green);
    assert_eq!(p.source, Some(Source::Cpu));
}

/// Swap bands on ABSOLUTE BYTES, never a percentage — ADR-0007's addendum. The
/// denominator moves, so a percentage can FALL while the machine worsens.
/// Mutation-proof: band on `swap_used / swap_total` and the case below (10.6 GB
/// used of a 64 GB total, i.e. 17%) reports Green.
#[test]
fn swap_is_banded_on_absolute_bytes_not_a_percentage() {
    let mut samples = healthy(21, 300);
    for s in &mut samples {
        s.swap_used_bytes = 10_660_000_000;
        // A large total, so any percentage-based rule would look calm.
        s.swap_total_bytes = 64_000_000_000;
    }
    let p = evaluate(at(300), &samples, &[], &cfg());
    let swap = p.reading(Dimension::Swap).unwrap();

    // 10.66 GB is past the 4 GB yellow line and short of the 12 GB red one.
    assert_eq!(swap.band, Band::Yellow);
    assert!(swap.band > Band::Green, "the machine is not calm");

    // The contrast that matters: `perf-scan` bands on swap PERCENT, and this same
    // reading is 17% of its (larger) total — comfortably "healthy" by that rule.
    let pct = 10_660_000_000.0 / 64_000_000_000.0 * 100.0;
    assert!(
        pct < 60.0,
        "a percentage rule would have called this calm at {pct:.0}%"
    );

    // And the same absolute reading stays Yellow even if the total grows, which
    // is the failure mode ADR-0007 describes: macOS growing the swap file makes
    // a percentage FALL while nothing improves.
    let mut bigger = samples.clone();
    for s in &mut bigger {
        s.swap_total_bytes = 256_000_000_000;
    }
    let p2 = evaluate(at(300), &bigger, &[], &cfg());
    assert_eq!(
        p2.reading(Dimension::Swap).unwrap().band,
        Band::Yellow,
        "the verdict must not depend on the denominator"
    );
}

/// Thrash is a RATE, which is the thing `perf-scan` structurally cannot compute.
/// Mutation-proof: use the lifetime counter instead of the delta and this reports
/// an absurd rate off the first sample.
#[test]
fn thrash_is_a_rate_derived_from_consecutive_samples() {
    let mut samples = healthy(21, 300);
    // 9,300 ops/sec across a 15s gap = 139,500 per step.
    for (i, s) in samples.iter_mut().enumerate() {
        s.swapins = 1_000 + i as u64 * 45_000;
        s.swapouts = 2_000 + i as u64 * 94_500;
    }
    let p = evaluate(at(300), &samples, &[], &cfg());
    let th = p.reading(Dimension::Thrash).unwrap();
    assert_eq!(th.band, Band::Red);
    assert!(
        (th.value - 9300.0).abs() < 1.0,
        "expected ~9300 ops/sec, got {}",
        th.value
    );
}

/// Set the per-interval thrash rate of a 15s-spaced series from `rates` (one entry
/// per INTERVAL, so `samples.len() - 1` of them) by accumulating the counters.
///
/// Thrash is a delta over elapsed time, so a fixture that wants a specific shape has
/// to build the cumulative counters that produce it rather than setting a rate.
fn with_thrash_rates(samples: &mut [Sample], rates: &[f64]) {
    assert_eq!(
        rates.len(),
        samples.len() - 1,
        "one rate per interval, not per sample"
    );
    let mut swapins = 1_000u64;
    let mut swapouts = 2_000u64;
    samples[0].swapins = swapins;
    samples[0].swapouts = swapouts;
    for (i, &r) in rates.iter().enumerate() {
        // Split the delta across both counters; the dimension sums them.
        let per_counter = (r * 15.0 / 2.0).round() as u64;
        swapins += per_counter;
        swapouts += per_counter;
        samples[i + 1].swapins = swapins;
        samples[i + 1].swapouts = swapouts;
    }
}

/// **The pin that the old implementation could not pass** (`banshee-s4d`).
///
/// Thrash bands on the WINDOW'S PEAK, so a machine that stormed and has just gone
/// quiet still reports red — and reports the peak as its value, not the calm
/// instant. Measured shape: an alert fired RED at 1706 ops/sec at 18:39 and the
/// verdict six minutes later said GREEN at 24 ops/sec, so an assistant reading it
/// told the user the machine felt fine while it was in fact storming every few
/// minutes.
///
/// FIXTURE NOTE (the recurring co-varying trap): the newest interval MUST sit in a
/// trough. A fixture whose storm runs to the end of the series passes with either
/// implementation, because "newest" and "window peak" agree there — which is exactly
/// how this bug survived having tests at all.
#[test]
fn thrash_bands_on_the_window_peak_so_a_storm_in_a_trough_still_reports_red() {
    let mut samples = healthy(21, 300);
    // 12 calm intervals, a 5-interval (75s) storm, then 3 calm intervals (45s).
    let mut rates = vec![20.0; 12];
    rates.extend(std::iter::repeat_n(2500.0, 5));
    rates.extend(std::iter::repeat_n(20.0, 3));
    with_thrash_rates(&mut samples, &rates);

    let p = evaluate(at(300), &samples, &[], &cfg());
    let th = p.reading(Dimension::Thrash).unwrap();
    assert_eq!(
        th.band,
        Band::Red,
        "a storm inside the window is still red once it has gone quiet, got value {}",
        th.value
    );
    assert!(
        (th.value - 2500.0).abs() < 1.0,
        "the value must be the window peak, not the newest 20 ops/sec: got {}",
        th.value
    );
    // And the wording must not call a window peak a current rate.
    assert!(
        th.detail.starts_with("peak "),
        "detail must say it is a peak: {:?}",
        th.detail
    );
}

/// The other half of the pin: an ISOLATED spike must not pin the band.
///
/// This is what makes the statistic an upper percentile rather than a plain maximum,
/// and it is a real measured case, not a hypothetical — quitting a 6.5 GB browser
/// produced a 6394 ops/sec burst as the kernel tore the working set down. That churn
/// was caused BY the remediation, so holding red for the rest of the window would
/// tell someone their fix failed at the moment it worked.
///
/// Mutation-proof: replace the drop-the-top-decile index with `vals.len() - 1` (a
/// plain max) and this fails while the test above still passes. Neither test alone
/// pins the statistic — they only do it as a pair.
#[test]
fn a_single_isolated_thrash_spike_does_not_pin_the_band() {
    let mut samples = healthy(21, 300);
    let mut rates = vec![20.0; 20];
    rates[10] = 6394.0; // one interval, mid-window
    with_thrash_rates(&mut samples, &rates);

    let p = evaluate(at(300), &samples, &[], &cfg());
    let th = p.reading(Dimension::Thrash).unwrap();
    assert_eq!(
        th.band,
        Band::Green,
        "one 15s burst is not a thrashing machine, got value {}",
        th.value
    );
}

/// A storm ageing out of the window must make the reported figure DECAY, never climb.
///
/// **Found by running the fixed daemon against a real machine, not by a test.** The
/// first implementation of `windowed_peak` used an upper percentile, and the live
/// series went `118 -> 118 -> 118 -> 523 -> 523 -> 54 -> 30` ops/sec: it more than
/// TRIPLED with no new storming, because a percentile's rank depends on how many
/// points are in the window, and points merely LEAVING moved the rank to a higher
/// order statistic. A number that rises while the machine calms is the same species
/// of lie the windowing was introduced to remove.
///
/// FIXTURE NOTE (the recurring co-varying trap, and it bit here): the intervals must
/// be UNEVENLY spaced. With a perfectly regular cadence the in-window count is
/// constant, the percentile's rank never moves, and the old implementation passes this
/// test — the defect is only reachable when the count crosses a boundary, which is
/// what real sampling jitter does and a tidy fixture does not.
#[test]
fn a_storm_ageing_out_of_the_window_only_ever_decays() {
    // Jittered spacing (15s and 20s), so the number of points inside a 300s window
    // varies rather than sitting at a constant 20.
    let mut t = 0i64;
    let mut times = Vec::new();
    for i in 0..44 {
        times.push(t);
        t += if i % 4 == 0 { 20 } else { 15 };
    }
    // A sustained storm early on, then nothing but calm, so it must age out.
    let points: Vec<(DateTime<Utc>, f64)> = times
        .iter()
        .enumerate()
        .map(|(i, &secs)| {
            let v = if (5..11).contains(&i) { 2500.0 } else { 15.0 };
            (at(secs), v)
        })
        .collect();

    let out = windowed_peak(&points, 300);
    // From the storm's last point onwards the figure may only fall.
    let tail: Vec<f64> = out[10..].iter().map(|(_, v)| *v).collect();
    for w in tail.windows(2) {
        assert!(
            w[1] <= w[0] + 1e-9,
            "the reported peak climbed with no new churn: {:?}",
            tail
        );
    }
    // And it must actually get all the way back down, not plateau forever.
    assert!(
        (tail.last().unwrap() - 15.0).abs() < 1e-9,
        "a storm long past the window must decay to the calm level, got {:?}",
        tail.last()
    );
}

/// `thrash_peak_window_secs` must actually be load-bearing.
///
/// A config value that silently does nothing is worse than no config value, and this
/// project has shipped that bug before (a tolerance configurable in `PressureConfig`
/// and hardcoded in the series). Same samples, two windows, two REPORTED VALUES: the
/// wide window still sees the storm, the narrow one has already forgotten it.
///
/// Asserts on `value` rather than `band` on purpose. The band is downstream of
/// hysteresis, which holds a pending de-escalation for `confirm_samples` after the
/// value has already dropped — so a band assertion here would be testing `replay`
/// rather than the window, and it fails for that reason alone (verified: the narrow
/// window reports value 20 while the band is still Red with Green pending).
#[test]
fn the_thrash_peak_window_is_load_bearing() {
    let mut samples = healthy(21, 300);
    let mut rates = vec![20.0; 12];
    rates.extend(std::iter::repeat_n(2500.0, 5));
    rates.extend(std::iter::repeat_n(20.0, 3));
    with_thrash_rates(&mut samples, &rates);

    let mut narrow = cfg();
    narrow.thrash_peak_window_secs = 20;
    let p = evaluate(at(300), &samples, &[], &narrow);
    let th = p.reading(Dimension::Thrash).unwrap();
    assert!(
        (th.value - 20.0).abs() < 1.0,
        "a 20s window cannot see a storm that ended 45s ago: got {}",
        th.value
    );
}

/// A reboot inside the window must not manufacture a thrash spike. The rate
/// module refuses such pairs, so the point is simply absent. Mutation-proof:
/// ignore `RateGap::Rebooted` and this reports a huge negative or wild value.
#[test]
fn a_reboot_inside_the_window_does_not_manufacture_a_thrash_spike() {
    let mut samples = healthy(6, 90);
    // Halfway through, the machine rebooted and the counters restarted.
    for s in samples.iter_mut().skip(3) {
        s.boot_time_secs = BOOT + 1000;
        s.swapins = 5;
        s.swapouts = 7;
    }
    let p = evaluate(at(90), &samples, &[], &cfg());
    if let Some(th) = p.reading(Dimension::Thrash) {
        assert!(th.value >= 0.0, "no negative rate: {}", th.value);
        assert!(th.value < 1000.0, "no manufactured spike: {}", th.value);
    }
}

/// Disk bands on free bytes and projects a time-to-full. That projection is the
/// sentinel's entire contribution on disk — a number, not a treemap (ADR-0003).
#[test]
fn disk_projects_a_time_to_full_when_space_is_falling() {
    let n = 21;
    let mut samples = healthy(n, 300);
    // Falling 100 MB per sample (15s) from 45 GB: yellow, emptying slowly
    // enough (~1.9 h to full) that time does not force it red.
    for (i, s) in samples.iter_mut().enumerate() {
        s.volumes[0].avail_bytes = 45_000_000_000 - (i as u64 * 100_000_000);
    }
    let p = evaluate(at(300), &samples, &[], &cfg());
    let disk = p.reading(Dimension::Disk).unwrap();
    assert_eq!(disk.band, Band::Yellow);
    assert!(
        disk.trend_per_sec.is_some_and(|t| t < 0.0),
        "trend must be negative: {:?}",
        disk.trend_per_sec
    );
    assert!(
        disk.detail.contains("full in"),
        "expected a projection, got {:?}",
        disk.detail
    );
}

/// Pin (c) of `banshee-3sn`: a volume EMPTYING FAST is red on time alone, at a
/// byte level where a flat volume is merely yellow. The 2026-09-15 descent spent
/// 40 minutes in the yellow band at ~12 MB/s — indistinguishable, before this,
/// from a volume sitting still at the same reading. Mutation-proof: drop the
/// `forced > outcome.band` arm in `evaluate` and the falling case reads Yellow.
#[test]
fn a_fast_emptying_disk_is_red_though_its_bytes_say_yellow() {
    // 40 GB free — mid-yellow — falling 250 MB per 15s tick: ~40 min to full.
    let mut falling = healthy(21, 300);
    for (i, s) in falling.iter_mut().enumerate() {
        s.volumes[0].avail_bytes = 45_000_000_000 - (i as u64 * 250_000_000);
    }
    let p = evaluate(at(300), &falling, &[], &cfg());
    let disk = p.reading(Dimension::Disk).unwrap();
    assert_eq!(disk.band, Band::Red, "red on time alone: {}", disk.detail);
    assert!(
        disk.severity >= 1.0,
        "and past its line by severity: {}",
        disk.severity
    );

    // The distinguishing contrast (the co-varying-fixture rule): the same
    // newest reading, flat, stays yellow — so it is the slope, not the bytes,
    // that escalated above.
    let mut flat = healthy(21, 300);
    for s in flat.iter_mut() {
        s.volumes[0].avail_bytes = 40_000_000_000;
    }
    let p = evaluate(at(300), &flat, &[], &cfg());
    assert_eq!(p.reading(Dimension::Disk).unwrap().band, Band::Yellow);
}

/// Banshee never walks the filesystem, so the disk finding hands off to a disk-usage tool
/// rather than answering "what is taking the space" itself (ADR-0003).
/// Mutation-proof: change the action and this fails.
#[test]
fn the_disk_finding_defers_to_disk_tool() {
    let mut samples = healthy(21, 300);
    for s in &mut samples {
        s.volumes[0].avail_bytes = 3_000_000_000;
    }
    let p = evaluate(at(300), &samples, &[], &cfg());
    let f = p
        .findings
        .iter()
        .find(|f| f.dimension == Dimension::Disk)
        .expect("a disk finding");
    assert_eq!(f.action, Action::OpenDiskTool);
    assert!(p.actions().contains(&Action::OpenDiskTool));
}

// ---- census-derived dimensions -----------------------------------------

#[test]
fn stale_sessions_and_orphans_come_from_the_census() {
    let censuses = vec![
        census(0, 7, 45, 49.2, 78_000),
        census(300, 7, 45, 49.2, 78_000),
    ];
    let p = evaluate(at(300), &healthy(21, 300), &censuses, &cfg());
    assert_eq!(p.reading(Dimension::Agents).unwrap().value, 7.0);
    assert_eq!(p.reading(Dimension::Orphans).unwrap().band, Band::Red);
    assert_eq!(p.reading(Dimension::Agents).unwrap().band, Band::Red);
}

/// Sprawl findings carry the reap actions, in perf-scan's measured impact order:
/// stale sessions BEFORE orphans. Mutation-proof: swap the two ranks and this
/// fails.
#[test]
fn the_worklist_puts_stale_sessions_before_orphans() {
    let censuses = vec![
        census(0, 7, 45, 49.2, 78_000),
        census(300, 7, 45, 49.2, 78_000),
    ];
    let p = evaluate(at(300), &healthy(21, 300), &censuses, &cfg());
    let actions = p.actions();
    let sessions = actions.iter().position(|a| *a == Action::ReapStaleSessions);
    let orphans = actions.iter().position(|a| *a == Action::ReapOrphans);
    assert!(sessions.is_some() && orphans.is_some(), "{actions:?}");
    assert!(
        sessions < orphans,
        "stale sessions must rank above orphans: {actions:?}"
    );
}

/// A saturated CPU offers "Reap stale agent sessions" ONLY when the census has
/// stale sessions to reap (`banshee-rad`). Seen 2026-09-15: the popover's single
/// affordance was that button over a census reading "0 stale"; pressing it
/// opened an empty dry run — during a crisis the one button on screen led
/// nowhere. Mutation-proof: drop the `stale_sessions > 0` guard in `action_for`
/// and the zero-stale case carries the dead button again.
#[test]
fn a_cpu_red_with_nothing_to_reap_offers_no_reap_button() {
    let mut samples = healthy(41, 600);
    set_cpu(&mut samples, 0.98);
    // Saturated cores, and a census with ZERO stale sessions.
    let censuses = vec![
        census(0, 0, 2, 49.2, 78_000),
        census(600, 0, 2, 49.2, 78_000),
    ];
    let p = evaluate(at(600), &samples, &censuses, &cfg());
    let cpu = p
        .findings
        .iter()
        .find(|f| f.dimension == Dimension::Cpu)
        .expect("a cpu finding");
    assert_eq!(cpu.action, Action::None, "nothing to reap");
    assert!(
        !p.actions().contains(&Action::ReapStaleSessions),
        "no dead button anywhere in the worklist: {:?}",
        p.actions()
    );

    // The distinguishing contrast (the co-varying-fixture rule): the SAME
    // saturated machine with stale sessions in the census keeps the button —
    // it is the census, not the band, that decides.
    let censuses = vec![
        census(0, 7, 2, 49.2, 78_000),
        census(600, 7, 2, 49.2, 78_000),
    ];
    let p = evaluate(at(600), &samples, &censuses, &cfg());
    let cpu = p
        .findings
        .iter()
        .find(|f| f.dimension == Dimension::Cpu)
        .expect("a cpu finding");
    assert_eq!(cpu.action, Action::ReapStaleSessions);
}

/// A managed-agent reading derived from a young process is NOT banded. Measured:
/// a log shipper restarted 109s earlier reported 14.6% off 16s of CPU.
/// Mutation-proof: drop the `managed_agents_min_life_secs` filter and a freshly
/// restarted agent group produces a reading.
#[test]
fn a_young_managed_agent_reading_is_skipped_entirely() {
    // 200% of one core, but the longest-lived process is only 60s old.
    let censuses = vec![census(0, 0, 2, 200.0, 60), census(300, 0, 2, 200.0, 60)];
    let p = evaluate(at(300), &healthy(21, 300), &censuses, &cfg());
    assert!(
        p.reading(Dimension::ManagedAgents).is_none(),
        "a 60s-old process must not be banded at all"
    );
    assert_eq!(p.level, Level::Quiet, "and must not raise the level");
}

/// Once the processes have lived long enough, growth does trip.
#[test]
fn a_mature_managed_agent_group_bands_on_growth() {
    let censuses = vec![
        census(0, 0, 2, 70.0, 78_000),
        census(300, 0, 2, 70.0, 78_000),
    ];
    let p = evaluate(at(300), &healthy(21, 300), &censuses, &cfg());
    let corp = p.reading(Dimension::ManagedAgents).unwrap();
    assert_eq!(corp.band, Band::Yellow, "70% is past the 60% line");
    assert!(corp.detail.contains("of one core"));
}

/// Today's measured baseline (49.2%) must be Green, or the dimension is a
/// standing warning about something the user cannot remove — and a permanent warning
/// is a muted one.
#[test]
fn todays_managed_agent_baseline_is_not_a_standing_warning() {
    let censuses = vec![healthy_census(0), healthy_census(300)];
    let p = evaluate(at(300), &healthy(21, 300), &censuses, &cfg());
    assert_eq!(
        p.reading(Dimension::ManagedAgents).unwrap().band,
        Band::Green
    );
    assert_eq!(p.level, Level::Quiet);
}

/// Managed agents get no reap action: they are centrally managed and cannot be
/// removed. Telling the user to kill them would be advice they cannot take.
#[test]
fn the_managed_agent_finding_recommends_no_action() {
    let censuses = vec![
        census(0, 0, 2, 120.0, 78_000),
        census(300, 0, 2, 120.0, 78_000),
    ];
    let p = evaluate(at(300), &healthy(21, 300), &censuses, &cfg());
    let f = p
        .findings
        .iter()
        .find(|f| f.dimension == Dimension::ManagedAgents)
        .expect("a managed-agents finding");
    assert_eq!(f.action, Action::None);
    assert!(
        !p.actions().contains(&Action::None),
        "None is not a worklist item"
    );
}

// ---- hysteresis end to end ---------------------------------------------

/// A brief spike inside the grace period is Restless, not Wailing — a release
/// build turns this machine red for seconds at a time.
#[test]
fn a_brief_spike_does_not_reach_wailing() {
    let mut samples = healthy(21, 300);
    // Only the last three intervals (30s) are saturated.
    let mut utils = vec![0.20; 20];
    for u in utils.iter_mut().rev().take(3) {
        *u = 0.98;
    }
    with_cpu_utils(&mut samples, &utils);
    let p = evaluate(at(300), &samples, &[], &cfg());
    assert_eq!(p.reading(Dimension::Cpu).unwrap().band, Band::Red);
    assert_eq!(
        p.level,
        Level::Restless,
        "30s of red is inside the 120s grace"
    );
    assert_eq!(p.glyph, "😧🔥");
}

/// Sustained past the grace period, the same red becomes Wailing.
#[test]
fn a_sustained_spike_reaches_wailing() {
    let mut samples = healthy(41, 600);
    // The last 20 intervals saturated — well past the 120s grace.
    let mut utils = vec![0.20; 40];
    for u in utils.iter_mut().skip(20) {
        *u = 0.98;
    }
    with_cpu_utils(&mut samples, &utils);
    let p = evaluate(at(600), &samples, &[], &cfg());
    let cpu = p.reading(Dimension::Cpu).unwrap();
    assert_eq!(cpu.band, Band::Red);
    assert!(cpu.held_secs >= 120, "held {}s", cpu.held_secs);
    assert_eq!(p.level, Level::Wailing);
}

/// `held_secs` comes from real timestamps, not from an assumed cadence. The two
/// tiers tick at 15s and 300s, and a sleeping laptop leaves gaps in both.
/// Mutation-proof: multiply `held_samples` by a constant 15 and this fails.
#[test]
fn held_secs_is_derived_from_timestamps_not_an_assumed_cadence() {
    // Samples 60s apart, not 15s.
    let mut samples: Vec<Sample> = (0..6).map(|i| sample(i * 60)).collect();
    // Intervals 1..5 saturated: red utilization points at t = 120, 180, 240, 300.
    let mut utils = vec![0.20; 5];
    for u in utils.iter_mut().skip(1) {
        *u = 0.98;
    }
    with_cpu_utils(&mut samples, &utils);
    let p = evaluate(at(300), &samples, &[], &cfg());
    let cpu = p.reading(Dimension::Cpu).unwrap();
    // Four red utilization points spanning 180 seconds.
    assert_eq!(cpu.held_secs, 180, "must reflect the real 60s spacing");
}

/// A dimension whose newest values want a lower band but have not confirmed it
/// reports the change as PENDING, so the UI can show "recovering" without
/// pretending it already has.
#[test]
fn a_recovering_dimension_reports_a_pending_band() {
    let mut samples = healthy(21, 300);
    // Saturated throughout, then one clearing interval at the end; confirm_samples
    // is 2, so the drop is pending rather than adopted.
    let mut utils = vec![0.98; 20];
    utils[19] = 0.20;
    with_cpu_utils(&mut samples, &utils);
    let p = evaluate(at(300), &samples, &[], &cfg());
    let cpu = p.reading(Dimension::Cpu).unwrap();
    assert_eq!(cpu.band, Band::Red, "not yet confirmed");
    assert_eq!(cpu.pending, Some(Band::Green));
}

/// The CPU finding earns the word "saturated" only at red — measured utilization at
/// or above the saturation line, the cores actually pegged (`banshee-87l.19`). Yellow
/// is "busy, keeping up". The claim follows the BAND, not the raw value — the third
/// case is a hysteresis-held red whose newest interval has already recovered, so an
/// implementation gating on the raw value gives the wrong answer there while agreeing
/// on the other two.
#[test]
fn the_cpu_finding_claims_saturation_only_at_red() {
    let cpu_message = |p: &Pressure| {
        p.findings
            .iter()
            .find(|f| f.dimension == Dimension::Cpu)
            .expect("a banded cpu dimension must produce a finding")
            .message
            .clone()
    };

    // Yellow (88% busy): the factual sentence only.
    let mut samples = healthy(21, 300);
    set_cpu(&mut samples, 0.88);
    let p = evaluate(at(300), &samples, &[], &cfg());
    assert_eq!(p.reading(Dimension::Cpu).unwrap().band, Band::Yellow);
    let msg = cpu_message(&p);
    assert!(msg.contains("88% busy"), "{msg}");
    assert!(
        !msg.contains("saturated"),
        "yellow must not claim saturation: {msg}"
    );

    // Red (98% busy): the saturation sentence.
    set_cpu(&mut samples, 0.98);
    let p = evaluate(at(300), &samples, &[], &cfg());
    assert_eq!(p.reading(Dimension::Cpu).unwrap().band, Band::Red);
    assert!(cpu_message(&p).contains("saturated"));

    // Held red, newest interval recovered to 20% busy: still the model's official
    // position while the drop is unconfirmed.
    let mut utils = vec![0.98; 20];
    utils[19] = 0.20;
    with_cpu_utils(&mut samples, &utils);
    let p = evaluate(at(300), &samples, &[], &cfg());
    let cpu = p.reading(Dimension::Cpu).unwrap();
    assert_eq!(cpu.band, Band::Red, "hysteresis must hold red");
    assert!(
        cpu.value < cfg().cpu.yellow,
        "the raw value must have recovered"
    );
    assert!(
        cpu_message(&p).contains("saturated"),
        "the message follows the band, not the raw value"
    );
}

// ---- the incident -------------------------------------------------------

/// The 2026-08-04 incident, reconstructed from its recorded numbers: load 76 on
/// 8 cores, swap 35.6 GB, 7 stale sessions, 45 orphaned helpers. This is the
/// event the whole app exists to have caught.
#[test]
fn the_august_incident_shrieks_and_names_the_right_worklist() {
    let mut samples = healthy(41, 600);
    for s in &mut samples {
        s.load1m = 76.0;
        s.swap_used_bytes = 35_600_000_000;
        s.pages_free = 13_267;
    }
    set_cpu(&mut samples, 0.98); // the cores really were pegged that night
    let censuses = vec![
        census(0, 7, 45, 49.2, 78_000),
        census(300, 7, 45, 49.2, 78_000),
        census(600, 7, 45, 49.2, 78_000),
    ];
    let p = evaluate(at(600), &samples, &censuses, &cfg());

    assert_eq!(p.level, Level::Shrieking);
    assert_eq!(p.glyph.chars().next().unwrap().to_string(), "💀");

    // The worklist, in measured-impact order: sessions, then orphans, then apps.
    let actions = p.actions();
    assert_eq!(
        actions.first(),
        Some(&Action::ReapStaleSessions),
        "got {actions:?}"
    );
    assert!(actions.contains(&Action::ReapOrphans));
    assert!(actions.contains(&Action::RelaunchApps));
    // And the last resort is last.
    assert_eq!(actions.last(), Some(&Action::Reboot), "got {actions:?}");
}

/// At Shrieking with swap pinned, the reboot advice appears — and says WHY, since
/// only a reboot truly resets a saturated compressor pool.
#[test]
fn a_pinned_swap_at_shrieking_recommends_a_reboot_last() {
    let mut samples = healthy(41, 600);
    for s in &mut samples {
        s.load1m = 76.0;
        s.swap_used_bytes = 35_600_000_000;
    }
    set_cpu(&mut samples, 0.98); // two sustained reds (cpu + swap) reach Shrieking
    let p = evaluate(at(600), &samples, &[], &cfg());
    assert_eq!(p.level, Level::Shrieking);
    let reboot = p
        .findings
        .iter()
        .find(|f| f.action == Action::Reboot)
        .expect("reboot advice");
    assert!(reboot.message.contains("compressor pool"), "{reboot:?}");
    assert_eq!(
        p.findings.last().map(|f| f.action),
        Some(Action::Reboot),
        "the last resort must be last"
    );
}

/// Uptime alone is never a finding. `perf-scan` is explicit that it is material
/// only alongside pinned swap, and a 30-day uptime on its own is not a problem.
/// Mutation-proof: drop the advisory filter in `build_findings` and this fails.
#[test]
fn a_long_uptime_alone_produces_no_finding() {
    let boot_30d_ago = 30 * 86_400;
    let mut samples = healthy(21, boot_30d_ago);
    for s in &mut samples {
        s.boot_time_secs = BOOT;
    }
    let p = evaluate(at(boot_30d_ago), &samples, &[], &cfg());

    let uptime = p.reading(Dimension::Uptime).unwrap();
    assert_eq!(uptime.band, Band::Red, "30 days is past the 21-day line");
    assert!(uptime.advisory);
    assert_eq!(p.level, Level::Quiet, "advisory must not raise the level");
    assert!(
        !p.findings.iter().any(|f| f.dimension == Dimension::Uptime),
        "no finding without memory pressure: {:?}",
        p.findings
    );
}

/// …but alongside swap pressure it becomes worth saying.
#[test]
fn a_long_uptime_with_swap_pressure_does_produce_a_finding() {
    let boot_30d_ago = 30 * 86_400;
    let mut samples = healthy(21, boot_30d_ago);
    for s in &mut samples {
        s.boot_time_secs = BOOT;
        s.swap_used_bytes = 5_000_000_000; // yellow
    }
    let p = evaluate(at(boot_30d_ago), &samples, &[], &cfg());
    assert!(
        p.findings.iter().any(|f| f.dimension == Dimension::Uptime),
        "expected an uptime finding: {:?}",
        p.findings
    );
}

// ---- determinism and the wire ------------------------------------------

/// The model is a pure function of history, so a restarted daemon reaches the
/// same verdict. Mutation-proof: hold state between calls and this fails.
#[test]
fn evaluation_is_deterministic_for_the_same_history() {
    let samples = healthy(21, 300);
    let censuses = vec![healthy_census(0), healthy_census(300)];
    let a = evaluate(at(300), &samples, &censuses, &cfg());
    let b = evaluate(at(300), &samples, &censuses, &cfg());
    assert_eq!(a, b);
}

/// Thresholds are configuration, not constants. Mutation-proof: read a hardcoded
/// threshold anywhere in the model and this fails.
#[test]
fn thresholds_are_configuration() {
    let samples = healthy(21, 300); // 20% busy, comfortably green by default
    let strict = PressureConfig {
        cpu: band::BandSpec::rising(0.05, 0.1, 0.01, 1),
        ..cfg()
    };
    let p = evaluate(at(300), &samples, &[], &strict);
    assert_eq!(
        p.reading(Dimension::Cpu).unwrap().band,
        Band::Red,
        "a stricter config must change the answer"
    );
}

/// The glyph is computed HERE and travels over the wire, so no client re-derives
/// it (ADR-0005).
#[test]
fn the_verdict_carries_the_glyph_and_a_spoken_label() {
    let mut samples = healthy(41, 600);
    set_cpu(&mut samples, 0.98);
    let p = evaluate(at(600), &samples, &[], &cfg());
    assert_eq!(p.glyph, "😱🔥");
    assert_eq!(p.accessibility_label, "Banshee: Wailing, CPU");
    assert_eq!(p.level_name, "Wailing");
    assert!(
        !p.accessibility_label.contains('😱'),
        "VoiceOver must get words"
    );
}

#[test]
fn the_wire_shape_is_camel_case_at_every_depth() {
    let p = evaluate(at(300), &healthy(21, 300), &[healthy_census(300)], &cfg());
    let v = serde_json::to_value(&p).unwrap();

    for key in [
        "evaluatedAt",
        "level",
        "levelName",
        "source",
        "glyph",
        "accessibilityLabel",
        "dimensions",
        "findings",
        "sampleCount",
        "censusCount",
    ] {
        assert!(v.get(key).is_some(), "missing top-level key {key}");
    }
    assert!(v.get("level_name").is_none(), "snake_case leaked");

    let d = &v["dimensions"][0];
    for key in [
        "dimension",
        "key",
        "label",
        "band",
        "value",
        "unit",
        "severity",
        "heldSecs",
        "trendPerSec",
        "detail",
        "advisory",
        "pending",
    ] {
        assert!(
            v["dimensions"][0].get(key).is_some(),
            "missing dimension key {key}"
        );
    }
    assert!(d.get("held_secs").is_none());

    assert_eq!(
        v["evaluatedAt"],
        serde_json::Value::String(crate::wire_time::to_wire(&at(300))),
        "the timestamp is the canonical 6-digit-micros Z form"
    );
}

/// Nullable fields are present-as-null, never absent — this binds the ENCODER.
/// A green machine's `source` is an explicit null, not a missing key.
#[test]
fn absent_values_serialize_as_explicit_null() {
    let p = evaluate(at(300), &healthy(21, 300), &[], &cfg());
    let v = serde_json::to_value(&p).unwrap();
    assert!(v.get("source").is_some(), "key must be present");
    assert!(v["source"].is_null(), "and explicitly null when green");

    let d = &v["dimensions"][0];
    assert!(d.get("pending").is_some());
    assert!(d["pending"].is_null());
}

/// Every level, band, glyph and finding must survive the round trip, or the Swift
/// client silently fails to decode a state it has never seen.
///
/// **Floats are compared with a tolerance, deliberately.** `serde_json` does NOT
/// round-trip `f64` bit-exactly: `0.000011574074074074077` serialises correctly
/// and parses back one ULP lower (reproduced in isolation, 2026-08-31). So exact
/// `==` on a decoded float is an invalid assertion for this wire format — the
/// same class of tolerance the datetime contract already documents for 7+
/// fractional digits. See docs/wire-format.md.
#[test]
fn the_verdict_round_trips_through_json() {
    let mut samples = healthy(41, 600);
    for s in &mut samples {
        s.load1m = 76.0;
        s.swap_used_bytes = 35_600_000_000;
    }
    set_cpu(&mut samples, 0.98); // a red CPU dimension in the round trip too
    let p = evaluate(at(600), &samples, &[healthy_census(600)], &cfg());
    let json = serde_json::to_string(&p).unwrap();
    let back: Pressure = serde_json::from_str(&json).unwrap();

    // Everything discrete must be identical.
    assert_eq!(back.level, p.level);
    assert_eq!(back.level_name, p.level_name);
    assert_eq!(back.source, p.source);
    assert_eq!(back.glyph, p.glyph);
    assert_eq!(back.accessibility_label, p.accessibility_label);
    assert_eq!(
        back.evaluated_at, p.evaluated_at,
        "datetimes ARE exact here"
    );
    assert_eq!(back.findings, p.findings, "findings carry no floats");
    assert_eq!(back.sample_count, p.sample_count);
    assert_eq!(back.census_count, p.census_count);
    assert_eq!(back.dimensions.len(), p.dimensions.len());

    for (b, o) in back.dimensions.iter().zip(&p.dimensions) {
        assert_eq!(b.dimension, o.dimension);
        assert_eq!(b.band, o.band);
        assert_eq!(b.unit, o.unit);
        assert_eq!(b.held_secs, o.held_secs);
        assert_eq!(b.detail, o.detail);
        assert_eq!(b.advisory, o.advisory);
        assert_eq!(b.pending, o.pending);
        // Floats: equal to within a relative epsilon, not bit-for-bit.
        assert!(close(b.value, o.value), "{:?} value", b.dimension);
        assert!(close(b.severity, o.severity), "{:?} severity", b.dimension);
        match (b.trend_per_sec, o.trend_per_sec) {
            (Some(x), Some(y)) => assert!(close(x, y), "{:?} trend", b.dimension),
            (None, None) => {}
            other => panic!("{:?} trend presence differs: {other:?}", b.dimension),
        }
    }
}

/// Relative comparison with an absolute floor, so tiny values (a trend of
/// 1.15e-5) are not judged by an absolute epsilon they can never meet.
fn close(a: f64, b: f64) -> bool {
    if a == b {
        return true;
    }
    let diff = (a - b).abs();
    diff <= f64::EPSILON * a.abs().max(b.abs()).max(1.0) * 4.0
}

// ---- against the real machine ------------------------------------------

/// The model must run against real readings and produce a coherent verdict.
/// Invariants only, so it holds in any load condition.
#[test]
fn a_real_evaluation_is_coherent() {
    use crate::collector::Collector;
    use crate::sys::RealProbe;

    let c = Collector::with_defaults(RealProbe);
    let now = Utc::now();
    // Two real readings a moment apart, so the rate dimensions have a pair.
    let a = c.collect_at(now - chrono::Duration::seconds(15)).unwrap();
    let b = c.collect_at(now).unwrap();

    let p = evaluate(now, &[a, b], &[], &cfg());

    assert_ne!(p.level, Level::Checking, "two samples is enough");
    assert!(!p.glyph.is_empty());
    assert!(p.accessibility_label.starts_with("Banshee: "));
    // Every reading must be internally consistent.
    for r in &p.dimensions {
        assert!(r.value.is_finite(), "{:?} value {}", r.dimension, r.value);
        assert!(r.severity.is_finite(), "{:?} severity", r.dimension);
        assert!(!r.detail.is_empty(), "{:?} has no detail", r.dimension);
        if r.band == Band::Green {
            assert!(
                r.severity < 1.0,
                "{:?} green but severity {}",
                r.dimension,
                r.severity
            );
        }
    }
    // A source is named exactly when the level is high enough to show one.
    assert_eq!(
        p.source.is_some(),
        p.level.shows_source() && p.source.is_some()
    );
    // Findings only ever describe non-green dimensions.
    for f in &p.findings {
        assert!(f.band > Band::Green, "{f:?}");
        assert!(!f.message.is_empty());
    }
}

// ---- the recovering modifier and the activity summary (ADR-0009) -----------

fn open_episode(d: Dimension, state: episode::EpisodeState, started: i64) -> episode::AlertEpisode {
    episode::AlertEpisode {
        id: Uuid::new_v4(),
        dimension: d,
        started_at: at(started),
        ended_at: None,
        peak: episode::EpisodePeak {
            band: Band::Red,
            level: Level::Wailing,
            severity: 2.0,
            at: at(started),
            message: "was red".into(),
            who_line: None,
        },
        census_at_peak: None,
        suppressed: 0,
        state,
        last_notified_at: Some(at(started)),
        recovering_since: if state == episode::EpisodeState::Recovering {
            Some(at(started + 60))
        } else {
            None
        },
        projection_bracket_secs: None,
    }
}

/// A calm verdict with a dimension inside its episode's down window wears the
/// decoration: 🩹 on the glyph, ", recovering" on the label, the flags set at both
/// depths. Mutation-proof: drop the `push_str` and the glyph stays bare while the
/// flag says recovering — the two surfaces disagree, which is what ADR-0005 forbids.
#[test]
fn a_calm_verdict_with_a_recovering_dimension_is_decorated() {
    let mut p = evaluate(at(600), &healthy(41, 600), &[healthy_census(600)], &cfg());
    assert_eq!(p.level, Level::Quiet, "precondition: calm");
    assert_eq!(p.glyph, "😴");
    p.attach_episodes(
        &[open_episode(
            Dimension::Disk,
            episode::EpisodeState::Recovering,
            0,
        )],
        at(600),
    );
    assert!(p.recovering);
    assert_eq!(p.glyph, "😴🩹");
    assert_eq!(p.accessibility_label, "Banshee: Quiet, recovering");
    assert!(p.reading(Dimension::Disk).unwrap().recovering);
    assert!(!p.reading(Dimension::Cpu).unwrap().recovering);
    assert_eq!(
        p.reading(Dimension::Disk).unwrap().band,
        Band::Green,
        "a modifier, not a band"
    );
    assert_eq!(p.activity.open, 1);
}

/// A FIRING episode is not recovering, and a Recovering one does not decorate a
/// face that already shows a source: at Restless and above something is red now,
/// and the source suffix owns the slot. Mutation-proof: decorate on any level and
/// the Wailing glyph grows a third emoji.
#[test]
fn the_decoration_is_only_for_a_calm_face_and_only_for_recovering() {
    let mut calm = evaluate(at(600), &healthy(41, 600), &[healthy_census(600)], &cfg());
    calm.attach_episodes(
        &[open_episode(
            Dimension::Disk,
            episode::EpisodeState::Firing,
            0,
        )],
        at(600),
    );
    assert!(!calm.recovering, "firing is not recovering");
    assert_eq!(calm.glyph, "😴");
    assert_eq!(calm.activity.open, 1, "but it IS an open episode");

    let mut samples = healthy(41, 600);
    set_cpu(&mut samples, 0.98);
    let mut loud = evaluate(at(600), &samples, &[healthy_census(600)], &cfg());
    assert!(
        loud.level >= Level::Restless,
        "precondition: {:?}",
        loud.level
    );
    let bare = loud.glyph.clone();
    loud.attach_episodes(
        &[open_episode(
            Dimension::Disk,
            episode::EpisodeState::Recovering,
            0,
        )],
        at(600),
    );
    assert!(loud.recovering, "the flag is still honest");
    assert!(loud.reading(Dimension::Disk).unwrap().recovering);
    assert_eq!(loud.glyph, bare, "the face is left to the source suffix");
    assert!(!loud.accessibility_label.ends_with(", recovering"));
}

/// Attaching twice with the same record yields the same verdict — a caller cannot
/// double-decorate. Mutation-proof: drop the `strip_suffix` and the second call
/// yields 😴🩹🩹.
#[test]
fn attaching_the_record_is_idempotent() {
    let mut p = evaluate(at(600), &healthy(41, 600), &[healthy_census(600)], &cfg());
    let record = [open_episode(
        Dimension::Disk,
        episode::EpisodeState::Recovering,
        0,
    )];
    p.attach_episodes(&record, at(600));
    let once = p.clone();
    p.attach_episodes(&record, at(600));
    assert_eq!(p, once);
    // …and an EMPTY record undoes the decoration entirely.
    p.attach_episodes(&[], at(600));
    assert_eq!(p.glyph, "😴");
    assert_eq!(p.accessibility_label, "Banshee: Quiet");
    assert!(!p.recovering);
}

/// `assess` is the three steps in the only correct order: the decoration must see
/// the episodes AFTER this tick's transitions. Here a firing disk episode meets a
/// green disk reading, so it moves to Recovering on THIS tick — and the verdict
/// already says so. Mutation-proof: attach the pre-reconcile record and
/// `recovering` reads false.
#[test]
fn assess_decorates_with_the_post_reconcile_record() {
    let open = open_episode(Dimension::Disk, episode::EpisodeState::Firing, 0);
    let out = assess(
        at(600),
        &healthy(41, 600),
        &[healthy_census(600)],
        std::slice::from_ref(&open),
        &cfg(),
    );
    assert_eq!(out.episodes.len(), 1, "the disk episode moved");
    assert_eq!(out.episodes[0].id, open.id);
    assert_eq!(out.episodes[0].state, episode::EpisodeState::Recovering);
    assert!(out.pressure.recovering, "the verdict saw the transition");
    assert_eq!(out.pressure.glyph, "😴🩹");
    assert!(out.notifications.is_empty(), "nothing to say about a dip");
}

// ---- the who line ----------------------------------------------------------
//
// Findings name the top consumers behind them, from the census the daemon already
// takes. The fixtures below are chosen so that the RIGHT rule and the nearest
// wrong one come apart (the recurring false-pin shape): a count ranking and a size
// ranking disagree at position three; a cumulative-seconds ranking and a
// cumulative-RATE ranking swap positions two and three; the summed per-group
// percentages (46.0) differ from the deduplicated total (29.2).

const FIXTURE_CENSUS: &str = include_str!("../../../../tests/fixtures/census-full.json");

fn full_census() -> Census {
    serde_json::from_str(FIXTURE_CENSUS).expect("the census fixture decodes")
}

fn who_of(d: Dimension, c: &Census, config: &PressureConfig) -> Vec<String> {
    who::attribute(d, c, config)
        .iter()
        .map(ToString::to_string)
        .collect()
}

/// Memory-shaped dimensions rank by RESIDENT SIZE. In the fixture the orphaned
/// helpers (23 processes, 1.6 GB) outnumber Slack (14, 2.1 GB) but are smaller,
/// so a count ranking puts orphans third and a size ranking puts Slack third.
/// Mutation-proof: rank on `count` instead of `measure` and this fails.
#[test]
fn the_memory_who_ranks_by_resident_size_not_process_count() {
    let c = full_census();
    let expected = vec![
        "Chrome ×128 at 8.1 GB".to_string(),
        "IDE agent helpers ×48 at 6.4 GB".to_string(),
        "Slack ×14 at 2.1 GB".to_string(),
    ];
    assert_eq!(who_of(Dimension::Memory, &c, &cfg()), expected);
    // Swap, the kernel's verdict and thrash are the same question — what is
    // resident — and must name the same consumers.
    for d in [
        Dimension::Swap,
        Dimension::KernelPressure,
        Dimension::Thrash,
    ] {
        assert_eq!(who_of(d, &c, &cfg()), expected, "{d:?}");
    }
}

/// CPU FALLBACK is the census's cumulative-CPU RULE — Σ cpu seconds over the
/// longest lifetime in the group — not by raw cumulative seconds. Reached only
/// when there are no recent-rate consumers (`cpu_consumers` empty: first census
/// after a restart), so the who-line names someone rather than nobody
/// (`banshee-aen`). In the fixture `claude` has 62× the CPU seconds of `kiro-cli`
/// but ran 82× longer, so by seconds claude ranks above kiro-cli and by rate it
/// ranks below. The managed agents appear ONCE, as the deduplicated total (7
/// processes, 29.2%), never as the sum of the per-group percentages (8 processes,
/// 46.0%).
/// Mutation-proof: rank on `cpu` instead of `cpu / life`, or sum the per-agent
/// list, and this fails.
#[test]
fn the_cpu_who_uses_the_cumulative_cpu_rule_and_the_deduplicated_total() {
    let mut c = full_census();
    c.cpu_consumers.clear(); // force the cumulative fallback
    let expected = vec![
        "managed agents ×7 at 29% of one core".to_string(),
        "kiro-cli ×1 at 2% of one core".to_string(),
        "claude ×1 at 1% of one core".to_string(),
    ];
    assert_eq!(who_of(Dimension::Cpu, &c, &cfg()), expected);
    // Heat is the work: thermal names the same consumers.
    assert_eq!(who_of(Dimension::Thermal, &c, &cfg()), expected);
    let line = who::who_line(&who::attribute(Dimension::Cpu, &c, &cfg())).unwrap();
    assert!(
        !line.contains("46%") && !line.contains("×8 at"),
        "the per-group percentages must never be summed: {line}"
    );
}

/// CPU prefers the RECENT-RATE lens (`banshee-aen`): the consumers the census
/// computed from the CPU-time delta between the last two censuses, which name
/// what is hot NOW rather than what has the largest lifetime total. The fixture's
/// recent consumers are led by a browser helper the census does not otherwise
/// size for CPU — proving the lens covers ANY process, not just stored agent
/// sessions, which is the whole point of the who-line delta-rate fix. The
/// cumulative fallback would instead name `managed agents` first.
/// Mutation-proof: skip `by_recent_cpu_rate` and the browser helper vanishes.
#[test]
fn the_cpu_who_prefers_the_recent_rate_lens_over_cumulative() {
    let c = full_census();
    let expected = vec![
        "Chrome Helper (Renderer) ×4 at 312% of one core".to_string(),
        "claude ×2 at 88% of one core".to_string(),
        "swift-frontend ×1 at 47% of one core".to_string(),
    ];
    assert_eq!(who_of(Dimension::Cpu, &c, &cfg()), expected);
    // Heat is the same question: thermal names the same recent consumers.
    assert_eq!(who_of(Dimension::Thermal, &c, &cfg()), expected);
    // The cumulative fleet total is NOT what a recent-rate census leads with.
    assert!(
        !who_of(Dimension::Cpu, &c, &cfg())
            .iter()
            .any(|w| w.starts_with("managed agents")),
        "recent-rate must not fall back while it has consumers"
    );
}

/// The recent-rate lens is EMPTY on the first census after a (re)start, and the
/// who-line must not regress to naming nobody — it falls back to the cumulative
/// rule. Distinguishes "measured, empty" (fall back) from "populated" (use it):
/// with consumers present the output is the recent-rate one, with them cleared it
/// is the cumulative one, and the two are different lines.
/// Mutation-proof: return `Some(Vec::new())` from `by_recent_cpu_rate` instead of
/// `None`, and the fallback never fires — this asserts a non-empty fallback line.
#[test]
fn the_cpu_who_falls_back_to_cumulative_when_no_recent_rate() {
    let mut c = full_census();
    let recent = who_of(Dimension::Cpu, &c, &cfg());
    c.cpu_consumers.clear();
    let fallback = who_of(Dimension::Cpu, &c, &cfg());
    assert_ne!(
        recent, fallback,
        "the two lenses must produce different lines"
    );
    assert!(
        fallback.iter().any(|w| w.starts_with("managed agents")),
        "the fallback must still name someone: {fallback:?}"
    );
}

/// A per-program cumulative-CPU ratio over a short lifetime is a startup burst,
/// not a rate, so a group younger than `managed_agents_min_life_secs` is not named —
/// even when it would top the list. Mutation-proof: drop the `life >= min_life`
/// filter and the 90% newborn leads.
#[test]
fn the_cpu_who_skips_a_group_too_young_for_the_ratio_to_mean_anything() {
    let mut c = full_census();
    c.cpu_consumers.clear(); // the young-group guard lives in the cumulative fallback
    c.agent_sessions.push(crate::census::AgentSession {
        pid: 99,
        program: "newborn".into(),
        rss_bytes: 1,
        age_secs: 60,
        age_days: 0,
        cpu_secs: 54.0, // 90% of one core — over 60 seconds
        tty: "ttys099".into(),
        is_stale: false,
        cwd: None,
    });
    let who = who_of(Dimension::Cpu, &c, &cfg());
    assert!(
        !who.iter().any(|w| w.starts_with("newborn")),
        "a 60s-old group must not be named for CPU: {who:?}"
    );
    // Same guard on the managed-agent total: a restarted fleet says nothing.
    c.monitor_total.longest_life_secs = 109;
    let who = who_of(Dimension::Cpu, &c, &cfg());
    assert!(
        !who.iter().any(|w| w.starts_with("managed agents")),
        "{who:?}"
    );
}

/// The stale-sessions who names STALE sessions only, grouped by program and sized
/// by the RSS reaping them frees. The fixture has one stale `claude` and one live
/// `kiro-cli`. Mutation-proof: drop the `is_stale` filter and kiro-cli appears.
#[test]
fn the_stale_sessions_who_names_only_stale_sessions() {
    let c = full_census();
    assert_eq!(
        who_of(Dimension::Agents, &c, &cfg()),
        vec!["claude ×1 at 914 MB".to_string()]
    );
}

/// Orphans follow the census's own breakdown by program, count first — the count
/// is what the dimension bands on.
#[test]
fn the_orphans_who_follows_the_census_breakdown() {
    let c = full_census();
    assert_eq!(
        who_of(Dimension::Orphans, &c, &cfg()),
        vec![
            "mcp-server-fetch ×14 at 1.1 GB".to_string(),
            "language-server ×9 at 537 MB".to_string(),
        ]
    );
}

/// The managed-agent who ranks groups by their OWN rate (never summed) and skips a
/// group too young to have a rate: the fixture's log shipper is 109 s old at a
/// startup-burst 14.6%. Mutation-proof: drop the lifetime filter and it appears.
#[test]
fn the_managed_agents_who_ranks_groups_by_their_own_rate_and_skips_young_ones() {
    let c = full_census();
    assert_eq!(
        who_of(Dimension::ManagedAgents, &c, &cfg()),
        vec!["endpoint-shield ×6 at 31% of one core".to_string()]
    );
}

/// Disk and uptime have no process behind them (ADR-0003: what is taking the
/// space is a disk tool's question), so they name nobody and carry no line.
#[test]
fn disk_and_uptime_name_nobody() {
    let c = full_census();
    for d in [Dimension::Disk, Dimension::Uptime] {
        let who = who::attribute(d, &c, &cfg());
        assert!(who.is_empty(), "{d:?}: {who:?}");
        assert_eq!(who::who_line(&who), None);
    }
}

/// `who_limit` is load-bearing: the fixture has seven memory-shaped candidates,
/// so 2 and 5 must produce 2 and 5. Mutation-proof: `take(3)` and both fail.
#[test]
fn the_who_limit_is_load_bearing_config() {
    let c = full_census();
    for limit in [2usize, 5] {
        let config = PressureConfig {
            who_limit: limit,
            ..cfg()
        };
        assert_eq!(
            who::attribute(Dimension::Memory, &c, &config).len(),
            limit,
            "who_limit {limit}"
        );
    }
}

/// The line's wording, pinned: `who: ` then the consumers comma-joined, each
/// `name ×count at detail`. Every surface prints this verbatim (ADR-0005).
#[test]
fn the_who_line_is_worded_once() {
    let consumers = vec![
        who::Consumer {
            name: "Chrome".into(),
            count: 115,
            detail: "7.7 GB".into(),
        },
        who::Consumer {
            name: "claude".into(),
            count: 8,
            detail: "2.1 GB".into(),
        },
    ];
    assert_eq!(
        who::who_line(&consumers).as_deref(),
        Some("who: Chrome ×115 at 7.7 GB, claude ×8 at 2.1 GB")
    );
    assert_eq!(
        who::who_line(&[]),
        None,
        "nobody to name → no line, not `who:`"
    );
}

/// Findings read the NEWEST census. Two censuses with different app groups: the
/// swap finding must name the newer one. Mutation-proof: `censuses.first()` and
/// this fails.
#[test]
fn findings_name_who_from_the_newest_census() {
    let mut samples = healthy(21, 600);
    for s in &mut samples {
        s.swap_used_bytes = 20_000_000_000; // red
    }
    let app = |name: &str| crate::census::AppGroup {
        name: name.into(),
        proc_count: 40,
        rss_bytes: 5_000_000_000,
    };
    let mut older = healthy_census(300);
    older.app_groups = vec![app("OlderBrowser")];
    let mut newer = healthy_census(600);
    newer.app_groups = vec![app("NewerBrowser")];
    let p = evaluate(at(600), &samples, &[older, newer], &cfg());
    let swap = p
        .findings
        .iter()
        .find(|f| f.dimension == Dimension::Swap)
        .expect("a swap finding");
    let line = swap.who_line.as_deref().expect("a who line");
    assert!(line.contains("NewerBrowser ×40 at 5.0 GB"), "{line}");
    assert!(!line.contains("OlderBrowser"), "{line}");
    assert_eq!(
        swap.who.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(),
        vec!["NewerBrowser", "managed agents", "orphaned helpers"],
        "the fixture census's three sized groups, biggest first: {:?}",
        swap.who
    );
}

/// No census yet → the finding still exists, and names nobody honestly.
#[test]
fn a_finding_without_a_census_names_nobody() {
    let mut samples = healthy(21, 300);
    for s in &mut samples {
        s.swap_used_bytes = 20_000_000_000;
    }
    let p = evaluate(at(300), &samples, &[], &cfg());
    let swap = p
        .findings
        .iter()
        .find(|f| f.dimension == Dimension::Swap)
        .expect("a swap finding");
    assert!(swap.who.is_empty());
    assert_eq!(swap.who_line, None);
}

/// Yellow findings name who too — yellow is where the cheap action still exists.
/// Mutation-proof: gate attribution on `band == Red` and this fails.
#[test]
fn a_yellow_finding_names_who_too() {
    let mut samples = healthy(21, 300);
    for s in &mut samples {
        s.swap_used_bytes = 6_000_000_000; // past the 4 GB yellow line, under red
    }
    let mut c = healthy_census(300);
    c.app_groups = vec![crate::census::AppGroup {
        name: "Chrome".into(),
        proc_count: 115,
        rss_bytes: 7_710_000_000,
    }];
    let p = evaluate(at(300), &samples, &[c], &cfg());
    let swap = p
        .findings
        .iter()
        .find(|f| f.dimension == Dimension::Swap)
        .expect("a swap finding");
    assert_eq!(swap.band, Band::Yellow);
    assert!(
        swap.who_line
            .as_deref()
            .is_some_and(|l| l.starts_with("who: Chrome ×115 at 7.7 GB")),
        "{:?}",
        swap.who_line
    );
}

// ---- the shared fixtures (raw bytes, not our own encoder) ----------------
//
// Parser tests start from RAW BYTES on purpose (Testing standard): round-tripping
// a value through our own encoder cannot catch casing or format drift, because
// both sides of the comparison share the bug. These same files are decoded by the
// Swift suite (`WireFormatTests`) and by `tests/e2e/run-e2e.sh`, so a change to
// the pressure wire format breaks both languages' tests in the same commit.

const FIXTURE_CHECKING: &str = include_str!("../../../../tests/fixtures/pressure-checking.json");
const FIXTURE_QUIET: &str = include_str!("../../../../tests/fixtures/pressure-quiet.json");
const FIXTURE_SHRIEKING: &str = include_str!("../../../../tests/fixtures/pressure-shrieking.json");
const FIXTURE_JETSAM: &str = include_str!("../../../../tests/fixtures/pressure-jetsam.json");
const FIXTURE_EPISODE: &str = include_str!("../../../../tests/fixtures/alert-episode.json");
const FIXTURE_DELTAS_NORMAL: &str = include_str!("../../../../tests/fixtures/deltas-normal.json");
const FIXTURE_DELTAS_GAP: &str = include_str!("../../../../tests/fixtures/deltas-gap.json");
const FIXTURE_DELTAS_EPISODE: &str = include_str!("../../../../tests/fixtures/deltas-episode.json");

#[test]
fn decodes_the_checking_fixture_from_raw_bytes() {
    let p: Pressure = serde_json::from_str(FIXTURE_CHECKING).unwrap();
    assert_eq!(p.level, Level::Checking);
    assert_eq!(p.level_name, "Checking");
    assert_eq!(p.source, None);
    assert_eq!(p.glyph, "🫧");
    assert!(p.dimensions.is_empty());
    assert!(p.findings.is_empty());
    assert_eq!(p.sample_count, 0);
}

/// A green machine: `source`, `trendPerSec` and `pending` are all explicit nulls
/// in the bytes, and must decode as `None` rather than failing.
#[test]
fn decodes_the_quiet_fixture_including_its_explicit_nulls() {
    let p: Pressure = serde_json::from_str(FIXTURE_QUIET).unwrap();
    assert_eq!(p.level, Level::Quiet);
    assert_eq!(p.source, None);
    // No source suffix below Restless — but the RECOVERING decoration (ADR-0009):
    // the disk dimension is green again inside its down window, so the calm face
    // says "calm, and was red a moment ago" rather than just "calm".
    assert_eq!(p.glyph, "😴🩹");
    assert_eq!(p.accessibility_label, "Banshee: Quiet, recovering");
    assert!(p.recovering);
    let disk = p.reading(Dimension::Disk).expect("disk reading");
    assert!(disk.recovering);
    assert_eq!(
        disk.band,
        Band::Green,
        "recovering is a modifier, not a band"
    );
    assert_eq!(
        p.activity,
        crate::pressure::episode::RecentActivity {
            open: 1,
            last_hour: 1,
            last_day: 2
        }
    );
    let cpu = p.reading(Dimension::Cpu).expect("cpu reading");
    assert!(!cpu.recovering);
    assert_eq!(cpu.band, Band::Green);
    assert_eq!(cpu.unit, Unit::Ratio);
    assert_eq!(cpu.trend_per_sec, None);
    assert_eq!(cpu.pending, None);
    assert!(!cpu.advisory);
    assert!(p.findings.is_empty(), "a quiet machine has nothing to say");
}

/// The FULLY-POPULATED fixture: every nullable field carries a real value, so a
/// decoder that quietly ignores one is caught. Nested per-dimension objects are
/// where casing bugs hide (wire-format Rule 3) — top-level keys get tested by
/// accident, nested ones do not.
#[test]
fn decodes_the_shrieking_fixture_at_full_depth() {
    let p: Pressure = serde_json::from_str(FIXTURE_SHRIEKING).unwrap();
    assert_eq!(p.level, Level::Shrieking);
    assert_eq!(p.source, Some(Source::Memory));
    assert_eq!(p.glyph, "💀🧠");
    assert_eq!(p.accessibility_label, "Banshee: Shrieking, memory");
    assert_eq!(p.dimensions.len(), 3);

    // Nested nullables present as REAL values, which is the case a decoder that
    // silently drops unknown/renamed keys would still pass on the quiet fixture.
    let cpu = p.reading(Dimension::Cpu).expect("cpu reading");
    assert_eq!(cpu.band, Band::Red);
    assert_eq!(cpu.pending, Some(Band::Yellow), "a pending de-escalation");
    assert!(close(
        cpu.trend_per_sec.expect("cpu trend"),
        0.004_166_666_666_666_667
    ));
    assert_eq!(cpu.held_secs, 3600);

    // The advisory dimension decodes as advisory. If this flipped, uptime would
    // start driving the level (level.rs excludes advisory dimensions by flag).
    let uptime = p.reading(Dimension::Uptime).expect("uptime reading");
    assert!(uptime.advisory);
    assert_eq!(uptime.unit, Unit::Days);

    // Findings carry the action verbatim, so the CLI and the app offer the same
    // worklist in the same order.
    let actions: Vec<Action> = p.findings.iter().map(|f| f.action).collect();
    assert_eq!(
        actions,
        vec![
            Action::RelaunchApps,
            Action::ReapStaleSessions,
            Action::Reboot
        ]
    );
    assert_eq!(
        p.actions(),
        vec![
            Action::RelaunchApps,
            Action::ReapStaleSessions,
            Action::Reboot
        ],
        "distinct actions, order preserved"
    );

    // The who-line: populated on the swap and cpu findings, and an
    // EXPLICIT null with an empty list on uptime — both branches in one fixture,
    // so a decoder that cannot tell them apart fails here.
    assert_eq!(
        p.findings[0].who_line.as_deref(),
        Some("who: Chrome ×115 at 7.7 GB, claude ×8 at 2.1 GB")
    );
    assert_eq!(
        p.findings[0].who,
        vec![
            who::Consumer {
                name: "Chrome".into(),
                count: 115,
                detail: "7.7 GB".into(),
            },
            who::Consumer {
                name: "claude".into(),
                count: 8,
                detail: "2.1 GB".into(),
            },
        ]
    );
    assert!(
        p.findings[1]
            .who_line
            .as_deref()
            .unwrap()
            .contains("62% of one core")
    );
    assert!(p.findings[2].who.is_empty());
    assert_eq!(p.findings[2].who_line, None);
}

/// The jetsam fixture is the case the swap fixture cannot
/// show: a kernel kill drives Shrieking on its OWN — no sustained catastrophic
/// red anywhere else — so it isolates the jetsam-immediate rule from the m5y
/// ceiling. It also carries the three memory-model changes at the wire level:
/// `memory` demoted to an advisory GREEN row, and `kernelFree`/`compressor` as
/// advisory reds that produce NO findings (only the non-advisory jetsam does).
#[test]
fn decodes_the_jetsam_fixture_at_full_depth() {
    let p: Pressure = serde_json::from_str(FIXTURE_JETSAM).unwrap();
    assert_eq!(p.level, Level::Shrieking);
    assert_eq!(p.source, Some(Source::Memory));

    // Jetsam: the driver — red and NOT advisory (a kill rings the bell).
    let jetsam = p.reading(Dimension::Jetsam).expect("jetsam reading");
    assert_eq!(jetsam.band, Band::Red);
    assert!(!jetsam.advisory, "a jetsam kill must drive the level");
    assert_eq!(jetsam.unit, Unit::Count);

    // Availability is demoted to advisory and reads GREEN even at Shrieking —
    // the whole point of yk8: the value macOS holds stable never bands, so it
    // must not be what decides the level.
    let memory = p.reading(Dimension::Memory).expect("memory reading");
    assert_eq!(memory.band, Band::Green);
    assert!(
        memory.advisory,
        "availability is informational, not a driver"
    );

    // The two new advisory signals decode as advisory, so they never alert.
    assert!(p.reading(Dimension::KernelFree).unwrap().advisory);
    assert_eq!(
        p.reading(Dimension::KernelFree).unwrap().unit,
        Unit::Percent
    );
    assert!(p.reading(Dimension::Compressor).unwrap().advisory);
    assert_eq!(p.reading(Dimension::Compressor).unwrap().unit, Unit::Bytes);

    // Only the non-advisory jetsam red is a finding: the advisory kernel-free
    // and compressor reds point at the same lever swap would and are muted.
    assert_eq!(
        p.findings.len(),
        1,
        "advisory reds are informational, not findings"
    );
    assert_eq!(p.findings[0].dimension, Dimension::Jetsam);
    assert_eq!(p.findings[0].action, Action::RelaunchApps);
}

/// banshee-714: the jetsam counter only sees the SUSTAINED-PRESSURE kill path
/// (`kern.memorystatus.kill_on_sustained_pressure_count`). On 2026-09-15 the
/// kernel killed ~170 processes via idle-exit (`rf:high`) and one via a
/// per-process-limit, and this counter stayed 0 the whole time — so the
/// zero-case detail must NOT say "no kills", which reads as "nothing was
/// killed". It says "no sustained-pressure kills" so the limitation is on the
/// wire, not just in a doc. (The honest cross-path counter is banshee-2dq.)
#[test]
fn the_zero_jetsam_detail_scopes_itself_to_sustained_pressure() {
    let detail = detail_for(Dimension::Jetsam, 0.0, None, None, None, &cfg());
    assert!(
        detail.contains("sustained-pressure"),
        "a zero must name the ONE kill path it counts, not imply none happened: {detail:?}"
    );
    assert_ne!(
        detail, "no kills this window",
        "the bare reassurance is the banshee-714 lie: it was 0 while 170 processes died"
    );

    // A positive count still reads as a confirmed kill under memory pressure.
    let killed = detail_for(Dimension::Jetsam, 2.0, None, None, None, &cfg());
    assert!(
        killed.contains('2') && killed.contains("memory pressure"),
        "got {killed:?}"
    );
}

/// banshee-cwl helper: a DimensionReading with just the fields the render
/// functions read, so `detail_for` / `message_for` can be driven at the seam.
fn dr(dimension: Dimension, value: f64, trend: Option<f64>) -> DimensionReading {
    DimensionReading {
        dimension,
        key: dimension.key().to_string(),
        label: dimension.label().to_string(),
        band: Band::Red,
        value,
        unit: dimension.unit(),
        severity: 2.0,
        held_secs: 0,
        observation_gap_secs: None,
        trend_per_sec: trend,
        detail: String::new(),
        advisory: dimension.is_advisory(),
        pending: None,
        recovering: false,
    }
}

/// banshee-cwl part 2: the swap detail reports how full the ALLOCATED POOL is
/// (used/total), which no other surface said. Without the total it must NOT
/// invent a percentage — the co-varying trap is a fixed pool size that happens
/// to match the value.
#[test]
fn swap_detail_names_pool_fill_only_when_the_total_is_known() {
    // 96 of a 100-byte pool in use -> "pool 96% full".
    let with_total = detail_for(Dimension::Swap, 96.0, None, None, Some(100), &cfg());
    assert!(with_total.contains("pool 96% full"), "got {with_total:?}");

    // No pool total (pre-sample, or swap off): the fill clause is absent, not 0%.
    let without = detail_for(Dimension::Swap, 96.0, None, None, None, &cfg());
    assert!(
        !without.contains("pool"),
        "must not fabricate a fill%: {without:?}"
    );
    assert!(!without.contains('%'), "got {without:?}");
}

/// banshee-cwl part 1: the disk finding names the swap that lives on the same
/// volume — but ONLY when the pool rivals what is free, which is the spiral. On
/// a roomy volume the coupling is noise and must stay silent.
#[test]
fn disk_finding_names_swap_only_when_it_rivals_free_space() {
    let disk = dr(Dimension::Disk, 12.0, None); // 12 bytes free

    // 24-byte pool on a volume with 12 free: swap >= free, name it.
    let coupled = message_for(&disk, Some(24));
    assert!(coupled.contains("of the volume is swap"), "got {coupled:?}");

    // A small pool on the same low volume: real, but not the story — stay quiet.
    let roomy = message_for(&disk, Some(6));
    assert!(
        !roomy.contains("swap"),
        "6<12, coupling is not material: {roomy:?}"
    );

    // No total at all: never claim a coupling.
    let unknown = message_for(&disk, None);
    assert!(
        !unknown.contains("of the volume is swap"),
        "got {unknown:?}"
    );
}

/// banshee-cwl part 3: swap gets a projection to its OWN ceiling (the allocated
/// pool), not the volume's. It appears only while climbing; a flat pool has no
/// exhaustion time.
#[test]
fn swap_finding_projects_pool_exhaustion_while_climbing() {
    // 96 of 100 used, climbing 1 byte/sec -> 4 bytes headroom -> ~4s to full.
    let climbing = message_for(&dr(Dimension::Swap, 96.0, Some(1.0)), Some(100));
    assert!(
        climbing.contains("allocated pool is 96% full"),
        "got {climbing:?}"
    );
    assert!(climbing.contains("exhausted in"), "got {climbing:?}");

    // Not climbing: fill% still stated, but no false "exhausted in X".
    let flat = message_for(&dr(Dimension::Swap, 96.0, None), Some(100));
    assert!(flat.contains("allocated pool is 96% full"), "got {flat:?}");
    assert!(
        !flat.contains("exhausted in"),
        "a flat pool has no projection: {flat:?}"
    );
}

/// A finding from a daemon that predates the who-line decodes with an empty
/// list and no line — the fields default rather than fail, so an older stored
/// peak or a mixed-version pair never blanks a verdict.
#[test]
fn a_finding_without_who_fields_decodes_to_nobody() {
    let raw = r#"{"dimension":"swap","band":"red","message":"6 GB of swap in use.",
        "action":"relaunchApps","actionLabel":"Quit and relaunch the biggest apps"}"#;
    let f: Finding = serde_json::from_str(raw).unwrap();
    assert!(f.who.is_empty());
    assert_eq!(f.who_line, None);
}

/// The episode fixture is the 2026-09-05 thrash afternoon as ONE record: a
/// closed episode that peaked at Shrieking and absorbed 39 re-fires — the
/// "one storm or forty" question answered from a single row (ADR-0009).
/// Every nullable carries a value, so a decoder that drops one is caught.
#[test]
fn decodes_the_episode_fixture_from_raw_bytes() {
    use crate::pressure::episode::{AlertEpisode, EpisodeState};
    let e: AlertEpisode = serde_json::from_str(FIXTURE_EPISODE).unwrap();
    assert_eq!(e.dimension, Dimension::Thrash);
    assert_eq!(e.state, EpisodeState::Closed);
    assert!(!e.is_open());
    assert_eq!(e.peak.band, Band::Red);
    assert_eq!(e.peak.level, Level::Shrieking);
    assert!(close(e.peak.severity, 3.494));
    assert_eq!(e.suppressed, 39);
    assert_eq!(
        e.started_at,
        crate::wire_time::from_wire("2026-09-05T17:24:03.870112Z").unwrap()
    );
    assert_eq!(
        e.ended_at,
        Some(crate::wire_time::from_wire("2026-09-05T21:40:12.000000Z").unwrap())
    );
    assert_eq!(
        e.recovering_since, e.ended_at,
        "closed at the moment it dropped below red"
    );
    assert!(e.last_notified_at.is_some());
    // Who was behind it at the peak, kept on the record after the census
    // that knew has been swept.
    assert_eq!(
        e.peak.who_line.as_deref(),
        Some("who: Chrome ×102 at 8.9 GB, claude ×10 at 2.1 GB")
    );
    // Re-encoding must reproduce the fixture's key structure — both languages
    // decode these bytes, and the Swift encoder key-set test pins the same list.
    let re = serde_json::to_value(&e).unwrap();
    let original: serde_json::Value = serde_json::from_str(FIXTURE_EPISODE).unwrap();
    assert_eq!(key_shape(&re), key_shape(&original));

    // The episode fixture now carries a `censusAtPeak`: the who-line's
    // inputs frozen at the worst moment, so a delta can still answer "what changed
    // since it started" after the raw censuses are swept. Every nested key is
    // pinned by the round-trip above; here we check the values decoded.
    let peak = e.census_at_peak.as_ref().expect("censusAtPeak populated");
    assert_eq!(peak.total_procs, 842);
    assert_eq!(peak.orphan_count, 23);
    assert_eq!(peak.consumers.len(), 3);
    assert_eq!(peak.consumers[0].name, "Chrome");
    assert_eq!(
        peak.consumers[0].kind,
        crate::pressure::deltas::ConsumerKind::AppGroup
    );
    assert_eq!(peak.consumers[0].rss_bytes, 8_900_000_000);
    assert!(close(peak.monitor_percent_of_one_core, 29.183));
}

/// An episode stored before deltas carries no `censusAtPeak`, and that must decode as `None`
/// rather than failing — Sparkle makes stored-record skew inevitable, and a
/// missing peak census must never make an old incident undecodable.
#[test]
fn an_episode_without_a_peak_census_decodes_to_none() {
    use crate::pressure::episode::AlertEpisode;
    let raw = r#"{
        "id":"3f9c1b2e-7a4d-4c8f-9e10-5b6a2d3c4e5f","dimension":"swap",
        "startedAt":"2026-09-05T17:24:03.870112Z","endedAt":null,
        "peak":{"band":"red","level":"wailing","severity":1.2,
            "at":"2026-09-05T18:00:00.000000Z","message":"m","whoLine":null},
        "suppressed":0,"state":"firing","lastNotifiedAt":null,"recoveringSince":null
    }"#;
    let e: AlertEpisode = serde_json::from_str(raw).unwrap();
    assert!(e.census_at_peak.is_none());
}

/// The delta fixtures decode at full depth, and re-encoding reproduces the exact
/// key structure — the cross-LANGUAGE pin (the Swift suite decodes the same
/// bytes) and the encoder-match pin in one. `deltas-normal.json` is the "why did
/// freeing 4 GB not help" story: Chrome freed 4 GB while claude grew, swap STILL
/// grew, and the verdict did not improve.
#[test]
fn decodes_the_delta_fixtures_at_full_depth() {
    use crate::pressure::deltas::{ConsumerKind, Deltas};

    let normal: Deltas = serde_json::from_str(FIXTURE_DELTAS_NORMAL).unwrap();
    assert_eq!(normal.lookback, "1h");
    assert_eq!(normal.episode_id, None);
    let verdict = normal
        .verdict
        .as_ref()
        .expect("a trustworthy anchor verdict");
    assert_eq!(verdict.now_level, Level::Restless);
    assert_eq!(
        verdict.now_source,
        Some(Source::Memory),
        "a populated source"
    );
    // The mover ranks by absolute change: Chrome freed the most, so it leads.
    assert_eq!(normal.consumers[0].name, "Chrome");
    assert_eq!(normal.consumers[0].kind, ConsumerKind::AppGroup);
    assert!(normal.consumers[0].delta_bytes < 0, "Chrome was freed");
    let claude = normal
        .consumers
        .iter()
        .find(|c| c.name == "claude")
        .unwrap();
    assert!(claude.delta_bytes > 0, "claude grew");
    assert_eq!(claude.kind, ConsumerKind::Sessions);
    assert!(normal.census.is_some());
    assert_eq!(normal.observation_gap_secs, None, "observed continuously");
    assert!(
        normal.summary.contains("still"),
        "freeing did not help: {}",
        normal.summary
    );

    // The gap fixture: a hole nobody watched across, reported at the top and per
    // dimension, and the sentence says the arithmetic spans it.
    let gap: Deltas = serde_json::from_str(FIXTURE_DELTAS_GAP).unwrap();
    assert_eq!(gap.observation_gap_secs, Some(1200));
    assert!(
        gap.dimensions
            .iter()
            .all(|d| d.observation_gap_secs == Some(1200))
    );
    assert!(gap.consumers.is_empty(), "no censuses in reach");
    assert!(gap.census.is_none());
    assert!(
        gap.summary.contains("spans a hole"),
        "sentence: {}",
        gap.summary
    );

    // The episode anchor: compared against a stored peak census, so `episodeId`
    // is present and the sentence is about the incident.
    let episode: Deltas = serde_json::from_str(FIXTURE_DELTAS_EPISODE).unwrap();
    assert_eq!(episode.lookback, "episode");
    assert!(episode.episode_id.is_some());
    assert!(episode.summary.starts_with("since the episode opened"));

    for (name, raw) in [
        ("deltas-normal.json", FIXTURE_DELTAS_NORMAL),
        ("deltas-gap.json", FIXTURE_DELTAS_GAP),
        ("deltas-episode.json", FIXTURE_DELTAS_EPISODE),
    ] {
        let decoded: Deltas = serde_json::from_str(raw).unwrap();
        let re_encoded = serde_json::to_value(&decoded).unwrap();
        let original: serde_json::Value = serde_json::from_str(raw).unwrap();
        assert_eq!(
            key_shape(&re_encoded),
            key_shape(&original),
            "{name}: the encoder's key structure differs from the fixture's"
        );
    }
}

/// The fixtures must be what our own encoder produces, not merely something it
/// can read. A fixture that has drifted from the encoder is a contract nobody is
/// enforcing — and both languages consume these bytes.
///
/// Mutation-proof: rename any key in a fixture (`heldSecs` → `held_secs`) and
/// this fails, because the re-encoded key set no longer matches.
#[test]
fn the_fixtures_match_what_the_encoder_emits() {
    for (name, raw) in [
        ("pressure-checking.json", FIXTURE_CHECKING),
        ("pressure-quiet.json", FIXTURE_QUIET),
        ("pressure-shrieking.json", FIXTURE_SHRIEKING),
        ("pressure-jetsam.json", FIXTURE_JETSAM),
    ] {
        let decoded: Pressure = serde_json::from_str(raw).unwrap();
        let re_encoded = serde_json::to_value(&decoded).unwrap();
        let original: serde_json::Value = serde_json::from_str(raw).unwrap();
        assert_eq!(
            key_shape(&re_encoded),
            key_shape(&original),
            "{name}: the encoder's key structure differs from the fixture's"
        );
    }
}

/// Every key path in a JSON value, sorted. Compares STRUCTURE without comparing
/// float bytes — which the wire format explicitly does not guarantee.
fn key_shape(v: &serde_json::Value) -> Vec<String> {
    fn walk(v: &serde_json::Value, path: &str, out: &mut Vec<String>) {
        match v {
            serde_json::Value::Object(map) => {
                for (k, child) in map {
                    let p = format!("{path}.{k}");
                    out.push(p.clone());
                    walk(child, &p, out);
                }
            }
            serde_json::Value::Array(items) => {
                for (i, child) in items.iter().enumerate() {
                    walk(child, &format!("{path}[{i}]"), out);
                }
            }
            _ => {}
        }
    }
    let mut out = Vec::new();
    walk(v, "", &mut out);
    out.sort();
    out
}

// ---- the Checking placeholder --------------------------------------------

/// `AppState.pressure` is `None` until the sampler's first evaluation, and the
/// route renders `Pressure::checking`. That placeholder must be EXACTLY what the
/// model says about an empty world — not a hand-written level.
///
/// This is the pin that stops the placeholder drifting into a confident `Quiet`.
/// Mutation-proof, verified: hardcode `Level::Quiet` in `Pressure::checking` and
/// this fails on the first assertion; change `sample_count: 0` to `1` and it fails
/// on the whole-value comparison.
///
/// **The first mutation tried on it did NOT distinguish anything**, and it is worth
/// recording which: flipping `decide(&[], 0, 0.0, false)` to `decide(&[], 0, 0.0, true)`
/// inside `Pressure::checking` changes nothing, because `decide` returns `Checking`
/// for an empty contribution slice either way — the `scoring.is_empty()` guard gets
/// there before `have_data` matters. Two co-varying properties again (the fifth
/// occurrence of that shape here). The mutation that bites is the one that names a
/// level directly, because naming a level is the mistake this function exists to
/// prevent.
#[test]
fn the_checking_placeholder_is_what_the_model_says_about_no_data() {
    let now = at(0);
    let placeholder = Pressure::checking(now);
    let from_model = evaluate(now, &[], &[], &cfg());

    assert_eq!(placeholder.level, from_model.level);
    assert_eq!(placeholder.level_name, from_model.level_name);
    assert_eq!(placeholder.glyph, from_model.glyph);
    assert_eq!(placeholder.source, from_model.source);
    assert_eq!(
        placeholder.accessibility_label,
        from_model.accessibility_label
    );
    assert_eq!(
        placeholder, from_model,
        "the whole verdict, not just the level"
    );
}

/// And it is emphatically NOT Quiet. Stated as its own assertion because this is
/// the one confusion the level scale exists to prevent: "we have not looked yet"
/// and "nothing is wrong" are different answers, and only one of them is safe to
/// show in a menu bar.
#[test]
fn the_checking_placeholder_is_never_quiet() {
    let p = Pressure::checking(at(0));
    assert_eq!(p.level, Level::Checking);
    assert_ne!(p.level, Level::Quiet);
    assert_ne!(p.glyph, Level::Quiet.glyph());
    assert_eq!(p.sample_count, 0, "it has seen nothing");
    assert!(p.findings.is_empty(), "and so has nothing to recommend");
}

/// Every finding carries its action's WORDING, not just the enum.
///
/// The label travels over the wire for the same reason the glyph does: the
/// alternative is a Rust `match` and a Swift `switch` kept in step by hand, and the
/// failure mode is a new `Action` variant rendering as blank text in whichever
/// client forgot to add a case.
///
/// Mutation-proof, verified: set `action_label` to `String::new()` in
/// `build_findings` and this fails; set it to a hardcoded string and the equality
/// against `action.label()` fails.
#[test]
fn every_finding_carries_its_actions_wording() {
    // A machine in real trouble, so the worklist has several distinct actions:
    // stale sessions, orphaned helpers, and a managed-agent group over its line.
    let censuses = vec![
        census(0, 7, 45, 49.2, 78_000),
        census(300, 7, 45, 49.2, 78_000),
    ];
    let p = evaluate(at(300), &healthy(21, 300), &censuses, &cfg());
    assert!(
        !p.findings.is_empty(),
        "precondition: something must be wrong"
    );
    assert!(
        p.actions().len() >= 2,
        "precondition: several distinct actions, got {:?}",
        p.actions()
    );

    for f in &p.findings {
        assert_eq!(
            f.action_label,
            f.action.label(),
            "{:?} carried the wrong wording",
            f.action
        );
        assert!(!f.action_label.is_empty());
    }

    // And it survives the wire, because that is the only reason it exists.
    let v = serde_json::to_value(&p).unwrap();
    assert!(
        v["findings"][0].get("actionLabel").is_some(),
        "actionLabel must be camelCase on the wire: {:#?}",
        v["findings"][0]
    );
    assert!(v["findings"][0].get("action_label").is_none());
}

/// Every `Action` variant has a distinct, non-empty label, so no variant can reach
/// a client as blank text. Mutation-proof: give two variants the same label and
/// this fails.
#[test]
fn every_action_has_a_distinct_label() {
    let actions = [
        Action::ReapStaleSessions,
        Action::ReapOrphans,
        Action::RelaunchApps,
        Action::Reboot,
        Action::OpenDiskTool,
        Action::None,
    ];
    let mut labels: Vec<&str> = actions.iter().map(|a| a.label()).collect();
    let n = labels.len();
    labels.sort_unstable();
    labels.dedup();
    assert_eq!(labels.len(), n, "action labels must be distinct");
    for a in actions {
        assert!(!a.label().is_empty(), "{a:?} has no label");
    }
}

/// The glyph and the spoken label must agree about whether a source is named.
///
/// They are computed by two functions that each re-apply the same
/// `shows_source()` filter, so they can drift apart in a way no single-field
/// assertion notices: a menu bar reading 😧 while VoiceOver says "Restless,
/// memory" is one bug, and a `source` field that contradicts the glyph is
/// another.
///
/// This exists because a hand probe against a STALE `target/debug` binary showed
/// exactly that contradiction — `source: "memory"` beside a suffix-less glyph —
/// and it took several minutes to establish that the library was fine and the
/// binary was old. An invariant asserted in both the unit suite and the e2e
/// harness makes that diagnosis immediate.
///
/// Mutation-proof: drop the `.filter(...)` from `Verdict::glyph` and the
/// below-threshold case gains a suffix the label does not have.
#[test]
fn the_glyph_and_the_label_never_disagree_about_the_source() {
    use crate::pressure::level::Verdict;

    for level in [
        Level::Checking,
        Level::Quiet,
        Level::Stirring,
        Level::Restless,
        Level::Wailing,
        Level::Shrieking,
    ] {
        for source in [
            None,
            Some(Source::Cpu),
            Some(Source::Memory),
            Some(Source::Disk),
            Some(Source::Sprawl),
            Some(Source::ManagedAgents),
        ] {
            let v = Verdict { level, source };
            let glyph = v.glyph();
            let label = v.accessibility_label();
            let named = source.is_some() && level.shows_source();

            // Compare against the PARTS, not a character count. `Source::Sprawl`
            // is 🕸️ — a spider web plus U+FE0F — so a "suffix means two chars"
            // test fails on it, and anything that truncates a glyph by character
            // count would mangle it in the menu bar too.
            let expected = match (named, source) {
                (true, Some(s)) => format!("{}{}", level.glyph(), s.glyph()),
                _ => level.glyph().to_string(),
            };
            assert_eq!(
                glyph, expected,
                "{level:?}/{source:?}: glyph disagrees with named={named}"
            );
            assert_eq!(
                label.contains(", "),
                named,
                "{level:?}/{source:?}: label {label:?} disagrees with named={named}"
            );
            if let (true, Some(s)) = (named, source) {
                assert!(
                    glyph.ends_with(s.glyph()),
                    "{glyph:?} must end with the source"
                );
                assert!(label.ends_with(s.label()), "{label:?} must name the source");
            }
        }
    }
}

/// …and the same invariant on a verdict that came out of `evaluate` against the
/// REAL machine, which is where a stale binary or a mis-wired field would show.
#[test]
fn a_real_verdict_keeps_the_glyph_and_the_source_in_step() {
    use crate::collector::Collector;
    use crate::sys::RealProbe;

    let c = Collector::with_defaults(RealProbe);
    let now = Utc::now();
    let a = c.collect_at(now - chrono::Duration::seconds(15)).unwrap();
    let b = c.collect_at(now).unwrap();
    let p = evaluate(now, &[a, b], &[], &cfg());

    let carries_a_suffix = p.glyph != p.level.glyph();
    assert_eq!(
        p.source.is_some(),
        carries_a_suffix,
        "level={:?} source={:?} glyph={:?} label={:?}",
        p.level,
        p.source,
        p.glyph,
        p.accessibility_label
    );
    if let Some(s) = p.source {
        assert!(
            p.glyph.ends_with(s.glyph()),
            "{:?} must end with the source",
            p.glyph
        );
    }
    assert_eq!(
        p.source.is_some(),
        p.accessibility_label.contains(", "),
        "the spoken label must name a source exactly when the glyph shows one"
    );
}

// ---- gaps in the observations (banshee-s6s) -------------------------------
//
// The bug these exist for was found by RUNNING the installed daemon, not by a
// test, and the reason the old suite could not see it is instructive: every
// held_secs test used an EVENLY SPACED series. That is the co-varying-fixture shape
// again — with uniform spacing, "counts the span" and "counts continuous
// observation" give the same answer, so no assertion could tell them apart.

/// A red either side of a gap is NOT sustained.
///
/// The exact shape observed on 2026-09-01: two samples, then a 17-minute hole, then
/// four more. Every dimension reported `held 17m` off 45 seconds of observation, and
/// because `held_secs` feeds `is_sustained_red`, the verdict was 💀 Shrieking.
///
/// Mutation-proof, verified: revert `Series::held` to
/// `newest - points[n - held]` and this fails with 1029 seconds of imaginary
/// continuity.
#[test]
fn a_run_does_not_count_across_a_gap_in_the_observations() {
    // Two red samples, a 17-minute hole, then four more. The gap machinery is
    // dimension-agnostic; swap (per-sample, absolute bytes) is the clearest red
    // vehicle — 20 GB is well past the 12 GB red line.
    let mut samples = Vec::new();
    for t in [0, 15] {
        let mut s = sample(t);
        s.swap_used_bytes = 20_000_000_000;
        samples.push(s);
    }
    for t in [1044, 1059, 1074, 1089] {
        let mut s = sample(t);
        s.swap_used_bytes = 20_000_000_000;
        samples.push(s);
    }

    let p = evaluate(at(1089), &samples, &[], &cfg());
    let swap = p.reading(Dimension::Swap).expect("swap reading");
    assert_eq!(swap.band, Band::Red, "precondition: the machine reads red");

    // 45 seconds of continuous observation, not 1089.
    assert_eq!(swap.held_secs, 45, "held must stop at the gap");
    assert_eq!(
        swap.observation_gap_secs,
        Some(1029),
        "and the gap must be REPORTED, not silently swallowed"
    );

    // Which means it is not sustained, so the level stays at Restless.
    assert_eq!(
        p.level,
        Level::Restless,
        "a red that has only been observed for 45s is a spike, not a crisis"
    );
    assert_ne!(p.level, Level::Wailing);
    assert_ne!(p.level, Level::Shrieking);
}

/// …and the same red WITHOUT a gap is sustained. Both halves matter: a fix that
/// simply stopped counting would make `is_sustained_red` unreachable and nothing
/// could ever escalate past Restless.
#[test]
fn an_uninterrupted_red_is_still_sustained() {
    let mut samples = Vec::new();
    for i in 0..40 {
        let mut s = sample(i * 15);
        s.swap_used_bytes = 20_000_000_000;
        samples.push(s);
    }
    let p = evaluate(at(39 * 15), &samples, &[], &cfg());
    let swap = p.reading(Dimension::Swap).expect("swap reading");
    assert_eq!(swap.band, Band::Red);
    assert_eq!(swap.held_secs, 585, "39 intervals of 15s");
    assert_eq!(swap.observation_gap_secs, None, "no gap to report");
    assert!(swap.held_secs >= cfg().red_grace_secs);
    assert_eq!(p.level, Level::Wailing, "sustained red escalates");
}

/// A missed tick is not a gap. The sampler uses `MissedTickBehavior::Delay`, so a
/// loaded machine routinely produces a double-length interval; treating that as a
/// hole would reset the held time constantly and nothing would ever escalate.
///
/// Mutation-proof: set `gap_tolerance` to 1.0 and this fails.
#[test]
fn a_missed_tick_is_tolerated() {
    let mut samples = Vec::new();
    // 15s spacing with one 30s hiccup in the middle.
    for t in [0, 15, 30, 60, 75, 90] {
        let mut s = sample(t);
        s.swap_used_bytes = 20_000_000_000;
        samples.push(s);
    }
    let p = evaluate(at(90), &samples, &[], &cfg());
    let swap = p.reading(Dimension::Swap).expect("swap reading");
    assert_eq!(swap.held_secs, 90, "a 30s hiccup must not break the run");
    assert_eq!(swap.observation_gap_secs, None);
}

/// The census tier's NORMAL spacing is 20× the sample tier's, and must not read as
/// a gap — **including before the series is long enough to self-calibrate**, which
/// is every census series in the daemon's first fifteen minutes.
///
/// TWO censuses is the case that matters and the case the per-tier configuration
/// exists for: one interval cannot calibrate anything, so the configured ceiling
/// decides, and if that were the sample tier's 45s then the census's ordinary 300s
/// spacing would read as a gap and agent sprawl could never escalate at all.
///
/// **The first version used FIVE censuses and was a false pin**: with four intervals
/// the series self-calibrates to a 300s median, so it passed even with the per-tier
/// logic removed. Self-healing is good behaviour and a bad test — it hid the only
/// case where the configuration is load-bearing.
///
/// Mutation-proof, verified: make `max_interval_secs` return the sample cadence for
/// every dimension and this fails.
#[test]
fn the_census_tiers_normal_spacing_is_not_a_gap() {
    // TWO censuses, one 300s interval — a daemon five minutes old.
    let censuses = vec![
        census(0, 7, 45, 49.2, 78_000),
        census(300, 7, 45, 49.2, 78_000),
    ];
    let p = evaluate(at(300), &healthy(20, 300), &censuses, &cfg());

    let agents = p.reading(Dimension::Agents).expect("agents reading");
    assert_eq!(
        agents.band,
        Band::Red,
        "precondition: 7 stale sessions is red"
    );
    assert_eq!(
        agents.held_secs, 300,
        "one 300s interval is normal census spacing, not a gap"
    );
    assert_eq!(agents.observation_gap_secs, None);

    // A longer census series is fine too — but it passes for a DIFFERENT reason
    // (self-calibration), which is why it cannot be the only case here.
    let many: Vec<Census> = (0..5)
        .map(|i| census(i * 300, 7, 45, 49.2, 78_000))
        .collect();
    let p = evaluate(at(1200), &healthy(40, 1200), &many, &cfg());
    let agents = p.reading(Dimension::Agents).expect("agents reading");
    assert_eq!(agents.held_secs, 1200);
    assert_eq!(agents.observation_gap_secs, None);
}

/// A series too short to calibrate falls back to the CONFIGURED cadence — and that
/// is the case that produced the false 💀, because one interval is never anomalous
/// relative to itself.
///
/// Mutation-proof, verified: drop the `MIN_INTERVALS_TO_SELF_CALIBRATE` guard so the
/// median is always used, and this fails: the median of a single 1029-second
/// interval is 1029, the ceiling becomes 3087, and the hole is waved through.
#[test]
fn two_samples_far_apart_cannot_calibrate_and_fall_back_to_config() {
    let mut samples = Vec::new();
    for t in [0, 1029] {
        let mut s = sample(t);
        s.swap_used_bytes = 20_000_000_000;
        samples.push(s);
    }
    let p = evaluate(at(1029), &samples, &[], &cfg());
    let swap = p.reading(Dimension::Swap).expect("swap reading");
    assert_eq!(swap.band, Band::Red);
    assert_eq!(swap.held_secs, 0, "one observation's worth of continuity");
    assert_eq!(swap.observation_gap_secs, Some(1029));
    assert_eq!(p.level, Level::Restless, "not a crisis on two readings");
}

/// A sampler running SLOWER than the model was configured for must still work.
///
/// This is the graceful-degradation half of the design: the ceiling self-calibrates
/// from the median interval, so a 60s-spaced series is read as 60s-spaced even
/// though the config says 15s. Without that, a config/reality mismatch would
/// declare every interval a gap, zero every run, and make escalation unreachable —
/// silently, which is not an acceptable way to fail safe.
///
/// Mutation-proof: return `configured_max_interval_secs` unconditionally from
/// `Series::max_interval` and this fails.
#[test]
fn a_slower_than_configured_cadence_self_calibrates() {
    let mut samples = Vec::new();
    for i in 0..6 {
        let mut s = sample(i * 60); // 60s spacing; config says 15s
        s.swap_used_bytes = 20_000_000_000;
        samples.push(s);
    }
    let p = evaluate(at(300), &samples, &[], &cfg());
    let swap = p.reading(Dimension::Swap).expect("swap reading");
    assert_eq!(
        swap.held_secs, 300,
        "the series' own cadence is what counts"
    );
    assert_eq!(swap.observation_gap_secs, None);
    assert!(
        swap.held_secs >= cfg().red_grace_secs,
        "and it can still escalate"
    );
}

/// The MEDIAN interval, not the mean — and the difference bites at the smallest
/// series that can self-calibrate at all.
///
/// A mean is dragged by the very outlier it is supposed to detect. With three
/// intervals of 15s, 3000s, 15s the mean is 1010s, so a mean-based ceiling of 3030s
/// waves the 3000s hole straight through as a missed tick. The median is 15s and the
/// ceiling 45s, which catches it.
///
/// Four points — two readings either side of a restart — is not a contrived shape;
/// it is the shape this bug arrived in.
///
/// **The first version of this test was a FALSE PIN and the arithmetic in its
/// comment was simply wrong.** It used intervals of 15, 1029, 15, 15, 15: mean 217,
/// ceiling 651, and 1029 > 651, so the mean caught it too and swapping median for
/// mean changed nothing. The general rule is that a mean only fails when the gap is
/// a large fraction of the whole window — which means FEW intervals, so the fixture
/// has to be small rather than dramatic. Mutation-proof, verified against
/// `intervals.iter().sum() / len()`.
#[test]
fn the_cadence_estimate_is_robust_to_the_gap_it_is_looking_for() {
    let mut samples = Vec::new();
    for t in [0, 15, 3015, 3030] {
        let mut s = sample(t);
        s.swap_used_bytes = 20_000_000_000;
        samples.push(s);
    }
    let p = evaluate(at(3030), &samples, &[], &cfg());
    let swap = p.reading(Dimension::Swap).expect("swap reading");
    assert_eq!(
        swap.observation_gap_secs,
        Some(3000),
        "a mean would have set the ceiling above the gap and missed it"
    );
    assert_eq!(swap.held_secs, 15, "only the 15s since the gap");
}

/// A sampler ticking FASTER than one second must not have its run shredded by the
/// occasional whole-second interval.
///
/// This is the case the configured floor exists for. Timestamp differences are
/// measured in WHOLE SECONDS, so a 120ms cadence yields intervals of 0 — and a
/// self-calibrated ceiling of `median × 3` is then also 0. Any single interval that
/// does cross a second boundary (a stalled machine, a slow census on the same
/// runtime) exceeds 0 and reads as a gap, resetting the held time to nothing. With
/// the configured floor the ceiling is 3s and the run survives.
///
/// Not hypothetical: `tests/e2e/run-e2e.sh` runs the daemon at 120ms.
///
/// **The first version of this test used a UNIFORM 120ms series and was a false
/// pin** — every interval was 0, and `0 > 0` is false, so a zero ceiling behaved
/// identically to a 3-second one. The fixture has to contain the hiccup for the
/// floor to matter.
///
/// Mutation-proof, verified: drop `.max(self.configured_max_interval_secs)` from
/// `Series::max_interval` and this fails.
#[test]
fn a_sub_second_cadence_survives_the_occasional_one_second_interval() {
    // Mostly 120ms apart, with two intervals that cross a second boundary.
    let offsets_ms: [i64; 12] = [
        0, 120, 240, 1300, 1420, 1540, 1660, 2700, 2820, 2940, 3060, 3180,
    ];
    let mut samples = Vec::new();
    for ms in offsets_ms {
        let mut s = sample(0);
        s.sampled_at = at(0) + chrono::Duration::milliseconds(ms);
        s.swap_used_bytes = 20_000_000_000;
        samples.push(s);
    }
    let mut config = cfg();
    // What `SamplerConfig::from_env` writes for a 120ms tick: rounded UP to 1s.
    config.sample_interval_secs = 1;

    let p = evaluate(
        samples[11].sampled_at,
        &samples,
        &config_censuses(),
        &config,
    );
    let swap = p.reading(Dimension::Swap).expect("swap reading");
    assert_eq!(swap.band, Band::Red);
    assert_eq!(
        swap.observation_gap_secs, None,
        "a 1s hiccup in a sub-second series is a hiccup, not a gap"
    );
    assert_eq!(swap.held_secs, 3, "the whole 3.18s run, not just the tail");
}

/// No censuses; a helper so the call above reads at one line.
fn config_censuses() -> Vec<Census> {
    Vec::new()
}

/// The trend is fitted over the trailing CONTIGUOUS run too. A line through two
/// clusters either side of a hole describes the hole, and this slope is what
/// produces the disk dimension's "full in N hours" — the most actionable sentence
/// the app emits.
///
/// Mutation-proof, verified: fit over `self.points` instead of
/// `self.contiguous_tail()` and the projection reappears, computed across the gap.
#[test]
fn the_trend_is_fitted_only_over_contiguous_observations() {
    // Disk falling fast BEFORE the gap, flat after it. A whole-window fit sees a
    // steep decline; a contiguous fit sees the flat part, which is all we watched.
    let mut samples = Vec::new();
    for (i, t) in [0i64, 15].iter().enumerate() {
        let mut s = sample(*t);
        s.volumes[0].avail_bytes = 400_000_000_000 - (i as u64 * 50_000_000_000);
        samples.push(s);
    }
    for t in [1044, 1059, 1074, 1089] {
        let mut s = sample(t);
        s.volumes[0].avail_bytes = 100_000_000_000; // flat
        samples.push(s);
    }

    let p = evaluate(at(1089), &samples, &[], &cfg());
    let disk = p.reading(Dimension::Disk).expect("disk reading");
    // Flat across the observed run, so no meaningful decline and no projection.
    let slope = disk
        .trend_per_sec
        .expect("a trend over 4 contiguous points");
    assert!(
        slope.abs() < 1.0,
        "the fit must describe the flat contiguous tail, got {slope}"
    );
    assert!(
        !disk.detail.contains("full in"),
        "no time-to-full projection across a gap: {}",
        disk.detail
    );
}
