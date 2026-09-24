// sampler — the loop that makes Banshee a sentinel rather than a command.
//
// Ticks the cheap tier, stores each reading, and periodically runs the retention
// sweep. The sweep lives ON this loop rather than on its own schedule so it
// cannot be independently disabled or forgotten, and so it runs on exactly the
// machines that are producing data (ADR-0006).
//
// Measured cost per tick (`cargo run --release --example sample_throughput`):
// ~171 µs to insert, ~1.2 ms for a steady-state sweep. Against a 15s tick that
// is a duty cycle of ~0.001%, which is why the single store mutex is sufficient
// and no connection pool is needed.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use banshee_core::census::CensusCollector;
use banshee_core::census::exec::CommandRunner;
use banshee_core::pressure::config::PressureConfig;
use banshee_core::pressure::episode::level_changed_materially;
use banshee_core::pressure::headroom::Headroom;
use banshee_core::pressure::{Assessment, Pressure, assess};
use banshee_core::sample_store::{Retention, SampleStore};
use banshee_core::sys::SysProbe;
use banshee_core::{Collector, sample::ROLLUP_BUCKET_SECS};
use chrono::Utc;
use tokio::sync::watch;

use crate::slack::{AlertSink, deliver_escalations};

/// Cadence of the cheap tier. Every signal it reads is a syscall costing
/// microseconds (ADR-0007), so 15s is affordable; the census tier needs
/// subprocesses and runs far less often.
///
/// The literal lives in `banshee_core`, because the PRESSURE MODEL needs it too —
/// it has to know what a normal interval looks like in order to recognise a gap.
/// Two places independently knowing the cadence is the same class
/// of bug as two places knowing the port.
pub const DEFAULT_INTERVAL: Duration =
    Duration::from_secs(banshee_core::DEFAULT_SAMPLE_INTERVAL_SECS);

/// How often to run the retention sweep. One bucket width: sweeping more often
/// than a bucket can close is pure overhead, and less often lets raw samples
/// pile up past their window.
pub const SWEEP_EVERY: Duration = Duration::from_secs(ROLLUP_BUCKET_SECS as u64);

/// Cadence of the census tier. Five minutes, because it spawns subprocesses
/// (`ps`, `tmux`, selective `lsof`) and the cheap tier deliberately does not —
/// ~288 spawns a day against the 28,800 a 15s subprocess loop would cost
/// (ADR-0007).
pub const CENSUS_EVERY: Duration = Duration::from_secs(banshee_core::DEFAULT_CENSUS_INTERVAL_SECS);

pub struct SamplerConfig {
    pub interval: Duration,
    pub census_every: Duration,
    pub sweep_every: Duration,
    pub retention: Retention,
    pub pressure: PressureConfig,
}

impl Default for SamplerConfig {
    fn default() -> Self {
        Self {
            interval: DEFAULT_INTERVAL,
            census_every: CENSUS_EVERY,
            sweep_every: SWEEP_EVERY,
            retention: Retention::default(),
            pressure: PressureConfig::default(),
        }
    }
}

impl SamplerConfig {
    /// Defaults, with the three cadences overridable in MILLISECONDS by
    /// `BANSHEE_SAMPLE_INTERVAL_MS`, `BANSHEE_CENSUS_INTERVAL_MS` and
    /// `BANSHEE_SWEEP_INTERVAL_MS`, and the three episode timings (ADR-0009) in
    /// SECONDS by `BANSHEE_EPISODE_UP_SECS`, `BANSHEE_EPISODE_DOWN_SECS` and
    /// `BANSHEE_EPISODE_REPEAT_SECS`.
    ///
    /// **This exists for `tests/e2e/run-e2e.sh`, and it is a seam rather than a
    /// feature.** The harness needs a daemon that reaches a settled verdict in
    /// under a second and then takes exactly one census, so its byte-parity
    /// comparisons run against a payload that is not changing underneath them. The
    /// alternative was a fixed `sleep 16` in the harness — which encodes an
    /// assumption about machine speed and is wrong in both directions.
    ///
    /// A bad value is IGNORED with a warning rather than fatal: these are not
    /// user-facing configuration, and a typo in a test script must not stop the
    /// sentinel from sampling. Contrast `BANSHEE_PORT`, which is user-facing and
    /// refuses loudly — a monitor on the wrong port is silently useless, whereas a
    /// monitor on the default cadence is simply correct.
    pub fn from_env() -> Self {
        let base = Self::default();
        let interval = env_millis("BANSHEE_SAMPLE_INTERVAL_MS", base.interval);
        let census_every = env_millis("BANSHEE_CENSUS_INTERVAL_MS", base.census_every);
        let mut config = Self {
            interval,
            census_every,
            sweep_every: env_millis("BANSHEE_SWEEP_INTERVAL_MS", base.sweep_every),
            ..base
        };
        // TELL THE MODEL what cadence it is actually getting. The model uses it to
        // tell a missed tick from a gap in the observations; if the two disagree,
        // every interval looks like a gap, every run collapses to zero, and nothing
        // can escalate past Restless — silently (`banshee-s6s`). The model
        // self-calibrates from the observed median as a backstop, but there is no
        // reason to make it guess when the sampler knows.
        //
        // Rounded UP to whole seconds, and never to zero: a sub-second test cadence
        // must not produce a zero-second expectation, which would make the
        // configured floor useless.
        config.pressure.sample_interval_secs = secs_ceil(interval);
        config.pressure.census_interval_secs = secs_ceil(census_every);
        // The episode lifecycle's timings, so a dogfood run (or the e2e harness)
        // can walk a real red → recovering → closed cycle in minutes instead of
        // waiting out the production windows. Same non-fatal rule as the
        // cadences: a typo must not stop the sentinel.
        let p = &mut config.pressure;
        p.episode_up_secs = env_secs("BANSHEE_EPISODE_UP_SECS", p.episode_up_secs);
        p.episode_down_secs = env_secs("BANSHEE_EPISODE_DOWN_SECS", p.episode_down_secs);
        p.episode_repeat_secs = env_secs("BANSHEE_EPISODE_REPEAT_SECS", p.episode_repeat_secs);
        p.disk_yellow_episode_up_secs = env_secs(
            "BANSHEE_DISK_YELLOW_EPISODE_UP_SECS",
            p.disk_yellow_episode_up_secs,
        );
        // The headroom read's memory budget per worker (ADR-0010). Bytes, via the
        // same lenient reader — the unit is seconds there and bytes here, but the
        // parse-or-fall-back rule is identical. Zero is refused: a zero budget
        // would make `memSlots` a division by zero, which `derive` guards by
        // ignoring the cap, so the knob would silently do nothing.
        p.headroom_memory_per_slot_bytes = match env_secs(
            "BANSHEE_HEADROOM_SLOT_BYTES",
            p.headroom_memory_per_slot_bytes,
        ) {
            0 => {
                tracing::warn!("BANSHEE_HEADROOM_SLOT_BYTES=0 is not a budget; using the default");
                PressureConfig::default().headroom_memory_per_slot_bytes
            }
            n => n,
        };
        config
    }
}

