// Integration tests: boot the real router on an ephemeral port with a temp
// database and drive it over real HTTP. No mocks — this is the same surface
// the CLI, the MCP server and the Swift app hit.

use std::sync::{Arc, Mutex};

use banshee_api::{AppState, Transport, build_router};
use banshee_core::actions::ActionRunner;
use banshee_core::census::config::CensusConfig;
use banshee_core::census::exec::{CommandRunner, RealRunner};
use banshee_core::pressure::band::Band;
use banshee_core::pressure::config::Dimension;
use banshee_core::pressure::episode::{AlertEpisode, EpisodePeak, EpisodeState};
use banshee_core::pressure::level::Level;
use banshee_core::sample::Sample;
use banshee_core::sys::{SysReading, VolumeReading};
use banshee_core::{CoreError, Result as CoreResult};
use banshee_core::{Headroom, Pressure, SampleStore};
use chrono::{DateTime, Utc};

const KEY: &str = "test-key-not-secret";
const BOOT: i64 = 1_788_100_000;

fn at(secs: i64) -> DateTime<Utc> {
    DateTime::from_timestamp(BOOT + secs, 0).unwrap()
}

/// A plausible reading of a calm machine.
fn reading() -> SysReading {
    SysReading {
        ncpu: 8,
        mem_total_bytes: 25_769_803_776,
        page_size: 16384,
        boot_time_secs: BOOT,
        load_1m: 4.0,
        load_5m: 4.0,
        load_15m: 4.0,
        swap_total_bytes: 8_589_934_592,
        swap_used_bytes: 1_000_000_000,
        pages_free: 400_000,
        pages_active: 200_000,
        pages_inactive: 200_000,
        pages_wired: 100_000,
        pages_speculative: 0,
        pages_compressor: 10_000,
        pages_purgeable: 0,
        pages_external: 0,
        swapins: 1_000,
        swapouts: 2_000,
        memory_pressure_level: 1,
        thermal_pressure_level: Some(0),
        cpu_speed_limit_percent: None,
        cpu_ticks_total: Some(80_000_000),
        cpu_ticks_idle: Some(60_000_000),
        kernel_free_percent: Some(50),
        jetsam_kills: Some(0),
    }
}

fn volumes() -> Vec<VolumeReading> {
    vec![VolumeReading {
        mount_point: "/System/Volumes/Data".into(),
        total_bytes: 494_384_795_648,
        avail_bytes: 100_000_000_000,
    }]
}

struct TestServer {
    base: String,
    client: reqwest::Client,
    store: Arc<Mutex<SampleStore>>,
    pressure: Arc<Mutex<Option<Pressure>>>,
    headroom: Arc<Mutex<Option<Headroom>>>,
    ops: Arc<Mutex<banshee_api::OpsHealth>>,
    _dir: tempfile::TempDir,
}

impl TestServer {
    async fn start() -> Self {
        Self::start_with_actions(ActionRunner::new(
            Box::new(RealRunner),
            CensusConfig::default(),
        ))
        .await
    }

    /// Boot with a specific `ActionRunner` — the action tests hand it one built
    /// on a `FakeRunner` so exercising execute cannot touch the real machine.
    async fn start_with_actions(actions: ActionRunner) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("banshee.db");
        // ONE store on the file. The Note slice's second connection went away
        // with the slice at schema v6; AppState has no place to put a second.
        let store = Arc::new(Mutex::new(SampleStore::open(&db).unwrap()));
        let pressure = Arc::new(Mutex::new(None));
        let headroom = Arc::new(Mutex::new(None));
        let ops = Arc::new(Mutex::new(banshee_api::OpsHealth::default()));
        let state = AppState {
            samples: Arc::clone(&store),
            pressure: Arc::clone(&pressure),
            headroom: Arc::clone(&headroom),
            actions: Arc::new(actions),
            api_key: KEY.into(),
            db_path: db.clone(),
            ops: Arc::clone(&ops),
            pressure_config: banshee_core::pressure::config::PressureConfig::default(),
        };
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        // The SOCKET flavour of the router, served over a TCP listener so `reqwest`
        // can drive it. The router keys on the `Transport` tag, not on how a request
        // arrived — so this exercises exactly the route table and auth the socket
        // gets, and `main` pairing the tag with the right listener is pinned
        // separately (`the_key_is_refused_over_tcp_and_accepted_over_the_socket`
        // below, and e2e against the real binary).
        tokio::spawn(async move {
            axum::serve(listener, build_router(state, Transport::Unix))
                .await
                .unwrap();
        });
        Self {
            base,
            client: reqwest::Client::new(),
            store,
            pressure,
            headroom,
            ops,
            _dir: dir,
        }
    }

    async fn get(&self, path: &str) -> reqwest::Response {
        self.client
            .get(format!("{}{path}", self.base))
            .header("x-api-key", KEY)
            .send()
            .await
            .unwrap()
    }

    async fn json(&self, path: &str) -> serde_json::Value {
        let resp = self.get(path).await;
        assert!(
            resp.status().is_success(),
            "GET {path} returned {}",
            resp.status()
        );
        resp.json().await.unwrap()
    }

    /// POST with an optional raw body (used for the action routes).
    async fn post(&self, path: &str, body: Option<&str>) -> reqwest::Response {
        let mut req = self
            .client
            .post(format!("{}{path}", self.base))
            .header("x-api-key", KEY);
        if let Some(b) = body {
            req = req
                .header("content-type", "application/json")
                .body(b.to_string());
        }
        req.send().await.unwrap()
    }

    /// Write `count` samples 15s apart, oldest first, as the sampler would.
    fn seed_samples(&self, count: i64) {
        let store = self.store.lock().unwrap();
        for i in 0..count {
            store
                .insert(&Sample::from_reading(at(i * 15), &reading(), &volumes()))
                .unwrap();
        }
    }

    fn publish(&self, p: Pressure) {
        *self.pressure.lock().unwrap() = Some(p);
    }

    fn publish_headroom(&self, h: Headroom) {
        *self.headroom.lock().unwrap() = Some(h);
    }
}

// ---- /health --------------------------------------------------------------

#[tokio::test]
async fn health_is_open_and_reports_version() {
    let s = TestServer::start().await;
    // Deliberately no api key header.
    let resp = reqwest::get(format!("{}/health", s.base)).await.unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["status"], "ok");
    assert!(body["version"].is_string());
    // The build stamp that makes the running daemon traceable (banshee-b2w).
    // A 7-hex-char short rev, optionally `+dirty`, or "unknown" outside a repo;
    // never absent, or an operator cannot tell what is serving.
    let rev = body["gitRev"].as_str().expect("gitRev must be present");
    assert!(!rev.is_empty(), "gitRev must not be blank");
}

/// `/health` is GET-only. A POST must be refused (405), not silently accepted —
/// this is load-bearing for the CSRF story (threat-model): the only open route
/// stays a read, so a browser's keyless simple-POST reaches nothing. Guards
/// against a future `.route("/health", post(...))` slip.
#[tokio::test]
async fn health_rejects_a_post() {
    let s = TestServer::start().await;
    let resp = reqwest::Client::new()
        .post(format!("{}/health", s.base))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 405, "/health must be GET-only");
}

