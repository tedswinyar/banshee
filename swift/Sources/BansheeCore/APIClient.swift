// APIClient — the only network boundary in the app. Mock THIS protocol in
// tests; never mock URLSession or individual views' data.

import Foundation
import os

public enum APIError: Error, LocalizedError, Equatable {
    case serverUnreachable(String)
    case httpError(status: Int, message: String)
    case decodingFailed(String)
    /// The daemon rejected our key even after re-reading the key file.
    ///
    /// **Deliberately NOT folded into `serverUnreachable`.** They look alike — no
    /// usable answer — but they are opposite problems: an unreachable daemon is not
    /// watching the machine, whereas a daemon that rejects our key IS watching and
    /// sampling normally; only this client is locked out. Reporting the second as the
    /// first tells the user their monitoring has stopped when it has not, and points
    /// them at restarting a healthy daemon.
    case unauthorized(String)

    public var errorDescription: String? {
        switch self {
        case .serverUnreachable(let detail):
            return "Cannot reach the Banshee server: \(detail)"
        case .httpError(let status, let message):
            return "\(message) (\(status))"
        case .decodingFailed(let detail):
            return "Unexpected response from server: \(detail)"
        case .unauthorized(let detail):
            return detail
        }
    }
}

/// The app's window onto the API — at parity with the HTTP surface since the
/// detail window grew callers for every read. (Before that it was deliberately
/// kept at `pressure()`/`health()`: a protocol method with no caller is a mock
/// nobody exercises.)
///
/// Parameter semantics are the server's (docs/wire-format.md): `limit` over 500
/// is a 400, `from`/`to` are half-open `[from, to)`, combining `limit` with a
/// window is a 400. The client passes them through rather than pre-validating —
/// the server's error message is the authoritative one.
public protocol APIClientProtocol: Sendable {
    func pressure() async throws -> Pressure
    func headroom() async throws -> Headroom
    func health() async throws -> Bool
    func samples(limit: Int?, from: Date?, to: Date?) async throws -> [Sample]
    func rollups(limit: Int?, from: Date?, to: Date?) async throws -> [Rollup]
    /// The most recent census, or nil when none has been taken yet (the server's
    /// 404). Nil is a real state during the first five minutes after install.
    func census() async throws -> Census?
    func alerts(since: Date?, limit: Int?) async throws -> [AlertEpisode]
    /// What changed versus a lookback: `vs` is `1h`/`24h`/`90s`/`episode`,
    /// or nil for the daemon's default lookback. The reduction and the human
    /// sentence are composed in core; this client renders them.
    func deltas(vs: String?) async throws -> Deltas
    func stats() async throws -> Stats

    // Actions — the app's first WRITE surface. `preview` is a dry run and
    // touches nothing; `execute` acts. Neither takes parameters: targets are
    // recomputed server-side at execution time, never sent by the client (PIDs
    // recycle — `banshee_core::actions`). Present on the protocol so a view model
    // can be driven by MockAPIClient without a real daemon killing real
    // processes.
    func reapStaleSessionsPreview() async throws -> SessionReapReport
    func reapStaleSessionsExecute() async throws -> SessionReapReport
    func reapOrphansPreview() async throws -> OrphanReapReport
    func reapOrphansExecute() async throws -> OrphanReapReport
}

// Default-argument conveniences. Protocols cannot carry default arguments, so
// these live on the protocol extension — call sites read `client.rollups()`
// while mocks still implement the one full-parameter method.
extension APIClientProtocol {
    public func samples(
        limit: Int? = nil, from: Date? = nil, to: Date? = nil
    ) async throws -> [Sample] {
        try await samples(limit: limit, from: from, to: to)
    }

    public func rollups(
        limit: Int? = nil, from: Date? = nil, to: Date? = nil
    ) async throws -> [Rollup] {
        try await rollups(limit: limit, from: from, to: to)
    }

