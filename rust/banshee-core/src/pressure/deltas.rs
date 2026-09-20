// pressure/deltas — what CHANGED: the newest census and verdict against a
// lookback, composed once and served on every surface (`banshee-32g`).
//
// The first question a person asks after taking an action is "did it help, and
// if not, what ate it". Measured 2026-09-05: Chrome dropped from 8.9 GB to 4.9 GB
// after Memory Saver was enabled and the machine got WORSE (Restless → Wailing),
// because the freed memory was absorbed elsewhere (agent sessions 8 → 10, 1.5 →
// 2.1 GB) and swap never gives itself back. Answering that took hand-diffing two
// census payloads pulled minutes apart. The tool stored both and never compared
// them. This module is the comparison.
//
// A PURE FUNCTION of stored history (ADR-0005): the samples and censuses in the
// window, a lookback, and the config. Nothing here is persisted and nothing is
// measured — the daemon already did both. Two things the arithmetic must not
// pretend:
//
// - **A delta across a gap in observation is arithmetic on a hole.** A laptop that
//   slept for forty minutes has a forty-minute hole in every series; "swap grew
//   1.3 GB vs 1h ago" is still a true difference between two readings, but the
//   reader must be told nobody was watching in between. The gap rule is the one
//   `held_secs` uses (`Series::max_interval`, `banshee-s6s`), applied to the range
//   between the two compared points, and it is REPORTED on the wire, per
//   dimension and at the top, never absorbed.
// - **"Used memory went down" is not "pressure went down" on macOS.** Swap and
//   the compressor do not release what they took; a freed 4 GB can be absorbed by
//   the next allocator in seconds. So the read carries the VERDICT at both ends —
//   the level the model gives the history as it stood at the anchor, and now —
//   beside the byte deltas, and the sentence says both.
//
// The `CensusSummary` here is also what an episode stores as `censusAtPeak`
// (ADR-0009 decision 1): the who-line's inputs, a few hundred bytes, so a future
// "what changed since the peak" can be computed after the censuses behind the
// incident have been swept.

use std::collections::BTreeMap;

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::config::{ALL_DIMENSIONS, Dimension, PressureConfig, Source, Unit};
use super::level::Level;
use super::who::sessions_by_program;
use super::{evaluate, fmt_bytes, kernel_level_name, series_for, thermal_level_name};
use crate::census::Census;
use crate::sample::Sample;

// ---- the census summary (stored in episodes, compared by deltas) ------------

/// Which kind of thing a summarised consumer is. Carried so a delta can say
/// "sessions" for `claude` and "app" for Chrome without re-deriving it from the
/// name, and so an app group and an agent program that happen to share a name
/// stay two rows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ConsumerKind {
    /// A configured app process group (Chrome, Slack…), sized by RSS.
    AppGroup,
    /// Interactive agent CLI sessions grouped by program (claude, kiro-cli…).
    Sessions,
    /// One of the census's rollups: `IDE agent helpers`, `orphaned helpers`,
    /// `managed agents`.
    Rollup,
}

/// One named consumer and its size at one instant.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConsumerSize {
    pub name: String,
    pub kind: ConsumerKind,
    pub count: u32,
    pub rss_bytes: u64,
}

/// A census reduced to the who-line's inputs: every sized consumer, biggest
/// first, plus the two scalars the sprawl and managed-agents dimensions band on.
///
/// Small enough to store inside an episode row for a year (ADR-0006) and
/// complete enough to diff against a live census. Zero-sized consumers are
/// omitted — absence IS zero to the comparison, and a row that costs nothing is
/// not a consumer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CensusSummary {
    #[serde(with = "crate::wire_time")]
    pub taken_at: DateTime<Utc>,
    pub total_procs: u32,
    pub consumers: Vec<ConsumerSize>,
    pub orphan_count: u32,
    /// The deduplicated managed-agent figure (`Census::monitor_total`), never a
    /// sum of per-group percentages.
    pub monitor_percent_of_one_core: f64,
}

