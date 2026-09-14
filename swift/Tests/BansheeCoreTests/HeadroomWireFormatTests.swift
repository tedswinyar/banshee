// The headroom decision (ADR-0010) decodes from the SHARED fixtures — the same
// bytes the Rust suite DERIVES from the pressure fixtures and compares against.
// If Swift and Rust ever disagree about the headroom wire format, these fail
// first.

import XCTest
@testable import BansheeCore

final class HeadroomWireFormatTests: XCTestCase {
    private func load(_ name: String) throws -> Headroom {
        try Wire.decoder().decode(Headroom.self, from: WireFormatTests.fixture(name))
    }

    /// No data is "wait", never headroom — the distinction the decision exists to
    /// make. Mutation-proof: a decoder that defaulted a missing/false `shouldWait`
    /// to `false` would still pass a checking-is-quiet reading; this pins the
    /// bytes and the polarity together.
    func testDecodesTheCheckingFixtureAsWaitNotCapacity() throws {
        let h = try load("headroom-checking.json")
        XCTAssertEqual(h.level, .checking)
        XCTAssertTrue(h.shouldWait)
        XCTAssertEqual(h.recommendedParallelism, 0)
        XCTAssertEqual(h.retryAfterSecs, 30)
        XCTAssertNil(h.cores, "an explicit null must decode as nil, not throw")
        XCTAssertTrue(h.reason.contains("not headroom"), h.reason)
        XCTAssertEqual(h.conditions.count, 7)
        let ready = try XCTUnwrap(h.conditions.first)
        XCTAssertEqual(ready.kind, "Ready")
        XCTAssertEqual(ready.status, .false)
        XCTAssertNil(ready.since)
        XCTAssertTrue(h.conditions.dropFirst().allSatisfy { $0.status == .unknown && $0.since == nil })
        XCTAssertEqual(h.pressing.map(\.kind), ["Ready"], "Ready False is the one piece of bad news")
    }

    /// The calm fixture: room for 3 more on 8 cores, `retryAfterSecs` an EXPLICIT
    /// null, and `since` populated only where there was a reading.
    func testDecodesTheQuietFixtureIncludingItsExplicitNulls() throws {
        let h = try load("headroom-quiet.json")
        XCTAssertEqual(h.level, .quiet)
        XCTAssertFalse(h.shouldWait)
        XCTAssertEqual(h.recommendedParallelism, 3)
        XCTAssertNil(h.retryAfterSecs)
        XCTAssertEqual(h.cores, 8)
        XCTAssertEqual(h.reason, "Quiet: room for 3 more workers on 8 cores — 53% busy; 0.5× per core.")
        XCTAssertEqual(
            h.conditions.map(\.kind),
            ["Ready", "CpuPressure", "MemoryPressure", "ThermalPressure", "DiskPressure", "SprawlPressure", "CorporatePressure"]
        )
        let cpu = try XCTUnwrap(h.conditions.first { $0.kind == "CpuPressure" })
        XCTAssertEqual(cpu.status, .false)
        XCTAssertEqual(cpu.reason, "AllGreen")
        XCTAssertEqual(cpu.message, "53% busy; 0.5× per core")
        XCTAssertEqual(cpu.since, WireDate.decode("2026-08-31T14:54:05.123456Z"), "600s before evaluatedAt")
        let mem = try XCTUnwrap(h.conditions.first { $0.kind == "MemoryPressure" })
        XCTAssertEqual(mem.status, .unknown)
        XCTAssertNil(mem.since)
        XCTAssertTrue(h.pressing.isEmpty)
    }

    /// The fully-populated decision: the 2026-08-04 incident. Every nullable
    /// carries a value somewhere in the payload.
    func testDecodesTheShriekingFixtureAtFullDepth() throws {
        let h = try load("headroom-shrieking.json")
        XCTAssertEqual(h.level, .shrieking)
        XCTAssertTrue(h.shouldWait)
        XCTAssertEqual(h.recommendedParallelism, 0)
        XCTAssertEqual(h.retryAfterSecs, 300)
        XCTAssertEqual(h.cores, 8)
        XCTAssertTrue(h.reason.hasPrefix("Shrieking: Swap in use is red"), h.reason)
        let mem = try XCTUnwrap(h.conditions.first { $0.kind == "MemoryPressure" })
        XCTAssertEqual(mem.status, .true)
        XCTAssertEqual(mem.reason, "SwapRed")
        XCTAssertEqual(mem.message, "35.6 GB in use, climbing 74 MB/min")
        XCTAssertEqual(mem.since, WireDate.decode("2026-08-04T21:41:09.870112Z"))
        XCTAssertEqual(h.pressing.map(\.kind), ["CpuPressure", "MemoryPressure"])
    }

    /// The co-variance breaker: Restless — a level a client might read as "some
    /// room" — with `shouldWait: true`, because of a fresh thermal red. A client
    /// that branched on the level instead of the flag gets this one wrong.
    func testDecodesTheThrottledFixtureWhereTheLevelAndTheFlagDisagree() throws {
        let h = try load("headroom-throttled.json")
        XCTAssertEqual(h.level, .restless)
        XCTAssertTrue(h.shouldWait, "the flag, not the level, is the decision")
        XCTAssertEqual(h.recommendedParallelism, 0)
        XCTAssertEqual(h.retryAfterSecs, 60)
        let thermal = try XCTUnwrap(h.conditions.first { $0.kind == "ThermalPressure" })
        XCTAssertEqual(thermal.status, .true)
        XCTAssertEqual(thermal.reason, "ThermalRed")
        XCTAssertEqual(h.pressing.map(\.kind), ["ThermalPressure"])
    }

