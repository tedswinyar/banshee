// Tests of the socket transport (ADR-0008 Phase 2): `UnixSocketHTTP` and `APIClient`
// riding on it.
//
// The I/O tests run against a REAL Unix-domain listener in-process (plain POSIX
// sockets on a background thread), not a mock of the connection — the same choice
// `banshee_core::uds_http`'s own round-trip test makes, and for the same reason: the
// framing this client writes has to be proven against a real reader, and the only
// mock allowed in this suite sits at the network boundary, which for a socket IS the
// listener. The parser tests start from RAW BYTES, including shapes a live server
// will not produce on demand.

import XCTest

@testable import BansheeCore

/// A one-shot-per-connection HTTP responder on a Unix socket. `handler` receives the
/// raw request text and returns the raw response bytes.
final class TestUnixServer: @unchecked Sendable {
    let path: String
    private let fd: Int32
    private let lock = NSLock()
    private var recorded: [String] = []
    private let handler: @Sendable (String) -> String

    var requests: [String] {
        lock.lock()
        defer { lock.unlock() }
        return recorded
    }

    init(handler: @escaping @Sendable (String) -> String) throws {
        self.handler = handler
        // Short on purpose: `sun_path` is 104 bytes and `NSTemporaryDirectory()` alone
        // is ~50 of them.
        path = "/tmp/bns-\(UUID().uuidString.prefix(8)).sock"
        fd = socket(AF_UNIX, SOCK_STREAM, 0)
        guard fd >= 0 else { throw NSError(domain: "socket", code: Int(errno)) }
        var addr = sockaddr_un()
        addr.sun_family = sa_family_t(AF_UNIX)
        let capacity = MemoryLayout.size(ofValue: addr.sun_path)
        path.withCString { src in
            withUnsafeMutablePointer(to: &addr.sun_path) {
                $0.withMemoryRebound(to: CChar.self, capacity: capacity) { dst in
                    _ = strlcpy(dst, src, capacity)
                }
            }
        }
        let bound = withUnsafePointer(to: &addr) {
            $0.withMemoryRebound(to: sockaddr.self, capacity: 1) {
                bind(fd, $0, socklen_t(MemoryLayout<sockaddr_un>.size))
            }
        }
        guard bound == 0, listen(fd, 8) == 0 else {
            let e = errno
            close(fd)
            throw NSError(domain: "bind/listen", code: Int(e))
        }
        let thread = Thread { [self] in self.serve() }
        thread.start()
    }

    private func serve() {
        while true {
            let client = accept(fd, nil, nil)
            if client < 0 { return }  // the listening fd was closed: done
            var raw = Data()
            var buf = [UInt8](repeating: 0, count: 4096)
            // Read the head, then exactly Content-Length more bytes (if any).
            while true {
                let n = read(client, &buf, buf.count)
                if n <= 0 { break }
                raw.append(contentsOf: buf[0..<n])
                if let split = raw.range(of: Data("\r\n\r\n".utf8)) {
                    let head = String(decoding: raw[raw.startIndex..<split.lowerBound], as: UTF8.self)
                    let declared = head.components(separatedBy: "\r\n").lazy
                        .compactMap { line -> Int? in
                            let parts = line.split(separator: ":", maxSplits: 1)
                            guard parts.count == 2,
                                  parts[0].trimmingCharacters(in: .whitespaces).lowercased() == "content-length"
                            else { return nil }
                            return Int(parts[1].trimmingCharacters(in: .whitespaces))
                        }.first ?? 0
                    if raw.count - split.upperBound >= declared { break }
                }
            }
            let request = String(decoding: raw, as: UTF8.self)
            lock.lock()
            recorded.append(request)
            lock.unlock()
            let response = Array(handler(request).utf8)
            var written = 0
            while written < response.count {
                let n = response[written...].withUnsafeBufferPointer { write(client, $0.baseAddress, $0.count) }
                if n <= 0 { break }
                written += n
            }
            close(client)
        }
    }

    func stop() {
        close(fd)
        unlink(path)
    }
}

final class UnixSocketHTTPTests: XCTestCase {
    private var servers: [TestUnixServer] = []

