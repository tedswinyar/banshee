// pressure/disk — the disk dimension's TIME axis (`banshee-3sn`).
//
// Bytes free is the wrong urgency scale for a volume that is emptying. Measured
// 2026-09-15: the data volume lost 37 GB in 50 minutes (~12 MB/s — an agent's
// cargo target plus growing swapfiles), and for 40 of those minutes the byte
// band said yellow, the linear severity said "less urgent than everything else",
// and the one notice had already been spent. "Full in 22 min" then happened in
// silence behind the memory glyph, because a byte-linear severity (1.08 at 12 GB
// free) can never outrank a thrash storm (8.36) — and at 0 bytes free everything
// falls over hard: swapfiles live on this same volume, so disk exhaustion also
// caps swap, and then the kernel kills.
//
// So disk gets a second severity axis, TIME, and the worse one wins:
//
//   severity = max( (red_bytes / free)²,  red_projection_secs / secs_to_full )
//
// The bytes term is SQUARED so it climbs the way concern does as free space
// approaches zero: 1.0 at the red line (30 GB), 2.25 at 20 GB, 3.0 —
// catastrophic, Shrieking alone, takes the glyph — at ~17 GB, 6.25 at 12 GB
// (the 2026-09-15 low; would have outranked everything but the thrash peak),
// 36 at 5 GB. The time term is 1.0 at one hour to full, 2.0 at 30 minutes,
// 3.0 at 20 minutes, 6.0 at 10. Because `decide` already promotes a single
// sustained catastrophic red to Shrieking and `dominant_source` ranks by
// severity, disk takes the glyph and the headroom reason by itself when it is
// about to fill — with no new level rules. Both reference values live in
// `PressureConfig` so they can be retuned without touching the curve.
//
// The projection itself is estimated with a Theil–Sen slope (median of pairwise
// slopes) rather than the least-squares fit the other dimensions use, because
// the projection must not FLICKER: observed 19:14Z the same day, the "full in"
// text vanished for one evaluation because a single flat/rebounding window
// flipped the least-squares slope's sign while the volume was still emptying.
// A median of pairwise slopes ignores a minority of artifact intervals the same
// way `Series::max_interval`'s median ignores a minority of outlier gaps.

use chrono::{DateTime, Utc};

use super::band::Band;

/// Change per second across `points`, by Theil–Sen: the MEDIAN of all pairwise
/// slopes. Robust to a one-window artifact (a cache purge bumping free space for
/// a tick), which a least-squares fit weights hardest exactly when it lands at
/// the end of the window. `None` below two distinct-time points.
pub(super) fn slope_per_sec(points: &[(DateTime<Utc>, f64)]) -> Option<f64> {
    let mut slopes = Vec::new();
    for (i, (t0, v0)) in points.iter().enumerate() {
        for (t1, v1) in &points[i + 1..] {
            let dt = (*t1 - *t0).num_milliseconds() as f64 / 1000.0;
            if dt > 0.0 {
                slopes.push((v1 - v0) / dt);
            }
        }
    }
    if slopes.is_empty() {
        return None;
    }
    slopes.sort_unstable_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let n = slopes.len();
    Some(if n % 2 == 1 {
        slopes[n / 2]
    } else {
        (slopes[n / 2 - 1] + slopes[n / 2]) / 2.0
    })
}

/// Seconds until the volume is full at the current slope. `Some` only while the
/// smoothed slope is genuinely falling — the same condition `detail_for` renders
/// "full in …" under, so the number an alert carries and the number severity
/// acts on cannot disagree.
pub(super) fn projected_full_secs(free_bytes: f64, slope: Option<f64>) -> Option<f64> {
    let s = slope.filter(|s| s.abs() > f64::EPSILON && *s < 0.0)?;
    // NaN as well as zero and negative: a broken reading projects nothing.
    if !free_bytes.is_finite() || free_bytes <= 0.0 {
        return None;
    }
    Some(free_bytes / -s)
}

/// The disk severity curve: distance-to-zero and time-to-full, worse one wins.
///
/// Replaces the byte-linear `severity_of` for Disk ONLY; every other dimension
/// keeps the linear normalisation. Deliberately positive even in the green band
/// (0.25 at the 60 GB yellow line) — the green severity is never compared
/// (`dominant_source` filters on band first), and clamping it negative to match
/// the linear contract would put a discontinuity exactly where the curve is
/// supposed to be smooth.
pub(super) fn severity(
    red_bytes: f64,
    red_projection_secs: f64,
    free_bytes: f64,
    projection_secs: Option<f64>,
) -> f64 {
    if !free_bytes.is_finite() {
        // Match `severity_of`'s guard: a broken reading is not a crisis.
        return -1.0;
    }
    // A zero-byte volume is catastrophic, not a division error.
    let bytes_term = (red_bytes / free_bytes.max(1.0)).powi(2);
    let time_term = projection_secs
        .map(|p| red_projection_secs / p.max(1.0))
        .unwrap_or(f64::NEG_INFINITY);
    bytes_term.max(time_term)
}

