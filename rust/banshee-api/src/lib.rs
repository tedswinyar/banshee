// banshee-api — the hub. Every client (CLI, MCP, Swift app) talks HTTP
// to this server; nothing else opens the database.
//
// Concurrency ceiling, stated honestly: the store is a single SQLite
// connection behind one `std::sync::Mutex`, so EVERY request — reads
// included — is fully serialized (it is a Mutex, not an RwLock). For a
// single-user local tool this is correct and cheap: each handler locks,
// does sub-millisecond synchronous SQLite work, and drops the guard without
// awaiting, so the "std Mutex held across .await" deadlock footgun is
// avoided. It does NOT scale to concurrent clients or heavy/slow queries —
// the moment either arrives, move to a connection pool (r2d2/deadpool-sqlite
// with WAL) or wrap DB calls in `spawn_blocking`. This is a deliberate
// ceiling for the target use case, not "no lock contention".

pub mod auth;
pub mod config;
pub mod routes;
pub mod sampler;
pub mod slack;

use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::Instant;

use banshee_core::SampleStore;
use banshee_core::actions::ActionRunner;
use banshee_core::pressure::Pressure;
use banshee_core::pressure::config::PressureConfig;
use banshee_core::pressure::headroom::Headroom;
use chrono::{DateTime, Utc};

/// When this process started, for `/stats`' `selfUptimeSecs` — the denominator
/// that turns cumulative CPU seconds into an honest lifetime average. A
/// process-wide `OnceLock` rather than an `AppState` field: it is a fact about
/// the PROCESS, `main` pins it before anything else runs, and a test server that
/// never touched it gets "since first asked", which is still true.
static PROCESS_STARTED: OnceLock<Instant> = OnceLock::new();

pub fn process_started() -> Instant {
    *PROCESS_STARTED.get_or_init(Instant::now)
}

#[derive(Clone)]
pub struct AppState {
    /// The one store. Until schema v6 this sat alongside a second `NoteStore` on the
    /// same database file — two connections and two mutexes where there should be
    /// one. Deleting the template's Note slice collapsed them. **Do not add a
    /// third:** ADR-0001's "one writer" invariant is about one writing process,
    /// and every additional connection is another place that can hold a lock while
    /// the sampler wants to write.
    pub samples: Arc<Mutex<SampleStore>>,
    /// The latest verdict, published by the sampler.
    ///
    /// `None` until the first evaluation. Read it through
    /// [`AppState::pressure_now`], which renders that as `Checking` — never as an
    /// invented `Quiet`.
    pub pressure: Arc<Mutex<Option<Pressure>>>,
    /// The verdict reduced to a decision for agents (ADR-0010), published by the
    /// sampler beside `pressure` from the same evaluation. `None` until the first
    /// evaluation; read it through [`AppState::headroom_now`], which renders that
    /// as "wait" — never as invented capacity.
    pub headroom: Arc<Mutex<Option<Headroom>>>,
    /// The two reap actions. Shares the census's config so the action's
    /// staleness threshold and busy list can never disagree with what the
    /// census showed the user. Never touches the store — action reports travel
    /// in the response, not the database.
    pub actions: Arc<ActionRunner>,
    pub api_key: String,
    /// Where the database file lives, so the backup route can place verified
    /// snapshots in a `backups/` directory beside it (banshee-3xh). Carried on
    /// the state rather than re-derived from config, so the handler cannot pick
    /// a different data dir than the store actually opened.
    pub db_path: std::path::PathBuf,
    /// Operational health of the sampling loop, surfaced on `/stats`.
    /// The sampler writes it; the route reads it. Without this a
    /// persistently failing evaluation serves a FROZEN verdict, and a repeatedly
    /// failing sweep silently violates ADR-0006's bounded-retention promise —
    /// both visible only in a log nobody reads. "If an operator must see it, it
    /// travels over the API" is the rule.
    pub ops: Arc<Mutex<OpsHealth>>,
    /// The SAME pressure config the sampler evaluates with (`SamplerConfig::pressure`).
    /// The deltas read recomputes the verdict over stored history (ADR-0005); with
    /// a different config than the sampler's, its verdict could disagree with
    /// `/pressure` on the very same machine. Carried here rather than re-derived
    /// so the two cannot drift.
    pub pressure_config: PressureConfig,
}

/// Liveness of the sampler's two periodic jobs — evaluation and the retention
/// sweep — as counters the `/stats` route can serve.
///
/// The `record_*` methods are the whole logic, split out so they are
/// mutation-tested in isolation rather than only through the big async loop.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OpsHealth {
    /// When `evaluate` last SUCCEEDED. A client comparing this to now can tell a
    /// live `Quiet` from one frozen minutes ago by a stuck evaluation.
    pub last_evaluated_at: Option<DateTime<Utc>>,
    /// Consecutive evaluation failures since the last success. Non-zero means the
    /// served verdict is going stale right now.
    pub consecutive_eval_failures: u64,
    /// When the retention sweep last SUCCEEDED.
    pub last_swept_at: Option<DateTime<Utc>>,
    /// Consecutive sweep failures since the last success. Sustained non-zero
    /// means the row count is no longer bounded — the DB is growing unswept.
    pub consecutive_sweep_failures: u64,
}