/// `/health` must mean "can serve requests", not "process exists" — so it TOUCHES
/// the store. Mutation-proof: replace the store probe with a bare `true` and this
/// still passes, so it is paired with the poisoned-mutex test in `lib.rs`; what
/// this pins is that the probe does not make a healthy server look degraded.
#[tokio::test]
async fn health_stays_ok_with_a_populated_store() {
    let s = TestServer::start().await;
    s.seed_samples(3);
    assert_eq!(s.json("/health").await["status"], "ok");
}

// ---- /backup --------------------------------------------------------------

/// A verified snapshot lands in a backups/ dir beside the live DB and reports
/// its path + row count. Exercises the REAL backup_verified path (SQLite online
/// backup + out-of-place re-open), which was dead code until this route landed.
#[tokio::test]
async fn backup_writes_a_verified_snapshot_and_reports_it() {
    let s = TestServer::start().await;
    s.seed_samples(3);

    let resp = s.post("/backup", None).await;
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();

    assert_eq!(body["samples"], 3, "the snapshot's verified row count");
    let path = body["path"].as_str().expect("a path");
    assert!(path.contains("/backups/"), "lands in backups/: {path}");
    assert!(
        std::path::Path::new(path).exists(),
        "the snapshot file must actually exist at {path}"
    );
    // …and it is an independently-openable banshee database with the rows.
    let restored = SampleStore::open(std::path::Path::new(path)).unwrap();
    assert_eq!(restored.count_samples().unwrap(), 3);
}

/// The backup route is keyed like every other write — an unauthenticated POST
/// is refused, so a browser (or anything without the key) cannot make the
/// daemon write files.
#[tokio::test]
async fn backup_requires_the_api_key() {
    let s = TestServer::start().await;
    let resp = reqwest::Client::new()
        .post(format!("{}/backup", s.base))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);
}

// ---- /pressure ------------------------------------------------------------

/// **The most important test in this file.** Before the sampler's first
/// evaluation, `/pressure` must answer `Checking` — never a confident `Quiet`.
///
/// Mutation-proof, verified: change `AppState::pressure_now` to fall back to a
/// `Quiet` verdict and this fails on both assertions. A menu bar that says 😴
/// before anything has been measured teaches its user to distrust the one glyph
/// that has to be trusted (ADR-0005).
#[tokio::test]
async fn pressure_before_the_first_evaluation_is_checking_not_quiet() {
    let s = TestServer::start().await;
    let body = s.json("/pressure").await;

    assert_eq!(body["level"], "checking");
    assert_ne!(body["level"], "quiet");
    assert_eq!(body["levelName"], "Checking");
    assert_eq!(body["glyph"], "🫧");
    assert_ne!(
        body["glyph"], "😴",
        "the sleeping face is a claim about calm"
    );
    assert_eq!(body["source"], serde_json::Value::Null);
    assert_eq!(body["dimensions"], serde_json::json!([]));
    assert_eq!(body["findings"], serde_json::json!([]));
    assert_eq!(body["sampleCount"], 0);
}

/// The published verdict is served BYTE-FOR-BYTE, glyph included. The API is not
/// allowed to re-derive, re-band or re-glyph anything (ADR-0005).
///
/// Mutation-proof: have the handler recompute the glyph from the level (a
/// plausible-looking "tidy-up") and this fails, because the source suffix is
/// dropped.
#[tokio::test]
async fn pressure_serves_the_published_verdict_verbatim() {
    let s = TestServer::start().await;
    let published = Pressure {
        level: Level::Wailing,
        level_name: "Wailing".into(),
        glyph: "😱🧠".into(),
        accessibility_label: "Banshee: Wailing, memory".into(),
        sample_count: 40,
        ..Pressure::checking(at(0))
    };
    s.publish(published.clone());

    let body = s.json("/pressure").await;
    assert_eq!(body["level"], "wailing");
    assert_eq!(body["glyph"], "😱🧠");
    assert_eq!(body["accessibilityLabel"], "Banshee: Wailing, memory");

    // And it decodes back into the exact same value — the round trip a client
    // actually performs.
    let decoded: Pressure = serde_json::from_value(body).unwrap();
    assert_eq!(decoded, published);
}

/// The whole per-dimension payload survives the wire at full depth, camelCase
/// keys and all. Rule 3 of the wire format: nested objects escape scrutiny, and
/// Banshee's payload is nested by nature.
#[tokio::test]
async fn pressure_carries_nested_dimension_objects_in_camel_case() {
    let s = TestServer::start().await;
    let fixture: Pressure = serde_json::from_str(include_str!(
        "../../../tests/fixtures/pressure-shrieking.json"
    ))
    .unwrap();
    s.publish(fixture.clone());

    let body = s.json("/pressure").await;
    let dims = body["dimensions"].as_array().expect("dimensions array");
    assert_eq!(dims.len(), 3);

    let cpu = dims.iter().find(|d| d["key"] == "cpu").expect("cpu");
    for key in [
        "dimension",
        "key",
        "label",
        "band",
        "value",
        "unit",
        "severity",
        "heldSecs",
        "trendPerSec",
        "detail",
        "advisory",
        "pending",
    ] {
        assert!(cpu.get(key).is_some(), "nested key {key} missing: {cpu:#?}");
    }
    assert!(cpu.get("held_secs").is_none(), "snake_case leaked at depth");
    assert!(
        cpu.get("trend_per_sec").is_none(),
        "snake_case leaked at depth"
    );

    // Findings are nested too, and carry the action a client offers as a button.
    let first = &body["findings"][0];
    assert_eq!(first["action"], "relaunchApps");
    assert_eq!(first["band"], "red");
}

/// A green dimension's `trendPerSec` and `pending` are explicit nulls, and a calm
/// machine's `source` is too. Present-as-null binds the ENCODER, not just the
/// decoder — an omitted key is a contract violation even though most decoders
/// tolerate it.
#[tokio::test]
async fn nullable_pressure_fields_are_present_as_null() {
    let s = TestServer::start().await;
    let fixture: Pressure =
        serde_json::from_str(include_str!("../../../tests/fixtures/pressure-quiet.json")).unwrap();
    s.publish(fixture);

    // Read the RAW BYTES, because `Value` cannot distinguish "key absent" from
    // "key present and null" once it has been indexed with a default.
    let text = s.get("/pressure").await.text().await.unwrap();
    let body: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert!(body.as_object().unwrap().contains_key("source"));
    assert_eq!(body["source"], serde_json::Value::Null);

    let cpu = &body["dimensions"][0];
    let obj = cpu.as_object().unwrap();
    assert!(obj.contains_key("trendPerSec"), "key must be present");
    assert_eq!(cpu["trendPerSec"], serde_json::Value::Null);
    assert!(obj.contains_key("pending"), "key must be present");
    assert_eq!(cpu["pending"], serde_json::Value::Null);
}

// ---- /headroom ------------------------------------------------------------

