// MockAPIClient — the ONE approved mock, at the network boundary
// (Testing standard: mock at boundaries, never internals).

import Foundation
@testable import BansheeCore

final class MockAPIClient: APIClientProtocol, @unchecked Sendable {
    /// What `pressure()` returns. `nil` makes the call fail as "not found", which
    /// is NOT a state the real API can be in — it always serves a verdict, falling
    /// back to `checking` — so a test using it is testing this client's error path
    /// rather than a server behaviour.
    var verdict: Pressure?
    /// What the record reads return. `censusResult` defaults to nil because that
    /// IS a real server state (404 before the first census) — unlike `verdict`.
    var samplesResult: [Sample] = []
    var rollupsResult: [Rollup] = []
    var censusResult: Census?
    var alertsResult: [AlertEpisode] = []
    var statsResult: Stats?
    /// What `deltas(vs:)` returns. `nil` fails as "not found", which — like
    /// `verdict` — is not a state the real API can be in (it always composes an
    /// answer, even when there is no history to compare against).
    var deltasResult: Deltas?
    /// What `headroom()` returns. `nil` fails as "not found", which — like
    /// `verdict` — is not a state the real API can be in.
    var headroomResult: Headroom?
    var failWith: APIError?
    /// Call counters, so polling/refresh behaviour can be observed without a UI.
    var pressureCalls = 0
    var samplesCalls = 0
    var rollupsCalls = 0
    var censusCalls = 0
    var alertsCalls = 0
    var statsCalls = 0
    var headroomCalls = 0
    var deltasCalls = 0
    /// The `vs` of the LAST `deltas` call, so a test can assert what was asked for.
    var lastDeltasVs: String?
    /// The window parameters of the LAST `rollups`/`alerts` call, so a test can
    /// assert what was asked for, not just that something was.
    var lastRollupsFrom: Date?
    var lastRollupsLimit: Int?
    var lastAlertsSince: Date?
    var lastAlertsLimit: Int?

    private func checkFailure() throws {
        if let failWith { throw failWith }
    }

    func pressure() async throws -> Pressure {
        pressureCalls += 1
        try checkFailure()
        guard let verdict else {
            throw APIError.httpError(status: 404, message: "no verdict configured")
        }
        return verdict
    }

    func headroom() async throws -> Headroom {
        headroomCalls += 1
        try checkFailure()
        guard let headroomResult else {
            throw APIError.httpError(status: 404, message: "no headroom configured")
        }
        return headroomResult
    }

    func health() async throws -> Bool {
        try checkFailure()
        return true
    }

    func samples(limit: Int?, from: Date?, to: Date?) async throws -> [Sample] {
        samplesCalls += 1
        try checkFailure()
        return samplesResult
    }

    func rollups(limit: Int?, from: Date?, to: Date?) async throws -> [Rollup] {
        rollupsCalls += 1
        lastRollupsFrom = from
        lastRollupsLimit = limit
        try checkFailure()
        return rollupsResult
    }

    func census() async throws -> Census? {
        censusCalls += 1
        try checkFailure()
        return censusResult
    }

    func alerts(since: Date?, limit: Int?) async throws -> [AlertEpisode] {
        alertsCalls += 1
        lastAlertsSince = since
        lastAlertsLimit = limit
        try checkFailure()
        return alertsResult
    }

    func deltas(vs: String?) async throws -> Deltas {
        deltasCalls += 1
        lastDeltasVs = vs
        try checkFailure()
        guard let deltasResult else {
            throw APIError.httpError(status: 404, message: "no deltas configured")
        }
        return deltasResult
    }

    func stats() async throws -> Stats {
        statsCalls += 1
        try checkFailure()
        guard let statsResult else {
            throw APIError.httpError(status: 404, message: "no stats configured")
        }
        return statsResult
    }

    // Actions. Preview and execute return SEPARATE stubs, so a test can prove
    // the model swapped one for the other — the whole point of the preview→execute
    // flow. Call counters catch a double-execute or an execute that skipped
    // preview.
    var sessionPreviewResult: SessionReapReport?
    var sessionExecuteResult: SessionReapReport?
    var orphanPreviewResult: OrphanReapReport?
    var orphanExecuteResult: OrphanReapReport?
    var sessionPreviewCalls = 0
    var sessionExecuteCalls = 0
    var orphanPreviewCalls = 0
    var orphanExecuteCalls = 0

