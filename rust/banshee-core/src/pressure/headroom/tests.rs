// headroom tests. The recurring false-pin shape in this project is a fixture
// where two properties co-vary, so every pin below is built around
// the case where the property under test and its nearest rival come APART:
// level + cores give one answer and thermal or memory flips it; a config knob
// changes the output; a mutation names a level the model did not.

use super::*;
use crate::pressure::episode::RecentActivity;
use crate::pressure::{DimensionReading, Pressure};

const FIXTURE_CHECKING: &str = include_str!("../../../../../tests/fixtures/pressure-checking.json");
const FIXTURE_QUIET: &str = include_str!("../../../../../tests/fixtures/pressure-quiet.json");
const FIXTURE_SHRIEKING: &str =
    include_str!("../../../../../tests/fixtures/pressure-shrieking.json");
const HEADROOM_CHECKING: &str =
    include_str!("../../../../../tests/fixtures/headroom-checking.json");
const HEADROOM_QUIET: &str = include_str!("../../../../../tests/fixtures/headroom-quiet.json");
const HEADROOM_SHRIEKING: &str =
    include_str!("../../../../../tests/fixtures/headroom-shrieking.json");
const HEADROOM_THROTTLED: &str =
    include_str!("../../../../../tests/fixtures/headroom-throttled.json");

fn at() -> DateTime<Utc> {
    crate::wire_time::from_wire("2026-09-08T23:15:00.000000Z").unwrap()
}

fn reading(
    d: Dimension,
    band: Band,
    value: f64,
    severity: f64,
    held: u64,
    detail: &str,
) -> DimensionReading {
    DimensionReading {
        dimension: d,
        key: d.key().to_string(),
        label: d.label().to_string(),
        band,
        value,
        unit: d.unit(),
        severity,
        held_secs: held,
        observation_gap_secs: None,
        trend_per_sec: None,
        detail: detail.to_string(),
        advisory: d.is_advisory(),
        pending: None,
        recovering: false,
    }
}

fn cpu(utilization: f64) -> DimensionReading {
    let band = if utilization >= 0.95 {
        Band::Red
    } else if utilization >= 0.85 {
        Band::Yellow
    } else {
        Band::Green
    };
    reading(
        Dimension::Cpu,
        band,
        utilization,
        (utilization - 0.85) / 0.10,
        600,
        &format!("{:.0}% busy", utilization * 100.0),
    )
}

fn memory(available_bytes: f64) -> DimensionReading {
    let band = if available_bytes < 512e6 {
        Band::Red
    } else if available_bytes < 2e9 {
        Band::Yellow
    } else {
        Band::Green
    };
    reading(
        Dimension::Memory,
        band,
        available_bytes,
        (2e9 - available_bytes) / (2e9 - 512e6),
        600,
        &format!("{} available", crate::pressure::fmt_bytes(available_bytes)),
    )
}

/// A verdict assembled from readings, with the level and source stated
/// explicitly — these tests are about the REDUCTION from verdict to decision,
/// not about `decide`, which has its own suite.
fn verdict(level: Level, source: Option<Source>, dims: Vec<DimensionReading>) -> Pressure {
    Pressure {
        evaluated_at: at(),
        level,
        level_name: level.name().to_string(),
        source,
        glyph: level.glyph().to_string(),
        accessibility_label: format!("Banshee: {}", level.name()),
        dimensions: dims,
        findings: Vec::new(),
        sample_count: 40,
        census_count: 3,
        recovering: false,
        activity: RecentActivity::default(),
    }
}

fn config() -> PressureConfig {
    PressureConfig::default()
}

/// The machine the sample fixtures describe: 8 cores, 24 GiB. The 25% hold-back
/// is then 6.44 GB, so `memory(12e9)` leaves 5.56 GB to spend and `memory(20e9)`
/// leaves 13.56 GB — 2 and 6 workers at 2 GiB.
fn eight() -> Option<MachineFacts> {
    machine(8)
}

fn machine(cores: u32) -> Option<MachineFacts> {
    Some(MachineFacts {
        cores,
        mem_total_bytes: 25_769_803_776,
    })
}