impl OpsHealth {
    pub fn record_eval_ok(&mut self, now: DateTime<Utc>) {
        self.last_evaluated_at = Some(now);
        self.consecutive_eval_failures = 0;
    }

    pub fn record_eval_failure(&mut self) {
        self.consecutive_eval_failures = self.consecutive_eval_failures.saturating_add(1);
    }

    pub fn record_sweep_ok(&mut self, now: DateTime<Utc>) {
        self.last_swept_at = Some(now);
        self.consecutive_sweep_failures = 0;
    }

    pub fn record_sweep_failure(&mut self) {
        self.consecutive_sweep_failures = self.consecutive_sweep_failures.saturating_add(1);
    }
}

impl AppState {
    /// Poison-tolerant access to the store. A panic in one handler while
    /// holding the lock poisons the mutex; recovering the inner guard here
    /// (rather than `.unwrap()`-panicking) keeps a single bad request from
    /// bricking every subsequent one. The store's operations are ACID per
    /// statement, so a recovered guard still sees a consistent database.
    pub fn samples(&self) -> MutexGuard<'_, SampleStore> {
        self.samples
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// The latest verdict, or `None` before the first evaluation.
    pub fn latest_pressure(&self) -> Option<Pressure> {
        self.pressure
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    /// The verdict to serve RIGHT NOW — always a real `Pressure`.
    ///
    /// The only place the `None` case is handled, so no handler and no client can
    /// answer it differently. Core decides what "we have not looked yet" looks
    /// like (`Pressure::checking`); this function does not know the level's name
    /// and must not learn it (ADR-0005).
    pub fn pressure_now(&self, now: DateTime<Utc>) -> Pressure {
        self.latest_pressure()
            .unwrap_or_else(|| Pressure::checking(now))
    }

    /// The headroom decision to serve RIGHT NOW — always a real `Headroom`.
    ///
    /// Before the first evaluation the answer is DERIVED from the Checking
    /// placeholder by the same core function the sampler uses, with no core
    /// count — so it says "wait" for the same reason and in the same words, and
    /// this crate never learns what "no data" should look like (ADR-0005,
    /// ADR-0010 decision 3).
    pub fn headroom_now(&self, now: DateTime<Utc>) -> Headroom {
        self.headroom
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
            .unwrap_or_else(|| {
                banshee_core::pressure::headroom::derive(
                    &Pressure::checking(now),
                    None,
                    &banshee_core::pressure::config::PressureConfig::default(),
                )
            })
    }

    /// The pressure config the sampler is running with — the one the deltas read
    /// must recompute against so its verdict cannot disagree with `/pressure`.
    pub fn pressure_config(&self) -> &PressureConfig {
        &self.pressure_config
    }
}

/// Which listener a `Router` is built for (ADR-0008, Phase 3).
///
/// The router does not — cannot — inspect a request to learn how it arrived, so the
/// answer is fixed when the router is BUILT and `main` pairs each flavour with its
/// listener. `Unix` is the 0600 socket and accepts the key. `Tcp` is the loopback port
/// and accepts NO credential on any route: there is one key for every client, so the
/// server cannot tell a caller's "explicit" `BANSHEE_API_KEY` from the file key, and
/// "allow explicit keys over TCP" would mean "allow the key over TCP". Only `/health`
/// answers there, so a supervisor's liveness probe and a stale client's 401 both still
/// work — a client that predates the socket hears "your credential is refused here"
/// rather than "connection refused", which would read as the daemon being down.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transport {
    /// The owner-only Unix-domain socket. The key is accepted here and nowhere else.
    Unix,
    /// Loopback TCP. `/health` only; every keyed route refuses with a 401 that names
    /// the socket, whether or not a key was presented.
    Tcp,
}

pub fn build_router(state: AppState, transport: Transport) -> axum::Router {
    routes::router(state, transport)
}

#[cfg(test)]
mod tests {
    use super::*;
    use banshee_core::pressure::level::Level;

    fn state(store: Arc<Mutex<SampleStore>>) -> AppState {
        AppState {
            samples: store,
            pressure: Arc::new(Mutex::new(None)),
            headroom: Arc::new(Mutex::new(None)),
            // Present because the struct requires it; nothing in these tests
            // calls an action route, so the real runner never spawns anything.
            actions: Arc::new(ActionRunner::new(
                Box::new(banshee_core::census::exec::RealRunner),
                banshee_core::census::config::CensusConfig::default(),
            )),
            api_key: "k".into(),
            db_path: std::path::PathBuf::from(":memory:"),
            ops: Arc::new(Mutex::new(OpsHealth::default())),
            pressure_config: PressureConfig::default(),
        }
    }