/// A whole-seconds override, or the fallback. Zero IS accepted here — an up delay
/// of 0 ("open on the first confirmed red") and a down window of 0 ("close on the
/// first green") are meaningful test configurations, unlike a zero tick period.
fn env_secs(name: &str, fallback: u64) -> u64 {
    match std::env::var(name) {
        Err(_) => fallback,
        Ok(raw) => match raw.trim().parse::<u64>() {
            Ok(secs) => secs,
            Err(_) => {
                tracing::warn!(
                    "{name}={raw:?} is not a non-negative integer number of seconds; \
                     using the default {fallback}"
                );
                fallback
            }
        },
    }
}

/// A duration as whole seconds, rounded up, minimum 1.
fn secs_ceil(d: Duration) -> u64 {
    let ms = d.as_millis();
    let secs = ms.div_ceil(1000);
    (secs as u64).max(1)
}

fn env_millis(name: &str, fallback: Duration) -> Duration {
    match std::env::var(name) {
        Err(_) => fallback,
        Ok(raw) => match raw.trim().parse::<u64>() {
            // Zero would make `tokio::time::interval` panic, which is a worse
            // failure than ignoring the value.
            Ok(ms) if ms > 0 => Duration::from_millis(ms),
            _ => {
                tracing::warn!(
                    "{name}={raw:?} is not a positive integer number of \
                     milliseconds; using the default {:?}",
                    fallback
                );
                fallback
            }
        },
    }
}

/// What the loop drives. Bundled rather than passed as four positional
/// arguments, because the next tier would have made `run` unreadable.
pub struct SamplerDeps<P: SysProbe, R: CommandRunner> {
    pub collector: Arc<Collector<P>>,
    pub census: Arc<CensusCollector<R>>,
    pub store: Arc<Mutex<SampleStore>>,
    /// The latest verdict, published for the API to serve without re-evaluating.
    ///
    /// Held here rather than recomputed per request because the model reads a
    /// window of history: a menu bar polling every 60s would otherwise re-read
    /// 40 samples and 2 censuses each time. It is a CACHE of a pure function, not
    /// state — `evaluate` remains reproducible from the database alone (ADR-0005).
    pub latest: Arc<Mutex<Option<Pressure>>>,
    /// The headroom decision from the SAME evaluation (ADR-0010), published in
    /// the same breath so `/headroom` and `/pressure` never describe different
    /// ticks. A cache of a pure function, like `latest`.
    pub latest_headroom: Arc<Mutex<Option<Headroom>>>,
    /// Where escalation (Shrieking) alerts go beyond the store and the menu bar.
    /// `None` when no webhook is configured — the daemon then behaves exactly as
    /// before. Delivery is best-effort and never blocks this loop.
    pub alert_sink: Option<Arc<dyn AlertSink>>,
    /// Operational liveness this loop writes on every eval and sweep, shared with
    /// the API so `/stats` can serve it (banshee-4r1).
    pub ops: Arc<Mutex<crate::OpsHealth>>,
}

