use std::sync::{Arc, Mutex};

use banshee_api::{
    AppState, Transport, auth, build_router,
    config::{Config, Profile},
    sampler,
};
use banshee_core::census::CensusCollector;
use banshee_core::census::config::CensusConfig;
use banshee_core::census::exec::RealRunner;
use banshee_core::{Collector, RealProbe, SampleStore};

/// How far up the port ladder to climb when the configured port is busy.
const PORT_LADDER_STEPS: u16 = 10;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .with_writer(std::io::stderr)
        .init();

    let config = match Config::from_env() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("banshee-api: config error: {e}");
            std::process::exit(2);
        }
    };
    tracing::info!(?config.profile, db = %config.db_path.display(), "starting");

    // ONE store, one connection, one mutex. Until schema v6 this process opened the same
    // file twice (a `NoteStore` for the template slice and a `SampleStore` for the
    // series); deleting the slice collapsed it. Do not add a second.
    let samples = match SampleStore::open(&config.db_path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!(
                "banshee-api: cannot open database {}: {e}",
                config.db_path.display()
            );
            std::process::exit(1);
        }
    };

    let api_key = match auth::load_or_create_key(&config.key_file) {
        Ok(k) => k,
        Err(e) => {
            eprintln!(
                "banshee-api: cannot read/create key file {}: {e}",
                config.key_file.display()
            );
            std::process::exit(1);
        }
    };

    // A malformed census overlay is a loud failure, not a silent fall back to
    // generic defaults: the symptom of the silent version is an EMPTY
    // monitoring-agent section, which reads as good news.
    //
    // `BANSHEE_CENSUS_OVERLAY` pins the overlay to an explicit file. A test
    // knob in the same family as `BANSHEE_SAMPLE_INTERVAL_MS`: the e2e harness
    // uses it to make action candidates IMPOSSIBLE (an absurd stale_days, an
    // orphan pattern nothing matches), so exercising the execute routes on a
    // developer machine provably cannot kill anything real.
    let census_config = match std::env::var("BANSHEE_CENSUS_OVERLAY") {
        Ok(p) => CensusConfig::load_from(std::path::Path::new(&p)),
        Err(_) => CensusConfig::load(),
    };
    let census_config = match census_config {
        Ok(c) => c,
        Err(e) => {
            eprintln!("banshee-api: census config error: {e}");
            std::process::exit(2);
        }
    };

    // Pin the uptime clock before the sampler or any client can ask (/stats).
    let _ = banshee_api::process_started();

    // Built once and shared: the state recomputes deltas with this exact config,
    // and the sampler evaluates with it — two copies would let the two drift.
    let sampler_config = sampler::SamplerConfig::from_env();

    let state = AppState {
        samples: Arc::new(Mutex::new(samples)),
        pressure: Arc::new(Mutex::new(None)),
        headroom: Arc::new(Mutex::new(None)),
        // The SAME config the census classifies with, so the action's staleness
        // threshold and busy list cannot disagree with what the census showed.
        actions: Arc::new(banshee_core::actions::ActionRunner::new(
            Box::new(RealRunner),
            census_config.clone(),
        )),
        api_key,
        db_path: config.db_path.clone(),
        ops: Arc::new(Mutex::new(banshee_api::OpsHealth::default())),
        // The SAME config the sampler evaluates with, so the deltas read's
        // recomputed verdict cannot disagree with /pressure (ADR-0005).
        pressure_config: sampler_config.pressure.clone(),
    };

    // The sampling loop is what makes this a sentinel rather than an API: it
    // must run whether or not any client is connected, and it must outlive a
    // closed window (ADR-0004). `shutdown_tx` stops it once axum has drained,
    // so a SIGTERM does not leave a half-written sample.
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    match CensusConfig::overlay_path() {
        Some(p) => tracing::info!(
            overlay = %p.display(),
            monitor_agents = census_config.monitor_agents.len(),
            "census config loaded from overlay"
        ),
        None => tracing::info!(
            searched = ?CensusConfig::overlay_paths(),
            monitor_agents = census_config.monitor_agents.len(),
            "census config: no overlay found, using generic defaults"
        ),
    }

    // The Slack escalation sink, if a webhook is configured. Absent by default —
    // the daemon then behaves exactly as before. The URL is a credential and
    // comes from the environment, never the repo (see slack::WEBHOOK_ENV).
    let alert_sink = banshee_api::slack::SlackSink::from_env()
        .map(|s| Arc::new(s) as Arc<dyn banshee_api::slack::AlertSink>);
    match &alert_sink {
        Some(_) => tracing::info!("Slack escalation sink enabled"),
        None => tracing::info!("no Slack webhook configured; escalation alerts stay in-app"),
    }

    let sampler_task = tokio::spawn(sampler::run(
        sampler::SamplerDeps {
            collector: Arc::new(Collector::with_defaults(RealProbe)),
            census: Arc::new(CensusCollector::new(RealRunner, census_config)),
            store: Arc::clone(&state.samples),
            latest: Arc::clone(&state.pressure),
            latest_headroom: Arc::clone(&state.headroom),
            alert_sink,
            ops: Arc::clone(&state.ops),
        },
        sampler_config,
        shutdown_rx,
    ));

    // Port ladder: try the configured port, then climb — EXCEPT in the prod
    // profile, which pins (ADR-0004). Whatever binds is REPORTED ON STDOUT —
    // clients must parse that rather than assume the configured port.
    let listener = bind_port(config.port, config.profile).await;
    let addr = listener.local_addr().expect("listener has a local addr");
    // The socket is the transport clients authenticate over (ADR-0008). TCP stays
    // bound but serves /health only: since Phase 3 the SERVER refuses every credential
    // there, so a stale client is locked out with a 401 that names the fix rather than
    // quietly kept working by sending the key to a squattable port.
    let socket_listener = bind_socket(&config.socket_path).await;
    println!("banshee-api listening on http://127.0.0.1:{}", addr.port());
    // ANNOUNCED, on the same principle as the port (ADR-0004): the configured path is
    // a request, and only what the daemon says it bound is the truth. `check-service.sh`
    // parses this rather than assuming.
    println!(
        "banshee-api listening on unix://{}",
        config.socket_path.display()
    );
    use std::io::Write;
    std::io::stdout().flush().ok();

    // One route table, two transports, two routers — because the router cannot tell
    // from a request how it arrived, the flavour is fixed here, where the listener is
    // known. THIS PAIRING IS THE SECURITY DECISION: swap the two and the port accepts
    // the key again. `tests/e2e/run-e2e.sh` pins it against the real binary (a keyed
    // read over TCP must 401 while the same read over the socket succeeds). Both
    // futures share the graceful-shutdown signal so a SIGTERM stops both rather than
    // leaving one serving from a half-dead process.
    let tcp = axum::serve(listener, build_router(state.clone(), Transport::Tcp))
        .with_graceful_shutdown(shutdown_signal());
    let uds = axum::serve(socket_listener, build_router(state, Transport::Unix))
        .with_graceful_shutdown(shutdown_signal());
    let (tcp_result, uds_result) = tokio::join!(tcp, uds);
    for (transport, result) in [("tcp", tcp_result), ("unix", uds_result)] {
        if let Err(e) = result {
            tracing::error!(transport, "server error: {e}");
            eprintln!("banshee-api: {transport} server error: {e}");
            std::process::exit(1);
        }
    }
    // The socket file is NOT removed on shutdown, deliberately. A crash cannot clean
    // up after itself, so the stale-socket path in `bind_socket` has to be correct
    // anyway — and making the tidy path depend on graceful shutdown would mean the
    // messy path is the one that never gets exercised.
    // Stop the sampler and let it finish the cycle it is in, so shutdown never
    // truncates a sample mid-transaction.
    let _ = shutdown_tx.send(true);
    if let Err(e) = sampler_task.await {
        tracing::warn!("sampler task did not shut down cleanly: {e}");
    }
    tracing::info!("shutdown complete");
}

