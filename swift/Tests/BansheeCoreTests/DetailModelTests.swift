// DetailModel — the detail window's view model, tested at the network boundary
// with MockAPIClient and the SHARED fixtures (so the data flowing through the
// tests is the data the wire actually carries).

import XCTest
@testable import BansheeCore

@MainActor
final class DetailModelTests: XCTestCase {
    private var mock: MockAPIClient!
    private var model: DetailModel!
    /// A fixed clock, so the `from`/`since` parameters are exact assertions
    /// rather than "roughly now" tolerances.
    private nonisolated static let frozenNow = Date(timeIntervalSince1970: 1_788_300_000)

    override func setUp() {
        super.setUp()
        mock = MockAPIClient()
        model = DetailModel(client: mock, now: { Self.frozenNow })
    }

    func testHistoryRefreshAsksForAWindowNotALimit() async throws {
        mock.rollupsResult = try Wire.decoder().decode(
            [Rollup].self, from: WireFormatTests.fixture("rollups.json")
        )
        await model.refreshHistory()

        XCTAssertEqual(model.rollups.count, 2)
        XCTAssertNil(model.lastError)
        // A window, never a limit: "the last N buckets" silently shrinks the
        // span whenever sampling had a gap.
        XCTAssertNil(mock.lastRollupsLimit)
        XCTAssertEqual(
            mock.lastRollupsFrom,
            Self.frozenNow.addingTimeInterval(-HistoryWindow.fiveHours.duration)
        )
    }

    func testChangingTheWindowChangesTheFromParameter() async {
        model.historyWindow = .week
        await model.refreshHistory()
        XCTAssertEqual(
            mock.lastRollupsFrom,
            Self.frozenNow.addingTimeInterval(-7 * 24 * 3600)
        )
    }

    func testHistoryKeepsTheLastGoodDataOnFailure() async throws {
        mock.rollupsResult = try Wire.decoder().decode(
            [Rollup].self, from: WireFormatTests.fixture("rollups.json")
        )
        await model.refreshHistory()
        XCTAssertEqual(model.rollups.count, 2)

        mock.failWith = .serverUnreachable("connection refused")
        await model.refreshHistory()
        // Stale beats blank; the error is surfaced separately.
        XCTAssertEqual(model.rollups.count, 2)
        XCTAssertNotNil(model.lastError)
    }

    func testNoCensusYetIsLoadedAndNil_ButAFailureIsNot() async throws {
        // The server's 404 → nil: a REAL state, loaded and empty.
        mock.censusResult = nil
        await model.refreshCensus()
        XCTAssertTrue(model.censusLoaded)
        XCTAssertNil(model.census)
        XCTAssertNil(model.lastError)

        // A transport failure must NOT count as "loaded, and there is nothing":
        // rendering that as a clean machine is the lie the flag exists to block.
        let failing = MockAPIClient()
        failing.failWith = .serverUnreachable("connection refused")
        let failedModel = DetailModel(client: failing, now: { Self.frozenNow })
        await failedModel.refreshCensus()
        XCTAssertFalse(failedModel.censusLoaded)
        XCTAssertNotNil(failedModel.lastError)
    }

    func testCensusRefreshDeliversTheFixture() async throws {
        mock.censusResult = try Wire.decoder().decode(
            Census.self, from: WireFormatTests.fixture("census-full.json")
        )
        await model.refreshCensus()
        XCTAssertEqual(model.census?.totalProcs, 812)
        XCTAssertEqual(model.census?.staleSessions.count, 1)
        XCTAssertTrue(model.censusLoaded)
    }

    func testAlertsAskForTheLongMemory() async throws {
        mock.alertsResult = [
            try Wire.decoder().decode(
                AlertEpisode.self, from: WireFormatTests.fixture("alert-episode.json")
            )
        ]
        await model.refreshAlerts()

        XCTAssertEqual(model.alerts.count, 1)
        // The server default is 24h; the alerts view is the LONG memory, so it
        // must ask for the year the retention keeps rather than accept the
        // default. Mutation-proof: drop the `since` parameter and this fails.
        XCTAssertEqual(
            mock.lastAlertsSince,
            Self.frozenNow.addingTimeInterval(-365 * 24 * 3600)
        )
        XCTAssertEqual(mock.lastAlertsLimit, 500)
    }

    func testStatsRefresh() async throws {
        mock.statsResult = try Wire.decoder().decode(
            Stats.self, from: WireFormatTests.fixture("stats.json")
        )
        await model.refreshStats()
        XCTAssertEqual(model.stats?.samples, 5729)
        XCTAssertNil(model.lastError)
    }
}
