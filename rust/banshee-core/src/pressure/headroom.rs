// pressure/headroom — the verdict, reduced to what an agent should DO (ADR-0010).
//
// A DECISION, not a number. Every system-monitor MCP server surveyed for 1.0
// returns raw metrics with threshold flags and leaves the agent to decide what
// they mean for the thing it is about to do — and the agent gets that wrong in
// two repeatable ways: it reads "no verdict yet" as headroom, and it re-derives
// a parallelism from raw numbers that disagrees with the human's menu bar. This
// module answers the question instead: should you wait, how many more workers
// can the machine absorb, when to ask again, and — in Kubernetes node-condition
// shape — why.
//
// A PURE FUNCTION of the `Pressure` the model already produced plus the newest
// sample's core count (ADR-0005): computed once per evaluation, published beside
// the verdict, served on HTTP, the CLI and MCP as one parity-table row. No client
// re-derives any of it, and nothing here is persisted — like the verdict it is
// recomputed from history.
//
// NEVER BLOCKS BY DEFAULT. `shouldWait` is advice with a reason attached; the
// hook built on this read should return `ask`, never `deny`. Banshee informs, the
// agent's human decides.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::Pressure;
use super::band::Band;
use super::config::{ALL_DIMENSIONS, Dimension, PressureConfig, Source};
use super::level::Level;

/// Kubernetes' tri-state, spelled the way Kubernetes spells it on the wire.
///
/// `Unknown` is load-bearing: a source with no reading yet (no census taken, a
/// pre-v11 thermal column) is neither "under pressure" nor "fine", and a client
/// that had only two states would have to pick one — which is how "no data"
/// becomes "all clear".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ConditionStatus {
    True,
    False,
    Unknown,
}

/// One statement about the machine, in the shape an agent already knows from
/// `kubectl describe node`: `{type, status, reason, message, since}`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Condition {
    /// `Ready`, or `<Source>Pressure` — see `CONDITION_SOURCES` for the order.
    #[serde(rename = "type")]
    pub kind: String,
    pub status: ConditionStatus,
    /// A CamelCase token naming the worst dimension and its band (`SwapRed`,
    /// `CpuYellow`), or `AllGreen` / `NoReading`. Stable; branch on it.
    pub reason: String,
    /// The worst dimension's `detail` string from the verdict, VERBATIM — the same
    /// words the popover and `banshee status` show (ADR-0005).
    pub message: String,
    /// When the worst dimension entered its current band: `evaluatedAt − heldSecs`.
    /// Held time stops at a gap in observation (`banshee-s6s`), so this can be
    /// later than the true transition — the same honesty the verdict has. Null
    /// when the status is `Unknown`.
    #[serde(with = "crate::wire_time::option")]
    pub since: Option<DateTime<Utc>>,
}

/// The answer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Headroom {
    #[serde(with = "crate::wire_time")]
    pub evaluated_at: DateTime<Utc>,
    /// The level this was derived from. `checking` means the daemon has not
    /// gathered enough samples — and `should_wait` is then true, because headroom
    /// is never invented before data exists.
    pub level: Level,
    /// Do not start new work right now. Advice, not enforcement (ADR-0010 dec. 4).
    /// Exactly `recommended_parallelism == 0`, by construction.
    pub should_wait: bool,
    /// How many MORE concurrent CPU-bound workers the machine can absorb right
    /// now. Zero exactly when `should_wait`.
    pub recommended_parallelism: u32,
    /// Seconds before asking again. Present exactly when `should_wait`; null
    /// when there is nothing to wait for.
    pub retry_after_secs: Option<u64>,
    /// The core count the arithmetic used, from the newest sample. Null before
    /// the first sample.
    pub cores: Option<u32>,
    /// One sentence a hook can print and a person can read: the level, the cause,
    /// the number. Composed here so every surface says the same words.
    pub reason: String,
    /// `Ready` first, then one per source in `CONDITION_SOURCES` order.
    pub conditions: Vec<Condition>,
}

