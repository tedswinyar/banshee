// Integration tests for the MCP server's HTTP-to-agent error translation.
// The unit tests in main.rs cover the protocol loop without a
// network; these boot a MOCK HTTP server (std only, no tokio) and drive the
// REAL compiled binary over a pipe, so what an agent would actually receive when
// the API misbehaves is pinned end to end:
//
//   - a 5xx with the {error} shape          -> isError, message + status shown
//   - a non-JSON body (proxy/text error)    -> isError, the RAW body (agentapi C1)
//   - the API unreachable                    -> isError, "cannot reach banshee-api"
//   - a healthy 200 JSON body                -> relayed BYTE-for-byte, no isError
//   - /health 503 degraded                   -> the body is SHOWN, not hidden
//
// The 30s request timeout is deliberately NOT exercised (a test that waits 30s
// is worse than the gap); the unreachable case covers the same failure-shaping
// path without the wait.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::process::{Command, Stdio};
use std::thread;

/// A one-shot HTTP mock: serves the SAME canned response to every connection
/// until dropped. Returns the base URL to point the MCP server at.
struct MockApi {
    base: String,
    _handle: thread::JoinHandle<()>,
}

impl MockApi {
    fn serving(status_line: &'static str, content_type: &'static str, body: &'static str) -> Self {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let handle = thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { break };
                // Drain the request headers (up to the blank line) so the client's
                // write completes; we do not route on them — one canned answer.
                let mut buf = [0u8; 1024];
                let _ = stream.read(&mut buf);
                let response = format!(
                    "HTTP/1.1 {status_line}\r\nContent-Type: {content_type}\r\n\
                     Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(response.as_bytes());
                let _ = stream.flush();
            }
        });
        Self {
            base,
            _handle: handle,
        }
    }
}

/// Send one JSON-RPC line to a freshly-spawned banshee-mcp pointed at `base`,
/// and return its single reply line, parsed.
fn call(base: &str, request_line: &str) -> serde_json::Value {
    let mut child = Command::new(env!("CARGO_BIN_EXE_banshee-mcp"))
        .env("BANSHEE_API_URL", base)
        .env("BANSHEE_API_KEY", "test-key")
        // A key FILE fallback would read the real machine's key; force the env
        // path so the test is hermetic.
        .env("BANSHEE_KEY_FILE", "/nonexistent-so-the-env-key-wins")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn banshee-mcp");

    {
        let mut stdin = child.stdin.take().unwrap();
        writeln!(stdin, "{request_line}").unwrap();
        // Drop stdin so the read loop sees EOF and the process exits after replying.
    }

    let mut out = String::new();
    BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut out)
        .expect("read a reply line");
    let _ = child.wait();
    serde_json::from_str(&out).unwrap_or_else(|e| panic!("reply was not JSON: {out:?} ({e})"))
}

const PRESSURE_CALL: &str =
    r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"pressure"}}"#;

#[test]
fn a_5xx_error_reaches_the_agent_as_iserror_with_the_message() {
    let api = MockApi::serving(
        "500 Internal Server Error",
        "application/json",
        r#"{"error":"boom"}"#,
    );
    let reply = call(&api.base, PRESSURE_CALL);
    assert_eq!(reply["result"]["isError"], true, "{reply}");
    let text = reply["result"]["content"][0]["text"].as_str().unwrap();
    assert!(
        text.contains("boom"),
        "the API's message must survive: {text}"
    );
    assert!(text.contains("500"), "the status must be shown: {text}");
}

#[test]
fn a_non_json_body_reaches_the_agent_raw_not_as_invalid_json() {
    // agentapi C1: a text/plain error from a proxy must not be swallowed into a
    // misleading "invalid JSON from API"; the agent needs the real words.
    let api = MockApi::serving("502 Bad Gateway", "text/plain", "upstream is down");
    let reply = call(&api.base, PRESSURE_CALL);
    assert_eq!(reply["result"]["isError"], true, "{reply}");
    let text = reply["result"]["content"][0]["text"].as_str().unwrap();
    assert!(
        text.contains("upstream is down"),
        "raw body must reach the agent: {text}"
    );
}

#[test]
fn an_unreachable_api_reaches_the_agent_as_iserror() {
    // Bind then drop, so the port is (almost certainly) closed — connection refused.
    let port = {
        let l = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        l.local_addr().unwrap().port()
    };
    let base = format!("http://127.0.0.1:{port}");
    let reply = call(&base, PRESSURE_CALL);
    assert_eq!(reply["result"]["isError"], true, "{reply}");
    let text = reply["result"]["content"][0]["text"].as_str().unwrap();
    assert!(
        text.contains("cannot reach banshee-api"),
        "an unreachable daemon must say so, not emit a raw reqwest error: {text}"
    );
}

#[test]
fn a_healthy_body_is_relayed_byte_for_byte() {
    // The ULP-preserving contract: the tool hands back the API's exact bytes, so
    // a float the API computed is not silently altered by a parse/reserialize.
    let body = r#"{"level":"quiet","cpuSecsTotal":15640.710000000001}"#;
    let api = MockApi::serving("200 OK", "application/json", body);
    let reply = call(&api.base, PRESSURE_CALL);
    assert!(
        reply["result"].get("isError").is_none(),
        "not an error: {reply}"
    );
    assert_eq!(
        reply["result"]["content"][0]["text"].as_str().unwrap(),
        body,
        "the API bytes must be relayed unchanged"
    );
}

#[test]
fn health_shows_a_degraded_body_rather_than_hiding_it() {
    // A 503 from /health is a valid answer an agent should SEE (the store is
    // unusable) — not an error to swallow. The health tool returns the body
    // regardless of status.
    let api = MockApi::serving(
        "503 Service Unavailable",
        "application/json",
        r#"{"status":"degraded"}"#,
    );
    let call_line = r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"health"}}"#;
    let reply = call(&api.base, call_line);
    let text = reply["result"]["content"][0]["text"].as_str().unwrap();
    assert!(
        text.contains("degraded"),
        "the degraded body must reach the agent: {text}"
    );
    assert!(
        reply["result"].get("isError").is_none(),
        "a degraded body is data, not an error: {reply}"
    );
}