/// Before the first evaluation the decision is "wait" with zero parallelism —
/// headroom is never invented before data exists (ADR-0010 decision 3). Same
/// guard as the Checking verdict, on the agent surface.
#[tokio::test]
async fn headroom_before_the_first_evaluation_is_wait_not_capacity() {
    let s = TestServer::start().await;
    let body = s.json("/headroom").await;
    assert_eq!(body["level"], "checking");
    assert_eq!(body["shouldWait"], true);
    assert_eq!(body["recommendedParallelism"], 0);
    assert!(body["retryAfterSecs"].as_u64().is_some_and(|n| n > 0));
    assert_eq!(body["cores"], serde_json::Value::Null);
    assert_eq!(body["conditions"][0]["type"], "Ready");
    assert_eq!(body["conditions"][0]["status"], "False");
}

/// The published decision is served verbatim — the route does not re-derive it
/// from `/pressure`, which is how the two would come to disagree. Mutation-proof:
/// derive from `pressure_now` in the handler instead of reading `headroom` and
/// this fails (the published pressure is still the Checking placeholder).
#[tokio::test]
async fn headroom_serves_the_published_decision_verbatim() {
    let s = TestServer::start().await;
    let fixture: Headroom = serde_json::from_str(include_str!(
        "../../../tests/fixtures/headroom-throttled.json"
    ))
    .unwrap();
    s.publish_headroom(fixture.clone());

    let text = s.get("/headroom").await.text().await.unwrap();
    let body: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(body["level"], "restless");
    assert_eq!(body["shouldWait"], true);
    assert_eq!(body["retryAfterSecs"], 60);
    assert_eq!(body["cores"], 8);
    // camelCase at depth, the Kubernetes `type` key, and present-as-null `since`
    // on the Unknown conditions.
    let conds = body["conditions"].as_array().unwrap();
    assert_eq!(conds.len(), 7);
    let thermal = conds
        .iter()
        .find(|c| c["type"] == "ThermalPressure")
        .unwrap();
    assert_eq!(thermal["status"], "True");
    assert_eq!(thermal["reason"], "ThermalRed");
    assert!(thermal["since"].is_string());
    let disk = conds.iter().find(|c| c["type"] == "DiskPressure").unwrap();
    assert!(disk.as_object().unwrap().contains_key("since"));
    assert!(disk["since"].is_null());
    assert!(!text.contains("should_wait"), "snake_case leaked: {text}");

    let decoded: Headroom = serde_json::from_value(body).unwrap();
    assert_eq!(decoded, fixture);
}

/// A not-waiting decision carries `retryAfterSecs` as an EXPLICIT null.
#[tokio::test]
async fn a_not_waiting_headroom_has_a_null_retry_not_a_missing_one() {
    let s = TestServer::start().await;
    let fixture: Headroom =
        serde_json::from_str(include_str!("../../../tests/fixtures/headroom-quiet.json")).unwrap();
    s.publish_headroom(fixture);
    let text = s.get("/headroom").await.text().await.unwrap();
    let body: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(body["shouldWait"], false);
    assert_eq!(body["recommendedParallelism"], 3);
    assert!(body.as_object().unwrap().contains_key("retryAfterSecs"));
    assert!(body["retryAfterSecs"].is_null());
}

// ---- /samples -------------------------------------------------------------

#[tokio::test]
async fn samples_are_returned_oldest_first() {
    let s = TestServer::start().await;
    s.seed_samples(5);
    let body = s.json("/samples").await;
    let items = body.as_array().expect("array");
    assert_eq!(items.len(), 5);

    let times: Vec<&str> = items
        .iter()
        .map(|s| s["sampledAt"].as_str().unwrap())
        .collect();
    let mut sorted = times.clone();
    sorted.sort_unstable();
    assert_eq!(times, sorted, "oldest first, so pairs can be diffed");
    // camelCase at depth: the volume rows are nested objects.
    assert_eq!(items[0]["volumes"][0]["mountPoint"], "/System/Volumes/Data");
    assert!(items[0].get("sampled_at").is_none(), "snake_case leaked");
}

#[tokio::test]
async fn samples_honour_an_explicit_limit_returning_the_newest() {
    let s = TestServer::start().await;
    s.seed_samples(10);
    let body = s.json("/samples?limit=3").await;
    let items = body.as_array().unwrap();
    assert_eq!(items.len(), 3);
    // The trailing three: 7*15, 8*15, 9*15 seconds after boot.
    assert_eq!(
        items[0]["sampledAt"],
        banshee_core::wire_time::to_wire(&at(7 * 15))
    );
    assert_eq!(
        items[2]["sampledAt"],
        banshee_core::wire_time::to_wire(&at(9 * 15))
    );
}

/// An empty series is `[]`, never `null` — arrays that are empty are empty arrays
/// (wire format, JSON conventions).
#[tokio::test]
async fn an_empty_series_is_an_empty_array() {
    let s = TestServer::start().await;
    let text = s.get("/samples").await.text().await.unwrap();
    assert_eq!(text.trim(), "[]");
}

/// A window read is half-open `[from, to)`, matching the store, so adjacent
/// windows tile without double-counting the boundary sample.
#[tokio::test]
async fn a_window_read_is_half_open_and_tiles() {
    let s = TestServer::start().await;
    s.seed_samples(6); // t = 0, 15, 30, 45, 60, 75

    let from = banshee_core::wire_time::to_wire(&at(0));
    let mid = banshee_core::wire_time::to_wire(&at(45));
    let to = banshee_core::wire_time::to_wire(&at(90));

    let first = s.json(&format!("/samples?from={from}&to={mid}")).await;
    let second = s.json(&format!("/samples?from={mid}&to={to}")).await;
    assert_eq!(first.as_array().unwrap().len(), 3, "t=0,15,30");
    assert_eq!(second.as_array().unwrap().len(), 3, "t=45,60,75");

    let whole = s.json(&format!("/samples?from={from}&to={to}")).await;
    assert_eq!(
        whole.as_array().unwrap().len(),
        6,
        "the two windows tile exactly, no boundary sample counted twice"
    );
}

/// `to` without `from` is a refusal, not a guess at an open-ended window.
#[tokio::test]
async fn to_without_from_is_a_bad_request() {
    let s = TestServer::start().await;
    let to = banshee_core::wire_time::to_wire(&at(0));
    let resp = s.get(&format!("/samples?to={to}")).await;
    assert_eq!(resp.status(), 400);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(body["error"].as_str().unwrap().contains("from"));
}

/// A window is already bounded, so combining it with `limit` is ambiguous —
/// which end would be dropped? Refuse rather than pick.
#[tokio::test]
async fn a_window_combined_with_limit_is_a_bad_request() {
    let s = TestServer::start().await;
    let from = banshee_core::wire_time::to_wire(&at(0));
    let resp = s.get(&format!("/samples?from={from}&limit=2")).await;
    assert_eq!(resp.status(), 400);
}

#[tokio::test]
async fn a_backwards_window_is_a_bad_request() {
    let s = TestServer::start().await;
    let from = banshee_core::wire_time::to_wire(&at(100));
    let to = banshee_core::wire_time::to_wire(&at(0));
    let resp = s.get(&format!("/samples?from={from}&to={to}")).await;
    assert_eq!(resp.status(), 400);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(body["error"].as_str().unwrap().contains("before"));
}

