// banshee-mcp — MCP server over stdio (JSON-RPC 2.0), following the
// house pattern: hand-rolled protocol loop, blocking HTTP to the
// API hub for all data. Never opens the database.
//
// One tool per route, and the tool returns the API's JSON UNCHANGED — no
// envelope, no re-shaping, no summarising. That is what lets
// `tests/e2e/run-e2e.sh` compare all three surfaces byte-for-byte instead of only
// the one row the template checked, and it means an agent and a human are looking
// at the same numbers.
//
// The tool DESCRIPTIONS carry the one thing an agent cannot infer from a schema:
// that `level`, `band`, `severity` and `glyph` are authoritative and must not be
// recomputed from the raw numbers (ADR-0005). An agent that re-derives a severity
// will get it subtly wrong and then confidently report it.

use std::io::{self, BufRead, Write};

use serde::Deserialize;
use serde_json::{Value, json};

const PROTOCOL_VERSION: &str = "2024-11-05";

/// Gate for the DESTRUCTIVE action tools. When unset, the `reap_*_execute`
/// tools are ABSENT from `tools/list` entirely — not present-and-refusing.
///
/// Absence over refusal is deliberate: an agent chooses tools from the list it
/// is shown, and a tool it cannot see is one it cannot decide to call, cannot
/// be prompt-injected into calling, and cannot "retry with different arguments"
/// after a refusal. A refusing-but-present kill tool is a loaded gun with the
/// safety described in the manual. The read-only PREVIEW tools are always
/// present — showing an agent what WOULD be reaped kills nothing.
const ACTIONS_ENV: &str = "BANSHEE_MCP_ENABLE_ACTIONS";

fn actions_enabled() -> bool {
    std::env::var(ACTIONS_ENV).as_deref() == Ok("1")
}

#[derive(Deserialize)]
struct JsonRpcRequest {
    id: Option<Value>,
    method: String,
    #[serde(default)]
    params: Value,
}

