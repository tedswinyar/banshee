// pressure — the model. Eleven dimensions in, one glyph and a ranked worklist out.
//
// Computed ONCE here and served over the wire, glyph included (ADR-0005). The
// menu bar, the window, the CLI and the MCP server all render what this decided;
// none of them re-derives a severity. That is the same guarantee ADR-0001 bought
// for the database, applied to the derived state — and the split-port-constant
// bug is the cheap version of the bug it prevents.
//
// `evaluate` is a PURE FUNCTION of stored history. No state is held between
// calls, so a restarted daemon reaches the same verdict as one that has been up
// for a week, and every transition is reachable from a test.

pub mod band;
pub mod config;
pub mod deltas;
mod disk;
pub mod episode;
pub mod headroom;
pub mod level;
pub mod who;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::census::Census;
use crate::rates::counter_rates;
use crate::sample::{Rollup, Sample};
use band::{Band, replay};
use config::{ALL_DIMENSIONS, Dimension, PressureConfig, Source, Unit};
use episode::{AlertEpisode, EpisodeState, Notification, RecentActivity, reconcile_episodes};
use headroom::Headroom;
use level::{Contribution, Level, decide};
use who::Consumer;

/// A recommended action, ranked by MEASURED impact.
///
/// The order is `perf-scan`'s, and it is not a guess: "reap stale sessions →
/// reap orphans → quit/relaunch the biggest app groups → reboot if swap stays
/// pinned" came out of the 2026-08-04 incident, where killing 12 stale tmux
/// sessions and 45 orphaned helpers moved the machine more than anything else
/// available.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Action {
    ReapStaleSessions,
    ReapOrphans,
    RelaunchApps,
    /// Only a reboot truly resets a saturated compressor pool.
    Reboot,
    /// "What is taking the space" is a disk-usage tool's question, not ours (ADR-0003).
    OpenDiskTool,
    /// Nothing to do but know.
    None,
}

impl Action {
    /// Lower ranks come first in the worklist.
    pub fn rank(self) -> u8 {
        match self {
            Action::ReapStaleSessions => 0,
            Action::ReapOrphans => 1,
            Action::RelaunchApps => 2,
            Action::OpenDiskTool => 3,
            Action::Reboot => 4,
            Action::None => 5,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Action::ReapStaleSessions => "Reap stale agent sessions",
            Action::ReapOrphans => "Reap orphaned helper processes",
            Action::RelaunchApps => "Quit and relaunch the biggest apps",
            Action::Reboot => "Reboot",
            Action::OpenDiskTool => "Open a disk-usage tool to see what is using the space",
            Action::None => "No action",
        }
    }
}

/// One dimension, as the UI receives it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DimensionReading {
    pub dimension: Dimension,
    pub key: String,
    pub label: String,
    pub band: Band,
    pub value: f64,
    pub unit: Unit,
    /// 0.0 at the yellow line, 1.0 at red, above 1.0 beyond. Comparable across
    /// dimensions, which is what makes "which source is winning" answerable.
    pub severity: f64,
    /// How long this band has held, counting back only as far as the observations
    /// are CONTINUOUS. A gap truncates it (`observation_gap_secs`).
    pub held_secs: u64,
    /// The gap in observations that cut `held_secs` short, when one did.
    ///
    /// Present so a client can be honest about the difference between "red for 45
    /// seconds" and "red for 45 seconds, and we were not watching for the 17
    /// minutes before that". Silently rounding the second up to the first is what
    /// produced a false 💀 (`banshee-s6s`).
    pub observation_gap_secs: Option<u64>,
    /// Change per second across the window, when a trend is meaningful.
    pub trend_per_sec: Option<f64>,
    /// Human phrasing, composed here so every surface says the same words.
    pub detail: String,
    /// Advisory dimensions inform but never drive the level.
    pub advisory: bool,
    /// A band the newest readings imply but which has not been confirmed yet.
    pub pending: Option<Band>,
    /// This dimension has an open alert episode in its down window: it was red,
    /// it has dropped below red, and the incident is not over until it stays
    /// there for `episode_down_secs` (ADR-0009). A MODIFIER, not a band — the band
    /// is honest about now; this is honest about the recent past.
    #[serde(default)]
    pub recovering: bool,
}

/// Something worth telling the user, with what to do about it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Finding {
    pub dimension: Dimension,
    pub band: Band,
    pub message: String,
    pub action: Action,
    /// The action's user-facing wording, carried over the wire for the same reason
    /// the glyph is (ADR-0005).
    ///
    /// A client could switch on `action` and supply its own string — and the Swift
    /// app briefly did — but that is a Rust `match` and a Swift `switch` that have
    /// to be kept in step by hand, which is the exact duplication the ADR rejects.
    /// Shipping the label means a new `Action` variant reaches every surface with
    /// its wording already attached, instead of rendering as blank text in whichever
    /// client forgot to add a case.
    pub action_label: String,
    /// The top consumers behind this finding, biggest first, from the census the
    /// daemon already takes (`who::attribute`). Empty when the dimension has no
    /// process behind it (disk, uptime) or no census has been taken yet.
    #[serde(default)]
    pub who: Vec<Consumer>,
    /// `who` as one line — `who: Chrome ×115 at 7.7 GB, claude ×8 at 2.1 GB` —
    /// composed here so the popover, the CLI, the MCP tool and the alert text
    /// name the same consumers in the same words (ADR-0005). Null exactly when
    /// `who` is empty.
    #[serde(default)]
    pub who_line: Option<String>,
}

/// The whole verdict.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Pressure {
    #[serde(with = "crate::wire_time")]
    pub evaluated_at: DateTime<Utc>,
    pub level: Level,
    pub level_name: String,
    pub source: Option<Source>,
    /// **The menu bar renders this string verbatim.** It is computed here so the
    /// four surfaces cannot disagree about it (ADR-0005).
    pub glyph: String,
    pub accessibility_label: String,
    pub dimensions: Vec<DimensionReading>,
    /// Ranked by measured impact, worst band first within a rank.
    pub findings: Vec<Finding>,
    pub sample_count: usize,
    pub census_count: usize,
    /// Some dimension is `recovering` (ADR-0009 decision 8). Not a seventh level:
    /// the six levels and their glyphs are unchanged, and this decorates them —
    /// 🩹 on the glyph and ", recovering" on the label when the machine is
    /// otherwise calm (Quiet or Stirring), so "calm, but was red a moment ago"
    /// and "calm" are not the same face.
    #[serde(default)]
    pub recovering: bool,
    /// The long memory, summarised (`banshee-qoz`): open episodes, and how many
    /// were active in the last hour and day. Zero on a fresh daemon.
    #[serde(default)]
    pub activity: RecentActivity,
}

/// The glyph suffix for a recovering machine. One code point, so `.glyph |
/// length` arithmetic in the e2e harness stays simple.
pub const RECOVERING_GLYPH: &str = "🩹";

