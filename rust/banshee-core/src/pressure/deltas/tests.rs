use super::*;
use crate::census::{AgentSession, AppGroup, HelperRollup, MonitorTotal, OrphanCensus};
use crate::sample::VolumeSample;
use crate::sys::DATA_VOLUME;

const BOOT: i64 = 1_788_100_000;

fn at(secs: i64) -> DateTime<Utc> {
    DateTime::from_timestamp(BOOT + secs, 0).unwrap()
}

fn cfg() -> PressureConfig {
    PressureConfig::default()
}

/// A sample `t` seconds after boot with a chosen swap load and free-page count.
fn sample(t: i64, swap_used: u64, pages_free: u64) -> Sample {
    Sample {
        id: Uuid::new_v4(),
        sampled_at: at(t),
        boot_time_secs: BOOT,
        ncpu: 8,
        mem_total_bytes: 25_769_803_776,
        page_size: 16384,
        load1m: 4.0,
        load5m: 4.0,
        load15m: 4.0,
        swap_total_bytes: 8_589_934_592,
        swap_used_bytes: swap_used,
        pages_free,
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
        volumes: vec![VolumeSample {
            mount_point: DATA_VOLUME.into(),
            total_bytes: 494_384_795_648,
            avail_bytes: 100_000_000_000,
        }],
    }
}

/// A census with a Chrome app group and a run of `claude` sessions.
fn census(
    t: i64,
    chrome_procs: u32,
    chrome_rss: u64,
    claude_procs: u32,
    claude_rss_each: u64,
    managed_rss: u64,
) -> Census {
    let sessions = (0..claude_procs)
        .map(|i| AgentSession {
            pid: 1000 + i,
            program: "claude".into(),
            rss_bytes: claude_rss_each,
            age_secs: 8 * 86_400,
            age_days: 8,
            cpu_secs: 100.0,
            tty: format!("ttys{i:03}"),
            is_stale: false,
            cwd: None,
        })
        .collect();
    let app_groups = if chrome_procs > 0 {
        vec![AppGroup {
            name: "Chrome".into(),
            proc_count: chrome_procs,
            rss_bytes: chrome_rss,
        }]
    } else {
        Vec::new()
    };
    Census {
        id: Uuid::new_v4(),
        taken_at: at(t),
        total_procs: 900,
        agent_sessions: sessions,
        ide_helpers: HelperRollup {
            count: 0,
            rss_bytes: 0,
        },
        orphans: OrphanCensus {
            total_count: 150,
            total_rss_bytes: 1_000_000_000,
            orphan_count: 5,
            orphan_rss_bytes: 0,
            orphans_by_program: Vec::new(),
        },
        tmux_sessions: Vec::new(),
        app_groups,
        monitor_agents: Vec::new(),
        monitor_total: MonitorTotal {
            proc_count: 13,
            rss_bytes: managed_rss,
            cpu_secs_total: 1000.0,
            percent_of_one_core: 49.2,
            longest_life_secs: 78_000,
        },
        tmux_available: true,
        cpu_consumers: Vec::new(),
        pressure_kills: None,
    }
}

// ---- summarise -------------------------------------------------------------

#[test]
fn summarise_groups_every_kind_biggest_first() {
    // Chrome 8 GB, two claude sessions 1 GB each (2 GB), managed agents 1.4 GB.
    let c = census(0, 100, 8_000_000_000, 2, 1_000_000_000, 1_400_000_000);
    let s = summarise(&c);
    let names: Vec<&str> = s.consumers.iter().map(|c| c.name.as_str()).collect();
    // Biggest first: Chrome (8) > claude (2) > managed agents (1.4).
    assert_eq!(names, vec!["Chrome", "claude", "managed agents"]);
    assert_eq!(s.consumers[0].kind, ConsumerKind::AppGroup);
    assert_eq!(s.consumers[1].kind, ConsumerKind::Sessions);
    assert_eq!(s.consumers[1].count, 2);
    assert_eq!(s.consumers[1].rss_bytes, 2_000_000_000);
    assert_eq!(s.consumers[2].kind, ConsumerKind::Rollup);
    assert_eq!(s.orphan_count, 5);
}

