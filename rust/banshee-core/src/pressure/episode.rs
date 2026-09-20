// pressure/episode — alerts as EPISODES with a lifecycle, not point events
// (ADR-0009).
//
// The hardest part of a monitor is not detecting a problem, it is not becoming
// noise — and the second hardest is not losing the SHAPE of an incident while
// staying quiet. A point-event model rate-limited to one per hour could not tell
// four storms from forty (`banshee-nvd`), never said "it's over", and let two
// dimensions describing one underlying event ring the bell twice.
//
// So: one dimension's continuous incident is ONE episode — `{startedAt, endedAt,
// peak, suppressed, state}` — with an explicit state machine:
//
//   Quiet ──(red held ≥ episode_up_secs)──▶ Firing
//   Firing ──(below red)──▶ Recovering ──(held below red ≥ episode_down_secs)──▶ Closed
//   Recovering ──(red again)──▶ Firing        (a flap REOPENS the episode; one incident)
//
// Everything here is a PURE FUNCTION of stored history plus the open episodes read
// back from the database. There is no inter-call RAM state — a restarted daemon
// reads the open episodes and continues them (ADR-0005's "hysteresis is a replay"
// rule, applied to the alert lifecycle).

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::band::Band;
use super::config::{ALL_DIMENSIONS, Dimension, PressureConfig};
use super::deltas::{CensusSummary, summarise};
use super::level::Level;
use super::{DimensionReading, Finding, Pressure};
use crate::census::Census;

/// Where an episode is in its lifecycle. Carried on the wire rather than left for
/// clients to derive from `endedAt`/`recoveringSince` — two clients deriving it
/// independently would eventually disagree (ADR-0005).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum EpisodeState {
    /// The dimension is red and the episode is live.
    Firing,
    /// The dimension has dropped below red but the incident is not over until it
    /// stays below for `episode_down_secs` (Prometheus `keep_firing_for`).
    Recovering,
    /// Over. `endedAt` is set and exactly one recovery notice was emitted.
    Closed,
}

/// The worst moment of an episode: the band, the whole-machine level, how far
/// past the line, when, and the words the finding used at that moment.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EpisodePeak {
    pub band: Band,
    pub level: Level,
    pub severity: f64,
    #[serde(with = "crate::wire_time")]
    pub at: DateTime<Utc>,
    pub message: String,
    /// The finding's who-line at that moment — who was behind it at the
    /// worst point, kept so `banshee alerts` can answer "what was it last time"
    /// after the census that knew has been swept. Null when the finding named
    /// nobody, and for episodes recorded before the line existed.
    #[serde(default)]
    pub who_line: Option<String>,
}

/// One continuous incident on one dimension. Persisted for a year (ADR-0006) as
/// JSON in `alert_episodes.detail`, so the shape can grow without a migration —
/// which is why every field added after v10 must be `#[serde(default)]`: the
/// store's read skips rows that fail to parse, and a required new field would
/// silently erase every episode written before it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AlertEpisode {
    pub id: Uuid,
    pub dimension: Dimension,
    /// When the dimension's red run began — `now - heldSecs` at the moment the
    /// episode opened, not the moment it crossed the up delay. The delay decides
    /// WHETHER to speak; it is not when the trouble started.
    #[serde(with = "crate::wire_time")]
    pub started_at: DateTime<Utc>,
    /// When the dimension last dropped below red, once the drop has held long
    /// enough to count. Null while the episode is open.
    #[serde(with = "crate::wire_time::option")]
    pub ended_at: Option<DateTime<Utc>>,
    pub peak: EpisodePeak,
    /// Who was running when the peak was recorded: a compact summary of
    /// the newest census at the moment the episode opened or its peak last
    /// moved — the who-line's inputs, a few hundred bytes. STORED, not
    /// recomputed: the samples and censuses behind a four-hour incident are
    /// swept long before anyone asks "what changed since it started", and this
    /// is the record's half of that comparison. Null when no census had been
    /// taken at the peak, and for episodes recorded before the field existed.
    #[serde(default)]
    pub census_at_peak: Option<CensusSummary>,
    /// How many re-reddenings this episode absorbed inside the repeat interval —
    /// each one a notification the point-event model would have rate-limited
    /// away. "+39 more" is the difference between one storm and forty.
    #[serde(default)]
    pub suppressed: u32,
    pub state: EpisodeState,
    /// When a notice for this episode last actually went out. Null when every
    /// notice so far was inhibited (see `inhibitors`).
    #[serde(default, with = "crate::wire_time::option")]
    pub last_notified_at: Option<DateTime<Utc>>,
    /// When the dimension dropped below red, while `Recovering`. Null while
    /// `Firing`; kept once `Closed` (it becomes `endedAt`).
    #[serde(default, with = "crate::wire_time::option")]
    pub recovering_since: Option<DateTime<Utc>>,
    /// The smallest `disk_projection_notify_secs` threshold the projected
    /// time-to-full has already crossed inside this episode — the state behind
    /// the disk re-notification rule (`banshee-3sn`). Monotonic while the
    /// episode is open: a projection that recovers and then worsens past the
    /// same line again is a re-fire, not new news. Null for every dimension but
    /// Disk, and while no projection has crossed a line.
    #[serde(default)]
    pub projection_bracket_secs: Option<u64>,
}

impl AlertEpisode {
    pub fn is_open(&self) -> bool {
        self.ended_at.is_none()
    }

    /// Whether the episode was live at any instant in `[from, to]`.
    pub fn active_within(&self, from: DateTime<Utc>, to: DateTime<Utc>) -> bool {
        self.started_at <= to && self.ended_at.is_none_or(|e| e >= from)
    }
}

/// What kind of news a notification carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum NotificationKind {
    /// A new episode crossed the up delay.
    Opened,
    /// The whole-machine level worsened past the episode's recorded peak while it
    /// was firing — Wailing to Shrieking is the Slack-worthy case.
    Reescalated,
    /// Still firing after `episode_repeat_secs`; a reminder, not new news.
    Repeat,
    /// The episode closed. Emitted exactly once per episode.
    Recovered,
}

/// Something to SAY. Distinct from the episode, which is something to RECORD:
/// every episode is written, most evaluations say nothing.
#[derive(Debug, Clone, PartialEq)]
pub struct Notification {
    pub kind: NotificationKind,
    pub episode_id: Uuid,
    pub dimension: Dimension,
    /// The whole-machine level NOW, which routes delivery: banners at Wailing,
    /// Slack at Shrieking.
    pub level: Level,
    /// The episode's peak level, which routes the RECOVERY notice: a crisis that
    /// was announced at Shrieking is owed its all-clear on the same channel.
    pub peak_level: Level,
    pub message: String,
    pub at: DateTime<Utc>,
    /// The episode's suppressed count at the moment of the notice, so a sink can
    /// render "+N more" without a second read.
    pub suppressed: u32,
}

impl Notification {
    /// Whether this should interrupt a person.
    ///
    /// For a recovery the question is not "is the machine bad now" but "was the
    /// user told it was bad, and is it genuinely over": the peak warranted a
    /// notification AND the machine is back at or below Stirring — the recovery
    /// clause of `level_changed_materially`, and ONLY that clause. A dimension
    /// recovering while another still holds the machine at Wailing is not good news
    /// yet, and announcing it would announce bad news as if it were good.
    ///
    /// Found by RUNNING the daemon (2026-09-07 dogfood): the first version reused
    /// `level_changed_materially(peak, now)` whole, whose ESCALATION clause is also
    /// true when the machine is worse now than the episode's peak — so a
    /// Restless-peak orphans episode closing while cpu had driven the machine to
    /// Shrieking would have announced its own recovery. Every unit fixture had
    /// `now <= peak`, so the two clauses never came apart.
    pub fn warrants_notification(&self) -> bool {
        match self.kind {
            NotificationKind::Recovered => {
                self.peak_level.warrants_notification() && self.level <= Level::Stirring
            }
            _ => self.level.warrants_notification(),
        }
    }

    /// Whether this should escape the machine (the Slack sink).
    pub fn warrants_escalation(&self) -> bool {
        match self.kind {
            NotificationKind::Recovered => {
                self.peak_level.warrants_escalation() && self.warrants_notification()
            }
            _ => self.level.warrants_escalation(),
        }
    }
}

/// The result of one reconciliation: the episodes that changed (to upsert) and
/// what to say about them (after inhibition).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Reconciled {
    pub episodes: Vec<AlertEpisode>,
    pub notifications: Vec<Notification>,
}