/// The sources that get a condition, in the order they appear on the wire. Fixed
/// so a client can index by position as well as by `type`, and so the array
/// length is a constant the e2e harness can assert.
pub const CONDITION_SOURCES: [Source; 6] = [
    Source::Cpu,
    Source::Memory,
    Source::Thermal,
    Source::Disk,
    Source::Sprawl,
    Source::ManagedAgents,
];

/// The sources whose RED zeroes the recommendation regardless of level (ADR-0010
/// decision 5). Memory because adding work adds pressure on the thing already
/// failing; thermal because a throttled CPU makes the load arithmetic a lie;
/// disk because builds write. CPU needs no entry — at red utilization the
/// free-core arithmetic `cores × (1 − utilization)` is already ~0. Sprawl and
/// managed agents are worklist items, not
/// capacity, and a fresh red on them does not stop new work.
const ZEROING_SOURCES: [Source; 3] = [Source::Memory, Source::Thermal, Source::Disk];

/// What the arithmetic needs to know about the machine itself, from the newest
/// sample. Neither figure is in the verdict: the verdict is about pressure, and
/// these are about capacity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MachineFacts {
    pub cores: u32,
    pub mem_total_bytes: u64,
}

/// Reduce a verdict to a decision.
///
/// `machine` comes from the newest sample; `None` means no sample has been taken,
/// which — like `Level::Checking` — is "wait", never an invented capacity.
pub fn derive(p: &Pressure, machine: Option<MachineFacts>, config: &PressureConfig) -> Headroom {
    let conditions = conditions_for(p);

    let Some(machine) = machine.filter(|_| p.level != Level::Checking) else {
        let retry = config.headroom_retry_checking_secs;
        return Headroom {
            evaluated_at: p.evaluated_at,
            level: p.level,
            should_wait: true,
            recommended_parallelism: 0,
            retry_after_secs: Some(retry),
            cores: machine.map(|m| m.cores),
            reason: format!(
                "{}: not enough samples for a verdict yet — this is not headroom. \
                 Wait; retry in {retry}s.",
                Level::Checking.name()
            ),
            conditions,
        };
    };

    let cores = machine.cores;

    // The cause that forces a wait, if any: a sustained red (the level itself),
    // or a fresh red on a source that zeroes the answer.
    let sustained = p.level >= Level::Wailing;
    let zeroing_red = worst_reading(p, |r| {
        r.band == Band::Red && ZEROING_SOURCES.contains(&r.dimension.source())
    });
    if sustained || zeroing_red.is_some() {
        let retry = match p.level {
            Level::Shrieking => config.headroom_retry_shrieking_secs,
            Level::Wailing => config.headroom_retry_wailing_secs,
            _ => config.headroom_retry_red_secs,
        };
        // Name the dominant source's worst dimension when the LEVEL is the cause
        // (it is what the menu bar names too); otherwise the red that zeroed it.
        let cause = if sustained {
            p.source
                .and_then(|s| {
                    worst_reading(p, |r| r.dimension.source() == s && r.band > Band::Green)
                })
                .or(zeroing_red)
        } else {
            zeroing_red
        };
        let cause_text = match cause {
            Some(r) => format!(
                "{} is {} — {} (held {})",
                r.label,
                band_word(r.band),
                r.detail,
                fmt_held(r.held_secs)
            ),
            None => "the level alone says so".to_string(),
        };
        return Headroom {
            evaluated_at: p.evaluated_at,
            level: p.level,
            should_wait: true,
            recommended_parallelism: 0,
            retry_after_secs: Some(retry),
            cores: Some(cores),
            reason: format!("{}: {cause_text}. Wait; retry in {retry}s.", p.level.name()),
            conditions,
        };
    }

    // Free capacity from the two resources a worker actually consumes. The CPU
    // term is `cores × (1 − utilization)` = the idle cores, honest now that the
    // CPU value IS measured utilization rather than load-per-core (`banshee-87l.19`):
    // a machine with a deep run queue over idle cores no longer reports zero free
    // cores, because the cores really are free.
    let cpu = p.reading(Dimension::Cpu);
    let cpu_slots = match cpu {
        Some(r) if r.value.is_finite() => {
            ((f64::from(cores) * (1.0 - r.value)).max(0.0)).floor() as u32
        }
        _ => cores,
    };
    // Memory: what is available MINUS the share of the machine agents may never
    // fill (`headroom_memory_reserve_fraction` of total — the human's and the
    // OS's, the way Bazel keeps a third and the Kubernetes providers reserve a
    // quarter of a laptop-sized node), divided by the budget per worker (2 GiB:
    // a coding agent process at 0.4–0.9 GB plus the build it spawns; Gentoo's
    // compile figure is 1.5–2 GB). Decided 2026-09-08 on published prior art.
    let mem = p.reading(Dimension::Memory);
    let mem_slots = match mem {
        Some(r) if r.value.is_finite() && config.headroom_memory_per_slot_bytes > 0 => {
            let reserve = machine.mem_total_bytes as f64
                * config.headroom_memory_reserve_fraction.clamp(0.0, 1.0);
            let spendable = (r.value - reserve).max(0.0);
            Some((spendable / config.headroom_memory_per_slot_bytes as f64).floor() as u32)
        }
        _ => None,
    };
    let mut slots = mem_slots.map_or(cpu_slots, |m| cpu_slots.min(m));

    // The level caps it: the more uneasy the machine, the less of it an agent
    // should take. Quiet is the whole machine.
    let cap = match p.level {
        Level::Stirring => cores.div_ceil(2),
        Level::Restless => cores.div_ceil(4),
        _ => cores,
    };
    slots = slots.min(cap);

    // Thermal MODERATE: the fans are at work and the machine is spending capacity
    // on cooling. Halve, rounding up.
    if p.reading(Dimension::Thermal)
        .is_some_and(|r| r.band == Band::Yellow)
    {
        slots = slots.div_ceil(2);
    }

    // A machine that is not in trouble can always run one more thing. This is
    // what makes `should_wait == (recommended_parallelism == 0)` hold.
    let slots = slots.max(1);

    let mut facts: Vec<String> = Vec::new();
    if let Some(r) = cpu {
        facts.push(r.detail.clone());
    }
    if let Some(r) = mem {
        facts.push(r.detail.clone());
    }
    let facts = if facts.is_empty() {
        String::new()
    } else {
        format!(" — {}", facts.join("; "))
    };

    Headroom {
        evaluated_at: p.evaluated_at,
        level: p.level,
        should_wait: false,
        recommended_parallelism: slots,
        retry_after_secs: None,
        cores: Some(cores),
        reason: format!(
            "{}: room for {slots} more worker{} on {cores} cores{facts}.",
            p.level.name(),
            if slots == 1 { "" } else { "s" }
        ),
        conditions,
    }
}