    /// The invariant every surface relies on, over all four fixtures: waiting IS
    /// zero parallelism, and a retry is present exactly then.
    func testShouldWaitIsExactlyZeroParallelismAcrossTheFixtures() throws {
        for name in ["headroom-checking.json", "headroom-quiet.json", "headroom-shrieking.json", "headroom-throttled.json"] {
            let h = try load(name)
            XCTAssertEqual(h.shouldWait, h.recommendedParallelism == 0, name)
            XCTAssertEqual(h.shouldWait, h.retryAfterSecs != nil, name)
        }
    }

    // Foot-gun guard (see WireFormatTests): each hand-written encode(to:) is a
    // HARD-CODED field list, pinned here so a field added to the struct but not to
    // the encoder goes red.
    func testEncodeCoversEveryHeadroomField() throws {
        let h = try load("headroom-shrieking.json")
        XCTAssertNotNil(h.retryAfterSecs, "the fixture must populate every nullable field")
        XCTAssertNotNil(h.cores)
        let data = try Wire.encoder().encode(h)
        let obj = try XCTUnwrap(JSONSerialization.jsonObject(with: data) as? [String: Any])
        XCTAssertEqual(
            Set(obj.keys),
            ["evaluatedAt", "level", "shouldWait", "recommendedParallelism", "retryAfterSecs", "cores", "reason", "conditions"],
            "encode(to:) key set drifted — a Headroom field was added without a matching encode line"
        )
        let conds = try XCTUnwrap(obj["conditions"] as? [[String: Any]])
        XCTAssertEqual(conds.count, 7)
        for c in conds {
            XCTAssertEqual(
                Set(c.keys), ["type", "status", "reason", "message", "since"],
                "Condition.encode(to:) key set drifted, or `kind` leaked instead of `type`"
            )
        }
    }

    /// Present-as-null binds the ENCODER: a nil `retryAfterSecs`, a nil `cores`
    /// and a nil `since` must go out as null keys, not vanish. Mutation-proof:
    /// replace any `encodeNil` branch with `encodeIfPresent` and the key is gone.
    func testEncodesNullableFieldsAsPresentNulls() throws {
        let h = try load("headroom-checking.json")
        let data = try Wire.encoder().encode(h)
        let obj = try XCTUnwrap(JSONSerialization.jsonObject(with: data) as? [String: Any])
        XCTAssertTrue(obj.keys.contains("retryAfterSecs"))
        XCTAssertTrue(obj["cores"] is NSNull, "cores must be a present null")

        let quiet = try load("headroom-quiet.json")
        let qdata = try Wire.encoder().encode(quiet)
        let qobj = try XCTUnwrap(JSONSerialization.jsonObject(with: qdata) as? [String: Any])
        XCTAssertTrue(qobj["retryAfterSecs"] is NSNull, "a not-waiting decision has a present-null retry")
        let conds = try XCTUnwrap(qobj["conditions"] as? [[String: Any]])
        let ready = try XCTUnwrap(conds.first)
        XCTAssertTrue(ready["since"] is NSNull, "Ready's since is a present null")
        XCTAssertEqual(ready["status"] as? String, "True", "Kubernetes spelling on the way out")
    }

    /// The round trip through our own codec: bytes → struct → bytes → struct is
    /// the identity, including the date in `since`.
    func testTheDecisionRoundTripsThroughOurCodec() throws {
        let h = try load("headroom-throttled.json")
        let again = try Wire.decoder().decode(Headroom.self, from: Wire.encoder().encode(h))
        XCTAssertEqual(again, h)
    }

    /// Version-skew resilience: a status this build does not know decodes to
    /// `.unknown` — the honest reading of "cannot tell" — rather than blanking the
    /// decision. Mutation-proof: drop the lenient init and this throws.
    func testUnknownConditionStatusDecodesToUnknownNotThrow() throws {
        let status = try Wire.decoder().decode(ConditionStatus.self, from: Data(#""Degraded""#.utf8))
        XCTAssertEqual(status, .unknown)
        // And `Level` stays STRICT here as everywhere (banshee-dnk): a level this
        // build cannot rank must fail loudly, not decode as something rankable.
        var json = try XCTUnwrap(String(data: WireFormatTests.fixture("headroom-quiet.json"), encoding: .utf8))
        XCTAssertTrue(json.contains(#""level": "quiet""#), "fixture shape changed; re-pin")
        json = json.replacingOccurrences(of: #""level": "quiet""#, with: #""level": "serene""#)
        XCTAssertThrowsError(try Wire.decoder().decode(Headroom.self, from: Data(json.utf8)))
    }

    /// The mock is the one approved mock, and it must speak the new read.
    func testTheMockServesAConfiguredDecision() async throws {
        let mock = MockAPIClient()
        mock.headroomResult = try load("headroom-quiet.json")
        let h = try await mock.headroom()
        XCTAssertEqual(h.recommendedParallelism, 3)
        XCTAssertEqual(mock.headroomCalls, 1)
    }
}
