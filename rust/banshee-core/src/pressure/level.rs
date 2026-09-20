// pressure/level — reducing ten dimensions to the one thing a person reads.
//
// The reduction IS the product. Raw numbers are almost worthless without it;
// `top` already shows raw numbers and nobody watches `top` all day. It lives in
// core and travels over the wire, glyph included, so the menu bar, the window,
// the CLI and the MCP server cannot come to disagree about whether the machine is
// on fire (ADR-0005).

use serde::{Deserialize, Serialize};

use super::band::Band;
use super::config::{ALL_DIMENSIONS, Dimension, Source};

/// Severity, as a person distinguishes it.
///
/// Five levels plus `Checking`, in banshee vocabulary. Reduced to "the handful of
/// states a person actually distinguishes" — the view never branches on a number.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Level {
    /// Not enough data yet.
    ///
    /// **Distinct from `Quiet` on purpose.** Without it the menu bar flashes
    /// "nothing is wrong" for the first moments after launch, which teaches the
    /// user to distrust the one glyph that must be trusted.
    Checking,
    Quiet,
    Stirring,
    Restless,
    Wailing,
    Shrieking,
}

impl Level {
    /// The menu bar face. One monotonic scale, so severity reads at a glance
    /// without decoding a symbol set.
    ///
    /// **No ghost.** 👻 is already another of the author's menu-bar apps' icon; two apps
    /// sharing a glyph defeats the point of having one.
    pub fn glyph(self) -> &'static str {
        match self {
            Level::Checking => "🫧",
            Level::Quiet => "😴",
            Level::Stirring => "🫥",
            Level::Restless => "😧",
            Level::Wailing => "😱",
            Level::Shrieking => "💀",
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Level::Checking => "Checking",
            Level::Quiet => "Quiet",
            Level::Stirring => "Stirring",
            Level::Restless => "Restless",
            Level::Wailing => "Wailing",
            Level::Shrieking => "Shrieking",
        }
    }

    /// At Restless and above the menu bar also shows WHICH source is winning.
    /// Below that there is nothing worth naming, and a suffix would be noise.
    pub fn shows_source(self) -> bool {
        self >= Level::Restless
    }

    /// Whether this level should interrupt the user with a notification.
    pub fn warrants_notification(self) -> bool {
        self >= Level::Wailing
    }

    /// Whether this level should escape the machine (the Slack sink), so a
    /// critical alert reaches the user with the app closed.
    pub fn warrants_escalation(self) -> bool {
        self >= Level::Shrieking
    }
}

/// One dimension's contribution, already banded.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Contribution {
    pub dimension: Dimension,
    pub band: Band,
    /// Seconds this dimension has continuously held its band.
    pub held_secs: u64,
    /// How far past the line it is: 0.0 at the yellow threshold, 1.0 at red,
    /// above 1.0 beyond red. Comparable ACROSS dimensions, which is what makes
    /// "which source is winning" answerable at all.
    pub severity: f64,
}

impl Contribution {
    /// A Red that has held past the grace period is a crisis; one that has not is
    /// a spike. A build can turn a machine red for thirty seconds and that is not
    /// something to shriek about.
    pub fn is_sustained_red(&self, grace_secs: u64) -> bool {
        self.band == Band::Red && self.held_secs >= grace_secs
    }
}

/// The reduction: level plus the source driving it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Verdict {
    pub level: Level,
    /// `None` at Quiet, Checking, and Stirring — nothing is worth naming yet.
    pub source: Option<Source>,
}

impl Verdict {
    /// What the menu bar shows: the face, plus the source glyph once there is a
    /// source worth naming.
    pub fn glyph(&self) -> String {
        match self.source.filter(|_| self.level.shows_source()) {
            Some(s) => format!("{}{}", self.level.glyph(), s.glyph()),
            None => self.level.glyph().to_string(),
        }
    }