#[test]
fn summarise_drops_zero_sized_consumers() {
    // No Chrome, no claude, no managed RSS: nothing to attribute.
    let c = census(0, 0, 0, 0, 0, 0);
    let s = summarise(&c);
    assert!(
        s.consumers.is_empty(),
        "a consumer that costs nothing is not a consumer, got {:?}",
        s.consumers
    );
}

// ---- parse_lookback --------------------------------------------------------

#[test]
fn parse_lookback_reads_every_unit() {
    let c = cfg();
    assert_eq!(
        parse_lookback("90s", &c),
        Ok(Lookback::Ago(Duration::seconds(90)))
    );
    assert_eq!(
        parse_lookback("30m", &c),
        Ok(Lookback::Ago(Duration::seconds(1800)))
    );
    assert_eq!(
        parse_lookback("1h", &c),
        Ok(Lookback::Ago(Duration::seconds(3600)))
    );
    assert_eq!(
        parse_lookback("600", &c),
        Ok(Lookback::Ago(Duration::seconds(600)))
    );
    assert_eq!(parse_lookback("  episode ", &c), Ok(Lookback::Episode));
}

#[test]
fn parse_lookback_refuses_out_of_bounds_rather_than_clamping() {
    let c = cfg();
    // Below one minute — cannot hold two samples.
    assert!(parse_lookback("30s", &c).is_err());
    // Above the raw retention ceiling: REFUSED, never silently clamped, or an
    // agent reasons about a window it did not get.
    assert!(parse_lookback("48h", &c).is_err());
    assert!(parse_lookback("0", &c).is_err());
    assert!(parse_lookback("soon", &c).is_err());
}

// ---- consumer_deltas: the co-variance breaker ------------------------------

/// The ranking this read exists to give is BY CHANGE, not by current size —
/// the biggest thing running is rarely the thing that moved. Chrome is 8 GB and
/// did not budge; claude is a quarter its size and grew 0.6 GB. claude must rank
/// first. A `sort by now_bytes` (the who-line's ranking) would put Chrome first
/// and this fixture tells the two apart — they co-vary in every fixture where the
/// mover is also the largest.
#[test]
fn consumer_deltas_rank_by_absolute_change_not_current_size() {
    let then = summarise(&census(
        0,
        100,
        8_000_000_000,
        8,
        187_500_000,
        1_000_000_000,
    ));
    let now = summarise(&census(
        300,
        100,
        8_000_000_000,
        10,
        210_000_000,
        1_000_000_000,
    ));
    let d = consumer_deltas(&then, &now);
    assert_eq!(
        d[0].name, "claude",
        "the mover ranks first, not the largest"
    );
    assert_eq!(d[0].then_count, 8);
    assert_eq!(d[0].now_count, 10);
    // Chrome present but unchanged, ranked below the smaller thing that moved.
    let chrome = d.iter().find(|c| c.name == "Chrome").unwrap();
    assert_eq!(chrome.delta_bytes, 0);
    assert!(
        d.iter().position(|c| c.name == "claude").unwrap()
            < d.iter().position(|c| c.name == "Chrome").unwrap()
    );
}

#[test]
fn consumer_words_lead_with_the_action() {
    assert_eq!(
        consumer_words("Chrome", 128, 34, 8_000_000_000, 4_000_000_000),
        "freed 4.0 GB from Chrome (×128 → ×34)"
    );
    assert_eq!(
        consumer_words("claude", 8, 10, 1_500_000_000, 2_100_000_000),
        "claude grew 600 MB (×8 → ×10)"
    );
    assert_eq!(
        consumer_words("Slack", 3, 3, 2_100_000_000, 2_100_000_000),
        "Slack unchanged at 2.1 GB"
    );
}

// ---- verdict / dimension words ---------------------------------------------