/// `Ready`, then one condition per source in `CONDITION_SOURCES` order.
fn conditions_for(p: &Pressure) -> Vec<Condition> {
    let mut out = Vec::with_capacity(1 + CONDITION_SOURCES.len());
    let ready = p.level != Level::Checking;
    out.push(Condition {
        kind: "Ready".to_string(),
        status: if ready {
            ConditionStatus::True
        } else {
            ConditionStatus::False
        },
        reason: if ready {
            "VerdictAvailable"
        } else {
            "Checking"
        }
        .to_string(),
        message: if ready {
            format!(
                "{} samples, {} censuses in the window",
                p.sample_count, p.census_count
            )
        } else {
            "not enough samples for a verdict yet".to_string()
        },
        since: None,
    });
    for source in CONDITION_SOURCES {
        // Uptime informs findings, never conditions: a 12-day uptime must not make
        // `MemoryPressure` read `UptimeYellow`. But the OTHER advisory dimensions —
        // availability, kernel reclaimability, compressor occupancy — ARE genuine
        // memory-pressure signals for the headroom decision (availability even
        // drives the per-slot math directly, below), so only Uptime is excluded
        // here, not everything `is_advisory`. `banshee-yk8` made Memory advisory
        // for the LEVEL without it ceasing to matter for headroom.
        let mine = |r: &super::DimensionReading| {
            r.dimension.source() == source && r.dimension != Dimension::Uptime
        };
        // Something out of band names itself: the worst dimension wins. When
        // EVERYTHING is green, "worst by severity" is meaningless — found live,
        // where an all-green memory source reported `peak 0 swap ops/sec` because
        // thrash at zero sits closer to its yellow line than 7 GB available does
        // to its own. The green message is the source's PRIMARY dimension, the
        // first declared (available memory, not thrash; agents, not orphans).
        let worst = worst_reading(p, |r| mine(r) && r.band > Band::Green)
            .or_else(|| p.dimensions.iter().find(|r| mine(r)));
        out.push(match worst {
            None => Condition {
                kind: condition_type(source),
                status: ConditionStatus::Unknown,
                reason: "NoReading".to_string(),
                message: format!("no {} reading yet", source.label()),
                since: None,
            },
            Some(r) => Condition {
                kind: condition_type(source),
                status: if r.band == Band::Red {
                    ConditionStatus::True
                } else {
                    ConditionStatus::False
                },
                reason: if r.band == Band::Green {
                    "AllGreen".to_string()
                } else {
                    format!("{}{}", camel(r.dimension.key()), band_word_camel(r.band))
                },
                message: r.detail.clone(),
                since: Some(
                    p.evaluated_at
                        - chrono::Duration::seconds(r.held_secs.min(i64::MAX as u64) as i64),
                ),
            },
        });
    }
    out
}