    /// Spoken by VoiceOver. An emoji-only menu bar is otherwise invisible to it.
    pub fn accessibility_label(&self) -> String {
        match self.source.filter(|_| self.level.shows_source()) {
            Some(s) => format!("Banshee: {}, {}", self.level.name(), s.label()),
            None => format!("Banshee: {}", self.level.name()),
        }
    }
}

/// Reduce contributions to a verdict.
///
/// `have_data` is separate from an empty slice: a machine can legitimately
/// produce zero contributions (every dimension unreadable) and that is Checking,
/// not Quiet.
pub fn decide(
    contributions: &[Contribution],
    grace_secs: u64,
    catastrophic_severity: f64,
    have_data: bool,
) -> Verdict {
    if !have_data {
        return Verdict {
            level: Level::Checking,
            source: None,
        };
    }

    // Advisory dimensions inform findings but never the level. Uptime alone is
    // not a problem, and a monitor that shrieked about a 30-day uptime would be
    // wrong every single time.
    let scoring: Vec<&Contribution> = contributions
        .iter()
        .filter(|c| !c.dimension.is_advisory())
        .collect();

    if scoring.is_empty() {
        return Verdict {
            level: Level::Checking,
            source: None,
        };
    }

    let yellows = scoring.iter().filter(|c| c.band == Band::Yellow).count();
    let reds: Vec<&&Contribution> = scoring.iter().filter(|c| c.band == Band::Red).collect();
    let sustained_reds = reds.iter().filter(|c| c.is_sustained_red(grace_secs));
    let sustained = sustained_reds.clone().count();
    // The worst any one sustained red is past its own line. A spike never reaches
    // here — `is_sustained_red` already required it to hold past grace — so this
    // measures depth of crisis, not a momentary excursion. NEG_INFINITY when there
    // are no sustained reds, which never clears the ceiling.
    let worst_sustained = sustained_reds
        .map(|c| c.severity)
        .fold(f64::NEG_INFINITY, f64::max);

    // A jetsam kill rings the top bell AT ONCE, without the sustain grace the
    // m5y ceiling requires of every other dimension. A kill is a confirmed past
    // event — the kernel already destroyed a process to survive — not a threshold
    // spike that might recover, so `red_grace_secs` (which exists to stop transient
    // spikes escalating) does not apply to it (`banshee-yk8`). Any red Jetsam
    // contribution is Shrieking, even a single one held for a single sample.
    if reds.iter().any(|c| c.dimension == Dimension::Jetsam) {
        return Verdict {
            level: Level::Shrieking,
            source: dominant_source(&scoring),
        };
    }

    let level = if sustained >= 2 {
        // Two dimensions in sustained crisis. The plan also listed
        // "swap ≥ 90% together with load > 3×" as a separate trigger; that is
        // subsumed here, since a thrashing and saturated machine is two
        // sustained reds. Keeping one rule beats keeping two that overlap.
        Level::Shrieking
    } else if sustained == 1 {
        // One sustained red is normally Wailing — the top bell is reserved for two
        // dimensions in crisis. But magnitude has to matter somewhere: a single
        // dimension catastrophically past its line (swap tens of GB deep, thrash
        // orders of magnitude over) IS a crisis by itself, and before `banshee-m5y`
        // it read identically to one a hair past red. Escalate only when it is both
        // sustained AND catastrophic, so a spike cannot ring it (`banshee-s6s`).
        if worst_sustained >= catastrophic_severity {
            Level::Shrieking
        } else {
            Level::Wailing
        }
    } else if !reds.is_empty() || yellows >= 2 {
        // A red inside its grace period, or several dimensions merely uneasy.
        Level::Restless
    } else if yellows == 1 {
        Level::Stirring
    } else {
        Level::Quiet
    };

    Verdict {
        level,
        source: dominant_source(&scoring),
    }
}