fn condition<'a>(h: &'a Headroom, kind: &str) -> &'a Condition {
    h.conditions
        .iter()
        .find(|c| c.kind == kind)
        .unwrap_or_else(|| panic!("no {kind} condition in {:?}", h.conditions))
}

/// The input behind `headroom-throttled.json`: Restless, 8 cores, 20% busy,
/// 12 GB available — and a thermal red 45 s old. Shared with the fixture
/// test and the co-variance test below.
fn throttled() -> Pressure {
    verdict(
        Level::Restless,
        Some(Source::Thermal),
        vec![
            cpu(0.20),
            memory(12e9),
            reading(
                Dimension::Thermal,
                Band::Red,
                2.0,
                1.0,
                45,
                "kernel reports heavy",
            ),
        ],
    )
}

// ---- the shared fixtures: derive(pressure fixture) == headroom fixture ------
//
// The headroom fixtures are what BOTH languages decode; deriving them here from
// the pressure fixtures is what ties the bytes to the function, so a formula
// change that forgets the fixtures fails here first.

fn assert_matches_fixture(derived: &Headroom, fixture: &str) {
    let expected: Headroom = serde_json::from_str(fixture).expect("the headroom fixture decodes");
    assert_eq!(derived, &expected);
    // And the bytes themselves say what the struct says — a fixture edited by
    // hand to a different `shouldWait` must not slip through the struct compare.
    let raw: serde_json::Value = serde_json::from_str(fixture).unwrap();
    assert_eq!(raw["shouldWait"], derived.should_wait);
    assert_eq!(
        raw["recommendedParallelism"],
        derived.recommended_parallelism
    );
}

/// No data is "wait", never headroom. Mutation-proof: return `should_wait: false`
/// (or any parallelism > 0) for the Checking branch and this fails — that is the
/// lie ADR-0010 decision 3 exists to prevent.
#[test]
fn the_checking_fixture_says_wait_and_never_invents_headroom() {
    let p: Pressure = serde_json::from_str(FIXTURE_CHECKING).unwrap();
    let h = derive(&p, None, &config());
    assert!(h.should_wait);
    assert_eq!(h.recommended_parallelism, 0);
    assert_eq!(h.retry_after_secs, Some(30));
    assert_eq!(h.cores, None);
    assert_eq!(h.level, Level::Checking);
    assert_eq!(condition(&h, "Ready").status, ConditionStatus::False);
    assert!(
        h.conditions[1..]
            .iter()
            .all(|c| c.status == ConditionStatus::Unknown && c.since.is_none()),
        "no reading means Unknown, not False: {:?}",
        h.conditions
    );
    assert!(h.reason.contains("not headroom"), "{}", h.reason);
    assert_matches_fixture(&h, HEADROOM_CHECKING);
}

/// A trustworthy verdict with no core count is still "wait": the arithmetic has
/// nothing to multiply by, and guessing a core count is inventing capacity.
#[test]
fn a_verdict_without_a_core_count_is_wait() {
    let p = verdict(Level::Quiet, None, vec![cpu(0.1)]);
    let h = derive(&p, None, &config());
    assert!(h.should_wait);
    assert_eq!(h.recommended_parallelism, 0);
    assert_eq!(h.cores, None);
}

/// The calm fixture: 8 cores at 53% busy is 3 free cores, no memory
/// reading means no memory cap, Quiet means no level cap. Mutation-proof: use
/// `ceil` instead of `floor` for `cpu_slots` and this reports 4.
#[test]
fn the_quiet_fixture_has_room_for_three_more_on_eight_cores() {
    let p: Pressure = serde_json::from_str(FIXTURE_QUIET).unwrap();
    let h = derive(&p, eight(), &config());
    assert!(!h.should_wait);
    assert_eq!(h.recommended_parallelism, 3, "floor(8 × (1 − 0.53)) = 3");
    assert_eq!(h.retry_after_secs, None, "nothing to wait for, so no retry");
    assert_eq!(h.cores, Some(8));
    assert_eq!(condition(&h, "Ready").status, ConditionStatus::True);
    assert_eq!(condition(&h, "CpuPressure").reason, "AllGreen");
    assert_eq!(
        condition(&h, "MemoryPressure").status,
        ConditionStatus::Unknown
    );
    assert_matches_fixture(&h, HEADROOM_QUIET);
}

