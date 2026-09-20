// banshee — CLI client. Talks HTTP to banshee-api; never opens the
// database directly. Designed for scripting: --json on every read, stable
// exit codes, all diagnostics on stderr.
//
// Exit codes:
//   0  success — INCLUDING when the machine is on fire
//   1  server rejected the request (4xx/5xx other than 404)
//   2  usage error (clap)
//   3  not found (no census taken yet)
//   4  cannot reach the API server
//
// **The severity level is NOT an exit code.** `banshee status` exits 0 whether
// the verdict is Quiet or Shrieking, because the exit code answers "did the
// command work", and overloading it would make `banshee status || notify` fire on
// an unreachable daemon exactly as loudly as on a thrashing machine — the two
// cases needing the most different responses. Scripts branch on
// `banshee status --json | jq -r .level`.
//
// Every subcommand here corresponds to exactly one HTTP route and one MCP tool.
// `tests/e2e/run-e2e.sh` enforces that, and enforces that --json output is
// byte-identical to the raw HTTP body. The one exception is `backup` (HTTP +
// CLI, no MCP tool — operator maintenance, not an agent surface; the carve-out
// is asserted in e2e).

use banshee_core::actions::{OrphanReapReport, SessionReapReport, SessionVerdict};
use banshee_core::census::Census;
use banshee_core::pressure::band::Band;
use banshee_core::pressure::deltas::Deltas;
use banshee_core::pressure::episode::{AlertEpisode, EpisodeState};
use banshee_core::pressure::headroom::{ConditionStatus, Headroom};
use banshee_core::sample::{Rollup, Sample};
use banshee_core::{Pressure, default_api_url};
use clap::{Parser, Subcommand};

const EXIT_SERVER_ERROR: i32 = 1;
const EXIT_NOT_FOUND: i32 = 3;
const EXIT_NO_CONNECTION: i32 = 4;

