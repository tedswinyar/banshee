// Wire-format tests for the record reads (samples, rollups, census, alerts,
// stats) — the P6b widening. Same discipline as WireFormatTests: decode the
// SHARED fixtures at the repo root, the exact bytes the Rust suite decodes
// (`sample.rs`, `census/mod.rs`), so a cross-language divergence fails here
// before it fails in a window.

import XCTest
@testable import BansheeCore

final class RecordWireFormatTests: XCTestCase {
    // ---- samples ----------------------------------------------------------

    func testDecodesTheSamplesFixtureAtFullDepth() throws {
        let samples = try Wire.decoder().decode(
            [Sample].self, from: WireFormatTests.fixture("samples.json")
        )
        XCTAssertEqual(samples.count, 2)

        let a = try XCTUnwrap(samples.first)
        XCTAssertEqual(a.ncpu, 8)
        XCTAssertEqual(a.pageSize, 16384)
        XCTAssertEqual(a.swapUsedBytes, 17_616_076_800)
        XCTAssertEqual(a.pagesFree, 27_965)
        XCTAssertEqual(a.swapins, 41_859_667)
        XCTAssertEqual(a.volumes.count, 2)
        XCTAssertEqual(a.volumes.first?.mountPoint, "/")
        XCTAssertEqual(a.volumes.first?.availBytes, 61_443_809_280)

        // The availability counters (banshee-cot): distinguishable per entry —
        // the first sample is the crisis shape (near-empty page cache).
        XCTAssertEqual(a.pagesPurgeable, 5_581)
        XCTAssertEqual(a.pagesExternal, 4_000)
        // …where the kernel's own verdict agrees: critical (banshee-0g4).
        XCTAssertEqual(a.memoryPressureLevel, 4)
        // Thermal (schema v11): the first sample is a throttled Intel shape — kernel
        // level 3 (trapping), PMU speed limit 62% — so both nullable fields are
        // non-null and distinguishable here…
        XCTAssertEqual(a.thermalPressureLevel, 3)
        XCTAssertEqual(a.cpuSpeedLimitPercent, 62)
        // CPU tick counters (schema v12): the first sample carries them, the
        // rate math derives utilization from the delta between the pair.
        XCTAssertEqual(a.cpuTicksTotal, 240_000_000)
        XCTAssertEqual(a.cpuTicksIdle, 96_000_000)
        // Kernel memory signals (schema v13, banshee-yk8): the crisis sample carries
        // both — low reclaimability and a lifetime jetsam count — non-null here.
        XCTAssertEqual(a.kernelFreePercent, 12)
        XCTAssertEqual(a.jetsamKills, 3)

        // The second sample is post-reboot: a DIFFERENT bootTimeSecs, the
        // discriminator that voids counter deltas across the pair.
        let b = try XCTUnwrap(samples.last)
        XCTAssertNotEqual(a.bootTimeSecs, b.bootTimeSecs)
        XCTAssertEqual(b.bootTimeSecs, 1_788_269_185)
        XCTAssertEqual(b.swapUsedBytes, 0)
        XCTAssertEqual(b.pagesExternal, 355_200)
        XCTAssertEqual(b.memoryPressureLevel, 1)
        // …and the second is a pre-v11 row: both null, decoded as nil, never 0.
        XCTAssertNil(b.thermalPressureLevel)
        XCTAssertNil(b.cpuSpeedLimitPercent)
        // The cpu-tick pair is present-as-null on the second sample too (both
        // move together): a gap in the pair yields no utilization point.
        XCTAssertNil(b.cpuTicksTotal)
        XCTAssertNil(b.cpuTicksIdle)
        // …and pre-v13 for the kernel memory signals: null, decoded as nil, never 0.
        XCTAssertNil(b.kernelFreePercent)
        XCTAssertNil(b.jetsamKills)
    }

