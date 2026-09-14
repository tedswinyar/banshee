// The HTTP surface: `/health` open, everything else key-file authed.
//
// Almost every route here is a READ — Banshee's DATABASE writer is a timer, not
// a person; the sampler owns every insert, and nothing below touches the store
// with a write. The `/actions/*` routes are the exception on the MACHINE
// rather than the database: they kill processes, which is why each one exists
// twice — a `preview` that only reports and an `execute` that acts — and why
// execute accepts no parameters: targets are recomputed at execution time, never
// taken from a caller whose list may describe a machine that no longer exists
// (PIDs recycle; see `banshee_core::actions`).
//
// PARITY IS A HARD GATE. Every route below has exactly one CLI subcommand and one
// MCP tool, and all three must return BYTE-IDENTICAL JSON for the singular reads.
// `tests/e2e/run-e2e.sh` iterates one table of triples and enforces it; the table
// is the contract, this file is one third of the implementation.
//
// ONE deliberate exception: `POST /backup` (banshee-3xh) has a CLI subcommand
// but NO MCP tool, not even a gated one — snapshotting the database is operator
// maintenance, and there is no scenario where an autonomous agent should do it
// (a loop would accumulate snapshots unbounded). The carve-out is TESTED, not
// silent: e2e asserts `backup` is absent from `tools/list`, the same both-ways
// discipline the reap-execute gate gets.
//
// Wire format: camelCase at every depth, explicit nulls, typed error mapping
// (docs/wire-format.md).

use std::sync::Arc;

use axum::{
    Json, Router,
    body::Bytes,
    extract::{FromRequest, FromRequestParts, Query, Request, State, rejection::QueryRejection},
    http::{StatusCode, header, request::Parts},
    middleware,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use banshee_core::actions::ActionRunner;
use banshee_core::pressure::episode::AlertEpisode;
use banshee_core::{CoreError, schema, wire_time};
use chrono::{DateTime, Utc};
use serde::Deserialize;

use crate::{AppState, Transport, auth};

/// Trailing samples returned by `GET /samples` with no parameters. At the 15s
/// cadence 60 samples is fifteen minutes — enough to see the shape of a spike
/// without handing an agent a day of history it did not ask for.
const DEFAULT_SAMPLE_LIMIT: usize = 60;
/// Alerts returned by `GET /alerts` with no parameters.
const DEFAULT_ALERT_LIMIT: usize = 100;
/// Trailing buckets returned by `GET /rollups` with no parameters. At the
/// 5-minute bucket width, 60 buckets is five hours — the same "recent enough to be
/// interesting, small enough to hand to an agent" trade as the sample default.
const DEFAULT_ROLLUP_LIMIT: usize = 60;
/// Hard ceiling on any `limit`. A 24-hour window is 5,760 samples, and a bare
/// array of those is roughly 2 MB — past what belongs in an agent's context.
const MAX_LIMIT: usize = 500;
/// Window `GET /alerts` covers when no `since` is given.
const DEFAULT_ALERT_WINDOW_HOURS: i64 = 24;

pub fn router(state: AppState, transport: Transport) -> Router {
    let keyed = Router::new()
        .route("/pressure", get(get_pressure))
        .route("/headroom", get(get_headroom))
        .route("/deltas", get(get_deltas))
        .route("/samples", get(list_samples))
        .route("/rollups", get(list_rollups))
        .route("/census", get(get_census))
        .route("/alerts", get(list_alerts))
        .route("/stats", get(get_stats))
        .route(
            "/actions/reap-stale-sessions/preview",
            post(preview_reap_stale_sessions),
        )
        .route(
            "/actions/reap-stale-sessions/execute",
            post(execute_reap_stale_sessions),
        )
        .route("/actions/reap-orphans/preview", post(preview_reap_orphans))
        .route("/actions/reap-orphans/execute", post(execute_reap_orphans))
        .route("/backup", post(create_backup));

    // The transport decides what guards the keyed routes (ADR-0008 Phase 3). Over the
    // socket the key is checked; over TCP nothing is checked because nothing is
    // ACCEPTED — the routes still exist there so a stale client gets a 401 that names
    // the fix rather than a 404 that looks like a version mismatch.
    let authed = match transport {
        Transport::Unix => keyed.layer(middleware::from_fn_with_state(
            state.clone(),
            auth::require_api_key,
        )),
        Transport::Tcp => keyed.layer(middleware::from_fn(auth::refuse_over_tcp)),
    };

    Router::new()
        .route("/health", get(health))
        .merge(authed)
        // Force axum's built-in empty 404/405 responses into the `{error}`
        // contract shape too, so NO non-2xx path escapes it.
        .layer(middleware::map_response(error_shape_fallback))
        .with_state(state)
}

/// Wire version of API errors: `{"error": "<message>"}` with a meaningful
/// status. The message is safe to expose (validation text, "not found");
/// internal DB/schema detail is generalized to a generic 500 in the
/// `CoreError` mapping so SQLite strings never reach a client.
#[derive(Debug)]
struct ApiError(StatusCode, String);

impl From<CoreError> for ApiError {
    fn from(e: CoreError) -> Self {
        match &e {
            CoreError::NotFound(_) => Self(StatusCode::NOT_FOUND, e.to_string()),
            CoreError::InvalidInput(_) => Self(StatusCode::BAD_REQUEST, e.to_string()),
            // Db / Schema / Io carry internal detail (e.g. a table name from a
            // constraint failure). Log the real error server-side; return a
            // generic message so nothing internal leaks over the wire.
            _ => {
                tracing::error!(error = %e, "internal server error");
                Self(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal server error".into(),
                )
            }
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(serde_json::json!({ "error": self.1 }))).into_response()
    }
}

// --- Extractors that route rejections through the `{error}` contract -------
//
// axum's stock rejections return `text/plain`, which the CLI/MCP `resp.json()`
// cannot parse — an agent that sends a bad parameter gets "invalid JSON from API"
// instead of the real reason. This thin wrapper preserves the rejection's status
// but re-clothes the body as `{"error": ...}`.
//
// The template also wrapped `Json<T>` (write DTOs, 422 on an unknown field) and
// `Path<T>` (400 on a malformed UUID). Both were removed as dead code when the
// surface was read-only; `ApiJson` came BACK with the first write routes (the
// `/actions/*` routes, per the obligation recorded in docs/wire-format.md). `Path<T>`
// is still owed by the first route with a path parameter — none exists yet.

struct ApiQuery<T>(T);

impl<S, T> FromRequestParts<S> for ApiQuery<T>
where
    Query<T>: FromRequestParts<S, Rejection = QueryRejection>,
    S: Send + Sync,
{
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        match Query::<T>::from_request_parts(parts, state).await {
            Ok(Query(value)) => Ok(Self(value)),
            Err(rejection) => Err(ApiError(rejection.status(), rejection.body_text())),
        }
    }
}