#[derive(Parser)]
#[command(
    name = "banshee",
    version,
    about = "Banshee command-line client — a proactive system-pressure sentinel"
)]
struct Cli {
    /// API base URL (default: BANSHEE_API_URL or http://127.0.0.1:18769)
    #[arg(long, global = true)]
    api_url: Option<String>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// The verdict: severity, dominant source, per-dimension readings, worklist
    Status {
        /// Emit the raw server JSON instead of a rendered report
        #[arg(long)]
        json: bool,
        /// Show every dimension, including the ones that are fine
        #[arg(long)]
        all: bool,
    },
    /// Should an agent start new work? The verdict reduced to a decision:
    /// wait or go, how many more workers, when to ask again, and why
    /// (Kubernetes-style conditions). Advice, not enforcement (ADR-0010).
    Headroom {
        /// Emit the raw server JSON instead of a rendered report
        #[arg(long)]
        json: bool,
    },
    /// What CHANGED versus a lookback: freed and grown consumers, per-dimension
    /// movement, and the verdict at both ends — so "why did freeing 4 GB not
    /// help" has an answer. Composed in core (ADR-0005).
    Deltas {
        /// Lookback: 30m, 1h, 24h, 90s, bare seconds, or `episode` (since the
        /// earliest open incident began). Omitted = the default (1h).
        #[arg(long)]
        vs: Option<String>,
        /// Emit the raw server JSON instead of a rendered report
        #[arg(long)]
        json: bool,
    },
    /// Raw samples from the time series, oldest first
    Samples {
        /// Trailing samples to return (1–500; omitted = the last 15 minutes)
        #[arg(long)]
        limit: Option<u32>,
        /// Window start, e.g. 2026-08-31T15:04:05Z (cannot be combined with --limit)
        #[arg(long)]
        from: Option<String>,
        /// Window end; defaults to now. Requires --from
        #[arg(long)]
        to: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Five-minute rollups: history from the last closed bucket back 30 days
    Rollups {
        /// Trailing buckets to return (1–500; omitted = the last five hours)
        #[arg(long)]
        limit: Option<u32>,
        /// Window start, e.g. 2026-08-31T15:04:05Z (cannot be combined with --limit)
        #[arg(long)]
        from: Option<String>,
        /// Window end; defaults to now. Requires --from
        #[arg(long)]
        to: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// The most recent census: sessions, orphans, app groups, managed agents
    Census {
        #[arg(long)]
        json: bool,
    },
    /// Alert episodes (newest first): one row per incident, with its peak and
    /// how many re-fires it absorbed. Open episodes are always included.
    Alerts {
        /// How far back to look, e.g. 2026-08-30T00:00:00Z (default: 24 hours)
        #[arg(long)]
        since: Option<String>,
        /// Maximum episodes to return (1–500)
        #[arg(long)]
        limit: Option<u32>,
        #[arg(long)]
        json: bool,
    },
    /// What Banshee costs: row counts, database size, schema version
    Stats {
        #[arg(long)]
        json: bool,
    },
    /// Check that the API server is reachable and healthy
    Health {
        #[arg(long)]
        json: bool,
    },
    /// Reap stale agent sessions (dry-run unless --execute). Skips busy panes
    /// and dirty git worktrees; targets are recomputed at execute time.
    ReapStaleSessions {
        /// Actually kill the reap candidates. Without this the command only
        /// previews — it reports what WOULD be reaped and touches nothing.
        #[arg(long)]
        execute: bool,
        #[arg(long)]
        json: bool,
    },
    /// Reap orphaned helper processes (dry-run unless --execute). The kill list
    /// is recomputed at execute time, never taken from the preview — PIDs recycle.
    ReapOrphans {
        /// Actually terminate the orphans (SIGTERM, grace period, then SIGKILL
        /// survivors). Without this the command only previews.
        #[arg(long)]
        execute: bool,
        #[arg(long)]
        json: bool,
    },
    /// Take a VERIFIED database snapshot into the daemon's backups/ directory.
    /// The daemon chooses a timestamped path (it owns the file, ADR-0001) and
    /// verifies the copy out-of-place before reporting success. Prints where it
    /// landed. Copy it to external media yourself for off-machine safety.
    Backup {
        #[arg(long)]
        json: bool,
    },
}

struct Client {
    transport: Transport,
    key: String,
}

/// How this invocation talks to the daemon (ADR-0008).
///
/// The Unix socket is the default and the only transport the file-loaded key travels
/// over. TCP survives for an explicitly requested `--api-url` / `BANSHEE_API_URL`, and
/// carries only a key the caller supplied themselves.
enum Transport {
    Unix(banshee_core::uds_http::UdsHttp),
    Tcp {
        base: String,
        http: reqwest::blocking::Client,
    },
}

impl Transport {
    fn tcp(url: String) -> Self {
        Transport::Tcp {
            base: url.trim_end_matches('/').to_string(),
            // A hung API must not stall a script/agent indefinitely (rust M4).
            http: reqwest::blocking::Client::builder()
                // Loopback-only: never route the keyed request through a system/env
                // HTTP proxy, which would carry X-Api-Key off the machine (banshee-4d0).
                .no_proxy()
                // Never follow a redirect: reqwest keeps custom headers across a 3xx,
                // so a redirect could carry X-Api-Key to another host (banshee-4d0).
                .redirect(reqwest::redirect::Policy::none())
                .timeout(std::time::Duration::from_secs(30))
                .connect_timeout(std::time::Duration::from_secs(5))
                .build()
                // Fallback must stay hardened: a bare Client::new() would restore
                // system-proxy + redirect-following (fail-open).
                .unwrap_or_else(|_| {
                    reqwest::blocking::Client::builder()
                        .no_proxy()
                        .redirect(reqwest::redirect::Policy::none())
                        .build()
                        .expect("hardened reqwest client")
                }),
        }
    }

    /// What to name in an error message.
    fn describe(&self) -> String {
        match self {
            Transport::Unix(c) => format!("unix://{}", c.path().display()),
            Transport::Tcp { base, .. } => base.clone(),
        }
    }
}

/// The socket to use, or `None` when there is none to use.
///
/// `BANSHEE_API_SOCKET` overrides; otherwise the daemon's default path, and only if it
/// EXISTS — a path that is merely configured is not a daemon, and reporting "cannot
/// connect" for a socket nobody ever created would be worse than falling back.
fn resolve_socket_path() -> Option<std::path::PathBuf> {
    if let Ok(p) = std::env::var(banshee_core::API_SOCKET_ENV) {
        let path = std::path::PathBuf::from(p);
        return if path.exists() { Some(path) } else { None };
    }
    let path = dirs::config_dir()?
        .join("banshee")
        .join(banshee_core::API_SOCKET_FILE);
    path.exists().then_some(path)
}

/// Decide which key, if any, this transport may carry (ADR-0008).
///
/// Extracted from `Client::new` so the decision can be TESTED without setting process
/// environment variables — which is racy across parallel tests and would make the one
/// security-critical branch here the hardest thing in the file to exercise.
///
/// `load_file_key` is injected for the same reason: the test must be able to tell
/// "withheld the key" from "there was no key file", and those are indistinguishable if
/// the function reads the real filesystem.
fn resolve_key(
    transport: &Transport,
    explicit: Option<String>,
    load_file_key: impl Fn() -> String,
) -> String {
    match (transport, explicit) {
        // The user pinned a key; it is theirs to spend wherever they point the client.
        (_, Some(k)) => k,
        (Transport::Unix(_), None) => load_file_key(),
        // **The file-loaded key never goes over TCP** — not even to loopback. Any local
        // user can bind 127.0.0.1:18769 before the daemon starts, and this key
        // authorises the reap/kill routes, so "the host is loopback" was never evidence
        // about who is listening.
        (Transport::Tcp { base, .. }, None) => {
            eprintln!(
                "banshee: not sending the local API key over TCP to {base} (ADR-0008); \
                 authenticated reads need the daemon's socket, and the daemon refuses \
                 every key over TCP anyway — run `make daemon-install` to update it, or \
                 unset BANSHEE_API_URL."
            );
            String::new()
        }
    }
}

/// The key from the key file, or empty when there is none to read.
fn read_key_file() -> String {
    std::env::var("BANSHEE_KEY_FILE")
        .map(std::path::PathBuf::from)
        .ok()
        .or_else(|| dirs::config_dir().map(|d| d.join("banshee/api_key")))
        .and_then(|p| std::fs::read_to_string(p).ok())
        .map(|k| k.trim().to_string())
        .unwrap_or_default()
}

impl Client {
    fn new(api_url: Option<String>) -> Self {
        // An explicitly requested URL wins: the caller asked for TCP, and may be
        // pointing at a tunnel or a test server.
        let explicit_url = api_url.or_else(|| std::env::var("BANSHEE_API_URL").ok());
        let socket = resolve_socket_path();

        // ADR-0008. The file-loaded key authorizes the reap/kill routes, and it is now
        // attached ONLY over the Unix socket — never over TCP, not even to loopback.
        //
        // Loopback was never a sufficient gate: ANY local user can bind 127.0.0.1:18769
        // before the daemon starts, and every client would then hand its key to them.
        // A socket file cannot be taken over that way — it lives in a directory only
        // this user can write, and the kernel enforces its 0600 mode (verified by
        // execution: chmod 000 denies connect even to the owner). So the transport, not
        // the hostname, is what decides whether the key may travel.
        //
        // An explicit BANSHEE_API_KEY is untouched by this: the user chose it, and it is
        // their credential to spend where they like.
        let explicit_key = std::env::var("BANSHEE_API_KEY")
            .ok()
            .map(|k| k.trim().to_string());

        let transport = match (explicit_url, socket) {
            // Asked for a URL: honour it, and carry only an explicit key.
            (Some(url), _) => Transport::tcp(url),
            // Default path: the socket, when the daemon has one.
            (None, Some(path)) => Transport::Unix(banshee_core::uds_http::UdsHttp::new(
                path,
                std::time::Duration::from_secs(30),
            )),
            // No socket and no URL: the daemon is either not running or predates the
            // socket. Fall back to the default URL so `banshee health` still reports
            // something useful, but WITHOUT the file key.
            (None, None) => Transport::tcp(default_api_url()),
        };

        let key = resolve_key(&transport, explicit_key, read_key_file);
        Self { transport, key }
    }

    /// Turn a (status, body) pair into JSON, or exit with a controlled diagnostic.
    /// Falls back to `status + raw body` when the body is NOT JSON, so a text/plain
    /// error from the API (or any proxy in front of it) is never swallowed into a
    /// misleading "invalid JSON" (agentapi C1).
    fn handle_response(&self, status: u16, text: String) -> Body {
        let success = (200..300).contains(&status);
        let exit_for = |status: u16| {
            if status == 404 {
                EXIT_NOT_FOUND
            } else {
                EXIT_SERVER_ERROR
            }
        };
        match serde_json::from_str::<serde_json::Value>(&text) {
            Ok(value) => {
                if !success {
                    let msg = value["error"].as_str().unwrap_or("unknown error");
                    eprintln!("banshee: {msg} ({status})");
                    if status == 401 {
                        eprintln!(
                            "banshee: the daemon rejected this key. If it rotated one, \
                             re-run; if you are on TCP, authenticated reads need the \
                             socket (ADR-0008)."
                        );
                    }
                    std::process::exit(exit_for(status));
                }
                // The parse happened only to validate the body and to read an
                // `error` field; what is CARRIED is the server's own bytes.
                Body { text }
            }
            Err(_) => {
                let body = text.trim();
                if !success {
                    let shown = if body.is_empty() {
                        "<empty response body>"
                    } else {
                        body
                    };
                    eprintln!("banshee: {shown} ({status})");
                    std::process::exit(exit_for(status));
                }
                eprintln!("banshee: invalid JSON from server: {body}");
                std::process::exit(EXIT_SERVER_ERROR);
            }
        }
    }

    /// Send over whichever transport this invocation resolved to.
    fn send(&self, method: &str, target: &str) -> Body {
        let (status, text) = match &self.transport {
            Transport::Unix(client) => match client.request(method, target, &self.key, None) {
                Ok(r) => (r.status, r.body),
                Err(e) => {
                    eprintln!("banshee: {e}");
                    eprintln!("banshee: is the daemon running? (make daemon-status)");
                    std::process::exit(EXIT_NO_CONNECTION);
                }
            },
            Transport::Tcp { base, http } => {
                let url = format!("{base}{target}");
                let req = if method == "POST" {
                    http.post(url)
                } else {
                    http.get(url)
                };
                let req = if self.key.is_empty() {
                    req
                } else {
                    req.header("x-api-key", &self.key)
                };
                let resp = req.send().unwrap_or_else(|e| {
                    eprintln!(
                        "banshee: cannot reach API at {}: {e}",
                        self.transport.describe()
                    );
                    eprintln!("banshee: is the server running? (make start)");
                    std::process::exit(EXIT_NO_CONNECTION);
                });
                let status = resp.status().as_u16();
                let text = resp.text().unwrap_or_else(|e| {
                    eprintln!(
                        "banshee: cannot read response body from {}: {e}",
                        self.transport.describe()
                    );
                    std::process::exit(EXIT_SERVER_ERROR);
                });
                (status, text)
            }
        };
        self.handle_response(status, text)
    }

    /// GET `path`, with `params` appended as a query string.
    fn get(&self, path: &str, params: &[(&str, String)]) -> Body {
        let mut target = path.to_string();
        if !params.is_empty() {
            let query: Vec<String> = params
                .iter()
                .map(|(k, v)| format!("{k}={}", urlencode(v)))
                .collect();
            target.push('?');
            target.push_str(&query.join("&"));
        }
        self.send("GET", &target)
    }

    /// POST `path` with no body. The action routes take no parameters (targets
    /// are recomputed server-side), so the CLI has nothing to send.
    fn post(&self, path: &str) -> Body {
        self.send("POST", path)
    }
}

/// A successful response, kept as the SERVER'S OWN BYTES.
///
/// `--json` prints `text` verbatim rather than re-serializing a parsed value, and
/// that is not tidiness — it is correctness. Parsing a float and printing it again
/// is not a round trip: `serde_json` reads `15640.710000000001` back one unit in
/// the last place lower and then emits `15640.71`. The parity harness caught
/// exactly that on a live census. A `--json` that silently perturbs the last bit
/// of every float feeds a wrong number into any `jq` pipeline or re-aggregation
/// downstream, and it is a difference no client could see by inspection.
struct Body {
    text: String,
}

impl Body {
    /// Decode through the SHARED core type, from the raw bytes.
    fn decode<T: serde::de::DeserializeOwned>(&self, what: &str) -> T {
        serde_json::from_str(&self.text).unwrap_or_else(|e| {
            eprintln!(
                "banshee: server returned {what} this client cannot parse \
                 (wire-format skew? upgrade the client): {e}"
            );
            std::process::exit(EXIT_SERVER_ERROR);
        })
    }

    /// A loosely-typed view, for the two places that render a payload with no
    /// core type of its own (`/stats`, `/health`).
    fn value(&self) -> serde_json::Value {
        serde_json::from_str(&self.text).unwrap_or(serde_json::Value::Null)
    }
}

/// Percent-encode the characters that appear in Banshee's parameters and are not
/// query-safe. Datetimes carry `:` and `+`, and an unencoded `+` decodes to a
/// SPACE server-side — which turned `…+01:00` into a 400 on a value the wire
/// contract explicitly promises to accept.
fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Print the server's JSON, UNCHANGED. **The bytes are the contract**: `--json`
/// output must be byte-identical to the raw HTTP body and to the MCP tool result,
/// which `tests/e2e/run-e2e.sh` checks for every surface. Pipe it through `jq` for
/// pretty-printing; do not re-serialize it here (see `Body`).
fn emit_json(body: &Body) {
    println!("{}", body.text.trim_end());
}

// --- Rendering -------------------------------------------------------------

/// The band marker. Vocabulary and layout belong to the client (ADR-0005); the
/// BAND itself came off the wire, so nothing is being re-derived here.
fn band_marker(band: Band) -> &'static str {
    match band {
        Band::Green => "  ",
        Band::Yellow => "! ",
        Band::Red => "!!",
    }
}

fn print_status(p: &Pressure, show_all: bool) {
    print!("{}", render_status(p, show_all));
}

/// Build the human `status` view as a string. Split out of `print_status` so the
/// wording — the verbatim glyph/label, the held time, the "N gap before" clause,
/// the pending marker, the findings worklist — is golden-testable without
/// capturing stdout (banshee-e8p). `print_status` is now only the `print!`.
fn render_status(p: &Pressure, show_all: bool) -> String {
    use std::fmt::Write as _;
    let mut s = String::new();
    // The glyph and the label are rendered VERBATIM. Recomposing either from the
    // level would be the local re-derivation ADR-0005 forbids, and the whole point
    // of the glyph is that it means the same thing on every surface.
    let _ = writeln!(s, "{}  {}", p.glyph, p.accessibility_label);

    if p.level == banshee_core::pressure::level::Level::Checking {
        let _ = writeln!(s);
        let _ = writeln!(
            s,
            "  Not enough data yet — the sampler needs a few readings."
        );
        let _ = writeln!(s, "  This is not a claim that the machine is calm.");
        return s;
    }

    let width = p
        .dimensions
        .iter()
        .filter(|r| show_all || r.band > Band::Green || r.recovering)
        .map(|r| r.label.chars().count())
        .max()
        .unwrap_or(0);

    let _ = writeln!(s);
    let mut shown = 0;
    for r in &p.dimensions {
        // A green dimension is hidden by default — unless it is RECOVERING, which
        // is the one green worth seeing: it was red a moment ago (ADR-0009).
        if !show_all && r.band == Band::Green && !r.recovering {
            continue;
        }
        shown += 1;
        let _ = write!(
            s,
            "  {} {:width$}  {}",
            band_marker(r.band),
            r.label,
            r.detail,
            width = width
        );
        if r.band > Band::Green {
            let _ = write!(s, "  (held {})", fmt_secs(r.held_secs));
            // Say when the held time is bounded by a HOLE rather than by the
            // machine recovering. "red for 45s" and "red for 45s, and nobody was
            // watching for the 17 minutes before that" are different claims, and
            // rounding the second up to the first is what produced a false 💀.
            if let Some(gap) = r.observation_gap_secs {
                let _ = write!(s, ", {} gap before", fmt_secs(gap));
            }
        }
        if let Some(pending) = r.pending {
            // A pending band is the honest "this is about to change" signal, and
            // hiding it makes hysteresis look like lag.
            let _ = write!(s, "  [→ {pending:?}]");
        }
        if r.recovering {
            // Below red, but inside its episode's down window: not over yet.
            let _ = write!(s, "  [recovering]");
        }
        let _ = writeln!(s);
    }
    if shown == 0 {
        let _ = writeln!(s, "  Nothing is out of band.");
    }

    if !p.findings.is_empty() {
        let _ = writeln!(s);
        let _ = writeln!(s, "Findings, best action first:");
        for (i, f) in p.findings.iter().enumerate() {
            let _ = writeln!(s, "  {}. {}", i + 1, f.message);
            // Who is behind it (the who-line): the server's line, verbatim. Composed in
            // core so this, the popover and the alert text name the same names.
            if let Some(who) = &f.who_line {
                let _ = writeln!(s, "     {who}");
            }
            if f.action != banshee_core::pressure::Action::None {
                // The wording the SERVER chose, not `f.action.label()` — even
                // though this client links core and the two are the same string
                // today. Reading the field is what keeps every surface honest: the
                // Swift app cannot call a Rust method, so if the label were not on
                // the wire it would have to duplicate the table (ADR-0005).
                let _ = writeln!(s, "     → {}", f.action_label);
            }
        }
    }

    let _ = writeln!(s);
    let _ = writeln!(
        s,
        "  {} samples, {} censuses in the window",
        p.sample_count, p.census_count
    );
    // The long memory, where operators look (banshee-qoz): a point-in-time
    // verdict hid a 12-alert afternoon from the assistant reading it.
    let a = &p.activity;
    let _ = writeln!(
        s,
        "  {} open episode{}, {} active in the last hour, {} in 24h  (banshee alerts)",
        a.open,
        if a.open == 1 { "" } else { "s" },
        a.last_hour,
        a.last_day
    );
    s
}

/// The human `headroom` view. The DECISION first, in the server's own words —
/// `reason` is composed in core so this, the MCP tool and the popover say the
/// same sentence (ADR-0005) — then the conditions as a table an operator can
/// scan the way they scan `kubectl describe node`. Split out so it is
/// golden-testable without capturing stdout.
fn render_headroom(h: &Headroom) -> String {
    use std::fmt::Write as _;
    let mut s = String::new();
    // WAIT / GO is the one thing a script or a person needs at a glance; the
    // words come from `should_wait`, never re-derived from the level.
    let verdict = if h.should_wait { "WAIT" } else { "GO" };
    let _ = writeln!(s, "{verdict}  {}", h.reason);
    let _ = writeln!(s);
    let _ = writeln!(
        s,
        "  recommended parallelism  {}",
        h.recommended_parallelism
    );
    match h.retry_after_secs {
        Some(secs) => {
            let _ = writeln!(s, "  retry after              {}", fmt_secs(secs));
        }
        None => {
            let _ = writeln!(s, "  retry after              —");
        }
    }
    let _ = writeln!(s);
    let width = h
        .conditions
        .iter()
        .map(|c| c.kind.chars().count())
        .max()
        .unwrap_or(0);
    for c in &h.conditions {
        // `Ready` has the opposite polarity to the pressure conditions: True is
        // good news there and bad news everywhere else (Kubernetes' own
        // convention). The mark follows the NEWS, the status column the wire.
        let mark = match (c.kind.as_str(), c.status) {
            (_, ConditionStatus::Unknown) => "?",
            ("Ready", ConditionStatus::True) => " ",
            ("Ready", ConditionStatus::False) => "!",
            (_, ConditionStatus::True) => "!",
            (_, ConditionStatus::False) => " ",
        };
        let _ = writeln!(
            s,
            "  {mark} {:width$}  {:<8} {:<18} {}",
            c.kind,
            format!("{:?}", c.status),
            c.reason,
            c.message,
            width = width
        );
    }
    let _ = writeln!(s);
    let _ = writeln!(
        s,
        "  Advice, not enforcement: Banshee informs, your human decides (ADR-0010)."
    );
    s
}

/// The human `deltas` view. The SENTENCE first — the composed answer to "did it
/// help" (`summary`, from core) — then the movers and dimensions as tables an
/// operator can scan, then the verdict, then the gap warning when the arithmetic
/// spans a hole. Every string here comes from the wire; this function ranks and
/// aligns, it does not compose words. Split out so it is golden-testable without
/// capturing stdout.
fn render_deltas(d: &Deltas) -> String {
    use std::fmt::Write as _;
    let mut s = String::new();
    let _ = writeln!(s, "{}", d.summary);
    let _ = writeln!(s);

    if let Some(v) = &d.verdict {
        let _ = writeln!(s, "  {}", v.detail);
        let _ = writeln!(s);
    }

    if !d.consumers.is_empty() {
        let _ = writeln!(s, "  what moved (by change):");
        for c in &d.consumers {
            let _ = writeln!(s, "    {}", c.detail);
        }
        let _ = writeln!(s);
    }

    if !d.dimensions.is_empty() {
        let _ = writeln!(s, "  dimensions:");
        for dim in &d.dimensions {
            // A dimension whose two ends span a hole is flagged inline: the number
            // is still true but nobody was watching between the readings.
            let flag = match dim.observation_gap_secs {
                Some(g) => format!("  (gap {})", fmt_secs(g)),
                None => String::new(),
            };
            let _ = writeln!(s, "    {}{}", dim.detail, flag);
        }
        let _ = writeln!(s);
    }

    if let Some(g) = d.observation_gap_secs {
        let _ = writeln!(
            s,
            "  ⚠ not observed for {} in between — this comparison spans a hole.",
            fmt_secs(g)
        );
    }
    s
}

fn fmt_secs(secs: u64) -> String {
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m", secs / 60)
    } else if secs < 86_400 {
        format!("{}h{}m", secs / 3600, (secs % 3600) / 60)
    } else {
        format!("{}d", secs / 86_400)
    }
}