    /// The v11 fields are present-as-null on the way OUT (a nil must encode as a
    /// null key, not vanish) and optional on the way IN (a pre-v11 daemon's JSON
    /// has no such keys). Mutation-proof: encodeIfPresent fails the first half;
    /// decode(UInt32?.self) with a required key fails the second.
    func testThermalFieldsArePresentAsNullAndOptionalOnDecode() throws {
        let samples = try Wire.decoder().decode(
            [Sample].self, from: WireFormatTests.fixture("samples.json"))
        let b = try XCTUnwrap(samples.last)
        let obj = try XCTUnwrap(
            JSONSerialization.jsonObject(with: Wire.encoder().encode(b)) as? [String: Any])
        XCTAssertTrue(obj["thermalPressureLevel"] is NSNull, "nil must encode as null, not be dropped")
        XCTAssertTrue(obj["cpuSpeedLimitPercent"] is NSNull)
        XCTAssertTrue(obj["cpuTicksTotal"] is NSNull, "nil cpu ticks must encode as null, not be dropped")
        XCTAssertTrue(obj["cpuTicksIdle"] is NSNull)
        XCTAssertTrue(obj["kernelFreePercent"] is NSNull, "nil kernel-free must encode as null, not be dropped")
        XCTAssertTrue(obj["jetsamKills"] is NSNull)

        var old = try XCTUnwrap(
            JSONSerialization.jsonObject(with: WireFormatTests.fixture("samples.json")) as? [[String: Any]])
        old[0].removeValue(forKey: "thermalPressureLevel")
        old[0].removeValue(forKey: "cpuSpeedLimitPercent")
        let decoded = try Wire.decoder().decode(
            [Sample].self, from: JSONSerialization.data(withJSONObject: old))
        XCTAssertNil(decoded[0].thermalPressureLevel, "absent keys decode as nil")
        XCTAssertEqual(decoded[0].memoryPressureLevel, 4, "the rest of the row is intact")
    }

    /// The fixture's second id is UPPERCASE on purpose: decoders accept any case
    /// (wire contract), and re-encoding emits lowercase. Foundation's `UUID`
    /// encodes `uuidString` UPPERCASE, which is why `Sample.encode` is
    /// hand-written — delete the `.lowercased()` and this fails.
    func testUUIDsDecodeAnyCaseAndEncodeLowercase() throws {
        let samples = try Wire.decoder().decode(
            [Sample].self, from: WireFormatTests.fixture("samples.json")
        )
        let b = try XCTUnwrap(samples.last)
        let obj = try XCTUnwrap(
            JSONSerialization.jsonObject(with: Wire.encoder().encode(b)) as? [String: Any]
        )
        XCTAssertEqual(obj["id"] as? String, "b7c8d9e0-f1a2-4b3c-9d4e-5f6a7b8c9d0e")
    }

    func testEncodeCoversEverySampleField() throws {
        let samples = try Wire.decoder().decode(
            [Sample].self, from: WireFormatTests.fixture("samples.json")
        )
        let obj = try XCTUnwrap(
            JSONSerialization.jsonObject(
                with: Wire.encoder().encode(try XCTUnwrap(samples.first))
            ) as? [String: Any]
        )
        XCTAssertEqual(
            Set(obj.keys),
            [
                "id", "sampledAt", "bootTimeSecs", "ncpu", "memTotalBytes",
                "pageSize", "load1m", "load5m", "load15m", "swapTotalBytes",
                "swapUsedBytes", "pagesFree", "pagesActive", "pagesInactive",
                "pagesWired", "pagesSpeculative", "pagesCompressor",
                "pagesPurgeable", "pagesExternal", "swapins", "swapouts",
                "memoryPressureLevel", "thermalPressureLevel", "cpuSpeedLimitPercent",
                "cpuTicksTotal", "cpuTicksIdle",
                "kernelFreePercent", "jetsamKills",
                "volumes",
            ],
            "encode(to:) key set drifted — a Sample field was added without a matching encode line"
        )
    }

    // ---- rollups ----------------------------------------------------------