/// Run the sampling loop until `shutdown` flips to `true`.
///
/// **A failed cycle must never stop the loop.** A transient kernel or database
/// error is a gap in the series; a dead sampler is a monitor that has silently
/// stopped monitoring, which is strictly worse and looks identical from outside.
/// So errors are logged and the loop continues.
pub async fn run<P, R>(
    deps: SamplerDeps<P, R>,
    config: SamplerConfig,
    mut shutdown: watch::Receiver<bool>,
) where
    P: SysProbe + 'static,
    R: CommandRunner + 'static,
{
    let SamplerDeps {
        collector,
        census,
        store,
        latest,
        latest_headroom,
        alert_sink,
        ops,
    } = deps;

    let mut tick = tokio::time::interval(config.interval);
    // Burst would fire the missed ticks back-to-back after a laptop wakes from
    // sleep, writing a clump of samples that all carry nearly the same instant.
    // Delay just resumes the cadence, and the gap in the series is the honest
    // record of a machine that was asleep.
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    let mut sweep_tick = tokio::time::interval(config.sweep_every);
    sweep_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    // The first tick of a tokio interval fires immediately; for the sweep that
    // would mean a full sweep at every startup, so consume it.
    sweep_tick.tick().await;

    // The census DOES take its first tick immediately: a freshly started daemon
    // with no census at all cannot answer "what is running", and that is the
    // question a user opening the app right after login will ask.
    let mut census_tick = tokio::time::interval(config.census_every);
    census_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    tracing::info!(
        interval_secs = config.interval.as_secs(),
        census_secs = config.census_every.as_secs(),
        sweep_secs = config.sweep_every.as_secs(),
        "sampler started"
    );

    loop {
        tokio::select! {
            _ = tick.tick() => {
                match collector.collect() {
                    Ok(sample) => {
                        // Lock, do sub-millisecond synchronous SQLite work, drop
                        // the guard. Never held across an await — that is the
                        // std::sync::Mutex deadlock footgun.
                        let result = {
                            let st = store.lock().unwrap_or_else(|p| p.into_inner());
                            st.insert(&sample)
                        };
                        if let Err(e) = result {
                            tracing::error!(error = %e, "failed to store sample");
                        }

                        // Re-evaluate on every sample: the verdict must be current
                        // when a client asks, and evaluation is pure and cheap
                        // against an in-memory window.
                        match evaluate_now(&store, &config.pressure) {
                            Ok(Assessment {
                                pressure: p,
                                headroom,
                                episodes,
                                notifications,
                            }) => {
                                let previous = latest
                                    .lock()
                                    .unwrap_or_else(|e| e.into_inner())
                                    .as_ref()
                                    .map(|prev| prev.level);
                                if previous != Some(p.level) {
                                    // A material change — into notification
                                    // territory, or all the way back out of it — is
                                    // the machine-wide story a person would want in
                                    // the log; the rest is churn at info.
                                    if level_changed_materially(previous, p.level) {
                                        tracing::warn!(
                                            level = %p.level_name,
                                            glyph = %p.glyph,
                                            was = ?previous,
                                            "pressure level changed materially"
                                        );
                                    } else {
                                        tracing::info!(
                                            level = %p.level_name,
                                            glyph = %p.glyph,
                                            was = ?previous,
                                            "pressure level changed"
                                        );
                                    }
                                }
                                for e in &episodes {
                                    tracing::info!(
                                        dimension = e.dimension.key(),
                                        state = ?e.state,
                                        suppressed = e.suppressed,
                                        peak = ?e.peak.level,
                                        "episode"
                                    );
                                }
                                for n in &notifications {
                                    // A notice is the one thing here a person is
                                    // meant to see, so it is logged at warn.
                                    tracing::warn!(
                                        dimension = n.dimension.key(),
                                        kind = ?n.kind,
                                        level = ?n.level,
                                        suppressed = n.suppressed,
                                        "ALERT: {}",
                                        n.message
                                    );
                                }
                                // Fire-and-forget the escalation (Shrieking) notices
                                // to Slack, if a sink is configured. Spawned, so a
                                // slow webhook never delays the next sample.
                                if let Some(sink) = &alert_sink {
                                    deliver_escalations(sink, &notifications);
                                }
                                *latest.lock().unwrap_or_else(|e| e.into_inner()) = Some(p);
                                *latest_headroom
                                    .lock()
                                    .unwrap_or_else(|e| e.into_inner()) = Some(headroom);
                                ops.lock()
                                    .unwrap_or_else(|e| e.into_inner())
                                    .record_eval_ok(Utc::now());
                            }
                            Err(e) => {
                                tracing::error!(error = %e, "failed to evaluate pressure");
                                // Count the failure so /stats shows the served
                                // verdict is going stale — the log alone is
                                // invisible (banshee-4r1).
                                ops.lock()
                                    .unwrap_or_else(|e| e.into_inner())
                                    .record_eval_failure();
                            }
                        }
                    }
                    Err(e) => tracing::error!(error = %e, "failed to read the kernel"),
                }
            }
            _ = census_tick.tick() => {
                // A census shells out, so it is done off the reactor: a `ps` walk
                // on a loaded machine can take tens of milliseconds, and blocking
                // the runtime would stall the 15s cheap tier behind it.
                let census_collector = Arc::clone(&census);
                let taken = tokio::task::spawn_blocking(move || census_collector.collect()).await;
                match taken {
                    Ok(Ok(c)) => {
                        let result = {
                            let st = store.lock().unwrap_or_else(|p| p.into_inner());
                            st.insert_census(&c)
                        };
                        match result {
                            Ok(()) => tracing::debug!(
                                sessions = c.agent_sessions.len(),
                                stale = c.stale_sessions().count(),
                                orphans = c.orphans.orphan_count,
                                procs = c.total_procs,
                                "census"
                            ),
                            Err(e) => tracing::error!(error = %e, "failed to store census"),
                        }
                    }
                    Ok(Err(e)) => tracing::error!(error = %e, "census failed"),
                    Err(e) => tracing::error!("census task panicked: {e}"),
                }
            }
            _ = sweep_tick.tick() => {
                let result = {
                    let st = store.lock().unwrap_or_else(|p| p.into_inner());
                    st.sweep(Utc::now(), config.retention)
                };
                match result {
                    Ok(report) => {
                        // Only speak when something happened; a heartbeat every
                        // 5 minutes is how a log becomes unreadable.
                        if report != Default::default() {
                            tracing::info!(
                                rollups_written = report.rollups_written,
                                raw_deleted = report.raw_samples_deleted,
                                rollups_deleted = report.rollups_deleted,
                                alerts_deleted = report.alerts_deleted,
                                censuses_deleted = report.censuses_deleted,
                                "retention sweep"
                            );
                        }
                        ops.lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .record_sweep_ok(Utc::now());
                    }
                    Err(e) => {
                        tracing::error!(error = %e, "retention sweep failed");
                        // A repeatedly failing sweep means the row count is no
                        // longer bounded (ADR-0006); surface it on /stats rather
                        // than leaving it only in the log (banshee-4r1).
                        ops.lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .record_sweep_failure();
                    }
                }
            }
            _ = shutdown.changed() => {
                if *shutdown.borrow() {
                    tracing::info!("sampler stopping");
                    return;
                }
            }
        }
    }
}