/// Resolve once SIGINT (Ctrl-C) or SIGTERM arrives, so axum can stop
/// accepting new connections and drain in-flight requests before the process
/// exits. `launchctl bootout` and `launchctl kickstart -k` both send SIGTERM;
/// without this the process is killed mid-request, which for a sampling daemon
/// means a truncated write on every service restart.
async fn shutdown_signal() {
    let ctrl_c = async {
        if let Err(e) = tokio::signal::ctrl_c().await {
            tracing::warn!("failed to install Ctrl-C handler: {e}");
            // Never resolve: fall back to SIGTERM-only shutdown.
            std::future::pending::<()>().await;
        }
    };

    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut sig) => {
                sig.recv().await;
            }
            Err(e) => {
                tracing::warn!("failed to install SIGTERM handler: {e}");
                std::future::pending::<()>().await;
            }
        }
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = ctrl_c => tracing::info!("SIGINT received; draining"),
        () = terminate => tracing::info!("SIGTERM received; draining"),
    }
}

/// How many ports to try, given the profile.
///
/// **The prod profile pins; dev and test climb** (ADR-0004). Prod is the
/// LaunchAgent's profile, and 18769 is the port every client falls back to
/// (`banshee_core::DEFAULT_API_PORT`) — a daemon that quietly moved to 18770 would
/// leave the MCP server and the menu bar talking to nothing while `launchctl`
/// reported a healthy service. A busy port there means a daemon is already running,
/// and the honest response is to fail loudly in the log.
///
/// Dev keeps the ladder so a foreground `./scripts/start.sh` never has to fight
/// whatever is already bound. Test uses port 0 (ephemeral), which needs no ladder
/// at all.
fn ladder_steps(base_port: u16, profile: Profile) -> u16 {
    match profile {
        // Port 0 = ephemeral; the kernel picks, so there is nothing to climb.
        _ if base_port == 0 => 1,
        Profile::Prod => 1,
        Profile::Dev | Profile::Test => PORT_LADDER_STEPS,
    }
}