/// A too-large limit is refused with the ceiling named, rather than silently
/// clamped. An agent that asks for 5,000 samples and receives 500 without being
/// told has to reason about a window it did not get.
#[tokio::test]
async fn an_over_large_limit_is_refused_over_http() {
    let s = TestServer::start().await;
    let resp = s.get("/samples?limit=5000").await;
    assert_eq!(resp.status(), 400);
    let body: serde_json::Value = resp.json().await.unwrap();
    let message = body["error"].as_str().unwrap();
    assert!(message.contains("500"), "must name the ceiling: {message}");
}

// ---- /rollups -------------------------------------------------------------

impl TestServer {
    /// Write `count` rollup buckets 5 minutes apart, as the retention sweep would.
    fn seed_rollups(&self, count: i64) {
        use banshee_core::sample::{Rollup, VolumeRollup};
        let store = self.store.lock().unwrap();
        for i in 0..count {
            store
                .upsert_rollup(&Rollup {
                    bucket_start: at(i * 300),
                    sample_count: 20,
                    load1m_avg: 1.0 + i as f64,
                    load1m_max: 2.0 + i as f64,
                    swap_used_avg: 1_000,
                    swap_used_max: 2_000,
                    pages_free_min: 100,
                    pages_compressor_max: 100,
                    // A reboot inside the last bucket voids its counter deltas, so
                    // one row exercises the null case.
                    swapins_delta: if i == count - 1 { None } else { Some(1) },
                    swapouts_delta: if i == count - 1 { None } else { Some(2) },
                    // Same split for the census scalars: the last bucket had no
                    // census, so its nulls ride the wire as PRESENT nulls.
                    stale_sessions_max: if i == count - 1 { None } else { Some(3) },
                    orphans_max: if i == count - 1 { None } else { Some(14) },
                    monitor_percent_max: if i == count - 1 { None } else { Some(49.2) },
                    total_procs_max: if i == count - 1 { None } else { Some(812) },
                    volumes: vec![VolumeRollup {
                        mount_point: "/System/Volumes/Data".into(),
                        total_bytes: 494_384_795_648,
                        avail_min: 100_000_000_000,
                        avail_avg: 101_000_000_000,
                    }],
                })
                .unwrap();
        }
    }
}

#[tokio::test]
async fn rollups_are_returned_oldest_first_with_their_volumes() {
    let s = TestServer::start().await;
    s.seed_rollups(5);
    let body = s.json("/rollups").await;
    let items = body.as_array().expect("array");
    assert_eq!(items.len(), 5);

    let buckets: Vec<&str> = items
        .iter()
        .map(|r| r["bucketStart"].as_str().unwrap())
        .collect();
    let mut sorted = buckets.clone();
    sorted.sort_unstable();
    assert_eq!(buckets, sorted, "oldest first, like /samples");

    // camelCase at depth, and the averages/maxima that make a rollup a rollup.
    assert!(items[0].get("load1mAvg").is_some());
    assert!(items[0].get("load1mMax").is_some());
    assert!(items[0].get("pagesFreeMin").is_some());
    assert!(items[0].get("load_1m_avg").is_none(), "snake_case leaked");
    assert_eq!(items[0]["volumes"][0]["mountPoint"], "/System/Volumes/Data");
    assert!(items[0]["volumes"][0].get("availMin").is_some());
}

/// A voided counter delta is `null`, not `0`. **Zero swap activity and "a reboot
/// inside this bucket means we cannot know" are different facts**, and a client that
/// renders the second as the first reports a calm five minutes that was never
/// observed.
#[tokio::test]
async fn a_voided_counter_delta_is_null_not_zero() {
    let s = TestServer::start().await;
    s.seed_rollups(3);
    let text = s.get("/rollups").await.text().await.unwrap();
    let body: serde_json::Value = serde_json::from_str(&text).unwrap();
    let last = body.as_array().unwrap().last().unwrap();
    let obj = last.as_object().unwrap();
    assert!(obj.contains_key("swapinsDelta"), "key must be present");
    assert_eq!(last["swapinsDelta"], serde_json::Value::Null);
    assert_ne!(last["swapinsDelta"], serde_json::json!(0));
    // …and a bucket with a valid delta carries the number.
    assert_eq!(body[0]["swapinsDelta"], 1);
}

/// `/rollups` takes the SAME parameters as `/samples`, so a caller has one thing to
/// learn. Mutation-proof: point `list_rollups` at `recent_samples` and the shape
/// assertions above fail; drop the window branch and this fails.
#[tokio::test]
async fn rollups_support_both_a_trailing_limit_and_a_window() {
    let s = TestServer::start().await;
    s.seed_rollups(10);

    let trailing = s.json("/rollups?limit=3").await;
    let items = trailing.as_array().unwrap();
    assert_eq!(items.len(), 3, "the trailing three buckets");
    assert_eq!(
        items[2]["bucketStart"],
        banshee_core::wire_time::to_wire(&at(9 * 300)),
        "…ending at the newest"
    );

    let from = banshee_core::wire_time::to_wire(&at(0));
    let to = banshee_core::wire_time::to_wire(&at(3 * 300));
    let window = s.json(&format!("/rollups?from={from}&to={to}")).await;
    assert_eq!(
        window.as_array().unwrap().len(),
        3,
        "half-open [from, to), so the bucket at `to` is excluded"
    );
}

#[tokio::test]
async fn rollups_reject_the_same_bad_parameters_as_samples() {
    let s = TestServer::start().await;
    let from = banshee_core::wire_time::to_wire(&at(0));
    for (probe, want) in [
        ("/rollups?limit=5000", "500"),
        ("/rollups?limit=nonsense", "limit"),
        ("/rollups?since=yesterday", ""), // unknown params are ignored on a read
    ] {
        let resp = s.get(probe).await;
        if want.is_empty() {
            assert_eq!(resp.status(), 200, "{probe} must be accepted");
            continue;
        }
        assert_eq!(resp.status(), 400, "{probe}");
        let body: serde_json::Value = resp.json().await.unwrap();
        assert!(body["error"].as_str().unwrap().contains(want), "{probe}");
    }
    assert_eq!(s.get(&format!("/rollups?to={from}")).await.status(), 400);
    assert_eq!(
        s.get(&format!("/rollups?from={from}&limit=2"))
            .await
            .status(),
        400
    );
}

/// An empty rollup tier is `[]`, never null — and it IS empty for the first 24
/// hours, because nothing is rolled up until the raw window ages out.
#[tokio::test]
async fn an_empty_rollup_tier_is_an_empty_array() {
    let s = TestServer::start().await;
    s.seed_samples(4);
    let text = s.get("/rollups").await.text().await.unwrap();
    assert_eq!(
        text.trim(),
        "[]",
        "samples exist but nothing is rolled up yet"
    );
}

// ---- /census --------------------------------------------------------------

/// No census yet is a 404 in the `{error}` shape — not `null`, and not an empty
/// census object, which would read as a clean machine.
///
/// Mutation-proof: return `Json(None::<Census>)` instead and this fails on the
/// status, which is the whole point: the CLI maps 404 to exit code 3, so "not yet"
/// stays distinguishable from "nothing running" in a script.
#[tokio::test]
async fn census_before_the_first_reading_is_a_404_in_the_error_shape() {
    let s = TestServer::start().await;
    let resp = s.get("/census").await;
    assert_eq!(resp.status(), 404);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(body["error"].as_str().unwrap().contains("census"));
}