    func reapStaleSessionsPreview() async throws -> SessionReapReport {
        sessionPreviewCalls += 1
        try checkFailure()
        guard let sessionPreviewResult else {
            throw APIError.httpError(status: 500, message: "no session preview configured")
        }
        return sessionPreviewResult
    }

    func reapStaleSessionsExecute() async throws -> SessionReapReport {
        sessionExecuteCalls += 1
        try checkFailure()
        guard let sessionExecuteResult else {
            throw APIError.httpError(status: 500, message: "no session execute configured")
        }
        return sessionExecuteResult
    }

    func reapOrphansPreview() async throws -> OrphanReapReport {
        orphanPreviewCalls += 1
        try checkFailure()
        guard let orphanPreviewResult else {
            throw APIError.httpError(status: 500, message: "no orphan preview configured")
        }
        return orphanPreviewResult
    }

    func reapOrphansExecute() async throws -> OrphanReapReport {
        orphanExecuteCalls += 1
        try checkFailure()
        guard let orphanExecuteResult else {
            throw APIError.httpError(status: 500, message: "no orphan execute configured")
        }
        return orphanExecuteResult
    }
}

extension Finding {
    /// A finding for tests. `actionLabel` defaults to core's own wording for the
    /// action, so a stub cannot accidentally assert on a label the server would never
    /// send.
    static func stub(
        dimension: String = "memory",
        band: Band = .red,
        message: String = "Only 89 MB of RAM free.",
        action: Action = .relaunchApps,
        actionLabel: String? = nil,
        who: [Consumer] = [],
        whoLine: String? = nil
    ) -> Finding {
        Finding(
            dimension: dimension,
            band: band,
            message: message,
            action: action,
            actionLabel: actionLabel ?? Self.coreLabel(for: action),
            who: who,
            whoLine: whoLine
        )
    }

    /// Mirrors `Action::label` in banshee-core. Only for stubs — production code
    /// reads `actionLabel` off the wire (that is the whole point of shipping it).
    private static func coreLabel(for action: Action) -> String {
        switch action {
        case .reapStaleSessions: return "Reap stale agent sessions"
        case .reapOrphans: return "Reap orphaned helper processes"
        case .relaunchApps: return "Quit and relaunch the biggest apps"
        case .reboot: return "Reboot"
        case .openDiskTool: return "Open a disk-usage tool to see what is using the space"
        case .none: return "No action"
        case .unknown: return ""
        }
    }
}

extension Pressure {
    /// A verdict for tests, defaulting to a calm machine with nothing to say.
    static func stub(
        level: Level = .quiet,
        levelName: String? = nil,
        source: Source? = nil,
        glyph: String = "😴",
        accessibilityLabel: String? = nil,
        dimensions: [DimensionReading] = [],
        findings: [Finding] = [],
        sampleCount: Int = 40,
        censusCount: Int = 3,
        recovering: Bool = false,
        activity: RecentActivity = .none
    ) -> Pressure {
        Pressure(
            evaluatedAt: Date(timeIntervalSince1970: 1_788_100_000),
            level: level,
            levelName: levelName ?? level.rawValue.capitalized,
            source: source,
            glyph: glyph,
            accessibilityLabel: accessibilityLabel
                ?? "Banshee: \(level.rawValue.capitalized)",
            dimensions: dimensions,
            findings: findings,
            sampleCount: sampleCount,
            censusCount: censusCount,
            recovering: recovering,
            activity: activity
        )
    }
}

extension DimensionReading {
    static func stub(
        key: String = "cpu",
        label: String = "CPU utilization",
        band: Band = .green,
        value: Double = 0.53,
        unit: ReadingUnit = .ratio,
        severity: Double = -0.235,
        heldSecs: UInt64 = 600,
        observationGapSecs: UInt64? = nil,
        trendPerSec: Double? = nil,
        detail: String = "53% busy; 0.5× per core",
        advisory: Bool = false,
        pending: Band? = nil,
        recovering: Bool = false
    ) -> DimensionReading {
        DimensionReading(
            dimension: key, key: key, label: label, band: band, value: value,
            unit: unit, severity: severity, heldSecs: heldSecs,
            observationGapSecs: observationGapSecs, trendPerSec: trendPerSec,
            detail: detail, advisory: advisory, pending: pending, recovering: recovering
        )
    }
}