/// The 2026-08-04 incident: two sustained reds. Shrieking is the top of the
/// ladder and the cause named is the DOMINANT source's worst dimension (swap),
/// which is what the menu bar names — not whichever red is declared first (cpu).
/// Mutation-proof: name `zeroing_red` first when `sustained` and the reason says
/// CPU.
#[test]
fn the_shrieking_fixture_waits_five_minutes_and_names_the_dominant_source() {
    let p: Pressure = serde_json::from_str(FIXTURE_SHRIEKING).unwrap();
    let h = derive(&p, eight(), &config());
    assert!(h.should_wait);
    assert_eq!(h.recommended_parallelism, 0);
    assert_eq!(h.retry_after_secs, Some(300));
    assert!(
        h.reason.starts_with("Shrieking: Swap in use is red"),
        "{}",
        h.reason
    );
    assert!(h.reason.contains("held 1h0m"), "{}", h.reason);
    let mem = condition(&h, "MemoryPressure");
    assert_eq!(mem.status, ConditionStatus::True);
    assert_eq!(
        mem.reason, "SwapRed",
        "uptime (yellow, advisory) must not win"
    );
    assert_eq!(mem.message, "35.6 GB in use, climbing 74 MB/min");
    assert_eq!(
        mem.since,
        Some(crate::wire_time::from_wire("2026-08-04T21:41:09.870112Z").unwrap()),
        "since = evaluatedAt − heldSecs (3600)"
    );
    assert_eq!(condition(&h, "CpuPressure").status, ConditionStatus::True);
    assert_eq!(
        condition(&h, "ThermalPressure").status,
        ConditionStatus::Unknown
    );
    assert_matches_fixture(&h, HEADROOM_SHRIEKING);
}

/// THE co-variance breaker. Level and cores alone say "room for 2" (Restless caps
/// 8 cores at 2; 20% busy leaves 6; memory 12 GB leaves 12). A FRESH thermal
/// red — 45 s held, inside the grace period, so the level is only Restless —
/// flips it to wait, because a throttled CPU makes the load arithmetic a lie.
///
/// The control is the same machine with a fresh SPRAWL red instead, which is
/// Restless for the same reason and keeps its 2. The two implementations
/// disagree only because of the thermal rule.
///
/// Mutation-proof, verified: drop `Source::Thermal` from `ZEROING_SOURCES` and
/// this reports 2 with `should_wait: false`.
#[test]
fn a_fresh_thermal_red_flips_a_restless_machine_from_two_slots_to_wait() {
    let control = verdict(
        Level::Restless,
        Some(Source::Sprawl),
        vec![
            cpu(0.20),
            memory(12e9),
            reading(Dimension::Orphans, Band::Red, 45.0, 1.2, 45, "45 orphaned"),
        ],
    );
    let h_control = derive(&control, eight(), &config());
    assert!(
        !h_control.should_wait,
        "a fresh SPRAWL red does not zero it"
    );
    assert_eq!(
        h_control.recommended_parallelism, 2,
        "Restless caps 8 cores at 2"
    );

    let h = derive(&throttled(), eight(), &config());
    assert!(h.should_wait, "thermal red must force a wait");
    assert_eq!(h.recommended_parallelism, 0);
    assert_eq!(
        h.retry_after_secs,
        Some(60),
        "a red below Wailing is the 60s rung"
    );
    assert!(
        h.reason.starts_with("Restless: Thermal pressure is red"),
        "{}",
        h.reason
    );
    assert!(h.reason.contains("held 45s"), "{}", h.reason);
    assert_eq!(
        condition(&h, "ThermalPressure").status,
        ConditionStatus::True
    );
    assert_eq!(condition(&h, "ThermalPressure").reason, "ThermalRed");
    assert_eq!(condition(&h, "CpuPressure").status, ConditionStatus::False);
    assert_matches_fixture(&h, HEADROOM_THROTTLED);
}

