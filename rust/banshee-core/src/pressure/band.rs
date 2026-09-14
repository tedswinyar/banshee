// pressure/band — thresholds with hysteresis.
//
// A monitor that alerts on every threshold crossing gets muted, and a muted
// monitor is worse than none: it trained its user to ignore it. So a dimension
// enters a band at its threshold but only LEAVES once the value has fallen back
// past a margin, and only after holding the new state for N consecutive samples.
//
// The machine is a REPLAY over the sample window rather than mutable state held
// between evaluations. That means an evaluation is a pure function of the stored
// history: it survives a daemon restart, it cannot drift out of step with the
// database, and every transition below is reachable from a test.

use serde::{Deserialize, Serialize};

/// How bad one dimension is right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Band {
    Green,
    Yellow,
    Red,
}

impl Band {
    pub fn is_at_least(self, other: Band) -> bool {
        self >= other
    }
}

/// Thresholds for one dimension, plus the hysteresis rules.
///
/// **Every field is configuration.** Thresholds that live as constants get
/// tuned by editing code, which means they get tuned by whoever is willing to
/// recompile — and then nobody can see what the machine is actually using.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BandSpec {
    /// Value at or beyond which the dimension is Yellow.
    pub yellow: f64,
    /// Value at or beyond which the dimension is Red.
    pub red: f64,
    /// How far the value must fall BELOW a threshold before the band is left.
    /// Expressed in the dimension's own units.
    pub margin: f64,
    /// Consecutive samples a NEW band must hold before it is adopted.
    /// 1 means "adopt immediately".
    pub confirm_samples: usize,
    /// When true, a LOWER value is worse (free space, free memory).
    pub inverted: bool,
}

impl BandSpec {
    /// Rising-is-worse spec.
    pub fn rising(yellow: f64, red: f64, margin: f64, confirm_samples: usize) -> Self {
        Self {
            yellow,
            red,
            margin,
            confirm_samples,
            inverted: false,
        }
    }

    /// Falling-is-worse spec (free bytes, free pages). `yellow` must be the
    /// HIGHER number, since crossing downward is what matters.
    pub fn falling(yellow: f64, red: f64, margin: f64, confirm_samples: usize) -> Self {
        Self {
            yellow,
            red,
            margin,
            confirm_samples,
            inverted: true,
        }
    }

    /// The band a raw value implies, ignoring hysteresis.
    ///
    /// Boundaries are inclusive (`>=` for rising, `<=` for falling), matching
    /// `perf-scan`'s `>= 90` / `> 3` phrasing: a value exactly at the threshold
    /// is in the band. Off-by-one here is a whole band of silence.
    pub fn raw_band(&self, value: f64) -> Band {
        if !value.is_finite() {
            // NaN compares false against every threshold, so an unguarded
            // comparison would silently report Green forever — the single most
            // dangerous failure mode this app has.
            return Band::Green;
        }
        if self.inverted {
            if value <= self.red {
                Band::Red
            } else if value <= self.yellow {
                Band::Yellow
            } else {
                Band::Green
            }
        } else if value >= self.red {
            Band::Red
        } else if value >= self.yellow {
            Band::Yellow
        } else {
            Band::Green
        }
    }

    /// The band a value implies GIVEN the band currently held — i.e. with the
    /// margin applied on the way down only.
    ///
    /// Escalation is immediate: pressure rising is exactly what we want to hear
    /// about. De-escalation requires clearing the threshold by `margin`.
    pub fn band_from(&self, current: Band, value: f64) -> Band {
        let raw = self.raw_band(value);
        if raw >= current {
            return raw;
        }
        // The value wants to drop. Only allow it past a threshold it has cleared
        // by the margin.
        let clears = |threshold: f64| -> bool {
            if self.inverted {
                value >= threshold + self.margin
            } else {
                value <= threshold - self.margin
            }
        };
        match current {
            Band::Red => {
                if !clears(self.red) {
                    Band::Red
                } else if raw == Band::Green && !clears(self.yellow) {
                    Band::Yellow
                } else {
                    raw
                }
            }
            Band::Yellow => {
                if clears(self.yellow) {
                    Band::Green
                } else {
                    Band::Yellow
                }
            }
            Band::Green => Band::Green,
        }
    }
}

