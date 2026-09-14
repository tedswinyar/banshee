// A minimal blocking HTTP/1.1 client over a Unix-domain socket (ADR-0008).
//
// **Why this exists rather than a dependency:** `reqwest` — which both the CLI and the
// MCP server already use — has no Unix-socket support at all, and the usual answer
// (`hyperlocal`) is async, which would mean giving two synchronous binaries a tokio
// runtime for the sake of one connect call. What is actually needed is small enough to
// own: connect, write a request, read a response.
//
// It is small *because the server is ours*. Measured against the real daemon before
// this was written: axum answers every route with `content-length` and never
// `transfer-encoding: chunked`, for small bodies and large. So there is no chunked
// decoder here, and `Connection: close` is sent so that read-to-EOF is a correct
// fallback if a body ever arrives unsized. Those two together are why this is ~100
// lines instead of a protocol implementation.
//
// It lives in `banshee-core` for the same reason `DEFAULT_API_PORT` does: two clients
// that hand-rolled their own would drift, and the drift would be in the transport that
// carries the API key.

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// A blocking HTTP client bound to one socket path.
#[derive(Debug, Clone)]
pub struct UdsHttp {
    path: PathBuf,
    timeout: Duration,
}

/// A response: the status code and the raw body bytes as a string.
///
/// The body is returned VERBATIM and never re-serialised, because the MCP server
/// relays the API's own bytes to its agent and the e2e parity table compares those
/// bytes unnormalised.
#[derive(Debug)]
pub struct UdsResponse {
    pub status: u16,
    pub body: String,
}