/// The same flip from the memory side, and on the KERNEL's dimension rather than
/// available bytes — 3 GB available is green, the kernel says critical anyway.
/// Mutation-proof: drop `Source::Memory` from `ZEROING_SOURCES` and this reports 2.
#[test]
fn a_fresh_kernel_pressure_red_forces_a_wait_even_with_memory_available() {
    let p = verdict(
        Level::Restless,
        Some(Source::Memory),
        vec![
            cpu(0.20),
            memory(3e9),
            reading(
                Dimension::KernelPressure,
                Band::Red,
                4.0,
                1.0,
                30,
                "kernel reports critical",
            ),
        ],
    );
    let h = derive(&p, eight(), &config());
    assert!(h.should_wait);
    assert_eq!(h.recommended_parallelism, 0);
    assert!(
        h.reason.contains("Kernel memory pressure is red"),
        "{}",
        h.reason
    );
    assert_eq!(condition(&h, "MemoryPressure").reason, "KernelPressureRed");
}

/// Disk red zeroes it too — builds write. Mutation-proof: drop `Source::Disk`.
#[test]
fn a_disk_red_forces_a_wait() {
    let p = verdict(
        Level::Restless,
        Some(Source::Disk),
        vec![
            cpu(0.20),
            reading(Dimension::Disk, Band::Red, 9e9, 1.2, 30, "9.0 GB free"),
        ],
    );
    let h = derive(&p, eight(), &config());
    assert!(h.should_wait);
    assert_eq!(condition(&h, "DiskPressure").reason, "DiskRed");
}

/// A sustained red is a wait whatever the resource arithmetic says: Wailing with
/// a red CORPORATE dimension (which is not a zeroing source) and 7 free cores
/// still waits, because the level is the notification level. Mutation-proof:
/// change `sustained` to `p.level > Level::Wailing` and this reports 4.
#[test]
fn wailing_waits_even_when_the_red_is_not_a_zeroing_source() {
    let p = verdict(
        Level::Wailing,
        Some(Source::Corporate),
        vec![
            cpu(0.10),
            memory(20e9),
            reading(
                Dimension::Corporate,
                Band::Red,
                110.0,
                1.25,
                900,
                "110.0% of one core",
            ),
        ],
    );
    let h = derive(&p, eight(), &config());
    assert!(h.should_wait);
    assert_eq!(h.recommended_parallelism, 0);
    assert_eq!(h.retry_after_secs, Some(120));
    assert!(
        h.reason.starts_with("Wailing: Managed agents is red"),
        "{}",
        h.reason
    );
}

// ---- the arithmetic, knob by knob --------------------------------------------

/// Memory per slot is load-bearing. 8 cores at 0.10× leaves 7; 10 GiB available on
/// a 24 GiB machine is 10.74 − 6.44 = 4.3 GB to spend: 2 workers at 2 GiB and 4 at
/// 1 GiB. Mutation-proof: ignore the config and use a literal, or drop `mem_slots`
/// from the `min`, and one of the two halves fails.
#[test]
fn the_memory_budget_per_slot_is_configuration_and_caps_the_answer() {
    let p = verdict(
        Level::Quiet,
        None,
        vec![cpu(0.10), memory(10.0 * 1024.0 * 1024.0 * 1024.0)],
    );
    let mut c = config();
    assert_eq!(
        derive(&p, eight(), &c).recommended_parallelism,
        2,
        "2 GiB per slot"
    );
    c.headroom_memory_per_slot_bytes = 1 << 30;
    assert_eq!(
        derive(&p, eight(), &c).recommended_parallelism,
        4,
        "1 GiB per slot"
    );
}