impl Pressure {
    /// The verdict for "we have not looked yet".
    ///
    /// Exists so that **no client ever invents one.** `AppState.pressure` is
    /// `None` until the sampler's first evaluation, and the honest rendering of
    /// that is `Checking` — not `Quiet`. Letting the API (or the CLI, or the menu
    /// bar) fill the gap itself would be exactly the local re-derivation ADR-0005
    /// forbids, and the failure mode is the worst one available: a glyph that
    /// says "nothing is wrong" before anything has been measured.
    ///
    /// Built by asking `decide` about an empty world rather than by naming a
    /// level, so it cannot drift from what the model says about no data;
    /// `the_checking_placeholder_is_what_the_model_says_about_no_data` pins that.
    pub fn checking(at: DateTime<Utc>) -> Self {
        // `grace_secs` is irrelevant with nothing to score, and passing the
        // config in would imply the placeholder depends on tuning. It does not.
        let verdict = decide(&[], 0, 0.0, false);
        Self {
            evaluated_at: at,
            level: verdict.level,
            level_name: verdict.level.name().to_string(),
            source: verdict.source,
            glyph: verdict.glyph(),
            accessibility_label: verdict.accessibility_label(),
            dimensions: Vec::new(),
            findings: Vec::new(),
            sample_count: 0,
            census_count: 0,
            recovering: false,
            activity: RecentActivity::default(),
        }
    }

    /// Fold the episode record into the verdict: mark recovering dimensions,
    /// decorate the glyph and label when the machine is otherwise calm, and
    /// summarise recent activity.
    ///
    /// `episodes` is the record AFTER reconciliation — every open episode plus
    /// everything active in the last day. Idempotent: calling it twice with the
    /// same record yields the same verdict, so a caller cannot double-decorate.
    pub fn attach_episodes(&mut self, episodes: &[AlertEpisode], now: DateTime<Utc>) {
        let recovering: Vec<Dimension> = episodes
            .iter()
            .filter(|e| e.state == EpisodeState::Recovering)
            .map(|e| e.dimension)
            .collect();
        for r in &mut self.dimensions {
            r.recovering = recovering.contains(&r.dimension);
        }
        self.recovering = !recovering.is_empty();
        self.activity = RecentActivity::summarise(episodes, now);

        // Strip any previous decoration before deciding, so this is idempotent.
        if let Some(bare) = self.glyph.strip_suffix(RECOVERING_GLYPH) {
            self.glyph = bare.to_string();
        }
        if let Some(bare) = self.accessibility_label.strip_suffix(", recovering") {
            self.accessibility_label = bare.to_string();
        }
        // Decorate only a CALM face. At Restless and above something is red now
        // and the source suffix already has the slot; at Checking the face is a
        // claim about no data, and a modifier would muddle it.
        if self.recovering && matches!(self.level, Level::Quiet | Level::Stirring) {
            self.glyph.push_str(RECOVERING_GLYPH);
            self.accessibility_label.push_str(", recovering");
        }
    }

    pub fn reading(&self, d: Dimension) -> Option<&DimensionReading> {
        self.dimensions.iter().find(|r| r.dimension == d)
    }

    /// Distinct recommended actions, best first, excluding `None`.
    pub fn actions(&self) -> Vec<Action> {
        let mut out: Vec<Action> = Vec::new();
        for f in &self.findings {
            if f.action != Action::None && !out.contains(&f.action) {
                out.push(f.action);
            }
        }
        out
    }
}

/// How long a band has held, and whether a gap in the observations cut that short.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Held {
    secs: u64,
    /// The gap that ended the run, when one did.
    gap_secs: Option<u64>,
}

/// A timestamped series for one dimension, plus the longest interval that still
/// counts as continuous observation for its tier.
struct Series {
    points: Vec<(DateTime<Utc>, f64)>,
    /// The configured ceiling for this dimension's tier. A FLOOR for the effective
    /// ceiling, not the whole of it — see `max_interval`.
    configured_max_interval_secs: u64,
    /// How many times the observed cadence an interval may reach before it counts as
    /// a gap. Carried from `PressureConfig` rather than hardcoded here: a literal 3
    /// in this file meant `gap_tolerance` could be retuned to no effect on any
    /// series long enough to self-calibrate, and a config value that silently does
    /// nothing is worse than no config value.
    gap_tolerance: f64,
}

/// Minimum intervals needed before a series can calibrate its own cadence. Two
/// points give one interval, and one interval is never anomalous relative to
/// itself — which is exactly the case that produced the false 💀 (two samples
/// seventeen minutes apart). Below this, the configured cadence is all there is.
const MIN_INTERVALS_TO_SELF_CALIBRATE: usize = 3;

impl Series {
    fn values(&self) -> Vec<f64> {
        self.points.iter().map(|(_, v)| *v).collect()
    }

    /// The longest interval that counts as continuous observation HERE.
    ///
    /// **Self-calibrating, with the configured cadence as a floor.** The median
    /// interval of the window is what this series actually does, so `tolerance ×
    /// median` adapts to a sampler running slower than the model was told — and a
    /// config/reality mismatch then degrades into a slightly loose ceiling instead
    /// of declaring every interval a gap, which would zero every run and quietly
    /// make `is_sustained_red` unreachable. Fail-safe is not good enough when it is
    /// silent.
    ///
    /// The median is deliberately robust to a MINORITY of outliers: in the series
    /// that exposed the bug — intervals of 15, 1029, 15, 15, 15 seconds — the median
    /// is 15, so the 1029-second hole is still recognised as a hole. A mean would
    /// have been dragged to 218 and waved it through.
    ///
    /// With fewer than three intervals there is nothing to calibrate against, so the
    /// configured cadence decides. That is the case that mattered most.
    fn max_interval(&self) -> u64 {
        let mut intervals: Vec<u64> = self
            .points
            .windows(2)
            .map(|w| (w[1].0 - w[0].0).num_seconds().max(0) as u64)
            .collect();
        if intervals.len() < MIN_INTERVALS_TO_SELF_CALIBRATE {
            return self.configured_max_interval_secs;
        }
        intervals.sort_unstable();
        let median = intervals[intervals.len() / 2];
        // The SAME tolerance the config applies, re-derived from the observed
        // cadence. Rounded up and saturating: a pathological series must not
        // overflow, and a sub-second median must not floor the ceiling to zero.
        let calibrated = ((median as f64) * self.gap_tolerance).ceil();
        let calibrated = if calibrated.is_finite() && calibrated >= 0.0 {
            calibrated as u64
        } else {
            u64::MAX
        };
        calibrated.max(self.configured_max_interval_secs)
    }