/// The write-DTO wrapper (docs/wire-format.md's "unknown-field / malformed-body
/// (422)" clause), restored with the first write routes.
///
/// - **No body at all is fine** — the execute endpoints take no parameters, and
///   demanding an empty `{}` would be ceremony.
/// - **A body that is sent must parse STRICTLY.** Every DTO here uses
///   `deny_unknown_fields`, so a client posting `{"pids": [...]}` — trying to
///   hand an execute endpoint a target list from an earlier preview — is
///   refused with a 422 naming the field, rather than having its list silently
///   ignored while the endpoint kills a different, freshly-computed set. The
///   two behaviours differ exactly when it matters: when the machine changed
///   between preview and execute.
/// - Rejections wear the `{error}` shape, not axum's stock `text/plain`.
#[derive(Debug)]
struct ApiJson<T>(T);

impl<S, T> FromRequest<S> for ApiJson<T>
where
    T: serde::de::DeserializeOwned + Default,
    S: Send + Sync,
{
    type Rejection = ApiError;

    async fn from_request(req: Request, state: &S) -> Result<Self, Self::Rejection> {
        let bytes = Bytes::from_request(req, state)
            .await
            .map_err(|rejection| ApiError(rejection.status(), rejection.body_text()))?;
        if bytes.iter().all(u8::is_ascii_whitespace) {
            return Ok(Self(T::default()));
        }
        serde_json::from_slice(&bytes).map(Self).map_err(|e| {
            ApiError(
                StatusCode::UNPROCESSABLE_ENTITY,
                format!(
                    "invalid request body: {e}. Execute endpoints take no parameters — \
                     targets are recomputed at execution time, never accepted from a caller."
                ),
            )
        })
    }
}

/// Re-clothe axum's built-in EMPTY error responses (unmatched route → 404,
/// wrong method → 405) as `{"error": ...}`. Handler and extractor responses
/// already carry a JSON body + content-type, so they pass through untouched.
async fn error_shape_fallback(response: Response) -> Response {
    let status = response.status();
    let is_empty_builtin = matches!(
        status,
        StatusCode::NOT_FOUND | StatusCode::METHOD_NOT_ALLOWED
    ) && response.headers().get(header::CONTENT_TYPE).is_none();

    if is_empty_builtin {
        let message = if status == StatusCode::NOT_FOUND {
            "no such route"
        } else {
            "method not allowed"
        };
        return (status, Json(serde_json::json!({ "error": message }))).into_response();
    }
    response
}