/// The hold-back is load-bearing and it is TOTAL memory it holds back — the
/// human's and the OS's share, which available memory alone does not protect.
/// 10 GiB available on 24 GiB: 2 workers with the 25% reserve, 5 with none.
/// Mutation-proof, verified: drop the `- reserve` and this reports 5; multiply by
/// the AVAILABLE figure instead of total and this reports 3.
#[test]
fn the_memory_reserve_is_a_fraction_of_total_and_is_configuration() {
    let p = verdict(
        Level::Quiet,
        None,
        vec![cpu(0.10), memory(10.0 * 1024.0 * 1024.0 * 1024.0)],
    );
    let mut c = config();
    assert_eq!(derive(&p, eight(), &c).recommended_parallelism, 2);
    c.headroom_memory_reserve_fraction = 0.0;
    assert_eq!(
        derive(&p, eight(), &c).recommended_parallelism,
        5,
        "no reserve: floor(10 GiB / 2 GiB)"
    );
    // A reserve larger than what is available spends nothing — and the machine is
    // not in trouble, so the floor of one still applies.
    c.headroom_memory_reserve_fraction = 0.5;
    let h = derive(&p, eight(), &c);
    assert_eq!(h.recommended_parallelism, 1);
    assert!(!h.should_wait);
}

/// The level cap is load-bearing, and it is what distinguishes Stirring from
/// Quiet when the resources say the same thing: 7 free cores and 6 memory slots,
/// capped at ceil(8/2) = 4. Mutation-proof: drop the `Level::Stirring` arm and
/// this reports 6.
#[test]
fn stirring_caps_at_half_the_cores_and_restless_at_a_quarter() {
    let dims = || {
        vec![
            cpu(0.10),
            memory(20e9),
            reading(
                Dimension::Orphans,
                Band::Yellow,
                12.0,
                0.07,
                300,
                "12 orphaned",
            ),
        ]
    };
    let quiet = derive(&verdict(Level::Quiet, None, dims()), eight(), &config());
    assert_eq!(
        quiet.recommended_parallelism, 6,
        "Quiet: 7 free cores, but 20 GB − 6.44 GB reserve is 6 workers at 2 GiB"
    );
    let stirring = derive(&verdict(Level::Stirring, None, dims()), eight(), &config());
    assert_eq!(stirring.recommended_parallelism, 4);
    let restless = derive(
        &verdict(Level::Restless, Some(Source::Sprawl), dims()),
        eight(),
        &config(),
    );
    assert_eq!(restless.recommended_parallelism, 2);
    // Odd core counts round UP — a 10-core machine at Restless still gets 3.
    let ten = derive(
        &verdict(Level::Restless, Some(Source::Sprawl), dims()),
        machine(10),
        &config(),
    );
    assert_eq!(ten.recommended_parallelism, 3);
}

/// Thermal MODERATE halves it, rounding up: Stirring (one yellow — thermal
/// itself) caps 8 cores at 4, then the halving makes it 2. The control is the
/// same machine with an ORPHANS yellow instead, which stays at 4. Mutation-proof:
/// drop the halving and both read 4.
#[test]
fn thermal_moderate_halves_the_answer() {
    let hot = verdict(
        Level::Stirring,
        None,
        vec![
            cpu(0.10),
            memory(20e9),
            reading(
                Dimension::Thermal,
                Band::Yellow,
                1.0,
                0.0,
                300,
                "kernel reports moderate",
            ),
        ],
    );
    let cool = verdict(
        Level::Stirring,
        None,
        vec![
            cpu(0.10),
            memory(20e9),
            reading(
                Dimension::Orphans,
                Band::Yellow,
                12.0,
                0.07,
                300,
                "12 orphaned",
            ),
        ],
    );
    assert_eq!(derive(&cool, eight(), &config()).recommended_parallelism, 4);
    assert_eq!(derive(&hot, eight(), &config()).recommended_parallelism, 2);
    assert!(
        !derive(&hot, eight(), &config()).should_wait,
        "moderate is a cut, not a wait"
    );
}