    func testDecodesTheRollupsFixtureIncludingTheVoidedDeltas() throws {
        let rollups = try Wire.decoder().decode(
            [Rollup].self, from: WireFormatTests.fixture("rollups.json")
        )
        XCTAssertEqual(rollups.count, 2)

        // First bucket: a normal five minutes. Avg and max are DISTINCT values —
        // carrying both is the whole reason a rollup is not a sample.
        let a = try XCTUnwrap(rollups.first)
        XCTAssertEqual(a.sampleCount, 20)
        XCTAssertNotEqual(a.load1mAvg, a.load1mMax)
        XCTAssertEqual(a.load1mMax, 12.41, accuracy: 1e-12)
        XCTAssertEqual(a.swapinsDelta, 18_220)
        XCTAssertEqual(a.swapoutsDelta, 4_096)
        XCTAssertEqual(a.volumes.first?.availMin, 61_443_809_280)
        XCTAssertNotEqual(a.volumes.first?.availMin, a.volumes.first?.availAvg)

        // The census scalars (banshee-yj1) — the first bucket saw a census.
        XCTAssertEqual(a.staleSessionsMax, 17)
        XCTAssertEqual(a.orphansMax, 45)
        XCTAssertEqual(try XCTUnwrap(a.monitorPercentMax), 49.2, accuracy: 1e-12)
        XCTAssertEqual(a.totalProcsMax, 912)

        // Second bucket: a reboot voided the counters. Nil, NOT zero.
        let b = try XCTUnwrap(rollups.last)
        XCTAssertNil(b.swapinsDelta)
        XCTAssertNil(b.swapoutsDelta)
        // …and no census fell in it: nil again, not a claim of zero orphans.
        XCTAssertNil(b.staleSessionsMax)
        XCTAssertNil(b.orphansMax)
        XCTAssertNil(b.monitorPercentMax)
        XCTAssertNil(b.totalProcsMax)
    }

    /// Nil deltas encode as PRESENT nulls. This fails against the derived
    /// conformance (encodeIfPresent omits the keys) — the reason `Rollup.encode`
    /// is hand-written.
    func testVoidedDeltasEncodeAsPresentNulls() throws {
        let rollups = try Wire.decoder().decode(
            [Rollup].self, from: WireFormatTests.fixture("rollups.json")
        )
        let voided = try XCTUnwrap(rollups.last)
        XCTAssertNil(voided.swapinsDelta, "the fixture's second bucket must carry the void")
        let obj = try XCTUnwrap(
            JSONSerialization.jsonObject(with: Wire.encoder().encode(voided)) as? [String: Any]
        )
        for key in [
            "swapinsDelta", "swapoutsDelta",
            "staleSessionsMax", "orphansMax", "monitorPercentMax", "totalProcsMax",
        ] {
            let value = try XCTUnwrap(obj[key], "\(key) must be PRESENT as null, not absent")
            XCTAssertTrue(value is NSNull, "\(key) must be null, got \(value)")
        }
    }

    func testEncodeCoversEveryRollupField() throws {
        let rollups = try Wire.decoder().decode(
            [Rollup].self, from: WireFormatTests.fixture("rollups.json")
        )
        // The POPULATED bucket, so a dropped optional cannot hide behind a null.
        let full = try XCTUnwrap(rollups.first)
        XCTAssertNotNil(full.swapinsDelta, "the fixture must populate every nullable field")
        XCTAssertNotNil(full.staleSessionsMax, "the fixture must populate every nullable field")
        let obj = try XCTUnwrap(
            JSONSerialization.jsonObject(with: Wire.encoder().encode(full)) as? [String: Any]
        )
        XCTAssertEqual(
            Set(obj.keys),
            [
                "bucketStart", "sampleCount", "load1mAvg", "load1mMax",
                "swapUsedAvg", "swapUsedMax", "pagesFreeMin",
                "pagesCompressorMax", "swapinsDelta", "swapoutsDelta",
                "staleSessionsMax", "orphansMax", "monitorPercentMax",
                "totalProcsMax", "volumes",
            ],
            "encode(to:) key set drifted — a Rollup field was added without a matching encode line"
        )
    }

    // ---- census -----------------------------------------------------------