fn fmt_bytes(b: u64) -> String {
    let b = b as f64;
    if b >= 1e9 {
        format!("{:.1} GB", b / 1e9)
    } else if b >= 1e6 {
        format!("{:.0} MB", b / 1e6)
    } else if b >= 1e3 {
        format!("{:.0} KB", b / 1e3)
    } else {
        format!("{b:.0} B")
    }
}

fn print_samples(samples: &[Sample]) {
    if samples.is_empty() {
        println!("no samples yet");
        return;
    }
    println!(
        "{:<20}  {:>9}  {:>10}  {:>10}  {:>8}",
        "sampled at", "load/core", "avail", "swap", "swap ops"
    );
    for (i, s) in samples.iter().enumerate() {
        let reading = s.to_reading();
        // Swap ops is a RATE and needs the previous sample; the first row has no
        // predecessor, so it reports "—" rather than a zero that looks like calm.
        let ops = if i == 0 {
            "—".to_string()
        } else {
            let prev = &samples[i - 1];
            let elapsed = (s.sampled_at - prev.sampled_at).num_milliseconds() as f64 / 1000.0;
            match banshee_core::rates::counter_rates(&prev.to_reading(), &reading, elapsed) {
                Ok(r) => format!("{:.0}", r.swapins_per_sec + r.swapouts_per_sec),
                // A reboot or a backwards clock between the pair, which has no
                // valid delta. Saying so beats printing a spike.
                Err(_) => "—".to_string(),
            }
        };
        println!(
            "{:<20}  {:>9.2}  {:>10}  {:>10}  {:>8}",
            s.sampled_at.format("%Y-%m-%d %H:%M:%S"),
            reading.load_per_core_1m(),
            // Available, not the free list — the number the pressure model
            // bands on. A column that disagreed with the
            // verdict would send someone chasing the wrong figure.
            fmt_bytes(reading.available_bytes()),
            fmt_bytes(s.swap_used_bytes),
            ops
        );
    }
}