/// Which FIRING dimensions mute another dimension's notices — never its episode.
///
/// A kernel-thrash red and a swap-volume red are one underlying event: the
/// compressor is churning because there is no RAM, and what it cannot hold has
/// spilled to disk. The thrash (swap ops/sec) is the actionable signal ("the
/// machine feels terrible, here is why"); the swap-volume notice on top of it is
/// the same story told twice, and a channel that hears every story twice gets
/// muted. Banding stays independent and honest — the swap episode still records,
/// still shows in `status` and `/alerts`; it just does not ring the bell while
/// thrash is already ringing it. (Free-memory availability, the notice this row
/// muted before `banshee-yk8`, is now advisory: it never opens an episode and so
/// never needs muting. Swap is the loud memory-volume signal that remains.)
///
/// Applied in the NOTIFICATION step only (ADR-0009 decision 7). A small static
/// table on purpose: inhibition is a claim about causality, and every row here is
/// a claim someone should be able to read and dispute.
pub fn inhibitors(d: Dimension) -> &'static [Dimension] {
    match d {
        Dimension::Swap => &[Dimension::Thrash],
        _ => &[],
    }
}

/// Advance every dimension's episode by one evaluation. PURE.
///
/// `open` is every episode with `endedAt == null`, read from the store (any
/// order). `census` is the NEWEST census, if one has been taken: an episode that
/// opens or moves its peak stores a summary of it as `censusAtPeak`. Returns the
/// episodes that changed — new, transitioned, or with an updated peak/suppressed
/// count — and the notifications that survived inhibition. Unchanged episodes
/// are not returned; the caller upserts only what came back.
pub fn reconcile_episodes(
    open: &[AlertEpisode],
    pressure: &Pressure,
    census: Option<&Census>,
    now: DateTime<Utc>,
    config: &PressureConfig,
) -> Reconciled {
    // NOTHING moves while the verdict is `Checking`.
    //
    // Found by running the daemon (2026-08-31): on the first sample the level is
    // Checking, but individual dimensions already have bands. The point-event
    // model alerted here — events that could never be delivered, but that burned
    // the per-dimension cooldown, eating the first hour of real alerting after
    // every start. The episode model has a second hazard in the same place: a
    // restart mid-crisis must not read "no trustworthy reading yet" as "below
    // red" and start the recovery clock on an ongoing episode. So an untrusted
    // verdict leaves every open episode exactly as it was.
    if pressure.level == Level::Checking {
        return Reconciled::default();
    }

    let mut stepped: Vec<(AlertEpisode, Option<Notification>)> = Vec::new();
    let mut firing_after: Vec<Dimension> = Vec::new();

    for d in ALL_DIMENSIONS {
        let existing = open.iter().find(|e| e.dimension == d && e.is_open());
        let reading = pressure.reading(d);
        let outcome = match (existing, reading) {
            (Some(e), Some(r)) => step(e, r, pressure, census, now, config),
            (None, Some(r)) => open_if_due(r, pressure, census, now, config).map(|e| (e, true)),
            // An open episode whose dimension is not observed this tick (no census
            // yet, a corporate reading skipped for a young process) is left alone:
            // "not measured" is not "below red".
            (Some(_), None) | (None, None) => None,
        };

        // Track who is firing AFTER this tick, for inhibition — including the
        // unchanged open episodes, which `stepped` does not carry.
        let firing_now = match &outcome {
            Some((e, _)) => e.state == EpisodeState::Firing,
            None => existing.is_some_and(|e| e.state == EpisodeState::Firing),
        };
        if firing_now {
            firing_after.push(d);
        }

        if let Some((episode, changed)) = outcome
            && changed
        {
            let notification = pending_notification(&episode, existing, pressure, now);
            stepped.push((episode, notification));
        }
    }

    let mut out = Reconciled::default();
    for (mut episode, notification) in stepped {
        if let Some(n) = notification {
            let inhibited = inhibitors(n.dimension)
                .iter()
                .any(|inhibitor| firing_after.contains(inhibitor));
            if !inhibited {
                // A notice that actually went out resets the repeat clock. An
                // inhibited one does NOT: when the inhibitor clears, the muted
                // dimension speaks on its next evaluation rather than an hour later.
                if n.kind != NotificationKind::Recovered {
                    episode.last_notified_at = Some(now);
                }
                out.notifications.push(n);
            }
        }
        out.episodes.push(episode);
    }
    out
}

/// A new episode, if the reading has been red — continuously observed — for the
/// up delay. `None` otherwise, which is the normal case and the whole point.
fn open_if_due(
    r: &DimensionReading,
    pressure: &Pressure,
    census: Option<&Census>,
    now: DateTime<Utc>,
    config: &PressureConfig,
) -> Option<AlertEpisode> {
    // Red is the actionable state. A yellow dimension is worth SEEING, in the
    // window and the time series, but not worth an episode — and interrupting
    // for it is exactly how a monitor gets muted.
    if r.band != Band::Red {
        return None;
    }
    // Advisory dimensions never alert. A 30-day uptime is not an event.
    if r.advisory {
        return None;
    }
    // The up delay reuses `held_secs`, which counts only CONTINUOUS observation:
    // a red that "held" across a 17-minute hole in the series
    // is 45 seconds of evidence, not 17 minutes, and does not open an episode.
    if r.held_secs < config.episode_up_secs {
        return None;
    }
    let mut episode = AlertEpisode {
        id: Uuid::new_v4(),
        dimension: r.dimension,
        started_at: now - Duration::seconds(r.held_secs as i64),
        ended_at: None,
        peak: peak_now(r, pressure, now),
        // The record's half of "what changed since this started": who was
        // running at the moment the peak was set.
        census_at_peak: census.map(summarise),
        suppressed: 0,
        state: EpisodeState::Firing,
        last_notified_at: None,
        recovering_since: None,
        projection_bracket_secs: None,
    };
    // Seed the projection bracket at open: the Opened notice already carries
    // the current projection in its message, so the first CROSSING after this
    // is the next news, not the state at open re-announced.
    record_projection(&mut episode, r, config);
    Some(episode)
}

/// Fold the projected time-to-full into a disk episode's bracket. Returns true
/// when the projection crossed INTO a smaller configured threshold than any
/// already recorded — the escalation that bypasses `episode_repeat_secs`
/// (`banshee-3sn`: a projection collapsing from 5.8 hours to 22 minutes inside
/// an open episode produced no second notice, because `record_peak` re-escalates
/// only on a LEVEL rise and the level was already Shrieking from memory).
fn record_projection(
    next: &mut AlertEpisode,
    r: &DimensionReading,
    config: &PressureConfig,
) -> bool {
    // Only disk has a time-to-exhaustion axis; every other dimension's trend is
    // not a countdown to zero.
    if next.dimension != Dimension::Disk {
        return false;
    }
    let Some(projection) = super::disk::projected_full_secs(r.value, r.trend_per_sec) else {
        return false;
    };
    let crossed = config
        .disk_projection_notify_secs
        .iter()
        .copied()
        .filter(|b| projection <= *b as f64)
        .min();
    match (crossed, next.projection_bracket_secs) {
        (Some(b), None) => {
            next.projection_bracket_secs = Some(b);
            true
        }
        (Some(b), Some(prev)) if b < prev => {
            next.projection_bracket_secs = Some(b);
            true
        }
        _ => false,
    }
}

/// Advance one open episode against its dimension's current reading. Returns the
/// episode and whether anything about it changed.
fn step(
    e: &AlertEpisode,
    r: &DimensionReading,
    pressure: &Pressure,
    census: Option<&Census>,
    now: DateTime<Utc>,
    config: &PressureConfig,
) -> Option<(AlertEpisode, bool)> {
    let mut next = e.clone();
    let red = r.band == Band::Red;
    let repeat = Duration::seconds(config.episode_repeat_secs as i64);
    let repeat_due = next.last_notified_at.is_none_or(|t| now - t >= repeat);

    match (e.state, red) {
        (EpisodeState::Firing, true) => {
            let escalated = record_peak(&mut next, r, pressure, census, now);
            let crossed = record_projection(&mut next, r, config);
            // Not "changed" for a quiet tick: a firing episode that neither
            // escalated nor hit the repeat clock is exactly as stored, and
            // rewriting it 240 times an hour buys nothing.
            let changed = escalated || crossed || repeat_due || next != *e;
            Some((next, changed))
        }
        (EpisodeState::Firing, false) => {
            next.state = EpisodeState::Recovering;
            next.recovering_since = Some(now);
            Some((next, true))
        }
        (EpisodeState::Recovering, true) => {
            // A flap: red again before the down window closed. The SAME episode
            // reopens — one incident, not two — and unless the repeat clock is
            // due this re-fire is a notification the model swallowed, which is
            // what `suppressed` counts.
            next.state = EpisodeState::Firing;
            next.recovering_since = None;
            let escalated = record_peak(&mut next, r, pressure, census, now);
            let crossed = record_projection(&mut next, r, config);
            if !escalated && !crossed && !repeat_due {
                next.suppressed = next.suppressed.saturating_add(1);
            }
            Some((next, true))
        }
        (EpisodeState::Recovering, false) => {
            let since = e.recovering_since.unwrap_or(now);
            if now - since >= Duration::seconds(config.episode_down_secs as i64) {
                next.state = EpisodeState::Closed;
                // The incident ended when the dimension dropped below red, not
                // when we finished waiting to be sure.
                next.ended_at = Some(since);
                Some((next, true))
            } else {
                None
            }
        }
        // A closed episode is never in `open`; nothing to step.
        (EpisodeState::Closed, _) => None,
    }
}