#[tokio::test]
async fn the_latest_census_is_served_with_camel_case_at_depth() {
    let s = TestServer::start().await;
    {
        use banshee_core::census::exec::CommandRunner;
        struct Fake;
        impl CommandRunner for Fake {
            fn run(&self, program: &str, _args: &[&str]) -> banshee_core::Result<String> {
                if program == "ps" {
                    return Ok(
                        "  1     0  14608 21:43:13  10:59.18 ??       /sbin/launchd\n".into(),
                    );
                }
                Ok(String::new())
            }
        }
        let collector = banshee_core::census::CensusCollector::new(
            Fake,
            banshee_core::census::config::CensusConfig::default(),
        );
        let census = collector.collect_at(at(0)).unwrap();
        s.store.lock().unwrap().insert_census(&census).unwrap();
    }

    let body = s.json("/census").await;
    assert_eq!(body["takenAt"], banshee_core::wire_time::to_wire(&at(0)));
    assert!(body.get("totalProcs").is_some());
    assert!(body.get("total_procs").is_none(), "snake_case leaked");
    // Nested objects carry the contract too.
    assert!(body["orphans"].get("orphanCount").is_some());
    assert!(body["monitorTotal"].get("percentOfOneCore").is_some());
    // `tmuxAvailable` is a distinct signal from "zero sessions": the UI must
    // never imply a clean machine when it could not look.
    assert!(body.get("tmuxAvailable").is_some());
}

// ---- /alerts --------------------------------------------------------------

/// A closed episode `started` ago that ran for ten minutes.
fn episode(dimension: Dimension, started: chrono::Duration, message: &str) -> AlertEpisode {
    let started_at = Utc::now() - started;
    AlertEpisode {
        id: uuid::Uuid::new_v4(),
        dimension,
        started_at,
        ended_at: Some(started_at + chrono::Duration::minutes(10)),
        peak: EpisodePeak {
            band: Band::Red,
            level: Level::Wailing,
            severity: 2.0,
            at: started_at,
            message: message.into(),
            who_line: None,
        },
        census_at_peak: None,
        suppressed: 0,
        state: EpisodeState::Closed,
        last_notified_at: Some(started_at),
        recovering_since: Some(started_at + chrono::Duration::minutes(10)),
        projection_bracket_secs: None,
    }
}

#[tokio::test]
async fn alerts_are_returned_newest_first() {
    let s = TestServer::start().await;
    {
        let store = s.store.lock().unwrap();
        for (i, dimension) in [Dimension::Cpu, Dimension::Swap, Dimension::Disk]
            .into_iter()
            .enumerate()
        {
            store
                .upsert_episode(&episode(
                    dimension,
                    chrono::Duration::minutes(30 * (3 - i as i64)),
                    &format!("{} is red", dimension.key()),
                ))
                .unwrap();
        }
    }

    let body = s.json("/alerts").await;
    let items = body.as_array().unwrap();
    assert_eq!(items.len(), 3);
    assert_eq!(items[0]["dimension"], "disk", "newest first");
    assert_eq!(items[2]["dimension"], "cpu");
    assert_eq!(items[0]["state"], "closed");
    assert_eq!(
        items[0]["peak"]["band"], "red",
        "camelCase and nesting hold"
    );
    assert_eq!(items[0]["peak"]["level"], "wailing");
    assert!(items[0].get("startedAt").is_some());
    assert!(items[0].get("endedAt").is_some());
    assert!(items[0].get("started_at").is_none(), "snake_case leaked");
}

/// An OPEN episode is served whatever its age and carries its nullables as
/// explicit nulls — the wire contract's present-as-null rule on the one payload
/// where a null is the most important fact (it means "still going").
#[tokio::test]
async fn an_open_episode_is_served_with_explicit_nulls() {
    let s = TestServer::start().await;
    let mut open = episode(Dimension::Swap, chrono::Duration::days(3), "still going");
    open.ended_at = None;
    open.recovering_since = None;
    open.state = EpisodeState::Firing;
    s.store.lock().unwrap().upsert_episode(&open).unwrap();

    let body = s.json("/alerts").await;
    let items = body.as_array().unwrap();
    assert_eq!(items.len(), 1, "three days old, still open, still served");
    assert!(items[0]["endedAt"].is_null());
    assert!(items[0].get("endedAt").is_some(), "present, not absent");
    assert!(items[0]["recoveringSince"].is_null());
    assert_eq!(items[0]["state"], "firing");
}

/// The default window is 24 hours: an alert older than that is out of scope
/// unless asked for, and `since` reaches it.
#[tokio::test]
async fn the_alert_window_defaults_to_a_day_and_since_reaches_further() {
    let s = TestServer::start().await;
    s.store
        .lock()
        .unwrap()
        .upsert_episode(&episode(
            Dimension::Swap,
            chrono::Duration::days(3),
            "three days ago",
        ))
        .unwrap();

    assert_eq!(
        s.json("/alerts").await.as_array().unwrap().len(),
        0,
        "a three-day-old alert is outside the default 24h window"
    );

    let since = banshee_core::wire_time::to_wire(&(Utc::now() - chrono::Duration::days(7)));
    assert_eq!(
        s.json(&format!("/alerts?since={since}"))
            .await
            .as_array()
            .unwrap()
            .len(),
        1,
        "a wider `since` must reach it"
    );
}

/// A capped alert read drops the OLDEST, not the newest — truncating a
/// newest-first list from the tail. Mutation-proof: truncate from the head
/// (`drain(..excess)`) and this fails, which is the version that hides the alert
/// that just fired.
#[tokio::test]
async fn a_capped_alert_read_keeps_the_newest() {
    let s = TestServer::start().await;
    {
        let store = s.store.lock().unwrap();
        for i in 0..5i64 {
            store
                .upsert_episode(&episode(
                    Dimension::Cpu,
                    chrono::Duration::minutes(15 * i),
                    &format!("alert {i}"),
                ))
                .unwrap();
        }
    }
    let body = s.json("/alerts?limit=2").await;
    let items = body.as_array().unwrap();
    assert_eq!(items.len(), 2);
    assert_eq!(items[0]["peak"]["message"], "alert 0", "the most recent");
    assert_eq!(items[1]["peak"]["message"], "alert 1");
}

#[tokio::test]
async fn a_malformed_since_names_the_parameter() {
    let s = TestServer::start().await;
    let resp = s.get("/alerts?since=yesterday").await;
    assert_eq!(resp.status(), 400);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(body["error"].as_str().unwrap().contains("since"));
}

// ---- /stats ---------------------------------------------------------------

/// Banshee must be able to say what it costs (ADR-0006). Mutation-proof: drop
/// `dbSizeBytes` from the payload and this fails.
#[tokio::test]
async fn stats_reports_counts_size_and_schema_version() {
    let s = TestServer::start().await;
    s.seed_samples(4);

    let body = s.json("/stats").await;
    assert_eq!(body["samples"], 4);
    assert_eq!(body["rollups"], 0);
    assert_eq!(body["censuses"], 0);
    assert_eq!(body["alerts"], 0);
    assert_eq!(
        body["schemaVersion"],
        banshee_core::schema::CURRENT_VERSION,
        "clients need to know which schema they are talking to"
    );
    assert!(
        body["dbSizeBytes"].as_u64().unwrap() > 0,
        "a monitor that will not say what it costs has no standing to complain"
    );
    for key in body.as_object().unwrap().keys() {
        assert!(!key.contains('_'), "snake_case key in /stats: {key}");
    }
}