    public func alerts(
        since: Date? = nil, limit: Int? = nil
    ) async throws -> [AlertEpisode] {
        try await alerts(since: since, limit: limit)
    }

    public func deltas(vs: String? = nil) async throws -> Deltas {
        try await deltas(vs: vs)
    }
}

public struct APIClient: APIClientProtocol {
    /// How the request reaches the daemon (ADR-0008).
    ///
    /// The Unix socket is the default and the ONLY transport the file-loaded key
    /// travels over. TCP survives for an explicitly requested `BANSHEE_API_URL` and
    /// for a daemon that predates the socket, and carries only a key the caller
    /// supplied themselves.
    public enum Transport: Sendable, Equatable {
        case tcp(URL)
        case unix(socketPath: String)

        /// What to name in a message.
        public var description: String {
            switch self {
            case .tcp(let url): return url.absoluteString
            case .unix(let path): return "unix://\(path)"
            }
        }
    }

    private static let logger = Logger(subsystem: "com.tedswinyar.banshee", category: "APIClient")

    public let transport: Transport
    /// The TCP base URL — the explicit one, or the default. Kept even when the
    /// transport is the socket, because the open `/health` route is still served
    /// there and a diagnostic may want to name it.
    public let baseURL: URL
    /// Set when this client is on TCP WITHOUT the file key because no daemon socket
    /// was found at this path (ADR-0008's upgrade case). A 401 then means "the daemon
    /// is older than this app", which needs a different message from "the key was
    /// rotated" — and the message names the path that was looked for, which is the
    /// configured `BANSHEE_API_SOCKET` when there is one, not always the default.
    private let fileKeyWithheldBecauseNoSocketAt: String?
    /// **How to obtain the key, not the key itself** (`banshee-dqh`).
    ///
    /// The daemon ROTATES its key when it finds the key file was readable by other
    /// users, so a key captured once at construction can go dead while the process
    /// lives. This app is long-lived by definition — it sits in the menu bar for
    /// days — and when its key died every request 401'd forever, which the UI
    /// rendered as "Nothing is watching" even though the daemon was healthy and
    /// sampling the whole time. The CLI never hit this only because it is a fresh
    /// process per invocation.
    ///
    /// Holding a provider means the key can be re-read on demand, which is what makes
    /// recovery possible without relaunching the app.
    private let keyProvider: @Sendable () -> String
    private let session: URLSession

    /// A TCP client with a FIXED key — an explicit `BANSHEE_API_KEY`, or a test.
    /// Nothing to re-read, so the provider returns the same value forever.
    public init(baseURL: URL, apiKey: String, session: URLSession = .shared) {
        self.init(baseURL: baseURL, session: session, keyProvider: { apiKey })
    }

    /// A TCP client that resolves its key through `keyProvider` on every attempt, so
    /// a rotated key file is picked up without restarting.
    public init(
        baseURL: URL,
        session: URLSession = .shared,
        fileKeyWithheldBecauseNoSocketAt: String? = nil,
        keyProvider: @escaping @Sendable () -> String
    ) {
        self.transport = .tcp(baseURL)
        self.baseURL = baseURL
        self.session = session
        self.fileKeyWithheldBecauseNoSocketAt = fileKeyWithheldBecauseNoSocketAt
        self.keyProvider = keyProvider
    }

    /// A client over the daemon's Unix socket (ADR-0008) — the production shape.
    public init(socketPath: String, keyProvider: @escaping @Sendable () -> String) {
        self.transport = .unix(socketPath: socketPath)
        self.baseURL = Self.defaultBaseURL
        self.session = .shared
        self.fileKeyWithheldBecauseNoSocketAt = nil
        self.keyProvider = keyProvider
    }

    /// The port `banshee-api` listens on in the `prod` profile.
    ///
    /// **This is a COPY of `banshee_core::DEFAULT_API_PORT`**, because Swift cannot
    /// read a Rust constant. It is not free-floating: `testDefaultPortMatchesTheRustConstant`
    /// reads `rust/banshee-core/src/lib.rs` and fails if the two ever diverge —
    /// which is how a copy of a fact drifts (another project shipped a server on
    /// 8766 and a config crate on 18766) caught by a test instead of by a user.
    public static let defaultPort = 18769