    /// Seconds the trailing `held` points span, **stopping at the first GAP**.
    ///
    /// Derived from real timestamps rather than by multiplying by an assumed
    /// cadence, because the sample tier and the census tier tick at different rates.
    /// That much was always right. What was missing is that a gap in the
    /// observations is not evidence of continuity:
    ///
    /// > Observed 2026-09-01, minutes after the daemon was first installed. The
    /// > database held two samples from 01:48 and four from 02:05 — a 17-minute
    /// > hole — and every out-of-band dimension reported `held 17m` off 45 seconds
    /// > of actual observation. The verdict was 💀 Shrieking, because `held_secs`
    /// > feeds `is_sustained_red`, which is what escalates Restless → Wailing →
    /// > Shrieking. Two alerts were recorded; at Shrieking the Slack sink would
    /// > have fired.
    ///
    /// The trigger is mundane and deliberately designed in: every daemon restart,
    /// and every laptop wake — `sampler::run` uses `MissedTickBehavior::Delay`
    /// specifically so that a sleeping machine leaves a gap rather than a burst of
    /// identical timestamps. So the sampler creates exactly the holes this used to
    /// misread, and a machine waking into any red dimension went straight to a
    /// banner on its second sample. That is the cry-wolf failure the whole
    /// hysteresis design exists to prevent (`banshee-s6s`).
    ///
    /// Walking backwards and stopping is the honest reading: we know the band held
    /// from the last gap to now, and we do not know what happened before it.
    fn held(&self, held: usize) -> Held {
        let n = self.points.len();
        if n == 0 || held == 0 {
            return Held {
                secs: 0,
                gap_secs: None,
            };
        }
        let newest = self.points[n - 1].0;
        let max_interval = self.max_interval();
        // The oldest index the hysteresis replay says is part of this run.
        let run_start = n.saturating_sub(held);

        // Walk back from the newest toward `run_start`, stopping at the first
        // interval too long to be a missed tick.
        let mut earliest = n - 1;
        let mut gap_secs = None;
        let mut i = n - 1;
        while i > run_start {
            let interval = (self.points[i].0 - self.points[i - 1].0)
                .num_seconds()
                .max(0) as u64;
            if interval > max_interval {
                gap_secs = Some(interval);
                break;
            }
            earliest = i - 1;
            i -= 1;
        }

        Held {
            secs: (newest - self.points[earliest].0).num_seconds().max(0) as u64,
            gap_secs,
        }
    }

    /// Change per second across the trailing CONTIGUOUS run, by least squares.
    ///
    /// Contiguous for the same reason `held` is: a line fitted through two clusters
    /// either side of a hole describes the hole, not the machine. That is not
    /// academic — this slope is what produces the disk dimension's "full in 4.0
    /// hours" projection, which is the most actionable sentence the app emits, and
    /// across a 17-minute gap it was arithmetic on an artefact.
    fn trend_per_sec(&self) -> Option<f64> {
        let tail = self.contiguous_tail();
        if tail.len() < 2 {
            return None;
        }
        let base = tail[0].0.timestamp() as f64;
        let pts: Vec<crate::rates::Point> = tail
            .iter()
            .map(|(t, v)| crate::rates::Point {
                t_secs: t.timestamp() as f64 - base,
                value: *v,
            })
            .collect();
        crate::rates::Trend::fit(&pts).map(|t| t.slope_per_sec)
    }

    /// The largest interval in `points[from..=to]` that is too long to be a missed
    /// tick — the hole a delta between the two ends would be arithmetic across.
    /// `None` when the range was observed continuously by this series' own rule
    /// (`max_interval`, the same ceiling `held` stops at). Reported, not absorbed:
    /// a comparison across a laptop's sleep is still a comparison, but the reader
    /// must be told it spans a hole — the held-time gap rule applied to a difference.
    fn largest_gap_between(&self, from: usize, to: usize) -> Option<u64> {
        if from >= to || to >= self.points.len() {
            return None;
        }
        let max_interval = self.max_interval();
        self.points[from..=to]
            .windows(2)
            .map(|w| (w[1].0 - w[0].0).num_seconds().max(0) as u64)
            .filter(|interval| *interval > max_interval)
            .max()
    }

    /// The longest suffix of the series with no gap in it.
    fn contiguous_tail(&self) -> &[(DateTime<Utc>, f64)] {
        let n = self.points.len();
        if n < 2 {
            return &self.points;
        }
        let max_interval = self.max_interval();
        let mut start = 0;
        for i in (1..n).rev() {
            let interval = (self.points[i].0 - self.points[i - 1].0)
                .num_seconds()
                .max(0) as u64;
            if interval > max_interval {
                start = i;
                break;
            }
        }
        &self.points[start..]
    }
}