    func testDecodesTheCensusFixtureAtFullDepth() throws {
        let c = try Wire.decoder().decode(
            Census.self, from: WireFormatTests.fixture("census-full.json")
        )
        XCTAssertEqual(c.totalProcs, 812)
        XCTAssertTrue(c.tmuxAvailable)

        // Sessions: the stale one carries a cwd (lsof ran for it), the live one
        // an EXPLICIT null. Both states must decode distinguishably.
        XCTAssertEqual(c.agentSessions.count, 2)
        let stale = try XCTUnwrap(c.agentSessions.first)
        XCTAssertTrue(stale.isStale)
        XCTAssertEqual(stale.cwd, "/Users/example/Code/old-project")
        XCTAssertEqual(stale.ageDays, 5)
        let live = try XCTUnwrap(c.agentSessions.last)
        XCTAssertFalse(live.isStale)
        XCTAssertNil(live.cwd)
        XCTAssertEqual(c.staleSessions.map(\.pid), [41712])

        XCTAssertEqual(c.ideHelpers.count, 48)
        XCTAssertEqual(c.orphans.orphanCount, 23)
        XCTAssertEqual(c.orphans.orphansByProgram.first?.program, "mcp-server-fetch")

        // tmux: idleDays nil is "unknown", not "idle forever"; the busy pane
        // must never look reapable.
        let busy = try XCTUnwrap(c.tmuxSessions.first)
        XCTAssertTrue(busy.isBusy)
        XCTAssertNil(busy.idleDays)
        XCTAssertFalse(busy.isStale)
        // The fixture's third session is stale AND busy — the case that
        // distinguishes the reapable filter from a filter on staleness alone.
        XCTAssertEqual(c.reapableTmuxSessions.map(\.name), ["scratch"])

        XCTAssertEqual(c.appGroups.first?.name, "Chrome")
        XCTAssertEqual(c.appGroups.first?.procCount, 128)
    }

    /// The never-sum rule, made distinguishable by construction: the fixture's
    /// per-agent percentages sum to 46.0 while the deduplicated global-
    /// denominator total is 29.183. A consumer summing the list gets a number
    /// this test proves is NOT the honest one.
    func testMonitorTotalIsNotTheSumOfTheAgentList() throws {
        let c = try Wire.decoder().decode(
            Census.self, from: WireFormatTests.fixture("census-full.json")
        )
        let summed = c.monitorAgents.map(\.percentOfOneCore).reduce(0, +)
        XCTAssertEqual(summed, 46.0, accuracy: 1e-9)
        XCTAssertEqual(c.monitorTotal.percentOfOneCore, 29.183, accuracy: 1e-9)
        XCTAssertGreaterThan(summed, c.monitorTotal.percentOfOneCore)
        XCTAssertEqual(c.monitorTotal.procCount, 7)
        XCTAssertEqual(c.monitorAgents.last?.longestLifeSecs, 109)
    }

    func testEncodeCoversEveryCensusField() throws {
        let c = try Wire.decoder().decode(
            Census.self, from: WireFormatTests.fixture("census-full.json")
        )
        let obj = try XCTUnwrap(
            JSONSerialization.jsonObject(with: Wire.encoder().encode(c)) as? [String: Any]
        )
        XCTAssertEqual(
            Set(obj.keys),
            [
                "id", "takenAt", "totalProcs", "agentSessions", "ideHelpers",
                "orphans", "tmuxSessions", "appGroups", "monitorAgents",
                "monitorTotal", "tmuxAvailable",
            ],
            "encode(to:) key set drifted — a Census field was added without a matching encode line"
        )
        // Lowercase-out for the census id too.
        XCTAssertEqual(obj["id"] as? String, "0c9d8e7f-6a5b-4c3d-9e2f-1a0b9c8d7e6f")
    }

    func testEncodeCoversEveryAgentSessionField() throws {
        let c = try Wire.decoder().decode(
            Census.self, from: WireFormatTests.fixture("census-full.json")
        )
        // The STALE session, whose cwd is populated — a dropped field cannot
        // hide behind a null.
        let session = try XCTUnwrap(c.agentSessions.first)
        XCTAssertNotNil(session.cwd, "the fixture must populate every nullable field")
        let obj = try XCTUnwrap(
            JSONSerialization.jsonObject(with: Wire.encoder().encode(session)) as? [String: Any]
        )
        XCTAssertEqual(
            Set(obj.keys),
            ["pid", "program", "rssBytes", "ageSecs", "ageDays", "cpuSecs", "tty", "isStale", "cwd"],
            "encode(to:) key set drifted — an AgentSession field was added without a matching encode line"
        )

        // And the nil cwd on the live session is a PRESENT null.
        let live = try XCTUnwrap(c.agentSessions.last)
        let liveObj = try XCTUnwrap(
            JSONSerialization.jsonObject(with: Wire.encoder().encode(live)) as? [String: Any]
        )
        let cwd = try XCTUnwrap(liveObj["cwd"], "cwd must be PRESENT as null, not absent")
        XCTAssertTrue(cwd is NSNull)
    }

