// who — the "who" line: which processes or app groups are behind a finding.
//
// Naming the culprit is the action. A verdict that says "35.6 GB of swap in use"
// tells a person the machine is in trouble; "who: Chrome ×115 at 7.7 GB, claude
// ×8 at 2.1 GB" tells them what to do about it. The census already knows every
// name this needs — this module RANKS what the census recorded and never spawns
// anything, reads no sysctl, and takes no measurement of its own (ADR-0007).
//
// Composed ONCE here and shipped on every finding (ADR-0005), so the popover,
// `banshee status`, the MCP tool and the alert text all name the same consumers
// in the same words. A client that ranked the census itself would be a second
// implementation of the attribution rules, and the rules are not obvious:
//
// - CPU is attributed by the census's CUMULATIVE-CPU rule (Σ cpu seconds ÷ the
//   longest lifetime in the group), never by summing per-group percentages —
//   each managed-agent group has its own denominator and their sum is
//   meaningless arithmetic (`Census::monitor_total`; the never-sum rule in
//   docs/signal-collection.md).
//   The managed agents appear as ONE entry, the deduplicated total.
// - A cumulative-CPU ratio over a short lifetime is noise (a log shipper 109 s
//   old measured 14.6% off 16 s of CPU), so a group younger than
//   `corporate_min_life_secs` is not named for CPU at all.
// - Memory, swap, kernel pressure and thrash are attributed by RESIDENT size.
//   RSS is the wrong lens for a machine already deep in swap (the swapped-out
//   pages are exactly what it does not count — `banshee-75z` is the follow-up),
//   but it is the census's honest measure of what a quit-and-relaunch reclaims.
// - Stale sessions and orphans are attributed by what reaping them frees.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::config::{Dimension, PressureConfig};
use super::fmt_bytes;
use crate::census::{AgentSession, Census};

/// One named consumer behind a finding: a process group and what it costs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Consumer {
    /// The census's name for it: an app group, an agent CLI program, an orphaned
    /// helper's program, or one of the rollups (`managed agents`, `IDE agent
    /// helpers`, `orphaned helpers`).
    pub name: String,
    /// Processes in the group.
    pub count: u32,
    /// The cost, in the dimension's own terms and already worded: `7.7 GB`,
    /// `29% of one core`. Composed here so the line reads the same everywhere.
    pub detail: String,
}

/// The wording of one consumer inside the line: `Chrome ×115 at 7.7 GB`.
impl std::fmt::Display for Consumer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} ×{} at {}", self.name, self.count, self.detail)
    }
}

/// The names behind a dimension, biggest first, at most `config.who_limit`.
///
/// Empty for dimensions that have no process behind them — disk (ADR-0003: what
/// is taking the space is a disk tool's question) and uptime — and for any
/// dimension when the census recorded nothing attributable.
pub fn attribute(d: Dimension, census: &Census, config: &PressureConfig) -> Vec<Consumer> {
    let ranked = match d {
        Dimension::Memory
        | Dimension::Swap
        | Dimension::KernelPressure
        | Dimension::Thrash
        // The second-pass memory signals name the same suspects: the biggest resident
        // consumers are what to relaunch to free RAM, whether the pressure shows
        // as swap, a kernel verdict, low reclaimability, a full compressor pool,
        // or a jetsam kill.
        | Dimension::KernelFree
        | Dimension::Compressor
        | Dimension::Jetsam => by_resident_size(census),
        // Heat is the work: naming the CPU consumers is the honest answer to
        // "what is making it hot", even though the lever is physical.
        Dimension::Cpu | Dimension::Thermal => by_recent_cpu_rate(census)
            // Empty on the first census after a (re)start, or when no pid is
            // shared with the previous one — never a census with data. Fall back
            // to the cumulative lens so the who-line names SOMEONE rather than
            // regressing to naming nobody the moment the daemon restarts.
            .unwrap_or_else(|| by_cumulative_cpu(census, config)),
        Dimension::Agents => stale_sessions_by_program(census),
        Dimension::Orphans => orphans_by_program(census),
        Dimension::Corporate => managed_agents_by_rate(census, config),
        Dimension::Disk | Dimension::Uptime => Vec::new(),
    };
    ranked
        .into_iter()
        .take(config.who_limit)
        .map(Ranked::into_consumer)
        .collect()
}