/// Evaluate the model against stored history.
///
/// `samples`, `censuses` and `rollups` must be OLDEST FIRST. All may be short
/// or empty; a thin history produces `Checking`, never a confident `Quiet`.
/// `rollups` is the week tier — the disk trend's whole input (`banshee-nio`);
/// empty means no week trend, which is exactly right for the callers that
/// compare short windows (deltas) rather than watch the machine.
pub fn evaluate(
    now: DateTime<Utc>,
    samples: &[Sample],
    censuses: &[Census],
    rollups: &[Rollup],
    config: &PressureConfig,
) -> Pressure {
    let window: &[Sample] = if samples.len() > config.window_samples {
        &samples[samples.len() - config.window_samples..]
    } else {
        samples
    };

    // The newest run-queue depth (load-per-core), carried into the CPU detail as
    // the labeled secondary. It is NOT the CPU dimension's value any more — that
    // is measured utilization — but a high demand over idle cores is the
    // scheduling-contention story the detail must be able to tell (`banshee-87l.19`).
    let cpu_demand = window.last().map(|s| s.to_reading().load_per_core_1m());
    // banshee-cwl: the swap pool lives on the data volume, so the swap and disk
    // findings read the allocated pool total together — how full the pool is, and
    // how much of the volume it occupies. Newest sample; `None` before the first.
    let swap_total = window.last().map(|s| s.swap_total_bytes);

    let mut readings = Vec::new();
    let mut contributions = Vec::new();

    for d in ALL_DIMENSIONS {
        // Disk reads the FULL slice the sampler fetched, not the ten-minute
        // window: its yellow episode up-delay is minutes long (`banshee-dok`),
        // and `held_secs` counts only what the series covers, so a ten-minute
        // series caps every observable hold at ten minutes. The replay is
        // causal — more history never changes the current band, it only lets
        // the hold be measured instead of saturated. Every other dimension
        // keeps the window; so does disk's SLOPE (see below).
        let history: &[Sample] = if d == Dimension::Disk {
            samples
        } else {
            window
        };
        let Some(series) = series_for(d, history, censuses, config) else {
            continue;
        };
        let values = series.values();
        let Some(outcome) = replay(config.spec(d), &values) else {
            continue;
        };

        let spec = config.spec(d);
        // Disk is judged on TIME as well as bytes (`banshee-3sn`): its slope is
        // the flicker-proof median estimator, its severity folds in the
        // projected time-to-full, and a near projection forces the band floor
        // regardless of bytes. Everything downstream — `decide`'s catastrophic
        // ceiling, `dominant_source`, the glyph, headroom's reason line —
        // follows from the severity and band with no disk-specific rules.
        let mut disk_note: Option<String> = None;
        let (band, held_samples, severity, trend) = if d == Dimension::Disk {
            // The slope and the projection floor stay on the ten-minute window
            // even though the byte band above replays the longer history: one
            // window cannot serve both a burst and a trend (`banshee-wql`), and
            // widening this one would retune the "full in …" projection, not
            // add a second horizon.
            let tail = series.contiguous_tail();
            let tail = &tail[tail.len().saturating_sub(config.window_samples)..];
            let slope = disk::slope_per_sec(tail);
            let projection = disk::projected_full_secs(outcome.value, slope);
            let severity = disk::severity(
                spec.red,
                config.disk_projection_red_secs,
                outcome.value,
                projection,
            );
            let (forced, forced_held) = disk::forced_floor(
                tail,
                config.disk_projection_red_secs,
                config.disk_projection_yellow_secs,
                spec.confirm_samples,
            );
            let (mut band, mut held) = if forced > outcome.band {
                (forced, forced_held)
            } else {
                (outcome.band, outcome.held_samples)
            };

            // The THIRD horizon (`banshee-nio`): a projection over the week
            // tier, because the ten-minute slope can only ever fire during a
            // burst — 90 GB lost over 7 days is ~150 KB/s, which against
            // 100 GB free projects "full in a week" and yet could never move
            // the band above. Its own window (the rollups), its own trigger
            // (full within `disk_trend_alert_secs`), its own ceiling (Yellow,
            // never Red, never severity): three horizons, deliberately, not
            // one window retuned (`banshee-wql`).
            let newest_volume = history.last().and_then(|s| s.volumes.first());

            // The BURST horizon (`banshee-636`): a dramatic drop notifies
            // whatever the absolute level — the shape that actually precedes
            // trouble here. Measured 2026-09-22: 43.8 → 9.2 GB in five hours
            // produced only short projection blips; this rule fires early in
            // that slide, with room still in hand to act.
            let total = newest_volume.map(|v| v.total_bytes as f64).unwrap_or(0.0);
            let (drop_band, drop_held) = disk::drop_floor(
                &series.points,
                total,
                config.disk_drop_window_secs,
                config.disk_drop_bytes,
                config.disk_drop_fraction,
                spec.confirm_samples,
            );
            if drop_band > band {
                band = drop_band;
                held = drop_held;
            } else if drop_band == band && drop_held > held {
                held = drop_held;
            }

            let mount = newest_volume.map(|v| v.mount_point.clone());
            let week = disk::week_points(rollups, mount.as_deref().unwrap_or_default());
            let week_slope = disk::week_slope_per_sec(&week, config.disk_trend_min_span_secs);
            let (trend_band, trend_held) = disk::trend_floor(
                &series.points,
                week_slope,
                config.disk_trend_alert_secs,
                spec.confirm_samples,
            );
            if trend_band > band {
                band = trend_band;
                held = trend_held;
            } else if trend_band == band && trend_held > held {
                // Same band from two causes: the band has stood as long as the
                // LONGEST-standing cause, and the standing trend is what lets a
                // slow drain meet the yellow episode delay (`banshee-dok`).
                held = trend_held;
            }
            // The detail carries every horizon that is speaking, burst first,
            // so the finding, the episode message and the app banner all say
            // why the band is what it is (ADR-0005).
            let mut notes = String::new();
            if drop_band == Band::Yellow
                && let Some(n) = disk::drop_note(&series.points, config.disk_drop_window_secs)
            {
                notes.push_str(&n);
            }
            if trend_band == Band::Yellow
                && let Some(s) = week_slope
                && let Some(n) = disk::trend_note(&week, s, outcome.value)
            {
                notes.push_str(&n);
            }
            if !notes.is_empty() {
                disk_note = Some(notes);
            }
            (band, held, severity, slope)
        } else {
            (
                outcome.band,
                outcome.held_samples,
                severity_of(spec, outcome.value),
                series.trend_per_sec(),
            )
        };
        let held = series.held(held_samples);
        let mut detail = detail_for(d, outcome.value, trend, cpu_demand, swap_total, config);
        if let Some(note) = disk_note {
            detail.push_str(&note);
        }

        contributions.push(Contribution {
            dimension: d,
            band,
            held_secs: held.secs,
            severity,
        });
        readings.push(DimensionReading {
            dimension: d,
            key: d.key().to_string(),
            label: d.label().to_string(),
            band,
            value: outcome.value,
            unit: d.unit(),
            severity,
            held_secs: held.secs,
            observation_gap_secs: held.gap_secs,
            trend_per_sec: trend,
            detail,
            advisory: d.is_advisory(),
            pending: outcome.pending,
            recovering: false,
        });
    }

    // "Enough data" is about the SAMPLE tier: it is the one that ticks every 15s
    // and underpins the rate signals. A machine with samples but no census yet is
    // still worth a verdict on CPU and memory.
    let have_data = window.len() >= config.min_samples && !contributions.is_empty();
    let verdict = decide(
        &contributions,
        config.red_grace_secs,
        config.catastrophic_severity,
        have_data,
    );

    // Attribution reads the NEWEST census: the who-line answers "who is behind
    // this now", and `censuses` is oldest-first.
    let findings = if have_data {
        build_findings(&readings, &verdict, censuses.last(), swap_total, config)
    } else {
        Vec::new()
    };

    Pressure {
        evaluated_at: now,
        level: verdict.level,
        level_name: verdict.level.name().to_string(),
        source: verdict.source.filter(|_| verdict.level.shows_source()),
        glyph: verdict.glyph(),
        accessibility_label: verdict.accessibility_label(),
        dimensions: readings,
        findings,
        sample_count: window.len(),
        census_count: censuses.len(),
        recovering: false,
        activity: RecentActivity::default(),
    }
}

/// One evaluation, complete: the verdict, the episode lifecycle advanced against
/// it, and the verdict decorated with the result.
#[derive(Debug, Clone, PartialEq)]
pub struct Assessment {
    /// The verdict to publish, `recovering`/`activity` attached.
    pub pressure: Pressure,
    /// The verdict reduced to what an agent should DO (ADR-0010), derived from
    /// the decorated `pressure` above and the newest sample's core count.
    /// Published beside the verdict so `/headroom` serves without re-deriving.
    pub headroom: Headroom,
    /// Episodes that changed this tick — the caller upserts exactly these.
    pub episodes: Vec<AlertEpisode>,
    /// What to say, after inhibition. Usually empty.
    pub notifications: Vec<Notification>,
}

/// `evaluate`, then `reconcile_episodes`, then `attach_episodes` — the three
/// pure steps the sampler runs, in the only order that is correct (the
/// decoration must see the episodes as they are AFTER this tick's transitions).
///
/// `recent_episodes` is every open episode plus everything active in the last
/// day, from the store, any order. Exists so the sampler cannot skip a step or
/// run them out of order; each step stays testable on its own.
pub fn assess(
    now: DateTime<Utc>,
    samples: &[Sample],
    censuses: &[Census],
    rollups: &[Rollup],
    recent_episodes: &[AlertEpisode],
    config: &PressureConfig,
) -> Assessment {
    let mut pressure = evaluate(now, samples, censuses, rollups, config);
    let open: Vec<AlertEpisode> = recent_episodes
        .iter()
        .filter(|e| e.is_open())
        .cloned()
        .collect();
    // The newest census rides along so an episode that opens or moves its peak
    // can store WHO was running at that moment (`censusAtPeak`): the
    // samples behind a four-hour incident are gone before anyone asks.
    let reconciled = reconcile_episodes(&open, &pressure, censuses.last(), now, config);

    // The record as it stands after this tick: changed episodes replace their
    // stored versions by id, new ones join.
    let mut record: Vec<AlertEpisode> = recent_episodes.to_vec();
    for changed in &reconciled.episodes {
        match record.iter_mut().find(|e| e.id == changed.id) {
            Some(slot) => *slot = changed.clone(),
            None => record.push(changed.clone()),
        }
    }
    pressure.attach_episodes(&record, now);

    // Headroom reads the verdict AFTER decoration, and the core count from the
    // newest sample — `None` before the first sample, which the function renders
    // as "wait", never as an invented capacity.
    let headroom = headroom::derive(
        &pressure,
        samples.last().map(|s| headroom::MachineFacts {
            cores: s.ncpu,
            mem_total_bytes: s.mem_total_bytes,
        }),
        config,
    );

    Assessment {
        pressure,
        headroom,
        episodes: reconciled.episodes,
        notifications: reconciled.notifications,
    }
}

