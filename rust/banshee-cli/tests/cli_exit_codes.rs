// The CLI's exit codes are its scripting contract (main.rs header: 0 ok,
// 1 server error, 2 usage, 3 not found, 4 cannot reach API). e2e covered exit 1;
// 2/3/4 were undocumented-by-test (banshee-e8p). These spawn the REAL binary
// (CARGO_BIN_EXE_banshee) so the std::process::exit paths are actually reached —
// a return-value test could not see them.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::process::Command;
use std::thread;

const EXIT_SERVER_ERROR: i32 = 1;
const EXIT_USAGE: i32 = 2;
const EXIT_NOT_FOUND: i32 = 3;
const EXIT_NO_CONNECTION: i32 = 4;

fn banshee(args: &[&str]) -> i32 {
    Command::new(env!("CARGO_BIN_EXE_banshee"))
        .args(args)
        // Never read the real machine's key file mid-test.
        .env("BANSHEE_API_KEY", "test-key")
        .env("BANSHEE_KEY_FILE", "/nonexistent")
        .status()
        .expect("spawn banshee")
        .code()
        .expect("banshee exited via a signal, not a code")
}

/// A one-shot mock API serving one canned HTTP status to every connection.
fn mock_serving(status_line: &'static str, body: &'static str) -> String {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { break };
            let mut buf = [0u8; 1024];
            let _ = stream.read(&mut buf);
            let response = format!(
                "HTTP/1.1 {status_line}\r\nContent-Type: application/json\r\n\
                 Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.write_all(response.as_bytes());
        }
    });
    base
}

/// Exit 2: a usage error (unknown subcommand) — clap's own code, the scripting
/// contract's "you called me wrong".
#[test]
fn an_unknown_subcommand_exits_2() {
    assert_eq!(banshee(&["frobnicate"]), EXIT_USAGE);
}

/// Exit 4: the daemon is unreachable. Bind-then-drop yields a closed port.
#[test]
fn an_unreachable_api_exits_4() {
    let port = {
        let l = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        l.local_addr().unwrap().port()
    };
    let url = format!("http://127.0.0.1:{port}");
    assert_eq!(banshee(&["--api-url", &url, "status"]), EXIT_NO_CONNECTION);
}

/// Exit 3: a 404 (e.g. no census taken yet) is distinct from a generic error, so
/// `banshee census || bootstrap` can branch on "nothing yet" vs "it broke".
#[test]
fn a_404_exits_3_not_found() {
    let base = mock_serving("404 Not Found", r#"{"error":"no census yet"}"#);
    assert_eq!(banshee(&["--api-url", &base, "census"]), EXIT_NOT_FOUND);
}

/// Exit 1: any other 4xx/5xx is a server error, the catch-all non-zero.
#[test]
fn a_500_exits_1_server_error() {
    let base = mock_serving("500 Internal Server Error", r#"{"error":"boom"}"#);
    assert_eq!(banshee(&["--api-url", &base, "status"]), EXIT_SERVER_ERROR);
}