/// `/stats` surfaces the sampler's operational liveness (banshee-4r1) so a stuck
/// evaluation or a failing sweep is visible over the wire, not just in a log.
/// Before anything has run the timestamps are present-as-null and the failure
/// counts are 0; after failures are recorded the counts reflect them.
/// Mutation-proof: drop `consecutiveSweepFailures` from the payload and this
/// fails — the exact "the sweep is silently failing" signal ADR-0006 needs.
#[tokio::test]
async fn stats_surfaces_sampler_operational_health() {
    let s = TestServer::start().await;

    let body = s.json("/stats").await;
    for key in [
        "lastEvaluatedAt",
        "consecutiveEvalFailures",
        "lastSweptAt",
        "consecutiveSweepFailures",
    ] {
        assert!(body.get(key).is_some(), "missing ops-health key {key}");
    }
    // Nothing has run yet: timestamps null, counts zero.
    assert!(body["lastEvaluatedAt"].is_null());
    assert!(body["lastSweptAt"].is_null());
    assert_eq!(body["consecutiveEvalFailures"], 0);
    assert_eq!(body["consecutiveSweepFailures"], 0);

    // Simulate the sampler recording failures then a good sweep.
    {
        let mut ops = s.ops.lock().unwrap();
        ops.record_eval_failure();
        ops.record_eval_failure();
        ops.record_sweep_ok(chrono::Utc::now());
    }
    let body = s.json("/stats").await;
    assert_eq!(
        body["consecutiveEvalFailures"], 2,
        "a stuck evaluation must be visible; the served verdict is going stale"
    );
    assert!(
        body["lastSweptAt"].is_string(),
        "a successful sweep stamps a time"
    );
    assert_eq!(body["consecutiveSweepFailures"], 0);
}

/// `/stats` says what Banshee ITSELF costs: the daemon's physical footprint
/// and its CPU as a lifetime average over its uptime. The percent is a reduction
/// served on the wire (ADR-0005), and the test pins it against the raw figures
/// beside it — so a payload that emitted a plausible constant, or dropped the
/// division, is distinguishable from the real thing. Mutation-proof: drop
/// `selfCpuPercent` or hardcode it and this fails.
#[tokio::test]
async fn stats_reports_what_banshee_itself_costs() {
    let s = TestServer::start().await;
    // Make sure this process has consumed measurable CPU before asking, or the
    // percent is 0/x and a dropped division is invisible.
    let spin_until = std::time::Instant::now() + std::time::Duration::from_millis(40);
    let mut x = 1u64;
    while std::time::Instant::now() < spin_until {
        x = x.wrapping_mul(6364136223846793005).wrapping_add(1);
    }
    std::hint::black_box(x);

    let body = s.json("/stats").await;
    let footprint = body["selfFootprintBytes"]
        .as_u64()
        .expect("selfFootprintBytes is an integer");
    assert!(
        footprint > 1 << 20,
        "a running daemon is over 1 MB; got {footprint}"
    );
    let cpu = body["selfCpuSecs"]
        .as_f64()
        .expect("selfCpuSecs is a number");
    assert!(
        cpu > 0.0,
        "CPU seconds must reflect the spin above; got {cpu}"
    );
    let up = body["selfUptimeSecs"]
        .as_f64()
        .expect("selfUptimeSecs is fractional seconds");
    assert!(
        up > 0.0 && up < 60.0,
        "a just-started test server; got {up}"
    );
    let pct = body["selfCpuPercent"]
        .as_f64()
        .expect("selfCpuPercent is a number");
    assert!(
        pct.is_finite() && pct > 0.0,
        "percent must be a real average; got {pct}"
    );
    // The percent IS cpu / uptime × 100, computed from the SAME snapshot the raw
    // figures came from — so recomputing it here must agree to float precision.
    // The test server is well under a second old, which is what makes this
    // distinguish "divided by uptime" from "did not divide": at up ≈ 0.02 the two
    // differ by 50×. (Floats on the wire: relative epsilon, never `==`.)
    let expect = cpu / up * 100.0;
    assert!(
        ((pct - expect) / expect).abs() < 1e-6,
        "selfCpuPercent {pct} is not selfCpuSecs/selfUptimeSecs×100 = {expect}"
    );
}

// ---- auth and the error-shape contract ------------------------------------

#[tokio::test]
async fn every_domain_route_requires_the_api_key() {
    let s = TestServer::start().await;
    for path in [
        "/pressure",
        "/headroom",
        "/samples",
        "/rollups",
        "/census",
        "/alerts",
        "/stats",
    ] {
        let resp = reqwest::get(format!("{}{path}", s.base)).await.unwrap();
        assert_eq!(resp.status(), 401, "{path} must require a key");
        let body: serde_json::Value = resp.json().await.unwrap();
        assert!(
            body.get("error").is_some(),
            "{path}: even 401 wears the {{error}} shape"
        );
    }
}

#[tokio::test]
async fn a_wrong_key_is_rejected() {
    let s = TestServer::start().await;
    let resp = reqwest::Client::new()
        .get(format!("{}/pressure", s.base))
        .header("x-api-key", "wrong")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);
}