/// Reduce a census to its summary. The grouping is the who-line's
/// (`who::by_resident_size`): app groups by RSS, sessions by program, and the
/// three rollups under the same names the who-line uses, so "Chrome" in a
/// finding and "Chrome" in a delta are the same Chrome.
pub fn summarise(census: &Census) -> CensusSummary {
    let mut consumers: Vec<ConsumerSize> = census
        .app_groups
        .iter()
        .filter(|g| g.proc_count > 0 && g.rss_bytes > 0)
        .map(|g| ConsumerSize {
            name: g.name.clone(),
            kind: ConsumerKind::AppGroup,
            count: g.proc_count,
            rss_bytes: g.rss_bytes,
        })
        .collect();
    for (program, (count, rss)) in sessions_by_program(census, |_| true) {
        if count > 0 && rss > 0 {
            consumers.push(ConsumerSize {
                name: program.to_string(),
                kind: ConsumerKind::Sessions,
                count,
                rss_bytes: rss,
            });
        }
    }
    for (name, count, rss) in [
        (
            "IDE agent helpers",
            census.ide_helpers.count,
            census.ide_helpers.rss_bytes,
        ),
        (
            "orphaned helpers",
            census.orphans.orphan_count,
            census.orphans.orphan_rss_bytes,
        ),
        (
            "managed agents",
            census.monitor_total.proc_count,
            census.monitor_total.rss_bytes,
        ),
    ] {
        if count > 0 && rss > 0 {
            consumers.push(ConsumerSize {
                name: name.to_string(),
                kind: ConsumerKind::Rollup,
                count,
                rss_bytes: rss,
            });
        }
    }
    // Biggest first; ties by name, so two censuses that differ only in ordering
    // summarise identically.
    consumers.sort_by(|a, b| {
        b.rss_bytes
            .cmp(&a.rss_bytes)
            .then_with(|| a.name.cmp(&b.name))
    });
    CensusSummary {
        taken_at: census.taken_at,
        total_procs: census.total_procs,
        consumers,
        orphan_count: census.orphans.orphan_count,
        monitor_percent_of_one_core: census.monitor_total.percent_of_one_core,
    }
}

// ---- the lookback ----------------------------------------------------------

/// What a caller asked to compare against.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lookback {
    /// This long ago.
    Ago(Duration),
    /// Since the earliest OPEN episode started — "what changed since this
    /// began". Resolved by the caller, who has the episode record.
    Episode,
}

/// Parse a `vs` parameter: `30m`, `1h`, `24h`, `90s`, bare seconds, or
/// `episode`. Bounded below at one minute (two samples) and above at
/// `deltas_max_lookback_secs` — above the ceiling is refused with the bound
/// named, never clamped: an agent that asks for a week and silently gets a day
/// reasons about a window it did not get (the `limit` rule, docs/wire-format.md).
pub fn parse_lookback(raw: &str, config: &PressureConfig) -> Result<Lookback, String> {
    let s = raw.trim();
    if s.eq_ignore_ascii_case("episode") {
        return Ok(Lookback::Episode);
    }
    let (digits, mult) = match s.chars().last() {
        Some('s') => (&s[..s.len() - 1], 1),
        Some('m') => (&s[..s.len() - 1], 60),
        Some('h') => (&s[..s.len() - 1], 3600),
        Some('d') => (&s[..s.len() - 1], 86_400),
        _ => (s, 1),
    };
    let n: u64 = digits.parse().ok().filter(|n| *n > 0).ok_or_else(|| {
        format!("vs must be a lookback such as 30m, 1h or 24h, or `episode` (got {raw:?})")
    })?;
    let secs = n.saturating_mul(mult);
    if secs < MIN_LOOKBACK_SECS {
        return Err(format!(
            "vs must be at least {MIN_LOOKBACK_SECS}s — two samples — (got {raw:?})"
        ));
    }
    if secs > config.deltas_max_lookback_secs {
        return Err(format!(
            "vs must not exceed {} — the raw retention; older history is rollups with no census behind it (got {raw:?})",
            fmt_secs(config.deltas_max_lookback_secs)
        ));
    }
    Ok(Lookback::Ago(Duration::seconds(secs as i64)))
}

