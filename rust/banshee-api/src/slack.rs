// slack — the escalation alert sink. When the machine reaches SHRIEKING, the one
// level that "warrants escalation", the episode notifications are POSTed to a
// Slack incoming webhook so a person away from the menu bar still finds out —
// and since ADR-0009 so is the one recovery notice for an episode that
// escalated, because a person told the machine was shrieking is owed the news
// that it stopped.
//
// Three properties, all load-bearing:
//
// - **Best-effort, never blocking.** A slow or dead webhook must not stall the
//   sampler — the daemon's job is to keep sampling, and a monitor that stops
//   watching because Slack is down is the exact failure it exists to prevent. So
//   delivery is fire-and-forget (`deliver_escalations` spawns) and the sink
//   swallows every error into a log line.
// - **Escalation only.** `deliver_escalations` sends only notifications that
//   `warrants_escalation()` (Shrieking now, or the recovery of a Shrieking-peak
//   episode) — banners already cover Wailing. The filter is a tested unit, not an
//   inline condition, so "which alerts go to Slack" cannot drift silently.
// - **The webhook URL never enters the repo.** It comes from
//   `BANSHEE_SLACK_WEBHOOK_URL`, the same env convention as the census overlay's
//   secrets. Unset → no sink → the daemon runs exactly as before. A set-but-junk
//   value warns loudly and disables the sink rather than crashing the daemon.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use banshee_core::pressure::episode::{Notification, NotificationKind};

/// The env var carrying the Slack incoming-webhook URL. Deliberately NOT a
/// config file in the repo — a webhook URL is a credential.
pub const WEBHOOK_ENV: &str = "BANSHEE_SLACK_WEBHOOK_URL";

/// Somewhere to deliver an escalation alert. A trait so the sampler can be driven
/// by a recording fake in tests without a real webhook.
///
/// `deliver` is best-effort by contract: it returns `()`, never an error, because
/// a delivery failure must be invisible to the sampler that called it.
pub trait AlertSink: Send + Sync {
    fn deliver<'a>(
        &'a self,
        notification: &'a Notification,
    ) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>>;
}

/// Spawn best-effort delivery of the ESCALATION alerts in `raised`. Returns
/// immediately; the POSTs happen on detached tasks so a hung webhook cannot
/// stall the sampling loop.
///
/// The escalation filter lives here, as one place, so it is the thing tests pin —
/// a sampler that delivered every raised alert (Wailing banners duplicated into
/// Slack) would be a regression this catches.
pub fn deliver_escalations(sink: &Arc<dyn AlertSink>, raised: &[Notification]) {
    for n in raised.iter().filter(|n| n.warrants_escalation()) {
        let sink = Arc::clone(sink);
        let n = n.clone();
        tokio::spawn(async move { sink.deliver(&n).await });
    }
}

/// The Slack payload for one notification — a pure function so its wording is
/// testable without a network. Slack incoming webhooks take `{"text": "..."}`.
pub fn slack_payload(n: &Notification) -> serde_json::Value {
    // The message the finding/episode already composed, not a second phrasing —
    // the same rule the banner and the worklist follow (ADR-0005).
    let (icon, headline) = match n.kind {
        NotificationKind::Recovered => (":white_check_mark:", "recovered"),
        NotificationKind::Reescalated => (":rotating_light:", "critical — worsening"),
        NotificationKind::Repeat => (":rotating_light:", "still critical"),
        NotificationKind::Opened => (":rotating_light:", "critical"),
    };
    // The suppressed count is the `banshee-nvd` fix on this surface too: "+39
    // more" is the difference between one storm and forty.
    let more = if n.suppressed > 0 {
        format!(" (+{} more)", n.suppressed)
    } else {
        String::new()
    };
    let text = format!(
        "{icon} *Banshee — {} {headline}*{more}\n{}\n_{}, {}_",
        n.dimension.key(),
        n.message,
        n.level.name(),
        n.at.format("%Y-%m-%d %H:%M:%S UTC")
    );
    serde_json::json!({ "text": text })
}

/// A Slack incoming-webhook sink.
pub struct SlackSink {
    webhook: String,
    http: reqwest::Client,
}

