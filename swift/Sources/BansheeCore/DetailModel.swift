// The detail window's view model — history, census, alerts, stats.
//
// Separate from PressureModel on purpose: the verdict polls forever because the
// menu bar is always visible, while these reads happen only while the window is
// open and looking at the matching tab. One model doing both would poll five
// routes to keep one glyph fresh.
//
// Same posture as PressureModel everywhere else: the last good data is KEPT on
// a transient failure (stale beats blank), errors are surfaced not swallowed,
// and nothing here re-derives anything core already decided.

import Foundation
import Observation

/// How much history the chart shows. Cases map to a `from` parameter — never a
/// `limit`, because "the last N buckets" quietly shrinks the window whenever
/// sampling had a gap, and a window that silently changes width is how charts
/// lie. The bucket math (5-minute buckets, ADR-0006) is the server's; this only
/// names a span.
public enum HistoryWindow: String, CaseIterable, Identifiable, Sendable {
    case fiveHours
    case day
    case week

    public var id: String { rawValue }

    public var label: String {
        switch self {
        case .fiveHours: return "5 hours"
        case .day: return "24 hours"
        case .week: return "7 days"
        }
    }

    public var duration: TimeInterval {
        switch self {
        case .fiveHours: return 5 * 3600
        case .day: return 24 * 3600
        case .week: return 7 * 24 * 3600
        }
    }
}

@MainActor
@Observable
public final class DetailModel {
    public private(set) var rollups: [Rollup] = []
    /// The latest census, or nil. Check `censusLoaded` before reading a meaning
    /// into nil: "the server said there is no census yet" and "we have not
    /// asked" are different sentences, and only one of them is about the server.
    public private(set) var census: Census?
    public private(set) var censusLoaded = false
    public private(set) var alerts: [AlertEpisode] = []
    public private(set) var stats: Stats?
    public var lastError: String?
    public var historyWindow: HistoryWindow = .fiveHours

    /// How far back the alerts list reaches. The server default is 24h; the
    /// alerts view exists to be the LONG memory, so it asks for the year the
    /// retention actually keeps (ADR-0006). The 500-row limit keeps the NEWEST.
    static let alertsLookback: TimeInterval = 365 * 24 * 3600
    static let alertsLimit = 500

    private let client: APIClientProtocol
    /// Injected clock, so tests can pin the `from`/`since` parameters exactly.
    private let now: @Sendable () -> Date

    /// The instance the app's window uses (same reasoning as
    /// `PressureModel.shared`: every tab must see the same data).
    public static let shared = DetailModel()

    public convenience init() {
        self.init(client: APIClient.fromEnvironment())
    }

    init(client: APIClientProtocol, now: @escaping @Sendable () -> Date = Date.init) {
        self.client = client
        self.now = now
    }

    public func refreshHistory() async {
        do {
            rollups = try await client.rollups(
                limit: nil,
                from: now().addingTimeInterval(-historyWindow.duration),
                to: nil
            )
            lastError = nil
        } catch {
            lastError = error.localizedDescription
        }
    }

    public func refreshCensus() async {
        do {
            census = try await client.census()
            censusLoaded = true
            lastError = nil
        } catch {
            // The load FAILED — which is not the same as "no census yet", so
            // `censusLoaded` is deliberately untouched: a nil census after an
            // error must not render as "nothing is running".
            lastError = error.localizedDescription
        }
    }

    public func refreshAlerts() async {
        do {
            alerts = try await client.alerts(
                since: now().addingTimeInterval(-Self.alertsLookback),
                limit: Self.alertsLimit
            )
            lastError = nil
        } catch {
            lastError = error.localizedDescription
        }
    }

    public func refreshStats() async {
        do {
            stats = try await client.stats()
            lastError = nil
        } catch {
            lastError = error.localizedDescription
        }
    }
}