/// Bind the Unix-domain socket clients authenticate over (ADR-0008).
///
/// This exists because a fixed loopback PORT can be impersonated: any local user may
/// bind 127.0.0.1:18769 before the daemon starts, and every client would then hand it
/// the file-loaded API key. A socket file cannot be impersonated the same way — it
/// lives in a directory only this user can write, and the kernel enforces its mode.
///
/// **Verified by execution on macOS 26, because the whole security claim rests on it:**
/// `chmod 000` on a listening socket denies `connect` even to its OWNER with `EACCES`,
/// so the mode is a real access control and not decoration (some BSDs ignore it).
/// Equally important and much easier to miss — **`bind()` creates the socket 0755**
/// under the usual umask, i.e. connectable by everyone. The mode must be set
/// AFTERWARDS, which leaves a window; the parent directory being owner-only is what
/// closes it, so both are done here and both are verified rather than assumed.
async fn bind_socket(path: &std::path::Path) -> tokio::net::UnixListener {
    use std::os::unix::fs::PermissionsExt;

    if !banshee_core::socket_path_fits(path) {
        eprintln!(
            "banshee-api: socket path is {} bytes, over the {}-byte sockaddr_un limit: {}",
            path.as_os_str().len(),
            banshee_core::MAX_SOCKET_PATH_LEN,
            path.display()
        );
        std::process::exit(1);
    }

    if let Some(parent) = path.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            eprintln!("banshee-api: cannot create {}: {e}", parent.display());
            std::process::exit(1);
        }
        // Owner-only, so nobody else can reach the socket even during the window
        // between bind() and the chmod below. Best-effort: a pre-existing directory
        // may be owned by someone else, in which case the bind will fail anyway.
        let _ = std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700));
    }

    // A STALE socket file must be removed, but a LIVE one must never be stolen.
    // Deciding by "does the file exist" would let a second daemon unlink the socket
    // the running one is serving on — the clients would keep talking to a deleted
    // inode and the new daemon would look healthy, which is ADR-0004's eight-day
    // outage with a different mechanism. So: try to CONNECT. Success means somebody
    // is serving and this process is the mistake.
    if path.exists() {
        match tokio::net::UnixStream::connect(path).await {
            Ok(_) => {
                tracing::error!(
                    socket = %path.display(),
                    "a daemon is already serving this socket; refusing to start a second one"
                );
                eprintln!(
                    "banshee-api: {} is already being served by a running daemon.\n\
                     banshee-api: this second instance is the mistake — run \
                     ./scripts/check-service.sh.",
                    path.display()
                );
                std::process::exit(1);
            }
            // Nobody is listening: the file is a leftover from a crash or a kill -9.
            Err(_) => {
                if let Err(e) = std::fs::remove_file(path) {
                    eprintln!(
                        "banshee-api: cannot remove stale socket {}: {e}",
                        path.display()
                    );
                    std::process::exit(1);
                }
                tracing::warn!(socket = %path.display(), "removed a stale socket file");
            }
        }
    }

    let listener = match tokio::net::UnixListener::bind(path) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("banshee-api: cannot bind {}: {e}", path.display());
            std::process::exit(1);
        }
    };

    if let Err(e) = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)) {
        eprintln!(
            "banshee-api: cannot make {} owner-only: {e}",
            path.display()
        );
        std::process::exit(1);
    }
    // VERIFY rather than trust. A socket left group/other-connectable is the whole
    // vulnerability this transport exists to close, so failing to tighten it must be
    // fatal and loud — never a warning in a log nobody reads.
    if let Err(why) = socket_is_owner_only(path) {
        eprintln!("banshee-api: {why}; refusing to serve");
        std::process::exit(1);
    }
    listener
}