    /// The default base URL when `BANSHEE_API_URL` is unset (or unusable).
    public static let defaultBaseURL = URL(string: "http://127.0.0.1:\(defaultPort)")!

    /// The env var naming the daemon's socket, and the socket's file name inside the
    /// config directory. COPIES of `banshee_core::API_SOCKET_ENV` / `API_SOCKET_FILE`,
    /// pinned against the Rust source by `testSocketConstantsMatchTheRustConstants`.
    public static let socketEnv = "BANSHEE_API_SOCKET"
    public static let socketFile = "api.sock"

    /// Where the prod daemon puts its socket: beside the key file.
    public static var defaultSocketPath: String {
        let home = FileManager.default.homeDirectoryForCurrentUser.path
        return "\(home)/Library/Application Support/banshee/\(socketFile)"
    }

    /// Where the key comes from, decided together with the transport.
    public enum KeySource: Sendable, Equatable {
        /// An explicit `BANSHEE_API_KEY`: the caller's own credential, spent wherever
        /// they point the client.
        case explicit(String)
        /// The key file (`BANSHEE_KEY_FILE` or the default), re-read on every attempt.
        case file(String?)
        /// No key at all: the file key is WITHHELD because the transport is TCP.
        /// Carries the socket path that was looked for (nil for an explicit URL), so
        /// the 401 message can name the place the user should look.
        case withheld(noSocketAt: String?)
    }

    public struct Resolution: Sendable, Equatable {
        public let transport: Transport
        public let key: KeySource
    }

    /// Resolve transport and key from the environment the way the CLI and the MCP
    /// server do (ADR-0008):
    ///
    ///   1. an explicit `BANSHEE_API_URL` wins — the caller asked for TCP (a tunnel,
    ///      a test server) — and carries only an explicit `BANSHEE_API_KEY`;
    ///   2. else the socket: `BANSHEE_API_SOCKET`, or the default path, if it EXISTS —
    ///      a path that is merely configured is not a daemon, and reporting "cannot
    ///      connect" for a socket nobody created would be worse than falling back;
    ///   3. else TCP to the default URL WITHOUT the file key. That daemon predates the
    ///      socket; a 401 from it names the fix.
    ///
    /// **The file-loaded key never goes over TCP** — not even to loopback. Any local
    /// user can bind 127.0.0.1:18769 before the daemon starts, and that key authorises
    /// the reap/kill routes, so "the host is loopback" was never evidence about who is
    /// listening. A socket file cannot be taken over that way: it lives in a directory
    /// only this user can write, and the kernel enforces its 0600 mode.
    ///
    /// Pure, with the environment and the filesystem injected, so the one
    /// security-critical decision in this file is the easiest thing in it to test.
    static func resolve(
        env: [String: String], fileExists: (String) -> Bool
    ) -> Resolution {
        let explicitKey = env["BANSHEE_API_KEY"]?.trimmingCharacters(in: .whitespacesAndNewlines)
        // Fail SOFT on a bad override: `BANSHEE_API_URL` is user-controlled, and
        // `URL(string:)` returns nil for "" and other junk. An empty/invalid value is
        // treated as "unset" rather than crashing at launch (H4).
        if let url = parseBaseURL(env["BANSHEE_API_URL"]) {
            return Resolution(
                transport: .tcp(url),
                key: explicitKey.map(KeySource.explicit) ?? .withheld(noSocketAt: nil))
        }
        let lookedFor: String = {
            if let configured = env[socketEnv], !configured.isEmpty { return configured }
            return defaultSocketPath
        }()
        let socketPath: String? = fileExists(lookedFor) ? lookedFor : nil
        if let socketPath {
            return Resolution(
                transport: .unix(socketPath: socketPath),
                key: explicitKey.map(KeySource.explicit) ?? .file(env["BANSHEE_KEY_FILE"]))
        }
        return Resolution(
            transport: .tcp(defaultBaseURL),
            key: explicitKey.map(KeySource.explicit) ?? .withheld(noSocketAt: lookedFor))
    }