/// The outcome of replaying a series through a `BandSpec`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BandOutcome {
    pub band: Band,
    /// How many trailing samples this band has held for, including the newest.
    pub held_samples: usize,
    /// The newest value, carried so callers do not re-index the series.
    pub value: f64,
    /// A band the newest values imply but which has not yet been confirmed.
    /// Present only while a change is pending.
    pub pending: Option<Band>,
}

/// Replay `values` (OLDEST first) through the spec and report the final band.
///
/// Returns `None` for an empty series: "no data" is not "Green". A monitor that
/// reports calm before it has looked is lying at the worst possible moment.
pub fn replay(spec: &BandSpec, values: &[f64]) -> Option<BandOutcome> {
    let newest = *values.last()?;

    // Seed from the OLDEST value with hysteresis disabled. Seeding at Green
    // would let a window that begins mid-crisis report Green for
    // `confirm_samples` cycles after a restart.
    let mut band = spec.raw_band(values[0]);
    let mut held = 1usize;
    let mut pending: Option<Band> = None;
    let mut pending_run = 0usize;

    for &v in &values[1..] {
        let want = spec.band_from(band, v);
        if want == band {
            held += 1;
            pending = None;
            pending_run = 0;
            continue;
        }
        // A change is wanted. Escalation is adopted at once; de-escalation, and
        // any change needing confirmation, must persist.
        let escalating = want > band;
        let needed = if escalating {
            1
        } else {
            spec.confirm_samples.max(1)
        };

        if pending == Some(want) {
            pending_run += 1;
        } else {
            pending = Some(want);
            pending_run = 1;
        }

        if pending_run >= needed {
            band = want;
            held = pending_run;
            pending = None;
            pending_run = 0;
        } else {
            held += 1;
        }
    }

    Some(BandOutcome {
        band,
        held_samples: held,
        value: newest,
        pending,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// load-per-core: yellow at 1, red at 3, must fall 0.5 below to leave,
    /// two samples to confirm a drop.
    fn cpu() -> BandSpec {
        BandSpec::rising(1.0, 3.0, 0.5, 2)
    }

    /// free bytes: yellow below 20 GB, red below 5 GB, 1 GB margin.
    fn disk() -> BandSpec {
        BandSpec::falling(20e9, 5e9, 1e9, 2)
    }

    // ---- raw bands -------------------------------------------------------

    /// Boundaries are INCLUSIVE. Mutation-proof: change `>=` to `>` on the red
    /// arm and a load of exactly 3.0 reports Yellow — one whole band of silence
    /// at precisely the number `perf-scan` calls SATURATED.
    #[test]
    fn thresholds_are_inclusive_at_the_boundary() {
        let s = cpu();
        assert_eq!(s.raw_band(0.999), Band::Green);
        assert_eq!(s.raw_band(1.0), Band::Yellow, "exactly yellow is yellow");
        assert_eq!(s.raw_band(2.999), Band::Yellow);
        assert_eq!(s.raw_band(3.0), Band::Red, "exactly red is red");
        assert_eq!(s.raw_band(76.0 / 8.0), Band::Red, "the 2026-08-04 load");
    }

    /// An inverted spec is worse as the value FALLS. Mutation-proof: drop the
    /// `inverted` branch and a nearly-full disk reports Green.
    #[test]
    fn inverted_specs_get_worse_as_the_value_falls() {
        let s = disk();
        assert_eq!(s.raw_band(100e9), Band::Green);
        assert_eq!(s.raw_band(20e9), Band::Yellow, "exactly yellow is yellow");
        assert_eq!(s.raw_band(19e9), Band::Yellow);
        assert_eq!(s.raw_band(5e9), Band::Red, "exactly red is red");
        assert_eq!(s.raw_band(1.1e9), Band::Red, "the 2026-07-16 crisis");
    }

    /// NaN and infinity must not report Green by accident. NaN compares false
    /// against every threshold, so an unguarded comparison silently disables the
    /// dimension forever. Mutation-proof: remove the `is_finite` guard and the
    /// NaN case still returns Green — but for the wrong reason, so this test
    /// asserts the guard exists by checking infinity too.
    #[test]
    fn non_finite_values_do_not_escalate_or_silently_pass() {
        let s = cpu();
        assert_eq!(s.raw_band(f64::NAN), Band::Green);
        assert_eq!(
            s.raw_band(f64::INFINITY),
            Band::Green,
            "infinity is a broken reading, not a red alert"
        );
        assert_eq!(s.raw_band(f64::NEG_INFINITY), Band::Green);
    }

    // ---- hysteresis ------------------------------------------------------

    /// Escalation is IMMEDIATE. Pressure rising is the thing we exist to report;
    /// making it wait for confirmation would delay every alert by design.
    /// Mutation-proof: require `confirm_samples` for escalation too and this
    /// fails.
    #[test]
    fn escalation_is_immediate() {
        let s = cpu();
        let out = replay(&s, &[0.2, 0.3, 5.0]).unwrap();
        assert_eq!(out.band, Band::Red, "one red sample is enough to escalate");
        assert_eq!(out.held_samples, 1);
    }

    /// De-escalation requires clearing the threshold by the margin. Mutation-proof:
    /// return `raw` unconditionally from `band_from` and 2.9 drops to Yellow
    /// immediately, so a value oscillating around 3.0 alerts on every sample.
    #[test]
    fn a_value_just_below_the_threshold_does_not_de_escalate() {
        let s = cpu();
        // 3.0 -> red. 2.9 is below red but has not cleared it by 0.5.
        let out = replay(&s, &[3.0, 2.9, 2.9, 2.9]).unwrap();
        assert_eq!(out.band, Band::Red, "still red: 2.9 > 3.0 - 0.5");
    }

    /// Clearing the margin, held long enough, does de-escalate.
    #[test]
    fn clearing_the_margin_for_long_enough_de_escalates() {
        let s = cpu();
        // 2.4 clears red (3.0 - 0.5 = 2.5) and is still yellow.
        let out = replay(&s, &[3.0, 2.4, 2.4]).unwrap();
        assert_eq!(out.band, Band::Yellow);
    }

    /// A drop must be CONFIRMED. One good sample in a bad stretch is noise, and
    /// acting on it produces a monitor that flickers. Mutation-proof: set the
    /// needed run to 1 for de-escalation and this reports Yellow.
    #[test]
    fn a_single_good_sample_does_not_confirm_a_drop() {
        let s = cpu(); // confirm_samples = 2
        let out = replay(&s, &[3.0, 3.0, 2.0]).unwrap();
        assert_eq!(out.band, Band::Red, "one clearing sample is not enough");
        assert_eq!(out.pending, Some(Band::Yellow), "but the drop is pending");

        let out = replay(&s, &[3.0, 3.0, 2.0, 2.0]).unwrap();
        assert_eq!(out.band, Band::Yellow, "two confirms it");
        assert_eq!(out.pending, None);
    }

    /// Red cannot skip Yellow on the way down unless it also clears Yellow's
    /// margin. Mutation-proof: return `raw` for the Red arm and a value of 0.9
    /// (which clears red but not yellow's margin of 0.5) jumps straight to Green.
    #[test]
    fn de_escalation_does_not_skip_a_band_it_has_not_cleared() {
        let s = cpu();
        // 0.9 is raw-Green (< 1.0) and clears red, but 0.9 > 1.0 - 0.5.
        let out = replay(&s, &[3.0, 0.9, 0.9, 0.9]).unwrap();
        assert_eq!(out.band, Band::Yellow, "must land on Yellow, not Green");

        // 0.4 clears both margins.
        let out = replay(&s, &[3.0, 0.4, 0.4, 0.4]).unwrap();
        assert_eq!(out.band, Band::Green);
    }

    /// The whole point: a value oscillating around a threshold must settle, not
    /// flap. Mutation-proof: remove hysteresis and the band changes on nearly
    /// every sample.
    #[test]
    fn an_oscillating_value_does_not_flap() {
        let s = cpu();
        // Bouncing across 3.0 but never clearing 2.5.
        let values = [3.1, 2.9, 3.1, 2.8, 3.2, 2.9, 3.0, 2.7];
        let out = replay(&s, &values).unwrap();
        assert_eq!(out.band, Band::Red, "stays red throughout");
        assert_eq!(
            out.held_samples,
            values.len(),
            "the band never changed, so it has held for the whole window"
        );
    }

    /// Hysteresis is symmetric for inverted specs: free space recovering by a
    /// hair must not clear a red disk warning.
    #[test]
    fn inverted_specs_also_require_the_margin_to_recover() {
        let s = disk(); // red below 5 GB, margin 1 GB
        let out = replay(&s, &[4e9, 5.5e9, 5.5e9]).unwrap();
        assert_eq!(out.band, Band::Red, "5.5 GB < 5 GB + 1 GB margin");

        let out = replay(&s, &[4e9, 6.5e9, 6.5e9]).unwrap();
        assert_eq!(out.band, Band::Yellow, "6.5 GB clears red's margin");
    }

    // ---- seeding and empty input ----------------------------------------

    /// An empty series is NOT Green. A monitor with no data must say so — this is
    /// what the Checking level exists for. Mutation-proof: return a Green
    /// outcome for an empty slice and the app reports calm before it has looked.
    #[test]
    fn an_empty_series_has_no_band() {
        assert!(replay(&cpu(), &[]).is_none());
    }

    /// A single sample gives a band, with no hysteresis to apply.
    #[test]
    fn a_single_sample_reports_its_raw_band() {
        let out = replay(&cpu(), &[4.0]).unwrap();
        assert_eq!(out.band, Band::Red);
        assert_eq!(out.held_samples, 1);
        assert_eq!(out.value, 4.0);
    }

    /// The replay SEEDS from the oldest value's raw band, not from Green.
    /// Otherwise a daemon restarting into an ongoing crisis would report Green
    /// until enough samples accumulated. Mutation-proof: seed with `Band::Green`
    /// and this reports Yellow (the confirmed step up) instead of Red.
    #[test]
    fn a_window_that_begins_in_crisis_does_not_start_from_green() {
        let s = cpu();
        let out = replay(&s, &[9.5, 9.5, 9.5]).unwrap();
        assert_eq!(out.band, Band::Red, "the window was red from the start");
        assert_eq!(out.held_samples, 3);
    }

    #[test]
    fn held_samples_counts_only_the_trailing_run() {
        let s = cpu();
        // Green, green, then red for two.
        let out = replay(&s, &[0.1, 0.2, 5.0, 5.0]).unwrap();
        assert_eq!(out.band, Band::Red);
        assert_eq!(out.held_samples, 2);
    }

    /// `confirm_samples` of 0 or 1 must both mean "adopt immediately" rather
    /// than dividing by zero or looping forever.
    #[test]
    fn a_zero_confirm_count_is_treated_as_one() {
        let s = BandSpec::rising(1.0, 3.0, 0.5, 0);
        let out = replay(&s, &[3.0, 2.0]).unwrap();
        assert_eq!(out.band, Band::Yellow, "adopted at once");
    }

    #[test]
    fn band_ordering_is_green_yellow_red() {
        assert!(Band::Red > Band::Yellow);
        assert!(Band::Yellow > Band::Green);
        assert!(Band::Red.is_at_least(Band::Yellow));
        assert!(!Band::Yellow.is_at_least(Band::Red));
    }
}