    func testEncodeCoversEveryTmuxSessionField() throws {
        let c = try Wire.decoder().decode(
            Census.self, from: WireFormatTests.fixture("census-full.json")
        )
        let stale = try XCTUnwrap(c.tmuxSessions.last)
        XCTAssertNotNil(stale.idleDays, "the fixture must populate every nullable field")
        let obj = try XCTUnwrap(
            JSONSerialization.jsonObject(with: Wire.encoder().encode(stale)) as? [String: Any]
        )
        XCTAssertEqual(
            Set(obj.keys),
            ["name", "paneCommand", "idleDays", "isBusy", "isStale"],
            "encode(to:) key set drifted — a TmuxSessionInfo field was added without a matching encode line"
        )

        let busy = try XCTUnwrap(c.tmuxSessions.first)
        let busyObj = try XCTUnwrap(
            JSONSerialization.jsonObject(with: Wire.encoder().encode(busy)) as? [String: Any]
        )
        let idle = try XCTUnwrap(busyObj["idleDays"], "idleDays must be PRESENT as null, not absent")
        XCTAssertTrue(idle is NSNull)
    }

    // ---- alerts + stats ---------------------------------------------------

    /// The episode fixture is the 2026-09-05 thrash afternoon as ONE record
    /// (ADR-0009): closed, peaked at Shrieking, 39 re-fires absorbed — the same
    /// bytes the Rust suite decodes. Every nullable carries a value.
    func testDecodesTheAlertEpisodeFixture() throws {
        let e = try Wire.decoder().decode(
            AlertEpisode.self, from: WireFormatTests.fixture("alert-episode.json")
        )
        XCTAssertEqual(e.dimension, "thrash")
        XCTAssertEqual(e.state, .closed)
        XCTAssertFalse(e.isOpen)
        XCTAssertEqual(e.peak.band, .red)
        XCTAssertEqual(e.peak.level, .shrieking)
        XCTAssertEqual(e.peak.severity, 3.494, accuracy: 1e-9)
        XCTAssertEqual(e.suppressed, 39)
        XCTAssertEqual(e.startedAt, WireDate.decode("2026-09-05T17:24:03.870112Z"))
        XCTAssertEqual(e.endedAt, WireDate.decode("2026-09-05T21:40:12.000000Z"))
        XCTAssertEqual(e.recoveringSince, e.endedAt, "closed at the moment it dropped below red")
        XCTAssertNotNil(e.lastNotifiedAt)
        XCTAssertEqual(e.id.uuidString.lowercased(), "3f9c1b2e-7a4d-4c8f-9e10-5b6a2d3c4e5f")
        XCTAssertTrue(e.peak.message.contains("4145 swap operations"))
        XCTAssertEqual(
            e.peak.whoLine, "who: Chrome ×102 at 8.9 GB, claude ×10 at 2.1 GB",
            "who was behind it at the peak (the who-line)")
        // The census summarised at the peak, so a delta can answer "what
        // changed since it started" after the raw censuses are swept.
        let peak = try XCTUnwrap(e.censusAtPeak, "censusAtPeak populated")
        XCTAssertEqual(peak.takenAt, WireDate.decode("2026-09-05T20:08:00.000000Z"))
        XCTAssertEqual(peak.totalProcs, 842)
        XCTAssertEqual(peak.orphanCount, 23)
        XCTAssertEqual(peak.monitorPercentOfOneCore, 29.183, accuracy: 1e-9)
        XCTAssertEqual(peak.consumers.count, 3)
        XCTAssertEqual(peak.consumers[0].name, "Chrome")
        XCTAssertEqual(peak.consumers[0].kind, .appGroup)
        XCTAssertEqual(peak.consumers[0].rssBytes, 8_900_000_000)
        XCTAssertEqual(peak.consumers[1].kind, .sessions)
        XCTAssertEqual(peak.consumers[2].kind, .rollup)
    }