    public static func fromEnvironment() -> APIClient {
        let resolution = resolve(
            env: ProcessInfo.processInfo.environment,
            fileExists: { FileManager.default.fileExists(atPath: $0) })
        return client(for: resolution)
    }

    /// Build the client a `Resolution` describes. The key file is re-read on every
    /// attempt rather than captured, so a rotation is picked up by a running app.
    static func client(for r: Resolution) -> APIClient {
        let provider: @Sendable () -> String
        var noSocketAt: String?
        switch r.key {
        case .explicit(let k): provider = { k }
        case .file(let path): provider = { readKeyFile(path: path) ?? "" }
        case .withheld(let looked):
            provider = { "" }
            noSocketAt = looked
        }
        switch r.transport {
        case .unix(let path):
            return APIClient(socketPath: path, keyProvider: provider)
        case .tcp(let url):
            if let noSocketAt {
                FileHandle.standardError.write(Data(
                    ("banshee: not sending the local API key over TCP to \(url.absoluteString) "
                        + "(ADR-0008); no daemon socket at \(noSocketAt)\n").utf8))
            }
            return APIClient(
                baseURL: url, session: .shared, fileKeyWithheldBecauseNoSocketAt: noSocketAt,
                keyProvider: provider)
        }
    }

    /// Resolve a raw `BANSHEE_API_URL` value to a usable base URL, falling
    /// back to the default for nil/empty/unparseable input. A URL missing a
    /// scheme (e.g. "localhost:18769") is not a usable HTTP base, so it too
    /// falls back rather than producing a scheme-less request that never
    /// connects.
    static func resolveBaseURL(_ raw: String?) -> URL {
        parseBaseURL(raw) ?? defaultBaseURL
    }

    /// The URL a raw `BANSHEE_API_URL` names, or nil when it names nothing usable —
    /// which `resolve` treats as UNSET (fall through to the socket), not as a request
    /// for TCP.
    static func parseBaseURL(_ raw: String?) -> URL? {
        guard let raw else { return nil }
        let trimmed = raw.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty,
              let url = URL(string: trimmed),
              url.scheme != nil,
              url.host != nil
        else { return nil }
        return url
    }

    public static func readKeyFile(path: String?) -> String? {
        let keyPath: String
        if let path {
            keyPath = path
        } else {
            let home = FileManager.default.homeDirectoryForCurrentUser.path
            keyPath = "\(home)/Library/Application Support/banshee/api_key"
        }
        guard let raw = try? String(contentsOfFile: keyPath, encoding: .utf8) else {
            return nil
        }
        let key = raw.trimmingCharacters(in: .whitespacesAndNewlines)
        return key.isEmpty ? nil : key
    }

    // MARK: - Endpoints

    /// The verdict. Never `nil`: before the sampler's first evaluation the API
    /// serves a `checking` verdict rather than an empty body, so this client has no
    /// "no data yet" case to invent an answer for (ADR-0005).
    public func pressure() async throws -> Pressure {
        try await request("GET", "/pressure")
    }

    /// The decision (ADR-0010). Never `nil` for the same reason as `pressure()`:
    /// before the first evaluation the API serves a `checking` decision that says
    /// "wait", so this client has no "no data yet" case to invent capacity for.
    public func headroom() async throws -> Headroom {
        try await request("GET", "/headroom")
    }

    public func health() async throws -> Bool {
        struct Health: Decodable { let status: String }
        let h: Health = try await request("GET", "/health")
        return h.status == "ok"
    }