/// The shortest lookback that can hold two samples at the production cadence.
pub const MIN_LOOKBACK_SECS: u64 = 60;

/// The default lookback as the label a caller would have typed, so the route
/// echoes `"1h"` rather than `"3600"` when `vs` is omitted — one spelling of
/// the window on the wire.
pub fn default_lookback_label(config: &PressureConfig) -> String {
    fmt_secs(config.deltas_default_lookback_secs)
}

// ---- the wire shape --------------------------------------------------------

/// The verdict at both ends of the comparison — the half of the answer bytes
/// cannot give. `then` is what the model says about the history as it stood at
/// the anchor; a pure function, so it is exactly the level the daemon would have
/// published had it evaluated at that instant.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VerdictDelta {
    pub then_level: Level,
    pub now_level: Level,
    pub then_source: Option<Source>,
    pub now_source: Option<Source>,
    /// `pressure Restless → Wailing`, or `pressure unchanged at Restless`.
    pub detail: String,
}

/// One dimension's value at both ends, and what lies between.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DimensionDelta {
    pub dimension: Dimension,
    pub key: String,
    pub label: String,
    pub unit: Unit,
    pub then: f64,
    pub now: f64,
    /// `now − then`, in the dimension's own unit.
    pub delta: f64,
    /// The observation actually compared against — the latest at or before the
    /// anchor, or the earliest there is when the lookback reaches past the
    /// history.
    #[serde(with = "crate::wire_time")]
    pub then_at: DateTime<Utc>,
    #[serde(with = "crate::wire_time")]
    pub now_at: DateTime<Utc>,
    /// The largest hole in THIS series between the two compared points, by the
    /// series' own gap rule. Present-as-null when the range was observed
    /// continuously. A non-null value means the delta is arithmetic across a
    /// period nobody was watching.
    pub observation_gap_secs: Option<u64>,
    /// `swap 12.0 GB → 13.3 GB (+1.3 GB)`, composed here so every surface reads
    /// the same words.
    pub detail: String,
}

/// One consumer at both ends. A consumer present at only one end has a zero
/// count and size at the other — "appeared" or "gone" are both differences.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConsumerDelta {
    pub name: String,
    pub kind: ConsumerKind,
    pub then_count: u32,
    pub now_count: u32,
    pub then_bytes: u64,
    pub now_bytes: u64,
    /// `nowBytes − thenBytes`, signed.
    pub delta_bytes: i64,
    /// `freed 4.0 GB from Chrome (×128 → ×34)`, `claude grew 0.6 GB (×8 → ×10)`.
    pub detail: String,
}

/// Which two censuses the consumer deltas compare.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CensusPair {
    #[serde(with = "crate::wire_time")]
    pub then_at: DateTime<Utc>,
    #[serde(with = "crate::wire_time")]
    pub now_at: DateTime<Utc>,
}