fn print_rollups(rollups: &[Rollup]) {
    if rollups.is_empty() {
        println!(
            "no rollups yet (the first appears within five minutes of the daemon starting; each bucket is summarised as it closes)"
        );
        return;
    }
    println!(
        "{:<20}  {:>4}  {:>9}  {:>9}  {:>10}  {:>10}  {:>8}  {:>5}  {:>5}  {:>6}  {:>5}",
        "bucket",
        "n",
        "load avg",
        "load max",
        "free min",
        "swap avg",
        "swap ops",
        "stale",
        "orph",
        "mon%",
        "procs"
    );
    for r in rollups {
        // A rollup's swap-op counts are DELTAS across the bucket and are None when
        // a reboot inside it voided them — "—" rather than 0, because zero swap
        // activity and "we cannot know" are different facts.
        let ops = match (r.swapins_delta, r.swapouts_delta) {
            (Some(i), Some(o)) => format!("{}", i + o),
            _ => "—".to_string(),
        };
        // Same rule for the census scalars: a bucket with no census in it says
        // "—", never 0 — zero orphans is a claim, a dash is "nobody looked".
        let opt = |v: Option<u32>| v.map_or("—".to_string(), |n| n.to_string());
        let mon = r
            .monitor_percent_max
            .map_or("—".to_string(), |p| format!("{p:.1}"));
        println!(
            "{:<20}  {:>4}  {:>9.2}  {:>9.2}  {:>10}  {:>10}  {:>8}  {:>5}  {:>5}  {:>6}  {:>5}",
            r.bucket_start.format("%Y-%m-%d %H:%M:%S"),
            r.sample_count,
            r.load1m_avg,
            r.load1m_max,
            // pages_free_min is in PAGES; the page size lives on the raw sample, not
            // on the rollup, so this reports pages rather than inventing a size.
            format!("{} pg", r.pages_free_min),
            fmt_bytes(r.swap_used_avg),
            ops,
            opt(r.stale_sessions_max),
            opt(r.orphans_max),
            mon,
            opt(r.total_procs_max)
        );
    }
}

