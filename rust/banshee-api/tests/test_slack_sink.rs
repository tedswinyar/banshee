// Integration test for the Slack escalation sink: boot a local mock webhook over
// real HTTP, deliver an alert through the real SlackSink, and assert the exact
// bytes it POSTed. This is the one thing the unit tests can't prove — that the
// client actually sends the payload to the URL — without hitting Slack.

use std::sync::{Arc, Mutex};

use axum::{Router, extract::State, routing::post};
use banshee_api::slack::{AlertSink, SlackSink};
use banshee_core::pressure::config::Dimension;
use banshee_core::pressure::episode::{Notification, NotificationKind};
use banshee_core::pressure::level::Level;
use chrono::{DateTime, Utc};

/// What the mock webhook captured: the raw body of the last POST it received.
type Captured = Arc<Mutex<Option<String>>>;

async fn capture(State(captured): State<Captured>, body: String) -> &'static str {
    *captured.lock().unwrap() = Some(body);
    "ok"
}

/// Boot a one-route mock webhook on an ephemeral port; return its URL and the
/// capture cell.
async fn mock_webhook() -> (String, Captured) {
    let captured: Captured = Arc::new(Mutex::new(None));
    let app = Router::new()
        .route("/hook", post(capture))
        .with_state(Arc::clone(&captured));
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let url = format!("http://{}/hook", listener.local_addr().unwrap());
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (url, captured)
}

fn alert() -> Notification {
    Notification {
        kind: NotificationKind::Opened,
        episode_id: uuid::Uuid::new_v4(),
        dimension: Dimension::Swap,
        level: Level::Shrieking,
        peak_level: Level::Shrieking,
        message: "35.6 GB of swap in use.".into(),
        at: DateTime::<Utc>::from_timestamp(1_788_100_000, 0).unwrap(),
        suppressed: 0,
    }
}

#[tokio::test]
async fn the_sink_posts_the_slack_payload_to_the_webhook() {
    let (url, captured) = mock_webhook().await;
    let sink = SlackSink::new(url);

    sink.deliver(&alert()).await;

    // The body is Slack's `{"text": ...}` shape carrying the alert's own message.
    let body = captured.lock().unwrap().clone();
    let body = body.expect("the webhook must have received a POST");
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    let text = json["text"].as_str().expect("a text field");
    assert!(text.contains("35.6 GB of swap in use."), "{text}");
    assert!(text.contains("swap"), "{text}");
    assert!(text.contains("Shrieking"), "{text}");
}