    /// Raw samples, oldest first (last 24h by retention). Dates encode in the
    /// CANONICAL wire form — the server decodes generously, but a client has no
    /// business exercising that generosity.
    public func samples(limit: Int?, from: Date?, to: Date?) async throws -> [Sample] {
        try await request("GET", "/samples", query: Self.windowQuery(limit: limit, from: from, to: to))
    }

    /// 5-minute buckets, oldest first (30 days by retention). Identical
    /// parameter semantics to `/samples`, deliberately: one thing to learn.
    public func rollups(limit: Int?, from: Date?, to: Date?) async throws -> [Rollup] {
        try await request("GET", "/rollups", query: Self.windowQuery(limit: limit, from: from, to: to))
    }

    /// The most recent census, or nil when none has been taken yet. The server
    /// answers 404 for that state — a real one during the first five minutes
    /// after install — so it maps to nil here rather than to a thrown error.
    public func census() async throws -> Census? {
        do {
            return try await request("GET", "/census")
        } catch APIError.httpError(status: 404, message: _) {
            return nil
        }
    }

    /// Alert episodes, newest first; open ones always included. A capped read
    /// drops the OLDEST.
    public func alerts(since: Date?, limit: Int?) async throws -> [AlertEpisode] {
        var query: [URLQueryItem] = []
        if let since { query.append(URLQueryItem(name: "since", value: WireDate.encode(since))) }
        if let limit { query.append(URLQueryItem(name: "limit", value: String(limit))) }
        return try await request("GET", "/alerts", query: query)
    }

    /// What changed versus a lookback. `vs` echoes onto the wire as `?vs=…`; the
    /// server bounds it by retention and refuses a lookback past the ceiling with
    /// a 400 rather than silently clamping.
    public func deltas(vs: String?) async throws -> Deltas {
        var query: [URLQueryItem] = []
        if let vs { query.append(URLQueryItem(name: "vs", value: vs)) }
        return try await request("GET", "/deltas", query: query)
    }

    public func stats() async throws -> Stats {
        try await request("GET", "/stats")
    }

    // MARK: - Actions
    //
    // POST with no body. The server's `ApiJson` extractor accepts an absent body
    // and would 422 a caller-supplied target list — which is why we send none.

    public func reapStaleSessionsPreview() async throws -> SessionReapReport {
        try await send("POST", "/actions/reap-stale-sessions/preview", bodyData: nil)
    }

    public func reapStaleSessionsExecute() async throws -> SessionReapReport {
        try await send("POST", "/actions/reap-stale-sessions/execute", bodyData: nil)
    }

    public func reapOrphansPreview() async throws -> OrphanReapReport {
        try await send("POST", "/actions/reap-orphans/preview", bodyData: nil)
    }

    public func reapOrphansExecute() async throws -> OrphanReapReport {
        try await send("POST", "/actions/reap-orphans/execute", bodyData: nil)
    }

    /// The shared `/samples` + `/rollups` parameter set. Only the parameters the
    /// caller supplied are sent — the server owns the "limit and window are
    /// mutually exclusive" rule, and pre-empting it here would just hide its
    /// error message.
    static func windowQuery(limit: Int?, from: Date?, to: Date?) -> [URLQueryItem] {
        var query: [URLQueryItem] = []
        if let limit { query.append(URLQueryItem(name: "limit", value: String(limit))) }
        if let from { query.append(URLQueryItem(name: "from", value: WireDate.encode(from))) }
        if let to { query.append(URLQueryItem(name: "to", value: WireDate.encode(to))) }
        return query
    }

    // MARK: - Transport

    private func request<T: Decodable>(
        _ method: String, _ path: String, query: [URLQueryItem] = []
    ) async throws -> T {
        try await send(method, path, query: query, bodyData: nil)
    }