    /// An episode stored before deltas carries no `censusAtPeak`; it decodes as nil rather than
    /// failing (Sparkle makes stored-record skew inevitable), and re-encodes with
    /// the key PRESENT as null. Mutation-proof: `decode` in place of
    /// `decodeIfPresent` throws here; drop the `encodeNil` branch and the key set
    /// loses `censusAtPeak`.
    func testAnEpisodeWithoutACensusAtPeakDecodesAndEncodesItAsNull() throws {
        let json = Data(
            #"""
            {"id":"3f9c1b2e-7a4d-4c8f-9e10-5b6a2d3c4e5f","dimension":"swap",
             "startedAt":"2026-09-05T17:24:03.870112Z","endedAt":null,
             "peak":{"band":"red","level":"wailing","severity":1.2,
                     "at":"2026-09-05T18:00:00.000000Z","message":"m","whoLine":null},
             "suppressed":0,"state":"firing","lastNotifiedAt":null,"recoveringSince":null}
            """#.utf8)
        let e = try Wire.decoder().decode(AlertEpisode.self, from: json)
        XCTAssertNil(e.censusAtPeak)
        let obj = try XCTUnwrap(
            JSONSerialization.jsonObject(with: Wire.encoder().encode(e)) as? [String: Any])
        XCTAssertTrue(obj["censusAtPeak"] is NSNull, "\(obj)")
    }

    /// A peak stored by a daemon from before the who-line carries no `whoLine`; it decodes as nil
    /// rather than failing, and re-encodes with the key PRESENT as null.
    /// Mutation-proof: `decode` in place of `decodeIfPresent` throws here; drop the
    /// `encodeNil` branch and the key set loses `whoLine`.
    func testAPeakWithoutAWhoLineDecodesAndEncodesItAsNull() throws {
        let json = Data(
            #"{"band":"red","level":"wailing","severity":2.0,"at":"2026-09-05T20:08:41.120000Z","message":"was red"}"#
                .utf8)
        let peak = try Wire.decoder().decode(EpisodePeak.self, from: json)
        XCTAssertNil(peak.whoLine)
        let obj = try XCTUnwrap(
            JSONSerialization.jsonObject(with: Wire.encoder().encode(peak)) as? [String: Any])
        XCTAssertEqual(Set(obj.keys), ["band", "level", "severity", "at", "message", "whoLine"])
        XCTAssertTrue(obj["whoLine"] is NSNull, "\(obj)")
    }

    func testEncodeCoversEveryAlertEpisodeField() throws {
        let e = try Wire.decoder().decode(
            AlertEpisode.self, from: WireFormatTests.fixture("alert-episode.json")
        )
        let obj = try XCTUnwrap(
            JSONSerialization.jsonObject(with: Wire.encoder().encode(e)) as? [String: Any]
        )
        XCTAssertEqual(
            Set(obj.keys),
            [
                "id", "dimension", "startedAt", "endedAt", "peak", "suppressed", "state",
                "lastNotifiedAt", "recoveringSince", "censusAtPeak",
            ],
            "encode(to:) key set drifted — an AlertEpisode field was added without a matching encode line"
        )
        XCTAssertEqual(obj["id"] as? String, "3f9c1b2e-7a4d-4c8f-9e10-5b6a2d3c4e5f")
        let peak = try XCTUnwrap(obj["peak"] as? [String: Any])
        XCTAssertEqual(Set(peak.keys), ["band", "level", "severity", "at", "message", "whoLine"])
        let census = try XCTUnwrap(obj["censusAtPeak"] as? [String: Any])
        XCTAssertEqual(
            Set(census.keys),
            ["takenAt", "totalProcs", "consumers", "orphanCount", "monitorPercentOfOneCore"])
        let consumer = try XCTUnwrap((census["consumers"] as? [[String: Any]])?.first)
        XCTAssertEqual(Set(consumer.keys), ["name", "kind", "count", "rssBytes"])
    }

    /// An OPEN episode's nullables are PRESENT as null, never absent — and null
    /// `endedAt` is the most important fact on the payload. Mutation-proof: drop
    /// the `encodeNil` branches and the keys vanish.
    func testAnOpenEpisodeEncodesItsNullsAsPresent() throws {
        let closed = try Wire.decoder().decode(
            AlertEpisode.self, from: WireFormatTests.fixture("alert-episode.json")
        )
        let open = AlertEpisode(
            id: closed.id, dimension: closed.dimension, startedAt: closed.startedAt,
            endedAt: nil, peak: closed.peak, suppressed: 0, state: .firing,
            lastNotifiedAt: nil, recoveringSince: nil
        )
        XCTAssertTrue(open.isOpen)
        let obj = try XCTUnwrap(
            JSONSerialization.jsonObject(with: Wire.encoder().encode(open)) as? [String: Any]
        )
        for key in ["endedAt", "lastNotifiedAt", "recoveringSince"] {
            let v = try XCTUnwrap(obj[key], "\(key) must be PRESENT as null, not absent")
            XCTAssertTrue(v is NSNull, key)
        }
        XCTAssertEqual(obj["state"] as? String, "firing")
    }

    func testDecodesTheStatsFixture() throws {
        let s = try Wire.decoder().decode(
            Stats.self, from: WireFormatTests.fixture("stats.json")
        )
        XCTAssertEqual(s.schemaVersion, 10)
        XCTAssertEqual(s.samples, 5729)
        XCTAssertEqual(s.rollups, 8433)
        XCTAssertEqual(s.censuses, 288)
        XCTAssertEqual(s.alerts, 12)
        XCTAssertEqual(s.dbSizeBytes, 3_560_448)
        // Sampler operational health (banshee-4r1): timestamps decode via
        // WireDate, the failure counts are 0 in the healthy fixture.
        XCTAssertNotNil(s.lastEvaluatedAt)
        XCTAssertNotNil(s.lastSweptAt)
        XCTAssertEqual(s.consecutiveEvalFailures, 0)
        XCTAssertEqual(s.consecutiveSweepFailures, 0)
        // What Banshee itself costs: the fixture carries all four so a
        // decoder that silently defaulted them would read 0 here and fail.
        XCTAssertEqual(s.selfFootprintBytes, 24_117_248)
        XCTAssertEqual(s.selfCpuSecs, 41.73, accuracy: 1e-9)
        XCTAssertEqual(s.selfUptimeSecs, 15122.4, accuracy: 1e-9)
        XCTAssertEqual(s.selfCpuPercent, 0.2759555615658, accuracy: 1e-9)
        XCTAssertTrue(s.reportsSelfCost)
    }

    /// A daemon from before the self-cost fields (0.1.x) still decodes — the store
    /// footer must not vanish over four missing keys — and says so via
    /// `reportsSelfCost`, so the UI shows "—" rather than "costs nothing".
    func testStatsWithoutSelfCostStillDecodes() throws {
        // Strip the four keys from the real fixture as JSON, not as text, so the
        // remaining bytes are exactly the old daemon's payload.
        var object = try XCTUnwrap(
            JSONSerialization.jsonObject(with: WireFormatTests.fixture("stats.json")) as? [String: Any])
        for key in ["selfFootprintBytes", "selfCpuSecs", "selfUptimeSecs", "selfCpuPercent"] {
            XCTAssertNotNil(object.removeValue(forKey: key), "fixture shape changed; re-pin \(key)")
        }
        let data = try JSONSerialization.data(withJSONObject: object)
        let s = try Wire.decoder().decode(Stats.self, from: data)
        XCTAssertEqual(s.samples, 5729, "the rest of the payload is intact")
        XCTAssertFalse(s.reportsSelfCost)
        XCTAssertEqual(s.selfFootprintBytes, 0)
    }

    // ---- client query composition ------------------------------------------

    /// Dates in query parameters go out in the CANONICAL wire form. The server
    /// decodes generously, but a client has no business exercising that.
    func testWindowQueryEncodesCanonicalDatetimes() throws {
        let from = try XCTUnwrap(WireDate.decode("2026-09-01T13:00:00.000000Z"))
        let items = APIClient.windowQuery(limit: nil, from: from, to: nil)
        XCTAssertEqual(items.map(\.name), ["from"])
        XCTAssertEqual(items.first?.value, "2026-09-01T13:00:00.000000Z")

        // Only what the caller supplied is sent — the server owns the
        // "limit and window are mutually exclusive" rule.
        XCTAssertTrue(APIClient.windowQuery(limit: nil, from: nil, to: nil).isEmpty)
        let limited = APIClient.windowQuery(limit: 288, from: nil, to: nil)
        XCTAssertEqual(limited.map(\.name), ["limit"])
        XCTAssertEqual(limited.first?.value, "288")
    }
}
