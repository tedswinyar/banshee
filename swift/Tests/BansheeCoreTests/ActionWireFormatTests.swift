// Wire-format tests for the action reports. Decode the SHARED fixtures at
// the repo root — the exact bytes the Rust suite decodes (`actions.rs`) — so a
// cross-language divergence fails here before it fails in a sheet.

import XCTest
@testable import BansheeCore

final class ActionWireFormatTests: XCTestCase {
    // ---- reap-stale-sessions ------------------------------------------------

    func testDecodesTheSessionReapFixtureAtFullDepth() throws {
        let report = try Wire.decoder().decode(
            SessionReapReport.self,
            from: WireFormatTests.fixture("reap-sessions.json")
        )
        XCTAssertEqual(report.action, "reapStaleSessions")
        XCTAssertTrue(report.executed)
        XCTAssertTrue(report.tmuxAvailable)
        XCTAssertEqual(report.staleDays, 2)
        XCTAssertEqual(report.candidates.count, 4)

        // [0] is the one reap, executed → outcome "killed".
        let reaped = report.candidates[0]
        XCTAssertEqual(reaped.verdict, .reap)
        XCTAssertEqual(reaped.outcome, "killed")
        XCTAssertEqual(report.toReap.map(\.id), [reaped.id], "exactly one reap")

        // [1] is the capitalized-Python busy case, stale AND spared.
        let busy = report.candidates[1]
        XCTAssertEqual(busy.verdict, .spare)
        XCTAssertEqual(busy.paneCommand, "Python")
        XCTAssertGreaterThan(try XCTUnwrap(busy.idleDays), 2.0)
        XCTAssertNil(busy.outcome, "a spared session has no outcome")

        // [3] undated: null idleDays is "unknown", and null cwd present-as-null.
        let undated = report.candidates[3]
        XCTAssertNil(undated.idleDays)
        XCTAssertNil(undated.cwd)
        XCTAssertEqual(undated.verdict, .spare)
    }

    /// Re-encoding preserves present-as-null on the three nullable fields; the
    /// synthesized encoder would drop them. Mutation-proof: delete an
    /// `encodeNil` branch in `SessionCandidate.encode` and the key vanishes here.
    func testSessionCandidateEncodesNullsExplicitly() throws {
        let report = try Wire.decoder().decode(
            SessionReapReport.self,
            from: WireFormatTests.fixture("reap-sessions.json")
        )
        let undated = report.candidates[3]
        let obj = try XCTUnwrap(
            JSONSerialization.jsonObject(
                with: Wire.encoder().encode(undated),
                options: [.fragmentsAllowed]
            ) as? [String: Any]
        )
        // Present, and NSNull — not absent.
        for key in ["idleDays", "cwd", "outcome"] {
            XCTAssertTrue(obj.keys.contains(key), "\(key) must be present")
            XCTAssertTrue(obj[key] is NSNull, "\(key) must be explicit null")
        }
    }

    /// Guards the hard-coded field list: decode the fixture, re-encode, decode
    /// again, and every field must survive the round trip.
    func testEncodeCoversEverySessionCandidateField() throws {
        let report = try Wire.decoder().decode(
            SessionReapReport.self,
            from: WireFormatTests.fixture("reap-sessions.json")
        )
        for original in report.candidates {
            let round = try Wire.decoder().decode(
                SessionCandidate.self, from: Wire.encoder().encode(original)
            )
            XCTAssertEqual(round, original)
        }
    }

    // ---- reap-orphans -------------------------------------------------------

    func testDecodesTheOrphanReapFixtureAtFullDepth() throws {
        let report = try Wire.decoder().decode(
            OrphanReapReport.self,
            from: WireFormatTests.fixture("reap-orphans.json")
        )
        XCTAssertEqual(report.action, "reapOrphans")
        XCTAssertTrue(report.executed)
        XCTAssertEqual(report.termWaitSecs, 8)
        XCTAssertEqual(report.candidates.count, 2)
        XCTAssertEqual(report.candidates[0].outcome, "terminated")
        XCTAssertEqual(report.candidates[1].outcome, "killed")
        XCTAssertGreaterThan(report.candidates[0].rssBytes, 0)
    }

    /// A preview's null outcome round-trips as an explicit null.
    func testOrphanCandidateEncodesNullOutcomeExplicitly() throws {
        let candidate = OrphanCandidate(
            pid: 4321, program: "mcp-server", args: "/x/mcp-server",
            rssBytes: 1024, ageSecs: 60, outcome: nil
        )
        let obj = try XCTUnwrap(
            JSONSerialization.jsonObject(
                with: Wire.encoder().encode(candidate)
            ) as? [String: Any]
        )
        XCTAssertTrue(obj.keys.contains("outcome"))
        XCTAssertTrue(obj["outcome"] is NSNull)
    }

    func testEncodeCoversEveryOrphanCandidateField() throws {
        let report = try Wire.decoder().decode(
            OrphanReapReport.self,
            from: WireFormatTests.fixture("reap-orphans.json")
        )
        for original in report.candidates {
            let round = try Wire.decoder().decode(
                OrphanCandidate.self, from: Wire.encoder().encode(original)
            )
            XCTAssertEqual(round, original)
        }
    }
}