#[test]
fn verdict_words_say_changed_or_unchanged() {
    assert_eq!(
        verdict_words(Level::Restless, Level::Wailing),
        "pressure Restless → Wailing"
    );
    assert_eq!(
        verdict_words(Level::Restless, Level::Restless),
        "pressure unchanged at Restless"
    );
}

#[test]
fn dimension_words_render_per_unit() {
    assert_eq!(
        dimension_words(Dimension::Swap, 12_000_000_000.0, 13_300_000_000.0),
        "swap 12.0 GB → 13.3 GB (+1.3 GB)"
    );
    // A fall renders with the minus glyph, not an ASCII hyphen.
    assert!(
        dimension_words(Dimension::Memory, 9_000_000_000.0, 8_100_000_000.0).contains("−900 MB")
    );
    // CPU is measured utilization now (`banshee-87l.19`), rendered as busy-percent,
    // not a load multiple.
    assert_eq!(
        dimension_words(Dimension::Cpu, 0.20, 0.98),
        "CPU 20% → 98% busy (+78 pts)"
    );
}

// ---- compute: the whole thing over stored history --------------------------

/// `samples` rising in swap and falling in free memory, two censuses. The read
/// carries a verdict at both ends, dimension deltas, and consumer deltas.
#[test]
fn compute_reports_verdict_dimensions_and_consumers() {
    let n = 20;
    // 20 samples, 15s apart, swap climbing 1 GB → 3 GB, free memory falling.
    let samples: Vec<Sample> = (0..n)
        .map(|i| {
            let t = (i as i64) * 15;
            sample(
                t,
                1_000_000_000 + (i as u64) * 100_000_000,
                400_000 - (i as u64) * 5_000,
            )
        })
        .collect();
    let then_at = samples[0].sampled_at;
    let now = samples[n - 1].sampled_at;
    let censuses = vec![
        census(0, 100, 8_000_000_000, 8, 187_500_000, 1_000_000_000),
        census(
            (n as i64 - 1) * 15,
            100,
            4_000_000_000,
            10,
            210_000_000,
            1_000_000_000,
        ),
    ];
    let d = compute(now, then_at, "1h", None, None, &samples, &censuses, &cfg());

    assert!(d.verdict.is_some());
    let swap = d
        .dimensions
        .iter()
        .find(|x| x.dimension == Dimension::Swap)
        .unwrap();
    assert!(swap.delta > 0.0, "swap grew");
    assert_eq!(swap.observation_gap_secs, None, "evenly spaced, no hole");
    // Chrome freed 4 GB, claude grew.
    let chrome = d.consumers.iter().find(|c| c.name == "Chrome").unwrap();
    assert!(chrome.delta_bytes < 0);
    assert!(d.census.is_some());
    assert!(d.summary.contains("Chrome"));
}

/// The episode half: the live censuses behind the incident are gone, but the
/// stored `censusAtPeak` still answers "what changed since this began". The
/// override is used in preference to any live census lookup.
#[test]
fn compute_uses_the_stored_override_when_live_censuses_are_swept() {
    let n = 10;
    let samples: Vec<Sample> = (0..n)
        .map(|i| sample((i as i64) * 15, 2_000_000_000, 400_000))
        .collect();
    // Only ONE live census in reach (the newest); the anchor's is swept.
    let live_now = census(
        (n as i64 - 1) * 15,
        40,
        4_900_000_000,
        10,
        210_000_000,
        2_100_000_000,
    );
    // The stored peak summary from long ago: Chrome was 8.9 GB, claude smaller.
    let stored_peak = summarise(&census(
        -100_000,
        102,
        8_900_000_000,
        8,
        187_500_000,
        1_400_000_000,
    ));

    let d = compute(
        samples[n - 1].sampled_at,
        samples[0].sampled_at,
        "episode",
        Some(Uuid::nil()),
        Some(&stored_peak),
        &samples,
        std::slice::from_ref(&live_now),
        &cfg(),
    );
    let chrome = d.consumers.iter().find(|c| c.name == "Chrome").unwrap();
    assert_eq!(
        chrome.then_bytes, 8_900_000_000,
        "compared against the STORED peak"
    );
    assert!(chrome.delta_bytes < 0, "Chrome fell from the peak");
    assert_eq!(d.episode_id, Some(Uuid::nil()));
    assert!(d.summary.starts_with("since the episode opened"));
}