/// **ADR-0008 Phase 3: the key is refused over TCP and accepted over the socket.**
///
/// One `AppState`, one key, one route, two listeners: the TCP flavour on a loopback
/// port and the socket flavour on a real Unix-domain socket. The SAME keyed `GET
/// /pressure` must 401 on the port — with the transport refusal, not the
/// wrong-key message — and 200 on the socket. Until Phase 3 the port answered 200
/// here, which is what kept every pre-ADR-0008 client silently working.
///
/// What other implementation would also pass this, and how the fixture rules it out:
/// - "check the key everywhere" (Phase 2's behaviour) — the port returns 200. Fails.
/// - "refuse everything" — the socket returns 401. Fails.
/// - "refuse the port's /health too" — the open /health probe is asserted 200.
/// - "the socket flavour stops checking the key" — a wrong key over the socket is
///   asserted 401 with the wrong-key body, so the transport refusal cannot be
///   masking a missing check.
/// - "refuse on the port only when a key is present" — a keyless request on the port
///   is asserted to get the SAME transport refusal, not the wrong-key message.
///
/// Mutation-proof (`.mutations/tcp-refuses-the-key.mut`,
/// `.mutations/socket-accepts-the-key.mut`): make the `Transport::Tcp` arm in
/// `routes::router` use `require_api_key`, or the `Transport::Unix` arm use
/// `refuse_over_tcp`, and this fails on the corresponding assertion.
#[tokio::test]
async fn the_key_is_refused_over_tcp_and_accepted_over_the_socket() {
    let s = TestServer::start().await;
    let state = AppState {
        samples: Arc::clone(&s.store),
        pressure: Arc::clone(&s.pressure),
        headroom: Arc::clone(&s.headroom),
        actions: Arc::new(ActionRunner::new(
            Box::new(RealRunner),
            CensusConfig::default(),
        )),
        api_key: KEY.into(),
        db_path: s._dir.path().join("banshee.db"),
        ops: Arc::clone(&s.ops),
        pressure_config: banshee_core::pressure::config::PressureConfig::default(),
    };

    // The TCP flavour, on a port.
    let tcp = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let tcp_base = format!("http://{}", tcp.local_addr().unwrap());
    let tcp_state = state.clone();
    tokio::spawn(async move {
        axum::serve(tcp, build_router(tcp_state, Transport::Tcp))
            .await
            .unwrap();
    });

    // The socket flavour, on a real Unix-domain socket in the test's temp dir. The
    // client is the same blocking `uds_http` the CLI and MCP server use.
    let sock_path = s._dir.path().join("api.sock");
    let uds = tokio::net::UnixListener::bind(&sock_path).unwrap();
    let uds_state = state;
    tokio::spawn(async move {
        axum::serve(uds, build_router(uds_state, Transport::Unix))
            .await
            .unwrap();
    });
    let uds_client =
        banshee_core::uds_http::UdsHttp::new(sock_path, std::time::Duration::from_secs(5));
    let over_socket = |key: &'static str| {
        let c = uds_client.clone();
        async move {
            tokio::task::spawn_blocking(move || c.request("GET", "/pressure", key, None))
                .await
                .unwrap()
                .unwrap()
        }
    };

    // The same valid key, the same route, the two transports.
    let refused = reqwest::Client::new()
        .get(format!("{tcp_base}/pressure"))
        .header("x-api-key", KEY)
        .send()
        .await
        .unwrap();
    assert_eq!(
        refused.status(),
        401,
        "a VALID key over TCP must be refused (ADR-0008 Phase 3)"
    );
    let body: serde_json::Value = refused.json().await.unwrap();
    assert_eq!(
        body["error"],
        banshee_api::auth::TCP_REFUSAL,
        "the refusal must name the transport, not claim the key was wrong"
    );

    let accepted = over_socket(KEY).await;
    assert_eq!(
        accepted.status, 200,
        "the same key over the socket must be accepted; body: {}",
        accepted.body
    );
    let verdict: serde_json::Value = serde_json::from_str(&accepted.body).unwrap();
    assert_eq!(verdict["level"], "checking", "and it is the real route");

    // The socket flavour still CHECKS the key — the port's refusal is not a missing
    // check wearing a different message.
    let wrong = over_socket("wrong").await;
    assert_eq!(wrong.status, 401);
    assert!(
        wrong.body.contains("missing or invalid"),
        "a wrong key over the socket is the wrong-key rejection, got: {}",
        wrong.body
    );

    // No key over TCP gets the transport refusal too — the port does not accept
    // credentials, so it does not ask for one.
    let keyless = reqwest::get(format!("{tcp_base}/pressure")).await.unwrap();
    assert_eq!(keyless.status(), 401);
    let body: serde_json::Value = keyless.json().await.unwrap();
    assert_eq!(body["error"], banshee_api::auth::TCP_REFUSAL);

    // /health stays open on the port: that is the one thing TCP is still for.
    let health = reqwest::get(format!("{tcp_base}/health")).await.unwrap();
    assert_eq!(health.status(), 200, "/health must survive on TCP");
    let body: serde_json::Value = health.json().await.unwrap();
    assert_eq!(body["status"], "ok");
}

/// The TCP flavour refuses the action routes as well — the kill routes are the
/// reason the key must not travel over the port, so they are checked by name rather
/// than trusted to the `/pressure` case above.
#[tokio::test]
async fn every_keyed_route_is_refused_over_tcp_including_actions() {
    let s = TestServer::start().await;
    let state = AppState {
        samples: Arc::clone(&s.store),
        pressure: Arc::clone(&s.pressure),
        headroom: Arc::clone(&s.headroom),
        actions: Arc::new(ActionRunner::new(
            Box::new(RealRunner),
            CensusConfig::default(),
        )),
        api_key: KEY.into(),
        db_path: s._dir.path().join("banshee.db"),
        ops: Arc::clone(&s.ops),
        pressure_config: banshee_core::pressure::config::PressureConfig::default(),
    };
    let tcp = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let tcp_base = format!("http://{}", tcp.local_addr().unwrap());
    tokio::spawn(async move {
        axum::serve(tcp, build_router(state, Transport::Tcp))
            .await
            .unwrap();
    });
    let client = reqwest::Client::new();
    for path in ["/samples", "/rollups", "/census", "/alerts", "/stats"] {
        let resp = client
            .get(format!("{tcp_base}{path}"))
            .header("x-api-key", KEY)
            .send()
            .await
            .unwrap();
        assert_eq!(
            resp.status(),
            401,
            "keyed GET {path} over TCP must be refused"
        );
    }
    for path in [
        "/actions/reap-stale-sessions/preview",
        "/actions/reap-stale-sessions/execute",
        "/actions/reap-orphans/preview",
        "/actions/reap-orphans/execute",
        "/backup",
    ] {
        let resp = client
            .post(format!("{tcp_base}{path}"))
            .header("x-api-key", KEY)
            .send()
            .await
            .unwrap();
        assert_eq!(
            resp.status(),
            401,
            "keyed POST {path} over TCP must be refused"
        );
        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["error"], banshee_api::auth::TCP_REFUSAL, "{path}");
    }
}

/// **"All non-2xx responses" is literal.** Framework-generated rejections leak
/// `text/plain` by default, and the CLI and MCP server both call `resp.json()` —
/// so a non-JSON error is destroyed into "invalid JSON from API" and the agent
/// loses the one recovery signal it needed.
///
/// Mutation-proof: remove the `error_shape_fallback` layer and the 404/405 cases
/// fail with empty bodies.
#[tokio::test]
async fn no_non_2xx_response_escapes_the_error_shape() {
    let s = TestServer::start().await;

    let cases: Vec<(&str, reqwest::Response)> = vec![
        ("unmatched route", s.get("/no-such-thing").await),
        (
            "wrong method",
            s.client
                .post(format!("{}/pressure", s.base))
                .header("x-api-key", KEY)
                .send()
                .await
                .unwrap(),
        ),
        ("bad query value", s.get("/samples?limit=nonsense").await),
        ("no census yet", s.get("/census").await),
    ];

    for (name, resp) in cases {
        let status = resp.status();
        assert!(!status.is_success(), "{name}: expected a failure status");
        let text = resp.text().await.unwrap();
        let body: serde_json::Value = serde_json::from_str(&text)
            .unwrap_or_else(|e| panic!("{name}: body was not JSON ({e}): {text:?}"));
        assert!(
            body.get("error").and_then(|e| e.as_str()).is_some(),
            "{name}: body must be {{\"error\": string}}, got {text}"
        );
    }
}

/// Unknown query parameters are IGNORED rather than rejected, which is the
/// opposite of the write-DTO rule (`deny_unknown_fields` → 422). Stated as a test
/// because the asymmetry is deliberate and would otherwise look like an
/// oversight: a stray `?foo=1` on a read is harmless, whereas a misspelled field
/// on a write silently loses data.
#[tokio::test]
async fn an_unknown_query_parameter_is_ignored_on_a_read() {
    let s = TestServer::start().await;
    s.seed_samples(2);
    let resp = s.get("/samples?unexpected=1").await;
    assert_eq!(resp.status(), 200);
}

