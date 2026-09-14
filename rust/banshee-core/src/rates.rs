// rates — turning readings into the derivatives that justify this app existing.
//
// `perf-scan` can only report LIFETIME swapin/swapout counters, because a single
// invocation is all it has. "Swap is 82% full" is ambiguous; "swap has climbed 18
// points in 40 minutes" is not. Everything in this module is the difference.
//
// Pure functions over values. No kernel, no clock, no database — so every edge
// case below is reachable from a test.

use crate::sys::SysReading;

/// Why a rate could not be computed. **Not an error**: "I cannot honestly tell
/// you a rate yet" is a legitimate answer, and forcing a number here is how
/// monitors come to display nonsense like -4,000 swapins/sec after a reboot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RateGap {
    /// `boot_time_secs` differs between the samples. Every lifetime counter
    /// restarted at zero, so the delta is meaningless.
    Rebooted,
    /// A monotonic counter went BACKWARDS without a reboot. Should not happen;
    /// if it does, the honest response is to discard rather than to report a
    /// negative rate or to wrap around into a huge positive one.
    CounterWentBackwards,
    /// Non-positive elapsed time: samples out of order, or the wall clock
    /// stepped backwards (NTP correction, sleep/wake). Dividing by this yields
    /// infinity or a sign flip.
    NoElapsedTime,
}

/// Per-second rates between two consecutive readings.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CounterRates {
    pub elapsed_secs: f64,
    pub swapins_per_sec: f64,
    pub swapouts_per_sec: f64,
    /// Measured CPU utilization over the interval, a fraction 0.0..=1.0:
    /// `Δ(total − idle) / Δtotal` of the cumulative per-core tick counters. This
    /// is the honest "how busy were the cores" number the CPU dimension bands on.
    /// Load-per-core (`banshee-87l.19`) cannot distinguish saturated cores from
    /// idle ones under a churning run queue. `None` when it cannot be computed:
    /// either sample predates schema v12 (no ticks), or `Δtotal` is zero (no
    /// elapsed CPU time to divide by), or the counters moved backwards without a
    /// reboot. A None here voids only utilization — the swap rates still stand.
    pub cpu_utilization: Option<f64>,
}

/// Compute per-second rates between two readings taken `elapsed_secs` apart.
///
/// Order matters: `prev` must be the earlier sample. Callers that cannot
/// guarantee that should sort first — this function will report
/// `NoElapsedTime` rather than silently returning a negative rate.
pub fn counter_rates(
    prev: &SysReading,
    curr: &SysReading,
    elapsed_secs: f64,
) -> std::result::Result<CounterRates, RateGap> {
    // Reboot check FIRST. After a reboot the counters are legitimately smaller,
    // so checking "went backwards" first would misattribute the cause and the
    // operator would see a spurious data-integrity warning instead of "the
    // machine restarted".
    if prev.boot_time_secs != curr.boot_time_secs {
        return Err(RateGap::Rebooted);
    }
    if !elapsed_secs.is_finite() || elapsed_secs <= 0.0 {
        return Err(RateGap::NoElapsedTime);
    }
    if curr.swapins < prev.swapins || curr.swapouts < prev.swapouts {
        return Err(RateGap::CounterWentBackwards);
    }

    Ok(CounterRates {
        elapsed_secs,
        swapins_per_sec: (curr.swapins - prev.swapins) as f64 / elapsed_secs,
        swapouts_per_sec: (curr.swapouts - prev.swapouts) as f64 / elapsed_secs,
        cpu_utilization: cpu_utilization(prev, curr),
    })
}