fn print_census(c: &Census) {
    println!(
        "census at {}  ({} processes)",
        c.taken_at.format("%Y-%m-%d %H:%M:%S"),
        c.total_procs
    );
    println!();
    let stale = c.stale_sessions().count();
    println!(
        "  agent sessions   {} ({stale} stale)",
        c.agent_sessions.len()
    );
    for s in c.stale_sessions() {
        println!(
            "     pid {:<7} {:<24} {:>9}  {} days  {}",
            s.pid,
            s.program,
            fmt_bytes(s.rss_bytes),
            s.age_days,
            s.cwd.as_deref().unwrap_or("")
        );
    }
    println!(
        "  orphaned procs   {} of {} ({})",
        c.orphans.orphan_count,
        c.orphans.total_count,
        fmt_bytes(c.orphans.orphan_rss_bytes)
    );
    for p in &c.orphans.orphans_by_program {
        println!(
            "     {:<28} {:>3}  {:>9}",
            p.program,
            p.count,
            fmt_bytes(p.rss_bytes)
        );
    }
    if c.tmux_available {
        println!("  tmux sessions    {}", c.tmux_sessions.len());
        for t in &c.tmux_sessions {
            let idle = t
                .idle_days
                .map(|d| format!("{d:.1} days idle"))
                .unwrap_or_else(|| "idle unknown".into());
            println!(
                "     {:<28} {:<12} {idle}{}",
                t.name,
                t.pane_command,
                if t.is_stale { "  [stale]" } else { "" }
            );
        }
    } else {
        // NOT the same as zero sessions. A census that could not look must never
        // read as a clean machine.
        println!("  tmux sessions    unavailable (tmux not installed or not running)");
    }
    println!("  app groups");
    for g in &c.app_groups {
        println!(
            "     {:<28} {:>3} procs  {:>9}",
            g.name,
            g.proc_count,
            fmt_bytes(g.rss_bytes)
        );
    }
    println!(
        "  managed agents   {:.1}% of one core, {} procs, {} (oldest {})",
        c.monitor_total.percent_of_one_core,
        c.monitor_total.proc_count,
        fmt_bytes(c.monitor_total.rss_bytes),
        fmt_secs(c.monitor_total.longest_life_secs)
    );
    // NEVER sum the per-agent percentages: each divides by the longest-lived
    // process in its OWN group, so the denominators differ. `monitorTotal` is the
    // deduplicated figure against one global denominator.
    for a in &c.monitor_agents {
        println!(
            "     {:<28} {:>3} procs  {:>9}  {:>5.1}% (own denominator)",
            a.name,
            a.proc_count,
            fmt_bytes(a.rss_bytes),
            a.percent_of_one_core
        );
    }
}

fn print_alerts(episodes: &[AlertEpisode]) {
    print!("{}", render_alerts(episodes));
}

/// The human `alerts` view: one line per EPISODE — when it started, how long it
/// ran (or that it is still open), the dimension, the peak level, how many
/// re-fires it absorbed, and the peak's wording. Split out so it is
/// golden-testable without capturing stdout.
fn render_alerts(episodes: &[AlertEpisode]) -> String {
    use std::fmt::Write as _;
    let mut s = String::new();
    if episodes.is_empty() {
        let _ = writeln!(s, "no alert episodes in the window");
        return s;
    }
    for e in episodes {
        let span = match (e.state, e.ended_at) {
            (EpisodeState::Closed, Some(end)) => format!(
                "ran {}",
                fmt_secs((end - e.started_at).num_seconds().max(0) as u64)
            ),
            (EpisodeState::Recovering, _) => "recovering".to_string(),
            _ => "OPEN".to_string(),
        };
        // "+N more": the re-fires the episode absorbed, each of which the old
        // one-per-hour model would have swallowed without a trace (banshee-nvd).
        let more = if e.suppressed > 0 {
            format!("  +{} more", e.suppressed)
        } else {
            String::new()
        };
        // Who was behind it at the peak (the who-line), when the finding named anyone.
        let who = e
            .peak
            .who_line
            .as_deref()
            .map(|w| format!("  ({w})"))
            .unwrap_or_default();
        let _ = writeln!(
            s,
            "{}  {:<11} {:<14} peak {:<9} {}{}{}",
            e.started_at.format("%Y-%m-%d %H:%M:%S"),
            span,
            e.dimension.key(),
            format!("{:?}", e.peak.level).to_lowercase(),
            e.peak.message,
            who,
            more
        );
    }
    s
}

fn print_stats(v: &serde_json::Value) {
    let n = |k: &str| v[k].as_u64().unwrap_or(0);
    println!("  schema version   v{}", n("schemaVersion"));
    println!("  samples          {}", n("samples"));
    println!("  rollups          {}", n("rollups"));
    println!("  censuses         {}", n("censuses"));
    println!("  alert episodes   {}", n("alerts"));
    println!("  database         {}", fmt_bytes(n("dbSizeBytes")));
    // The file settles at its high-water mark; SQLite reuses freed pages rather
    // than returning them to the OS. Saying so pre-empts the "the sweep isn't
    // working" report.
    println!("  (size is the high-water mark — SQLite reuses freed pages)");

    // What the daemon itself costs: footprint, and CPU as the lifetime
    // average over its uptime — the sustained figure, not a burst reading.
    let pct = v["selfCpuPercent"].as_f64().unwrap_or(0.0);
    let uptime = v["selfUptimeSecs"].as_f64().unwrap_or(0.0) as u64;
    println!(
        "  banshee itself   {} footprint, {pct:.2}% of one core over {}",
        fmt_bytes(n("selfFootprintBytes")),
        fmt_secs(uptime)
    );

    // Sampler operational health (banshee-4r1): a stuck evaluation or a failing
    // sweep is otherwise invisible outside the log. `—` when nothing has run yet.
    let when = |k: &str| v[k].as_str().unwrap_or("—");
    println!("  last evaluated   {}", when("lastEvaluatedAt"));
    println!("  last swept       {}", when("lastSweptAt"));
    let eval_fail = n("consecutiveEvalFailures");
    let sweep_fail = n("consecutiveSweepFailures");
    if eval_fail > 0 || sweep_fail > 0 {
        // Loud only when there is something wrong — a healthy daemon says nothing
        // here, so a non-empty line is a signal, not noise.
        println!(
            "  ⚠ sampler health  {eval_fail} eval failure(s), {sweep_fail} sweep failure(s) since last success"
        );
    }
}

fn print_session_reap(r: &SessionReapReport) {
    let verb = if r.executed { "reaped" } else { "would reap" };
    if !r.tmux_available {
        // "Could not look" is not "nothing to reap" — the census's rule, and
        // an action that killed nothing because tmux was unreachable must not
        // read as a clean machine.
        println!("tmux unavailable — could not inspect sessions (nothing was reaped)");
        return;
    }
    let reap: Vec<&_> = r
        .candidates
        .iter()
        .filter(|c| c.verdict == SessionVerdict::Reap)
        .collect();
    let spared = r.candidates.len() - reap.len();
    println!(
        "reap stale sessions ({verb}): {} candidate(s), {spared} spared  [idle ≥ {} days]",
        reap.len(),
        r.stale_days
    );
    for c in &r.candidates {
        let idle = c
            .idle_days
            .map(|d| format!("{d:.1}d idle"))
            .unwrap_or_else(|| "idle unknown".into());
        let tag = match c.verdict {
            SessionVerdict::Reap => c.outcome.as_deref().unwrap_or("reap"),
            SessionVerdict::Spare => "spare",
        };
        println!("  {:<8} {:<24} {idle:>14}   {}", tag, c.session, c.reason);
    }
    if r.executed {
        // Killing a session orphans its MCP servers, so the ranked worklist
        // pairs the two (docs/signal-collection.md).
        println!();
        println!("  Sessions orphan their helpers when killed — consider `banshee reap-orphans`.");
    }
}