/// Read the window and the episode record, assess, and persist the episodes that
/// moved.
///
/// The episode record is read from the DATABASE rather than kept in memory, so
/// the lifecycle survives a daemon restart (ADR-0009 decision 9): a restart
/// mid-crisis continues the open episode instead of re-announcing it, and the
/// repeat clock keeps its place. The last day is read so the verdict's
/// `activity` summary is honest; every open episode comes along however old.
fn evaluate_now(
    store: &Arc<Mutex<SampleStore>>,
    config: &PressureConfig,
) -> banshee_core::Result<Assessment> {
    let now = Utc::now();

    let st = store.lock().unwrap_or_else(|e| e.into_inner());
    // More than the model window: the extra history is disk's, whose yellow
    // episode up-delay is longer than the window spans (`banshee-dok`). The
    // model still bands everything else on the trailing `window_samples`.
    let samples = st.recent_samples(config.history_samples())?;
    // Two censuses is enough for the census-derived dimensions to have a series to
    // apply hysteresis over; more would only slow the read.
    let censuses = st.recent_censuses(3)?;
    let recent = st.episodes_since(now - chrono::Duration::hours(24))?;
    // The week tier, for the disk trend (`banshee-nio`): an indexed read of a
    // few thousand buckets, milliseconds against the 15s cadence.
    let rollups = st.rollups_between(
        now - chrono::Duration::seconds(config.disk_trend_window_secs as i64),
        now,
    )?;

    let assessment = assess(now, &samples, &censuses, &rollups, &recent, config);
    for e in &assessment.episodes {
        st.upsert_episode(e)?;
    }
    Ok(assessment)
}

#[cfg(test)]
mod tests {
    /// One lock for every test that touches the process environment: two
    /// tests each holding their OWN lock would still race each other.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    use super::*;
    use banshee_core::census::exec::CommandRunner;
    use banshee_core::sys::{SysReading, VolumeReading};
    use banshee_core::{CoreError, Result};
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct FakeProbe {
        /// Shared with the test, because `Collector` owns the probe privately —
        /// the counter has to be observable from outside without a getter that
        /// exists only for tests.
        reads: Arc<AtomicUsize>,
        /// Fail every read once this is set, to prove the loop survives it.
        fail: bool,
    }

    impl SysProbe for FakeProbe {
        fn read(&self) -> Result<SysReading> {
            self.reads.fetch_add(1, Ordering::SeqCst);
            if self.fail {
                return Err(CoreError::Schema("simulated kernel failure".into()));
            }
            Ok(SysReading {
                ncpu: 8,
                mem_total_bytes: 25_769_803_776,
                page_size: 16384,
                boot_time_secs: 1_788_122_573,
                load_1m: 1.0,
                load_5m: 1.0,
                load_15m: 1.0,
                swap_total_bytes: 8_589_934_592,
                swap_used_bytes: 1_000,
                pages_free: 1_000,
                pages_active: 1_000,
                pages_inactive: 1_000,
                pages_wired: 1_000,
                pages_speculative: 0,
                pages_compressor: 1_000,
                pages_purgeable: 0,
                pages_external: 0,
                swapins: 1,
                swapouts: 2,
                memory_pressure_level: 1,
                thermal_pressure_level: Some(0),
                cpu_speed_limit_percent: None,
                cpu_ticks_total: Some(80_000_000),
                cpu_ticks_idle: Some(60_000_000),
                kernel_free_percent: Some(50),
                jetsam_kills: Some(0),
            })
        }
        fn volumes(&self, _: &[String]) -> Result<Vec<VolumeReading>> {
            Ok(vec![VolumeReading {
                mount_point: "/System/Volumes/Data".into(),
                total_bytes: 1_000_000,
                avail_bytes: 500_000,
            }])
        }
    }

    /// A census runner that returns a fixed, minimal `ps` reading and counts
    /// how often it was asked — enough to prove the census runs on the loop
    /// without duplicating the census module's own tests.
    struct FakeCensusRunner {
        calls: Arc<AtomicUsize>,
    }

    impl CommandRunner for FakeCensusRunner {
        fn run(&self, program: &str, _args: &[&str]) -> Result<String> {
            if program == "ps" {
                self.calls.fetch_add(1, Ordering::SeqCst);
                // One core row and one args row; enough for a valid census.
                return Ok("  1     0  14608 21:43:13  10:59.18 ??       /sbin/launchd\n".into());
            }
            Ok(String::new())
        }
    }

