// UnixSocketHTTP — a minimal HTTP/1.1 client over a Unix-domain socket (ADR-0008,
// Phase 2). The Swift twin of `banshee_core::uds_http`; mirror its decisions, not
// just its shape.
//
// **Why this exists:** `URLSession` cannot connect to a Unix socket at all — there is
// no scheme, no proxy trick, no configuration key (verified, not assumed). The socket
// is the only transport the file-loaded API key travels over, so without this the
// menu-bar app cannot authenticate against a current daemon. Network.framework CAN
// (`NWEndpoint.unix(path:)`), and that is what this uses.
//
// **Why it is small:** the server is ours. Measured against the real daemon, axum
// answers every route with `content-length` and never `transfer-encoding: chunked`,
// so there is no chunked decoder here, and `Connection: close` is sent so that
// read-to-EOF is a correct fallback if a body ever arrives unsized. Those two together
// are why this is a page of code rather than a protocol implementation.
//
// Decisions carried over from the Rust client, each of which has a test:
//   - a body shorter than its own Content-Length is an ERROR, not a short read —
//     returning the partial bytes would hand the decoder half a JSON document and
//     the failure would then be reported as malformed JSON, pointing at the wrong thing;
//   - an empty key sends NO `X-Api-Key` header at all — a blank header is an empty
//     credential (a failed auth attempt), not an unauthenticated probe of `/health`;
//   - the 104-byte `sun_path` limit is checked before connecting, so the error names
//     the limit instead of surfacing as an inexplicable connection failure.

import Foundation
import Network

public struct UnixSocketHTTP: Sendable {
    /// **`sun_path` is 104 bytes on macOS**, including the NUL. A COPY of
    /// `banshee_core::MAX_SOCKET_PATH_LEN`, pinned against the Rust source by
    /// `testSocketConstantsMatchTheRustConstants`.
    public static let maxSocketPathLength = 104

    public let path: String
    public let timeout: Duration

    public init(path: String, timeout: Duration = .seconds(30)) {
        self.path = path
        self.timeout = timeout
    }

    /// Whether `path` fits in `sockaddr_un`. Checked before `connect` so the failure
    /// names the real problem; the kernel would report `EINVAL`, which reads as a bug.
    public static func pathFits(_ path: String) -> Bool {
        path.utf8.count < maxSocketPathLength
    }

    /// One request. `target` is the path plus any query string, already encoded.
    /// Returns the body bytes VERBATIM and the status code; a 4xx/5xx is a response,
    /// not an error, so the caller can see a 401 and re-read a rotated key.
    ///
    /// `key` is attached as `X-Api-Key` when non-empty. It is safe to attach here
    /// without a destination check: the destination is a socket file this user owns,
    /// which is the entire point of the transport.
    public func request(
        _ method: String, target: String, key: String, body: Data? = nil
    ) async throws -> (Data, Int) {
        guard Self.pathFits(path) else {
            throw APIError.serverUnreachable(
                "socket path is \(path.utf8.count) bytes, over the "
                    + "\(Self.maxSocketPathLength)-byte sockaddr_un limit: \(path)")
        }
        var head = "\(method) \(target) HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n"
        if !key.isEmpty {
            head += "X-Api-Key: \(key)\r\n"
        }
        var wire = Data()
        if let body {
            head += "Content-Type: application/json\r\nContent-Length: \(body.count)\r\n\r\n"
            wire.append(Data(head.utf8))
            wire.append(body)
        } else {
            head += "\r\n"
            wire.append(Data(head.utf8))
        }

        let raw = try await exchange(wire)
        return try Self.parseResponse(raw)
    }

    /// Connect, write `wire`, read to EOF. Bounded by `timeout` as a whole, because
    /// Network.framework has no single "the exchange took too long" knob and a hung
    /// daemon must not hang the menu bar.
    private func exchange(_ wire: Data) async throws -> Data {
        let path = self.path
        let timeout = self.timeout
        return try await withThrowingTaskGroup(of: Data.self) { group in
            group.addTask { try await SocketExchange(path: path).run(wire) }
            group.addTask {
                try await Task.sleep(for: timeout)
                throw APIError.serverUnreachable(
                    "no response from banshee-api on \(path) within \(timeout)")
            }
            // First to finish wins; the loser is cancelled (a cancelled exchange
            // cancels its connection).
            guard let first = try await group.next() else {
                throw APIError.serverUnreachable("socket exchange produced no result")
            }
            group.cancelAll()
            return first
        }
    }