/// A hole longer than the gap ceiling is reported per dimension and at the top,
/// and the sentence says the arithmetic spans it — never absorbed (`banshee-s6s`).
#[test]
fn compute_reports_an_observation_gap() {
    // Five samples 15s apart, then a 20-minute hole, then five more.
    let mut samples: Vec<Sample> = (0..5)
        .map(|i| sample((i as i64) * 15, 2_000_000_000, 400_000))
        .collect();
    for i in 0..5 {
        samples.push(sample(60 + 1200 + (i as i64) * 15, 3_000_000_000, 300_000));
    }
    let then_at = samples[0].sampled_at;
    let now = samples.last().unwrap().sampled_at;
    let d = compute(now, then_at, "1h", None, None, &samples, &[], &cfg());
    assert_eq!(d.observation_gap_secs, Some(1200));
    assert!(
        d.summary.contains("spans a hole"),
        "sentence: {}",
        d.summary
    );
}

/// When the history is shorter than the ask, the anchor is the earliest sample
/// and the sentence says so — a truncated lookback is not the lookback.
#[test]
fn compute_reports_a_truncated_lookback() {
    let samples: Vec<Sample> = (0..10)
        .map(|i| sample((i as i64) * 15, 2_000_000_000, 400_000))
        .collect();
    let now = samples.last().unwrap().sampled_at;
    // Ask for an anchor an hour before the first sample.
    let then_at = samples[0].sampled_at - Duration::seconds(3600);
    let d = compute(now, then_at, "1h", None, None, &samples, &[], &cfg());
    assert_eq!(d.anchor_at, Some(samples[0].sampled_at));
    assert!(d.actual_lookback_secs.unwrap() < d.requested_lookback_secs);
    assert!(d.summary.contains("asked for"), "sentence: {}", d.summary);
}

/// The 2026-09-05 shape in one word: memory was freed and the machine did not
/// get better, so swap "still" grew. Exercised through `sentence` directly so the
/// level arithmetic is not what is under test.
#[test]
fn the_sentence_says_still_when_freeing_did_not_help() {
    let verdict = VerdictDelta {
        then_level: Level::Restless,
        now_level: Level::Wailing,
        then_source: None,
        now_source: None,
        detail: verdict_words(Level::Restless, Level::Wailing),
    };
    let dimensions = vec![DimensionDelta {
        dimension: Dimension::Swap,
        key: "swap".into(),
        label: "Swap".into(),
        unit: Unit::Bytes,
        then: 12_000_000_000.0,
        now: 13_300_000_000.0,
        delta: 1_300_000_000.0,
        then_at: at(0),
        now_at: at(300),
        observation_gap_secs: None,
        detail: String::new(),
    }];
    let consumers = vec![ConsumerDelta {
        name: "Chrome".into(),
        kind: ConsumerKind::AppGroup,
        then_count: 128,
        now_count: 34,
        then_bytes: 8_000_000_000,
        now_bytes: 4_000_000_000,
        delta_bytes: -4_000_000_000,
        detail: consumer_words("Chrome", 128, 34, 8_000_000_000, 4_000_000_000),
    }];
    let s = sentence(
        "1h",
        3600,
        Some(3600),
        Some(&verdict),
        &dimensions,
        &consumers,
        None,
        &cfg(),
    );
    assert!(s.contains("freed 4.0 GB from Chrome"), "sentence: {s}");
    assert!(s.contains("swap still grew 1.3 GB"), "sentence: {s}");
    assert!(s.contains("Restless → Wailing"), "sentence: {s}");
}