// --- Query parsing ---------------------------------------------------------
//
// Every parameter arrives as a STRING and is validated here, so a bad value earns
// a clean `400 {error}` from our own code rather than a stock text/plain `Query`
// rejection that names a serde type an agent cannot act on.

/// Parse and range-check a `limit`.
///
/// **A too-large limit is a 400, not a silent clamp.** The template clamped
/// (`n.min(MAX_LIMIT)`), which answers a request for 5,000 rows with 500 and no
/// indication that anything was dropped — an agent then reasons about a window it
/// did not get. Refusing says so.
fn parse_limit(raw: Option<&str>, default: usize) -> Result<usize, ApiError> {
    match raw.map(str::trim) {
        None | Some("") => Ok(default),
        Some(s) => {
            let n: usize = s.parse().map_err(|_| {
                ApiError(
                    StatusCode::BAD_REQUEST,
                    format!("limit must be an integer between 1 and {MAX_LIMIT} (got {s:?})"),
                )
            })?;
            if n == 0 || n > MAX_LIMIT {
                return Err(ApiError(
                    StatusCode::BAD_REQUEST,
                    format!("limit must be between 1 and {MAX_LIMIT} (got {n})"),
                ));
            }
            Ok(n)
        }
    }
}

/// Parse a datetime parameter through the wire contract's GENEROUS decoder, so a
/// client that emits 3 fractional digits (Swift, JavaScript) or a numeric offset
/// is accepted rather than 400ed on a format the contract promises to take.
fn parse_time(raw: Option<&str>, name: &str) -> Result<Option<DateTime<Utc>>, ApiError> {
    match raw.map(str::trim) {
        None | Some("") => Ok(None),
        Some(s) => wire_time::from_wire(s).map(Some).map_err(|_| {
            ApiError(
                StatusCode::BAD_REQUEST,
                format!(
                    "{name} must be an ISO-8601 UTC datetime such as \
                     2026-08-31T15:04:05.123456Z (got {s:?})"
                ),
            )
        }),
    }
}

// --- Handlers --------------------------------------------------------------

async fn health(State(state): State<AppState>) -> impl IntoResponse {
    // Touch the store so /health means "can serve requests", not "process
    // exists". `state.samples()` is poison-tolerant, so a prior handler panic
    // does not turn a working store into a false "degraded".
    let ok = state.samples().count_samples().is_ok();
    let status = if ok { "ok" } else { "degraded" };
    let code = if ok {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };
    (
        code,
        Json(serde_json::json!({
            "status": status,
            "version": env!("CARGO_PKG_VERSION"),
            // The revision this binary was BUILT from (build.rs), `+dirty` when
            // the tree had uncommitted changes, "unknown" outside a git repo.
            // This is the answer to "what is actually running?" — the crate
            // version above is deliberately NOT bumped per release
            // (VERSIONING.md), so it cannot answer that question.
            "gitRev": env!("BANSHEE_GIT_REV"),
        })),
    )
}

/// The verdict. The whole product in one object: level, dominant source, the
/// glyph the menu bar renders verbatim, the per-dimension readings, and the ranked
/// worklist.
///
/// **Never re-evaluates.** The sampler computes this on every sample and publishes
/// it; this handler renders what it was handed. Evaluating here would read a
/// 40-sample window per request and could disagree with the menu bar by a tick.
async fn get_pressure(State(state): State<AppState>) -> Response {
    Json(state.pressure_now(Utc::now())).into_response()
}