/// `who: Chrome ×115 at 7.7 GB, claude ×8 at 2.1 GB` — or `None` when there is
/// nobody to name, so a surface can omit the line rather than print `who:` alone.
pub fn who_line(consumers: &[Consumer]) -> Option<String> {
    if consumers.is_empty() {
        return None;
    }
    let names: Vec<String> = consumers.iter().map(ToString::to_string).collect();
    Some(format!("who: {}", names.join(", ")))
}

/// A candidate with its NUMERIC measure, so ranking never depends on the rounded
/// wording (two groups at 1.06 and 1.14 GB both read "1.1 GB" and must still
/// sort by size). Worded into a `Consumer` only after the cut.
struct Ranked {
    name: String,
    count: u32,
    measure: f64,
    unit: Measure,
}

#[derive(Clone, Copy)]
enum Measure {
    Bytes,
    PercentOfOneCore,
}

impl Ranked {
    /// `None` for a group that costs nothing: a zero-RSS rollup or a group with no
    /// CPU time is not a consumer and would only pad the line.
    fn new(name: impl Into<String>, count: u32, measure: f64, unit: Measure) -> Option<Self> {
        (count > 0 && measure > 0.0).then(|| Ranked {
            name: name.into(),
            count,
            measure,
            unit,
        })
    }

    fn into_consumer(self) -> Consumer {
        Consumer {
            name: self.name,
            count: self.count,
            detail: match self.unit {
                Measure::Bytes => fmt_bytes(self.measure),
                Measure::PercentOfOneCore => format!("{:.0}% of one core", self.measure),
            },
        }
    }
}

/// Biggest measure first; equal measures by name, so the line is stable across
/// censuses that differ only in ordering.
fn rank(mut out: Vec<Ranked>) -> Vec<Ranked> {
    out.sort_by(|a, b| {
        b.measure
            .partial_cmp(&a.measure)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.name.cmp(&b.name))
    });
    out
}

/// Memory-shaped dimensions: everything the census sized, ranked by RSS.
///
/// App groups, interactive agent sessions grouped by program, and the three
/// rollups the census keeps as totals. The groups may overlap in principle (an
/// app-group pattern could match an agent CLI); the census's own lists have the
/// same property and the overlap is a configuration choice, not a bug here.
fn by_resident_size(census: &Census) -> Vec<Ranked> {
    let mut out: Vec<Ranked> = census
        .app_groups
        .iter()
        .filter_map(|g| Ranked::new(&g.name, g.proc_count, g.rss_bytes as f64, Measure::Bytes))
        .collect();

    for (program, (count, rss)) in sessions_by_program(census, |_| true) {
        out.extend(Ranked::new(program, count, rss as f64, Measure::Bytes));
    }
    out.extend(Ranked::new(
        "IDE agent helpers",
        census.ide_helpers.count,
        census.ide_helpers.rss_bytes as f64,
        Measure::Bytes,
    ));
    out.extend(Ranked::new(
        "orphaned helpers",
        census.orphans.orphan_count,
        census.orphans.orphan_rss_bytes as f64,
        Measure::Bytes,
    ));
    out.extend(Ranked::new(
        "managed agents",
        census.monitor_total.proc_count,
        census.monitor_total.rss_bytes as f64,
        Measure::Bytes,
    ));
    rank(out)
}

/// CPU by RECENT rate: the consumers the census computed from the CPU-time delta
/// between the last two censuses (`census::recent_cpu_consumers`, `banshee-aen`).
/// This is the honest answer to "what is hot NOW" — a process that burned a core
/// for the last five minutes outranks one that has a large cumulative total from
/// an hour of startup work and is now idle. Already ranked and capped by core;
/// `rank` here only re-sorts on the unrounded percent so the wording never
/// decides the order.
///
/// `None` (not an empty `Vec`) when the census carries no recent-rate consumers,
/// so the caller can distinguish "measured, nobody" from "could not measure yet"
/// and fall back to the cumulative lens.
fn by_recent_cpu_rate(census: &Census) -> Option<Vec<Ranked>> {
    if census.cpu_consumers.is_empty() {
        return None;
    }
    Some(rank(
        census
            .cpu_consumers
            .iter()
            .filter_map(|c| {
                Ranked::new(
                    &c.name,
                    c.proc_count,
                    c.percent_of_one_core,
                    Measure::PercentOfOneCore,
                )
            })
            .collect(),
    ))
}