/// Utilization over the interval as a fraction 0.0..=1.0, or `None` when it
/// cannot be computed honestly. Kept separate from the swap-rate guards on
/// purpose: an anomaly in the tick counters (a pre-v12 sample, a zero interval,
/// a backwards counter) voids ONLY utilization, leaving the swap rates — which
/// come from independent counters — intact.
fn cpu_utilization(prev: &SysReading, curr: &SysReading) -> Option<f64> {
    let (pt, pi, ct, ci) = (
        prev.cpu_ticks_total?,
        prev.cpu_ticks_idle?,
        curr.cpu_ticks_total?,
        curr.cpu_ticks_idle?,
    );
    // Monotonic counters. A regression without a reboot (already excluded above)
    // should not happen; if it does, refuse rather than report a negative rate.
    if ct < pt || ci < pi {
        return None;
    }
    let d_total = ct - pt;
    let d_idle = ci - pi;
    // No CPU time elapsed → nothing to divide by. And idle can never exceed the
    // total; a reading that says so is corrupt, so refuse it rather than report a
    // negative utilization.
    if d_total == 0 || d_idle > d_total {
        return None;
    }
    Some((d_total - d_idle) as f64 / d_total as f64)
}

/// One point in a time series: seconds since an arbitrary epoch, and a value.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Point {
    pub t_secs: f64,
    pub value: f64,
}

/// A least-squares fit over a window.
///
/// Two-point differencing is far too noisy for "is this trending badly?" — one
/// unlucky pair of samples during a build would trip an alert. A fit over the
/// whole window is the difference between a trend and a coincidence.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Trend {
    /// Change in value per second. Negative means falling.
    pub slope_per_sec: f64,
    /// Value the fit predicts at the LAST point's time. Used as the basis for
    /// projections so a noisy final sample cannot dominate them.
    pub fitted_last: f64,
    pub points: usize,
    pub span_secs: f64,
}

impl Trend {
    /// Ordinary least squares of `value` against `t_secs`.
    ///
    /// Returns `None` when a slope would be meaningless: fewer than two points,
    /// or every point at the same instant (zero variance in `t`, which is a
    /// division by zero, not a vertical line).
    pub fn fit(points: &[Point]) -> Option<Trend> {
        if points.len() < 2 {
            return None;
        }
        let n = points.len() as f64;
        let mean_t = points.iter().map(|p| p.t_secs).sum::<f64>() / n;
        let mean_v = points.iter().map(|p| p.value).sum::<f64>() / n;

        let mut sxx = 0.0;
        let mut sxy = 0.0;
        for p in points {
            let dt = p.t_secs - mean_t;
            sxx += dt * dt;
            sxy += dt * (p.value - mean_v);
        }
        if sxx == 0.0 {
            // Every sample carries the same timestamp. There is no time axis to
            // regress against; a "slope" here would be 0/0.
            return None;
        }

        let slope = sxy / sxx;
        let intercept = mean_v - slope * mean_t;
        let last_t = points.last()?.t_secs;
        let first_t = points.first()?.t_secs;

        Some(Trend {
            slope_per_sec: slope,
            fitted_last: intercept + slope * last_t,
            points: points.len(),
            span_secs: last_t - first_t,
        })
    }