/// The floor of one: a calm machine with no free cores still says "run one",
/// so `should_wait` is exactly `recommended_parallelism == 0`. Mutation-proof:
/// drop `.max(1)` and this reports 0 with `should_wait: false` — a contradiction
/// no agent can act on.
#[test]
fn a_calm_machine_with_no_free_cores_still_has_room_for_one() {
    let p = verdict(Level::Quiet, None, vec![cpu(0.95), memory(20e9)]);
    let h = derive(&p, eight(), &config());
    assert!(!h.should_wait);
    assert_eq!(
        h.recommended_parallelism, 1,
        "floor(8 × 0.05) = 0, floored to 1"
    );
    assert_eq!(h.retry_after_secs, None);
    assert!(
        h.reason.contains("room for 1 more worker on 8 cores"),
        "{}",
        h.reason
    );
}

/// The invariant every surface relies on, over a sweep of machines: waiting and
/// zero parallelism are the same fact, and a retry is present exactly then.
#[test]
fn should_wait_is_exactly_zero_parallelism_and_retry_is_present_exactly_then() {
    let cases = vec![
        derive(
            &serde_json::from_str::<Pressure>(FIXTURE_CHECKING).unwrap(),
            None,
            &config(),
        ),
        derive(
            &serde_json::from_str::<Pressure>(FIXTURE_QUIET).unwrap(),
            eight(),
            &config(),
        ),
        derive(
            &serde_json::from_str::<Pressure>(FIXTURE_SHRIEKING).unwrap(),
            eight(),
            &config(),
        ),
        derive(&throttled(), eight(), &config()),
        derive(
            &verdict(Level::Quiet, None, vec![cpu(0.95)]),
            eight(),
            &config(),
        ),
        derive(
            &verdict(Level::Stirring, None, vec![cpu(0.5), memory(900e6)]),
            eight(),
            &config(),
        ),
        derive(
            &verdict(
                Level::Restless,
                Some(Source::Memory),
                vec![cpu(0.5), memory(400e6)],
            ),
            eight(),
            &config(),
        ),
    ];
    for h in cases {
        assert_eq!(h.should_wait, h.recommended_parallelism == 0, "{h:?}");
        assert_eq!(h.should_wait, h.retry_after_secs.is_some(), "{h:?}");
    }
}

/// The retry ladder is configuration, rung by rung. Each knob is set to a value
/// nothing else uses, so a rung wired to the wrong knob (or to a literal) is
/// caught. Mutation-proof: swap the Wailing and Shrieking arms and this fails.
#[test]
fn the_retry_ladder_is_configuration_rung_by_rung() {
    let mut c = config();
    c.headroom_retry_checking_secs = 11;
    c.headroom_retry_red_secs = 22;
    c.headroom_retry_wailing_secs = 33;
    c.headroom_retry_shrieking_secs = 44;

    let checking: Pressure = serde_json::from_str(FIXTURE_CHECKING).unwrap();
    assert_eq!(derive(&checking, None, &c).retry_after_secs, Some(11));

    let fresh_red = verdict(
        Level::Restless,
        Some(Source::Memory),
        vec![cpu(0.2), memory(400e6)],
    );
    assert_eq!(derive(&fresh_red, eight(), &c).retry_after_secs, Some(22));

    let wailing = verdict(Level::Wailing, Some(Source::Cpu), vec![cpu(3.5)]);
    assert_eq!(derive(&wailing, eight(), &c).retry_after_secs, Some(33));

    let shrieking = verdict(
        Level::Shrieking,
        Some(Source::Memory),
        vec![cpu(3.5), memory(400e6)],
    );
    assert_eq!(derive(&shrieking, eight(), &c).retry_after_secs, Some(44));
}

// ---- conditions ---------------------------------------------------------------

/// `Ready` first, then the six sources in a FIXED order — a client may index by
/// position. Mutation-proof: reorder `CONDITION_SOURCES` and this fails.
#[test]
fn conditions_are_ready_then_the_six_sources_in_order() {
    let p: Pressure = serde_json::from_str(FIXTURE_QUIET).unwrap();
    let h = derive(&p, eight(), &config());
    let kinds: Vec<&str> = h.conditions.iter().map(|c| c.kind.as_str()).collect();
    assert_eq!(
        kinds,
        vec![
            "Ready",
            "CpuPressure",
            "MemoryPressure",
            "ThermalPressure",
            "DiskPressure",
            "SprawlPressure",
            "CorporatePressure",
        ]
    );
}