/// CPU: interactive agent sessions by program under the cumulative-CPU rule, and
/// the managed agents as ONE deduplicated entry. Nothing else in the census
/// carries CPU time (app groups and orphans are sized, not timed).
fn by_cumulative_cpu(census: &Census, config: &PressureConfig) -> Vec<Ranked> {
    let min_life = config.corporate_min_life_secs;

    // (count, Σ cpu_secs, max lifetime) per program.
    let mut groups: BTreeMap<&str, (u32, f64, u64)> = BTreeMap::new();
    for s in &census.agent_sessions {
        let e = groups.entry(s.program.as_str()).or_insert((0, 0.0, 0));
        e.0 += 1;
        e.1 += s.cpu_secs;
        e.2 = e.2.max(s.age_secs);
    }
    let mut out: Vec<Ranked> = groups
        .into_iter()
        // The same guard `Dimension::Corporate` applies: a ratio whose
        // denominator is a lifetime of seconds is a startup burst, not a rate.
        // `life > 0` also keeps the division honest when min_life is tuned to 0.
        .filter(|(_, (_, _, life))| *life >= min_life && *life > 0)
        .filter_map(|(program, (count, cpu, life))| {
            Ranked::new(
                program,
                count,
                cpu * 100.0 / life as f64,
                Measure::PercentOfOneCore,
            )
        })
        .collect();

    let total = &census.monitor_total;
    if total.longest_life_secs >= min_life {
        out.extend(Ranked::new(
            "managed agents",
            total.proc_count,
            total.percent_of_one_core,
            Measure::PercentOfOneCore,
        ));
    }
    rank(out)
}

/// Stale sessions grouped by program, sized by the RSS reaping them frees.
fn stale_sessions_by_program(census: &Census) -> Vec<Ranked> {
    rank(
        sessions_by_program(census, |s| s.is_stale)
            .into_iter()
            .filter_map(|(program, (count, rss))| {
                Ranked::new(program, count, rss as f64, Measure::Bytes)
            })
            .collect(),
    )
}

/// Orphans by program, in the census's own order (count descending): the count
/// is what the dimension bands on, so the biggest count is the biggest cause.
fn orphans_by_program(census: &Census) -> Vec<Ranked> {
    census
        .orphans
        .orphans_by_program
        .iter()
        .filter_map(|p| Ranked::new(&p.program, p.count, p.rss_bytes as f64, Measure::Bytes))
        .collect()
}

/// Managed-agent groups by their own rate. Ranked, never summed: each figure
/// has its own denominator and the finding's headline number is
/// `monitor_total`, not this list.
fn managed_agents_by_rate(census: &Census, config: &PressureConfig) -> Vec<Ranked> {
    rank(
        census
            .monitor_agents
            .iter()
            .filter(|a| a.longest_life_secs >= config.corporate_min_life_secs)
            .filter_map(|a| {
                Ranked::new(
                    &a.name,
                    a.proc_count,
                    a.percent_of_one_core,
                    Measure::PercentOfOneCore,
                )
            })
            .collect(),
    )
}

/// (count, Σ rss) per agent program, over the sessions `keep` admits. Shared
/// with `deltas::summarise`, so the census summary an episode stores groups
/// sessions exactly as the who-line does.
pub(super) fn sessions_by_program(
    census: &Census,
    keep: impl Fn(&AgentSession) -> bool,
) -> BTreeMap<&str, (u32, u64)> {
    let mut groups: BTreeMap<&str, (u32, u64)> = BTreeMap::new();
    for s in census.agent_sessions.iter().filter(|s| keep(s)) {
        let e = groups.entry(s.program.as_str()).or_insert((0, 0));
        e.0 += 1;
        e.1 = e.1.saturating_add(s.rss_bytes);
    }
    groups
}