/// Which source is winning.
///
/// Worst band first, then furthest past its own threshold, then the declaration
/// order in `ALL_DIMENSIONS`. The final tie-break is what makes this
/// deterministic: two evaluations of identical data must name the same source, or
/// the menu bar suffix flickers between equals.
fn dominant_source(scoring: &[&Contribution]) -> Option<Source> {
    let worst = scoring
        .iter()
        .filter(|c| c.band > Band::Green)
        .max_by(|a, b| {
            a.band
                .cmp(&b.band)
                .then_with(|| {
                    a.severity
                        .partial_cmp(&b.severity)
                        .unwrap_or(std::cmp::Ordering::Equal)
                })
                // Reverse, because `max_by` keeps the LAST maximum and we want the
                // earliest-declared dimension to win a genuine tie.
                .then_with(|| index_of(b.dimension).cmp(&index_of(a.dimension)))
        })?;
    Some(worst.dimension.source())
}

fn index_of(d: Dimension) -> usize {
    ALL_DIMENSIONS
        .iter()
        .position(|x| *x == d)
        .unwrap_or(usize::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    const GRACE: u64 = 120;
    const CATASTROPHIC: f64 = 3.0;

    fn c(dimension: Dimension, band: Band, held_secs: u64, severity: f64) -> Contribution {
        Contribution {
            dimension,
            band,
            held_secs,
            severity,
        }
    }

    fn green(d: Dimension) -> Contribution {
        c(d, Band::Green, 600, -1.0)
    }

    // ---- levels ----------------------------------------------------------

    /// No data is Checking, NOT Quiet. Mutation-proof: return `Quiet` for
    /// `!have_data` and this fails — and the menu bar would claim calm before it
    /// had looked.
    #[test]
    fn no_data_is_checking_not_quiet() {
        let v = decide(&[], GRACE, CATASTROPHIC, false);
        assert_eq!(v.level, Level::Checking);
        assert_eq!(v.source, None);
        assert_ne!(v.level, Level::Quiet);
    }

    /// Data that contains only advisory dimensions is still Checking: nothing
    /// scoreable was read. Mutation-proof: drop the `scoring.is_empty()` guard and
    /// this reports Quiet off a single uptime reading.
    #[test]
    fn only_advisory_data_is_still_checking() {
        let v = decide(
            &[c(Dimension::Uptime, Band::Red, 9999, 3.0)],
            GRACE,
            CATASTROPHIC,
            true,
        );
        assert_eq!(v.level, Level::Checking);
    }

    /// A jetsam kill rings the top bell AT ONCE. A fresh, non-sustained red Jetsam
    /// — held for a single sample, severity a hair past its line — is Shrieking,
    /// where any other lone fresh red is only Restless. A kill is a confirmed past
    /// event, so the sustain grace the catastrophic ceiling requires does not apply (`banshee-yk8`).
    /// Mutation-proof: delete the jetsam branch in `decide` and
    /// this single fresh red falls back to Restless.
    #[test]
    fn a_single_jetsam_kill_shrieks_without_the_sustain_grace() {
        // Held 0s (fresh), severity 0.5 (just past red): not sustained, nowhere
        // near the catastrophic ceiling. Any other dimension in this state is
        // Restless.
        let v = decide(
            &[c(Dimension::Jetsam, Band::Red, 0, 0.5)],
            GRACE,
            CATASTROPHIC,
            true,
        );
        assert_eq!(
            v.level,
            Level::Shrieking,
            "a jetsam kill is the top bell at once"
        );
        assert_eq!(v.source, Some(Source::Memory));

        // The SAME fresh, low-severity red on a non-jetsam dimension is only
        // Restless — so it is the jetsam identity, not the band alone, that
        // escalates. (The two co-vary in every fixture that does not contrast them.)
        let other = decide(
            &[c(Dimension::Swap, Band::Red, 0, 0.5)],
            GRACE,
            CATASTROPHIC,
            true,
        );
        assert_eq!(other.level, Level::Restless);
    }

    /// A jetsam reading that is NOT red must not escalate — the branch keys on the
    /// red band, not merely on the dimension being present. A green jetsam (no
    /// kills this window) beside one yellow is Stirring, exactly as without it.
    #[test]
    fn a_green_jetsam_does_not_ring_the_bell() {
        let v = decide(
            &[
                c(Dimension::Jetsam, Band::Green, 600, -1.0),
                c(Dimension::Swap, Band::Yellow, 600, 0.5),
            ],
            GRACE,
            CATASTROPHIC,
            true,
        );
        assert_eq!(v.level, Level::Stirring);
    }

    #[test]
    fn all_green_is_quiet_with_no_source() {
        let v = decide(
            &[green(Dimension::Cpu), green(Dimension::Disk)],
            GRACE,
            CATASTROPHIC,
            true,
        );
        assert_eq!(v.level, Level::Quiet);
        assert_eq!(v.glyph(), "😴");
    }

    #[test]
    fn one_yellow_is_stirring() {
        let v = decide(
            &[
                c(Dimension::Cpu, Band::Yellow, 60, 0.3),
                green(Dimension::Disk),
            ],
            GRACE,
            CATASTROPHIC,
            true,
        );
        assert_eq!(v.level, Level::Stirring);
        assert_eq!(v.glyph(), "🫥", "no source suffix below Restless");
    }

    /// Two uneasy dimensions is worse than one, even with no red anywhere.
    /// Mutation-proof: require `yellows >= 3` and this reports Stirring.
    #[test]
    fn two_yellows_is_restless() {
        let v = decide(
            &[
                c(Dimension::Cpu, Band::Yellow, 60, 0.3),
                c(Dimension::Swap, Band::Yellow, 60, 0.2),
            ],
            GRACE,
            CATASTROPHIC,
            true,
        );
        assert_eq!(v.level, Level::Restless);
    }

    /// A red that has NOT held past grace is a spike, not a crisis. A release
    /// build turns this machine red for seconds at a time. Mutation-proof: treat
    /// any red as sustained and this reports Wailing.
    #[test]
    fn a_fresh_red_is_restless_not_wailing() {
        let v = decide(
            &[c(Dimension::Cpu, Band::Red, 30, 1.4)],
            GRACE,
            CATASTROPHIC,
            true,
        );
        assert_eq!(v.level, Level::Restless, "30s < 120s grace");
        assert_eq!(v.glyph(), "😧🔥", "but the source is named");
    }

    /// Once it holds past grace, it is a crisis.
    #[test]
    fn a_sustained_red_is_wailing() {
        let v = decide(
            &[c(Dimension::Cpu, Band::Red, 180, 1.4)],
            GRACE,
            CATASTROPHIC,
            true,
        );
        assert_eq!(v.level, Level::Wailing);
        assert_eq!(v.glyph(), "😱🔥");
    }

    /// The grace boundary is inclusive, matching the band thresholds.
    #[test]
    fn the_grace_boundary_is_inclusive() {
        assert_eq!(
            decide(
                &[c(Dimension::Cpu, Band::Red, GRACE, 1.4)],
                GRACE,
                CATASTROPHIC,
                true
            )
            .level,
            Level::Wailing
        );
        assert_eq!(
            decide(
                &[c(Dimension::Cpu, Band::Red, GRACE - 1, 1.4)],
                GRACE,
                CATASTROPHIC,
                true
            )
            .level,
            Level::Restless
        );
    }

    /// Two sustained reds is the top of the scale. Mutation-proof: require 3 and
    /// the 2026-08-04 shape (saturated AND thrashing) reports Wailing.
    #[test]
    fn two_sustained_reds_is_shrieking() {
        let v = decide(
            &[
                c(Dimension::Cpu, Band::Red, 600, 2.2),
                c(Dimension::Thrash, Band::Red, 600, 9.0),
            ],
            GRACE,
            CATASTROPHIC,
            true,
        );
        assert_eq!(v.level, Level::Shrieking);
        assert_eq!(v.glyph(), "💀🧠", "thrash is further past its line");
    }

    /// Two reds where only ONE is sustained is Wailing, not Shrieking — the
    /// second is still a spike. Mutation-proof: count raw reds instead of
    /// sustained ones and this reports Shrieking.
    #[test]
    fn a_sustained_red_plus_a_fresh_red_is_wailing() {
        let v = decide(
            &[
                c(Dimension::Cpu, Band::Red, 600, 2.2),
                c(Dimension::Disk, Band::Red, 15, 1.1),
            ],
            GRACE,
            CATASTROPHIC,
            true,
        );
        assert_eq!(v.level, Level::Wailing);
    }

    /// One dimension catastrophically past its line, sustained, rings the top bell
    /// on its own — before the catastrophic ceiling (`banshee-m5y`) this was Wailing, indistinguishable from a
    /// dimension a hair past red. Swap 28 GB+ is severity ≥ 3.0. Mutation-proof:
    /// replace the escalation branch with a bare `Level::Wailing` and this fails.
    #[test]
    fn a_single_catastrophic_sustained_red_shrieks() {
        let v = decide(
            &[c(Dimension::Swap, Band::Red, 3600, 3.5)],
            GRACE,
            CATASTROPHIC,
            true,
        );
        assert_eq!(v.level, Level::Shrieking);
        assert_eq!(v.glyph(), "💀🧠");
    }

    /// The ceiling is inclusive: severity EXACTLY at it shrieks, a hair below it
    /// wails. This is the boundary the co-varying-fixture trap would blur — the two
    /// levels genuinely come apart only across this line. Mutation-proof: change
    /// `>=` to `>` and the exactly-at case drops to Wailing.
    #[test]
    fn the_catastrophic_ceiling_is_inclusive() {
        assert_eq!(
            decide(
                &[c(Dimension::Swap, Band::Red, 3600, CATASTROPHIC)],
                GRACE,
                CATASTROPHIC,
                true,
            )
            .level,
            Level::Shrieking,
        );
        assert_eq!(
            decide(
                &[c(Dimension::Swap, Band::Red, 3600, CATASTROPHIC - 0.01)],
                GRACE,
                CATASTROPHIC,
                true,
            )
            .level,
            Level::Wailing,
        );
    }

    /// A catastrophic reading that has NOT held past grace is a spike, and a spike
    /// must never ring the top bell — that is the false-Shrieking shape (`banshee-s6s`)
    /// where a gap made one fresh reading look sustained. Severity
    /// 9.0 but only 30s held: Restless, not Wailing and not Shrieking.
    /// Mutation-proof: drop the sustain requirement (gate the escalation on raw
    /// severity instead of a sustained red) and this shrieks off a single spike.
    #[test]
    fn a_catastrophic_spike_that_has_not_held_does_not_shriek() {
        let v = decide(
            &[c(Dimension::Swap, Band::Red, 30, 9.0)],
            GRACE,
            CATASTROPHIC,
            true,
        );
        assert_eq!(v.level, Level::Restless, "30s < 120s grace: still a spike");
    }

    /// The 2026-08-04 incident, reconstructed: load 9.5/core, swap 35.6 GB,
    /// stale sessions, orphaned helpers — all sustained.
    #[test]
    fn the_august_incident_reads_as_shrieking() {
        let v = decide(
            &[
                c(Dimension::Cpu, Band::Red, 3600, 3.2),
                c(Dimension::Swap, Band::Red, 3600, 3.9),
                c(Dimension::Agents, Band::Red, 3600, 2.7),
                c(Dimension::Orphans, Band::Red, 3600, 1.2),
            ],
            GRACE,
            CATASTROPHIC,
            true,
        );
        assert_eq!(v.level, Level::Shrieking);
        assert_eq!(v.source, Some(Source::Memory), "swap is furthest past");
    }

    // ---- dominant source ------------------------------------------------

    /// Band beats severity, and the case where that MATTERS is a hysteresis-held
    /// band.
    ///
    /// Severity and band normally co-vary by construction: severity is 1.0 at the
    /// red line, so `severity >= 1.0` means Red. The first version of this test
    /// gave the Red dimension the higher severity too, so comparing severity
    /// first produced the same answer and the mutation passed — a FALSE PIN, the
    /// fourth of its shape in this project.
    ///
    /// They come apart exactly when hysteresis holds a band the raw value has
    /// already left: a dimension recovering from Red (severity 0.80, still banded
    /// Red because it has not cleared the margin) must still outrank one climbing
    /// through Yellow (severity 0.90). The band encodes a judgement about
    /// PERSISTENCE that the instantaneous number does not.
    ///
    /// Mutation-proof, verified: compare severity before band and this names Cpu.
    #[test]
    fn a_worse_band_wins_over_a_higher_severity() {
        let v = decide(
            &[
                // Climbing, but only Yellow — and the higher raw severity.
                c(Dimension::Cpu, Band::Yellow, 600, 0.90),
                // Recovering, still held Red by the margin — lower raw severity.
                c(Dimension::Disk, Band::Red, 600, 0.80),
            ],
            GRACE,
            CATASTROPHIC,
            true,
        );
        assert_eq!(
            v.source,
            Some(Source::Disk),
            "the held Red must outrank the higher-severity Yellow"
        );
    }

    /// And the ordinary case still holds: among two Reds, severity decides.
    #[test]
    fn among_equal_bands_the_higher_severity_still_wins() {
        let v = decide(
            &[
                c(Dimension::Cpu, Band::Red, 600, 1.10),
                c(Dimension::Disk, Band::Red, 600, 4.00),
            ],
            GRACE,
            CATASTROPHIC,
            true,
        );
        assert_eq!(v.source, Some(Source::Disk));
    }

    /// Within a band, furthest past its own threshold wins. Mutation-proof:
    /// remove the severity comparison and declaration order decides instead, so
    /// Cpu wins.
    #[test]
    fn within_a_band_the_worst_severity_wins() {
        let v = decide(
            &[
                c(Dimension::Cpu, Band::Red, 600, 1.1),
                c(Dimension::Orphans, Band::Red, 600, 4.0),
            ],
            GRACE,
            CATASTROPHIC,
            true,
        );
        assert_eq!(v.source, Some(Source::Sprawl));
    }

    /// A genuine tie must resolve the SAME way every time, or the menu bar suffix
    /// flickers between equals. Mutation-proof: drop the final tie-break and the
    /// result depends on iteration order.
    #[test]
    fn an_exact_tie_is_broken_deterministically_by_declaration_order() {
        let a = c(Dimension::Cpu, Band::Red, 600, 2.0);
        let b = c(Dimension::Disk, Band::Red, 600, 2.0);

        // Same inputs, both orders — the answer must not move.
        let one = decide(&[a, b], GRACE, CATASTROPHIC, true);
        let two = decide(&[b, a], GRACE, CATASTROPHIC, true);
        assert_eq!(one.source, two.source);
        assert_eq!(
            one.source,
            Some(Source::Cpu),
            "Cpu is declared before Disk in ALL_DIMENSIONS"
        );
    }

    /// Green dimensions never become the dominant source, however many there are.
    #[test]
    fn a_green_machine_has_no_dominant_source() {
        let v = decide(
            &[
                green(Dimension::Cpu),
                green(Dimension::Swap),
                green(Dimension::Disk),
            ],
            GRACE,
            CATASTROPHIC,
            true,
        );
        assert_eq!(v.source, None);
        assert_eq!(v.glyph(), "😴");
    }

    /// Advisory dimensions cannot be the dominant source either — otherwise a
    /// long uptime would put 🧠 in the menu bar with nothing wrong.
    /// Mutation-proof: include advisory dimensions in `scoring` and this fails.
    #[test]
    fn an_advisory_dimension_is_never_the_dominant_source() {
        let v = decide(
            &[
                c(Dimension::Uptime, Band::Red, 99999, 9.0),
                c(Dimension::Cpu, Band::Yellow, 600, 0.2),
            ],
            GRACE,
            CATASTROPHIC,
            true,
        );
        assert_eq!(v.level, Level::Stirring, "uptime must not raise the level");
        assert_eq!(v.glyph(), "🫥", "and must not put a source in the menu bar");
    }

    // ---- glyphs and labels ----------------------------------------------

    /// The five levels are a single monotonic face scale with no repeats, and no
    /// ghost — 👻 belongs to another of the author's menu-bar apps.
    #[test]
    fn every_level_has_a_distinct_non_ghost_glyph() {
        let levels = [
            Level::Checking,
            Level::Quiet,
            Level::Stirring,
            Level::Restless,
            Level::Wailing,
            Level::Shrieking,
        ];
        let mut glyphs: Vec<&str> = levels.iter().map(|l| l.glyph()).collect();
        let n = glyphs.len();
        glyphs.sort_unstable();
        glyphs.dedup();
        assert_eq!(glyphs.len(), n, "level glyphs must be distinct");
        for l in levels {
            assert_ne!(l.glyph(), "👻", "the ghost belongs to another app");
            assert!(!l.name().is_empty());
        }
    }

    #[test]
    fn levels_order_from_checking_up_to_shrieking() {
        assert!(Level::Shrieking > Level::Wailing);
        assert!(Level::Wailing > Level::Restless);
        assert!(Level::Restless > Level::Stirring);
        assert!(Level::Stirring > Level::Quiet);
        assert!(Level::Quiet > Level::Checking);
    }

    /// The source suffix appears at Restless and above, and nowhere below.
    /// Mutation-proof: change the threshold to `>= Level::Stirring` and the
    /// Stirring case gains a suffix it should not have.
    #[test]
    fn the_source_suffix_appears_only_from_restless_upward() {
        assert!(!Level::Checking.shows_source());
        assert!(!Level::Quiet.shows_source());
        assert!(!Level::Stirring.shows_source());
        assert!(Level::Restless.shows_source());
        assert!(Level::Wailing.shows_source());
        assert!(Level::Shrieking.shows_source());
    }

    /// VoiceOver must get words, because an emoji-only menu bar is invisible to
    /// it. Mutation-proof: return the glyph from `accessibility_label` and this
    /// fails on the alphabetic assertion.
    #[test]
    fn the_accessibility_label_is_words_not_glyphs() {
        let v = Verdict {
            level: Level::Wailing,
            source: Some(Source::Memory),
        };
        let label = v.accessibility_label();
        assert_eq!(label, "Banshee: Wailing, memory");
        assert!(
            label.chars().any(|ch| ch.is_alphabetic()),
            "must contain words"
        );
        assert!(!label.contains('😱'), "must not read out an emoji");
        assert!(!label.contains('🧠'));

        let quiet = Verdict {
            level: Level::Quiet,
            source: None,
        };
        assert_eq!(quiet.accessibility_label(), "Banshee: Quiet");
    }

    /// A source present in the data but below the display threshold must not leak
    /// into the glyph.
    #[test]
    fn a_source_is_not_shown_below_the_threshold_even_when_known() {
        let v = Verdict {
            level: Level::Stirring,
            source: Some(Source::ManagedAgents),
        };
        assert_eq!(v.glyph(), "🫥");
        assert_eq!(v.accessibility_label(), "Banshee: Stirring");
    }

    // ---- notification routing -------------------------------------------

    /// Banners at Wailing and above; Slack only at Shrieking. A monitor that
    /// interrupts on Stirring is a monitor that gets muted.
    #[test]
    fn notifications_start_at_wailing_and_escalation_at_shrieking() {
        assert!(!Level::Restless.warrants_notification());
        assert!(Level::Wailing.warrants_notification());
        assert!(Level::Shrieking.warrants_notification());

        assert!(!Level::Wailing.warrants_escalation());
        assert!(Level::Shrieking.warrants_escalation());
    }

    #[test]
    fn sustained_red_requires_both_the_band_and_the_duration() {
        let fresh = c(Dimension::Cpu, Band::Red, 10, 1.0);
        let old = c(Dimension::Cpu, Band::Red, 500, 1.0);
        let yellow_old = c(Dimension::Cpu, Band::Yellow, 500, 0.5);
        assert!(!fresh.is_sustained_red(GRACE));
        assert!(old.is_sustained_red(GRACE));
        assert!(
            !yellow_old.is_sustained_red(GRACE),
            "yellow is never sustained red"
        );
    }
}