/// How far past its own threshold a value is, normalised so dimensions compare.
///
/// 0.0 at yellow, 1.0 at red, above 1.0 beyond red, negative when green.
fn severity_of(spec: &band::BandSpec, value: f64) -> f64 {
    if !value.is_finite() {
        return -1.0;
    }
    let span = if spec.inverted {
        spec.yellow - spec.red
    } else {
        spec.red - spec.yellow
    };
    if span <= 0.0 {
        // A degenerate spec (red == yellow) would divide by zero. Report a bare
        // "at or past the line" rather than infinity, which would win every
        // dominant-source comparison forever.
        return if spec.raw_band(value) == Band::Green {
            -1.0
        } else {
            1.0
        };
    }
    if spec.inverted {
        (spec.yellow - value) / span
    } else {
        (value - spec.yellow) / span
    }
}

/// How many consecutive intervals a median filter smooths over. 3 at the 15s
/// cadence means churn must outlast ~30s to survive — which is roughly where a
/// person starts to FEEL it, and is short enough that a real storm is never missed.
const THRASH_SMOOTH_INTERVALS: usize = 3;

/// Rewrite a spiky series so each point carries the worst SUSTAINED level over the
/// `window_secs` leading up to it, rather than that instant's value (`banshee-s4d`).
///
/// Two stages, and both are load-bearing:
///
/// 1. **A trailing median filter** over `THRASH_SMOOTH_INTERVALS`. This is what
///    removes single-interval artifacts. The measured case: quitting a 6.5 GB
///    browser produced a 6394 ops/sec burst as the kernel tore the working set down
///    — real churn, but caused BY the remediation, so letting it set the band would
///    tell someone their fix had failed at the moment it worked. A median needs a
///    MAJORITY of the smoothing window to be elevated, so one spike vanishes while
///    anything sustained passes through untouched. Trailing, not centred, because a
///    centred filter would need a sample from the future to judge the newest point.
/// 2. **A sliding max** of the filtered series over the time window. Max is now safe
///    precisely because stage 1 removed the artifacts, and it buys the property a
///    percentile could not give: as the window slides with no new churn, the
///    reported figure only ever DECAYS.
///
/// That decay property is why this is not an upper percentile, which is what the
/// first version of this function used. A percentile's rank is a function of how many
/// points are in the window, so points merely LEAVING could move the rank to a higher
/// order statistic — observed live: the figure climbed 118 -> 523 ops/sec with no new
/// storming at all, when the in-window count crossed 20 and the dropped-decile count
/// changed. A number that rises while the machine calms down is the same species of
/// lie this whole function exists to remove.
///
/// The window is measured in TIME, not in samples, which makes gaps behave
/// correctly for free: a hole longer than the window pushes the pre-hole storm out
/// of scope, so a laptop that slept through yesterday's thrashing does not wake up
/// claiming to be thrashing now. That is the `held_secs` lesson (`banshee-s6s`)
/// applied to a peak instead of a run.
///
/// Points must be OLDEST FIRST. Returns points in the same order, one per input.
fn windowed_peak(points: &[(DateTime<Utc>, f64)], window_secs: u64) -> Vec<(DateTime<Utc>, f64)> {
    // Stage 1: trailing median.
    let smoothed: Vec<(DateTime<Utc>, f64)> = points
        .iter()
        .enumerate()
        .map(|(i, &(at, _))| {
            let lo = i.saturating_sub(THRASH_SMOOTH_INTERVALS - 1);
            let mut vals: Vec<f64> = points[lo..=i].iter().map(|(_, v)| *v).collect();
            vals.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            (at, vals[vals.len() / 2])
        })
        .collect();

    // Stage 2: sliding max over the time window.
    let window = chrono::Duration::seconds(window_secs as i64);
    smoothed
        .iter()
        .enumerate()
        .map(|(i, &(at, v))| {
            let cutoff = at - window;
            // The point at `i` is always inside its own window (`at > at - window`
            // for a positive window), so this fold always has a value to start from.
            let peak = smoothed[..=i]
                .iter()
                .filter(|(t, _)| *t > cutoff)
                .map(|(_, v)| *v)
                .fold(v, f64::max);
            (at, peak)
        })
        .collect()
}