impl SlackSink {
    /// Build a sink from the environment, or `None` when no webhook is
    /// configured. A set-but-invalid value is a WARNING and `None`, not a crash:
    /// the sink is a delivery convenience, and a typo in it must not stop the
    /// sentinel from sampling.
    pub fn from_env() -> Option<Self> {
        let raw = std::env::var(WEBHOOK_ENV).ok()?;
        Self::from_url(raw.trim())
    }

    /// Validate a webhook URL and build the sink. `None` for empty or non-https
    /// input — Slack webhooks are always https, and a scheme-less or http URL is
    /// almost certainly a mistake that would silently never deliver.
    pub fn from_url(url: &str) -> Option<Self> {
        if url.is_empty() {
            return None;
        }
        if !url.starts_with("https://") {
            tracing::warn!(
                "{WEBHOOK_ENV} is set but is not an https URL; the Slack sink is disabled"
            );
            return None;
        }
        Some(Self::new(url.to_string()))
    }

    /// Build a sink for an already-trusted URL, skipping the scheme check. The
    /// validating entry points (`from_env`/`from_url`) are how production builds
    /// one; this exists so a test can point the sink at a local http mock.
    pub fn new(webhook: String) -> Self {
        let http = reqwest::Client::builder()
            // A bounded timeout so a hung endpoint frees the detached task rather
            // than leaking it forever.
            .timeout(Duration::from_secs(10))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Self { webhook, http }
    }

    /// POST one alert, swallowing every failure into a log line. Never returns an
    /// error: the caller is the sampler, and its correctness must not depend on
    /// Slack being reachable.
    async fn post(&self, n: &Notification) {
        let payload = slack_payload(n);
        match self.http.post(&self.webhook).json(&payload).send().await {
            Ok(resp) if resp.status().is_success() => {
                tracing::info!(
                    dimension = n.dimension.key(),
                    kind = ?n.kind,
                    "escalation alert delivered to Slack"
                );
            }
            Ok(resp) => {
                // A non-2xx from Slack (bad webhook, rate limit) is logged, not
                // retried — the alert is already in the store's year-long memory.
                tracing::warn!(
                    status = resp.status().as_u16(),
                    dimension = n.dimension.key(),
                    "Slack rejected the escalation alert"
                );
            }
            Err(e) => {
                // reqwest's error Display includes the request URL — here the full
                // webhook, a credential. Strip it so the secret never reaches a log.
                tracing::warn!(error = %e.without_url(), "could not deliver escalation alert to Slack");
            }
        }
    }
}