fn response(id: Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

fn error_response(id: Value, code: i64, message: &str) -> Value {
    json!({
        "jsonrpc": "2.0", "id": id,
        "error": { "code": code, "message": message }
    })
}

/// Where the API key comes from — kept as a SOURCE rather than a value.
///
/// The daemon rotates its key when it finds the key file was readable by other users,
/// and this server is long-lived: an agent host starts it once and keeps it for the
/// whole session. A key captured at startup therefore goes dead mid-session, and every
/// tool call 401s from then on. Observed exactly that way — the MCP server returned
/// "missing or invalid X-Api-Key (401)" for the rest of a session while the daemon was
/// healthy the whole time. The CLI escaped only because it is a fresh process per
/// invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
enum KeySource {
    /// An explicit `BANSHEE_API_KEY`, or a deliberately empty key for a
    /// non-loopback URL. Nothing to re-read.
    Fixed(String),
    /// The key file, re-read on demand so a rotation is picked up.
    File(std::path::PathBuf),
}

impl KeySource {
    fn resolve(&self) -> String {
        match self {
            KeySource::Fixed(k) => k.clone(),
            KeySource::File(p) => std::fs::read_to_string(p)
                .map(|k| k.trim().to_string())
                .unwrap_or_default(),
        }
    }
}

/// How this server talks to the daemon (ADR-0008). The Unix socket is the default and
/// the only transport the file-loaded key travels over; TCP survives for an explicit
/// `BANSHEE_API_URL` and carries only a key the caller supplied.
enum Transport {
    Unix(banshee_core::uds_http::UdsHttp),
    Tcp {
        base: String,
        http: reqwest::blocking::Client,
    },
}

impl Transport {
    fn describe(&self) -> String {
        match self {
            Transport::Unix(c) => format!("unix://{}", c.path().display()),
            Transport::Tcp { base, .. } => base.clone(),
        }
    }
}

struct Api {
    transport: Transport,
    key: KeySource,
}

/// The socket to use, or `None` when there is none. A configured-but-absent path is
/// not a daemon, so it falls back rather than reporting a connection failure.
fn resolve_socket_path() -> Option<std::path::PathBuf> {
    if let Ok(p) = std::env::var(banshee_core::API_SOCKET_ENV) {
        let path = std::path::PathBuf::from(p);
        return path.exists().then_some(path);
    }
    let path = dirs::config_dir()?
        .join("banshee")
        .join(banshee_core::API_SOCKET_FILE);
    path.exists().then_some(path)
}

/// A reqwest client that cannot be talked into leaking the key: no system proxy, no
/// redirect-following, bounded timeouts. The fallback keeps those properties, because a
/// bare `Client::new()` would restore both (fail-open).
fn hardened_http() -> reqwest::blocking::Client {
    reqwest::blocking::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(std::time::Duration::from_secs(30))
        .connect_timeout(std::time::Duration::from_secs(5))
        .build()
        .unwrap_or_else(|_| {
            reqwest::blocking::Client::builder()
                .no_proxy()
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .expect("hardened reqwest client")
        })
}

impl Api {
    fn from_env() -> Self {
        // An explicitly requested URL wins: the caller asked for TCP.
        let explicit_url = std::env::var("BANSHEE_API_URL").ok();
        let socket = resolve_socket_path();

        // ADR-0008. The file-loaded key travels over the SOCKET only — never over TCP,
        // not even to loopback. Loopback was never a sufficient gate: any local user can
        // bind 127.0.0.1:18769 before the daemon starts and harvest the key from every
        // client. A socket file cannot be taken over that way; it lives in a directory
        // only this user can write and the kernel enforces its 0600 mode.
        let explicit_key = std::env::var("BANSHEE_API_KEY")
            .ok()
            .map(|k| k.trim().to_string());

        let transport = match (explicit_url, socket) {
            (Some(url), _) => Transport::Tcp {
                base: url.trim_end_matches('/').to_string(),
                http: hardened_http(),
            },
            (None, Some(path)) => Transport::Unix(banshee_core::uds_http::UdsHttp::new(
                path,
                std::time::Duration::from_secs(30),
            )),
            (None, None) => Transport::Tcp {
                base: banshee_core::default_api_url(),
                http: hardened_http(),
            },
        };

        let key = match (&transport, explicit_key) {
            (_, Some(k)) => KeySource::Fixed(k),
            // Held as a PATH, so a rotation is picked up (banshee-dqh).
            (Transport::Unix(_), None) => std::env::var("BANSHEE_KEY_FILE")
                .map(std::path::PathBuf::from)
                .ok()
                .or_else(|| dirs::config_dir().map(|d| d.join("banshee/api_key")))
                .map(KeySource::File)
                .unwrap_or_else(|| KeySource::Fixed(String::new())),
            (Transport::Tcp { base, .. }, None) => {
                eprintln!(
                    "banshee-mcp: not sending the local API key over TCP to {base} (ADR-0008); \
                     authenticated reads need the daemon's socket."
                );
                KeySource::Fixed(String::new())
            }
        };
        Self { transport, key }
    }

    /// Send a keyed request over whichever transport was resolved, re-reading the key
    /// and retrying ONCE on 401 (`banshee-dqh`).
    ///
    /// Returns `(status, body)` rather than a typed response so both transports can
    /// share one call site — and because a 401 has to be VISIBLE here in order to
    /// trigger the re-read.
    ///
    /// The retry is conditional on the re-read key being DIFFERENT. Retrying with the
    /// identical key would just be a second identical rejection — two requests for one
    /// answer on every genuinely-misconfigured call.
    fn send_keyed(&self, method: &str, target: &str) -> Result<(u16, String), String> {
        let key = self.key.resolve();
        let (status, body) = self.attempt(method, target, &key)?;
        if status == 401 {
            let fresh = self.key.resolve();
            if fresh != key && !fresh.is_empty() {
                return self.attempt(method, target, &fresh);
            }
        }
        Ok((status, body))
    }

    fn attempt(&self, method: &str, target: &str, key: &str) -> Result<(u16, String), String> {
        match &self.transport {
            Transport::Unix(client) => client
                .request(method, target, key, None)
                .map(|r| (r.status, r.body)),
            Transport::Tcp { base, http } => {
                let url = format!("{base}{target}");
                let req = if method == "POST" {
                    http.post(url)
                } else {
                    http.get(url)
                };
                let req = if key.is_empty() {
                    req
                } else {
                    req.header("x-api-key", key)
                };
                let resp = req.send().map_err(|e| {
                    format!(
                        "cannot reach banshee-api at {}: {e}",
                        self.transport.describe()
                    )
                })?;
                let status = resp.status().as_u16();
                let body = resp
                    .text()
                    .map_err(|e| format!("cannot read API response body: {e}"))?;
                Ok((status, body))
            }
        }
    }

    /// POST `path` with no body, returning the API's JSON BYTES verbatim. The
    /// action routes take no parameters; targets are recomputed server-side.
    fn post(&self, path: &str) -> Result<String, String> {
        let (status, body) = self.send_keyed("POST", path)?;
        Self::body_or_error(status, body)
    }

    /// GET `path` with `params` appended, returning the API's JSON BYTES verbatim.
    fn get(&self, path: &str, params: &[(&str, String)]) -> Result<String, String> {
        let mut target = path.to_string();
        if !params.is_empty() {
            let query: Vec<String> = params
                .iter()
                .map(|(k, v)| format!("{k}={}", urlencode(v)))
                .collect();
            target.push('?');
            target.push_str(&query.join("&"));
        }
        let (status, body) = self.send_keyed("GET", &target)?;
        Self::body_or_error(status, body)
    }

    /// GET /health. Unlike [`get`], a `503 degraded` body is a valid answer
    /// an agent should SEE (the store is unusable), not an error to hide — so
    /// return the JSON body regardless of status.
    fn health(&self) -> Result<String, String> {
        // /health is unauthenticated, so no key and no retry — but it still goes over
        // the resolved transport, or a socket-only daemon would look unreachable.
        let (status, text) = self.attempt("GET", "/health", "")?;
        // Parsed only to VALIDATE; what is returned is the server's own bytes.
        match serde_json::from_str::<Value>(&text) {
            Ok(_) => Ok(text),
            Err(_) => {
                let body = text.trim();
                let shown = if body.is_empty() {
                    "<empty response body>"
                } else {
                    body
                };
                Err(format!(
                    "health returned a non-JSON body: {shown} ({status})"
                ))
            }
        }
    }

    /// Validate a response and return the API's BYTES, or an error string.
    ///
    /// The bytes are passed through rather than re-serialized from a parsed
    /// `Value`, because that is not a round trip: `serde_json` reads
    /// `15640.710000000001` back one unit in the last place lower and then emits
    /// `15640.71`. The parity harness caught exactly that on a live census. An
    /// agent doing arithmetic on a number the tool quietly altered has no way to
    /// notice.
    ///
    /// When the body is NOT JSON (e.g. a text/plain error from the API or a
    /// proxy), fall back to `status line + raw body` so the real failure reaches
    /// the agent instead of a misleading "invalid JSON from API" (agentapi C1).
    fn body_or_error(status: u16, text: String) -> Result<String, String> {
        let success = (200..300).contains(&status);
        match serde_json::from_str::<Value>(&text) {
            Ok(value) => {
                if !success {
                    let msg = value["error"].as_str().unwrap_or("unknown error");
                    return Err(format!("{msg} ({status})"));
                }
                Ok(text)
            }
            Err(_) => {
                let body = text.trim();
                let shown = if body.is_empty() {
                    "<empty response body>"
                } else {
                    body
                };
                if success {
                    Err(format!(
                        "unexpected non-JSON success body from API: {shown}"
                    ))
                } else {
                    Err(format!("{shown} ({status})"))
                }
            }
        }
    }
}

/// Percent-encode a query parameter value. Datetimes carry `:` and `+`, and an
/// unencoded `+` decodes to a SPACE server-side — turning a `…+01:00` timestamp
/// the wire contract promises to accept into a 400.
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

/// The read + preview tools (always present) plus, when `actions_enabled` is
/// true, the two DESTRUCTIVE execute tools.
fn tool_definitions(actions_enabled: bool) -> Value {
    let mut tools = read_and_preview_tools();
    if actions_enabled {
        tools
            .as_array_mut()
            .unwrap()
            .extend(execute_tools().as_array().unwrap().iter().cloned());
    }
    tools
}

fn read_and_preview_tools() -> Value {
    json!([
        {
            "name": "pressure",
            "description": "How is this Mac doing right now? Returns Banshee's whole \
                verdict: `level` (checking | quiet | stirring | restless | wailing | \
                shrieking), `source` (which of cpu/memory/disk/sprawl/corporate is \
                driving it), the `glyph` shown in the menu bar, one reading per \
                dimension with its `band` and `severity`, and `findings` — a worklist \
                already ranked by measured impact. Each finding names WHO is behind \
                it: `who` (top consumers, biggest first, from the daemon's own \
                process census) and `whoLine`, the same list as one sentence — quote \
                it verbatim. Memory/swap figures are RESIDENT size; CPU figures are \
                cumulative CPU over the group's lifetime (a sustained rate), not an \
                instantaneous %CPU. `managed agents` is one deduplicated entry — \
                never add per-group percentages from `census` to it. \
                \n\nUSE THESE VALUES AS GIVEN. `level`, `band`, `severity` and `glyph` \
                are computed once, in one place, from a window of history with \
                hysteresis applied; recomputing a severity from the raw numbers will \
                disagree with what the human sees in their menu bar. \
                \n\n`level: \"checking\"` means the daemon has not gathered enough \
                samples yet — it is NOT a claim that the machine is calm. Say so \
                rather than reporting a healthy machine. \
                \n\n`recovering: true` (and per-dimension `recovering`) means a \
                dimension was red and has dropped below red inside its alert \
                episode's down window — calm now, but the incident is not over. \
                `activity` {open, lastHour, lastDay} counts alert episodes; a quiet \
                level with a non-zero `lastDay` means the day was not quiet — call \
                `alerts` for the story.",
            "inputSchema": { "type": "object", "properties": {}, "required": [] }
        },
        {
            "name": "headroom",
            "description": "Should you start new work on this Mac right now? A DECISION, \
                not a number (ADR-0010): `shouldWait` (boolean — act on it), \
                `recommendedParallelism` (how many MORE concurrent CPU-bound workers \
                the machine can absorb right now; 0 exactly when shouldWait), \
                `retryAfterSecs` (when to ask again; null when not waiting), `cores`, \
                one `reason` sentence you can quote verbatim, and `conditions` in \
                Kubernetes node-condition shape — `Ready` then one per pressure \
                source, each {type, status: True|False|Unknown, reason, message, \
                since}. Every value is computed once, in the daemon, from the same \
                evaluation as `pressure`. \
                \n\nTWO THINGS AGENTS GET WRONG. (1) `level: \"checking\"` is NOT \
                headroom: it means the daemon has not gathered enough samples yet, \
                and the answer is `shouldWait: true` with parallelism 0 — wait \
                `retryAfterSecs`, do not treat an empty verdict as a green light. \
                (2) Do NOT re-derive a parallelism from raw numbers (`samples`, \
                `census`, core counts, free memory). The figure here already accounts \
                for hysteresis, a throttled CPU (which makes load figures understate \
                the work), what macOS means by available memory, and the level the \
                human sees in their menu bar; your arithmetic will disagree with all \
                of that. Use `recommendedParallelism` AS GIVEN. \
                \n\nThis is ADVICE, not enforcement: Banshee informs, your human \
                decides. `shouldWait: true` means tell them and ask, not silently \
                stop.",
            "inputSchema": { "type": "object", "properties": {}, "required": [] }
        },
        {
            "name": "deltas",
            "description": "What CHANGED on this Mac versus a lookback — the tool that \
                answers \"I freed memory, why did it not help?\". Returns, all composed \
                once in the daemon (never re-derive them): `summary` (one sentence you \
                can quote), `verdict` (the pressure level at BOTH ends — then and now), \
                `dimensions` (each dimension's then/now/delta in its own unit), \
                `consumers` (per app group and per agent-session program: freed or \
                grown, ranked BY CHANGE not by current size), and `census` (the two \
                census instants compared). \
                \n\nTWO THINGS AGENTS GET WRONG. (1) \"Used memory went DOWN\" is NOT \
                \"pressure went DOWN\" on macOS: swap and the compressor do not give \
                back what they took, so freed memory is often absorbed in seconds and \
                the machine gets WORSE. That is why the VERDICT rides at both ends — \
                read `verdict.thenLevel` → `verdict.nowLevel`, not just the byte \
                deltas. (2) A delta ACROSS A GAP in observation is arithmetic on a \
                hole: a laptop that slept has no readings for that span. When \
                `observationGapSecs` (top-level, or per dimension) is non-null, nobody \
                was watching in between — the difference is still real but do not read \
                it as continuous. \
                \n\n`vs` accepts `30m`, `1h`, `24h`, `90s`, bare seconds, or `episode` \
                (since the earliest still-open incident began, compared against its \
                stored peak — which survives even after the raw samples behind it are \
                swept). Omit `vs` for the default, 1 hour. A lookback past the 24-hour \
                raw retention is refused, not clamped; when the history is shorter than \
                asked, `actualLookbackSecs` says how far it really reached. \
                \n\n`vs=episode` errors when no episode is open — there is nothing to \
                compare against, which is good news.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "vs": {
                        "type": "string",
                        "description": "Lookback: 30m, 1h, 24h, 90s, bare seconds, or `episode`. Omit for the default (1h). Past the 24h raw retention is refused, not clamped."
                    }
                },
                "required": []
            }
        },
        {
            "name": "samples",
            "description": "Raw readings from the time series, OLDEST FIRST — load, \
                free memory, swap, swap counters and per-volume free space, sampled \
                every 15 seconds. Use this to see the SHAPE of a change (is swap \
                climbing or recovering?) after `pressure` has told you what is wrong. \
                Only the last 24 hours are kept raw; older history exists as 5-minute \
                rollups that this tool does not return.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "limit": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": 500,
                        "description": "Trailing samples to return, 1–500. Omit for the last 15 minutes. Over 500 is refused, not truncated."
                    },
                    "from": {
                        "type": "string",
                        "description": "Window start as an ISO-8601 UTC datetime, e.g. 2026-08-31T15:04:05.123456Z. Cannot be combined with limit."
                    },
                    "to": {
                        "type": "string",
                        "description": "Window end; defaults to now. Requires `from`."
                    }
                },
                "required": []
            }
        },
        {
            "name": "rollups",
            "description": "History as 5-minute buckets, dense from the last \
                CLOSED bucket back 30 days: each bucket is summarised as soon as \
                it closes (within five minutes), and raw samples are deleted at \
                24h once their bucket exists — so \"what did last week look like\" \
                AND \"what did the last hour look like at a glance\" are answered \
                here; `samples` is for the 15-second detail of the last day. \
                \n\nEach bucket carries averages AND maxima (`load1mAvg` vs \
                `load1mMax`, `swapUsedAvg` vs `swapUsedMax`, `pagesFreeMin`), which \
                is why this is a separate tool rather than an option on `samples`: \
                the shapes genuinely differ. Use the MAX when asking \"how bad did it \
                get\" and the average when asking \"what was it usually like\"; \
                reporting an average as though it were a peak is the mistake this \
                data makes easy. \
                \n\n`swapinsDelta`/`swapoutsDelta` are INCREASES across the bucket \
                and are null when a reboot inside it voided the counters — null is \
                not zero. \
                \n\nA window is not row-capped: the full 30-day retention is 8,640 \
                buckets, which is a lot of context. Ask for the window you need.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "limit": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": 500,
                        "description": "Trailing buckets to return, 1–500. Omit for the last five hours. Over 500 is refused, not truncated."
                    },
                    "from": {
                        "type": "string",
                        "description": "Window start as an ISO-8601 UTC datetime. Cannot be combined with limit."
                    },
                    "to": {
                        "type": "string",
                        "description": "Window end; defaults to now. Requires `from`."
                    }
                },
                "required": []
            }
        },
        {
            "name": "census",
            "description": "What is actually RUNNING, from the 5-minute census: agent \
                CLI sessions (with which are stale and their working directories), \
                orphaned helper processes whose parents are gone, tmux sessions, the \
                biggest app process groups, and centrally-managed monitoring agents. \
                This is the tool that answers \"what should I close?\". \
                \n\nRead `monitorTotal.percentOfOneCore` for the managed-agent cost; \
                NEVER sum the per-agent `percentOfOneCore` values, because each \
                divides by the longest-lived process in its own group and the \
                denominators differ. \
                \n\n`tmuxAvailable: false` means tmux could not be inspected — that is \
                not the same as zero sessions. \
                \n\nErrors with 'no census has been taken yet' during the first moments \
                after the daemon starts.",
            "inputSchema": { "type": "object", "properties": {}, "required": [] }
        },
        {
            "name": "alerts",
            "description": "Alert EPISODES, NEWEST FIRST: one entry per continuous \
                incident on one dimension, not one per notification. Each carries \
                `startedAt`, `endedAt` (null while still open), `state` (firing, \
                recovering, closed), `peak` (the worst band/level/severity, when, and \
                the wording at that moment) and `suppressed` — how many re-fires the \
                episode absorbed, so `suppressed: 39` is forty storms and `0` is one. \
                Open episodes are ALWAYS included whatever `since` says. Kept for a \
                year, so this is the long memory: the samples behind a bad afternoon \
                are gone while its episodes are still here. Read this after `pressure` \
                whenever `activity.lastDay` is non-zero — a calm verdict does not mean \
                a calm day.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "since": {
                        "type": "string",
                        "description": "How far back to look, as an ISO-8601 UTC datetime. Defaults to 24 hours ago."
                    },
                    "limit": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": 500,
                        "description": "Maximum episodes to return, 1–500. A capped read drops the oldest, keeping the most recent."
                    }
                },
                "required": []
            }
        },
        {
            "name": "stats",
            "description": "What Banshee itself costs: row counts per tier, the \
                database size on disk, and the schema version. `dbSizeBytes` is the \
                file's high-water mark rather than the current row count — SQLite \
                reuses freed pages instead of returning them to the OS, so the file \
                does not shrink after a retention sweep.",
            "inputSchema": { "type": "object", "properties": {}, "required": [] }
        },
        {
            "name": "health",
            "description": "Check the API server's health. Returns {status, version}; \
                status is \"ok\" when the datastore is usable, \"degraded\" otherwise.",
            "inputSchema": { "type": "object", "properties": {}, "required": [] }
        },
        {
            "name": "reap_stale_sessions_preview",
            "description": "DRY RUN. Show which stale agent CLI sessions would be \
                reaped and which would be SPARED, and why. Kills nothing — this is \
                the tool to call BEFORE proposing a reap to the human. \
                \n\nEach candidate carries a `verdict` (reap | spare) and a `reason`. \
                A session is spared when a pane is mid-build (a busy command), when \
                its working directory is a git repo with uncommitted changes, or when \
                its idle age is unknown — \"we could not tell\" is never \"safe to \
                kill\". `tmuxAvailable: false` means tmux could not be inspected, \
                which is not the same as no sessions. \
                \n\nTo actually reap, the human must have enabled the execute tools \
                (BANSHEE_MCP_ENABLE_ACTIONS=1); if you do not see a \
                reap_stale_sessions_execute tool, they have not, and you cannot reap.",
            "inputSchema": { "type": "object", "properties": {}, "required": [] }
        },
        {
            "name": "reap_orphans_preview",
            "description": "DRY RUN. List the orphaned helper processes (MCP servers, \
                language servers, parsers whose parent session has died) that would be \
                terminated. Kills nothing. \
                \n\nThe list you see here is NOT the list that would be killed: an \
                execute recomputes candidates at kill time, because PIDs recycle and \
                killing a recycled PID is unacceptable. Use this to tell the human how \
                many orphans exist and what they are, not to hand a PID list to \
                anything.",
            "inputSchema": { "type": "object", "properties": {}, "required": [] }
        }
    ])
}