    /// Seconds until the fitted line reaches `target`, or `None` when it never
    /// will (wrong-signed slope, flat line, or already past the target).
    ///
    /// This is what turns "disk is filling" into "disk is full on Thursday".
    pub fn secs_until(&self, target: f64) -> Option<f64> {
        let remaining = target - self.fitted_last;
        if remaining == 0.0 {
            return Some(0.0);
        }
        // The line must be moving TOWARD the target. Same sign means it is.
        if self.slope_per_sec == 0.0 || remaining.signum() != self.slope_per_sec.signum() {
            return None;
        }
        let secs = remaining / self.slope_per_sec;
        if secs.is_finite() && secs >= 0.0 {
            Some(secs)
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> SysReading {
        SysReading {
            ncpu: 8,
            mem_total_bytes: 25_769_803_776,
            page_size: 16384,
            boot_time_secs: 1_788_122_573,
            load_1m: 1.0,
            load_5m: 1.0,
            load_15m: 1.0,
            swap_total_bytes: 8_589_934_592,
            swap_used_bytes: 1_000_000,
            pages_free: 100_000,
            pages_active: 100_000,
            pages_inactive: 100_000,
            pages_wired: 100_000,
            pages_speculative: 0,
            pages_compressor: 0,
            pages_purgeable: 0,
            pages_external: 0,
            swapins: 1_000_000,
            swapouts: 2_000_000,
            memory_pressure_level: 1,
            thermal_pressure_level: Some(0),
            cpu_speed_limit_percent: None,
            cpu_ticks_total: Some(1_000_000),
            cpu_ticks_idle: Some(600_000),
            kernel_free_percent: Some(50),
            jetsam_kills: Some(0),
        }
    }

    #[test]
    fn computes_per_second_rates_over_the_elapsed_interval() {
        let prev = base();
        let mut curr = base();
        curr.swapins += 1500; // over 15s => 100/s
        curr.swapouts += 300; // over 15s => 20/s

        let r = counter_rates(&prev, &curr, 15.0).unwrap();
        assert_eq!(r.swapins_per_sec, 100.0);
        assert_eq!(r.swapouts_per_sec, 20.0);
        assert_eq!(r.elapsed_secs, 15.0);
    }

    /// Utilization is the busy fraction of the tick delta, not of the lifetime
    /// totals. Mutation-proof: divide by `ct` (the cumulative total) instead of
    /// `d_total` and this reads ~0.4 instead of 0.6, because the machine spent
    /// most of its LIFETIME idle even in a busy interval.
    #[test]
    fn computes_cpu_utilization_from_the_tick_delta() {
        let prev = base();
        let mut curr = base();
        curr.cpu_ticks_total = Some(1_100_000); // +100_000 total
        curr.cpu_ticks_idle = Some(640_000); //   + 40_000 idle => 60_000 busy
        let r = counter_rates(&prev, &curr, 15.0).unwrap();
        assert_eq!(r.cpu_utilization, Some(0.6));
    }

    /// The false-positive breaker (`banshee-87l.19`): idle cores read as LOW
    /// utilization regardless of how high the run queue is. The load fields are
    /// pegged here (a 3.26×-per-core run queue on eight cores) while the tick
    /// delta shows the cores were 92% idle — utilization must report ~0.08, the
    /// signal that keeps the CPU dimension green when nothing is actually
    /// computing.
    #[test]
    fn a_high_run_queue_over_idle_cores_reads_as_low_utilization() {
        let mut prev = base();
        let mut curr = base();
        for r in [&mut prev, &mut curr] {
            r.load_1m = 26.0; // 3.26x per core on 8 cores
        }
        curr.cpu_ticks_total = Some(1_100_000); // +100_000 total
        curr.cpu_ticks_idle = Some(692_000); //   + 92_000 idle => 8_000 busy
        let r = counter_rates(&prev, &curr, 15.0).unwrap();
        assert!(
            r.cpu_utilization.is_some_and(|u| (u - 0.08).abs() < 1e-9),
            "got {:?}",
            r.cpu_utilization
        );
    }

    /// A pre-v12 sample has no ticks, so utilization is None — but the swap rates,
    /// from independent counters, still stand. Mutation-proof: void the whole
    /// result on missing ticks and the swap assertions fail.
    #[test]
    fn missing_ticks_void_only_utilization_not_the_swap_rates() {
        let prev = base();
        let mut curr = base();
        curr.swapins += 1500; // 100/s over 15s
        curr.cpu_ticks_total = None; // pre-v12 row
        curr.cpu_ticks_idle = None;
        let r = counter_rates(&prev, &curr, 15.0).unwrap();
        assert_eq!(r.cpu_utilization, None);
        assert_eq!(r.swapins_per_sec, 100.0);
    }

    /// No CPU time elapsed between the two reads (a sub-tick interval) → nothing
    /// to divide by, so utilization is None rather than 0/0 = NaN, which would
    /// compare false against every band and silently disable the dimension.
    #[test]
    fn a_zero_tick_delta_yields_no_utilization() {
        let prev = base();
        let mut curr = base();
        curr.cpu_ticks_total = prev.cpu_ticks_total; // identical: Δtotal == 0
        curr.cpu_ticks_idle = prev.cpu_ticks_idle;
        let r = counter_rates(&prev, &curr, 15.0).unwrap();
        assert_eq!(r.cpu_utilization, None);
    }

    /// The headline correctness property. After a reboot the lifetime counters
    /// restart at zero, so a naive delta is hugely negative. Mutation-proof:
    /// delete the `boot_time_secs` comparison and this returns a rate of about
    /// -66,666 swapins/sec instead of `Rebooted`.
    #[test]
    fn a_reboot_invalidates_the_delta_rather_than_reporting_a_negative_rate() {
        let prev = base();
        let mut curr = base();
        curr.boot_time_secs = prev.boot_time_secs + 500; // rebooted
        curr.swapins = 12; // counters restarted
        curr.swapouts = 3;

        assert_eq!(
            counter_rates(&prev, &curr, 15.0),
            Err(RateGap::Rebooted),
            "a reboot must not produce a rate"
        );
    }

    /// Reboot must be diagnosed BEFORE the backwards-counter check, or the
    /// operator is told the data is corrupt when the machine merely restarted.
    /// Mutation-proof: move the backwards check above the reboot check and this
    /// yields `CounterWentBackwards`.
    #[test]
    fn a_reboot_is_diagnosed_as_a_reboot_not_as_a_counter_fault() {
        let prev = base();
        let mut curr = base();
        curr.boot_time_secs += 1;
        curr.swapins = 0; // both true: rebooted AND smaller
        curr.swapouts = 0;
        assert_eq!(counter_rates(&prev, &curr, 15.0), Err(RateGap::Rebooted));
    }

    #[test]
    fn counters_going_backwards_without_a_reboot_is_a_gap_not_a_negative_rate() {
        let prev = base();
        let mut curr = base();
        curr.swapins -= 5; // same boot time, smaller counter
        assert_eq!(
            counter_rates(&prev, &curr, 15.0),
            Err(RateGap::CounterWentBackwards)
        );
    }

    /// Each counter is checked independently — a regression in only swapouts
    /// must still be caught. Mutation-proof: drop the `curr.swapouts <
    /// prev.swapouts` half of the condition and this returns a negative rate.
    #[test]
    fn a_regression_in_either_counter_is_caught() {
        let prev = base();
        let mut curr = base();
        curr.swapins += 100; // forwards
        curr.swapouts -= 1; // backwards
        assert_eq!(
            counter_rates(&prev, &curr, 15.0),
            Err(RateGap::CounterWentBackwards)
        );
    }

    /// Zero and negative elapsed time both divide badly: 0 gives infinity, and a
    /// backwards clock flips the sign of a legitimate increase.
    #[test]
    fn non_positive_elapsed_time_is_refused() {
        let prev = base();
        let mut curr = base();
        curr.swapins += 100;
        for bad in [0.0, -15.0, f64::NAN, f64::INFINITY] {
            assert_eq!(
                counter_rates(&prev, &curr, bad),
                Err(RateGap::NoElapsedTime),
                "elapsed {bad} must be refused"
            );
        }
    }

    // ---- Trend ----------------------------------------------------------

    #[test]
    fn fits_a_falling_line_and_reports_a_negative_slope() {
        // 1 GB free, losing 1 MB/s over a minute.
        let pts: Vec<Point> = (0..=60)
            .map(|i| Point {
                t_secs: f64::from(i),
                value: 1e9 - f64::from(i) * 1e6,
            })
            .collect();
        let t = Trend::fit(&pts).unwrap();
        assert!(
            (t.slope_per_sec - -1e6).abs() < 1.0,
            "slope {}",
            t.slope_per_sec
        );
        assert_eq!(t.points, 61);
        assert_eq!(t.span_secs, 60.0);
        assert!((t.fitted_last - (1e9 - 60.0 * 1e6)).abs() < 1e3);
    }

    /// A single sample cannot have a trend, and neither can an empty series.
    /// Returning 0.0 here would read as "perfectly stable" on the very first
    /// sample after startup, which is the worst possible moment to claim calm.
    #[test]
    fn fewer_than_two_points_has_no_trend() {
        assert!(Trend::fit(&[]).is_none());
        assert!(
            Trend::fit(&[Point {
                t_secs: 5.0,
                value: 1.0
            }])
            .is_none()
        );
    }

    /// Zero variance in t is a division by zero, not a vertical line.
    /// Mutation-proof: remove the `sxx == 0.0` guard and `slope_per_sec`
    /// becomes NaN, which compares false against every threshold and would
    /// silently disable the dimension.
    #[test]
    fn identical_timestamps_have_no_trend() {
        let pts = vec![
            Point {
                t_secs: 100.0,
                value: 1.0,
            },
            Point {
                t_secs: 100.0,
                value: 9.0,
            },
            Point {
                t_secs: 100.0,
                value: 5.0,
            },
        ];
        assert!(
            Trend::fit(&pts).is_none(),
            "a zero time span must not yield a slope"
        );
    }

    /// The projection that turns a slope into a date.
    #[test]
    fn projects_time_until_a_falling_series_hits_zero() {
        // 100 GB free, losing 1 GB per hour.
        let per_sec = 1e9 / 3600.0;
        let pts: Vec<Point> = (0..=10)
            .map(|i| Point {
                t_secs: f64::from(i) * 3600.0,
                value: 100e9 - f64::from(i) * 1e9,
            })
            .collect();
        let t = Trend::fit(&pts).unwrap();
        assert!((t.slope_per_sec - -per_sec).abs() < 1.0);

        let secs = t.secs_until(0.0).expect("a falling line reaches zero");
        // 90 GB remain at the last point, at 1 GB/hour => 90 hours.
        assert!(
            (secs / 3600.0 - 90.0).abs() < 0.1,
            "got {} hours",
            secs / 3600.0
        );
    }

    /// A series moving AWAY from the target never reaches it. Mutation-proof:
    /// drop the sign comparison and this returns a large NEGATIVE duration,
    /// which a naive formatter would happily render as a time-to-full.
    #[test]
    fn a_rising_series_never_reaches_zero() {
        let pts: Vec<Point> = (0..=10)
            .map(|i| Point {
                t_secs: f64::from(i) * 60.0,
                value: 50e9 + f64::from(i) * 1e9, // freeing space
            })
            .collect();
        let t = Trend::fit(&pts).unwrap();
        assert!(t.slope_per_sec > 0.0);
        assert_eq!(
            t.secs_until(0.0),
            None,
            "space is being freed; there is no time-to-full"
        );
    }

    #[test]
    fn a_flat_series_never_reaches_a_different_target() {
        let pts: Vec<Point> = (0..=10)
            .map(|i| Point {
                t_secs: f64::from(i) * 60.0,
                value: 42e9,
            })
            .collect();
        let t = Trend::fit(&pts).unwrap();
        assert_eq!(t.slope_per_sec, 0.0);
        assert_eq!(t.secs_until(0.0), None);
    }

    #[test]
    fn already_at_the_target_is_zero_seconds_away() {
        let pts = vec![
            Point {
                t_secs: 0.0,
                value: 10.0,
            },
            Point {
                t_secs: 10.0,
                value: 0.0,
            },
        ];
        let t = Trend::fit(&pts).unwrap();
        assert_eq!(t.secs_until(0.0), Some(0.0));
    }

    /// Noise must not dominate the fit. Two-point differencing on the last pair
    /// here would report a steep climb; the fit sees the flat truth. This is the
    /// reason `Trend` exists rather than a subtraction.
    #[test]
    fn a_single_noisy_sample_does_not_dominate_the_fit() {
        let mut pts: Vec<Point> = (0..20)
            .map(|i| Point {
                t_secs: f64::from(i) * 15.0,
                value: 100.0,
            })
            .collect();
        // One wild final reading.
        pts.push(Point {
            t_secs: 300.0,
            value: 400.0,
        });

        let t = Trend::fit(&pts).unwrap();
        let two_point = (400.0 - 100.0) / 15.0; // 20.0 per second
        assert!(
            t.slope_per_sec < two_point / 10.0,
            "fit slope {} should be far below the two-point slope {two_point}",
            t.slope_per_sec
        );
    }
}
