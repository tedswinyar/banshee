// Tests of APIClient's OWN credential handling — the key-rotation recovery path.
//
// These cannot go through `MockAPIClient`: that mock replaces `APIClient` wholesale
// at the `APIClientProtocol` boundary, so it can only prove things about callers,
// never about the client's retry logic. Stubbing `URLProtocol` instead keeps the mock
// exactly at the network boundary — the same boundary, one layer lower — which is
// where the testing standard says to put it. The seam already existed: `APIClient`
// takes an injected `URLSession`.

import XCTest

@testable import BansheeCore

/// Intercepts every request, records the key it carried, and answers from a
/// test-supplied handler.
final class StubURLProtocol: URLProtocol {
    struct Exchange {
        let key: String
        let path: String
        /// Whether an `X-Api-Key` header was present at all — an empty key must send
        /// NO header, and `key == ""` cannot tell "absent" from "blank".
        let keyHeaderPresent: Bool
    }

    /// Serialised because URLProtocol instances are created by the loading system on
    /// its own queues.
    private static let lock = NSLock()
    nonisolated(unsafe) private static var _exchanges: [Exchange] = []
    /// Returns (status, body) for a request bearing `key`.
    nonisolated(unsafe) private static var _handler: ((String) -> (Int, Data))?

    static func reset(handler: @escaping (String) -> (Int, Data)) {
        lock.lock()
        defer { lock.unlock() }
        _exchanges = []
        _handler = handler
    }

    static var exchanges: [Exchange] {
        lock.lock()
        defer { lock.unlock() }
        return _exchanges
    }

    static func session() -> URLSession {
        let config = URLSessionConfiguration.ephemeral
        config.protocolClasses = [StubURLProtocol.self]
        return URLSession(configuration: config)
    }

    override class func canInit(with request: URLRequest) -> Bool { true }
    override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }

    override func startLoading() {
        let header = request.value(forHTTPHeaderField: "X-Api-Key")
        let key = header ?? ""
        let path = request.url?.path ?? ""
        Self.lock.lock()
        Self._exchanges.append(Exchange(key: key, path: path, keyHeaderPresent: header != nil))
        let handler = Self._handler
        Self.lock.unlock()

        let (status, body) = handler?(key) ?? (500, Data())
        let response = HTTPURLResponse(
            url: request.url!, statusCode: status, httpVersion: "HTTP/1.1",
            headerFields: ["Content-Type": "application/json"])!
        client?.urlProtocol(self, didReceive: response, cacheStoragePolicy: .notAllowed)
        client?.urlProtocol(self, didLoad: body)
        client?.urlProtocolDidFinishLoading(self)
    }

    override func stopLoading() {}
}

final class APIClientAuthTests: XCTestCase {
    private func keyFile(_ contents: String) throws -> String {
        let dir = FileManager.default.temporaryDirectory
            .appendingPathComponent("banshee-key-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        let path = dir.appendingPathComponent("api_key").path
        try contents.write(toFile: path, atomically: true, encoding: .utf8)
        return path
    }

    /// **A rotated key is picked up without restarting the app** — the whole point.
    ///
    /// FIXTURE NOTE (the trap this was written against): asserting only "a 401 causes
    /// a second request" would pass whether or not the key was actually RE-READ, which
    /// is the property that matters. So the stub rewrites the key file when it rejects
    /// the old key — exactly what the daemon does when it rotates — and the test
    /// asserts the second request carried the NEW key. That is what distinguishes
    /// "retried" from "retried with fresh credentials".
    func testARotatedKeyIsRereadAndTheRequestRetried() async throws {
        let path = try keyFile("old-key")
        StubURLProtocol.reset { key in
            if key == "new-key" {
                return (200, Data(#"{"status":"ok"}"#.utf8))
            }
            // Reject, and rotate the file underneath — the daemon's own behaviour.
            try? "new-key".write(toFile: path, atomically: true, encoding: .utf8)
            return (401, Data(#"{"error":"missing or invalid X-Api-Key"}"#.utf8))
        }

        let client = APIClient(
            baseURL: URL(string: "http://127.0.0.1:18769")!,
            session: StubURLProtocol.session()
        ) { APIClient.readKeyFile(path: path) ?? "" }

        let ok = try await client.health()
        XCTAssertTrue(ok, "the retry should have succeeded")

        let keys = StubURLProtocol.exchanges.map(\.key)
        XCTAssertEqual(
            keys, ["old-key", "new-key"],
            "the second attempt must carry the RE-READ key, not the cached one")
    }

    /// A genuinely wrong key must NOT be retried.
    ///
    /// Pairs with the test above: "always retry once on 401" passes that one while
    /// doubling every request on a machine whose key is simply wrong. The retry is
    /// conditional on the provider returning something DIFFERENT, and this is the
    /// fixture where those two implementations come apart.
    func testAnUnchangedKeyIsNotRetried() async throws {
        let path = try keyFile("stale-key")
        StubURLProtocol.reset { _ in
            (401, Data(#"{"error":"missing or invalid X-Api-Key"}"#.utf8))
        }
        let client = APIClient(
            baseURL: URL(string: "http://127.0.0.1:18769")!,
            session: StubURLProtocol.session()
        ) { APIClient.readKeyFile(path: path) ?? "" }

        do {
            _ = try await client.health()
            XCTFail("expected .unauthorized")
        } catch let error as APIError {
            guard case .unauthorized = error else {
                return XCTFail("expected .unauthorized, got \(error)")
            }
        }
        XCTAssertEqual(
            StubURLProtocol.exchanges.count, 1,
            "an unchanged key must be sent once, not twice")
    }

    /// A 401 must surface as `.unauthorized`, never as `.serverUnreachable` or a bare
    /// `.httpError` — the UI branches on this to avoid claiming nothing is watching
    /// when the daemon is healthy and only this client is locked out.
    func testAPersistent401IsUnauthorizedNotAConnectionFailure() async throws {
        let path = try keyFile("stale-key")
        StubURLProtocol.reset { _ in (401, Data(#"{"error":"nope"}"#.utf8)) }
        let client = APIClient(
            baseURL: URL(string: "http://127.0.0.1:18769")!,
            session: StubURLProtocol.session()
        ) { APIClient.readKeyFile(path: path) ?? "" }

        do {
            _ = try await client.health()
            XCTFail("expected a throw")
        } catch let error as APIError {
            switch error {
            case .unauthorized(let detail):
                // The copy must not tell the user monitoring has stopped.
                XCTAssertTrue(
                    detail.lowercased().contains("still watching"),
                    "the message must say the daemon is still watching: \(detail)")
            default:
                XCTFail("a 401 must be .unauthorized, got \(error)")
            }
        }
    }

    /// Non-401 failures keep their existing shape: a 500 is still `.httpError`, so the
    /// rotation path cannot swallow unrelated server errors.
    func testA500IsStillAnHTTPErrorAndIsNotRetried() async throws {
        let path = try keyFile("some-key")
        StubURLProtocol.reset { _ in (500, Data(#"{"error":"boom"}"#.utf8)) }
        let client = APIClient(
            baseURL: URL(string: "http://127.0.0.1:18769")!,
            session: StubURLProtocol.session()
        ) { APIClient.readKeyFile(path: path) ?? "" }

        do {
            _ = try await client.health()
            XCTFail("expected a throw")
        } catch let error as APIError {
            guard case .httpError(let status, _) = error, status == 500 else {
                return XCTFail("expected .httpError(500), got \(error)")
            }
        }
        XCTAssertEqual(StubURLProtocol.exchanges.count, 1, "a 500 must not be retried")
    }
}
