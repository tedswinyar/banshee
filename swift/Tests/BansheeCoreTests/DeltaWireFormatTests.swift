// Cross-language pins for the delta wire types. These decode the SHARED
// fixtures (`tests/fixtures/deltas-*.json`) — the same bytes the Rust suite
// decodes and the e2e parity harness compares. If Swift and Rust ever disagree
// about the delta wire format, these fail first.

import XCTest

@testable import BansheeCore

final class DeltaWireFormatTests: XCTestCase {
    // ---- decode --------------------------------------------------------------

    /// The "why did freeing 4 GB not help" story, fully populated: a trustworthy
    /// verdict at both ends with a real source, Chrome freed 4 GB leading the
    /// consumers by ABSOLUTE change while claude grew, and the sentence says swap
    /// "still" grew — freeing did not help.
    func testDecodesTheNormalDeltaFixture() throws {
        let d = try Wire.decoder().decode(
            Deltas.self, from: WireFormatTests.fixture("deltas-normal.json")
        )
        XCTAssertEqual(d.lookback, "1h")
        XCTAssertNil(d.episodeId)
        XCTAssertFalse(d.spansGap)
        XCTAssertNil(d.observationGapSecs)

        let verdict = try XCTUnwrap(d.verdict, "a trustworthy anchor verdict")
        XCTAssertEqual(verdict.nowLevel, .restless)
        XCTAssertEqual(verdict.nowSource, .memory, "a populated source, not null")

        // Ranked by absolute change: Chrome freed the most, so it leads even
        // though claude is smaller. A rank by current size would put Chrome first
        // for the wrong reason — this is the co-variance breaker on the wire.
        XCTAssertEqual(d.consumers.first?.name, "Chrome")
        XCTAssertEqual(d.consumers.first?.kind, .appGroup)
        XCTAssertLessThan(try XCTUnwrap(d.consumers.first?.deltaBytes), 0, "Chrome was freed")
        let claude = try XCTUnwrap(d.consumers.first { $0.name == "claude" })
        XCTAssertGreaterThan(claude.deltaBytes, 0, "claude grew")
        XCTAssertEqual(claude.kind, .sessions)

        XCTAssertNotNil(d.census)
        XCTAssertTrue(d.summary.contains("still"), "freeing did not help: \(d.summary)")
    }

    /// The honesty case: a hole nobody watched across is reported at the top and
    /// on EVERY dimension, `consumers`/`census` are the empty branch, and the
    /// sentence warns the arithmetic spans a hole.
    func testDecodesTheGapDeltaFixture() throws {
        let d = try Wire.decoder().decode(
            Deltas.self, from: WireFormatTests.fixture("deltas-gap.json")
        )
        XCTAssertEqual(d.observationGapSecs, 1200)
        XCTAssertTrue(d.spansGap)
        XCTAssertFalse(d.dimensions.isEmpty)
        XCTAssertTrue(
            d.dimensions.allSatisfy { $0.observationGapSecs == 1200 },
            "every compared dimension spans the same hole")
        XCTAssertTrue(d.consumers.isEmpty, "no censuses in reach")
        XCTAssertNil(d.census)
        XCTAssertTrue(d.summary.contains("spans a hole"), "sentence: \(d.summary)")
    }

    /// The episode anchor: compared against the stored `censusAtPeak`, so
    /// `episodeId` is present (lowercased out) and the sentence is about the
    /// incident, not a clock interval.
    func testDecodesTheEpisodeDeltaFixture() throws {
        let d = try Wire.decoder().decode(
            Deltas.self, from: WireFormatTests.fixture("deltas-episode.json")
        )
        XCTAssertEqual(d.lookback, "episode")
        XCTAssertEqual(
            d.episodeId?.uuidString.lowercased(), "3f9c1b2e-7a4d-4c8f-9e10-5b6a2d3c4e5f")
        XCTAssertTrue(d.summary.hasPrefix("since the episode opened"))
    }