/// The band floor the PROJECTION forces, regardless of bytes: at or under
/// `yellow_secs` to full is at least Yellow, at or under `red_secs` is Red.
/// This is the "orange when it should be red" case from 2026-09-15 — 40 minutes
/// of "full in ~1 h" spent entirely inside the yellow byte band.
///
/// Returns the floor and how many trailing samples it has held, judged
/// per-sample: sample `i`'s floor uses only `points[..=i]`, what the model knew
/// at that tick. A NEW floor — in EITHER direction — must persist `confirm`
/// consecutive samples before it is adopted. That is stricter than
/// `band::replay`, which escalates immediately; a raw byte reading crossing red
/// is a fact, but a projection is a ratio of two estimates, and the bead's rule
/// is that a one-tick slope must not flip the band (`banshee-3sn`).
pub(super) fn forced_floor(
    points: &[(DateTime<Utc>, f64)],
    red_secs: f64,
    yellow_secs: f64,
    confirm: usize,
) -> (Band, usize) {
    let floor_at = |i: usize| -> Band {
        match slope_per_sec(&points[..=i]).and_then(|s| projected_full_secs(points[i].1, Some(s))) {
            Some(p) if p <= red_secs => Band::Red,
            Some(p) if p <= yellow_secs => Band::Yellow,
            _ => Band::Green,
        }
    };

    if points.is_empty() {
        return (Band::Green, 0);
    }
    let confirm = confirm.max(1);
    // One point has no slope, so the seed is always Green: forcing needs at
    // least `confirm` more samples of evidence, never a single tick.
    let mut band = floor_at(0);
    let mut held = 1usize;
    let mut pending: Option<Band> = None;
    let mut run = 0usize;
    for i in 1..points.len() {
        let want = floor_at(i);
        if want == band {
            held += 1;
            pending = None;
            run = 0;
            continue;
        }
        if pending == Some(want) {
            run += 1;
        } else {
            pending = Some(want);
            run = 1;
        }
        if run >= confirm {
            band = want;
            held = run;
            pending = None;
            run = 0;
        } else {
            held += 1;
        }
    }
    (band, held)
}

#[cfg(test)]
mod tests {
    use super::*;

    const RED_BYTES: f64 = 30e9;
    const RED_SECS: f64 = 3600.0;
    const YELLOW_SECS: f64 = 7200.0;

    fn at(secs: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(1_757_000_000 + secs, 0).unwrap()
    }

    /// A falling series at `per_tick` bytes per 15s tick, newest = `newest`.
    fn falling(n: usize, newest: f64, per_tick: f64) -> Vec<(DateTime<Utc>, f64)> {
        (0..n)
            .map(|i| (at(i as i64 * 15), newest + (n - 1 - i) as f64 * per_tick))
            .collect()
    }

    // ---- the curve itself -------------------------------------------------

    /// Ted's table, pinned: the bytes term is (30 GB / free) SQUARED, so it
    /// climbs the way concern does as free approaches zero. Mutation-proof:
    /// change `.powi(2)` to `.powi(1)` and 12 GB free reads 2.5 instead of 6.25.
    #[test]
    fn the_bytes_term_is_squared_distance_to_zero() {
        let sev = |free: f64| severity(RED_BYTES, RED_SECS, free, None);
        assert!((sev(30e9) - 1.0).abs() < 1e-9, "1.0 at the red line");
        assert!((sev(20e9) - 2.25).abs() < 1e-9);
        assert!((sev(12e9) - 6.25).abs() < 1e-9, "the 2026-09-15 low");
        assert!((sev(5e9) - 36.0).abs() < 1e-9);
        assert!((sev(60e9) - 0.25).abs() < 1e-9, "smooth through yellow");
    }