/// The destructive tools. Absent from `tools/list` unless the human set
/// `BANSHEE_MCP_ENABLE_ACTIONS=1` — see `ACTIONS_ENV`.
fn execute_tools() -> Value {
    json!([
        {
            "name": "reap_stale_sessions_execute",
            "description": "DESTRUCTIVE. Kill the stale agent CLI sessions that pass \
                every safety rail (not busy, no uncommitted git changes, known idle \
                age ≥ the threshold). Candidates are recomputed at THIS moment, not \
                taken from an earlier preview. Takes no arguments. \
                \n\nCall reap_stale_sessions_preview first and show the human what will \
                be killed; this tool is enabled only because they opted in, but it is \
                still their machine and their unsaved work. Killing sessions orphans \
                their helper processes, so follow with reap_orphans.",
            "inputSchema": { "type": "object", "properties": {}, "required": [] }
        },
        {
            "name": "reap_orphans_execute",
            "description": "DESTRUCTIVE. Terminate orphaned helper processes: SIGTERM \
                the freshly-recomputed set, wait a grace period, then SIGKILL only the \
                ones that both survived AND still match as the same orphaned program \
                (a recycled PID fails that test and is spared). Takes no arguments — \
                you cannot supply a PID list, by design.",
            "inputSchema": { "type": "object", "properties": {}, "required": [] }
        }
    ])
}