/// A yellow is `False` with a reason that says yellow — visible, not alarming.
/// A source with two dimensions names the worst one. Advisory uptime is not a
/// memory reading: alone, it leaves `MemoryPressure` Unknown.
#[test]
fn a_condition_names_the_worst_non_advisory_dimension_of_its_source() {
    let p = verdict(
        Level::Stirring,
        None,
        vec![
            cpu(0.3),
            reading(
                Dimension::Swap,
                Band::Yellow,
                5e9,
                0.125,
                200,
                "5.0 GB in use",
            ),
            reading(
                Dimension::Uptime,
                Band::Yellow,
                12.0,
                0.36,
                86400,
                "12.0 days",
            ),
        ],
    );
    let h = derive(&p, eight(), &config());
    let mem = condition(&h, "MemoryPressure");
    assert_eq!(mem.status, ConditionStatus::False, "yellow is not pressure");
    assert_eq!(mem.reason, "SwapYellow");
    assert_eq!(mem.message, "5.0 GB in use");
    assert_eq!(mem.since, Some(at() - chrono::Duration::seconds(200)));

    let only_uptime = verdict(
        Level::Quiet,
        None,
        vec![
            cpu(0.3),
            reading(
                Dimension::Uptime,
                Band::Yellow,
                12.0,
                0.36,
                86400,
                "12.0 days",
            ),
        ],
    );
    let h = derive(&only_uptime, eight(), &config());
    assert_eq!(
        condition(&h, "MemoryPressure").status,
        ConditionStatus::Unknown
    );
    assert_eq!(condition(&h, "MemoryPressure").reason, "NoReading");
}

/// An all-green source reports its PRIMARY dimension (available memory), not the
/// green that happens to sit closest to its own yellow line. Found live: thrash
/// at 0 ops/sec has severity −0.11 and 7 GB available has −3.5, so "worst by
/// severity" named thrash and `MemoryPressure` read `peak 0 swap ops/sec`.
/// Mutation-proof, verified: pick the worst green by severity and this names
/// thrash. The yellow case is unchanged — out of band still wins.
#[test]
fn an_all_green_source_reports_its_primary_dimension() {
    let thrash = reading(
        Dimension::Thrash,
        Band::Green,
        0.0,
        -0.11,
        600,
        "peak 0 swap ops/sec",
    );
    let p = verdict(
        Level::Quiet,
        None,
        vec![cpu(0.3), memory(7.2e9), thrash.clone()],
    );
    let h = derive(&p, eight(), &config());
    let mem = condition(&h, "MemoryPressure");
    assert_eq!(mem.reason, "AllGreen");
    assert_eq!(
        mem.message, "7.2 GB available",
        "the primary dimension, not the closest green"
    );

    // Out of band still wins over the primary: a yellow thrash names itself.
    let hot_thrash = reading(
        Dimension::Thrash,
        Band::Yellow,
        150.0,
        0.05,
        60,
        "peak 150 swap ops/sec",
    );
    let p = verdict(
        Level::Stirring,
        None,
        vec![cpu(0.3), memory(7.2e9), hot_thrash],
    );
    let h = derive(&p, eight(), &config());
    assert_eq!(condition(&h, "MemoryPressure").reason, "ThrashYellow");
    assert_eq!(
        condition(&h, "MemoryPressure").message,
        "peak 150 swap ops/sec"
    );
}

// ---- wire shape ---------------------------------------------------------------