    // ---- encode: present-as-null and the full key set ------------------------

    /// The encoder emits EVERY field, nullables present as null, at every depth.
    /// Mutation-proof: drop a line from `Deltas.encode(to:)` and the key set here
    /// loses that key; the synthesized encoder would silently omit a nil.
    func testEncodeCoversEveryDeltasField() throws {
        let d = try Wire.decoder().decode(
            Deltas.self, from: WireFormatTests.fixture("deltas-normal.json")
        )
        let obj = try XCTUnwrap(
            JSONSerialization.jsonObject(with: Wire.encoder().encode(d)) as? [String: Any]
        )
        XCTAssertEqual(
            Set(obj.keys),
            [
                "evaluatedAt", "lookback", "episodeId", "requestedLookbackSecs", "anchorAt",
                "actualLookbackSecs", "verdict", "dimensions", "consumers", "census",
                "observationGapSecs", "summary",
            ],
            "encode(to:) key set drifted — a Deltas field was added without a matching encode line"
        )
        // episodeId is null on a clock-interval delta, and PRESENT as null.
        XCTAssertTrue(obj["episodeId"] is NSNull, "\(obj)")

        let verdict = try XCTUnwrap(obj["verdict"] as? [String: Any])
        XCTAssertEqual(
            Set(verdict.keys), ["thenLevel", "nowLevel", "thenSource", "nowSource", "detail"])

        let dimension = try XCTUnwrap((obj["dimensions"] as? [[String: Any]])?.first)
        XCTAssertEqual(
            Set(dimension.keys),
            [
                "dimension", "key", "label", "unit", "then", "now", "delta", "thenAt", "nowAt",
                "observationGapSecs", "detail",
            ])

        let consumer = try XCTUnwrap((obj["consumers"] as? [[String: Any]])?.first)
        XCTAssertEqual(
            Set(consumer.keys),
            [
                "name", "kind", "thenCount", "nowCount", "thenBytes", "nowBytes", "deltaBytes",
                "detail",
            ])

        let census = try XCTUnwrap(obj["census"] as? [String: Any])
        XCTAssertEqual(Set(census.keys), ["thenAt", "nowAt"])
    }

    /// The gap fixture pins the OTHER branch of every nullable: `census` present
    /// as null, `observationGapSecs` a real number, `verdict` still present.
    func testAGapDeltaEncodesItsNullCensusAsPresent() throws {
        let d = try Wire.decoder().decode(
            Deltas.self, from: WireFormatTests.fixture("deltas-gap.json")
        )
        let obj = try XCTUnwrap(
            JSONSerialization.jsonObject(with: Wire.encoder().encode(d)) as? [String: Any]
        )
        XCTAssertTrue(obj["census"] is NSNull, "census present as null when there were not two")
        XCTAssertEqual(obj["observationGapSecs"] as? Int, 1200)
    }

    /// A delta with an episode anchor emits `episodeId` lowercased (the UUID
    /// contract), not the mixed-case Swift default.
    func testEpisodeDeltaEncodesItsIdLowercased() throws {
        let d = try Wire.decoder().decode(
            Deltas.self, from: WireFormatTests.fixture("deltas-episode.json")
        )
        let obj = try XCTUnwrap(
            JSONSerialization.jsonObject(with: Wire.encoder().encode(d)) as? [String: Any]
        )
        XCTAssertEqual(obj["episodeId"] as? String, "3f9c1b2e-7a4d-4c8f-9e10-5b6a2d3c4e5f")
    }

    /// A kind a future daemon names decodes to `.unknown` rather than failing the
    /// whole delta — the same lenient-decode reasoning as `Source`/`Action`.
    func testAnUnknownConsumerKindDecodesToUnknown() throws {
        let json = Data(#"{"name":"x","kind":"gpuGroup","count":1,"rssBytes":1}"#.utf8)
        let c = try Wire.decoder().decode(ConsumerSize.self, from: json)
        XCTAssertEqual(c.kind, .unknown)
    }
}