fn series_for(
    d: Dimension,
    samples: &[Sample],
    censuses: &[Census],
    config: &PressureConfig,
) -> Option<Series> {
    let points: Vec<(DateTime<Utc>, f64)> = match d {
        // MEASURED utilization (`banshee-87l.19`), a fraction 0.0..=1.0, from the
        // per-core tick counters — NOT load-per-core, which counts a churning
        // security agent's runnable threads and read 3.26× per core over cores that
        // were half idle. A rate needs two samples, and `counter_rates` refuses a
        // pair that spans a reboot or a backwards clock, so a gap yields no point
        // rather than a wild reading — exactly as Thrash does. `cpu_utilization` is
        // `None` for a pre-v12 pair (no ticks) or a zero-CPU-time interval; those
        // pairs are skipped, so a thin or migrated history simply produces no CPU
        // verdict rather than a false one.
        Dimension::Cpu => {
            let mut util = Vec::new();
            for w in samples.windows(2) {
                let (prev, curr) = (&w[0], &w[1]);
                let elapsed =
                    (curr.sampled_at - prev.sampled_at).num_milliseconds() as f64 / 1000.0;
                if let Ok(r) = counter_rates(&prev.to_reading(), &curr.to_reading(), elapsed)
                    && let Some(u) = r.cpu_utilization
                {
                    util.push((curr.sampled_at, u));
                }
            }
            util
        }
        // The kernel's thermal verdict (0 nominal … 4 sleeping). None is a pre-v11
        // row that never recorded it — SKIPPED, not read as nominal: 0 is a REAL
        // reading here, so treating absence as 0 would invent a calm kernel.
        Dimension::Thermal => samples
            .iter()
            .filter_map(|s| {
                s.thermal_pressure_level
                    .map(|l| (s.sampled_at, f64::from(l)))
            })
            .collect(),
        // AVAILABLE, not free (`banshee-cot`): macOS keeps the free list
        // deliberately tiny, so `free_bytes` reads 100–500 MB on any healthy
        // machine and the dimension sat red while the kernel's own pressure
        // level said NORMAL — six false alerts in one night.
        Dimension::Memory => samples
            .iter()
            .map(|s| (s.sampled_at, s.to_reading().available_bytes() as f64))
            .collect(),
        Dimension::Swap => samples
            .iter()
            .map(|s| (s.sampled_at, s.swap_used_bytes as f64))
            .collect(),
        // The kernel's own verdict (1 normal, 2 warn, 4 critical). 0 means a
        // pre-v8 row that never recorded it — SKIPPED, not read as normal:
        // letting migrated history claim the kernel said all-clear would be
        // inventing a verdict, and 0 would also read as Green forever.
        Dimension::KernelPressure => samples
            .iter()
            .filter(|s| s.memory_pressure_level > 0)
            .map(|s| (s.sampled_at, f64::from(s.memory_pressure_level)))
            .collect(),
        Dimension::Thrash => {
            // A rate needs two samples, and `counter_rates` refuses pairs that
            // span a reboot or a backwards clock — so gaps simply produce no
            // point rather than a wild spike.
            let mut rates = Vec::new();
            for w in samples.windows(2) {
                let (prev, curr) = (&w[0], &w[1]);
                let elapsed =
                    (curr.sampled_at - prev.sampled_at).num_milliseconds() as f64 / 1000.0;
                if let Ok(r) = counter_rates(&prev.to_reading(), &curr.to_reading(), elapsed) {
                    rates.push((curr.sampled_at, r.swapins_per_sec + r.swapouts_per_sec));
                }
            }
            // Band on the WINDOW'S PEAK, never the newest interval (`banshee-s4d`).
            // Thrash is the one dimension whose signal is a spike train rather than
            // a level, so the newest sample answers "is it churning this second?"
            // when the question a person is asking is "has this machine been
            // janky?". Reported through `value` as well as the band, so the number
            // on screen and the colour cannot disagree.
            windowed_peak(&rates, config.thrash_peak_window_secs)
        }
        Dimension::Disk => samples
            .iter()
            .filter_map(|s| {
                // The data volume is the first configured mount. Not `/`: on APFS
                // that is the sealed system snapshot.
                s.volumes
                    .first()
                    .map(|v| (s.sampled_at, v.avail_bytes as f64))
            })
            .collect(),
        Dimension::Uptime => samples
            .iter()
            .map(|s| {
                let up = s.sampled_at.timestamp() - s.boot_time_secs;
                (s.sampled_at, up.max(0) as f64 / 86_400.0)
            })
            .collect(),
        Dimension::Agents => censuses
            .iter()
            .map(|c| (c.taken_at, c.stale_sessions().count() as f64))
            .collect(),
        Dimension::Orphans => censuses
            .iter()
            .map(|c| (c.taken_at, f64::from(c.orphans.orphan_count)))
            .collect(),
        Dimension::ManagedAgents => censuses
            .iter()
            // A ratio whose denominator is a process that started moments ago is
            // noise: a log shipper 109s old measured 14.6% off 16s of CPU. Skip
            // the reading rather than band on it.
            .filter(|c| c.monitor_total.longest_life_secs >= config.managed_agents_min_life_secs)
            .map(|c| (c.taken_at, c.monitor_total.percent_of_one_core))
            .collect(),
        // The kernel's reclaimability percentage (`kern.memorystatus_level`).
        // None is a pre-v13 row that never recorded it — SKIPPED, not read as 0:
        // 0 is a REAL reading (nothing reclaimable), so treating absence as 0
        // would invent a starving kernel. Advisory: shown, never banded into the
        // level.
        Dimension::KernelFree => samples
            .iter()
            .filter_map(|s| s.kernel_free_percent.map(|p| (s.sampled_at, f64::from(p))))
            .collect(),
        // Compressed-pool occupancy in bytes: pages held compressed × page size.
        // Always present (both are non-optional counters), so no skip. Advisory —
        // it backs the reboot finding, it does not drive the level.
        Dimension::Compressor => samples
            .iter()
            .map(|s| {
                (
                    s.sampled_at,
                    (s.pages_compressor as f64) * (s.page_size as f64),
                )
            })
            .collect(),
        // Genuine sustained-pressure kills, counted by the census from
        // `/usr/bin/log show` over the gap since the previous census
        // (`banshee-2dq`). Each `pressure_kills` is already a PER-WINDOW count of
        // honest kills — idle-exit `rf:low` reaping is excluded at the source — so
        // the windows do not overlap and simply accumulate; no reboot guard is
        // needed because these are not a lifetime counter. `None` is a pre-v15
        // census that never recorded it — SKIPPED. The running total holds at its
        // peak for the rest of the window, so a single kill keeps the dimension red
        // long enough for `decide` to ring the top bell (`banshee-yk8`). This
        // replaces the old sysctl-delta lens, which fired on ANY jetsam including
        // routine idle-exit reaping and so was a false-positive generator; the
        // `samples.jetsam_kills` counter stays on the wire as a cheap secondary
        // observable but no longer drives the band.
        Dimension::Jetsam => {
            let mut total = 0.0;
            let mut points = Vec::new();
            for c in censuses {
                if let Some(kills) = c.pressure_kills {
                    total += kills as f64;
                    points.push((c.taken_at, total));
                }
            }
            points
        }
    };

    if points.is_empty() {
        return None;
    }
    Some(Series {
        points,
        // Per-tier, from config: the census legitimately ticks ~20× slower, so one
        // shared ceiling would either shred every census run or wave a fifteen-
        // minute hole in the sample series through as continuous observation. This
        // is a FLOOR — `Series::max_interval` raises it to match what the series
        // actually does, when it has enough intervals to tell.
        configured_max_interval_secs: config.max_interval_secs(d),
        gap_tolerance: config.gap_tolerance,
    })
}