/// camelCase at every depth, `type` (the Kubernetes spelling, not `kind`),
/// statuses spelled `True`/`False`/`Unknown`, nullables PRESENT as null, and a
/// round trip through our own codec. Mutation-proof: drop the `rename = "type"`
/// and the key check fails; drop `wire_time::option` on `since` and the null
/// check fails.
#[test]
fn the_wire_shape_is_camel_case_with_kubernetes_spellings_and_explicit_nulls() {
    let p: Pressure = serde_json::from_str(FIXTURE_QUIET).unwrap();
    let h = derive(&p, eight(), &config());
    let v = serde_json::to_value(&h).unwrap();

    // `to_value` sorts keys; the key SET is the contract, and the serialized
    // bytes (which keep struct order) must lead with the decision fields.
    let mut keys: Vec<&str> = v.as_object().unwrap().keys().map(String::as_str).collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        vec![
            "conditions",
            "cores",
            "evaluatedAt",
            "level",
            "reason",
            "recommendedParallelism",
            "retryAfterSecs",
            "shouldWait",
        ]
    );
    let text = serde_json::to_string(&h).unwrap();
    assert!(
        text.starts_with(r#"{"evaluatedAt":"2026-08-31T15:04:05.123456Z","level":"quiet","shouldWait":false,"recommendedParallelism":3,"retryAfterSecs":null,"cores":8,"#),
        "{text}"
    );
    assert!(!text.contains("should_wait"), "snake_case leaked: {text}");
    assert!(
        !text.contains("\"kind\""),
        "the Kubernetes key is `type`: {text}"
    );
    assert!(
        v["retryAfterSecs"].is_null(),
        "present-as-null when not waiting"
    );
    let c0 = &v["conditions"][0];
    assert_eq!(c0["type"], "Ready");
    assert_eq!(c0["status"], "True");
    assert!(
        c0.get("since").is_some_and(|s| s.is_null()),
        "Ready's since is present-as-null"
    );
    let mem = &v["conditions"][2];
    assert_eq!(mem["type"], "MemoryPressure");
    assert_eq!(mem["status"], "Unknown");
    assert!(mem["since"].is_null());
    let cpu = &v["conditions"][1];
    assert_eq!(
        cpu["since"], "2026-08-31T14:54:05.123456Z",
        "600s before evaluatedAt"
    );

    let back: Headroom = serde_json::from_value(v).unwrap();
    assert_eq!(back, h);

    // The CANONICAL datetime form, six fractional digits even when they are all
    // zero. This is the case that distinguishes `wire_time::option` from chrono's
    // default serializer: on `…05.123456Z` the two agree to the byte, and the
    // first version of this test was a false pin for exactly that reason (caught
    // by `scripts/mutate.sh`, fifteenth instance of the co-varying shape). On a
    // whole-second instant chrono writes `…00Z` and the contract says `…00.000000Z`.
    let t = serde_json::to_value(derive(&throttled(), eight(), &config())).unwrap();
    assert_eq!(
        t["conditions"][1]["since"], "2026-09-08T23:05:00.000000Z",
        "since must use the canonical 6-digit form even for a zero fraction"
    );
    assert_eq!(t["evaluatedAt"], "2026-09-08T23:15:00.000000Z");
}

/// Dump the four derived payloads as pretty JSON. Not an assertion — it is how
/// the shared fixtures were GENERATED and are regenerated after a formula
/// change (`cargo test -p banshee-core dump_headroom_fixtures -- --ignored
/// --nocapture`), then read by a person before they are committed.
#[test]
#[ignore]
fn dump_headroom_fixtures() {
    let checking: Pressure = serde_json::from_str(FIXTURE_CHECKING).unwrap();
    let quiet: Pressure = serde_json::from_str(FIXTURE_QUIET).unwrap();
    let shrieking: Pressure = serde_json::from_str(FIXTURE_SHRIEKING).unwrap();
    for (name, h) in [
        ("headroom-checking", derive(&checking, None, &config())),
        ("headroom-quiet", derive(&quiet, eight(), &config())),
        ("headroom-shrieking", derive(&shrieking, eight(), &config())),
        (
            "headroom-throttled",
            derive(&throttled(), eight(), &config()),
        ),
    ] {
        println!(
            "=== {name}.json\n{}",
            serde_json::to_string_pretty(&h).unwrap()
        );
    }
}