/// The answer: what changed, by how much, across what, and the sentence.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Deltas {
    #[serde(with = "crate::wire_time")]
    pub evaluated_at: DateTime<Utc>,
    /// The lookback as requested — `1h`, `24h`, `episode` — echoed so a caller
    /// reading several answers can tell them apart.
    pub lookback: String,
    /// `episode` anchors carry the episode's id; null otherwise.
    pub episode_id: Option<Uuid>,
    /// Seconds between `evaluatedAt` and the requested anchor.
    pub requested_lookback_secs: u64,
    /// The sample-tier observation actually compared against: the latest sample
    /// at or before the anchor, or the earliest sample there is when the lookback
    /// reaches past the history. Null when there are no samples at all.
    #[serde(with = "crate::wire_time::option")]
    pub anchor_at: Option<DateTime<Utc>>,
    /// `evaluatedAt − anchorAt`; shorter than requested when the history is
    /// shorter than the lookback. Null with `anchorAt`.
    pub actual_lookback_secs: Option<u64>,
    /// Null when there are no samples to evaluate.
    pub verdict: Option<VerdictDelta>,
    /// Every dimension with two distinct observations in reach, in
    /// `ALL_DIMENSIONS` order.
    pub dimensions: Vec<DimensionDelta>,
    /// Every consumer in either compared census, largest absolute change first.
    /// Empty when fewer than two censuses are in reach.
    pub consumers: Vec<ConsumerDelta>,
    /// The two censuses behind `consumers`; null when there were not two.
    pub census: Option<CensusPair>,
    /// The largest `observationGapSecs` across `dimensions` — one field an agent
    /// can check before trusting any number here. Null when every compared range
    /// was observed continuously.
    pub observation_gap_secs: Option<u64>,
    /// The words a person reads: `vs 1h ago: freed 4.0 GB from Chrome; claude
    /// grew 0.6 GB (×8 → ×10); swap still grew 1.3 GB; available memory fell
    /// 0.9 GB; pressure Restless → Wailing.` Composed here (ADR-0005) so the
    /// CLI, the MCP tool and any view say the same sentence.
    pub summary: String,
}

// ---- the computation -------------------------------------------------------

/// Compare now against `then_at`, over stored history.
///
/// `samples` and `censuses` are OLDEST FIRST and should reach back far enough
/// before `then_at` for the anchor verdict to have a window (`window_samples`
/// samples and three censuses). `lookback` is the caller's word for the anchor
/// (`1h`, `episode`), echoed on the wire and used in the sentence.
#[allow(clippy::too_many_arguments)]
pub fn compute(
    now: DateTime<Utc>,
    then_at: DateTime<Utc>,
    lookback: &str,
    episode_id: Option<Uuid>,
    then_census_override: Option<&CensusSummary>,
    samples: &[Sample],
    censuses: &[Census],
    config: &PressureConfig,
) -> Deltas {
    let requested = (now - then_at).num_seconds().max(0) as u64;

    // The sample tier is the primary clock: its anchor is THE anchor.
    let anchor_idx = anchor_index(samples.iter().map(|s| s.sampled_at), then_at);
    let anchor_at = anchor_idx.map(|i| samples[i].sampled_at);
    let actual = anchor_at.map(|a| (now - a).num_seconds().max(0) as u64);

    let verdict = anchor_idx.map(|i| {
        // The history as it stood at the anchor: samples up to and including
        // it, and the three newest censuses taken by then — the same slice the
        // sampler evaluates over, so the level is the one it would have
        // published.
        let then_censuses = last_n_before(censuses, samples[i].sampled_at, 3);
        let then = evaluate(samples[i].sampled_at, &samples[..=i], then_censuses, config);
        let now_censuses = last_n_before(censuses, now, 3);
        let now_p = evaluate(now, samples, now_censuses, config);
        VerdictDelta {
            detail: verdict_words(then.level, now_p.level),
            then_level: then.level,
            now_level: now_p.level,
            then_source: then.source,
            now_source: now_p.source,
        }
    });

    let mut dimensions = Vec::new();
    for d in ALL_DIMENSIONS {
        let Some(series) = series_for(d, samples, censuses, config) else {
            continue;
        };
        let n = series.points.len();
        let Some(then_idx) = anchor_index(series.points.iter().map(|(t, _)| *t), then_at) else {
            continue;
        };
        let now_idx = n - 1;
        if then_idx == now_idx {
            // One observation is not a comparison.
            continue;
        }
        let (then_t, then_v) = series.points[then_idx];
        let (now_t, now_v) = series.points[now_idx];
        dimensions.push(DimensionDelta {
            dimension: d,
            key: d.key().to_string(),
            label: d.label().to_string(),
            unit: d.unit(),
            then: then_v,
            now: now_v,
            delta: now_v - then_v,
            then_at: then_t,
            now_at: now_t,
            observation_gap_secs: series.largest_gap_between(then_idx, now_idx),
            detail: dimension_words(d, then_v, now_v),
        });
    }

    // The stored summary (an episode's `censusAtPeak`) is preferred when the
    // caller has one: it is the record's half of "what changed since this began",
    // and it survives the sweep that takes the live censuses behind a four-hour
    // incident. Without one, compare the two live censuses in reach.
    let then_summary = then_census_override.cloned().or_else(|| {
        anchor_index(censuses.iter().map(|c| c.taken_at), then_at).map(|i| summarise(&censuses[i]))
    });
    let (consumers, census) = match (then_summary, censuses.last()) {
        (Some(then), Some(newest)) if then.taken_at < newest.taken_at => {
            let now_s = summarise(newest);
            (
                consumer_deltas(&then, &now_s),
                Some(CensusPair {
                    then_at: then.taken_at,
                    now_at: now_s.taken_at,
                }),
            )
        }
        _ => (Vec::new(), None),
    };

    let observation_gap_secs = dimensions
        .iter()
        .filter_map(|d| d.observation_gap_secs)
        .max();

    let summary = sentence(
        lookback,
        requested,
        actual,
        verdict.as_ref(),
        &dimensions,
        &consumers,
        observation_gap_secs,
        config,
    );

    Deltas {
        evaluated_at: now,
        lookback: lookback.to_string(),
        episode_id,
        requested_lookback_secs: requested,
        anchor_at,
        actual_lookback_secs: actual,
        verdict,
        dimensions,
        consumers,
        census,
        observation_gap_secs,
        summary,
    }
}