/// The verdict reduced to what an agent should DO (ADR-0010): `shouldWait`,
/// `recommendedParallelism`, `retryAfterSecs`, and Kubernetes-shaped
/// `conditions`. A decision, not a number — and advice, not enforcement.
///
/// **Never re-derives.** Published by the sampler from the same evaluation as
/// `/pressure`, so the two cannot describe different machines; before the first
/// evaluation `headroom_now` renders "wait".
async fn get_headroom(State(state): State<AppState>) -> Response {
    Json(state.headroom_now(Utc::now())).into_response()
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct DeltaQuery {
    /// The lookback: `30m`, `1h`, `24h`, `90s`, bare seconds, or `episode`.
    /// Absent means the configured default (`1h`).
    vs: Option<String>,
}

/// What CHANGED since a lookback: the newest census and verdict against
/// then, per app group, per session program, per dimension, plus the sentence.
///
/// A PURE FUNCTION of stored history (ADR-0005) — `banshee_core::pressure::deltas`
/// composes every number and every word, so the CLI, the MCP tool and any view
/// read the same answer. Recomputed on each call over the SAME config the sampler
/// evaluates with (`state.pressure_config`), so its verdict cannot disagree with
/// `/pressure`.
///
/// Two honesties the arithmetic carries rather than hides: a delta across a hole
/// in observation reports the hole (`observationGapSecs`), and "used memory went
/// down" is not "pressure went down" — the verdict rides at both ends.
///
/// `vs=episode` anchors to the earliest OPEN episode and compares against its
/// stored `censusAtPeak`, which survives the sweep that takes the live censuses
/// behind a long incident. With no episode open it is a 400, not an empty answer.
async fn get_deltas(
    State(state): State<AppState>,
    ApiQuery(q): ApiQuery<DeltaQuery>,
) -> Result<Response, ApiError> {
    use banshee_core::pressure::deltas::{self, Lookback};

    let now = Utc::now();
    let config = state.pressure_config().clone();
    let raw =
        q.vs.clone()
            .unwrap_or_else(|| deltas::default_lookback_label(&config));
    let lookback =
        deltas::parse_lookback(&raw, &config).map_err(|m| ApiError(StatusCode::BAD_REQUEST, m))?;

    let (then_at, episode_id, override_summary) = match lookback {
        Lookback::Ago(d) => (now - d, None, None),
        Lookback::Episode => {
            // The earliest open episode: "what changed since this began" is asking
            // about the incident that has been going longest.
            let ep = state
                .samples()
                .open_episodes()?
                .into_iter()
                .min_by_key(|e| e.started_at)
                .ok_or_else(|| {
                    ApiError(
                        StatusCode::BAD_REQUEST,
                        "no open episode to compare against; pass vs=<duration> such as 1h".into(),
                    )
                })?;
            (ep.started_at, Some(ep.id), ep.census_at_peak.clone())
        }
    };

    // Reach back past the anchor by a full window so the anchor verdict has the
    // same slice the sampler evaluates over. `samples_between` is `[from, to)`, so
    // push `to` a second past now to include the latest reading.
    let window_span = chrono::Duration::seconds(
        (config.window_samples as i64 + 5) * config.sample_interval_secs.max(1) as i64,
    );
    let from = then_at - window_span;
    let samples = state
        .samples()
        .samples_between(from, now + chrono::Duration::seconds(1))?;

    // Censuses only live for the raw window (~24h), so a generous trailing count
    // covers everything a delta can reach; cap it so a pathological lookback
    // cannot ask for an unbounded hydration.
    let span_secs = (now - from).num_seconds().max(0) as usize;
    let census_limit = (span_secs / config.census_interval_secs.max(1) as usize + 6).min(4_000);
    let censuses = state.samples().recent_censuses(census_limit)?;

    let deltas = deltas::compute(
        now,
        then_at,
        &raw,
        episode_id,
        override_summary.as_ref(),
        &samples,
        &censuses,
        &config,
    );
    Ok(Json(deltas).into_response())
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SampleQuery {
    limit: Option<String>,
    from: Option<String>,
    to: Option<String>,
}

/// Raw samples, OLDEST FIRST, in one of two modes.
///
/// - no `from` → the trailing `limit` samples
/// - `from` (and optionally `to`, defaulting to now) → that window
///
/// Oldest-first in both modes so a caller can diff consecutive pairs to recover a
/// rate without reversing, which is the whole reason the series exists.
///
/// **Raw only.** Beyond the 24-hour raw window the samples have been rolled up
/// into 5-minute buckets (ADR-0006) and this route returns nothing for them —
/// deliberately, rather than silently substituting a different shape. A rollup is
/// not a sample: it has averages and maxima where a sample has a reading, and a
/// route that returned either depending on the age of the window would be a union
/// type on the wire.
async fn list_samples(
    State(state): State<AppState>,
    ApiQuery(q): ApiQuery<SampleQuery>,
) -> Result<Response, ApiError> {
    let from = parse_time(q.from.as_deref(), "from")?;
    let to = parse_time(q.to.as_deref(), "to")?;

    if from.is_none() && to.is_some() {
        return Err(ApiError(
            StatusCode::BAD_REQUEST,
            "to requires from; pass a window or neither".into(),
        ));
    }

    let samples = match from {
        None => {
            let limit = parse_limit(q.limit.as_deref(), DEFAULT_SAMPLE_LIMIT)?;
            state.samples().recent_samples(limit)?
        }
        Some(from) => {
            if q.limit.is_some() {
                return Err(ApiError(
                    StatusCode::BAD_REQUEST,
                    "limit and from are mutually exclusive: a window is already bounded".into(),
                ));
            }
            let to = to.unwrap_or_else(Utc::now);
            if to < from {
                return Err(ApiError(
                    StatusCode::BAD_REQUEST,
                    "to must not be before from".into(),
                ));
            }
            state.samples().samples_between(from, to)?
        }
    };
    Ok(Json(samples).into_response())
}

/// Five-minute rollups, OLDEST FIRST — the same two modes as `/samples`.
///
/// This is where history older than 24 hours lives. The raw samples are swept at
/// that age and collapsed into these buckets, which are kept for 30 days
/// (ADR-0006), so anything asking "what did last week look like" reads here.
///
/// **Deliberately a separate route rather than a widened `/samples`.** A rollup has
/// `load1mAvg` and `load1mMax` where a sample has `load1m`; a route that returned
/// one shape or the other depending on how old the window was would be a union type
/// on the wire, and every client would need to sniff which it got.
///
/// Parameters mirror `/samples` exactly so there is one thing to learn: `limit` for
/// the trailing N buckets, or `from` (with optional `to`) for a window. A window is
/// NOT row-capped — the retention policy is the bound, and refusing "give me the
/// month" would be refusing the one question this route exists to answer. It is
/// 8,640 buckets at the ceiling, which reads in ~24 ms.
async fn list_rollups(
    State(state): State<AppState>,
    ApiQuery(q): ApiQuery<SampleQuery>,
) -> Result<Response, ApiError> {
    let from = parse_time(q.from.as_deref(), "from")?;
    let to = parse_time(q.to.as_deref(), "to")?;

    if from.is_none() && to.is_some() {
        return Err(ApiError(
            StatusCode::BAD_REQUEST,
            "to requires from; pass a window or neither".into(),
        ));
    }

    let rollups = match from {
        None => {
            let limit = parse_limit(q.limit.as_deref(), DEFAULT_ROLLUP_LIMIT)?;
            state.samples().recent_rollups(limit)?
        }
        Some(from) => {
            if q.limit.is_some() {
                return Err(ApiError(
                    StatusCode::BAD_REQUEST,
                    "limit and from are mutually exclusive: a window is already bounded".into(),
                ));
            }
            let to = to.unwrap_or_else(Utc::now);
            if to < from {
                return Err(ApiError(
                    StatusCode::BAD_REQUEST,
                    "to must not be before from".into(),
                ));
            }
            state.samples().rollups_between(from, to)?
        }
    };
    Ok(Json(rollups).into_response())
}

/// The most recent census: sessions, orphans, app groups, managed agents.
///
/// **404 when none has been taken yet**, rather than `null` or an empty object.
/// The census tier ticks every five minutes and takes its first reading at
/// startup, so this window is short — but during it "no census" is a real state,
/// and an empty census would read as a clean machine. `Checking` does that job for
/// the verdict; here the honest answer is that the resource does not exist.
async fn get_census(State(state): State<AppState>) -> Result<Response, ApiError> {
    let census = state.samples().latest_census()?.ok_or_else(|| {
        ApiError(
            StatusCode::NOT_FOUND,
            "no census has been taken yet; the census tier runs every 5 minutes".into(),
        )
    })?;
    Ok(Json(census).into_response())
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AlertQuery {
    since: Option<String>,
    limit: Option<String>,
}

/// Alert EPISODES, NEWEST FIRST (ADR-0009). Kept for a year (ADR-0006), so this
/// is the long memory: the samples behind a moment are long gone while the
/// episode is still here. Every open episode is included regardless of `since` —
/// a three-day-old episode still firing is exactly what a 24-hour read is asking
/// about.
async fn list_alerts(
    State(state): State<AppState>,
    ApiQuery(q): ApiQuery<AlertQuery>,
) -> Result<Response, ApiError> {
    let limit = parse_limit(q.limit.as_deref(), DEFAULT_ALERT_LIMIT)?;
    let since = parse_time(q.since.as_deref(), "since")?
        .unwrap_or_else(|| Utc::now() - chrono::Duration::hours(DEFAULT_ALERT_WINDOW_HOURS));

    let mut episodes: Vec<AlertEpisode> = state.samples().episodes_since(since)?;
    // Truncate the NEWEST-first list from the tail, so a capped read drops the
    // oldest episodes rather than the ones that just opened.
    episodes.truncate(limit);
    Ok(Json(episodes).into_response())
}

/// What Banshee costs and how much history it holds.
///
/// A monitor that will not say what it costs has no standing to complain about
/// anything else (ADR-0006). `dbSizeBytes` is the file's HIGH-WATER MARK, not the
/// current row count: SQLite marks freed pages reusable rather than returning them
/// to the OS, so the file does not shrink after a retention sweep.
async fn get_stats(State(state): State<AppState>) -> Result<Response, ApiError> {
    let store = state.samples();
    let (samples, rollups, censuses, alerts, db_size) = (
        store.count_samples()?,
        store.count_rollups()?,
        store.count_censuses()?,
        store.count_episodes()?,
        store.size_bytes()?,
    );
    drop(store);

    // Operational liveness (banshee-4r1), so a stuck evaluation or a failing
    // sweep is visible over the wire rather than only in the log. Datetimes use
    // the canonical wire form; nulls are present-as-null (nothing recorded yet).
    let ops = state.ops.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let wire_time = |t: Option<chrono::DateTime<chrono::Utc>>| {
        t.map(|t| serde_json::Value::String(wire_time::to_wire(&t)))
            .unwrap_or(serde_json::Value::Null)
    };

    // What Banshee ITSELF costs: the daemon's physical
    // footprint and its CPU as a lifetime average over its own uptime — the
    // burst-vs-sustained figure `%CPU` cannot give. The percent is a reduction,
    // so it travels on the wire rather than being re-derived by four clients
    // (ADR-0005); the raw seconds ride beside it so nothing is hidden.
    // Uptime is FRACTIONAL seconds on purpose: a client (and the API test)
    // recomputes the percent from the raw figures on the same payload, and a
    // whole-second uptime makes that impossible for the first second of life —
    // which is exactly when the test server asks, and was how a dropped division
    // once passed the pin.
    let me = banshee_core::sys::self_usage()?;
    let uptime_secs = crate::process_started()
        .elapsed()
        .as_secs_f64()
        .max(f64::EPSILON);
    let self_cpu_percent = me.cpu_secs / uptime_secs * 100.0;

    let stats = serde_json::json!({
        "schemaVersion": schema::CURRENT_VERSION,
        "samples": samples,
        "rollups": rollups,
        "censuses": censuses,
        "alerts": alerts,
        "dbSizeBytes": db_size,
        "lastEvaluatedAt": wire_time(ops.last_evaluated_at),
        "consecutiveEvalFailures": ops.consecutive_eval_failures,
        "lastSweptAt": wire_time(ops.last_swept_at),
        "consecutiveSweepFailures": ops.consecutive_sweep_failures,
        "selfFootprintBytes": me.footprint_bytes,
        "selfCpuSecs": me.cpu_secs,
        "selfUptimeSecs": uptime_secs,
        "selfCpuPercent": self_cpu_percent,
    });
    Ok(Json(stats).into_response())
}

// --- Actions -----------------------------------------------------------------
//
// Preview and execute call THE SAME core function pair, which shares one
// candidate selector per action (`banshee_core::actions`) — the handlers add
// nothing but the thread hop. They hop because an action shells out (tmux, git,
// ps, kill) and execute additionally sleeps through the SIGTERM grace period;
// neither belongs on the reactor.

/// The execute endpoints' write DTO: deliberately EMPTY, and strict. The empty
/// struct is the wire-level statement of the recompute rail — there is no field
/// a caller could use to supply targets, and trying one is a 422, not a silent
/// ignore.
#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct ExecuteBody {}

async fn run_action<T, F>(state: AppState, f: F) -> Result<Response, ApiError>
where
    T: serde::Serialize + Send + 'static,
    F: FnOnce(&ActionRunner) -> Result<T, CoreError> + Send + 'static,
{
    let actions = Arc::clone(&state.actions);
    let report = tokio::task::spawn_blocking(move || f(&actions))
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "action task panicked");
            ApiError(
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal server error".into(),
            )
        })??;
    Ok(Json(report).into_response())
}