    /// A failure increments the counter; a success resets it AND stamps the time.
    /// Mutation-proof: make `record_eval_failure` a no-op and the count stays 0;
    /// drop the reset in `record_eval_ok` and the count survives a success.
    #[test]
    fn ops_health_tracks_eval_success_and_failure() {
        let mut ops = OpsHealth::default();
        assert_eq!(ops.consecutive_eval_failures, 0);
        assert!(ops.last_evaluated_at.is_none());

        ops.record_eval_failure();
        ops.record_eval_failure();
        assert_eq!(ops.consecutive_eval_failures, 2, "failures accumulate");

        let now = Utc::now();
        ops.record_eval_ok(now);
        assert_eq!(
            ops.consecutive_eval_failures, 0,
            "a success resets the streak"
        );
        assert_eq!(ops.last_evaluated_at, Some(now), "and stamps the time");
    }

    /// Same contract for the sweep — the counter that says the DB may be growing
    /// unswept (ADR-0006). Kept separate from eval so one failing does not mask
    /// the other's health.
    #[test]
    fn ops_health_tracks_sweep_success_and_failure() {
        let mut ops = OpsHealth::default();
        ops.record_sweep_failure();
        assert_eq!(ops.consecutive_sweep_failures, 1);
        assert_eq!(
            ops.consecutive_eval_failures, 0,
            "sweep and eval are independent"
        );

        let now = Utc::now();
        ops.record_sweep_ok(now);
        assert_eq!(ops.consecutive_sweep_failures, 0);
        assert_eq!(ops.last_swept_at, Some(now));
    }

    // Mutation-proof (Testing standard): revert `samples()` to
    // `.lock().unwrap()` and this test panics instead of passing.
    #[test]
    fn store_access_survives_a_poisoned_mutex() {
        let store = Arc::new(Mutex::new(SampleStore::open_in_memory().unwrap()));
        // Poison the mutex: panic while holding the guard on another thread.
        let poisoner = Arc::clone(&store);
        let _ = std::thread::spawn(move || {
            let _guard = poisoner.lock().unwrap();
            panic!("simulated handler panic while holding the store lock");
        })
        .join();
        assert!(store.is_poisoned(), "precondition: mutex must be poisoned");

        let state = state(store);
        // Must NOT panic despite the poison, and the store must still work.
        assert_eq!(state.samples().count_samples().unwrap(), 0);
    }

    /// Before the sampler's first evaluation there is no verdict, and the answer
    /// to "how is the machine" is `Checking` — not `Quiet`.
    ///
    /// Mutation-proof, verified: change `pressure_now` to
    /// `unwrap_or_else(|| Pressure { level: Level::Quiet, .. })`-shaped output (or
    /// simply assert on the wrong level) and this fails. This is the guard on the
    /// one lie the menu bar must never tell.
    #[test]
    fn an_unevaluated_daemon_reports_checking_not_quiet() {
        let state = state(Arc::new(Mutex::new(SampleStore::open_in_memory().unwrap())));
        assert!(state.latest_pressure().is_none(), "precondition");

        let p = state.pressure_now(Utc::now());
        assert_eq!(p.level, Level::Checking);
        assert_ne!(p.level, Level::Quiet);
        assert_eq!(p.glyph, Level::Checking.glyph());
        assert!(p.dimensions.is_empty());
    }

    /// Before the first evaluation the headroom answer is "wait", with zero
    /// parallelism — never an invented capacity (ADR-0010 decision 3).
    ///
    /// Mutation-proof, verified: make `headroom_now` fall back to a hand-built
    /// `Headroom { should_wait: false, recommended_parallelism: 1, .. }` and
    /// this fails; so does deriving from a Quiet placeholder instead of Checking.
    #[test]
    fn an_unevaluated_daemon_has_no_headroom() {
        let state = state(Arc::new(Mutex::new(SampleStore::open_in_memory().unwrap())));
        let h = state.headroom_now(Utc::now());
        assert!(h.should_wait);
        assert_eq!(h.recommended_parallelism, 0);
        assert_eq!(h.level, Level::Checking);
        assert_eq!(h.cores, None);
        assert!(h.retry_after_secs.is_some(), "waiting needs a retry");
    }

    /// Once the sampler publishes, the decision is served verbatim too.
    #[test]
    fn a_published_headroom_is_served_verbatim() {
        let state = state(Arc::new(Mutex::new(SampleStore::open_in_memory().unwrap())));
        let mut published = state.headroom_now(Utc::now());
        published.should_wait = false;
        published.recommended_parallelism = 5;
        published.retry_after_secs = None;
        published.cores = Some(8);
        *state.headroom.lock().unwrap() = Some(published.clone());

        assert_eq!(state.headroom_now(Utc::now()), published);
    }

    /// Once the sampler publishes, that verdict is served verbatim — the API does
    /// not re-evaluate, re-band, or re-glyph it.
    #[test]
    fn a_published_verdict_is_served_verbatim() {
        let state = state(Arc::new(Mutex::new(SampleStore::open_in_memory().unwrap())));
        let published = Pressure {
            glyph: "😱🧠".into(),
            level: Level::Wailing,
            level_name: "Wailing".into(),
            ..Pressure::checking(Utc::now())
        };
        *state.pressure.lock().unwrap() = Some(published.clone());

        assert_eq!(state.pressure_now(Utc::now()), published);
    }
}