impl UdsHttp {
    pub fn new(path: impl Into<PathBuf>, timeout: Duration) -> Self {
        Self {
            path: path.into(),
            timeout,
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Send one request. `target` is the path plus any query string, already encoded.
    ///
    /// `key` is attached as `X-Api-Key` when non-empty. It is safe to attach here
    /// without a destination check: the destination is a socket file this user owns,
    /// which is the entire point of the transport — unlike a TCP port, it cannot be
    /// occupied by somebody else's process.
    pub fn request(
        &self,
        method: &str,
        target: &str,
        key: &str,
        body: Option<&str>,
    ) -> Result<UdsResponse, String> {
        if !crate::socket_path_fits(&self.path) {
            return Err(format!(
                "socket path is {} bytes, over the {}-byte sockaddr_un limit: {}",
                self.path.as_os_str().len(),
                crate::MAX_SOCKET_PATH_LEN,
                self.path.display()
            ));
        }
        let mut stream = UnixStream::connect(&self.path)
            .map_err(|e| format!("cannot reach banshee-api on {}: {e}", self.path.display()))?;
        stream.set_read_timeout(Some(self.timeout)).ok();
        stream.set_write_timeout(Some(self.timeout)).ok();

        // `Host` is mandatory in HTTP/1.1 and meaningless over a socket, so it is a
        // constant. `Connection: close` makes the server hang up after the response,
        // which is what lets read-to-EOF below be a correct fallback.
        let mut req =
            format!("{method} {target} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n");
        if !key.is_empty() {
            req.push_str(&format!("X-Api-Key: {key}\r\n"));
        }
        match body {
            Some(b) => {
                req.push_str("Content-Type: application/json\r\n");
                req.push_str(&format!("Content-Length: {}\r\n\r\n", b.len()));
                req.push_str(b);
            }
            None => req.push_str("\r\n"),
        }

        stream
            .write_all(req.as_bytes())
            .map_err(|e| format!("cannot send request to banshee-api: {e}"))?;
        stream.flush().ok();

        let mut raw = Vec::new();
        stream
            .read_to_end(&mut raw)
            .map_err(|e| format!("cannot read banshee-api response: {e}"))?;
        parse_response(&raw)
    }
}

/// Split a raw HTTP/1.1 response into status and body.
///
/// Separate from the I/O so it can be tested from RAW BYTES, including the shapes a
/// live server will not produce on demand — a truncated header block, a missing status
/// line, a body shorter than its own `Content-Length`.
pub fn parse_response(raw: &[u8]) -> Result<UdsResponse, String> {
    // Header/body boundary. Only CRLFCRLF: a bare-LF split is not HTTP, and accepting
    // it would mean guessing at where a malformed response ends.
    let split = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or_else(|| "banshee-api response has no header terminator".to_string())?;
    let head = std::str::from_utf8(&raw[..split])
        .map_err(|_| "banshee-api response headers are not UTF-8".to_string())?;
    let body_bytes = &raw[split + 4..];

    let mut lines = head.split("\r\n");
    let status_line = lines
        .next()
        .ok_or_else(|| "banshee-api response has no status line".to_string())?;
    // "HTTP/1.1 200 OK" — the code is the second field.
    let status: u16 = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|c| c.parse().ok())
        .ok_or_else(|| format!("banshee-api sent an unparseable status line: {status_line:?}"))?;

    // Prefer Content-Length when present so a keep-alive server (or a proxy that
    // ignored `Connection: close`) cannot make this hang or over-read. Falling back to
    // "everything after the headers" is correct precisely because we asked to close.
    let declared = head
        .split("\r\n")
        .skip(1)
        .filter_map(|l| l.split_once(':'))
        .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
        .and_then(|(_, v)| v.trim().parse::<usize>().ok());

    let body_slice = match declared {
        Some(n) if n <= body_bytes.len() => &body_bytes[..n],
        // A body shorter than its own Content-Length is a TRUNCATED response. Returning
        // what arrived would hand a client half a JSON document to parse, and the
        // resulting error would name JSON rather than the truncation.
        Some(n) => {
            return Err(format!(
                "banshee-api response was truncated: {} bytes of a declared {n}",
                body_bytes.len()
            ));
        }
        None => body_bytes,
    };
    Ok(UdsResponse {
        status,
        body: String::from_utf8_lossy(body_slice).to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_normal_response_splits_into_status_and_body() {
        let raw = b"HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: 13\r\n\r\n{\"status\":\"x\"}";
        let r = parse_response(raw).unwrap();
        assert_eq!(r.status, 200);
        assert_eq!(r.body, "{\"status\":\"x\"}".get(..13).unwrap());
    }

    /// A 401 is a RESPONSE, not a transport error: the client has to see the code so it
    /// can re-read a rotated key and retry.
    #[test]
    fn an_error_status_is_returned_not_turned_into_an_err() {
        let raw = b"HTTP/1.1 401 Unauthorized\r\ncontent-length: 9\r\n\r\n{\"e\":\"x\"}";
        let r = parse_response(raw).unwrap();
        assert_eq!(r.status, 401);
        assert_eq!(r.body, "{\"e\":\"x\"}");
    }

    /// Without Content-Length, everything after the headers is the body — correct
    /// because the request asked the server to close the connection.
    #[test]
    fn a_response_without_content_length_reads_to_the_end() {
        let raw = b"HTTP/1.1 200 OK\r\ncontent-type: application/json\r\n\r\n{\"a\":1}";
        assert_eq!(parse_response(raw).unwrap().body, "{\"a\":1}");
    }

    /// **A truncated body must be an error, not a short read.** Returning the partial
    /// bytes would hand the caller half a JSON document, and the failure would then be
    /// reported as malformed JSON — pointing at the wrong thing entirely.
    #[test]
    fn a_body_shorter_than_its_content_length_is_an_error() {
        let raw = b"HTTP/1.1 200 OK\r\ncontent-length: 99\r\n\r\n{\"a\":1}";
        let err = parse_response(raw).unwrap_err();
        assert!(err.contains("truncated"), "{err}");
    }

    #[test]
    fn a_response_with_no_header_terminator_is_an_error() {
        assert!(parse_response(b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\n").is_err());
    }

    #[test]
    fn an_unparseable_status_line_is_an_error() {
        assert!(parse_response(b"NOT-HTTP\r\n\r\nbody").is_err());
    }

    /// Header names are case-insensitive on the wire, and axum sends them lowercase
    /// while the RFC examples are title-case. A case-sensitive match would silently
    /// fall back to read-to-EOF and lose the truncation check.
    #[test]
    fn content_length_is_matched_case_insensitively() {
        let raw = b"HTTP/1.1 200 OK\r\nContent-Length: 99\r\n\r\nshort";
        assert!(parse_response(raw).unwrap_err().contains("truncated"));
    }

    /// Round-trip against a REAL socket, so the framing this client writes is proven
    /// against a real reader rather than only against its own parser.
    #[test]
    fn a_request_is_written_as_valid_http_over_a_real_socket() {
        use std::io::{BufRead, BufReader};
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("t.sock");
        let listener = std::os::unix::net::UnixListener::bind(&sock).unwrap();

        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut request_line = String::new();
            reader.read_line(&mut request_line).unwrap();
            let mut headers = Vec::new();
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                if line == "\r\n" || line.is_empty() {
                    break;
                }
                headers.push(line.trim().to_string());
            }
            stream
                .write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 4\r\n\r\nokay")
                .unwrap();
            (request_line, headers)
        });

        let client = UdsHttp::new(&sock, Duration::from_secs(5));
        let resp = client
            .request("GET", "/health?x=1", "secret-key", None)
            .unwrap();
        assert_eq!(resp.status, 200);
        assert_eq!(resp.body, "okay");

        let (request_line, headers) = handle.join().unwrap();
        assert_eq!(request_line.trim(), "GET /health?x=1 HTTP/1.1");
        assert!(
            headers.iter().any(|h| h == "Host: localhost"),
            "HTTP/1.1 requires Host: {headers:?}"
        );
        assert!(
            headers.iter().any(|h| h == "X-Api-Key: secret-key"),
            "the key must be attached: {headers:?}"
        );
        assert!(
            headers.iter().any(|h| h == "Connection: close"),
            "close is what makes read-to-EOF correct: {headers:?}"
        );
    }

    /// An empty key must NOT produce an `X-Api-Key:` header at all. A blank header is
    /// not the same as no header — it presents an empty credential, which reads as a
    /// failed auth attempt rather than an unauthenticated probe of `/health`.
    #[test]
    fn an_empty_key_sends_no_header() {
        use std::io::{BufRead, BufReader};
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("t.sock");
        let listener = std::os::unix::net::UnixListener::bind(&sock).unwrap();
        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut all = String::new();
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                if line == "\r\n" || line.is_empty() {
                    break;
                }
                all.push_str(&line);
            }
            stream
                .write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\n\r\nhi")
                .unwrap();
            all
        });
        let client = UdsHttp::new(&sock, Duration::from_secs(5));
        client.request("GET", "/health", "", None).unwrap();
        let sent = handle.join().unwrap();
        assert!(
            !sent.to_lowercase().contains("x-api-key"),
            "no key means no header: {sent:?}"
        );
    }

    /// A socket path over the kernel limit must fail with a message that NAMES the
    /// limit. `connect` reports `EINVAL`, which reads as a bug in the caller.
    #[test]
    fn an_overlong_socket_path_names_the_limit() {
        let long = std::path::PathBuf::from(format!("/tmp/{}", "x".repeat(200)));
        let err = UdsHttp::new(long, Duration::from_secs(1))
            .request("GET", "/health", "", None)
            .unwrap_err();
        assert!(err.contains("sockaddr_un limit"), "{err}");
    }
}