async fn preview_reap_stale_sessions(State(state): State<AppState>) -> Result<Response, ApiError> {
    run_action(state, |a| a.preview_stale_sessions()).await
}

async fn execute_reap_stale_sessions(
    State(state): State<AppState>,
    ApiJson(ExecuteBody {}): ApiJson<ExecuteBody>,
) -> Result<Response, ApiError> {
    run_action(state, |a| a.execute_stale_sessions()).await
}

async fn preview_reap_orphans(State(state): State<AppState>) -> Result<Response, ApiError> {
    run_action(state, |a| a.preview_orphans()).await
}

async fn execute_reap_orphans(
    State(state): State<AppState>,
    ApiJson(ExecuteBody {}): ApiJson<ExecuteBody>,
) -> Result<Response, ApiError> {
    run_action(state, |a| a.execute_orphans()).await
}

/// Take a VERIFIED snapshot of the database into a daemon-owned `backups/`
/// directory beside the live file, and report where it landed and how many
/// samples it holds.
///
/// **No caller-supplied destination**, deliberately — same posture as the reap
/// execute routes accepting no target list. A path from the wire would let any
/// key holder make the daemon write anywhere its uid can, and would need
/// traversal validation; a timestamped name in a fixed directory needs none.
/// The operator copies the verified snapshot to external media themselves —
/// producing a *verified* copy is the daemon's job (only it may open the DB,
/// ADR-0001); moving a file is not.
///
/// Runs on `spawn_blocking`: the SQLite online-backup copy can take longer than
/// a sub-millisecond query, and holding the store mutex across it would stall
/// the sampler. Fine here because backup is operator-initiated and rare.
async fn create_backup(State(state): State<AppState>) -> Result<Response, ApiError> {
    // `:memory:` (the test-state default) has no parent to write beside, and an
    // in-memory database is nothing to back up. Refuse legibly rather than
    // writing a snapshot into the process's cwd.
    let parent = match state.db_path.parent() {
        Some(p)
            if !state.db_path.as_os_str().is_empty()
                && state.db_path != std::path::Path::new(":memory:") =>
        {
            p.to_path_buf()
        }
        _ => {
            return Err(ApiError(
                StatusCode::BAD_REQUEST,
                "this daemon has no on-disk database to back up".into(),
            ));
        }
    };
    let dest = parent.join("backups").join(format!(
        "banshee-{}.db",
        Utc::now().format("%Y%m%dT%H%M%SZ")
    ));

    let store = Arc::clone(&state.samples);
    let dest_for_task = dest.clone();
    let count = tokio::task::spawn_blocking(move || {
        let guard = store.lock().unwrap_or_else(|p| p.into_inner());
        banshee_core::backup::backup_verified(&guard, &dest_for_task)
    })
    .await
    .map_err(|e| {
        tracing::error!(error = %e, "backup task panicked");
        ApiError(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal server error".into(),
        )
    })??;

    Ok(Json(serde_json::json!({
        "path": dest.to_string_lossy(),
        "samples": count,
    }))
    .into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

    // Mutation-proof (Testing standard): change the generic-500 arm to return
    // `Self(INTERNAL_SERVER_ERROR, e.to_string())` and this fails, because the
    // internal detail would then leak into the body. (Db/Schema/Io all share
    // this arm; Schema needs no rusqlite dependency to construct in a test.)
    #[test]
    fn internal_errors_do_not_leak_to_clients() {
        let leaky = CoreError::Schema("secret_internal_detail: samples.id".into());
        let ApiError(status, body) = leaky.into();
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(body, "internal server error");
        assert!(
            !body.contains("secret_internal_detail"),
            "internal detail must not reach the client"
        );
    }

    #[test]
    fn user_facing_errors_keep_their_message() {
        let ApiError(status, body) = CoreError::NotFound("census".into()).into();
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert!(body.contains("census"));

        let ApiError(status, body) = CoreError::InvalidInput("limit is nonsense".into()).into();
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(body.contains("limit"));
    }

    /// An over-large limit is REFUSED, not silently clamped.
    ///
    /// Mutation-proof, verified: replace the `n > MAX_LIMIT` refusal with
    /// `Ok(n.min(MAX_LIMIT))` — the template's behaviour — and this fails. The two
    /// implementations are indistinguishable to a client that asks for 100 and
    /// distinguishable only above the ceiling, so the fixture has to be above it.
    #[test]
    fn an_over_large_limit_is_refused_rather_than_clamped() {
        let err =
            parse_limit(Some("5000"), DEFAULT_SAMPLE_LIMIT).expect_err("5000 must be refused");
        assert_eq!(err.0, StatusCode::BAD_REQUEST);
        assert!(err.1.contains("500"), "the message must state the ceiling");

        // The boundary itself is allowed — off-by-one in the other direction.
        assert_eq!(parse_limit(Some("500"), 60).unwrap(), MAX_LIMIT);
        assert_eq!(parse_limit(Some("499"), 60).unwrap(), 499);
    }

    #[test]
    fn limit_rejects_zero_garbage_and_absence_falls_back_to_the_default() {
        assert!(parse_limit(Some("0"), 60).is_err());
        assert!(parse_limit(Some("-1"), 60).is_err());
        assert!(parse_limit(Some("many"), 60).is_err());
        assert_eq!(parse_limit(None, 60).unwrap(), 60);
        assert_eq!(parse_limit(Some("   "), 60).unwrap(), 60, "blank == absent");
        assert_eq!(parse_limit(Some(" 7 "), 60).unwrap(), 7, "trimmed");
    }

    /// Datetime parameters go through the contract's GENEROUS decoder, so every
    /// producer in docs/wire-format.md is accepted on the way IN even though only
    /// the canonical form is ever emitted.
    ///
    /// Mutation-proof: parse with a fixed `%Y-%m-%dT%H:%M:%S%.6fZ` format instead
    /// and the 3-digit and numeric-offset cases fail — which is exactly the bug a
    /// Swift or JavaScript client would hit first.
    #[test]
    fn time_parameters_accept_every_producer_the_contract_promises() {
        let canonical = parse_time(Some("2026-08-31T15:04:05.123456Z"), "from")
            .unwrap()
            .unwrap();
        for variant in [
            "2026-08-31T15:04:05.123456Z",
            "2026-08-31T15:04:05.123456+00:00",
        ] {
            assert_eq!(
                parse_time(Some(variant), "from").unwrap().unwrap(),
                canonical,
                "{variant} must decode to the same instant"
            );
        }
        // Three fractional digits (Swift/JS) and no fraction at all are accepted
        // as instants, even though they are not the same instant.
        assert!(parse_time(Some("2026-08-31T15:04:05.123Z"), "from").is_ok());
        assert!(parse_time(Some("2026-08-31T15:04:05Z"), "from").is_ok());

        assert_eq!(parse_time(None, "from").unwrap(), None);
        assert_eq!(parse_time(Some(""), "from").unwrap(), None);
    }

    /// The execute DTO's whole job: absent (or blank) body is fine, and a
    /// caller-supplied target list is REFUSED with the reason — not silently
    /// ignored while the endpoint kills a freshly-computed set instead.
    ///
    /// Mutation-proof twice: drop `deny_unknown_fields` from `ExecuteBody` and
    /// the pids case decodes cleanly; short-circuit the whitespace check and the
    /// absent-body case 422s.
    #[tokio::test]
    async fn an_execute_body_is_optional_but_never_a_target_list() {
        let empty = Request::builder().body(axum::body::Body::empty()).unwrap();
        assert!(
            ApiJson::<ExecuteBody>::from_request(empty, &())
                .await
                .is_ok()
        );

        let blank = Request::builder()
            .body(axum::body::Body::from("  \n"))
            .unwrap();
        assert!(
            ApiJson::<ExecuteBody>::from_request(blank, &())
                .await
                .is_ok()
        );

        let pids = Request::builder()
            .body(axum::body::Body::from(r#"{"pids": [33001, 44001]}"#))
            .unwrap();
        let ApiError(status, body) = ApiJson::<ExecuteBody>::from_request(pids, &())
            .await
            .expect_err("a pid list must be refused");
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert!(body.contains("pids"), "must name the field: {body}");
        assert!(
            body.contains("recomputed at execution time"),
            "must say why: {body}"
        );

        let garbage = Request::builder()
            .body(axum::body::Body::from("not json"))
            .unwrap();
        let ApiError(status, _) = ApiJson::<ExecuteBody>::from_request(garbage, &())
            .await
            .expect_err("garbage must be refused");
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    }

    /// A bad datetime names the parameter AND shows the expected form. An agent
    /// that gets "invalid value" has to guess; one that gets an example retries
    /// correctly. Mutation-proof: drop the `{name}` interpolation and this fails.
    #[test]
    fn a_bad_time_parameter_says_which_one_and_what_was_wanted() {
        let err = parse_time(Some("yesterday"), "since").expect_err("refused");
        assert_eq!(err.0, StatusCode::BAD_REQUEST);
        assert!(
            err.1.contains("since"),
            "must name the parameter: {}",
            err.1
        );
        assert!(err.1.contains("2026-"), "must show the form: {}", err.1);
        assert!(
            err.1.contains("yesterday"),
            "must echo the input: {}",
            err.1
        );
    }
}