    override func tearDown() {
        servers.forEach { $0.stop() }
        servers = []
        super.tearDown()
    }

    private func server(_ handler: @escaping @Sendable (String) -> String) throws -> TestUnixServer {
        let s = try TestUnixServer(handler: handler)
        servers.append(s)
        return s
    }

    private static func ok(_ body: String) -> String {
        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: \(body.utf8.count)\r\n\r\n\(body)"
    }

    // MARK: - Framing, proven against a real reader

    /// The request this client writes must be valid HTTP/1.1 as a real server reads it:
    /// request line, mandatory Host, the key, and `Connection: close` — which is what
    /// makes read-to-EOF a correct fallback.
    func testARequestIsWrittenAsValidHTTPOverARealSocket() async throws {
        let s = try server { _ in Self.ok("okay") }
        let (body, status) = try await UnixSocketHTTP(path: s.path)
            .request("GET", target: "/health?x=1", key: "secret-key")
        XCTAssertEqual(status, 200)
        XCTAssertEqual(String(decoding: body, as: UTF8.self), "okay")

        let sent = try XCTUnwrap(s.requests.first)
        XCTAssertTrue(sent.hasPrefix("GET /health?x=1 HTTP/1.1\r\n"), sent)
        XCTAssertTrue(sent.contains("\r\nHost: localhost\r\n"), "HTTP/1.1 requires Host: \(sent)")
        XCTAssertTrue(sent.contains("\r\nX-Api-Key: secret-key\r\n"), "the key must be attached: \(sent)")
        XCTAssertTrue(sent.contains("\r\nConnection: close\r\n"), "close is what makes read-to-EOF correct: \(sent)")
    }

    /// An empty key must NOT produce an `X-Api-Key:` header at all. A blank header is
    /// an empty credential, which reads as a failed auth attempt rather than an
    /// unauthenticated probe of `/health`.
    func testAnEmptyKeySendsNoHeader() async throws {
        let s = try server { _ in Self.ok("{}") }
        _ = try await UnixSocketHTTP(path: s.path).request("GET", target: "/health", key: "")
        let sent = try XCTUnwrap(s.requests.first)
        XCTAssertFalse(sent.lowercased().contains("x-api-key"), "no key means no header: \(sent)")
    }