    /// The time term is 1.0 at one hour to full, and the WORSE axis wins.
    /// Today's data: 12 GB free + 22 min to full must score by time (~2.7),
    /// not by the old linear bytes rule (1.08).
    #[test]
    fn the_time_term_wins_when_the_volume_empties_fast() {
        let sev = severity(RED_BYTES, RED_SECS, 12e9, Some(22.0 * 60.0));
        assert!(
            (sev - 6.25).abs() < 1e-9,
            "12 GB free: bytes (6.25) still beats 22 min (2.73): {sev}"
        );
        // At 40 GB free — yellow by bytes, term 0.5625 — a 45-minute projection
        // is what decides.
        let sev = severity(RED_BYTES, RED_SECS, 40e9, Some(45.0 * 60.0));
        assert!(
            (sev - RED_SECS / 2700.0).abs() < 1e-9,
            "time term must win at 40 GB + 45 min: {sev}"
        );
        assert!(sev >= 1.0, "red on time alone");
    }

    /// Pin (a): equal bytes free, different slopes, must rank DIFFERENTLY —
    /// the steeper slope higher. This is the axis the byte-linear severity was
    /// blind to. Mutation-proof: drop the time term (return bytes_term) and the
    /// two are equal.
    #[test]
    fn equal_bytes_with_a_steeper_slope_ranks_higher() {
        let slow = severity(RED_BYTES, RED_SECS, 40e9, Some(4.0 * 3600.0));
        let fast = severity(RED_BYTES, RED_SECS, 40e9, Some(30.0 * 60.0));
        assert!(
            fast > slow,
            "same bytes, steeper slope must rank higher: {fast} vs {slow}"
        );
    }

    /// Pin (b): equal slopes, different bytes, must also rank differently —
    /// with projections long enough that the BYTES term is the one deciding,
    /// so this pin does not co-vary with pin (a)'s.
    #[test]
    fn equal_slopes_with_less_free_ranks_higher() {
        // Same slope (~0.55 MB/s): projections 6h at 12 GB, 12.6h at 25 GB —
        // both time terms below 0.17, so bytes decide.
        let slope = -12e9 / (6.0 * 3600.0);
        let low = severity(
            RED_BYTES,
            RED_SECS,
            12e9,
            projected_full_secs(12e9, Some(slope)),
        );
        let high = severity(
            RED_BYTES,
            RED_SECS,
            25e9,
            projected_full_secs(25e9, Some(slope)),
        );
        assert!(
            low > high,
            "same slope, fewer bytes must rank higher: {low} vs {high}"
        );
        assert!((low - 6.25).abs() < 1e-9, "and by the bytes term: {low}");
    }

    /// The guards: a broken reading is not a crisis, an empty volume is not a
    /// division error, and no projection means the bytes term alone.
    #[test]
    fn severity_guards_its_edges() {
        assert_eq!(severity(RED_BYTES, RED_SECS, f64::NAN, None), -1.0);
        assert!(severity(RED_BYTES, RED_SECS, 0.0, None).is_finite());
        assert!(severity(RED_BYTES, RED_SECS, 0.0, None) > 100.0);
        assert!(
            severity(RED_BYTES, RED_SECS, 100e9, Some(0.0)).is_finite(),
            "a zero projection must clamp, not divide by zero"
        );
    }

    // ---- the projection ---------------------------------------------------

    /// A projection exists only while the smoothed slope is genuinely falling.
    #[test]
    fn a_projection_needs_a_falling_slope_and_positive_bytes() {
        assert_eq!(projected_full_secs(10e9, None), None);
        assert_eq!(projected_full_secs(10e9, Some(0.0)), None);
        assert_eq!(
            projected_full_secs(10e9, Some(5e6)),
            None,
            "filling, not emptying"
        );
        assert_eq!(projected_full_secs(0.0, Some(-5e6)), None);
        let p = projected_full_secs(12e9, Some(-1e9 / 60.0)).unwrap();
        assert!((p - 720.0).abs() < 1e-6, "12 GB at 1 GB/min: 12 min");
    }