// ---- /actions -------------------------------------------------------------
//
// These drive the write routes end to end over real HTTP against an
// ActionRunner backed by a FakeRunner, so no real process is ever touched. The
// point is the ROUTING and the wire shape; the rails themselves are pinned in
// banshee-core's unit tests.

/// A canned `CommandRunner` for the integration tests. `banshee-core`'s own
/// `FakeRunner` is `#[cfg(test)]` and so invisible across the crate boundary;
/// this is the minimum an integration test needs — replay stdout by command
/// key, never spawn anything.
struct CannedRunner {
    responses: std::collections::HashMap<String, String>,
}

impl CannedRunner {
    fn new(pairs: &[(&str, &str)]) -> Self {
        Self {
            responses: pairs
                .iter()
                .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
                .collect(),
        }
    }
}

impl CommandRunner for CannedRunner {
    fn run(&self, program: &str, args: &[&str]) -> CoreResult<String> {
        let key = if args.is_empty() {
            program.to_string()
        } else {
            format!("{program} {}", args.join(" "))
        };
        // Missing tmux/git keys behave as empty output (no server / clean),
        // matching a real runner's non-zero-exit-is-not-an-error contract. A
        // missing `ps` key is a genuine spawn failure so the error path stays
        // reachable.
        if program == "ps" && !self.responses.contains_key(&key) {
            return Err(CoreError::Schema(format!("cannot run `{program}`: canned")));
        }
        Ok(self.responses.get(&key).cloned().unwrap_or_default())
    }
}

const TMUX_ACTION_CMD: &str = "tmux list-panes -a -F #{session_name}|#{pane_pid}|#{pane_current_command}|#{session_activity}|#{pane_current_path}";
const PS_CORE_CMD: &str = "ps -Ao pid=,ppid=,rss=,etime=,time=,tty=,comm=";
const PS_ARGS_CMD: &str = "ps -Ao pid=,args=";

/// A machine with exactly one reapable session (`old-idle`, idle ~3.4 days,
/// clean worktree) and one spared busy one (`Python`, mid-build). Activity
/// timestamps are relative to the fixed clock the FakeRunner sees through the
/// server's real `Utc::now()`, so ages are large enough to be unambiguously
/// stale whenever the test runs.
fn action_runner() -> ActionRunner {
    let now = Utc::now().timestamp();
    let old = now - 5 * 86_400;
    let tmux = format!(
        "old-idle|100|zsh|{old}|/tmp/clean\n\
         hot|200|Python|{old}|/tmp/build\n"
    );
    let ps_core = "\
33001     1  47104 03:10:00   0:44.20 ??       example-notes-mcp-server
44001     1  22528 02:00:00   0:09.00 ??       rust-language-server
99100 99001   4096    00:03   0:00.02 ttys003  zsh
";
    let ps_args = "\
33001 /Users/example/.local/bin/example-notes-mcp-server
44001 /Users/example/.local/share/nvim/mason/bin/rust-language-server
99100 /bin/zsh
";
    let runner = CannedRunner::new(&[
        (TMUX_ACTION_CMD, &tmux),
        ("git -C /tmp/clean status --porcelain", ""),
        (PS_CORE_CMD, ps_core),
        (PS_ARGS_CMD, ps_args),
    ]);
    ActionRunner::new(Box::new(runner), CensusConfig::default())
        .with_term_wait(std::time::Duration::ZERO)
}

async fn post_json(s: &TestServer, path: &str) -> serde_json::Value {
    let resp = s.post(path, None).await;
    assert!(
        resp.status().is_success(),
        "POST {path} → {}",
        resp.status()
    );
    resp.json().await.unwrap()
}

/// Preview reports without touching the machine; execute reports `executed:true`.
/// The reapable session shows a verdict of `reap`; the busy one is spared.
#[tokio::test]
async fn reap_stale_sessions_preview_and_execute_route_to_core() {
    let s = TestServer::start_with_actions(action_runner()).await;

    let preview = post_json(&s, "/actions/reap-stale-sessions/preview").await;
    assert_eq!(preview["action"], "reapStaleSessions");
    assert_eq!(preview["executed"], false);
    let idle = preview["candidates"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["session"] == "old-idle")
        .expect("old-idle candidate");
    assert_eq!(idle["verdict"], "reap");
    assert!(idle["outcome"].is_null(), "preview outcomes are null");
    assert!(
        preview["candidates"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["session"] == "hot" && c["verdict"] == "spare"),
        "the busy pane must be spared: {preview}"
    );

    let execute = post_json(&s, "/actions/reap-stale-sessions/execute").await;
    assert_eq!(execute["executed"], true);
}

/// Orphan preview lists ppid-1 helpers; execute reports outcomes on each.
#[tokio::test]
async fn reap_orphans_preview_and_execute_route_to_core() {
    let s = TestServer::start_with_actions(action_runner()).await;

    let preview = post_json(&s, "/actions/reap-orphans/preview").await;
    assert_eq!(preview["action"], "reapOrphans");
    let pids: Vec<u64> = preview["candidates"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["pid"].as_u64().unwrap())
        .collect();
    assert_eq!(pids, vec![33001, 44001]);

    let execute = post_json(&s, "/actions/reap-orphans/execute").await;
    assert_eq!(execute["executed"], true);
    assert!(
        execute["candidates"]
            .as_array()
            .unwrap()
            .iter()
            .all(|c| c["outcome"].is_string()),
        "every executed candidate has an outcome: {execute}"
    );
}

/// An empty body executes; a caller-supplied target list is a 422 in the
/// {error} shape. This is the write route bringing back the wrapper the
/// read-only surface deleted (docs/wire-format.md).
#[tokio::test]
async fn execute_accepts_no_body_but_refuses_a_target_list() {
    let s = TestServer::start_with_actions(action_runner()).await;

    let empty = s.post("/actions/reap-orphans/execute", None).await;
    assert_eq!(empty.status(), 200);

    let with_pids = s
        .post(
            "/actions/reap-orphans/execute",
            Some(r#"{"pids": [33001]}"#),
        )
        .await;
    assert_eq!(with_pids.status(), 422, "a target list must be refused");
    let body: serde_json::Value = with_pids.json().await.unwrap();
    assert!(
        body["error"].as_str().unwrap().contains("pids"),
        "the 422 must name the field and wear the error shape: {body}"
    );
}

/// The action routes are keyed like every other non-health route.
#[tokio::test]
async fn action_routes_require_the_api_key() {
    let s = TestServer::start_with_actions(action_runner()).await;
    for path in [
        "/actions/reap-stale-sessions/preview",
        "/actions/reap-stale-sessions/execute",
        "/actions/reap-orphans/preview",
        "/actions/reap-orphans/execute",
    ] {
        let resp = reqwest::Client::new()
            .post(format!("{}{path}", s.base))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 401, "unauthenticated {path}");
    }
}

/// A GET on an action route is a 405 in the {error} shape — actions are POST.
#[tokio::test]
async fn actions_reject_get_with_the_error_shape() {
    let s = TestServer::start_with_actions(action_runner()).await;
    let resp = s.get("/actions/reap-orphans/preview").await;
    assert_eq!(resp.status(), 405);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(
        body.get("error").is_some(),
        "405 must wear the error shape: {body}"
    );
}