fn condition_type(source: Source) -> String {
    let name = match source {
        Source::Cpu => "Cpu",
        Source::Memory => "Memory",
        Source::Thermal => "Thermal",
        Source::Disk => "Disk",
        Source::Sprawl => "Sprawl",
        Source::ManagedAgents => "ManagedAgents",
    };
    format!("{name}Pressure")
}

/// The worst reading matching `pick`: worst band first, then furthest past its
/// own line, then declaration order — the same ordering `dominant_source` uses,
/// so a condition names the dimension the menu bar would.
fn worst_reading(
    p: &Pressure,
    pick: impl Fn(&super::DimensionReading) -> bool,
) -> Option<&super::DimensionReading> {
    let index_of = |d: Dimension| {
        ALL_DIMENSIONS
            .iter()
            .position(|x| *x == d)
            .unwrap_or(usize::MAX)
    };
    p.dimensions.iter().filter(|r| pick(r)).max_by(|a, b| {
        a.band
            .cmp(&b.band)
            .then_with(|| {
                a.severity
                    .partial_cmp(&b.severity)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .then_with(|| index_of(b.dimension).cmp(&index_of(a.dimension)))
    })
}

fn band_word(b: Band) -> &'static str {
    match b {
        Band::Green => "green",
        Band::Yellow => "yellow",
        Band::Red => "red",
    }
}

fn band_word_camel(b: Band) -> &'static str {
    match b {
        Band::Green => "Green",
        Band::Yellow => "Yellow",
        Band::Red => "Red",
    }
}

/// `kernelPressure` → `KernelPressure`.
fn camel(key: &str) -> String {
    let mut c = key.chars();
    match c.next() {
        Some(first) => first.to_uppercase().chain(c).collect(),
        None => String::new(),
    }
}

/// Held time at a human scale, for the reason sentence.
fn fmt_held(secs: u64) -> String {
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m", secs / 60)
    } else {
        format!("{}h{}m", secs / 3600, (secs % 3600) / 60)
    }
}

#[cfg(test)]
mod tests;