/// Fold the current reading into the episode's peak. Returns true when the
/// whole-machine LEVEL rose past the recorded peak — the re-escalation that
/// bypasses the repeat interval. A rise in severity within the same level is
/// recorded silently: a red drifting deeper into red is the same news.
fn record_peak(
    next: &mut AlertEpisode,
    r: &DimensionReading,
    pressure: &Pressure,
    census: Option<&Census>,
    now: DateTime<Utc>,
) -> bool {
    if pressure.level > next.peak.level {
        move_peak(next, r, pressure, census, now);
        return true;
    }
    if r.severity > next.peak.severity {
        move_peak(next, r, pressure, census, now);
    }
    false
}

/// The peak and the census behind it move TOGETHER, and only together: the
/// stored census describes the moment the peak was set, not the newest tick.
/// A quiet tick with a fresh census leaves both alone, so "who was running at
/// the worst moment" stays the worst moment's answer.
fn move_peak(
    next: &mut AlertEpisode,
    r: &DimensionReading,
    pressure: &Pressure,
    census: Option<&Census>,
    now: DateTime<Utc>,
) {
    next.peak = peak_now(r, pressure, now);
    next.census_at_peak = census.map(summarise);
}

fn peak_now(r: &DimensionReading, pressure: &Pressure, now: DateTime<Utc>) -> EpisodePeak {
    EpisodePeak {
        band: r.band,
        level: pressure.level,
        severity: r.severity,
        at: now,
        message: message_for(r, pressure),
        who_line: finding_for(r, pressure).and_then(|f| f.who_line.clone()),
    }
}

/// What, if anything, this change is worth saying. `before` is the episode as it
/// was stored (None for a new one).
fn pending_notification(
    after: &AlertEpisode,
    before: Option<&AlertEpisode>,
    pressure: &Pressure,
    now: DateTime<Utc>,
) -> Option<Notification> {
    let reading = pressure.reading(after.dimension);
    let message = |fallback: &str| {
        reading
            .map(|r| message_for(r, pressure))
            .unwrap_or_else(|| fallback.to_string())
    };
    let kind = match (before, after.state) {
        (None, EpisodeState::Firing) => NotificationKind::Opened,
        (Some(_), EpisodeState::Closed) => NotificationKind::Recovered,
        (Some(b), EpisodeState::Firing) => {
            if after.peak.level > b.peak.level {
                NotificationKind::Reescalated
            } else if after.projection_bracket_secs != b.projection_bracket_secs
                && after.projection_bracket_secs.is_some()
            {
                // The disk projection crossed a notify threshold (`banshee-3sn`):
                // "full in 22 min" inside an episode announced at "red since
                // 11:51" is new news, exactly as a level rise is. The message
                // below reuses the finding's wording, which carries the current
                // projection.
                NotificationKind::Reescalated
            } else if b.suppressed != after.suppressed {
                // A swallowed re-fire: counted, not spoken.
                return None;
            } else if b.state == EpisodeState::Firing && after.peak != b.peak {
                // A silent peak update (severity), nothing else.
                return None;
            } else {
                NotificationKind::Repeat
            }
        }
        // Firing → Recovering, or an unchanged state: nothing to say.
        _ => return None,
    };

    let text = match kind {
        NotificationKind::Recovered => {
            let ended = after.ended_at.unwrap_or(now);
            // "Orphaned helpers recovered after 1m (now 6 orphaned)". The reading's
            // detail, not the finding's message — a recovered dimension has no
            // finding, and the label is already the subject of the sentence.
            let now_detail = reading
                .map(|r| format!(" (now {})", r.detail))
                .unwrap_or_default();
            format!(
                "{} recovered after {}{}",
                after.dimension.label(),
                fmt_secs((ended - after.started_at).num_seconds().max(0) as u64),
                now_detail
            )
        }
        // "35.6 GB of swap in use. Red for 4m; who: Chrome ×115 at 7.7 GB, claude
        // ×8 at 2.1 GB." — the finding's wording, then how long, then who.
        // The duration rather than a clock time: it is honest across time zones
        // and a repeat an hour in reads "red for 1h2m", which is the news.
        _ => {
            let mut text = message(&after.peak.message);
            text.push_str(&format!(
                " Red for {}",
                fmt_secs((now - after.started_at).num_seconds().max(0) as u64)
            ));
            if let Some(who) = reading
                .and_then(|r| finding_for(r, pressure))
                .and_then(|f| f.who_line.as_deref())
            {
                text.push_str("; ");
                text.push_str(who);
            }
            text.push('.');
            text
        }
    };

    Some(Notification {
        kind,
        episode_id: after.id,
        dimension: after.dimension,
        level: pressure.level,
        peak_level: after.peak.level,
        message: text,
        at: now,
        suppressed: after.suppressed,
    })
}

fn finding_for<'a>(r: &DimensionReading, p: &'a Pressure) -> Option<&'a Finding> {
    p.findings.iter().find(|f| f.dimension == r.dimension)
}

fn message_for(r: &DimensionReading, p: &Pressure) -> String {
    // Reuse the finding's wording when there is one, so an alert and the worklist
    // never describe the same condition in two different ways.
    if let Some(f) = finding_for(r, p) {
        return f.message.clone();
    }
    format!("{}: {}", r.label, r.detail)
}

fn fmt_secs(secs: u64) -> String {
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m", secs / 60)
    } else if secs < 86_400 {
        format!("{}h{}m", secs / 3600, (secs % 3600) / 60)
    } else {
        format!("{}d{}h", secs / 86_400, (secs % 86_400) / 3600)
    }
}

/// Whether a level CHANGE is itself worth announcing, independent of any
/// dimension.
///
/// Used for the "the machine recovered" message: a user who was told the machine
/// was shrieking is owed the news that it stopped. Since ADR-0009 this also
/// routes every episode's recovery notice (`Notification::warrants_notification`).
pub fn level_changed_materially(previous: Option<Level>, current: Level) -> bool {
    match previous {
        None => current.warrants_notification(),
        Some(prev) => {
            // Escalation into notification territory, or full recovery from it.
            (current > prev && current.warrants_notification())
                || (prev.warrants_notification() && current <= Level::Stirring)
        }
    }
}

/// The long memory, summarised for `status` and the pressure read:
/// how many episodes are live, and how many were active in the
/// last hour and the last day. Counts ACTIVITY, not onsets — an episode that
/// opened five hours ago and is still firing was very much part of the last hour.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecentActivity {
    /// Firing or Recovering right now.
    pub open: usize,
    /// Active at any point in the last hour.
    pub last_hour: usize,
    /// Active at any point in the last 24 hours.
    pub last_day: usize,
}