/// The index of the latest instant at or before `then_at`, or — when the
/// lookback reaches past the history — the earliest instant there is. `None`
/// only for an empty series.
fn anchor_index(
    times: impl Iterator<Item = DateTime<Utc>>,
    then_at: DateTime<Utc>,
) -> Option<usize> {
    let times: Vec<DateTime<Utc>> = times.collect();
    if times.is_empty() {
        return None;
    }
    Some(times.iter().rposition(|t| *t <= then_at).unwrap_or(0))
}

/// The last `n` censuses taken at or before `at`, oldest first.
fn last_n_before(censuses: &[Census], at: DateTime<Utc>, n: usize) -> &[Census] {
    let end = censuses
        .iter()
        .rposition(|c| c.taken_at <= at)
        .map_or(0, |i| i + 1);
    &censuses[end.saturating_sub(n)..end]
}

/// Join two summaries by (kind, name). Absent means zero. Largest absolute
/// change first, then by name — never by current size, which is the ranking the
/// who-line already gives and the one this read exists to NOT give: the biggest
/// thing running is rarely the thing that moved.
fn consumer_deltas(then: &CensusSummary, now: &CensusSummary) -> Vec<ConsumerDelta> {
    let mut joined: BTreeMap<(ConsumerKind, &str), (u32, u64, u32, u64)> = BTreeMap::new();
    for c in &then.consumers {
        let e = joined.entry((c.kind, c.name.as_str())).or_default();
        e.0 = c.count;
        e.1 = c.rss_bytes;
    }
    for c in &now.consumers {
        let e = joined.entry((c.kind, c.name.as_str())).or_default();
        e.2 = c.count;
        e.3 = c.rss_bytes;
    }
    let mut out: Vec<ConsumerDelta> = joined
        .into_iter()
        .map(|((kind, name), (tc, tb, nc, nb))| {
            let delta_bytes = nb as i64 - tb as i64;
            ConsumerDelta {
                detail: consumer_words(name, tc, nc, tb, nb),
                name: name.to_string(),
                kind,
                then_count: tc,
                now_count: nc,
                then_bytes: tb,
                now_bytes: nb,
                delta_bytes,
            }
        })
        .collect();
    out.sort_by(|a, b| {
        b.delta_bytes
            .abs()
            .cmp(&a.delta_bytes.abs())
            .then_with(|| a.name.cmp(&b.name))
    });
    out
}