    /// A POST carries its body with a Content-Length and a JSON content type.
    func testAPostCarriesItsBodyFramed() async throws {
        let s = try server { _ in Self.ok("{}") }
        let body = Data(#"{"a":1}"#.utf8)
        _ = try await UnixSocketHTTP(path: s.path).request("POST", target: "/x", key: "k", body: body)
        let sent = try XCTUnwrap(s.requests.first)
        XCTAssertTrue(sent.hasPrefix("POST /x HTTP/1.1\r\n"), sent)
        XCTAssertTrue(sent.contains("\r\nContent-Length: 7\r\n"), sent)
        XCTAssertTrue(sent.contains("\r\nContent-Type: application/json\r\n"), sent)
        XCTAssertTrue(sent.hasSuffix("\r\n\r\n{\"a\":1}"), sent)
    }

    /// A 401 is a RESPONSE, not a transport error: the client must see the code so it
    /// can re-read a rotated key and retry.
    func testAnErrorStatusIsReturnedNotThrown() async throws {
        let s = try server { _ in "HTTP/1.1 401 Unauthorized\r\ncontent-length: 9\r\n\r\n{\"e\":\"x\"}" }
        let (body, status) = try await UnixSocketHTTP(path: s.path).request("GET", target: "/pressure", key: "k")
        XCTAssertEqual(status, 401)
        XCTAssertEqual(String(decoding: body, as: UTF8.self), #"{"e":"x"}"#)
    }

    /// A socket nobody is serving is `serverUnreachable` — the same error shape the
    /// TCP path produces for a daemon that is not running, so the UI's "nothing is
    /// watching" state is reached the same way on both transports — and it is reported
    /// FAST. Network.framework parks a missing Unix socket in `.waiting(ENOENT)` forever
    /// (verified by execution); a client that waits for `.failed` hangs for its whole
    /// timeout, and this client's default is 30 s. Mutation-proof: stop treating ENOENT
    /// as definitive and the elapsed-time bound fires.
    func testASocketNobodyServesIsServerUnreachableFast() async {
        let started = Date()
        do {
            _ = try await UnixSocketHTTP(path: "/tmp/bns-nobody-\(UUID().uuidString.prefix(6)).sock")
                .request("GET", target: "/health", key: "")
            XCTFail("expected a throw")
        } catch let error as APIError {
            guard case .serverUnreachable = error else { return XCTFail("expected .serverUnreachable, got \(error)") }
        } catch {
            XCTFail("expected APIError, got \(error)")
        }
        XCTAssertLessThan(Date().timeIntervalSince(started), 5, "a missing socket must fail fast, not wait out the timeout")
    }

    /// A socket path over the kernel limit must fail with a message that NAMES the
    /// limit; `connect` would report `EINVAL`, which reads as a bug in the caller.
    func testAnOverlongSocketPathNamesTheLimit() async {
        let long = "/tmp/" + String(repeating: "x", count: 200)
        XCTAssertFalse(UnixSocketHTTP.pathFits(long))
        do {
            _ = try await UnixSocketHTTP(path: long).request("GET", target: "/health", key: "")
            XCTFail("expected a throw")
        } catch let error as APIError {
            guard case .serverUnreachable(let detail) = error else { return XCTFail("\(error)") }
            XCTAssertTrue(detail.contains("sockaddr_un limit"), detail)
            XCTAssertTrue(detail.contains("\(UnixSocketHTTP.maxSocketPathLength)"), detail)
        } catch {
            XCTFail("expected APIError, got \(error)")
        }
    }

    // MARK: - The parser, from raw bytes

    func testANormalResponseSplitsIntoStatusAndBody() throws {
        let raw = Data("HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: 14\r\n\r\n{\"status\":\"x\"}".utf8)
        let (body, status) = try UnixSocketHTTP.parseResponse(raw)
        XCTAssertEqual(status, 200)
        XCTAssertEqual(String(decoding: body, as: UTF8.self), #"{"status":"x"}"#)
    }

    /// Without Content-Length, everything after the headers is the body — correct
    /// because the request asked the server to close the connection.
    func testAResponseWithoutContentLengthReadsToTheEnd() throws {
        let raw = Data("HTTP/1.1 200 OK\r\ncontent-type: application/json\r\n\r\n{\"a\":1}".utf8)
        let (body, _) = try UnixSocketHTTP.parseResponse(raw)
        XCTAssertEqual(String(decoding: body, as: UTF8.self), #"{"a":1}"#)
    }

    /// Content-Length wins over trailing bytes, so a server that ignored
    /// `Connection: close` and sent more cannot make the body over-read.
    func testContentLengthBoundsTheBody() throws {
        let raw = Data("HTTP/1.1 200 OK\r\ncontent-length: 7\r\n\r\n{\"a\":1}TRAILING".utf8)
        let (body, _) = try UnixSocketHTTP.parseResponse(raw)
        XCTAssertEqual(String(decoding: body, as: UTF8.self), #"{"a":1}"#)
    }

    /// **A truncated body must be an error, not a short read.** Returning the partial
    /// bytes would hand the decoder half a JSON document, and the failure would then
    /// be reported as malformed JSON — pointing at the wrong thing entirely.
    func testABodyShorterThanItsContentLengthIsAnError() {
        let raw = Data("HTTP/1.1 200 OK\r\ncontent-length: 99\r\n\r\n{\"a\":1}".utf8)
        XCTAssertThrowsError(try UnixSocketHTTP.parseResponse(raw)) { error in
            guard case .decodingFailed(let detail)? = error as? APIError else { return XCTFail("\(error)") }
            XCTAssertTrue(detail.contains("truncated"), detail)
        }
    }

    /// Header names are case-insensitive on the wire; axum sends lowercase, the RFC
    /// shows Title-Case. A case-sensitive match would silently fall back to
    /// read-to-EOF and lose the truncation check.
    func testContentLengthIsMatchedCaseInsensitively() {
        let raw = Data("HTTP/1.1 200 OK\r\nContent-Length: 99\r\n\r\nshort".utf8)
        XCTAssertThrowsError(try UnixSocketHTTP.parseResponse(raw)) { error in
            guard case .decodingFailed(let detail)? = error as? APIError else { return XCTFail("\(error)") }
            XCTAssertTrue(detail.contains("truncated"), detail)
        }
    }

    func testAResponseWithNoHeaderTerminatorIsAnError() {
        XCTAssertThrowsError(try UnixSocketHTTP.parseResponse(Data("HTTP/1.1 200 OK\r\ncontent-length: 2\r\n".utf8)))
    }

    func testAnUnparseableStatusLineIsAnError() {
        XCTAssertThrowsError(try UnixSocketHTTP.parseResponse(Data("NOT-HTTP\r\n\r\nbody".utf8)))
    }

    // MARK: - APIClient over the socket

    /// The whole client works over the socket, including the key-rotation recovery
    /// path (`banshee-dqh`): the daemon rejects the old key and rotates the file; the
    /// second attempt must carry the RE-READ key. Same fixture shape as the TCP test,
    /// because "retried" and "retried with fresh credentials" must stay distinguishable
    /// on this transport too.
    func testAPIClientOverTheSocketRereadsARotatedKey() async throws {
        let dir = FileManager.default.temporaryDirectory.appendingPathComponent("bns-key-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        let keyPath = dir.appendingPathComponent("api_key").path
        try "old-key".write(toFile: keyPath, atomically: true, encoding: .utf8)

        let s = try server { request in
            if request.contains("X-Api-Key: new-key") {
                return "HTTP/1.1 200 OK\r\ncontent-length: 15\r\n\r\n{\"status\":\"ok\"}"
            }
            try? "new-key".write(toFile: keyPath, atomically: true, encoding: .utf8)
            return "HTTP/1.1 401 Unauthorized\r\ncontent-length: 37\r\n\r\n{\"error\":\"missing or invalid X-Api-Key\"}"
        }
        let client = APIClient(socketPath: s.path) { APIClient.readKeyFile(path: keyPath) ?? "" }
        XCTAssertEqual(client.transport, .unix(socketPath: s.path))

        let healthy = try await client.health()
        XCTAssertTrue(healthy, "the retry should have succeeded")

        let keys = s.requests.map { req -> String in
            let line = req.components(separatedBy: "\r\n").first { $0.hasPrefix("X-Api-Key: ") }
            return line.map { String($0.dropFirst("X-Api-Key: ".count)) } ?? "<none>"
        }
        XCTAssertEqual(keys, ["old-key", "new-key"], "the second attempt must carry the RE-READ key")
    }

    /// A key the daemon keeps rejecting surfaces as `.unauthorized` (the UI's
    /// "locked out", not "nothing is watching"), and is not retried.
    func testAPIClientOverTheSocketReportsAPersistent401AsUnauthorized() async throws {
        let s = try server { _ in
            "HTTP/1.1 401 Unauthorized\r\ncontent-length: 16\r\n\r\n{\"error\":\"nope\"}"
        }
        let client = APIClient(socketPath: s.path) { "stale-key" }
        do {
            _ = try await client.health()
            XCTFail("expected .unauthorized")
        } catch let error as APIError {
            guard case .unauthorized(let detail) = error else { return XCTFail("\(error)") }
            XCTAssertTrue(detail.lowercased().contains("still watching"), detail)
        }
        XCTAssertEqual(s.requests.count, 1, "an unchanged key must be sent once, not twice")
    }

    /// The query string reaches the socket the same way it reaches TCP.
    func testQueryParametersAreEncodedIntoTheTarget() async throws {
        let s = try server { _ in Self.ok("[]") }
        let client = APIClient(socketPath: s.path) { "k" }
        let from = Date(timeIntervalSince1970: 1_700_000_000)
        _ = try await client.samples(limit: nil, from: from, to: nil)
        let sent = try XCTUnwrap(s.requests.first)
        XCTAssertTrue(sent.hasPrefix("GET /samples?from=2023-11-14T22:13:20.000000Z HTTP/1.1\r\n"), sent)
    }
}
