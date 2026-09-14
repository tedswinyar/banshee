// Tests of how `APIClient` chooses its transport and its key (ADR-0008 Phase 2).
//
// `APIClient.resolve` is pure — environment and filesystem injected — so the one
// security-critical decision in the client can be exercised without setting process
// environment variables (racy across parallel tests) or creating real sockets.

import XCTest

@testable import BansheeCore

final class APIClientTransportTests: XCTestCase {
    private let defaultSocket = APIClient.defaultSocketPath
    private let nothingExists: (String) -> Bool = { _ in false }
    private let everythingExists: (String) -> Bool = { _ in true }

    /// **The file-loaded key never goes over TCP.** With no socket anywhere, the client
    /// falls back to the default URL but WITHHOLDS the file key — even though the key
    /// file exists and would have been readable. This is the ADR-0008 decision itself:
    /// any local user can bind the port, so "loopback" was never evidence about who is
    /// listening. Mutation-proof: return `.file(nil)` in the fallback branch and the
    /// key assertion fires.
    func testWithNoSocketTheFileKeyIsWithheldOverTCP() {
        let onlyTheKeyFileExists: (String) -> Bool = { $0.hasSuffix("/api_key") }
        let r = APIClient.resolve(env: [:], fileExists: onlyTheKeyFileExists)
        XCTAssertEqual(r.transport, .tcp(APIClient.defaultBaseURL))
        XCTAssertEqual(r.key, .withheld(noSocketAt: defaultSocket))
    }

    /// The default socket, when it exists, is the transport and carries the file key.
    func testTheDefaultSocketIsUsedWhenPresent() {
        let r = APIClient.resolve(env: [:], fileExists: { $0 == self.defaultSocket })
        XCTAssertEqual(r.transport, .unix(socketPath: defaultSocket))
        XCTAssertEqual(r.key, .file(nil))
    }

    /// `BANSHEE_KEY_FILE` travels with the file key, so a dev daemon's key can be used
    /// without editing the default location.
    func testAConfiguredKeyFileIsCarried() {
        let r = APIClient.resolve(
            env: ["BANSHEE_KEY_FILE": "/tmp/k"], fileExists: { $0 == self.defaultSocket })
        XCTAssertEqual(r.key, .file("/tmp/k"))
    }

    /// `BANSHEE_API_SOCKET` names the socket; the default is not consulted.
    func testAConfiguredSocketWins() {
        let r = APIClient.resolve(env: [APIClient.socketEnv: "/tmp/x.sock"], fileExists: everythingExists)
        XCTAssertEqual(r.transport, .unix(socketPath: "/tmp/x.sock"))
    }

    /// A configured socket that does not exist is not a daemon: fall back to TCP
    /// without the key rather than reporting "cannot connect" to a file nobody made.
    /// It does NOT silently try the default path instead — the user said where the
    /// daemon is, and a wrong answer from a different daemon would be worse.
    func testAConfiguredButAbsentSocketFallsBackToKeylessTCP() {
        let r = APIClient.resolve(
            env: [APIClient.socketEnv: "/tmp/missing.sock"],
            fileExists: { $0 == self.defaultSocket })
        XCTAssertEqual(r.transport, .tcp(APIClient.defaultBaseURL))
        XCTAssertEqual(r.key, .withheld(noSocketAt: "/tmp/missing.sock"),
                       "the message must name the path the user configured, not the default")
    }

    /// An explicit URL is a request for TCP and wins over an existing socket — a
    /// tunnel, a test server — and it gets no file key.
    func testAnExplicitURLWinsAndGetsNoFileKey() {
        let r = APIClient.resolve(env: ["BANSHEE_API_URL": "http://127.0.0.1:9000"], fileExists: everythingExists)
        XCTAssertEqual(r.transport, .tcp(URL(string: "http://127.0.0.1:9000")!))
        XCTAssertEqual(r.key, .withheld(noSocketAt: nil))
    }

    /// An explicit key is the caller's own credential and goes wherever they point
    /// the client — over TCP to an explicit URL, or over the socket.
    func testAnExplicitKeyGoesAnywhere() {
        let tcp = APIClient.resolve(
            env: ["BANSHEE_API_URL": "http://192.0.2.1:18769", "BANSHEE_API_KEY": " k "],
            fileExists: nothingExists)
        XCTAssertEqual(tcp.key, .explicit("k"))
        let unix = APIClient.resolve(env: ["BANSHEE_API_KEY": "k"], fileExists: { $0 == self.defaultSocket })
        XCTAssertEqual(unix.transport, .unix(socketPath: defaultSocket))
        XCTAssertEqual(unix.key, .explicit("k"))
    }

    /// An empty or unusable `BANSHEE_API_URL` is treated as UNSET (H4's fail-soft
    /// rule), so it falls through to the socket rather than forcing keyless TCP.
    func testAnUnusableURLIsTreatedAsUnset() {
        for raw in ["", "   ", "localhost:18769", "not a url"] {
            let r = APIClient.resolve(env: ["BANSHEE_API_URL": raw], fileExists: { $0 == self.defaultSocket })
            XCTAssertEqual(r.transport, .unix(socketPath: defaultSocket), "\(raw.debugDescription) should fall through")
        }
    }

    /// `client(for:)` builds what the resolution says: the socket client, or a TCP
    /// client that knows its key was withheld.
    func testTheClientMatchesTheResolution() {
        let unix = APIClient.client(for: .init(transport: .unix(socketPath: "/tmp/s.sock"), key: .file(nil)))
        XCTAssertEqual(unix.transport, .unix(socketPath: "/tmp/s.sock"))
        let tcp = APIClient.client(for: .init(transport: .tcp(APIClient.defaultBaseURL), key: .withheld(noSocketAt: "/x")))
        XCTAssertEqual(tcp.transport, .tcp(APIClient.defaultBaseURL))
    }

    /// A 401 from a keyless TCP fallback is a DIFFERENT failure from a rotated key: no
    /// key was sent, because the daemon offered no socket — it predates ADR-0008. The
    /// message must name the fix (update the daemon), and the request must carry no
    /// blank `X-Api-Key` header. Mutation-proof: drop the `fileKeyWithheld` branch and
    /// the message talks about rotation instead.
    func testA401OnKeylessTCPNamesTheDaemonUpgrade() async throws {
        StubURLProtocol.reset { _ in (401, Data(#"{"error":"missing or invalid X-Api-Key"}"#.utf8)) }
        let client = APIClient(
            baseURL: URL(string: "http://127.0.0.1:18769")!,
            session: StubURLProtocol.session(),
            fileKeyWithheldBecauseNoSocketAt: "/tmp/where-i-looked.sock"
        ) { "" }
        do {
            _ = try await client.pressure()
            XCTFail("expected .unauthorized")
        } catch let error as APIError {
            guard case .unauthorized(let detail) = error else { return XCTFail("\(error)") }
            XCTAssertTrue(detail.contains("make daemon-install"), detail)
            XCTAssertTrue(detail.contains("no daemon socket at /tmp/where-i-looked.sock"), detail)
            XCTAssertTrue(detail.contains("still watching"), detail)
            XCTAssertFalse(detail.lowercased().contains("rotated"), "this is not a rotation: \(detail)")
        }
        let exchange = try XCTUnwrap(StubURLProtocol.exchanges.first)
        XCTAssertFalse(exchange.keyHeaderPresent, "a withheld key must not become a blank header")
        XCTAssertEqual(StubURLProtocol.exchanges.count, 1, "nothing to re-read, so no retry")
    }
}