fn detail_for(
    d: Dimension,
    value: f64,
    trend: Option<f64>,
    cpu_demand: Option<f64>,
    swap_total: Option<u64>,
    config: &PressureConfig,
) -> String {
    let rising = trend.filter(|t| t.abs() > f64::EPSILON);
    match d {
        // Headline is MEASURED utilization; the run queue (load-per-core) is the
        // labeled secondary (`banshee-87l.19`). When demand is deep but the cores
        // are NOT busy — below the yellow line — that is scheduling contention, not
        // saturation, and the words say exactly that. No "saturation", no fire, and
        // deliberately NO "I/O wait": XNU's load average counts running and runnable
        // threads only, never uninterruptible/I-O-wait, so the contention is at the
        // scheduler's run queue, not on disk.
        Dimension::Cpu => {
            let mut s = format!("{:.0}% busy", value * 100.0);
            if let Some(t) = rising {
                s.push_str(&format!(
                    ", {} {:.0}%/min",
                    direction(t),
                    (t * 60.0 * 100.0).abs()
                ));
            }
            if let Some(demand) = cpu_demand {
                if demand > config.cpu_contention_demand_per_core && value < config.cpu.yellow {
                    s.push_str(&format!(
                        "; {demand:.1}× per core queued — scheduling contention, \
                         cores not saturated"
                    ));
                } else {
                    s.push_str(&format!("; {demand:.1}× per core"));
                }
            }
            s
        }
        Dimension::Memory => {
            let mut s = format!("{} available", fmt_bytes(value));
            if let Some(t) = rising
                && t < 0.0
            {
                s.push_str(&format!(", falling {}/min", fmt_bytes(-t * 60.0)));
            }
            s
        }
        Dimension::Swap => {
            let mut s = format!("{} in use", fmt_bytes(value));
            if let Some(t) = rising
                && t > 0.0
            {
                s.push_str(&format!(", climbing {}/min", fmt_bytes(t * 60.0)));
            }
            // banshee-cwl: how full the allocated pool is (used/total) bounds how
            // much further swap can grow before macOS must take more of the volume.
            // A percentage a person recognises — "pool 96% full".
            if let Some(total) = swap_total.filter(|t| *t > 0) {
                let pct = (value / total as f64) * 100.0;
                s.push_str(&format!(" \u{b7} pool {pct:.0}% full"));
            }
            s
        }
        // "peak" is not decoration: the number IS a window maximum over a smoothed
        // series, and calling it a current rate is the lie this dimension used to
        // tell (`banshee-s4d`).
        //
        // Deliberately NO trend suffix. The first version appended ", intensifying"
        // whenever `rising` was positive, and live observation showed it on EVERY
        // reading of a decaying storm — 523 -> 54 -> 30 ops/sec, all three labelled
        // intensifying. A peak series is dominated by whenever the storm happened, so
        // its slope says nothing about what the churn is doing now. Better to omit
        // the temporal claim than to make one that is wrong most of the time.
        Dimension::Thrash => format!("peak {value:.0} swap ops/sec"),
        Dimension::KernelPressure => format!("kernel reports {}", kernel_level_name(value)),
        Dimension::Thermal => format!("kernel reports {}", thermal_level_name(value)),
        Dimension::Agents => format!("{value:.0} stale"),
        Dimension::Orphans => format!("{value:.0} orphaned"),
        Dimension::ManagedAgents => format!("{value:.1}% of one core"),
        Dimension::Disk => {
            let mut s = format!("{} free", fmt_bytes(value));
            // The projection is the sentinel's whole contribution on disk: a
            // number, not a treemap (ADR-0003).
            if let Some(t) = rising
                && t < 0.0
                && value > 0.0
            {
                let secs = value / -t;
                s.push_str(&format!(", full in {}", fmt_duration(secs)));
            }
            s
        }
        Dimension::Uptime => format!("{value:.1} days"),
        Dimension::KernelFree => format!("{value:.0}% reclaimable"),
        Dimension::Compressor => format!("{} held compressed", fmt_bytes(value)),
        Dimension::Jetsam => {
            if value >= 1.0 {
                format!("{value:.0} killed under memory pressure")
            } else {
                "no sustained-pressure kills this window".to_string()
            }
        }
    }
}

fn direction(t: f64) -> &'static str {
    if t > 0.0 { "rising" } else { "falling" }
}

/// The kernel's name for its own level. Any value outside the sysctl's contract
/// is rendered as the number rather than guessed at.
/// The macOS `OSThermalPressureLevel` names, by value.
fn thermal_level_name(value: f64) -> String {
    match value as i64 {
        0 => "nominal".to_string(),
        1 => "moderate".to_string(),
        2 => "heavy".to_string(),
        3 => "trapping".to_string(),
        4 => "sleeping".to_string(),
        other => format!("level {other}"),
    }
}

fn kernel_level_name(value: f64) -> String {
    match value as i64 {
        1 => "normal".to_string(),
        2 => "warn".to_string(),
        4 => "critical".to_string(),
        other => format!("level {other}"),
    }
}

pub(crate) fn fmt_bytes(b: f64) -> String {
    let abs = b.abs();
    if abs >= 1e9 {
        format!("{:.1} GB", b / 1e9)
    } else if abs >= 1e6 {
        format!("{:.0} MB", b / 1e6)
    } else if abs >= 1e3 {
        format!("{:.0} KB", b / 1e3)
    } else {
        format!("{b:.0} B")
    }
}

fn fmt_duration(secs: f64) -> String {
    if secs < 3600.0 {
        format!("{:.0} min", secs / 60.0)
    } else if secs < 86_400.0 {
        format!("{:.1} hours", secs / 3600.0)
    } else {
        format!("{:.1} days", secs / 86_400.0)
    }
}

/// The worklist.
///
/// Ranked by the action's MEASURED impact, then by band, then by how far past the
/// line — so the top of the list is the thing most worth doing, not the thing
/// that happens to be listed first in the enum.
fn build_findings(
    readings: &[DimensionReading],
    verdict: &level::Verdict,
    census: Option<&Census>,
    swap_total: Option<u64>,
    config: &PressureConfig,
) -> Vec<Finding> {
    // Every finding — yellow as well as red — names its consumers. Yellow is
    // where a person can still act cheaply, and "who" is the action.
    let who_for = |d: Dimension| -> Vec<Consumer> {
        census
            .map(|c| who::attribute(d, c, config))
            .unwrap_or_default()
    };
    // What the reap button would actually find. Zero when no census has been
    // taken yet: an action must not claim there is something to reap before
    // anyone has looked (`banshee-rad`).
    let stale_sessions = census.map(|c| c.stale_sessions().count()).unwrap_or(0);
    let mut findings: Vec<Finding> = readings
        .iter()
        .filter(|r| r.band > Band::Green)
        // Advisory dimensions are informational rows, not findings — with ONE
        // exception. Uptime is worth saying alongside memory pressure (`perf-scan`:
        // long uptime plus pinned swap means fragmented, unreclaimable memory; on
        // its own it is not a finding). The OTHER advisory memory signals
        // (`banshee-yk8`: availability, kernel-free, compressor) all point at the
        // same RelaunchApps lever swap already names, so a finding for each would
        // be the same story told three more times — and the compressor gets its
        // say in the standalone reboot finding below. So admit a non-advisory
        // dimension always, and Uptime only when swap is elevated; nothing else.
        .filter(|r| {
            !r.advisory
                || (r.dimension == Dimension::Uptime
                    && readings
                        .iter()
                        .any(|o| o.dimension == Dimension::Swap && o.band > Band::Green))
        })
        .map(|r| {
            let action = action_for(r.dimension, r.band, stale_sessions);
            let who = who_for(r.dimension);
            Finding {
                dimension: r.dimension,
                band: r.band,
                message: message_for(r, swap_total),
                action,
                action_label: action.label().to_string(),
                who_line: who::who_line(&who),
                who,
            }
        })
        .collect();

    findings.sort_by(|a, b| {
        a.action
            .rank()
            .cmp(&b.action.rank())
            .then_with(|| b.band.cmp(&a.band))
            .then_with(|| a.dimension.cmp(&b.dimension))
    });

    // At Shrieking, say the thing the incident taught: only a reboot truly resets
    // a saturated compressor pool. Appended last because it is the last resort.
    if verdict.level == Level::Shrieking
        && readings
            .iter()
            .any(|r| r.dimension == Dimension::Swap && r.band == Band::Red)
        && !findings.iter().any(|f| f.action == Action::Reboot)
    {
        // Back the claim with the measured occupancy (`banshee-xlw`): before the
        // memory second pass this asserted a mechanism it collected no signal for.
        // Now it cites the Compressor reading if one is present — a saturated pool
        // is the reason a reboot, not a relaunch, is the lever.
        let message = match readings
            .iter()
            .find(|r| r.dimension == Dimension::Compressor)
        {
            Some(r) => format!(
                "Swap has stayed pinned and {} is held compressed. Only a reboot \
                 truly resets a saturated compressor pool.",
                fmt_bytes(r.value)
            ),
            None => "Swap has stayed pinned. Only a reboot truly resets a saturated \
                     compressor pool."
                .to_string(),
        };
        // The reboot is about the compressor pool, not about any one process, so
        // it names nobody: the swap finding above already did.
        findings.push(Finding {
            dimension: Dimension::Compressor,
            band: Band::Red,
            message,
            action: Action::Reboot,
            action_label: Action::Reboot.label().to_string(),
            who: Vec::new(),
            who_line: None,
        });
    }

    findings
}