/// Whether the socket at `path` is safe to serve on: owner-only, so no other local
/// user can connect (ADR-0008).
///
/// Split out from `bind_socket` so the refusal can be TESTED. `bind_socket` exits the
/// process on failure — correct for a daemon, untestable in a unit test — and a
/// security check that cannot be exercised is a security check nobody knows still
/// works.
fn socket_is_owner_only(path: &std::path::Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    let mode = std::fs::metadata(path)
        .map_err(|e| format!("cannot stat {}: {e}", path.display()))?
        .permissions()
        .mode();
    if mode & 0o077 == 0 {
        Ok(())
    } else {
        Err(format!(
            "{} is mode {:04o}, which allows group/other access",
            path.display(),
            mode & 0o7777
        ))
    }
}

async fn bind_port(base_port: u16, profile: Profile) -> tokio::net::TcpListener {
    let steps = ladder_steps(base_port, profile);
    for offset in 0..steps {
        let port = base_port.saturating_add(offset);
        match tokio::net::TcpListener::bind(("127.0.0.1", port)).await {
            Ok(l) => return l,
            Err(e) => {
                tracing::warn!(port, "bind failed: {e}");
            }
        }
    }
    if steps == 1 {
        // The pinned case. Say what to check, because "address in use" on a
        // LaunchAgent almost always means the agent is already running and
        // something just tried to start a second one.
        tracing::error!(
            port = base_port,
            "cannot bind the pinned port; the daemon may already be running \
             (scripts/check-service.sh)"
        );
        eprintln!(
            "banshee-api: port {base_port} is busy and the {profile:?} profile does \
             not climb the port ladder.\n\
             banshee-api: if the LaunchAgent is already running, this second \
             instance is the mistake — run ./scripts/check-service.sh."
        );
    } else {
        eprintln!(
            "banshee-api: no free port in {base_port}..{}",
            base_port.saturating_add(steps)
        );
    }
    std::process::exit(1);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **The daemon must refuse to serve a socket other users can connect to**
    /// (ADR-0008).
    ///
    /// This is the whole security property of the transport, and it is easy to get
    /// wrong silently: `bind()` creates the socket **0755** under the usual umask —
    /// connectable by everybody — so the `chmod` afterwards IS the access control, and
    /// a mode that failed to apply must stop the daemon rather than produce a warning
    /// in a log nobody reads.
    ///
    /// Verified by execution on macOS that the mode is a real control at all: `chmod
    /// 000` on a listening socket denies `connect` even to its owner with `EACCES`.
    /// Some BSDs ignore the mode entirely, in which case this check would be theatre.
    ///
    /// FIXTURE NOTE: the two cases must differ ONLY in the group/other bits. A fixture
    /// comparing 0600 against 0777 would also pass an implementation that keyed on the
    /// owner bits, or on equality with 0600 — so the permissive case here is **0640**,
    /// which is owner-identical to 0600 and differs only in the bits that matter.
    #[test]
    fn the_socket_is_owner_only_or_the_daemon_refuses_to_serve() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("api.sock");
        let _listener = std::os::unix::net::UnixListener::bind(&path).unwrap();

        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert!(
            socket_is_owner_only(&path).is_ok(),
            "0600 is exactly what the daemon must accept"
        );

        // Owner-identical to 0600; only group gains read.
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o640)).unwrap();
        let err = socket_is_owner_only(&path)
            .expect_err("a group-readable socket must be refused, not served");
        assert!(
            err.contains("0640") && err.contains("group/other"),
            "the refusal must name the mode and why: {err}"
        );

        // And the mode bind() actually leaves behind, which is the realistic failure.
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(
            socket_is_owner_only(&path).is_err(),
            "0755 is what bind() creates under the usual umask; it must be refused"
        );
    }

    /// A socket that does not exist is an ERROR, never a pass. A check that treats
    /// "cannot stat" as "fine" would wave through the case where the socket was
    /// unlinked between bind and verification.
    #[test]
    fn a_missing_socket_is_not_treated_as_safe() {
        assert!(socket_is_owner_only(std::path::Path::new("/nonexistent/api.sock")).is_err());
    }

    /// The prod profile PINS its port; dev and test climb.
    ///
    /// Mutation-proof, verified: give `Profile::Prod` `PORT_LADDER_STEPS` and this
    /// fails — which is the state where a second daemon silently serves on 18770
    /// while every client keeps asking 18769.
    #[test]
    fn only_the_prod_profile_pins_its_port() {
        assert_eq!(ladder_steps(18769, Profile::Prod), 1, "prod must pin");
        assert_eq!(ladder_steps(18779, Profile::Dev), PORT_LADDER_STEPS);
        assert_eq!(ladder_steps(18999, Profile::Test), PORT_LADDER_STEPS);
    }

    /// An ephemeral port has nothing to climb, whatever the profile.
    #[test]
    fn an_ephemeral_port_never_climbs() {
        for profile in [Profile::Prod, Profile::Dev, Profile::Test] {
            assert_eq!(ladder_steps(0, profile), 1, "{profile:?}");
        }
    }
}