// ---- the words -------------------------------------------------------------

fn signed_bytes(delta: f64) -> String {
    let sign = if delta >= 0.0 { "+" } else { "−" };
    format!("{sign}{}", fmt_bytes(delta.abs()))
}

fn verdict_words(then: Level, now: Level) -> String {
    if then == now {
        format!("pressure unchanged at {}", now.name())
    } else {
        format!("pressure {} → {}", then.name(), now.name())
    }
}

/// `swap 12.0 GB → 13.3 GB (+1.3 GB)` — the table wording, per unit.
fn dimension_words(d: Dimension, then: f64, now: f64) -> String {
    let noun = match d {
        Dimension::Cpu => "CPU",
        Dimension::Thermal => "thermal",
        Dimension::Memory => "available memory",
        Dimension::Swap => "swap",
        Dimension::KernelPressure => "kernel pressure",
        Dimension::Thrash => "peak thrash",
        Dimension::Agents => "stale sessions",
        Dimension::Orphans => "orphaned helpers",
        Dimension::ManagedAgents => "managed agents",
        Dimension::Disk => "free disk",
        Dimension::Uptime => "uptime",
        Dimension::KernelFree => "kernel reclaimable",
        Dimension::Compressor => "compressed memory",
        Dimension::Jetsam => "memory kills",
    };
    let delta = now - then;
    match d {
        Dimension::Memory | Dimension::Swap | Dimension::Disk | Dimension::Compressor => format!(
            "{noun} {} → {} ({})",
            fmt_bytes(then),
            fmt_bytes(now),
            signed_bytes(delta)
        ),
        Dimension::Cpu => format!(
            "{noun} {:.0}% → {:.0}% busy ({:+.0} pts)",
            then * 100.0,
            now * 100.0,
            delta * 100.0
        ),
        Dimension::Thermal => format!(
            "{noun} {} → {}",
            thermal_level_name(then),
            thermal_level_name(now)
        ),
        Dimension::KernelPressure => format!(
            "{noun} {} → {}",
            kernel_level_name(then),
            kernel_level_name(now)
        ),
        Dimension::Thrash => format!("{noun} {then:.0} → {now:.0} swap ops/sec ({delta:+.0})"),
        Dimension::Agents | Dimension::Orphans | Dimension::Jetsam => {
            format!("{noun} {then:.0} → {now:.0} ({delta:+.0})")
        }
        Dimension::ManagedAgents => {
            format!("{noun} {then:.1}% → {now:.1}% of one core ({delta:+.1})")
        }
        Dimension::KernelFree => {
            format!("{noun} {then:.0}% → {now:.0}% ({delta:+.0} pts)")
        }
        Dimension::Uptime => format!("{noun} {then:.1} → {now:.1} days"),
    }
}

/// `freed 4.0 GB from Chrome (×128 → ×34)`, `claude grew 0.6 GB (×8 → ×10)`,
/// `Slack unchanged at 2.1 GB`. "Freed" leads for a fall because the action
/// that caused it is what the reader did; growth leads with the name because
/// the reader is looking for what ate it.
fn consumer_words(name: &str, then_count: u32, now_count: u32, then_b: u64, now_b: u64) -> String {
    let delta = now_b as i64 - then_b as i64;
    let counts = if then_count != now_count {
        format!(" (×{then_count} → ×{now_count})")
    } else {
        String::new()
    };
    if delta < 0 {
        format!(
            "freed {} from {name}{counts}",
            fmt_bytes(delta.unsigned_abs() as f64)
        )
    } else if delta > 0 {
        format!("{name} grew {}{counts}", fmt_bytes(delta as f64))
    } else {
        format!("{name} unchanged at {}{counts}", fmt_bytes(now_b as f64))
    }
}