    /// Split a raw HTTP/1.1 response into (body, status).
    ///
    /// Separate from the I/O so it can be tested from RAW BYTES, including the shapes
    /// a live server will not produce on demand — a truncated header block, a missing
    /// status line, a body shorter than its own `Content-Length`.
    public static func parseResponse(_ raw: Data) throws -> (Data, Int) {
        // Only CRLFCRLF: a bare-LF split is not HTTP, and accepting it would mean
        // guessing at where a malformed response ends.
        guard let split = raw.range(of: Data("\r\n\r\n".utf8)) else {
            throw APIError.decodingFailed("banshee-api response has no header terminator")
        }
        guard let head = String(data: raw[raw.startIndex..<split.lowerBound], encoding: .utf8) else {
            throw APIError.decodingFailed("banshee-api response headers are not UTF-8")
        }
        let bodyBytes = raw[split.upperBound...]

        let lines = head.components(separatedBy: "\r\n")
        guard let statusLine = lines.first else {
            throw APIError.decodingFailed("banshee-api response has no status line")
        }
        // "HTTP/1.1 200 OK" — the code is the second field.
        let fields = statusLine.split(separator: " ", omittingEmptySubsequences: true)
        guard fields.count >= 2, let status = Int(fields[1]) else {
            throw APIError.decodingFailed(
                "banshee-api sent an unparseable status line: \(statusLine.debugDescription)")
        }

        // Prefer Content-Length when present so a keep-alive server cannot make this
        // over-read; falling back to "everything after the headers" is correct
        // precisely because we asked the server to close. Header names are
        // case-insensitive on the wire: axum sends lowercase, the RFC shows Title-Case.
        let declared: Int? = lines.dropFirst().lazy.compactMap { line -> Int? in
            guard let colon = line.firstIndex(of: ":") else { return nil }
            let name = line[..<colon].trimmingCharacters(in: .whitespaces)
            guard name.caseInsensitiveCompare("content-length") == .orderedSame else { return nil }
            return Int(line[line.index(after: colon)...].trimmingCharacters(in: .whitespaces))
        }.first

        let body: Data
        switch declared {
        case .some(let n) where n <= bodyBytes.count:
            body = Data(bodyBytes.prefix(n))
        case .some(let n):
            // A body shorter than its own Content-Length is a TRUNCATED response.
            throw APIError.decodingFailed(
                "banshee-api response was truncated: \(bodyBytes.count) bytes of a declared \(n)")
        case .none:
            body = Data(bodyBytes)
        }
        return (body, status)
    }
}

/// One connection's lifetime: connect, send, read to EOF. A class because
/// `NWConnection` delivers its events through callbacks on a queue, and the
/// continuation that bridges them to async must be resumed exactly once.
private final class SocketExchange: @unchecked Sendable {
    private let connection: NWConnection
    private let queue = DispatchQueue(label: "com.tedswinyar.banshee.uds")
    private var received = Data()
    private let path: String

    init(path: String) {
        self.path = path
        // `.tcp` here selects a stream protocol stack; over a Unix endpoint there is
        // no TCP on the wire, but Network.framework needs a transport parameterisation
        // and this is the one that gives a byte stream.
        connection = NWConnection(to: .unix(path: path), using: .tcp)
    }

    func run(_ wire: Data) async throws -> Data {
        try await withTaskCancellationHandler {
            try await withCheckedThrowingContinuation { (cont: CheckedContinuation<Data, Error>) in
                // Resume-once guard: NWConnection can report `.failed` after `.ready`
                // (a mid-read reset), and a continuation resumed twice is a crash.
                nonisolated(unsafe) var done = false
                let finish: @Sendable (Result<Data, Error>) -> Void = { [self] result in
                    self.queue.async {
                        guard !done else { return }
                        done = true
                        self.connection.cancel()
                        cont.resume(with: result)
                    }
                }
                let path = self.path
                connection.stateUpdateHandler = { [self] state in
                    switch state {
                    case .ready:
                        self.connection.send(
                            content: wire, completion: .contentProcessed { error in
                                if let error {
                                    finish(.failure(APIError.serverUnreachable(
                                        "cannot send request to banshee-api: \(error)")))
                                    return
                                }
                                self.readToEnd(finish)
                            })
                    case .failed(let error):
                        // Network.framework reports the server's close as
                        // `.failed(ENETDOWN)` on a Unix socket — measured 40/40 against a
                        // live listener — and it RACES the final `receive` callback. If
                        // bytes have arrived, the close is the end of the response, not a
                        // failure; `parseResponse`'s Content-Length check is what catches
                        // the case where they were not all of it.
                        if !self.received.isEmpty {
                            finish(.success(self.received))
                        } else {
                            finish(.failure(APIError.serverUnreachable(
                                "cannot reach banshee-api on \(path): \(error)")))
                        }
                    case .waiting(let error):
                        // `.waiting` is Network.framework's "no viable path yet; I will
                        // retry when the network changes". For a socket FILE that never
                        // comes: a missing socket reports `.waiting(ENOENT)` within
                        // milliseconds and NEVER `.failed` (verified by execution, not
                        // assumed), so a client that only
                        // fails on `.failed` hangs until its own timeout. The
                        // definitive errno values are failures now; anything else is left
                        // to the exchange-wide timeout in case it really is transient.
                        if Self.isDefinitive(error) {
                            finish(.failure(APIError.serverUnreachable(
                                "cannot reach banshee-api on \(path): \(error)")))
                        }
                    case .cancelled:
                        finish(.failure(APIError.serverUnreachable(
                            "connection to banshee-api on \(path) was cancelled")))
                    default:
                        break
                    }
                }
                connection.start(queue: queue)
            }
        } onCancel: {
            connection.cancel()
        }
    }

    /// ENOENT (no socket file), ECONNREFUSED (file, but nobody listening — a stale
    /// socket), EACCES/EPERM (not ours). Each is a fact about the filesystem, not a
    /// path that may come back.
    private static func isDefinitive(_ error: NWError) -> Bool {
        guard case .posix(let code) = error else { return false }
        switch code {
        case .ENOENT, .ECONNREFUSED, .EACCES, .EPERM: return true
        default: return false
        }
    }

    private func readToEnd(_ finish: @escaping @Sendable (Result<Data, Error>) -> Void) {
        connection.receive(minimumIncompleteLength: 1, maximumLength: 1 << 16) {
            [self] content, _, isComplete, error in
            if let content { self.received.append(content) }
            if let error {
                finish(.failure(APIError.serverUnreachable(
                    "cannot read banshee-api response: \(error)")))
                return
            }
            if isComplete {
                finish(.success(self.received))
                return
            }
            self.readToEnd(finish)
        }
    }
}