    private func send<T: Decodable>(
        _ method: String, _ path: String, query: [URLQueryItem] = [], bodyData: Data?
    ) async throws -> T {
        let key = keyProvider()
        let (data, status) = try await attempt(
            method, path, query: query, bodyData: bodyData, key: key)

        // A 401 may simply mean the daemon rotated its key under us. Re-read and try
        // once more (`banshee-dqh`) — but ONLY when the provider actually returns
        // something different, because retrying with the identical key would just be
        // a second identical rejection: two requests, one answer, and a doubled load
        // on every genuinely-wrong-key start.
        var finalData = data
        var finalStatus = status
        if status == 401 {
            let fresh = keyProvider()
            if fresh != key, !fresh.isEmpty {
                // Worth a line in the unified log: this is the daemon having rotated its
                // key under a running app, which is otherwise invisible when it works.
                Self.logger.notice("daemon rejected the key; the key file has changed — retrying with the re-read key over \(self.transport.description, privacy: .public)")
                (finalData, finalStatus) = try await attempt(
                    method, path, query: query, bodyData: bodyData, key: fresh)
            }
        }

        if finalStatus == 401 {
            if let lookedFor = fileKeyWithheldBecauseNoSocketAt {
                // Not a rejected key — no key was sent. The daemon answered on TCP but
                // offered no socket, so it predates ADR-0008; the fix is to update it.
                throw APIError.unauthorized(
                    "This app did not send its key: it found no daemon socket at "
                        + "\(lookedFor), and the key never travels over TCP "
                        + "(ADR-0008). The daemon is older than this app — run "
                        + "`make daemon-install` (or reinstall from the app) to update it. "
                        + "The daemon is still running and still watching the machine."
                )
            }
            throw APIError.unauthorized(
                "The daemon rejected this app's key. It usually means the key was "
                    + "rotated because the key file had become readable by other users. "
                    + "The daemon is still running and still watching the machine."
            )
        }
        guard (200..<300).contains(finalStatus) else {
            throw APIError.httpError(
                status: finalStatus, message: Self.errorMessage(from: finalData))
        }

        do {
            return try Wire.decoder().decode(T.self, from: finalData)
        } catch {
            throw APIError.decodingFailed(String(describing: error))
        }
    }

    /// One HTTP attempt with an explicit key. Returns the body and status so the
    /// caller can decide whether a 401 is worth retrying with a fresh credential.
    private func attempt(
        _ method: String, _ path: String, query: [URLQueryItem], bodyData: Data?, key: String
    ) async throws -> (Data, Int) {
        switch transport {
        case .unix(let socketPath):
            // The request target is the path plus the query, already encoded — built
            // through URLComponents so the encoding is the same one the TCP path gets.
            var components = URLComponents()
            components.path = path
            if !query.isEmpty { components.queryItems = query }
            let target = (components.percentEncodedPath) + (components.percentEncodedQuery.map { "?" + $0 } ?? "")
            return try await UnixSocketHTTP(path: socketPath)
                .request(method, target: target, key: key, body: bodyData)
        case .tcp(let base):
            var url = base.appending(path: path)
            if !query.isEmpty {
                url.append(queryItems: query)
            }
            var req = URLRequest(url: url)
            req.httpMethod = method
            // An empty key sends NO header: a blank `X-Api-Key` is an empty credential,
            // not an unauthenticated request (the same rule as the socket client).
            if !key.isEmpty {
                req.setValue(key, forHTTPHeaderField: "X-Api-Key")
            }
            if let bodyData {
                req.setValue("application/json", forHTTPHeaderField: "Content-Type")
                req.httpBody = bodyData
            }
            do {
                let (data, response) = try await session.data(for: req)
                return (data, (response as? HTTPURLResponse)?.statusCode ?? 0)
            } catch {
                throw APIError.serverUnreachable(error.localizedDescription)
            }
        }
    }

    /// Parse the wire error shape `{"error": "<message>"}` from raw bytes.
    public static func errorMessage(from data: Data) -> String {
        struct WireError: Decodable { let error: String }
        if let decoded = try? JSONDecoder().decode(WireError.self, from: data) {
            return decoded.error
        }
        return "unknown server error"
    }
}