/// Render an argument as a query-string scalar: accepts a JSON string or a
/// JSON number (agents send `limit` either way), rejects anything else.
fn scalar_arg(args: &Value, key: &str) -> Option<String> {
    match &args[key] {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

/// Collect the named arguments that are present, in a stable order.
fn query_from(args: &Value, keys: &[&'static str]) -> Vec<(&'static str, String)> {
    keys.iter()
        .filter_map(|k| scalar_arg(args, k).map(|v| (*k, v)))
        .collect()
}

fn handle_tool_call(api: &Api, name: &str, args: &Value) -> Result<String, String> {
    match name {
        "pressure" => api.get("/pressure", &[]),
        "headroom" => api.get("/headroom", &[]),
        "deltas" => api.get("/deltas", &query_from(args, &["vs"])),
        "samples" => api.get("/samples", &query_from(args, &["limit", "from", "to"])),
        "rollups" => api.get("/rollups", &query_from(args, &["limit", "from", "to"])),
        "census" => api.get("/census", &[]),
        "alerts" => api.get("/alerts", &query_from(args, &["since", "limit"])),
        "stats" => api.get("/stats", &[]),
        "health" => api.health(),
        "reap_stale_sessions_preview" => api.post("/actions/reap-stale-sessions/preview"),
        "reap_orphans_preview" => api.post("/actions/reap-orphans/preview"),
        // The execute tools are absent from tools/list when the gate is off, so
        // a well-behaved client never reaches these. The guard is defence in
        // depth against a client that calls a tool it was never offered — the
        // gate must hold on the CALL, not only on the listing.
        "reap_stale_sessions_execute" if actions_enabled() => {
            api.post("/actions/reap-stale-sessions/execute")
        }
        "reap_orphans_execute" if actions_enabled() => api.post("/actions/reap-orphans/execute"),
        "reap_stale_sessions_execute" | "reap_orphans_execute" => Err(format!(
            "{name} is disabled; set {ACTIONS_ENV}=1 to enable the destructive action tools"
        )),
        other => Err(format!("unknown tool: {other}")),
    }
}

fn handle_request(api: &Api, req: &JsonRpcRequest) -> Option<Value> {
    let id = req.id.clone()?; // Notifications (no id) get no response.
    let resp = match req.method.as_str() {
        "initialize" => response(
            id,
            json!({
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": { "tools": {} },
                "serverInfo": {
                    "name": "banshee-mcp",
                    "version": env!("CARGO_PKG_VERSION")
                }
            }),
        ),
        "tools/list" => response(id, json!({ "tools": tool_definitions(actions_enabled()) })),
        "tools/call" => {
            let name = req.params["name"].as_str().unwrap_or_default();
            let args = req.params.get("arguments").cloned().unwrap_or(json!({}));
            match handle_tool_call(api, name, &args) {
                // The API's bytes, unchanged. See `body_or_error`.
                Ok(result) => response(
                    id,
                    json!({
                        "content": [{ "type": "text", "text": result }]
                    }),
                ),
                Err(msg) => response(
                    id,
                    json!({
                        "content": [{ "type": "text", "text": msg }],
                        "isError": true
                    }),
                ),
            }
        }
        "ping" => response(id, json!({})),
        _ => error_response(id, -32601, &format!("method not found: {}", req.method)),
    };
    Some(resp)
}

/// Turn one line of stdin into the reply to write, or `None` when nothing is
/// owed (a blank line, or a notification with no id). Split out of `main` so the
/// protocol loop — malformed JSON, unknown methods, notifications — is testable
/// without a subprocess: `main` only does I/O around this.
fn handle_line(api: &Api, line: &str) -> Option<Value> {
    if line.trim().is_empty() {
        return None;
    }
    match serde_json::from_str::<JsonRpcRequest>(line) {
        Ok(req) => handle_request(api, &req),
        // A malformed line still gets a JSON-RPC parse error (-32700) with a null
        // id, per the spec — the client asked something we could not read, and
        // silence would look like a hang.
        Err(e) => Some(error_response(
            Value::Null,
            -32700,
            &format!("parse error: {e}"),
        )),
    }
}

fn main() {
    let api = Api::from_env();
    let stdin = io::stdin();
    let mut stdout = io::stdout();

    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        if let Some(reply) = handle_line(&api, &line) {
            let out = serde_json::to_string(&reply).unwrap();
            if writeln!(stdout, "{out}")
                .and_then(|_| stdout.flush())
                .is_err()
            {
                break;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every tool must have a name, a description, and an object input schema —
    /// and the tool set must match the parity table in `tests/e2e/run-e2e.sh`.
    /// Mutation-proof: delete a tool and this fails here as well as in e2e, which
    /// is the point: the unit test fails in seconds, the harness proves it end to
    /// end.
    fn names_of(tools: &Value) -> Vec<String> {
        tools
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap().to_string())
            .collect()
    }

    #[test]
    fn the_tool_set_matches_the_parity_table() {
        // Actions DISABLED: nine reads plus the two read-only previews. The
        // destructive execute tools must NOT appear.
        let tools = tool_definitions(false);
        let mut sorted = names_of(&tools);
        sorted.sort();
        assert_eq!(
            sorted,
            vec![
                "alerts",
                "census",
                "deltas",
                "headroom",
                "health",
                "pressure",
                "reap_orphans_preview",
                "reap_stale_sessions_preview",
                "rollups",
                "samples",
                "stats"
            ],
            "the MCP tool set drifted from the parity table"
        );
        assert_eq!(sorted.len(), names_of(&tools).len(), "duplicate tool name");

        for t in tools.as_array().unwrap() {
            let name = t["name"].as_str().unwrap();
            assert_eq!(t["inputSchema"]["type"], "object", "{name}");
            let description = t["description"].as_str().unwrap_or_default();
            assert!(
                description.len() > 40,
                "{name}: a one-line description is not enough for an agent to \
                 choose between the read tools"
            );
        }
    }

    /// The destructive tools are ABSENT without the gate and PRESENT with it —
    /// absent, not present-and-refusing, so an agent cannot choose or be
    /// injected into choosing a kill it was never offered. Mutation-proof: make
    /// `tool_definitions` ignore its flag and this fails on the disabled case.
    #[test]
    fn execute_tools_appear_only_when_enabled() {
        let disabled = names_of(&tool_definitions(false));
        assert!(
            !disabled.iter().any(|n| n.ends_with("_execute")),
            "no execute tool may be listed while disabled: {disabled:?}"
        );

        let enabled = names_of(&tool_definitions(true));
        assert!(enabled.contains(&"reap_stale_sessions_execute".to_string()));
        assert!(enabled.contains(&"reap_orphans_execute".to_string()));
        // Enabling ADDS to the read+preview set, never removes.
        for n in &disabled {
            assert!(enabled.contains(n), "enabling dropped {n}");
        }
        assert_eq!(enabled.len(), disabled.len() + 2);
    }

    /// Calling an execute tool while the gate is off is refused with the reason,
    /// even though a well-behaved client would never see it in the list. The gate
    /// must hold on the CALL, not only on the listing — a prompt-injected or
    /// buggy client can name a tool it was never offered. Mutation-proof: drop
    /// the `if actions_enabled()` guard arms and this stops erroring.
    #[test]
    fn a_disabled_execute_tool_is_refused_on_call() {
        // The default test environment has BANSHEE_MCP_ENABLE_ACTIONS unset.
        assert!(!actions_enabled(), "precondition: gate off by default");
        let api = Api::from_env();
        for name in ["reap_stale_sessions_execute", "reap_orphans_execute"] {
            let err = handle_tool_call(&api, name, &json!({})).unwrap_err();
            assert!(err.contains("disabled"), "{name}: {err}");
            assert!(
                err.contains(ACTIONS_ENV),
                "{name} must name the env var: {err}"
            );
        }
    }

    /// The `pressure` tool's description must TELL the agent not to re-derive a
    /// severity, and must explain `checking`. Those are the two things an agent
    /// cannot infer from the schema and will otherwise get wrong — reporting a
    /// calm machine during startup, or recomputing a band from raw load.
    ///
    /// Mutation-proof: strip either sentence from the description and this fails.
    #[test]
    fn the_pressure_tool_warns_against_re_deriving_a_severity() {
        let tools = tool_definitions(false);
        let pressure = tools
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["name"] == "pressure")
            .expect("pressure tool");
        let description = pressure["description"].as_str().unwrap();
        assert!(
            description.contains("AS GIVEN") || description.contains("as given"),
            "must tell the agent the values are authoritative"
        );
        assert!(
            description.contains("checking"),
            "must explain the checking state"
        );
        assert!(
            description.contains("NOT a claim"),
            "must say that checking is not a claim of calm"
        );
    }

    /// The `headroom` description must say what an agent gets WRONG (ADR-0010):
    /// that `checking` is not headroom, that parallelism is not to be re-derived
    /// from raw numbers, and that the answer is advice rather than enforcement.
    /// Mutation-proof: strip any of the three passages and this fails.
    #[test]
    fn the_headroom_tool_names_what_agents_get_wrong() {
        let tools = tool_definitions(false);
        let headroom = tools
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["name"] == "headroom")
            .expect("headroom tool");
        let description = headroom["description"].as_str().unwrap();
        assert!(
            description.contains("is NOT") && description.contains("headroom"),
            "must say checking is not headroom: {description}"
        );
        assert!(
            description.contains("re-derive"),
            "must warn against re-deriving parallelism: {description}"
        );
        assert!(
            description.contains("AS GIVEN"),
            "must say to use the figure as given: {description}"
        );
        assert!(
            description.contains("not enforcement"),
            "must say it is advice, not enforcement: {description}"
        );
        assert!(
            description.contains("shouldWait") && description.contains("recommendedParallelism"),
            "must name the two fields an agent branches on: {description}"
        );
    }

    /// The census description must carry the do-not-sum warning. A real census
    /// summed to 59.9% of one core against an honest 49.2% — a 10.7-point
    /// overstatement that looks like a plausible answer.
    #[test]
    fn the_census_tool_warns_against_summing_percentages() {
        let tools = tool_definitions(false);
        let census = tools
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["name"] == "census")
            .expect("census tool");
        let description = census["description"].as_str().unwrap();
        assert!(description.contains("NEVER sum"), "{description}");
        assert!(description.contains("monitorTotal"), "{description}");
    }

    /// Arguments are forwarded as a query string, and datetimes survive it. An
    /// unencoded `+` decodes to a space server-side. Mutation-proof: return
    /// `s.to_string()` from `urlencode` and this fails.
    #[test]
    fn arguments_become_an_encoded_query_string() {
        let args = json!({ "limit": 40, "from": "2026-08-31T15:04:05.123456+01:00" });
        let params = query_from(&args, &["limit", "from", "to"]);
        assert_eq!(
            params,
            vec![
                ("limit", "40".to_string()),
                ("from", "2026-08-31T15:04:05.123456+01:00".to_string()),
            ],
            "absent parameters must be omitted, not sent empty"
        );
        let encoded = urlencode(&params[1].1);
        assert!(
            !encoded.contains('+'),
            "a raw + decodes to a space: {encoded}"
        );
        assert!(encoded.contains("%3A"), "colons must be encoded: {encoded}");
    }

    /// Agents send `limit` as a JSON number OR a string; both must work, and
    /// anything else must be ignored rather than sent as `limit=null`.
    #[test]
    fn a_limit_is_accepted_as_a_number_or_a_string() {
        assert_eq!(
            scalar_arg(&json!({"limit": 40}), "limit").as_deref(),
            Some("40")
        );
        assert_eq!(
            scalar_arg(&json!({"limit": "40"}), "limit").as_deref(),
            Some("40")
        );
        assert_eq!(scalar_arg(&json!({"limit": null}), "limit"), None);
        assert_eq!(scalar_arg(&json!({"limit": [40]}), "limit"), None);
        assert_eq!(scalar_arg(&json!({}), "limit"), None);
    }

    /// A tool result must be the API's bytes, not a re-serialized value.
    ///
    /// This is the float bug the parity harness caught, pinned in-process: an
    /// agent asked to reason about `cpuSecsTotal` was being handed a number one
    /// unit in the last place away from what the API computed, with nothing in the
    /// payload to indicate it.
    ///
    /// Mutation-proof, verified: put `serde_json::to_string_pretty(&result)` back
    /// into the `tools/call` success arm — which is what `handle_tool_call`
    /// returning a `Value` forced — and the round trip below is what the agent
    /// receives, so this fails.
    #[test]
    fn a_tool_result_relays_the_api_bytes_unchanged() {
        let raw = r#"{"monitorTotal":{"cpuSecsTotal":15640.710000000001}}"#;
        // The success arm wraps the tool's `String` directly.
        let wrapped = json!({ "content": [{ "type": "text", "text": raw }] });
        assert_eq!(wrapped["content"][0]["text"].as_str().unwrap(), raw);

        // The mutation, executed: parsing and re-serializing changes the bytes.
        let parsed: Value = serde_json::from_str(raw).unwrap();
        let round_tripped = serde_json::to_string(&parsed).unwrap();
        assert_ne!(
            round_tripped, raw,
            "if a Value round trip were lossless this test would prove nothing"
        );
        assert!(
            !round_tripped.contains("15640.710000000001"),
            "the ULP loss must be reproducible here: {round_tripped}"
        );
    }

    #[test]
    fn an_unknown_tool_is_an_error_not_a_panic() {
        let api = Api::from_env();
        let err = handle_tool_call(&api, "close_note", &json!({})).unwrap_err();
        assert!(err.contains("unknown tool"), "{err}");
    }

    // ---- the stdio JSON-RPC protocol loop (handle_line / handle_request) ----
    //
    // These arms never touch the network, so they run without a daemon. The
    // network-dependent error translation is covered by tests/mcp_integration.rs
    // (a mock HTTP server + the real binary over a pipe).

    /// A malformed line is a JSON-RPC parse error (-32700) with a null id, not a
    /// panic and not silence. Mutation-proof: return `None` from the `Err` arm of
    /// `handle_line` and this fails — the client would hang waiting for a reply.
    #[test]
    fn a_malformed_line_is_a_parse_error() {
        let api = Api::from_env();
        let reply = handle_line(&api, "{not json").expect("a parse error is owed a reply");
        assert_eq!(reply["error"]["code"], -32700);
        assert_eq!(reply["id"], Value::Null);
        assert!(
            reply["error"]["message"]
                .as_str()
                .unwrap()
                .contains("parse"),
            "{reply}"
        );
    }

    /// A blank line owes no reply — it is keepalive noise, not a request.
    #[test]
    fn a_blank_line_gets_no_reply() {
        let api = Api::from_env();
        assert!(handle_line(&api, "   ").is_none());
    }

    /// A notification (no `id`) gets NO response, per JSON-RPC — even for a method
    /// we would otherwise answer. Mutation-proof: change `req.id.clone()?` to
    /// substitute a default id and this fails, because we would reply to a
    /// notification and desync the client's id accounting.
    #[test]
    fn a_notification_gets_no_reply() {
        let api = Api::from_env();
        assert!(handle_line(&api, r#"{"jsonrpc":"2.0","method":"ping"}"#).is_none());
    }

    /// An unknown METHOD (not tool) is JSON-RPC -32601, distinct from an unknown
    /// tool (which is a tools/call result with isError). Mutation-proof: route the
    /// fallthrough to a success `response` and this fails.
    #[test]
    fn an_unknown_method_is_method_not_found() {
        let api = Api::from_env();
        let reply = handle_line(&api, r#"{"jsonrpc":"2.0","id":1,"method":"tools/delete"}"#)
            .expect("a reply is owed");
        assert_eq!(reply["error"]["code"], -32601);
        assert_eq!(reply["id"], 1);
    }

    /// `initialize` reports the protocol version and names the server — the
    /// handshake a client keys on before it will send anything else.
    #[test]
    fn initialize_returns_the_handshake() {
        let api = Api::from_env();
        let reply = handle_line(&api, r#"{"jsonrpc":"2.0","id":0,"method":"initialize"}"#)
            .expect("a reply is owed");
        assert_eq!(reply["result"]["protocolVersion"], PROTOCOL_VERSION);
        assert_eq!(reply["result"]["serverInfo"]["name"], "banshee-mcp");
        assert!(reply["result"]["capabilities"]["tools"].is_object());
    }

    /// A tools/call for an unknown tool is a RESULT with `isError: true` (so the
    /// agent sees the reason in the content), NOT a JSON-RPC protocol error —
    /// those are different channels. Mutation-proof: move the unknown-tool case
    /// to `error_response` and this fails on the `isError` assertion.
    #[test]
    fn an_unknown_tool_call_is_a_result_with_iserror() {
        let api = Api::from_env();
        let reply = handle_line(
            &api,
            r#"{"jsonrpc":"2.0","id":7,"method":"tools/call","params":{"name":"nope"}}"#,
        )
        .expect("a reply is owed");
        assert!(
            reply.get("error").is_none(),
            "not a protocol error: {reply}"
        );
        assert_eq!(reply["result"]["isError"], true);
        assert!(
            reply["result"]["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("unknown tool")
        );
    }

    /// A file-backed key is RE-READ on every resolve, so a rotation is picked up by a
    /// running server (`banshee-dqh`).
    ///
    /// This is the property whose absence broke a live session: the MCP server held
    /// the key it read at startup, the daemon rotated it, and every tool call 401'd for
    /// the rest of the session while the daemon stayed healthy.
    ///
    /// The fixture CHANGES THE FILE between the two resolves. Asserting only that
    /// `resolve()` returns the file's contents once would pass with a value captured at
    /// construction — it cannot distinguish "read the file" from "re-read the file",
    /// which is the whole distinction.
    #[test]
    fn a_file_backed_key_is_reread_on_every_resolve() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("api_key");
        std::fs::write(&path, "old-key\n").unwrap();
        let src = KeySource::File(path.clone());
        assert_eq!(src.resolve(), "old-key");

        // The daemon rotates the file underneath us.
        std::fs::write(&path, "new-key\n").unwrap();
        assert_eq!(
            src.resolve(),
            "new-key",
            "a rotated key file must be picked up without restarting"
        );
    }

    /// An explicit key is NOT re-read — there is no file, and the user pinned it.
    /// Pairs with the test above: "always read from disk" would break an explicit
    /// `BANSHEE_API_KEY`, which is the caller's own deliberate choice.
    #[test]
    fn an_explicit_key_is_used_verbatim_and_never_reread() {
        let src = KeySource::Fixed("pinned".to_string());
        assert_eq!(src.resolve(), "pinned");
        assert_eq!(src.resolve(), "pinned");
    }

    /// A missing or unreadable key file resolves to EMPTY rather than panicking. The
    /// daemon will answer 401 and the agent gets a clear error — a crash in an MCP
    /// server takes the whole tool surface down instead.
    #[test]
    fn a_missing_key_file_resolves_empty_rather_than_panicking() {
        let src = KeySource::File(std::path::PathBuf::from("/nonexistent/banshee/api_key"));
        assert_eq!(src.resolve(), "");
    }
}