impl RecentActivity {
    pub fn summarise(episodes: &[AlertEpisode], now: DateTime<Utc>) -> Self {
        let within = |d: Duration| {
            episodes
                .iter()
                .filter(|e| e.active_within(now - d, now))
                .count()
        };
        Self {
            open: episodes.iter().filter(|e| e.is_open()).count(),
            last_hour: within(Duration::hours(1)),
            last_day: within(Duration::hours(24)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::census::{AppGroup, HelperRollup, MonitorTotal, OrphanCensus};
    use crate::pressure::config::Unit;

    fn at(secs: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(1_788_100_000 + secs, 0).unwrap()
    }

    /// A census whose one Chrome app group carries `chrome_rss` and nothing else,
    /// so two censuses can be told apart by that single number — the co-variance
    /// breaker for "was the census captured at the peak, or at the newest tick".
    fn census_with_chrome(chrome_rss: u64) -> Census {
        Census {
            id: Uuid::new_v4(),
            taken_at: at(0),
            total_procs: 100,
            agent_sessions: Vec::new(),
            ide_helpers: HelperRollup::default(),
            orphans: OrphanCensus::default(),
            tmux_sessions: Vec::new(),
            app_groups: vec![AppGroup {
                name: "Chrome".into(),
                proc_count: 10,
                rss_bytes: chrome_rss,
            }],
            monitor_agents: Vec::new(),
            monitor_total: MonitorTotal::default(),
            tmux_available: true,
            cpu_consumers: Vec::new(),
        }
    }

    fn reading(dimension: Dimension, band: Band, held_secs: u64) -> DimensionReading {
        DimensionReading {
            dimension,
            key: dimension.key().to_string(),
            label: dimension.label().to_string(),
            band,
            value: 99.0,
            unit: Unit::Ratio,
            severity: if band == Band::Red { 2.0 } else { -0.5 },
            held_secs,
            observation_gap_secs: None,
            trend_per_sec: None,
            detail: "detail".into(),
            advisory: dimension.is_advisory(),
            pending: None,
            recovering: false,
        }
    }

    fn pressure(level: Level, readings: Vec<DimensionReading>) -> Pressure {
        Pressure {
            evaluated_at: at(0),
            level,
            level_name: level.name().to_string(),
            source: None,
            glyph: level.glyph().to_string(),
            accessibility_label: format!("Banshee: {}", level.name()),
            dimensions: readings,
            findings: Vec::new(),
            sample_count: 40,
            census_count: 2,
            recovering: false,
            activity: RecentActivity::default(),
        }
    }

    fn red(dimension: Dimension, held_secs: u64, level: Level) -> Pressure {
        pressure(level, vec![reading(dimension, Band::Red, held_secs)])
    }

    fn cfg() -> PressureConfig {
        PressureConfig::default()
    }

    /// An open, firing episode as the store would hand it back.
    fn firing(
        dimension: Dimension,
        started: i64,
        notified: Option<i64>,
        peak: Level,
    ) -> AlertEpisode {
        AlertEpisode {
            id: Uuid::new_v4(),
            dimension,
            started_at: at(started),
            ended_at: None,
            peak: EpisodePeak {
                band: Band::Red,
                level: peak,
                severity: 2.0,
                at: at(started),
                message: "prior".into(),
                who_line: None,
            },
            census_at_peak: None,
            suppressed: 0,
            state: EpisodeState::Firing,
            last_notified_at: notified.map(at),
            recovering_since: None,
            projection_bracket_secs: None,
        }
    }

    fn recovering(dimension: Dimension, started: i64, since: i64, peak: Level) -> AlertEpisode {
        AlertEpisode {
            state: EpisodeState::Recovering,
            recovering_since: Some(at(since)),
            last_notified_at: Some(at(started)),
            ..firing(dimension, started, Some(started), peak)
        }
    }

    fn kinds(r: &Reconciled) -> Vec<NotificationKind> {
        r.notifications.iter().map(|n| n.kind).collect()
    }

    // ---- opening ---------------------------------------------------------

    /// The startup defect, in its episode form: at `Checking` the dimensions
    /// already have bands but the verdict is not trustworthy. Nothing may OPEN —
    /// and, new with episodes, nothing may MOVE either: a restart mid-crisis must
    /// not start the recovery clock on an ongoing episode because the first
    /// sample has no verdict. Mutation-proof: remove the `Checking` guard and the
    /// open swap episode below moves to Recovering.
    #[test]
    fn nothing_opens_or_moves_while_the_verdict_is_still_checking() {
        // KernelPressure (a banded memory dimension), not Memory: availability is
        // advisory since `banshee-yk8` and would never open regardless, which would
        // silently defang this guard test. A banded dimension's red WOULD open with
        // the guard gone.
        let mut p = red(Dimension::KernelPressure, 600, Level::Checking);
        // Swap is NOT among the readings (unobserved), but with the guard gone
        // the kernel-pressure red would open and the `(Some, None)` arm would...
        // leave swap alone. So give swap a GREEN reading: that is the case where an
        // unguarded reconcile would push the open episode into Recovering.
        p.dimensions.push(reading(Dimension::Swap, Band::Green, 30));
        let open = vec![firing(Dimension::Swap, -3600, Some(-3600), Level::Wailing)];
        let out = reconcile_episodes(&open, &p, None, at(0), &cfg());
        assert_eq!(
            out,
            Reconciled::default(),
            "an untrusted verdict changes nothing"
        );
    }

    /// …and once the verdict IS trusted, the same reading opens an episode. Paired
    /// with the test above so the guard cannot be satisfied by silencing everything.
    #[test]
    fn a_sustained_red_opens_a_firing_episode_and_says_so_once() {
        // Swap, not Memory: since `banshee-yk8` the availability dimension is
        // advisory and advisory dimensions never alert, so a banded memory
        // dimension is the right subject for the generic episode-opening path.
        let p = red(Dimension::Swap, 600, Level::Wailing);
        let out = reconcile_episodes(&[], &p, None, at(0), &cfg());
        assert_eq!(out.episodes.len(), 1);
        let e = &out.episodes[0];
        assert_eq!(e.dimension, Dimension::Swap);
        assert_eq!(e.state, EpisodeState::Firing);
        assert_eq!(e.ended_at, None);
        assert_eq!(e.peak.level, Level::Wailing);
        assert_eq!(e.peak.band, Band::Red);
        assert_eq!(e.suppressed, 0);
        assert_eq!(
            e.last_notified_at,
            Some(at(0)),
            "the opening notice went out"
        );
        assert_eq!(kinds(&out), vec![NotificationKind::Opened]);
        assert!(out.notifications[0].warrants_notification());
    }

    /// `startedAt` is when the red RUN began, not when the up delay was crossed.
    /// Mutation-proof: use `now` and this fails — the two readings come apart by
    /// exactly `held_secs`.
    #[test]
    fn the_episode_starts_when_the_red_run_began_not_when_it_crossed_the_delay() {
        let p = red(Dimension::Cpu, 600, Level::Wailing);
        let out = reconcile_episodes(&[], &p, None, at(0), &cfg());
        assert_eq!(out.episodes[0].started_at, at(-600));
    }

    /// The up delay: a red that has not been held long enough does not open. The
    /// fixture is red with held 119s against a 120s delay, so a `>` vs `>=` slip or
    /// a dropped guard both fail. Mutation-proof: remove the `held_secs <
    /// episode_up_secs` check.
    #[test]
    fn a_red_below_the_up_delay_does_not_open() {
        let c = PressureConfig {
            episode_up_secs: 120,
            ..cfg()
        };
        let p = red(Dimension::Cpu, 119, Level::Restless);
        assert_eq!(
            reconcile_episodes(&[], &p, None, at(0), &c),
            Reconciled::default()
        );
        let p = red(Dimension::Cpu, 120, Level::Restless);
        assert_eq!(
            reconcile_episodes(&[], &p, None, at(0), &c).episodes.len(),
            1,
            "at the line"
        );
    }

    /// The up delay is CONFIGURED, and the config is load-bearing: the same
    /// reading opens under one value and not under another. A config value that
    /// silently does nothing is worse than none.
    /// Mutation-proof: hardcode 120 in `open_if_due` and the 300s case opens.
    #[test]
    fn the_up_delay_is_load_bearing_config() {
        let p = red(Dimension::Cpu, 200, Level::Wailing);
        let short = PressureConfig {
            episode_up_secs: 60,
            ..cfg()
        };
        let long = PressureConfig {
            episode_up_secs: 300,
            ..cfg()
        };
        assert_eq!(
            reconcile_episodes(&[], &p, None, at(0), &short)
                .episodes
                .len(),
            1
        );
        assert!(
            reconcile_episodes(&[], &p, None, at(0), &long)
                .episodes
                .is_empty()
        );
    }

    /// Held time counts only CONTINUOUS observation, and the episode inherits that
    /// honesty for free — but only if it reads `held_secs` rather than, say,
    /// `now - first sample`. A red with 45s held behind a 17-minute gap is 45s of
    /// evidence. Mutation-proof: ignore `held_secs` (treat any red as due).
    #[test]
    fn a_red_behind_a_gap_is_judged_on_its_observed_run_only() {
        let mut r = reading(Dimension::Swap, Band::Red, 45);
        r.observation_gap_secs = Some(1029);
        let p = pressure(Level::Restless, vec![r]);
        assert!(
            reconcile_episodes(&[], &p, None, at(0), &cfg())
                .episodes
                .is_empty()
        );
    }

    /// Yellow is worth SEEING, not an episode. Mutation-proof: open on
    /// `>= Yellow` and a merely-uneasy machine starts a record.
    #[test]
    fn a_yellow_dimension_does_not_open() {
        let p = pressure(
            Level::Stirring,
            vec![reading(Dimension::Cpu, Band::Yellow, 900)],
        );
        assert!(
            reconcile_episodes(&[], &p, None, at(0), &cfg())
                .episodes
                .is_empty()
        );
    }

    /// Advisory dimensions never alert, however red and however long.
    /// Mutation-proof: drop the advisory filter.
    #[test]
    fn an_advisory_dimension_never_opens() {
        let p = pressure(
            Level::Wailing,
            vec![reading(Dimension::Uptime, Band::Red, 86_400)],
        );
        assert!(
            reconcile_episodes(&[], &p, None, at(0), &cfg())
                .episodes
                .is_empty()
        );
    }

    /// Two red dimensions open two episodes: episodes are PER DIMENSION, so a
    /// thrashing machine cannot silence a filling disk.
    #[test]
    fn episodes_are_per_dimension() {
        let p = pressure(
            Level::Shrieking,
            vec![
                reading(Dimension::Cpu, Band::Red, 600),
                reading(Dimension::Disk, Band::Red, 600),
            ],
        );
        let out = reconcile_episodes(&[], &p, None, at(0), &cfg());
        let dims: Vec<Dimension> = out.episodes.iter().map(|e| e.dimension).collect();
        assert_eq!(dims, vec![Dimension::Cpu, Dimension::Disk]);
        assert_eq!(out.notifications.len(), 2);
    }

    fn cpu_finding(who_line: Option<&str>) -> crate::pressure::Finding {
        let who = who_line
            .map(|_| {
                vec![crate::pressure::who::Consumer {
                    name: "claude".into(),
                    count: 8,
                    detail: "62% of one core".into(),
                }]
            })
            .unwrap_or_default();
        crate::pressure::Finding {
            dimension: Dimension::Cpu,
            band: Band::Red,
            message: "Load is 9.5× the core count.".into(),
            action: crate::pressure::Action::ReapStaleSessions,
            action_label: crate::pressure::Action::ReapStaleSessions
                .label()
                .to_string(),
            who,
            who_line: who_line.map(str::to_string),
        }
    }

    /// The opening notice LEADS with the finding's wording, so the banner and the
    /// worklist never describe one condition in two ways; then how long it has
    /// been red, then who is behind it. The peak keeps the finding's
    /// message as-is and the who-line beside it.
    #[test]
    fn the_notice_reuses_the_findings_wording_then_says_how_long_and_who() {
        let mut p = red(Dimension::Cpu, 600, Level::Wailing);
        p.findings
            .push(cpu_finding(Some("who: claude ×8 at 62% of one core")));
        let out = reconcile_episodes(&[], &p, None, at(0), &cfg());
        assert_eq!(
            out.notifications[0].message,
            "Load is 9.5× the core count. Red for 10m; who: claude ×8 at 62% of one core."
        );
        assert_eq!(out.episodes[0].peak.message, "Load is 9.5× the core count.");
        assert_eq!(
            out.episodes[0].peak.who_line.as_deref(),
            Some("who: claude ×8 at 62% of one core")
        );
    }

    /// A finding that names nobody (no census yet, or a disk finding) produces a
    /// notice with no dangling "who:" and a peak with a null who-line.
    /// Mutation-proof with the test above: hardcode either branch and one fails.
    #[test]
    fn a_notice_for_a_finding_that_names_nobody_has_no_who_clause() {
        let mut p = red(Dimension::Cpu, 600, Level::Wailing);
        p.findings.push(cpu_finding(None));
        let out = reconcile_episodes(&[], &p, None, at(0), &cfg());
        assert_eq!(
            out.notifications[0].message,
            "Load is 9.5× the core count. Red for 10m."
        );
        assert_eq!(out.episodes[0].peak.who_line, None);
    }

    /// The who-line is re-read from the CURRENT finding on every notice, so a
    /// repeat an hour in names who is behind it now, not who opened it.
    #[test]
    fn a_repeat_names_who_is_behind_it_now() {
        let mut p = red(Dimension::Cpu, 7200, Level::Wailing);
        p.findings
            .push(cpu_finding(Some("who: Chrome ×115 at 7.7 GB")));
        let opened = firing(Dimension::Cpu, -7200, Some(-3601), Level::Wailing);
        let out = reconcile_episodes(&[opened], &p, None, at(0), &cfg());
        assert_eq!(kinds(&out), vec![NotificationKind::Repeat]);
        assert!(
            out.notifications[0]
                .message
                .ends_with("Red for 2h0m; who: Chrome ×115 at 7.7 GB."),
            "{}",
            out.notifications[0].message
        );
    }

    // ---- firing: repeat, suppression, re-escalation ---------------------

    /// A firing episode inside the repeat interval says nothing and — the point —
    /// is not even rewritten. Mutation-proof: drop the repeat check and a
    /// sustained red notifies on every 15-second sample, 240 an hour.
    #[test]
    fn a_firing_episode_inside_the_repeat_interval_stays_quiet() {
        let open = vec![firing(Dimension::Cpu, -1800, Some(-1800), Level::Wailing)];
        let p = red(Dimension::Cpu, 2400, Level::Wailing);
        let out = reconcile_episodes(&open, &p, None, at(0), &cfg());
        assert_eq!(
            out,
            Reconciled::default(),
            "nothing to say, nothing to write"
        );
    }

    /// Once the repeat interval has elapsed the episode speaks again, as a
    /// REPEAT, and the clock resets. The interval is the old cooldown (3600s).
    #[test]
    fn a_firing_episode_repeats_after_the_repeat_interval() {
        let open = vec![firing(Dimension::Cpu, -7200, Some(-3601), Level::Wailing)];
        let p = red(Dimension::Cpu, 7200, Level::Wailing);
        let out = reconcile_episodes(&open, &p, None, at(0), &cfg());
        assert_eq!(kinds(&out), vec![NotificationKind::Repeat]);
        assert_eq!(out.episodes[0].last_notified_at, Some(at(0)));
        assert_eq!(
            out.episodes[0].id, open[0].id,
            "the SAME episode, not a new one"
        );
    }

    /// The repeat interval is load-bearing config. Mutation-proof: hardcode 3600
    /// in `step` and the 60s config below stops repeating at 90s.
    #[test]
    fn the_repeat_interval_is_load_bearing_config() {
        let open = vec![firing(Dimension::Cpu, -600, Some(-90), Level::Wailing)];
        let p = red(Dimension::Cpu, 600, Level::Wailing);
        let brief = PressureConfig {
            episode_repeat_secs: 60,
            ..cfg()
        };
        assert_eq!(
            kinds(&reconcile_episodes(&open, &p, None, at(0), &brief)),
            vec![NotificationKind::Repeat]
        );
        assert!(
            reconcile_episodes(&open, &p, None, at(0), &cfg())
                .notifications
                .is_empty()
        );
    }

    /// Re-escalation bypasses the repeat interval: an episode opened at Wailing
    /// whose machine is now Shrieking is new news (and Slack-worthy). The peak
    /// moves with it. Mutation-proof: compare severity instead of level in
    /// `record_peak` — the fixture keeps severity FLAT so only the level differs.
    #[test]
    fn a_worsening_level_reescalates_inside_the_repeat_interval() {
        let open = vec![firing(Dimension::Swap, -600, Some(-60), Level::Wailing)];
        let p = red(Dimension::Swap, 600, Level::Shrieking);
        let out = reconcile_episodes(&open, &p, None, at(0), &cfg());
        assert_eq!(kinds(&out), vec![NotificationKind::Reescalated]);
        assert!(
            out.notifications[0].warrants_escalation(),
            "Shrieking escapes the machine"
        );
        assert_eq!(out.episodes[0].peak.level, Level::Shrieking);
        assert_eq!(out.episodes[0].peak.at, at(0));
    }

    /// Drifting DEEPER into red at the same level is the same news: the peak
    /// severity is recorded, silently. Mutation-proof: notify on any peak change.
    #[test]
    fn a_deeper_red_at_the_same_level_updates_the_peak_silently() {
        let open = vec![firing(Dimension::Swap, -600, Some(-60), Level::Wailing)];
        let mut r = reading(Dimension::Swap, Band::Red, 600);
        r.severity = 9.0;
        let p = pressure(Level::Wailing, vec![r]);
        let out = reconcile_episodes(&open, &p, None, at(0), &cfg());
        assert!(out.notifications.is_empty(), "already red, already said");
        assert_eq!(out.episodes.len(), 1, "but the peak is recorded");
        assert!((out.episodes[0].peak.severity - 9.0).abs() < 1e-9);
    }

    /// `censusAtPeak` and the peak move TOGETHER and only together.
    /// The stored summary answers "who was running at the WORST moment", so it is
    /// captured when the peak is set, moves when the peak moves, and is frozen when
    /// the peak holds — never overwritten by the newest tick. Three assertions,
    /// three mutations caught: `census.map(summarise)` → `None` in `open_if_due`
    /// (the open assertion fails, it stays None); dropping the assignment in
    /// `move_peak` (the deeper-red assertion fails, it stays 8 GB); and setting it
    /// unconditionally on every step (the frozen assertion fails, it reads the
    /// 1 GB of the newest tick instead of the 5 GB behind the peak).
    #[test]
    fn the_peak_census_tracks_the_worst_moment_and_is_frozen_there() {
        // Open deep: Chrome at 8 GB is who was running when this peak was set.
        let mut r0 = reading(Dimension::Swap, Band::Red, 600);
        r0.severity = 9.0;
        let e0 = reconcile_episodes(
            &[],
            &pressure(Level::Wailing, vec![r0]),
            Some(&census_with_chrome(8_000_000_000)),
            at(0),
            &cfg(),
        )
        .episodes;
        assert_eq!(
            e0[0]
                .census_at_peak
                .as_ref()
                .expect("the census behind the peak is captured on open")
                .consumers[0]
                .rss_bytes,
            8_000_000_000
        );

        // A DEEPER red moves the peak, so the census behind it moves too (5 GB).
        let mut r1 = reading(Dimension::Swap, Band::Red, 900);
        r1.severity = 12.0;
        let e1 = reconcile_episodes(
            &e0,
            &pressure(Level::Wailing, vec![r1]),
            Some(&census_with_chrome(5_000_000_000)),
            at(30),
            &cfg(),
        )
        .episodes;
        assert_eq!(
            e1[0].census_at_peak.as_ref().unwrap().consumers[0].rss_bytes,
            5_000_000_000,
            "the census moves WITH the peak"
        );

        // A shallower red past the repeat interval, with a THIRD census (1 GB): the
        // peak does not move, so its census is frozen — not the newest tick's.
        let mut r2 = reading(Dimension::Swap, Band::Red, 3600);
        r2.severity = 3.0;
        let e2 = reconcile_episodes(
            &e1,
            &pressure(Level::Wailing, vec![r2]),
            Some(&census_with_chrome(1_000_000_000)),
            at(100_000),
            &cfg(),
        )
        .episodes;
        assert_eq!(
            e2[0].census_at_peak.as_ref().unwrap().consumers[0].rss_bytes,
            5_000_000_000,
            "frozen at the peak's census, not overwritten by the newest tick"
        );
    }

    // ---- recovering and closing ------------------------------------------

    /// Dropping below red does not end the episode; it starts the down window.
    /// Nothing is said yet — a brief dip is not recovery.
    #[test]
    fn dropping_below_red_moves_a_firing_episode_to_recovering() {
        let open = vec![firing(Dimension::Cpu, -600, Some(-600), Level::Wailing)];
        let p = pressure(
            Level::Stirring,
            vec![reading(Dimension::Cpu, Band::Yellow, 15)],
        );
        let out = reconcile_episodes(&open, &p, None, at(0), &cfg());
        assert_eq!(out.episodes[0].state, EpisodeState::Recovering);
        assert_eq!(out.episodes[0].recovering_since, Some(at(0)));
        assert_eq!(out.episodes[0].ended_at, None, "not over yet");
        assert!(out.notifications.is_empty());
    }

    /// keep_firing_for: below red for less than the down window is still the same
    /// open episode, untouched. Mutation-proof: close on the first green.
    #[test]
    fn a_recovering_episode_inside_the_down_window_is_left_alone() {
        let open = vec![recovering(Dimension::Cpu, -3600, -599, Level::Wailing)];
        let p = pressure(
            Level::Quiet,
            vec![reading(Dimension::Cpu, Band::Green, 599)],
        );
        assert_eq!(
            reconcile_episodes(&open, &p, None, at(0), &cfg()),
            Reconciled::default()
        );
    }

    /// Once the down window elapses the episode CLOSES with exactly one recovery
    /// notice, and `endedAt` is when the dimension dropped below red — not when
    /// we finished waiting. Mutation-proof: set `ended_at = now`; the fixture puts
    /// 600s between the two.
    #[test]
    fn a_recovering_episode_closes_after_the_down_window_with_one_notice() {
        let open = vec![recovering(Dimension::Cpu, -3600, -600, Level::Wailing)];
        let p = pressure(
            Level::Quiet,
            vec![reading(Dimension::Cpu, Band::Green, 600)],
        );
        let out = reconcile_episodes(&open, &p, None, at(0), &cfg());
        let e = &out.episodes[0];
        assert_eq!(e.state, EpisodeState::Closed);
        assert_eq!(e.ended_at, Some(at(-600)));
        assert_eq!(e.id, open[0].id);
        assert_eq!(kinds(&out), vec![NotificationKind::Recovered]);
        assert!(
            out.notifications[0].message.contains("recovered after 50m"),
            "{}",
            out.notifications[0].message
        );
        assert!(
            out.notifications[0].warrants_notification(),
            "Wailing → Quiet is owed its all-clear"
        );
    }

    /// The down window is load-bearing config. Mutation-proof: hardcode 600 in
    /// `step` and the 60s config stops closing at 120s.
    #[test]
    fn the_down_window_is_load_bearing_config() {
        let open = vec![recovering(Dimension::Cpu, -3600, -120, Level::Wailing)];
        let p = pressure(
            Level::Quiet,
            vec![reading(Dimension::Cpu, Band::Green, 120)],
        );
        let brief = PressureConfig {
            episode_down_secs: 60,
            ..cfg()
        };
        assert_eq!(
            reconcile_episodes(&open, &p, None, at(0), &brief).episodes[0].state,
            EpisodeState::Closed
        );
        assert!(
            reconcile_episodes(&open, &p, None, at(0), &cfg())
                .episodes
                .is_empty()
        );
    }

    /// A recovery whose peak never warranted a notification is recorded but not
    /// announced: silence for a non-event. Mutation-proof: route Recovered on the
    /// current level alone and this fires (Quiet is "recovery" from anything).
    #[test]
    fn a_recovery_from_an_unannounced_peak_is_not_announced() {
        let open = vec![recovering(Dimension::Cpu, -3600, -600, Level::Restless)];
        let p = pressure(
            Level::Quiet,
            vec![reading(Dimension::Cpu, Band::Green, 600)],
        );
        let out = reconcile_episodes(&open, &p, None, at(0), &cfg());
        assert_eq!(
            kinds(&out),
            vec![NotificationKind::Recovered],
            "still emitted for the record"
        );
        assert!(
            !out.notifications[0].warrants_notification(),
            "but not worth interrupting for"
        );
    }

    /// Partial recovery is not news: swap recovering while cpu still holds the
    /// machine at Wailing must not announce good news. Mutation-proof: route
    /// Recovered on `peak_level.warrants_notification()` alone.
    #[test]
    fn a_recovery_while_the_machine_is_still_wailing_is_not_announced() {
        let open = vec![recovering(Dimension::Swap, -3600, -600, Level::Shrieking)];
        let p = pressure(
            Level::Wailing,
            vec![
                reading(Dimension::Swap, Band::Green, 600),
                reading(Dimension::Cpu, Band::Red, 30),
            ],
        );
        let out = reconcile_episodes(&open, &p, None, at(0), &cfg());
        let recovered = out
            .notifications
            .iter()
            .find(|n| n.kind == NotificationKind::Recovered)
            .unwrap();
        assert!(!recovered.warrants_notification());
        assert!(!recovered.warrants_escalation());
    }

    /// The case the dogfood run exposed: a dimension recovering while the machine is
    /// WORSE than that episode's peak, because another dimension escalated. That is
    /// not good news either, and the escalation clause of `level_changed_materially`
    /// must not smuggle it through. Mutation-proof: route Recovered through
    /// `level_changed_materially(Some(peak), now)` and both cases below announce.
    #[test]
    fn a_recovery_while_the_machine_has_since_got_worse_is_not_announced() {
        let p = pressure(
            Level::Shrieking,
            vec![
                reading(Dimension::Orphans, Band::Green, 600),
                reading(Dimension::Cpu, Band::Red, 600),
                reading(Dimension::Swap, Band::Red, 600),
            ],
        );
        // An unannounced peak (Restless) — a non-event's recovery is silence anyway…
        let quiet_peak = vec![recovering(Dimension::Orphans, -3600, -600, Level::Restless)];
        let out = reconcile_episodes(&quiet_peak, &p, None, at(0), &cfg());
        let n = out
            .notifications
            .iter()
            .find(|n| n.kind == NotificationKind::Recovered)
            .unwrap();
        assert!(
            !n.warrants_notification(),
            "peak Restless, machine Shrieking: {n:?}"
        );
        // …and an ANNOUNCED peak (Wailing) whose machine is now Shrieking: worse, not over.
        let loud_peak = vec![recovering(Dimension::Orphans, -3600, -600, Level::Wailing)];
        let out = reconcile_episodes(&loud_peak, &p, None, at(0), &cfg());
        let n = out
            .notifications
            .iter()
            .find(|n| n.kind == NotificationKind::Recovered)
            .unwrap();
        assert!(
            !n.warrants_notification(),
            "peak Wailing, machine Shrieking: {n:?}"
        );
        assert!(!n.warrants_escalation());
    }

    /// A Shrieking-peak recovery escalates (Slack hears the all-clear); a
    /// Wailing-peak recovery does not. Mutation-proof: route escalation on the
    /// current level — Quiet never escalates, so both would read false.
    #[test]
    fn a_recovery_escalates_exactly_when_its_peak_did() {
        let p = pressure(
            Level::Quiet,
            vec![reading(Dimension::Swap, Band::Green, 600)],
        );
        let shrieked = vec![recovering(Dimension::Swap, -3600, -600, Level::Shrieking)];
        let wailed = vec![recovering(Dimension::Swap, -3600, -600, Level::Wailing)];
        assert!(
            reconcile_episodes(&shrieked, &p, None, at(0), &cfg()).notifications[0]
                .warrants_escalation()
        );
        assert!(
            !reconcile_episodes(&wailed, &p, None, at(0), &cfg()).notifications[0]
                .warrants_escalation()
        );
    }

    // ---- flapping --------------------------------------------------------

    /// A re-red during Recovering REOPENS the same episode. Inside the repeat
    /// interval that is a swallowed notification, so `suppressed` climbs and
    /// nothing is said. Mutation-proof: open a fresh episode instead — the id
    /// changes; or drop the `suppressed` increment.
    #[test]
    fn a_flap_reopens_the_same_episode_and_counts_the_swallowed_notice() {
        let open = vec![recovering(Dimension::Thrash, -1800, -120, Level::Wailing)];
        let p = red(Dimension::Thrash, 15, Level::Wailing);
        let out = reconcile_episodes(&open, &p, None, at(0), &cfg());
        let e = &out.episodes[0];
        assert_eq!(e.id, open[0].id, "one incident, not two");
        assert_eq!(e.state, EpisodeState::Firing);
        assert_eq!(e.recovering_since, None);
        assert_eq!(e.suppressed, 1);
        assert_eq!(
            e.started_at, open[0].started_at,
            "the incident's start is kept"
        );
        assert!(out.notifications.is_empty(), "swallowed, not spoken");
    }

    /// Forty storms in an hour: one episode, suppressed 39. This is the
    /// `banshee-nvd` case — the record distinguishes one storm from forty.
    #[test]
    fn forty_storms_are_one_episode_with_thirty_nine_suppressed() {
        let c = PressureConfig {
            episode_down_secs: 300,
            ..cfg()
        };
        let mut open = vec![firing(
            Dimension::Thrash,
            -3000,
            Some(-3000),
            Level::Wailing,
        )];
        let mut t = -2999;
        for _ in 0..39 {
            // dips below red for 30s (inside the 300s down window), then back.
            let dip = pressure(
                Level::Quiet,
                vec![reading(Dimension::Thrash, Band::Green, 30)],
            );
            let out = reconcile_episodes(&open, &dip, None, at(t), &c);
            open = out.episodes;
            t += 30;
            let storm = red(Dimension::Thrash, 15, Level::Wailing);
            let out = reconcile_episodes(&open, &storm, None, at(t), &c);
            open = out.episodes;
            t += 30;
        }
        assert_eq!(open.len(), 1);
        assert_eq!(open[0].suppressed, 39);
        assert_eq!(open[0].state, EpisodeState::Firing);
    }

    /// A re-red AFTER the repeat interval is a Repeat notice, not a suppression:
    /// it was spoken. Mutation-proof: increment `suppressed` unconditionally on
    /// reopen.
    #[test]
    fn a_flap_past_the_repeat_interval_speaks_instead_of_counting() {
        let mut e = recovering(Dimension::Thrash, -7200, -120, Level::Wailing);
        e.last_notified_at = Some(at(-3601));
        let p = red(Dimension::Thrash, 15, Level::Wailing);
        let out = reconcile_episodes(&[e], &p, None, at(0), &cfg());
        assert_eq!(kinds(&out), vec![NotificationKind::Repeat]);
        assert_eq!(out.episodes[0].suppressed, 0);
    }

    /// A re-red that also worsens the level re-escalates rather than counting as
    /// suppressed — deterioration is always news.
    #[test]
    fn a_flap_that_escalates_is_spoken() {
        let open = vec![recovering(Dimension::Thrash, -1800, -120, Level::Wailing)];
        let p = red(Dimension::Thrash, 15, Level::Shrieking);
        let out = reconcile_episodes(&open, &p, None, at(0), &cfg());
        assert_eq!(kinds(&out), vec![NotificationKind::Reescalated]);
        assert_eq!(out.episodes[0].suppressed, 0);
    }

    // ---- restarts and unobserved dimensions ------------------------------

    /// An open episode whose dimension has NO reading this tick (census not yet
    /// taken after a restart) is left exactly as it was: "not measured" is not
    /// "below red". Mutation-proof: treat a missing reading as green.
    #[test]
    fn an_unobserved_dimension_leaves_its_episode_untouched() {
        let open = vec![firing(
            Dimension::Orphans,
            -3600,
            Some(-3600),
            Level::Wailing,
        )];
        let p = red(Dimension::Cpu, 30, Level::Restless);
        let out = reconcile_episodes(&open, &p, None, at(0), &cfg());
        assert!(out.episodes.is_empty(), "{out:?}");
    }

    // ---- inhibition ------------------------------------------------------

    /// A firing thrash episode mutes swap-volume's NOTICE, not its episode: the
    /// swap episode is still opened and recorded, but nothing is said for it,
    /// and its repeat clock is NOT consumed. Mutation-proof: return an empty
    /// inhibitor table, or set `last_notified_at` regardless.
    #[test]
    fn thrash_inhibits_the_swap_notice_but_not_the_swap_episode() {
        let p = pressure(
            Level::Shrieking,
            vec![
                reading(Dimension::Swap, Band::Red, 600),
                reading(Dimension::Thrash, Band::Red, 600),
            ],
        );
        let out = reconcile_episodes(&[], &p, None, at(0), &cfg());
        let dims: Vec<Dimension> = out.episodes.iter().map(|e| e.dimension).collect();
        assert_eq!(
            dims,
            vec![Dimension::Swap, Dimension::Thrash],
            "both RECORDED"
        );
        let spoken: Vec<Dimension> = out.notifications.iter().map(|n| n.dimension).collect();
        assert_eq!(spoken, vec![Dimension::Thrash], "one story, one bell");
        let swap = out
            .episodes
            .iter()
            .find(|e| e.dimension == Dimension::Swap)
            .unwrap();
        assert_eq!(swap.last_notified_at, None, "a muted notice did not go out");
    }

    /// The inhibition is DIRECTIONAL and specific: swap firing does not mute
    /// thrash (nothing mutes thrash — it is the actionable signal), and the table
    /// carries exactly one row, `Swap → Thrash`, not its reverse.
    #[test]
    fn inhibition_is_directional() {
        let open = vec![firing(Dimension::Swap, -3600, Some(-3600), Level::Wailing)];
        let p = pressure(
            Level::Shrieking,
            vec![
                reading(Dimension::Swap, Band::Red, 3600),
                reading(Dimension::Thrash, Band::Red, 600),
            ],
        );
        let out = reconcile_episodes(&open, &p, None, at(0), &cfg());
        assert!(
            out.notifications
                .iter()
                .any(|n| n.dimension == Dimension::Thrash && n.kind == NotificationKind::Opened),
            "swap does not mute thrash"
        );
        assert_eq!(inhibitors(Dimension::Thrash), &[] as &[Dimension]);
        assert_eq!(inhibitors(Dimension::Swap), &[Dimension::Thrash]);
    }

    /// Once the inhibitor is merely RECOVERING (no longer red), the muted
    /// dimension speaks on its next evaluation — an hour was not lost to the
    /// mute. Mutation-proof: inhibit on "has an open episode" instead of "is
    /// Firing".
    #[test]
    fn a_recovering_inhibitor_no_longer_mutes() {
        let open = vec![
            recovering(Dimension::Thrash, -3600, -60, Level::Shrieking),
            firing(Dimension::Swap, -3600, None, Level::Shrieking),
        ];
        let p = pressure(
            Level::Wailing,
            vec![
                reading(Dimension::Swap, Band::Red, 3600),
                reading(Dimension::Thrash, Band::Green, 60),
            ],
        );
        let out = reconcile_episodes(&open, &p, None, at(0), &cfg());
        let swap = out
            .notifications
            .iter()
            .find(|n| n.dimension == Dimension::Swap)
            .unwrap();
        assert_eq!(swap.kind, NotificationKind::Repeat);
        assert_eq!(
            out.episodes
                .iter()
                .find(|e| e.dimension == Dimension::Swap)
                .unwrap()
                .last_notified_at,
            Some(at(0))
        );
    }

    // ---- level changes ----------------------------------------------------

    #[test]
    fn recovery_from_a_crisis_is_a_material_change() {
        assert!(level_changed_materially(
            Some(Level::Shrieking),
            Level::Quiet
        ));
        assert!(level_changed_materially(
            Some(Level::Wailing),
            Level::Stirring
        ));
    }

    #[test]
    fn escalation_into_notification_territory_is_material() {
        assert!(level_changed_materially(
            Some(Level::Restless),
            Level::Wailing
        ));
        assert!(level_changed_materially(
            Some(Level::Wailing),
            Level::Shrieking
        ));
        assert!(level_changed_materially(None, Level::Wailing));
    }

    #[test]
    fn movement_below_the_notification_threshold_is_not_material() {
        assert!(!level_changed_materially(
            Some(Level::Quiet),
            Level::Stirring
        ));
        assert!(!level_changed_materially(
            Some(Level::Stirring),
            Level::Restless
        ));
        assert!(!level_changed_materially(
            Some(Level::Restless),
            Level::Quiet
        ));
        assert!(!level_changed_materially(None, Level::Restless));
    }

    #[test]
    fn partial_recovery_within_the_crisis_range_is_not_material() {
        assert!(!level_changed_materially(
            Some(Level::Shrieking),
            Level::Wailing
        ));
        assert!(!level_changed_materially(
            Some(Level::Wailing),
            Level::Restless
        ));
    }

    // ---- activity ---------------------------------------------------------

    /// Activity counts episodes ACTIVE in the window, not episodes that STARTED in
    /// it: the five-hour-old episode still firing was part of the last hour.
    /// Mutation-proof: count by `started_at >= from` and the open one drops out
    /// of `last_hour`.
    #[test]
    fn activity_counts_episodes_active_in_the_window_not_onsets() {
        let mut closed_recent = firing(Dimension::Cpu, -1800, Some(-1800), Level::Wailing);
        closed_recent.state = EpisodeState::Closed;
        closed_recent.ended_at = Some(at(-600));
        let mut closed_old = firing(
            Dimension::Disk,
            -20 * 3600,
            Some(-20 * 3600),
            Level::Wailing,
        );
        closed_old.state = EpisodeState::Closed;
        closed_old.ended_at = Some(at(-19 * 3600));
        let episodes = vec![
            firing(Dimension::Swap, -5 * 3600, Some(-5 * 3600), Level::Wailing),
            closed_recent,
            closed_old,
        ];
        assert_eq!(
            RecentActivity::summarise(&episodes, at(0)),
            RecentActivity {
                open: 1,
                last_hour: 2,
                last_day: 3
            }
        );
    }

    // ---- wire -------------------------------------------------------------

    /// The whole episode round-trips as camelCase JSON with every nullable
    /// present-as-null, and older rows missing the post-v10 fields still decode
    /// (the store skips rows that fail to parse, so a required new field would
    /// silently erase history).
    #[test]
    fn an_episode_round_trips_and_tolerates_missing_defaulted_fields() {
        let e = recovering(Dimension::Swap, -3600, -600, Level::Shrieking);
        let v = serde_json::to_value(&e).unwrap();
        assert_eq!(v["state"], "recovering");
        assert_eq!(v["peak"]["level"], "shrieking");
        assert!(v.get("endedAt").is_some(), "present");
        assert!(v["endedAt"].is_null(), "as null");
        assert!(v.get("started_at").is_none(), "snake_case leaked");
        let back: AlertEpisode = serde_json::from_value(v.clone()).unwrap();
        assert_eq!(back, e);

        let mut trimmed = v;
        trimmed.as_object_mut().unwrap().remove("suppressed");
        trimmed.as_object_mut().unwrap().remove("lastNotifiedAt");
        trimmed.as_object_mut().unwrap().remove("recoveringSince");
        let old: AlertEpisode = serde_json::from_value(trimmed).expect("older rows still parse");
        assert_eq!(old.suppressed, 0);
        assert_eq!(old.last_notified_at, None);
    }

    // ---- the disk projection re-notifies (`banshee-3sn`) -------------------

    /// A disk reading with a projection: `value / -trend` is what
    /// `record_projection` reads, and the detail is what the notice's message
    /// carries.
    fn disk_reading(free_bytes: f64, projection_secs: f64, held_secs: u64) -> DimensionReading {
        let mut r = reading(Dimension::Disk, Band::Red, held_secs);
        r.value = free_bytes;
        r.trend_per_sec = Some(-free_bytes / projection_secs);
        r.detail = format!(
            "{:.1} GB free, full in {:.0} min",
            free_bytes / 1e9,
            projection_secs / 60.0
        );
        r
    }

    /// Pin (d) of `banshee-3sn`, the 2026-09-15 silence: an open episode whose
    /// LEVEL is already at its peak (Shrieking, held there by memory) and whose
    /// repeat clock is nowhere near due must still notify when the projection
    /// collapses past a threshold — that is the "full in 22 min" that happened
    /// behind the memory glyph. The message carries the new projection.
    /// Mutation-proof: remove the projection clause from `pending_notification`'s
    /// kind selection and this crossing is a silent peak update.
    #[test]
    fn a_projection_crossing_renotifies_before_the_repeat_interval() {
        // Notified 2 minutes ago; repeat is 3600s. Level Shrieking == the peak,
        // so neither of the old escalation paths can speak.
        let e = firing(Dimension::Disk, 0, Some(1200), Level::Shrieking);
        let r = disk_reading(12e9, 22.0 * 60.0, 1320);
        let p = pressure(Level::Shrieking, vec![r]);

        let out = reconcile_episodes(&[e], &p, None, at(1320), &cfg());
        assert_eq!(kinds(&out), vec![NotificationKind::Reescalated]);
        let n = &out.notifications[0];
        assert!(
            n.message.contains("full in 22 min"),
            "the notice must carry the new projection: {}",
            n.message
        );
        assert_eq!(
            out.episodes[0].projection_bracket_secs,
            Some(1800),
            "22 min crossed the 30-minute line"
        );
        assert_eq!(
            out.episodes[0].last_notified_at,
            Some(at(1320)),
            "a spoken notice resets the repeat clock"
        );
    }

    /// The bracket is monotonic: wobbling within an announced bracket says
    /// nothing, and only a DEEPER crossing speaks again. Without this, a
    /// projection oscillating around one threshold re-notifies on every tick —
    /// the flapping the episode model exists to absorb.
    #[test]
    fn only_a_deeper_projection_crossing_speaks_again() {
        let mut e = firing(Dimension::Disk, 0, Some(1200), Level::Shrieking);
        e.projection_bracket_secs = Some(1800);

        // 25 minutes: inside the already-announced 30-minute bracket. Silence.
        let p = pressure(Level::Shrieking, vec![disk_reading(12e9, 1500.0, 1320)]);
        let out = reconcile_episodes(&[e.clone()], &p, None, at(1320), &cfg());
        assert_eq!(kinds(&out), Vec::<NotificationKind>::new());

        // 8 minutes: past the 10-minute line. News.
        let p = pressure(Level::Shrieking, vec![disk_reading(6e9, 480.0, 1440)]);
        let out = reconcile_episodes(&[e], &p, None, at(1440), &cfg());
        assert_eq!(kinds(&out), vec![NotificationKind::Reescalated]);
        assert_eq!(out.episodes[0].projection_bracket_secs, Some(600));
    }

    /// An episode that OPENS with a near projection announces once: the bracket
    /// is seeded at open (the Opened notice already carries the projection), so
    /// the same state is not re-announced on the next tick as a "crossing".
    #[test]
    fn the_bracket_is_seeded_at_open_not_reannounced() {
        let r = disk_reading(12e9, 22.0 * 60.0, 150);
        let p = pressure(Level::Wailing, vec![r.clone()]);
        let out = reconcile_episodes(&[], &p, None, at(150), &cfg());
        assert_eq!(kinds(&out), vec![NotificationKind::Opened]);
        let opened = out.episodes[0].clone();
        assert_eq!(opened.projection_bracket_secs, Some(1800), "seeded at open");

        // The next tick, same projection: nothing new to say.
        let p = pressure(Level::Wailing, vec![disk_reading(12e9, 22.0 * 60.0, 165)]);
        let out = reconcile_episodes(&[opened], &p, None, at(165), &cfg());
        assert_eq!(kinds(&out), Vec::<NotificationKind>::new());
    }
}