    struct Rig {
        collector: Arc<Collector<FakeProbe>>,
        census: Arc<CensusCollector<FakeCensusRunner>>,
        store: Arc<Mutex<SampleStore>>,
        config: SamplerConfig,
        reads: Arc<AtomicUsize>,
        census_calls: Arc<AtomicUsize>,
        latest: Arc<Mutex<Option<Pressure>>>,
        latest_headroom: Arc<Mutex<Option<Headroom>>>,
        ops: Arc<Mutex<crate::OpsHealth>>,
    }

    impl Rig {
        fn deps(&self) -> SamplerDeps<FakeProbe, FakeCensusRunner> {
            SamplerDeps {
                collector: Arc::clone(&self.collector),
                census: Arc::clone(&self.census),
                store: Arc::clone(&self.store),
                latest: Arc::clone(&self.latest),
                latest_headroom: Arc::clone(&self.latest_headroom),
                // No sink in the loop tests: escalation delivery is covered in
                // `slack.rs` against a recording fake, so the loop tests keep the
                // network out of it.
                alert_sink: None,
                // Shared with the Rig so a test can inspect the ops-health the
                // loop records (banshee-4r1).
                ops: Arc::clone(&self.ops),
            }
        }
    }

    fn fast(fail: bool) -> Rig {
        let reads = Arc::new(AtomicUsize::new(0));
        let census_calls = Arc::new(AtomicUsize::new(0));
        Rig {
            collector: Arc::new(Collector::with_defaults(FakeProbe {
                reads: Arc::clone(&reads),
                fail,
            })),
            census: Arc::new(CensusCollector::new(
                FakeCensusRunner {
                    calls: Arc::clone(&census_calls),
                },
                banshee_core::census::config::CensusConfig::default(),
            )),
            store: Arc::new(Mutex::new(SampleStore::open_in_memory().unwrap())),
            config: SamplerConfig {
                interval: Duration::from_millis(10),
                census_every: Duration::from_millis(30),
                sweep_every: Duration::from_millis(50),
                retention: Retention::default(),
                pressure: PressureConfig::default(),
            },
            reads,
            census_calls,
            latest: Arc::new(Mutex::new(None)),
            latest_headroom: Arc::new(Mutex::new(None)),
            ops: Arc::new(Mutex::new(crate::OpsHealth::default())),
        }
    }

    /// Poll until a condition holds rather than sleeping a fixed time. A fixed
    /// sleep encodes an assumption about machine speed and is wrong in both
    /// directions — too short and it flakes under load, too long and every run
    /// pays the worst case.
    async fn settle<F: Fn() -> bool>(cond: F) -> bool {
        for _ in 0..400 {
            if cond() {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        false
    }

    /// A sub-second cadence must not become a ZERO-second expectation for the
    /// model: the configured gap ceiling would then be meaningless, and only the
    /// self-calibrating median would stand between a gap and a false "sustained".
    ///
    /// Mutation-proof: drop the `.max(1)` and the 120ms case returns 0.
    #[test]
    fn a_sub_second_cadence_rounds_up_to_one_second() {
        assert_eq!(secs_ceil(Duration::from_millis(120)), 1);
        assert_eq!(secs_ceil(Duration::from_millis(1)), 1);
        assert_eq!(secs_ceil(Duration::from_secs(15)), 15);
        assert_eq!(secs_ceil(Duration::from_millis(15_001)), 16, "rounds UP");
        assert_eq!(secs_ceil(Duration::from_secs(300)), 300);
    }

    /// The sampler must TELL the model what cadence it resolved — through
    /// `from_env`, which is the thing that can actually be broken.
    ///
    /// **The first version of this test asserted on a hand-built config and never
    /// called `from_env` at all**, so deleting the propagation from `from_env`
    /// changed nothing and the pin was theatre. Testing the environment path means
    /// setting environment variables, which is process-global state; these are the
    /// only tests in this crate that touch `BANSHEE_*_INTERVAL_MS`, and `config.rs`'s
    /// env tests deliberately clear a disjoint set, so there is nothing to race.
    ///
    /// Mutation-proof, verified: remove the two propagation lines from `from_env`
    /// and this fails, leaving the model expecting 15s while the sampler ticks at
    /// 120ms — every interval a "gap", every run zero, nothing able to escalate.
    #[test]
    fn the_resolved_cadence_is_propagated_to_the_pressure_model() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());

        // The defaults must already agree, without any environment at all.
        let defaults = SamplerConfig::default();
        assert_eq!(
            defaults.pressure.sample_interval_secs,
            defaults.interval.as_secs(),
            "the defaults must agree before any override"
        );
        assert_eq!(
            defaults.pressure.census_interval_secs,
            defaults.census_every.as_secs()
        );

        let keys = [
            "BANSHEE_SAMPLE_INTERVAL_MS",
            "BANSHEE_CENSUS_INTERVAL_MS",
            "BANSHEE_EPISODE_UP_SECS",
            "BANSHEE_EPISODE_DOWN_SECS",
            "BANSHEE_EPISODE_REPEAT_SECS",
            "BANSHEE_HEADROOM_SLOT_BYTES",
        ];
        let saved: Vec<_> = keys.iter().map(|k| (*k, std::env::var(k).ok())).collect();
        unsafe {
            std::env::set_var("BANSHEE_SAMPLE_INTERVAL_MS", "120");
            std::env::set_var("BANSHEE_CENSUS_INTERVAL_MS", "600000");
            std::env::set_var("BANSHEE_EPISODE_UP_SECS", "0");
            std::env::set_var("BANSHEE_EPISODE_DOWN_SECS", "7");
            std::env::set_var("BANSHEE_EPISODE_REPEAT_SECS", "not a number");
            std::env::set_var("BANSHEE_HEADROOM_SLOT_BYTES", "536870912");
        }

        let config = SamplerConfig::from_env();

        // Restore before asserting, so a failure cannot leak state into other tests.
        for (k, v) in saved {
            unsafe {
                match v {
                    Some(v) => std::env::set_var(k, v),
                    None => std::env::remove_var(k),
                }
            }
        }

        assert_eq!(config.interval, Duration::from_millis(120));
        assert_eq!(config.census_every, Duration::from_millis(600_000));
        assert_eq!(
            config.pressure.sample_interval_secs, 1,
            "a 120ms tick must reach the model as 1s, not as the 15s default"
        );
        assert_eq!(config.pressure.census_interval_secs, 600);
        // The episode timings (ADR-0009) reach the model through the same path —
        // the one that can actually be broken. Zero is a legal up
        // delay; garbage falls back to the default rather than to zero.
        assert_eq!(config.pressure.episode_up_secs, 0);
        assert_eq!(config.pressure.episode_down_secs, 7);
        assert_eq!(
            config.pressure.episode_repeat_secs,
            PressureConfig::default().episode_repeat_secs,
            "garbage must fall back to the default, never to 0"
        );
        // The headroom memory budget (ADR-0010) reaches the model the same way.
        // 512 MiB, a value nothing else uses, so a knob wired to the wrong field
        // or left at the literal default is caught here and not in production.
        assert_eq!(
            config.pressure.headroom_memory_per_slot_bytes, 536_870_912,
            "BANSHEE_HEADROOM_SLOT_BYTES must reach PressureConfig"
        );
    }