    /// Pin (e): the slope must not FLICKER on a one-window artifact. The fixture
    /// distinguishes the estimators: a steadily emptying volume with one late
    /// 8 GB rebound (a cache purge) flips the least-squares slope positive —
    /// which is exactly the 19:14Z observation, the "full in" text vanishing for
    /// one evaluation — while the median of pairwise slopes stays negative.
    /// Mutation-proof: replace `slope_per_sec`'s body with the least-squares
    /// `Trend::fit` and this fails on the sign assertion.
    #[test]
    fn one_rebound_window_does_not_flip_the_slope() {
        // Falling 250 MB/tick from 12.25 GB, then ONE tick where a purge frees
        // ~5 GB (an agent's temp dir deleted mid-crisis) while the underlying
        // writer keeps writing. The rebound lands at the end of the window,
        // where least squares weights it hardest.
        let mut points = falling(9, 10.25e9, 0.25e9);
        points.push((at(9 * 15), 15.5e9));

        // The fixture genuinely distinguishes: least squares reads the rebound
        // as growth (the co-varying-fixture rule — prove the two estimators
        // come apart on THIS data, or the pin pins nothing).
        let base = points[0].0.timestamp() as f64;
        let ls_pts: Vec<crate::rates::Point> = points
            .iter()
            .map(|(t, v)| crate::rates::Point {
                t_secs: t.timestamp() as f64 - base,
                value: *v,
            })
            .collect();
        let ls = crate::rates::Trend::fit(&ls_pts).unwrap().slope_per_sec;
        assert!(
            ls >= 0.0,
            "fixture must flip the least-squares slope, got {ls}"
        );

        let ts = slope_per_sec(&points).unwrap();
        assert!(ts < 0.0, "the median slope must stay falling, got {ts}");
        assert!(
            projected_full_secs(15.5e9, Some(ts)).is_some(),
            "and the projection must survive the rebound window"
        );
    }

    #[test]
    fn a_flat_series_has_a_zero_slope_and_no_projection() {
        let points: Vec<_> = (0..6).map(|i| (at(i * 15), 100e9)).collect();
        let s = slope_per_sec(&points).unwrap();
        assert_eq!(s, 0.0);
        assert_eq!(projected_full_secs(100e9, Some(s)), None);
    }

    #[test]
    fn fewer_than_two_points_has_no_slope() {
        assert_eq!(slope_per_sec(&[]), None);
        assert_eq!(slope_per_sec(&[(at(0), 5e9)]), None);
    }

    // ---- the forced floor ---------------------------------------------------

    /// Pin (c)'s mechanism: a projection at or under an hour forces Red however
    /// many bytes are free. 40 GB free is mid-yellow; emptying at ~250 MB/tick
    /// it is ~40 minutes from empty. Mutation-proof: delete the Red arm of
    /// `floor_at` and this floors at Yellow.
    #[test]
    fn a_sub_hour_projection_forces_red_whatever_the_bytes_say() {
        let points = falling(21, 40e9, 0.25e9);
        let (band, held) = forced_floor(&points, RED_SECS, YELLOW_SECS, 2);
        assert_eq!(band, Band::Red);
        assert!(held >= 2, "and it has held: {held}");

        // The distinguishing contrast: the same bytes, flat, force nothing.
        let flat: Vec<_> = (0..21).map(|i| (at(i * 15), 40e9)).collect();
        assert_eq!(forced_floor(&flat, RED_SECS, YELLOW_SECS, 2).0, Band::Green);
    }

    /// Between one and two hours to full the floor is Yellow — the 18:08–18:51Z
    /// stretch, 37 GB lost straight through the yellow band with "full in ~1 h"
    /// on screen the whole way.
    #[test]
    fn a_sub_two_hour_projection_forces_at_least_yellow() {
        // 90 GB — green by bytes — emptying at 250 MB/tick: 90 min to full.
        let points = falling(21, 90e9, 0.25e9);
        let (band, _) = forced_floor(&points, RED_SECS, YELLOW_SECS, 2);
        assert_eq!(band, Band::Yellow, "green bytes, yellow time");
    }

    /// The floor needs `confirm` consecutive samples IN BOTH DIRECTIONS — a
    /// one-tick slope cannot flip it on, and a one-tick recovery cannot flip it
    /// off (`banshee-3sn`). Mutation-proof: adopt escalation immediately (needed
    /// = 1 when `want > band`) and the two-point case forces Red off a single
    /// slope estimate.
    #[test]
    fn the_forced_floor_needs_confirmation_both_ways() {
        // Two points, one steep interval: a projection exists, but one tick of
        // evidence must not force anything with confirm = 2.
        let two = falling(2, 10e9, 5e9);
        assert_eq!(forced_floor(&two, RED_SECS, YELLOW_SECS, 2).0, Band::Green);

        // A held Red floor, then a single rebound tick that lifts the newest
        // projection out of range: still Red — the drop is pending, not adopted.
        let mut points = falling(20, 10e9, 0.25e9);
        points.push((at(20 * 15), 200e9));
        let (band, _) = forced_floor(&points, RED_SECS, YELLOW_SECS, 2);
        assert_eq!(band, Band::Red, "one good tick does not lift the floor");
    }
}