impl AlertSink for SlackSink {
    fn deliver<'a>(
        &'a self,
        notification: &'a Notification,
    ) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>> {
        Box::pin(self.post(notification))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use banshee_core::pressure::config::Dimension;
    use banshee_core::pressure::level::Level;
    use chrono::{DateTime, Utc};
    use std::sync::Mutex;

    fn alert(dimension: Dimension, level: Level) -> Notification {
        notification(NotificationKind::Opened, dimension, level, level)
    }

    fn notification(
        kind: NotificationKind,
        dimension: Dimension,
        level: Level,
        peak_level: Level,
    ) -> Notification {
        Notification {
            kind,
            episode_id: uuid::Uuid::new_v4(),
            dimension,
            level,
            peak_level,
            message: "6.1 GB of swap in use.".into(),
            at: DateTime::<Utc>::from_timestamp(1_788_100_000, 0).unwrap(),
            suppressed: 0,
        }
    }

    /// The payload carries the alert's own message (not a second phrasing) and
    /// names the dimension and level. Mutation-proof: drop the `n.message`
    /// interpolation and this fails.
    #[test]
    fn the_payload_carries_the_alert_message_dimension_and_level() {
        let v = slack_payload(&alert(Dimension::Swap, Level::Shrieking));
        let text = v["text"].as_str().unwrap();
        assert!(text.contains("6.1 GB of swap in use."), "{text}");
        assert!(text.contains("swap"), "{text}");
        assert!(text.contains("Shrieking"), "{text}");
        assert!(
            !text.contains("more"),
            "no suppressed count when nothing was swallowed"
        );
    }

    /// A recovery reads as good news, and a swallowed-count is surfaced.
    /// Mutation-proof: drop the `kind` match and the recovery
    /// wears the rotating light; drop the `more` clause and forty storms read as
    /// one.
    #[test]
    fn the_payload_distinguishes_recovery_and_carries_the_suppressed_count() {
        let mut n = notification(
            NotificationKind::Recovered,
            Dimension::Thrash,
            Level::Quiet,
            Level::Shrieking,
        );
        n.suppressed = 39;
        let text = slack_payload(&n)["text"].as_str().unwrap().to_string();
        assert!(text.starts_with(":white_check_mark:"), "{text}");
        assert!(text.contains("recovered"), "{text}");
        assert!(text.contains("(+39 more)"), "{text}");
        assert!(!text.contains("critical"), "{text}");
    }

    /// `from_url` accepts https and rejects everything else — a scheme-less or
    /// http URL is disabled rather than silently never delivering. Mutation-proof:
    /// drop the `starts_with("https://")` guard and the http/empty cases build a
    /// sink.
    #[test]
    fn from_url_requires_https_and_rejects_junk() {
        assert!(SlackSink::from_url("https://hooks.slack.com/services/T/B/X").is_some());
        assert!(SlackSink::from_url("").is_none());
        assert!(SlackSink::from_url("http://insecure.example/x").is_none());
        assert!(SlackSink::from_url("hooks.slack.com/no-scheme").is_none());
        assert!(SlackSink::from_url("not a url").is_none());
    }

    /// A recording sink for the routing test.
    struct RecordingSink {
        delivered: Arc<Mutex<Vec<Dimension>>>,
    }

    impl AlertSink for RecordingSink {
        fn deliver<'a>(
            &'a self,
            n: &'a Notification,
        ) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>> {
            let delivered = Arc::clone(&self.delivered);
            let dimension = n.dimension;
            Box::pin(async move {
                delivered.lock().unwrap().push(dimension);
            })
        }
    }

    /// ONLY escalation (Shrieking) alerts are delivered. A Wailing alert — worth a
    /// banner, already delivered that way — must NOT also go to Slack.
    ///
    /// Mutation-proof: drop the `warrants_escalation()` filter in
    /// `deliver_escalations` and the Wailing alert appears in the delivered set.
    #[tokio::test]
    async fn only_shrieking_alerts_are_delivered() {
        let delivered = Arc::new(Mutex::new(Vec::new()));
        let sink: Arc<dyn AlertSink> = Arc::new(RecordingSink {
            delivered: Arc::clone(&delivered),
        });

        let raised = vec![
            alert(Dimension::Cpu, Level::Wailing),     // banner-only
            alert(Dimension::Swap, Level::Shrieking),  // escalates
            alert(Dimension::Memory, Level::Restless), // below notification
            alert(Dimension::Disk, Level::Shrieking),  // escalates
            // The all-clear for an episode that escalated goes to the same channel;
            // the all-clear for one that only ever wailed does not.
            notification(
                NotificationKind::Recovered,
                Dimension::Thrash,
                Level::Quiet,
                Level::Shrieking,
            ),
            notification(
                NotificationKind::Recovered,
                Dimension::Orphans,
                Level::Quiet,
                Level::Wailing,
            ),
        ];
        deliver_escalations(&sink, &raised);

        // Delivery is spawned; poll until the three escalations land.
        for _ in 0..200 {
            if delivered.lock().unwrap().len() >= 3 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        let mut got = delivered.lock().unwrap().clone();
        got.sort_by_key(|d| d.key());
        assert_eq!(
            got,
            vec![Dimension::Disk, Dimension::Swap, Dimension::Thrash],
            "only the Shrieking alerts and the Shrieking-peak recovery"
        );
    }

    /// A dead endpoint must not panic or hang the caller: `deliver` returns after
    /// the bounded timeout with the failure swallowed. This is the best-effort
    /// contract the sampler relies on.
    #[tokio::test]
    async fn a_dead_webhook_is_swallowed_not_propagated() {
        // Port 1 is not listening; the connect fails fast.
        let sink = SlackSink::from_url("https://127.0.0.1:1/services/x").unwrap();
        // Returns `()`, no panic — the whole point.
        sink.deliver(&alert(Dimension::Swap, Level::Shrieking))
            .await;
    }

    /// A `tracing` writer that keeps everything, so a test can assert on what was
    /// actually logged rather than on what the code looks like it logs.
    #[derive(Clone)]
    struct CaptureWriter(Arc<Mutex<Vec<u8>>>);

    impl std::io::Write for CaptureWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for CaptureWriter {
        type Writer = Self;
        fn make_writer(&'a self) -> Self::Writer {
            self.clone()
        }
    }

    /// The capture buffer, behind a GLOBAL subscriber installed exactly once.
    ///
    /// **Not a scoped `with_default`, and that is the whole point.** `tracing` caches
    /// each callsite's `Interest` process-globally, and installing or dropping a scoped
    /// subscriber on ANY thread rebuilds that cache. So with tests running in parallel,
    /// another test's scope ending could re-evaluate this crate's `warn!` callsite
    /// against a thread that has no subscriber, cache it as "never", and leave a
    /// concurrent scoped capture reading an empty buffer. Measured exactly that: this
    /// test passed alone and with `--test-threads=1`, and failed in the parallel suite
    /// with `got: ""`.
    ///
    /// A single global subscriber makes interest stable for the whole binary. Other
    /// tests' output interleaves here, which is fine — the assertions search for
    /// substrings unique to this test.
    fn captured_logs() -> Arc<Mutex<Vec<u8>>> {
        static BUF: std::sync::OnceLock<Arc<Mutex<Vec<u8>>>> = std::sync::OnceLock::new();
        BUF.get_or_init(|| {
            let buf = Arc::new(Mutex::new(Vec::new()));
            let subscriber = tracing_subscriber::fmt()
                .with_writer(CaptureWriter(Arc::clone(&buf)))
                .with_ansi(false)
                .finish();
            // If something else already installed one, capture is impossible and every
            // assertion below would be vacuous — so fail loudly rather than silently.
            tracing::subscriber::set_global_default(subscriber)
                .expect("no other global tracing subscriber may be installed in this test binary");
            buf
        })
        .clone()
    }

    /// **A FAILED delivery must never write the webhook to a log** (`banshee-a4i`).
    ///
    /// `reqwest`'s error `Display` includes the request URL, and for this sink the
    /// request URL IS the credential — so the natural `error = %e` would persist the
    /// webhook into `~/Library/Logs/banshee-api.err.log` on every failed POST, where it
    /// would sit for as long as the log does. `without_url()` is the guard.
    ///
    /// Asserts on CAPTURED LOG OUTPUT, not on the source. A test that reads the code (or
    /// one that only checks `deliver` does not panic, as the dead-webhook test above
    /// does) cannot tell `%e` from `%e.without_url()` — which is exactly the distinction
    /// that matters, and the reason that test did not cover this.
    #[tokio::test]
    async fn a_failed_delivery_never_logs_the_webhook_secret() {
        // A distinctive secret so a substring search cannot pass by accident.
        const SECRET: &str = "T00000000-B11111111-zzzTOPSECRETslacktokenzzz";
        let buf = captured_logs();
        let url = format!("https://127.0.0.1:1/services/{SECRET}");

        SlackSink::from_url(&url)
            .unwrap()
            .deliver(&alert(Dimension::Swap, Level::Shrieking))
            .await;

        let logged = String::from_utf8(buf.lock().unwrap().clone()).unwrap();
        // The delivery must genuinely have failed and been logged, or this test would
        // pass vacuously on an empty buffer — the co-varying-fixture trap in the shape
        // it takes for a log assertion. This assertion is what caught the capture being
        // unreliable in the first place.
        assert!(
            logged.contains("could not deliver escalation alert"),
            "expected the failure to be logged at all, got: {logged:?}"
        );
        assert!(
            !logged.contains(SECRET),
            "the webhook secret reached a log line: {logged:?}"
        );
        // Neither may the host+path leak by another route.
        assert!(
            !logged.contains("/services/"),
            "the webhook path reached a log line: {logged:?}"
        );
    }
}