    /// A zero slot budget is refused, not honoured: `derive` treats a zero
    /// budget as "no memory cap", so honouring it would make the knob silently
    /// do nothing — and a config value that silently does nothing is worse than
    /// none. Mutation-proof: drop the `0 =>` arm and this reads 0.
    #[test]
    fn a_zero_headroom_slot_budget_falls_back_to_the_default() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let saved = std::env::var("BANSHEE_HEADROOM_SLOT_BYTES").ok();
        unsafe { std::env::set_var("BANSHEE_HEADROOM_SLOT_BYTES", "0") };
        let config = SamplerConfig::from_env();
        unsafe {
            match saved {
                Some(v) => std::env::set_var("BANSHEE_HEADROOM_SLOT_BYTES", v),
                None => std::env::remove_var("BANSHEE_HEADROOM_SLOT_BYTES"),
            }
        }
        assert_eq!(
            config.pressure.headroom_memory_per_slot_bytes,
            PressureConfig::default().headroom_memory_per_slot_bytes
        );
    }

    /// The cadence seam: an override in milliseconds is honoured, and garbage is
    /// IGNORED rather than fatal or (worse) zero — `tokio::time::interval` panics
    /// on a zero period, which would take the whole daemon down over a typo in a
    /// test script.
    ///
    /// Mutation-proof: drop the `ms > 0` guard and the "0" case returns a
    /// zero-length interval instead of the default, and this fails.
    #[test]
    fn a_cadence_override_is_parsed_and_bad_values_fall_back() {
        // Env vars are process-global; this test reads through the helper
        // directly rather than mutating them, so it cannot race the others.
        let fallback = Duration::from_secs(15);
        for (raw, want) in [
            ("250", Duration::from_millis(250)),
            ("1", Duration::from_millis(1)),
            ("0", fallback),
            ("-1", fallback),
            ("half a second", fallback),
            ("", fallback),
        ] {
            let key = "BANSHEE_TEST_CADENCE_MS";
            unsafe { std::env::set_var(key, raw) };
            assert_eq!(env_millis(key, fallback), want, "{raw:?}");
            unsafe { std::env::remove_var(key) };
        }
        // An unset variable takes the default.
        assert_eq!(
            env_millis("BANSHEE_TEST_UNSET_CADENCE_MS", fallback),
            fallback
        );
    }

    #[tokio::test]
    async fn the_loop_stores_samples_on_every_tick() {
        let rig = fast(false);
        let store = Arc::clone(&rig.store);
        let (tx, rx) = watch::channel(false);
        let handle = tokio::spawn(run(rig.deps(), rig.config, rx));

        let count = || {
            store
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .count_samples()
                .unwrap()
        };
        assert!(
            settle(|| count() >= 3).await,
            "expected samples to accumulate"
        );

        tx.send(true).unwrap();
        handle.await.unwrap();
    }

    /// The loop records its operational liveness so `/stats` can serve it:
    /// a healthy loop stamps `last_evaluated_at` and keeps the
    /// failure count at 0. Mutation-proof: drop the `record_eval_ok` call in the
    /// loop's success arm and `last_evaluated_at` stays None here.
    #[tokio::test]
    async fn the_loop_records_operational_health() {
        let rig = fast(false);
        let ops = Arc::clone(&rig.ops);
        let (tx, rx) = watch::channel(false);
        let handle = tokio::spawn(run(rig.deps(), rig.config, rx));

        assert!(
            settle(|| ops
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .last_evaluated_at
                .is_some())
            .await,
            "a healthy loop must stamp last_evaluated_at"
        );
        assert_eq!(
            ops.lock()
                .unwrap_or_else(|p| p.into_inner())
                .consecutive_eval_failures,
            0,
            "a healthy loop has no eval failures"
        );

        tx.send(true).unwrap();
        handle.await.unwrap();
    }

    /// The headline resilience property. A monitor whose loop dies on a
    /// transient error has silently stopped monitoring, and looks identical from
    /// outside to one that is working. Mutation-proof: change the `Err` arm to
    /// `return` and this hangs, then fails.
    #[tokio::test]
    async fn a_failing_kernel_read_does_not_kill_the_loop() {
        let rig = fast(true);
        let store = Arc::clone(&rig.store);
        let reads = Arc::clone(&rig.reads);
        let (tx, rx) = watch::channel(false);
        let handle = tokio::spawn(run(rig.deps(), rig.config, rx));

        // Reads keep being ATTEMPTED even though every one fails.
        assert!(
            settle(|| reads.load(Ordering::SeqCst) >= 3).await,
            "the loop must keep trying after failures"
        );
        // And nothing was stored, because a failed read must not become a
        // zeroed sample.
        assert_eq!(
            store
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .count_samples()
                .unwrap(),
            0
        );

        tx.send(true).unwrap();
        handle.await.unwrap();
    }

    /// The census runs on the same loop, at its own cadence, and is persisted.
    /// Mutation-proof: remove the `census_tick` arm and this hangs, then fails.
    #[tokio::test]
    async fn the_census_runs_on_the_loop_and_is_stored() {
        let rig = fast(false);
        let store = Arc::clone(&rig.store);
        let calls = Arc::clone(&rig.census_calls);
        let (tx, rx) = watch::channel(false);
        let handle = tokio::spawn(run(rig.deps(), rig.config, rx));

        let stored = || {
            store
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .count_censuses()
                .unwrap()
        };
        assert!(settle(|| stored() >= 2).await, "censuses must accumulate");
        // Two `ps` calls per census.
        assert!(calls.load(Ordering::SeqCst) >= 4);

        tx.send(true).unwrap();
        handle.await.unwrap();
    }

    /// The census is SLOWER than the cheap tier and must not stall it. With a
    /// 10ms sample tick and a 30ms census tick, samples must clearly outnumber
    /// censuses. Mutation-proof: run the census inline on the reactor without
    /// `spawn_blocking` and the ratio collapses under a slow `ps`.
    #[tokio::test]
    async fn the_cheap_tier_ticks_faster_than_the_census() {
        let rig = fast(false);
        let store = Arc::clone(&rig.store);
        let (tx, rx) = watch::channel(false);
        let handle = tokio::spawn(run(rig.deps(), rig.config, rx));

        let counts = || {
            let st = store.lock().unwrap_or_else(|p| p.into_inner());
            (st.count_samples().unwrap(), st.count_censuses().unwrap())
        };
        assert!(settle(|| counts().1 >= 3).await, "need several censuses");
        let (samples, censuses) = counts();
        assert!(
            samples > censuses,
            "expected more samples ({samples}) than censuses ({censuses})"
        );

        tx.send(true).unwrap();
        handle.await.unwrap();
    }

    /// The loop evaluates pressure on every sample and publishes the verdict, so
    /// a client asking "what is the level" never triggers a 40-sample read.
    /// Mutation-proof: remove the `evaluate_now` call and `latest` stays None.
    #[tokio::test]
    async fn the_loop_publishes_a_verdict() {
        let rig = fast(false);
        let latest = Arc::clone(&rig.latest);
        let (tx, rx) = watch::channel(false);
        let handle = tokio::spawn(run(rig.deps(), rig.config, rx));

        let has_verdict = || {
            latest
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .as_ref()
                .map(|p| p.level)
        };
        assert!(
            settle(|| has_verdict().is_some()).await,
            "a verdict must be published"
        );
        // The fake probe reports a calm machine; two samples is enough to leave
        // Checking behind.
        assert!(
            settle(|| has_verdict()
                .is_some_and(|l| l != banshee_core::pressure::level::Level::Checking))
            .await,
            "got {:?}",
            has_verdict()
        );
        // And the headroom decision is published from the SAME evaluation
        // (ADR-0010), carrying the fake probe's core count. The fake describes a
        // tiny machine (16 MB available, 500 KB of disk), so the DECISION here is
        // "wait" — the test is about publication, and the invariant, not the value.
        // Mutation-proof: drop the `latest_headroom` store and this stays None.
        let headroom = rig
            .latest_headroom
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
            .expect("headroom is published beside the verdict");
        assert_eq!(headroom.level, has_verdict().unwrap());
        assert_eq!(headroom.cores, Some(8), "the fake probe's core count");
        assert_eq!(
            headroom.should_wait,
            headroom.recommended_parallelism == 0,
            "{headroom:?}"
        );

        tx.send(true).unwrap();
        handle.await.unwrap();
    }

    /// A red machine opens an alert EPISODE, and the episode is PERSISTED — so
    /// the lifecycle survives a restart instead of re-announcing everything
    /// (ADR-0009). Mutation-proof: drop the `upsert_episode` call and this fails.
    #[tokio::test]
    async fn a_red_machine_records_a_persisted_alert() {
        let rig = fast(false);
        // Make the fake kernel look saturated: the probe reports load 1.0 on 8
        // cores, so tighten the threshold instead of faking a new probe.
        let mut config = rig.config;
        config.pressure.cpu = banshee_core::pressure::band::BandSpec::rising(0.01, 0.02, 0.001, 1);
        config.pressure.red_grace_secs = 0;
        config.pressure.episode_up_secs = 0;
        config.pressure.min_samples = 2;

        let store = Arc::clone(&rig.store);
        let (tx, rx) = watch::channel(false);
        let deps = SamplerDeps {
            collector: Arc::clone(&rig.collector),
            census: Arc::clone(&rig.census),
            store: Arc::clone(&store),
            latest: Arc::clone(&rig.latest),
            latest_headroom: Arc::clone(&rig.latest_headroom),
            alert_sink: None,
            ops: Arc::new(Mutex::new(crate::OpsHealth::default())),
        };
        let handle = tokio::spawn(run(deps, config, rx));

        let episodes = || {
            store
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .count_episodes()
                .unwrap()
        };
        assert!(
            settle(|| episodes() >= 1).await,
            "an episode must be recorded"
        );

        // Episodes are PER DIMENSION, and one per dimension. The fake probe's
        // tiny pages_free and volume make memory and disk red too, so several
        // dimensions legitimately open one episode each.
        let settled = episodes();
        assert!(settled >= 1, "got {settled}");

        // Many more samples pass at a 10ms tick. A sustained red is ONE episode,
        // so the count must not grow — the point-event model would have written
        // a row per cooldown; this writes one row per incident.
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert_eq!(
            episodes(),
            settled,
            "a sustained red must stay one open episode, not accumulate rows"
        );

        // And each dimension has exactly one, still open and firing.
        let open = store
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .open_episodes()
            .unwrap();
        assert_eq!(open.len(), settled, "every episode is still open");
        let mut dims: Vec<&str> = open.iter().map(|e| e.dimension.key()).collect();
        let n = dims.len();
        dims.sort_unstable();
        dims.dedup();
        assert_eq!(dims.len(), n, "no dimension opened twice: {dims:?}");
        assert!(
            open.iter()
                .all(|e| e.state == banshee_core::pressure::episode::EpisodeState::Firing),
            "{open:?}"
        );
        // The verdict the loop published carries the record it just wrote.
        let published = rig
            .latest
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
            .unwrap();
        assert_eq!(
            published.activity.open, settled,
            "activity rides the verdict"
        );

        tx.send(true).unwrap();
        handle.await.unwrap();
    }

    #[tokio::test]
    async fn the_loop_stops_when_shutdown_is_signalled() {
        let rig = fast(false);
        let (tx, rx) = watch::channel(false);
        let handle = tokio::spawn(run(rig.deps(), rig.config, rx));

        tx.send(true).unwrap();
        // If the loop ignored the signal this await would never return, and the
        // test would time out rather than fail — so bound it explicitly.
        let stopped = tokio::time::timeout(Duration::from_secs(5), handle).await;
        assert!(stopped.is_ok(), "the loop must exit on shutdown");
    }

    /// A shutdown channel that reports `false` must NOT stop the loop — only a
    /// true value means stop. Mutation-proof: drop the `if *shutdown.borrow()`
    /// check and this fails, because `changed()` fires on any send.
    #[tokio::test]
    async fn a_false_shutdown_signal_does_not_stop_the_loop() {
        let rig = fast(false);
        let store = Arc::clone(&rig.store);
        let (tx, rx) = watch::channel(false);
        let handle = tokio::spawn(run(rig.deps(), rig.config, rx));

        tx.send(false).unwrap(); // a change, but not a stop
        let count = || {
            store
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .count_samples()
                .unwrap()
        };
        assert!(
            settle(|| count() >= 2).await,
            "the loop must still be sampling after a false signal"
        );

        tx.send(true).unwrap();
        tokio::time::timeout(Duration::from_secs(5), handle)
            .await
            .expect("must stop on a true signal")
            .unwrap();
    }

    /// The sweep must run from the LOOP, not only when called directly.
    ///
    /// Note what does NOT work: shrinking `Retention::raw` below the bucket
    /// width cannot force a sweep, because the cutoff is floored to a bucket
    /// boundary (`bucket_start_of(now - raw)`), so freshly written samples are
    /// always inside the current, deliberately-unrolled bucket. The first
    /// version of this test did that and failed for a correct reason. So it
    /// pre-seeds genuinely aged data instead.
    #[tokio::test]
    async fn the_sweep_runs_on_the_loop_and_rolls_up_aged_samples() {
        let rig = fast(false);
        let store = Arc::clone(&rig.store);

        // A sample from 30 hours ago: outside the default 24h raw window and in
        // a long-closed bucket.
        {
            let st = store.lock().unwrap_or_else(|p| p.into_inner());
            let reading = rig.collector.collect().unwrap().to_reading();
            let old = Utc::now() - chrono::Duration::hours(30);
            st.insert(&banshee_core::sample::Sample::from_reading(
                old,
                &reading,
                &[],
            ))
            .unwrap();
            assert_eq!(st.count_rollups().unwrap(), 0, "precondition");
        }

        let (tx, rx) = watch::channel(false);
        let handle = tokio::spawn(run(rig.deps(), rig.config, rx));

        let rollups = || {
            store
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .count_rollups()
                .unwrap()
        };
        assert!(
            settle(|| rollups() >= 1).await,
            "the sweep must run from the loop itself"
        );

        tx.send(true).unwrap();
        handle.await.unwrap();
    }
}