fn print_orphan_reap(r: &OrphanReapReport) {
    let verb = if r.executed { "reaped" } else { "would reap" };
    println!(
        "reap orphans ({verb}): {} orphaned helper(s)  [SIGTERM, {}s grace, then SIGKILL]",
        r.candidates.len(),
        r.term_wait_secs
    );
    for c in &r.candidates {
        let tag = c.outcome.as_deref().unwrap_or("orphan");
        println!(
            "  {:<11} pid {:<7} {:<28} {:>9}  {}",
            tag,
            c.pid,
            c.program,
            fmt_bytes(c.rss_bytes),
            fmt_secs(c.age_secs)
        );
    }
    if r.candidates.is_empty() {
        println!("  No orphaned helpers.");
    }
}

fn main() {
    let cli = Cli::parse();
    let client = Client::new(cli.api_url);

    match cli.command {
        Command::Health { json } => {
            let body = client.get("/health", &[]);
            if json {
                emit_json(&body);
            } else {
                let v = body.value();
                // `status` is "degraded" when the datastore is unusable, which is
                // a 503 the client has already surfaced; printing it plainly here
                // keeps `banshee health` readable without --json.
                //
                // `gitRev` is the identity of the RUNNING daemon; the crate
                // version deliberately does not move per release (VERSIONING.md),
                // so the rev is what answers "is my latest install serving?".
                println!(
                    "{} (banshee-api {}, rev {})",
                    v["status"].as_str().unwrap_or("unknown"),
                    v["version"].as_str().unwrap_or("unknown"),
                    v["gitRev"].as_str().unwrap_or("unknown")
                );
            }
        }
        Command::Status { json, all } => {
            let body = client.get("/pressure", &[]);
            if json {
                emit_json(&body);
            } else {
                print_status(&body.decode::<Pressure>("a verdict"), all);
            }
        }
        Command::Headroom { json } => {
            let body = client.get("/headroom", &[]);
            if json {
                emit_json(&body);
            } else {
                print!(
                    "{}",
                    render_headroom(&body.decode::<Headroom>("a headroom decision"))
                );
            }
        }
        Command::Deltas { vs, json } => {
            let mut params: Vec<(&str, String)> = Vec::new();
            if let Some(v) = vs {
                params.push(("vs", v));
            }
            let body = client.get("/deltas", &params);
            if json {
                emit_json(&body);
            } else {
                print!(
                    "{}",
                    render_deltas(&body.decode::<Deltas>("a deltas report"))
                );
            }
        }
        Command::Samples {
            limit,
            from,
            to,
            json,
        } => {
            let mut params: Vec<(&str, String)> = Vec::new();
            if let Some(l) = limit {
                params.push(("limit", l.to_string()));
            }
            if let Some(f) = from {
                params.push(("from", f));
            }
            if let Some(t) = to {
                params.push(("to", t));
            }
            let body = client.get("/samples", &params);
            if json {
                emit_json(&body);
            } else {
                print_samples(&body.decode::<Vec<Sample>>("samples"));
            }
        }
        Command::Rollups {
            limit,
            from,
            to,
            json,
        } => {
            let mut params: Vec<(&str, String)> = Vec::new();
            if let Some(l) = limit {
                params.push(("limit", l.to_string()));
            }
            if let Some(f) = from {
                params.push(("from", f));
            }
            if let Some(t) = to {
                params.push(("to", t));
            }
            let body = client.get("/rollups", &params);
            if json {
                emit_json(&body);
            } else {
                print_rollups(&body.decode::<Vec<Rollup>>("rollups"));
            }
        }
        Command::Census { json } => {
            let body = client.get("/census", &[]);
            if json {
                emit_json(&body);
            } else {
                print_census(&body.decode::<Census>("a census"));
            }
        }
        Command::Alerts { since, limit, json } => {
            let mut params: Vec<(&str, String)> = Vec::new();
            if let Some(s) = since {
                params.push(("since", s));
            }
            if let Some(l) = limit {
                params.push(("limit", l.to_string()));
            }
            let body = client.get("/alerts", &params);
            if json {
                emit_json(&body);
            } else {
                print_alerts(&body.decode::<Vec<AlertEpisode>>("alert episodes"));
            }
        }
        Command::Stats { json } => {
            let body = client.get("/stats", &[]);
            if json {
                emit_json(&body);
            } else {
                print_stats(&body.value());
            }
        }
        Command::ReapStaleSessions { execute, json } => {
            let path = if execute {
                "/actions/reap-stale-sessions/execute"
            } else {
                "/actions/reap-stale-sessions/preview"
            };
            let body = client.post(path);
            if json {
                emit_json(&body);
            } else {
                print_session_reap(&body.decode::<SessionReapReport>("a reap report"));
            }
        }
        Command::ReapOrphans { execute, json } => {
            let path = if execute {
                "/actions/reap-orphans/execute"
            } else {
                "/actions/reap-orphans/preview"
            };
            let body = client.post(path);
            if json {
                emit_json(&body);
            } else {
                print_orphan_reap(&body.decode::<OrphanReapReport>("a reap report"));
            }
        }
        Command::Backup { json } => {
            let body = client.post("/backup");
            if json {
                emit_json(&body);
            } else {
                let v = body.value();
                println!(
                    "verified backup: {} ({} samples)",
                    v["path"].as_str().unwrap_or("unknown"),
                    v["samples"].as_u64().unwrap_or(0)
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    /// **The file-loaded key must never travel over TCP** (ADR-0008).
    ///
    /// The hole this closes: any local user can bind 127.0.0.1:18769 before the daemon
    /// starts, and every client would then hand them a key that authorises the
    /// reap/kill routes. "The host is loopback" was never evidence about WHO is
    /// listening, so the transport decides, not the hostname.
    ///
    /// FIXTURE NOTE: `load_file_key` returns a distinctive value rather than reading the
    /// real key file, so "withheld the key" is distinguishable from "there was no key
    /// file to read". With a real filesystem read those two are the same empty string,
    /// and the test would pass on a machine that simply had no key.
    #[test]
    fn the_file_key_is_withheld_from_a_tcp_transport() {
        let file_key = || "FILE-KEY-SHOULD-NOT-ESCAPE".to_string();

        let tcp = super::Transport::tcp("http://127.0.0.1:18769".to_string());
        assert_eq!(
            super::resolve_key(&tcp, None, file_key),
            "",
            "the file key must not be attached to a TCP transport, loopback or not"
        );

        let dir = tempfile::tempdir().unwrap();
        let unix = super::Transport::Unix(banshee_core::uds_http::UdsHttp::new(
            dir.path().join("api.sock"),
            std::time::Duration::from_secs(1),
        ));
        assert_eq!(
            super::resolve_key(&unix, None, file_key),
            "FILE-KEY-SHOULD-NOT-ESCAPE",
            "over the socket the file key is exactly what should be sent"
        );
    }

    /// An EXPLICIT `BANSHEE_API_KEY` is the caller's own credential and goes wherever
    /// they point the client — including TCP.
    ///
    /// Pairs with the test above: "never send any key over TCP" would pass that one
    /// while breaking every legitimate `--api-url` use (a tunnel, a remote daemon, the
    /// e2e harness), and the two implementations are otherwise indistinguishable.
    #[test]
    fn an_explicit_key_is_still_sent_over_tcp() {
        let tcp = super::Transport::tcp("http://example.test:9999".to_string());
        assert_eq!(
            super::resolve_key(&tcp, Some("mine".to_string()), || "file".to_string()),
            "mine"
        );
    }

    use super::*;

    /// `--json` must relay the server's bytes UNCHANGED.
    ///
    /// The float here is the one the parity harness actually caught: HTTP served
    /// `15640.710000000001` and the CLI printed `15640.71`, because it parsed the
    /// body into a `serde_json::Value` and re-serialized it — and `serde_json` does
    /// not round-trip `f64` bit-exactly. A CLI that quietly moves the last bit of
    /// every float feeds a wrong number into every downstream `jq` pipeline, and no
    /// consumer can see it happening.
    ///
    /// Mutation-proof, verified: change `emit_json` to
    /// `serde_json::to_string_pretty(&body.value())` — the obvious "prettier"
    /// version — and this fails on the float.
    #[test]
    fn json_output_relays_the_servers_bytes_unchanged() {
        let raw = r#"{"monitorTotal":{"cpuSecsTotal":15640.710000000001},"source":null}"#;
        let body = Body { text: raw.into() };

        // What `emit_json` would print, without capturing stdout.
        assert_eq!(body.text.trim_end(), raw);

        // And the re-serializing version really does differ, so the assertion
        // above is not vacuous — this is the mutation, executed.
        let round_tripped = serde_json::to_string(&body.value()).unwrap();
        assert_ne!(
            round_tripped, raw,
            "if a Value round trip were lossless this test would prove nothing"
        );
        assert!(
            round_tripped.contains("15640.71") && !round_tripped.contains("15640.710000000001"),
            "the ULP loss must be reproducible here, not just in e2e: {round_tripped}"
        );
    }

    /// A typed decode reads the RAW BYTES rather than a value that has already
    /// been through our own encoder, so casing and format drift is visible
    /// (Testing standard).
    #[test]
    fn a_typed_decode_starts_from_the_raw_bytes() {
        let raw = include_str!("../../../tests/fixtures/pressure-shrieking.json");
        let body = Body { text: raw.into() };
        let p: Pressure = body.decode("a verdict");
        assert_eq!(p.glyph, "💀🧠");
        assert_eq!(p.dimensions.len(), 3);
        assert_eq!(p.findings.len(), 3);
    }

    // ---- golden pins for the human `status` view (banshee-e8p) --------------
    //
    // The --json path is byte-pinned by e2e; the HUMAN view was untested, so a
    // mangled glyph, a wrong held/gap phrasing, or a dropped finding would reach
    // a terminal unnoticed. These decode the shared fixtures and assert on
    // render_status's output.

    fn verdict(fixture: &str) -> Pressure {
        serde_json::from_str(fixture).expect("a pressure fixture decodes")
    }

    /// The Shrieking verdict: glyph VERBATIM, the held time and the "gap before"
    /// clause (the banshee-s6s honesty), and the ranked worklist with the
    /// server's own action wording. Mutation-proof: drop the gap clause in
    /// render_status and the "17m gap before" assertion fails.
    #[test]
    fn status_renders_the_shrieking_verdict_with_held_gap_and_findings() {
        let p = verdict(include_str!(
            "../../../tests/fixtures/pressure-shrieking.json"
        ));
        let out = render_status(&p, false);

        assert!(
            out.starts_with("💀🧠  Banshee: Shrieking, memory"),
            "the glyph + label lead, verbatim: {out:?}"
        );
        // cpu: heldSecs 3600 → "1h0m", observationGapSecs 1029 → "17m".
        assert!(out.contains("CPU utilization"), "{out}");
        assert!(out.contains("(held 1h0m"), "held time in human form: {out}");
        assert!(
            out.contains("17m gap before"),
            "the gap that bounded the held time must be shown: {out}"
        );
        assert!(out.contains("Findings, best action first:"), "{out}");
        assert!(out.contains("35.6 GB of swap in use."), "{out}");
        assert!(
            out.contains("→ Quit and relaunch the biggest apps"),
            "the server's action wording, verbatim: {out}"
        );
        // The who-line sits under its finding, verbatim, and a finding
        // that names nobody (the fixture's uptime) prints no `who:` at all.
        // Mutation-proof: drop the who line and the first assertion fails;
        // print `who_line.unwrap_or_default()` unconditionally and the count
        // of "who:" lines still matches (2 of 3 findings name anyone) — so the
        // second assertion pins the omission rather than the presence.
        assert!(
            out.contains("\n     who: Chrome ×115 at 7.7 GB, claude ×8 at 2.1 GB\n"),
            "{out}"
        );
        assert_eq!(
            out.matches("who:").count(),
            2,
            "two findings name someone, uptime does not: {out}"
        );
        assert!(
            out.contains("40 samples, 3 censuses in the window"),
            "{out}"
        );
    }

    /// Green dimensions are HIDDEN by default and shown only with --all; and a
    /// verdict with nothing out of band says so rather than printing an empty
    /// block. Mutation-proof: drop the `!show_all && green → continue` skip and
    /// the default view stops being empty.
    #[test]
    fn status_hides_green_dimensions_unless_all_is_requested() {
        let p = verdict(include_str!("../../../tests/fixtures/pressure-quiet.json"));

        let default = render_status(&p, false);
        assert!(
            !default.contains("CPU utilization"),
            "green dimensions are hidden by default: {default}"
        );
        // …except a RECOVERING green (ADR-0009): the quiet fixture's disk is green
        // again inside its episode's down window, and that is the one green worth
        // seeing. Mutation-proof: drop `&& !r.recovering` from the skip and the
        // disk row (and its marker) vanish from the default view.
        assert!(
            default.contains("Disk headroom") && default.contains("[recovering]"),
            "a recovering dimension shows by default, marked: {default}"
        );
        assert!(
            default.starts_with("😴🩹  Banshee: Quiet, recovering"),
            "the decorated glyph and label lead, verbatim: {default}"
        );
        // The long memory rides the status view (banshee-qoz). Mutation-proof:
        // drop the activity line and this fails.
        assert!(
            default.contains("1 open episode, 1 active in the last hour, 2 in 24h"),
            "{default}"
        );

        // A verdict with NOTHING recovering and nothing out of band says so rather
        // than printing an empty block.
        let mut calm = p.clone();
        for r in &mut calm.dimensions {
            r.recovering = false;
        }
        let calm_view = render_status(&calm, false);
        assert!(
            calm_view.contains("Nothing is out of band."),
            "a calm machine's default view names that: {calm_view}"
        );

        let all = render_status(&p, true);
        assert!(
            all.contains("CPU utilization") && all.contains("Disk headroom"),
            "--all shows the green dimensions: {all}"
        );
        assert!(!all.contains("Nothing is out of band."), "{all}");
    }

    /// The alerts view renders EPISODES: start, how long it ran, dimension, peak
    /// level, the peak's wording, and "+N more" for the re-fires it absorbed.
    /// Mutation-proof: drop the `more` clause and forty storms
    /// read as one; render `ended_at` instead of the span and "ran 4h16m" goes.
    #[test]
    fn alerts_renders_an_episode_with_its_span_peak_and_suppressed_count() {
        let e: AlertEpisode =
            serde_json::from_str(include_str!("../../../tests/fixtures/alert-episode.json"))
                .unwrap();
        let out = render_alerts(std::slice::from_ref(&e));
        assert!(out.contains("2026-09-05 17:24:03"), "{out}");
        assert!(
            out.contains("ran 4h16m"),
            "the span, not the end instant: {out}"
        );
        assert!(out.contains("thrash"), "{out}");
        assert!(out.contains("peak shrieking"), "{out}");
        assert!(out.contains("+39 more"), "the suppressed count: {out}");
        assert!(
            out.contains("4145 swap operations"),
            "the peak's wording: {out}"
        );
        assert!(
            out.contains("(who: Chrome ×102 at 8.9 GB, claude ×10 at 2.1 GB)"),
            "who was behind it at the peak (the who-line): {out}"
        );

        // An OPEN episode says so instead of inventing a span.
        let mut open = e;
        open.ended_at = None;
        open.state = EpisodeState::Firing;
        open.suppressed = 0;
        open.peak.who_line = None;
        let out = render_alerts(&[open]);
        assert!(out.contains("OPEN"), "{out}");
        assert!(
            !out.contains("who:") && !out.contains("()"),
            "a peak that named nobody prints no who clause: {out}"
        );
        assert!(
            !out.contains("more"),
            "no count when nothing was swallowed: {out}"
        );

        assert!(
            render_alerts(&[]).contains("no alert episodes"),
            "the empty case is named"
        );
    }

    /// Checking is NOT calm: the view must say the sampler lacks data, never
    /// imply a healthy machine. Mutation-proof: return early with an empty string
    /// and the "not a claim" line disappears.
    #[test]
    fn status_says_checking_is_not_a_claim_of_calm() {
        let p = verdict(include_str!(
            "../../../tests/fixtures/pressure-checking.json"
        ));
        let out = render_status(&p, false);
        assert!(out.contains("Not enough data yet"), "{out}");
        assert!(
            out.contains("not a claim that the machine is calm"),
            "{out}"
        );
        // No findings/worklist when there is no data.
        assert!(!out.contains("Findings"), "{out}");
    }

    // ---- golden pins for the human `headroom` view (ADR-0010) --------------

    fn headroom(fixture: &str) -> Headroom {
        serde_json::from_str(fixture).expect("a headroom fixture decodes")
    }

    /// The rendered line that names a condition, so the pins do not depend on
    /// column widths.
    fn line_with<'a>(out: &'a str, needle: &str) -> &'a str {
        out.lines()
            .find(|l| l.contains(needle))
            .unwrap_or_else(|| panic!("no line mentions {needle}:\n{out}"))
    }

    /// The decision leads, in the server's words, and the conditions follow as a
    /// table. Mutation-proof: derive WAIT/GO from `level` instead of
    /// `should_wait` and the throttled case (Restless, but waiting) prints GO.
    #[test]
    fn headroom_renders_wait_from_the_flag_not_the_level() {
        let h = headroom(include_str!(
            "../../../tests/fixtures/headroom-throttled.json"
        ));
        let out = render_headroom(&h);
        assert!(
            out.starts_with("WAIT  Restless: Thermal pressure is red"),
            "{out}"
        );
        assert!(out.contains("recommended parallelism  0"), "{out}");
        assert!(out.contains("retry after              1m"), "{out}");
        let thermal = line_with(&out, "ThermalPressure");
        assert!(
            thermal.starts_with("  ! "),
            "a True pressure is marked: {thermal}"
        );
        assert!(
            thermal.contains("True") && thermal.contains("ThermalRed"),
            "{thermal}"
        );
        let cpu = line_with(&out, "CpuPressure");
        assert!(
            cpu.starts_with("    "),
            "a False pressure is unmarked: {cpu}"
        );
        assert!(cpu.contains("False") && cpu.contains("AllGreen"), "{cpu}");
        let corp = line_with(&out, "ManagedAgentsPressure");
        assert!(
            corp.starts_with("  ? "),
            "Unknown is a question mark: {corp}"
        );
        assert!(corp.contains("NoReading"), "{corp}");
        // Ready has the opposite polarity: True is good news and is NOT marked.
        let ready = line_with(&out, "Ready");
        assert!(
            ready.starts_with("    "),
            "Ready True is not a warning: {ready}"
        );
        assert!(out.contains("Advice, not enforcement"), "{out}");
    }

    /// The calm case says GO with the number, and an EXPLICIT dash for the retry
    /// — never a zero, which would read as "retry immediately".
    #[test]
    fn headroom_renders_go_with_the_slot_count_and_no_retry() {
        let h = headroom(include_str!("../../../tests/fixtures/headroom-quiet.json"));
        let out = render_headroom(&h);
        assert!(
            out.starts_with("GO  Quiet: room for 3 more workers on 8 cores"),
            "{out}"
        );
        assert!(out.contains("recommended parallelism  3"), "{out}");
        assert!(out.contains("retry after              —"), "{out}");
        assert!(!out.contains("retry after              0"), "{out}");
    }

    /// Checking is WAIT, and says it is not headroom. Mutation-proof: print GO
    /// whenever the level is below Wailing and this fails.
    #[test]
    fn headroom_says_checking_is_wait_not_headroom() {
        let h = headroom(include_str!(
            "../../../tests/fixtures/headroom-checking.json"
        ));
        let out = render_headroom(&h);
        assert!(out.starts_with("WAIT  Checking:"), "{out}");
        assert!(out.contains("this is not headroom"), "{out}");
        let cpu = line_with(&out, "CpuPressure");
        assert!(
            cpu.starts_with("  ? ") && cpu.contains("NoReading"),
            "{cpu}"
        );
        // Ready False IS marked: the verdict is not trustworthy yet.
        let ready = line_with(&out, "Ready");
        assert!(
            ready.starts_with("  ! ") && ready.contains("Checking"),
            "{ready}"
        );
    }

    /// A datetime carries `:` and `+`. An unencoded `+` decodes to a SPACE
    /// server-side, so `…+01:00` — a form the wire contract explicitly promises to
    /// accept — would come back as a 400 from the CLI while working fine from
    /// curl. Mutation-proof: return `s.to_string()` from `urlencode` and this
    /// fails on the `+`.
    #[test]
    fn datetimes_survive_the_query_string() {
        assert_eq!(
            urlencode("2026-08-31T15:04:05.123456Z"),
            "2026-08-31T15%3A04%3A05.123456Z"
        );
        let offset = urlencode("2026-08-31T15:04:05.123456+01:00");
        assert!(
            !offset.contains('+'),
            "a raw + decodes to a space: {offset}"
        );
        assert!(offset.contains("%2B"));
        // Unreserved characters are left alone, so the common case stays readable
        // in a log or an strace.
        assert_eq!(urlencode("500"), "500");
        assert_eq!(urlencode("a-b_c.d~e"), "a-b_c.d~e");
    }

    /// Durations read as durations. The held-time column is the difference between
    /// "red for a moment" and "red all afternoon", which is the difference between
    /// Restless and Wailing.
    #[test]
    fn durations_render_at_a_human_scale() {
        assert_eq!(fmt_secs(0), "0s");
        assert_eq!(fmt_secs(59), "59s");
        assert_eq!(fmt_secs(60), "1m");
        assert_eq!(fmt_secs(3599), "59m");
        assert_eq!(fmt_secs(3600), "1h0m");
        assert_eq!(fmt_secs(7_260), "2h1m");
        assert_eq!(fmt_secs(86_400), "1d");
        assert_eq!(fmt_secs(604_800), "7d");
    }

    #[test]
    fn byte_sizes_render_at_a_human_scale() {
        assert_eq!(fmt_bytes(0), "0 B");
        assert_eq!(fmt_bytes(999), "999 B");
        assert_eq!(fmt_bytes(1_500), "2 KB");
        assert_eq!(fmt_bytes(35_600_000_000), "35.6 GB");
    }

    /// The three bands must be visually DISTINCT in a plain-text table, and the
    /// markers must be the same width so the columns line up. Mutation-proof: give
    /// Yellow and Red the same marker and this fails.
    #[test]
    fn band_markers_are_distinct_and_column_aligned() {
        let markers = [
            band_marker(Band::Green),
            band_marker(Band::Yellow),
            band_marker(Band::Red),
        ];
        let mut sorted = markers;
        sorted.sort_unstable();
        sorted.iter().reduce(|a, b| {
            assert_ne!(a, b, "band markers must be distinguishable");
            b
        });
        for m in markers {
            assert_eq!(m.chars().count(), 2, "markers must be one width: {m:?}");
        }
    }
}