/// The sentence. Consumers that moved (largest first, at most `who_limit`, each
/// at least `deltas_min_consumer_bytes`), then swap and available memory when
/// they moved that much, then the verdict, then the honesty clauses — the gap
/// and the truncated lookback.
#[allow(clippy::too_many_arguments)]
fn sentence(
    lookback: &str,
    requested: u64,
    actual: Option<u64>,
    verdict: Option<&VerdictDelta>,
    dimensions: &[DimensionDelta],
    consumers: &[ConsumerDelta],
    gap: Option<u64>,
    config: &PressureConfig,
) -> String {
    let Some(actual) = actual else {
        return format!("vs {lookback}: nothing to compare yet — no samples in the window.");
    };

    // "vs 1h ago", "since the episode opened 2h14m ago", or — when the history
    // is shorter than the ask — "vs 23m ago (asked for 1h)". The tolerance is
    // one sample interval: an anchor a tick short of the request is the request.
    let truncated = actual + config.sample_interval_secs < requested;
    let mut head = if lookback == "episode" {
        format!("since the episode opened {} ago", fmt_secs(actual))
    } else {
        format!("vs {} ago", fmt_secs(actual))
    };
    if truncated {
        head.push_str(&format!(" (asked for {})", fmt_secs(requested)));
    }

    let mut clauses: Vec<String> = Vec::new();
    let min = config.deltas_min_consumer_bytes as i64;
    let movers: Vec<&ConsumerDelta> = consumers
        .iter()
        .filter(|c| c.delta_bytes.abs() >= min)
        .take(config.who_limit)
        .collect();
    let freed_any = movers.iter().any(|c| c.delta_bytes < 0);
    clauses.extend(movers.iter().map(|c| c.detail.clone()));

    let worse = verdict.is_some_and(|v| v.now_level >= v.then_level);
    for (d, up, down) in [
        (Dimension::Swap, "grew", "shrank"),
        (Dimension::Memory, "rose", "fell"),
    ] {
        if let Some(dd) = dimensions.iter().find(|x| x.dimension == d)
            && dd.delta.abs() >= min as f64
        {
            let noun = if d == Dimension::Swap {
                "swap"
            } else {
                "available memory"
            };
            // "still": something was freed and the machine did not get better —
            // the 2026-09-05 shape, in one word.
            let still = if freed_any && worse && ((d == Dimension::Swap) == (dd.delta > 0.0)) {
                "still "
            } else {
                ""
            };
            let verb = if dd.delta > 0.0 { up } else { down };
            clauses.push(format!(
                "{noun} {still}{verb} {}",
                fmt_bytes(dd.delta.abs())
            ));
        }
    }

    if let Some(v) = verdict {
        clauses.push(if v.then_level == Level::Checking {
            format!(
                "no trustworthy verdict at the anchor; pressure now {}",
                v.now_level.name()
            )
        } else {
            v.detail.clone()
        });
    }

    if clauses.is_empty() {
        clauses.push("nothing moved".to_string());
    }
    if let Some(g) = gap {
        clauses.push(format!(
            "not observed for {} in between — this arithmetic spans a hole",
            fmt_secs(g)
        ));
    }
    format!("{head}: {}.", clauses.join("; "))
}

fn fmt_secs(secs: u64) -> String {
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m", secs / 60)
    } else if secs < 86_400 {
        let m = (secs % 3600) / 60;
        if m == 0 {
            format!("{}h", secs / 3600)
        } else {
            format!("{}h{}m", secs / 3600, m)
        }
    } else {
        format!("{}d{}h", secs / 86_400, (secs % 86_400) / 3600)
    }
}

#[cfg(test)]
mod tests;