fn message_for(r: &DimensionReading, swap_total: Option<u64>) -> String {
    match r.dimension {
        // Red means MEASURED utilization is at or above the saturation line — the
        // cores really are pegged, not merely that the run queue is long (`banshee-87l.19`).
        // That is the ONE case that earns the word "saturated"
        // and the fire glyph. Yellow is "busy, keeping up". The claim follows the
        // BAND, not the raw value: a held red is still the model's official
        // position while recovery is unconfirmed. No "I/O wait" — XNU load counts
        // runnable threads only, and in any case this dimension is now busy-time,
        // not load.
        Dimension::Cpu if r.band == Band::Red => {
            format!("CPU is saturated: cores are {:.0}% busy.", r.value * 100.0)
        }
        Dimension::Cpu => format!("CPU is {:.0}% busy.", r.value * 100.0),
        // Red is Apple's "serious"/"critical": the machine IS being slowed. That
        // changes how every load figure should be read, so say so. Yellow is
        // "fair": fans up, headroom shrinking, nothing throttled yet.
        Dimension::Thermal if r.band == Band::Red => format!(
            "The kernel reports {} thermal pressure. The CPU is being throttled, so load \
             figures understate the work being asked of it.",
            thermal_level_name(r.value)
        ),
        Dimension::Thermal => format!(
            "The kernel reports {} thermal pressure — fans up, headroom shrinking.",
            thermal_level_name(r.value)
        ),
        Dimension::Memory => format!("Only {} of RAM available.", fmt_bytes(r.value)),
        Dimension::Swap => {
            let mut m = format!("{} of swap in use.", fmt_bytes(r.value));
            // banshee-cwl: give swap the projection disk already has, but against
            // its OWN ceiling — the allocated pool, exhausted before macOS must grow
            // onto the volume. The volume's own "full in X" (disk dimension) is the
            // next ceiling and is not repeated here.
            if let Some(total) = swap_total.filter(|t| *t > 0) {
                let pct = (r.value / total as f64) * 100.0;
                m.push_str(&format!(" The allocated pool is {pct:.0}% full"));
                if let Some(t) = r.trend_per_sec.filter(|t| *t > 0.0) {
                    let remaining = (total as f64 - r.value).max(0.0);
                    m.push_str(&format!(
                        " and would be exhausted in {} at this rate",
                        fmt_duration(remaining / t)
                    ));
                }
                m.push('.');
            }
            m
        }
        // The one claim no other dimension can make: this is the kernel's own
        // statement about itself, not our inference about the kernel.
        Dimension::KernelPressure => format!(
            "The kernel itself reports {} memory pressure.",
            kernel_level_name(r.value)
        ),
        Dimension::Thrash => format!(
            "Peaked at {:.0} swap operations per second — the machine is thrashing.",
            r.value
        ),
        Dimension::Agents => format!(
            "{:.0} agent sessions past the staleness threshold.",
            r.value
        ),
        Dimension::Orphans => format!(
            "{:.0} orphaned helper processes. Their parent sessions are gone.",
            r.value
        ),
        Dimension::ManagedAgents => format!(
            "Managed agents are using {:.0}% of one core — up from the recorded baseline.",
            r.value
        ),
        Dimension::Disk => {
            let mut m = format!(
                "{} free on the data volume. {}",
                fmt_bytes(r.value),
                r.detail
            );
            // banshee-cwl: swapfiles live on this same volume. When the pool rivals
            // what is free, the disk finding must SAY so — the two reds are one
            // spiral (memory -> swap -> disk -> jetsam), never named before.
            if let Some(total) = swap_total.filter(|t| *t > 0)
                && total as f64 >= r.value
            {
                m.push_str(&format!(
                    " {} of the volume is swap.",
                    fmt_bytes(total as f64)
                ));
            }
            m
        }
        Dimension::Uptime => format!(
            "Up {:.0} days. Long uptime with pinned swap means fragmented, \
             unreclaimable memory.",
            r.value
        ),
        Dimension::KernelFree => {
            format!("The kernel reports {:.0}% of memory reclaimable.", r.value)
        }
        Dimension::Compressor => {
            format!("{} held in the compressor pool.", fmt_bytes(r.value))
        }
        Dimension::Jetsam => format!(
            "The kernel killed {:.0} process(es) under sustained memory pressure.",
            r.value
        ),
    }
}

fn action_for(d: Dimension, band: Band, stale_sessions: usize) -> Action {
    match d {
        Dimension::Agents => Action::ReapStaleSessions,
        Dimension::Orphans => Action::ReapOrphans,
        // Memory pressure's cheapest real lever is the biggest app group:
        // quit-and-relaunch reclaims swapped pages. Measured 2026-08-31: Chrome
        // at 115 processes and 7.71 GB. The kernel's verdict is the same story,
        // so it points at the same lever.
        // The memory cluster's cheapest real lever is relaunching the biggest app
        // group. Jetsam (a kill already happened) and the advisory availability /
        // kernel-free / compressor readings all point at the same lever — free RAM
        // by quitting a heavy app. The standalone Shrieking finding owns the reboot.
        Dimension::Memory
        | Dimension::Swap
        | Dimension::KernelPressure
        | Dimension::Jetsam
        | Dimension::KernelFree
        | Dimension::Compressor => Action::RelaunchApps,
        Dimension::Thrash => Action::RelaunchApps,
        Dimension::Uptime => Action::Reboot,
        // "What is taking the space" is a disk-usage tool's question (ADR-0003).
        Dimension::Disk => Action::OpenDiskTool,
        // Nothing to reap: these agents are centrally managed and the user cannot
        // remove them. Cutting process churn lowers their cost as a side effect,
        // which the sprawl findings already cover.
        Dimension::ManagedAgents => Action::None,
        // Heat has no process to reap: the lever is physical (surface, lid,
        // ambient) or simply time. The CPU dimension already routes a sustained
        // load red to reaping; pointing thermal there too would say the same
        // thing twice and mis-describe the one case where the load is legitimate.
        Dimension::Thermal => Action::None,
        // A saturated CPU routes to reaping ONLY when the census actually has
        // stale sessions to reap. During the 2026-09-15 crisis the popover's one
        // button was "Reap stale agent sessions" over a census reading "0 stale";
        // pressing it opened an empty dry run — the single affordance on screen
        // led nowhere (`banshee-rad`). With no census yet, same answer: an action
        // must not promise something to reap before anyone has looked.
        Dimension::Cpu => {
            if band == Band::Red && stale_sessions > 0 {
                Action::ReapStaleSessions
            } else {
                Action::None
            }
        }
    }
}

#[cfg(test)]
mod tests;
