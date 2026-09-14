// Wire-format tests decode the SHARED fixtures (tests/fixtures/ at the repo
// root) — the same bytes the Rust tests and the e2e parity harness use.
// If Swift and Rust ever disagree about the wire format, these fail first.

import XCTest
@testable import BansheeCore

final class WireFormatTests: XCTestCase {
    /// Locate the repo-root fixtures dir relative to this source file.
    static func fixture(_ name: String) throws -> Data {
        let url = URL(fileURLWithPath: #filePath)
            .deletingLastPathComponent() // → BansheeCoreTests/
            .deletingLastPathComponent() // → Tests/
            .deletingLastPathComponent() // → swift/
            .deletingLastPathComponent() // → repo root
            .appending(path: "tests/fixtures/\(name)")
        return try Data(contentsOf: url)
    }

    // ---- the shared pressure fixtures ------------------------------------
    //
    // The SAME bytes the Rust suite decodes (`pressure/tests.rs`) and the e2e
    // harness compares. If Swift and Rust ever disagree about the wire format,
    // these fail first — which is the whole reason the fixtures live at the repo
    // root rather than inside either language's test tree.

    func testDecodesTheCheckingFixture() throws {
        let p = try Wire.decoder().decode(
            Pressure.self, from: Self.fixture("pressure-checking.json")
        )
        XCTAssertEqual(p.level, .checking)
        XCTAssertEqual(p.levelName, "Checking")
        XCTAssertEqual(p.glyph, "🫧")
        XCTAssertNil(p.source)
        XCTAssertTrue(p.dimensions.isEmpty)
        XCTAssertTrue(p.findings.isEmpty)
        XCTAssertEqual(p.sampleCount, 0)
        // The distinction the level scale exists to make.
        XCTAssertNotEqual(p.level, .quiet)
        XCTAssertNotEqual(p.glyph, "😴")
    }

    func testDecodesTheQuietFixtureIncludingItsExplicitNulls() throws {
        let p = try Wire.decoder().decode(
            Pressure.self, from: Self.fixture("pressure-quiet.json")
        )
        XCTAssertEqual(p.level, .quiet)
        XCTAssertNil(p.source, "an explicit null must decode as nil, not throw")
        // The RECOVERING decoration (ADR-0009), rendered verbatim like every glyph:
        // the disk dimension is green again inside its episode's down window.
        XCTAssertEqual(p.glyph, "😴🩹")
        XCTAssertEqual(p.accessibilityLabel, "Banshee: Quiet, recovering")
        XCTAssertTrue(p.recovering)
        XCTAssertEqual(p.activity, RecentActivity(open: 1, lastHour: 1, lastDay: 2))
        let disk = try XCTUnwrap(p.dimensions.first { $0.key == "disk" })
        XCTAssertTrue(disk.recovering)
        XCTAssertEqual(disk.band, .green, "recovering is a modifier, not a band")
        XCTAssertEqual(
            p.concerning.map(\.key), ["disk"],
            "a recovering green is the one green worth showing"
        )
        let cpu = try XCTUnwrap(p.dimensions.first { $0.key == "cpu" })
        XCTAssertFalse(cpu.recovering)
        XCTAssertEqual(cpu.band, .green)
        XCTAssertEqual(cpu.unit, .ratio)
        XCTAssertNil(cpu.trendPerSec)
        XCTAssertNil(cpu.pending)
        XCTAssertNil(cpu.observationGapSecs, "no gap on a continuously observed series")
        XCTAssertFalse(cpu.advisory)
    }

    /// A daemon from before ADR-0009 sends no `recovering`/`activity`; that must
    /// decode as "none", not fail the verdict and blank the menu bar to "nothing
    /// is watching" (Sparkle makes app/daemon skew inevitable). Mutation-proof:
    /// replace `decodeIfPresent … ?? false` with `decode` and this throws.
    func testAVerdictWithoutTheEpisodeFieldsStillDecodes() throws {
        var obj = try XCTUnwrap(
            JSONSerialization.jsonObject(with: Self.fixture("pressure-quiet.json")) as? [String: Any]
        )
        obj.removeValue(forKey: "recovering")
        obj.removeValue(forKey: "activity")
        var dims = try XCTUnwrap(obj["dimensions"] as? [[String: Any]])
        for i in dims.indices { dims[i].removeValue(forKey: "recovering") }
        obj["dimensions"] = dims
        let data = try JSONSerialization.data(withJSONObject: obj)
        let p = try Wire.decoder().decode(Pressure.self, from: data)
        XCTAssertFalse(p.recovering)
        XCTAssertEqual(p.activity, .none)
        XCTAssertTrue(p.dimensions.allSatisfy { !$0.recovering })
    }

    /// The FULLY-POPULATED fixture, at full depth. Nested objects escape scrutiny
    /// (interop rule 3) and Banshee's payload is nested by nature — per-dimension
    /// readings and per-finding actions.
    func testDecodesTheShriekingFixtureAtFullDepth() throws {
        let p = try Wire.decoder().decode(
            Pressure.self, from: Self.fixture("pressure-shrieking.json")
        )
        XCTAssertEqual(p.level, .shrieking)
        XCTAssertEqual(p.source, .memory)
        XCTAssertEqual(p.glyph, "💀🧠")
        XCTAssertEqual(p.accessibilityLabel, "Banshee: Shrieking, memory")
        XCTAssertEqual(p.dimensions.count, 3)

        let cpu = try XCTUnwrap(p.dimensions.first { $0.key == "cpu" })
        XCTAssertEqual(cpu.band, .red)
        XCTAssertEqual(cpu.pending, .yellow, "a pending de-escalation")
        XCTAssertEqual(cpu.heldSecs, 3600)
        // A hole in the observations before the run. The client must be able to say
        // so rather than presenting held time as unbroken watching.
        XCTAssertEqual(cpu.observationGapSecs, 1029)
        // Floats are compared with a tolerance, deliberately: the wire contract
        // does not guarantee bit-exact f64 round-tripping in EITHER language.
        XCTAssertEqual(try XCTUnwrap(cpu.trendPerSec), 0.004_166_666_666_666_667, accuracy: 1e-15)

        let uptime = try XCTUnwrap(p.dimensions.first { $0.key == "uptime" })
        XCTAssertTrue(uptime.advisory, "uptime informs findings but never the level")
        XCTAssertEqual(uptime.unit, .days)

        // The worklist arrives in core's order, ranked by measured impact.
        XCTAssertEqual(
            p.findings.map(\.action),
            [.relaunchApps, .reapStaleSessions, .reboot]
        )
        // …with the wording already attached, so no client has to keep a table of
        // action strings in step with Rust's by hand (ADR-0005).
        XCTAssertEqual(
            p.findings.map(\.actionLabel),
            [
                "Quit and relaunch the biggest apps",
                "Reap stale agent sessions",
                "Reboot",
            ]
        )
        // The who-line: swap and cpu name someone, uptime names nobody —
        // an EXPLICIT null with an empty list, so both branches are in one fixture.
        XCTAssertEqual(
            p.findings.map(\.whoLine),
            [
                "who: Chrome ×115 at 7.7 GB, claude ×8 at 2.1 GB",
                "who: claude ×8 at 62% of one core, managed agents ×13 at 49% of one core",
                nil,
            ]
        )
        XCTAssertEqual(
            p.findings[0].who,
            [
                Consumer(name: "Chrome", count: 115, detail: "7.7 GB"),
                Consumer(name: "claude", count: 8, detail: "2.1 GB"),
            ]
        )
        XCTAssertEqual(p.findings[2].who, [])
        XCTAssertEqual(p.concerning.count, 3, "all three are out of band")
    }

    /// The jetsam fixture: a kernel kill drives Shrieking on
    /// its own — no sustained catastrophic red elsewhere — so the client sees the
    /// new memory model over the wire. `memory` is demoted to an advisory GREEN
    /// row (it must never decide the level), `kernelFree`/`compressor` are advisory
    /// reds, and only the non-advisory `jetsam` red is a finding.
    func testDecodesTheJetsamFixture() throws {
        let p = try Wire.decoder().decode(
            Pressure.self, from: Self.fixture("pressure-jetsam.json")
        )
        XCTAssertEqual(p.level, .shrieking)
        XCTAssertEqual(p.source, .memory)

        let jetsam = try XCTUnwrap(p.dimensions.first { $0.key == "jetsam" })
        XCTAssertEqual(jetsam.band, .red)
        XCTAssertFalse(jetsam.advisory, "a jetsam kill must drive the level")
        XCTAssertEqual(jetsam.unit, .count)

        // Availability reads GREEN and advisory even at Shrieking — the yk8 point.
        let memory = try XCTUnwrap(p.dimensions.first { $0.key == "memory" })
        XCTAssertEqual(memory.band, .green)
        XCTAssertTrue(memory.advisory, "availability is informational, not a driver")

        let kernelFree = try XCTUnwrap(p.dimensions.first { $0.key == "kernelFree" })
        XCTAssertTrue(kernelFree.advisory)
        XCTAssertEqual(kernelFree.unit, .percent)
        let compressor = try XCTUnwrap(p.dimensions.first { $0.key == "compressor" })
        XCTAssertTrue(compressor.advisory)
        XCTAssertEqual(compressor.unit, .bytes)

        // Only the non-advisory jetsam red is a finding; the advisory reds are muted.
        XCTAssertEqual(p.findings.map(\.dimension), ["jetsam"])
        XCTAssertEqual(p.findings.first?.action, .relaunchApps)
    }

    /// Every enum case the wire can carry must decode. An unknown case throws, and
    /// a client that throws on a state the server can legitimately report shows an
    /// error instead of a verdict — the failure mode is a blank menu bar during the
    /// exact moment the user needs it.
    func testEveryLevelBandUnitAndActionCaseDecodes() throws {
        for level in Level.allCases {
            let json = Data("\"\(level.rawValue)\"".utf8)
            XCTAssertEqual(try Wire.decoder().decode(Level.self, from: json), level)
        }
        for band in Band.allCases {
            let json = Data("\"\(band.rawValue)\"".utf8)
            XCTAssertEqual(try Wire.decoder().decode(Band.self, from: json), band)
        }
        // Rust emits these as camelCase (`serde(rename_all = "camelCase")`), which
        // for single-word variants is just lowercase and for the others is NOT
        // snake_case. `perSecond` and `reapStaleSessions` are the cases that would
        // break under a snake_case assumption.
        for raw in ["ratio", "bytes", "perSecond", "count", "percent", "days"] {
            XCTAssertNoThrow(
                try Wire.decoder().decode(ReadingUnit.self, from: Data("\"\(raw)\"".utf8)),
                "ReadingUnit must decode \(raw)"
            )
        }
        for raw in [
            "reapStaleSessions", "reapOrphans", "relaunchApps", "reboot",
            "openDiskTool", "none",
        ] {
            XCTAssertNoThrow(
                try Wire.decoder().decode(Action.self, from: Data("\"\(raw)\"".utf8)),
                "Action must decode \(raw)"
            )
        }
        for raw in ["cpu", "memory", "disk", "sprawl", "corporate"] {
            XCTAssertNoThrow(
                try Wire.decoder().decode(Source.self, from: Data("\"\(raw)\"".utf8)),
                "Source must decode \(raw)"
            )
        }
    }

    func testEncodesCamelCaseWithExplicitTimestampFormat() throws {
        let p = try Wire.decoder().decode(
            Pressure.self, from: Self.fixture("pressure-quiet.json")
        )
        let data = try Wire.encoder().encode(p)
        let obj = try XCTUnwrap(
            JSONSerialization.jsonObject(with: data) as? [String: Any]
        )
        XCTAssertNotNil(obj["evaluatedAt"], "keys must be camelCase")
        XCTAssertNotNil(obj["levelName"])
        XCTAssertNil(obj["level_name"], "snake_case must not appear")
        XCTAssertEqual(obj["evaluatedAt"] as? String, "2026-08-31T15:04:05.123456Z")
    }

    // Pins the hand-written encode(to:). The synthesized Codable encoder omits nil
    // keys (encodeIfPresent), which violates present-as-null on encode — this test
    // FAILS against the derived conformance, in BOTH types.
    func testEncodesNullableFieldsAsPresentNulls() throws {
        let p = try Wire.decoder().decode(
            Pressure.self, from: Self.fixture("pressure-quiet.json")
        )
        XCTAssertNil(p.source)
        let data = try Wire.encoder().encode(p)
        let obj = try XCTUnwrap(JSONSerialization.jsonObject(with: data) as? [String: Any])
        let source = try XCTUnwrap(obj["source"], "source must be PRESENT as null")
        XCTAssertTrue(source is NSNull, "source must be null, got \(source)")

        // …and at depth, which is where the derived conformance would still be in
        // use if only the outer type had been hand-written.
        let dims = try XCTUnwrap(obj["dimensions"] as? [[String: Any]])
        let cpu = try XCTUnwrap(dims.first)
        for key in ["observationGapSecs", "trendPerSec", "pending"] {
            let value = try XCTUnwrap(cpu[key], "\(key) must be PRESENT as null, not absent")
            XCTAssertTrue(value is NSNull, "\(key) must be null, got \(value)")
        }
    }

    // Foot-gun guard: each hand-written encode(to:) is a HARD-CODED field list. If
    // a field is added to the struct but not to encode, the synthesized decoder
    // reads it while the encoder silently drops it — an asymmetric round-trip that
    // loses data on write with zero compiler complaint. These pin the key sets, so
    // they go red the moment a list falls behind.
    func testEncodeCoversEveryPressureField() throws {
        let p = try Wire.decoder().decode(
            Pressure.self, from: Self.fixture("pressure-shrieking.json")
        )
        XCTAssertNotNil(p.source, "the fixture must populate every nullable field")
        let data = try Wire.encoder().encode(p)
        let obj = try XCTUnwrap(JSONSerialization.jsonObject(with: data) as? [String: Any])
        XCTAssertEqual(
            Set(obj.keys),
            [
                "evaluatedAt", "level", "levelName", "source", "glyph",
                "accessibilityLabel", "dimensions", "findings", "sampleCount",
                "censusCount", "recovering", "activity",
            ],
            "encode(to:) key set drifted — a Pressure field was added without a matching encode line"
        )
    }

    /// `whoLine` is nullable, so `Finding.encode` is hand-written and its key list
    /// is pinned here — including that a nil line is PRESENT as null (the uptime
    /// finding), not dropped. Mutation-proof: remove the `encodeNil` branch and
    /// the third key set loses `whoLine`.
    func testEncodeCoversEveryFindingField() throws {
        let p = try Wire.decoder().decode(
            Pressure.self, from: Self.fixture("pressure-shrieking.json")
        )
        let expected: Set<String> = [
            "dimension", "band", "message", "action", "actionLabel", "who", "whoLine",
        ]
        for finding in p.findings {
            let data = try Wire.encoder().encode(finding)
            let obj = try XCTUnwrap(JSONSerialization.jsonObject(with: data) as? [String: Any])
            XCTAssertEqual(
                Set(obj.keys), expected,
                "encode(to:) key set drifted for \(finding.dimension) — a Finding field was added without a matching encode line"
            )
        }
        let uptime = try XCTUnwrap(p.findings.first { $0.dimension == "uptime" })
        XCTAssertNil(uptime.whoLine, "the fixture's uptime finding names nobody")
        let obj = try XCTUnwrap(
            JSONSerialization.jsonObject(with: Wire.encoder().encode(uptime)) as? [String: Any])
        XCTAssertTrue(obj["whoLine"] is NSNull, "a nil who-line is present as null: \(obj)")
    }

    func testEncodeCoversEveryDimensionField() throws {
        let p = try Wire.decoder().decode(
            Pressure.self, from: Self.fixture("pressure-shrieking.json")
        )
        let cpu = try XCTUnwrap(p.dimensions.first { $0.key == "cpu" })
        XCTAssertNotNil(cpu.trendPerSec, "the fixture must populate every nullable field")
        XCTAssertNotNil(cpu.pending)
        XCTAssertNotNil(cpu.observationGapSecs)
        let data = try Wire.encoder().encode(cpu)
        let obj = try XCTUnwrap(JSONSerialization.jsonObject(with: data) as? [String: Any])
        XCTAssertEqual(
            Set(obj.keys),
            [
                "dimension", "key", "label", "band", "value", "unit", "severity",
                "heldSecs", "observationGapSecs", "trendPerSec", "detail",
                "advisory", "pending", "recovering",
            ],
            "encode(to:) key set drifted — a DimensionReading field was added without a matching encode line"
        )
    }

    /// The whole verdict survives a round trip through our own codec, discrete
    /// fields exactly. Floats are excluded by the contract, so this asserts on what
    /// clients actually branch on.
    func testTheVerdictRoundTripsThroughOurCodec() throws {
        let original = try Wire.decoder().decode(
            Pressure.self, from: Self.fixture("pressure-shrieking.json")
        )
        let reencoded = try Wire.encoder().encode(original)
        let back = try Wire.decoder().decode(Pressure.self, from: reencoded)

        XCTAssertEqual(back.level, original.level)
        XCTAssertEqual(back.levelName, original.levelName)
        XCTAssertEqual(back.source, original.source)
        XCTAssertEqual(back.glyph, original.glyph)
        XCTAssertEqual(back.accessibilityLabel, original.accessibilityLabel)
        XCTAssertEqual(back.evaluatedAt, original.evaluatedAt)
        XCTAssertEqual(back.dimensions.map(\.key), original.dimensions.map(\.key))
        XCTAssertEqual(back.dimensions.map(\.band), original.dimensions.map(\.band))
        XCTAssertEqual(back.dimensions.map(\.pending), original.dimensions.map(\.pending))
        XCTAssertEqual(back.findings.map(\.action), original.findings.map(\.action))
    }

    // L3: the decode regex requires the colon in the offset (±HH:MM), matching
    // the wire contract. Colon-less offsets are rejected consistently at the
    // regex gate rather than depending on ICU parser leniency.
    func testColonlessOffsetIsRejected() {
        XCTAssertNil(WireDate.decode("2026-03-17T14:30:00+0100"), "colon-less offset is out of contract")
        XCTAssertNotNil(WireDate.decode("2026-03-17T14:30:00+01:00"), "colon offset is in contract")
    }

    // L4: the reviewer predicted a 3-digit year for pre-year-1000 dates. It
    // does not reproduce — `yyyy` zero-pads to a minimum of four digits — so
    // there is no lower-bound saturation guard. This pins the actual behavior:
    // an ancient instant still emits a 4-digit year that satisfies the `\d{4}`
    // contract and round-trips.
    func testEncodePadsAncientYearsToFourDigits() throws {
        let ancient = Date(timeIntervalSince1970: -70_000_000_000) // ~year 250
        let encoded = WireDate.encode(ancient)
        XCTAssertNotNil(
            try? #/^\d{4}-/#.prefixMatch(in: encoded),
            "year must be exactly 4 digits, got \(encoded)"
        )
        // Round-trips through our own decoder.
        XCTAssertEqual(WireDate.decode(encoded).map(WireDate.encode), encoded)
    }

    func testEncodeSaturatesAboveYear9999() {
        // Far past year 9999 → clamp to the maximum canonical instant.
        let farFuture = Date(timeIntervalSince1970: 400_000_000_000)
        XCTAssertEqual(WireDate.encode(farFuture), "9999-12-31T23:59:59.999999Z")
    }

    // The generous-decode table (interop guide rule 5): every known
    // producer's format must parse.
    func testDecodesAllKnownDatetimeVariants() {
        for variant in [
            "2026-03-17T14:30:00Z",
            "2026-03-17T14:30:00.123Z",
            "2026-03-17T14:30:00.123456Z",
            "2026-03-17T14:30:00.123456+00:00",
            "2026-03-17T14:30:00+01:00",
        ] {
            XCTAssertNotNil(WireDate.decode(variant), "must accept \(variant)")
        }
    }

    func testRejectsGarbageDatetimes() {
        for junk in ["", "yesterday", "2026-03-17", "14:30:00"] {
            XCTAssertNil(WireDate.decode(junk), "must reject \(junk)")
        }
    }

    // The SHARED generous-decode fixture — the Rust decode tests iterate the
    // same file. Every `input` must decode and re-encode to exactly
    // `canonical`; a Rust/Swift divergence on any producer variant fails here.
    func testSharedDatetimeVariantsDecodeToCanonical() throws {
        struct Variant: Decodable { let input: String; let canonical: String }
        struct Fixture: Decodable { let accept: [Variant]; let reject: [String] }
        let f = try JSONDecoder().decode(Fixture.self, from: Self.fixture("datetime-variants.json"))
        for row in f.accept {
            let d = try XCTUnwrap(WireDate.decode(row.input), "must accept \(row.input)")
            XCTAssertEqual(WireDate.encode(d), row.canonical, "canonical mismatch for \(row.input)")
        }
        for bad in f.reject {
            XCTAssertNil(WireDate.decode(bad), "must reject \(bad)")
        }
    }

    func testCanonicalEncodeRoundTripsThroughRustFormat() throws {
        // Encode then re-decode: the canonical 6-digit format must survive
        // our own decoder (and, by the shared fixtures, Rust's).
        let now = try XCTUnwrap(WireDate.decode("2026-08-19T10:00:00.250000Z"))
        let encoded = WireDate.encode(now)
        XCTAssertEqual(encoded, "2026-08-19T10:00:00.250000Z")
        XCTAssertEqual(WireDate.decode(encoded), now)
    }

    func testErrorShapeParsesFromRawBytes() {
        let raw = Data(#"{"error": "limit must be between 1 and 500 (got 5000)"}"#.utf8)
        XCTAssertEqual(
            APIClient.errorMessage(from: raw),
            "limit must be between 1 and 500 (got 5000)"
        )
        XCTAssertEqual(APIClient.errorMessage(from: Data("not json".utf8)), "unknown server error")
    }

    /// **The port literal is not free-floating.** Swift cannot read a Rust
    /// constant, so `APIClient.defaultPort` is a copy — and a copy of a fact is
    /// exactly how another project ended up with a server on 8766 and a config
    /// crate on 18766. This test reads the Rust source and fails if the two diverge.
    ///
    /// Mutation-proof, verified: change either side's literal and this fails,
    /// naming both values.
    func testDefaultPortMatchesTheRustConstant() throws {
        let libRS = URL(fileURLWithPath: #filePath)
            .deletingLastPathComponent()  // → BansheeCoreTests/
            .deletingLastPathComponent()  // → Tests/
            .deletingLastPathComponent()  // → swift/
            .deletingLastPathComponent()  // → repo root
            .appending(path: "rust/banshee-core/src/lib.rs")
        let source = try String(contentsOf: libRS, encoding: .utf8)

        let pattern = #/pub const DEFAULT_API_PORT: u16 = (?<port>\d+);/#
        let match = try XCTUnwrap(
            source.firstMatch(of: pattern),
            "DEFAULT_API_PORT is no longer declared in banshee-core/src/lib.rs — "
                + "if it moved, move this test's pointer with it rather than deleting the check"
        )
        let rustPort = try XCTUnwrap(Int(match.output.port))
        XCTAssertEqual(
            APIClient.defaultPort, rustPort,
            "Swift says \(APIClient.defaultPort), Rust says \(rustPort) — "
                + "the server and the app would talk past each other"
        )
        XCTAssertEqual(APIClient.defaultBaseURL.port, rustPort)
    }

    // H4: an empty or junk BANSHEE_API_URL must not crash the app on
    // launch. resolveBaseURL falls back to the default instead of force-
    // unwrapping nil.
    func testResolveBaseURLFailsSoftOnBadInput() {
        XCTAssertEqual(APIClient.resolveBaseURL(nil), APIClient.defaultBaseURL)
        XCTAssertEqual(APIClient.resolveBaseURL(""), APIClient.defaultBaseURL)
        XCTAssertEqual(APIClient.resolveBaseURL("   "), APIClient.defaultBaseURL)
        // Scheme-less / host-less values are not usable HTTP bases → default.
        XCTAssertEqual(APIClient.resolveBaseURL("localhost:18769"), APIClient.defaultBaseURL)
        // A valid override is honored.
        XCTAssertEqual(
            APIClient.resolveBaseURL("http://127.0.0.1:9000"),
            URL(string: "http://127.0.0.1:9000")
        )
    }

    /// The socket constants are COPIES of `banshee_core::API_SOCKET_ENV`,
    /// `API_SOCKET_FILE` and `MAX_SOCKET_PATH_LEN` (ADR-0008), pinned the same way the
    /// port is: read the Rust source and fail if either side drifts. The env var name
    /// is what makes `BANSHEE_API_SOCKET` mean the same socket to the CLI, the MCP
    /// server and the app; the file name is where the app looks for the daemon; the
    /// limit is what makes an over-long path fail with a message instead of `EINVAL`.
    func testSocketConstantsMatchTheRustConstants() throws {
        let libRS = URL(fileURLWithPath: #filePath)
            .deletingLastPathComponent()  // → BansheeCoreTests/
            .deletingLastPathComponent()  // → Tests/
            .deletingLastPathComponent()  // → swift/
            .deletingLastPathComponent()  // → repo root
            .appending(path: "rust/banshee-core/src/lib.rs")
        let source = try String(contentsOf: libRS, encoding: .utf8)

        let env = try XCTUnwrap(
            source.firstMatch(of: #/pub const API_SOCKET_ENV: &str = "(?<v>[^"]+)";/#),
            "API_SOCKET_ENV is no longer declared in banshee-core/src/lib.rs")
        XCTAssertEqual(APIClient.socketEnv, String(env.v))

        let file = try XCTUnwrap(
            source.firstMatch(of: #/pub const API_SOCKET_FILE: &str = "(?<v>[^"]+)";/#),
            "API_SOCKET_FILE is no longer declared in banshee-core/src/lib.rs")
        XCTAssertEqual(APIClient.socketFile, String(file.v))
        XCTAssertTrue(APIClient.defaultSocketPath.hasSuffix("/banshee/\(file.v)"))

        let limit = try XCTUnwrap(
            source.firstMatch(of: #/pub const MAX_SOCKET_PATH_LEN: usize = (?<v>\d+);/#),
            "MAX_SOCKET_PATH_LEN is no longer declared in banshee-core/src/lib.rs")
        XCTAssertEqual(UnixSocketHTTP.maxSocketPathLength, Int(limit.v))
    }

    // Version-skew resilience: a finding whose action this build doesn't know —
    // a since-renamed or newly-added action from a daemon on a different version
    // (a real crash once, when an action was renamed daemon-side) — must decode to `.unknown`, NOT throw and blank
    // the whole verdict. Mutation-proof: drop Action's lenient init(from:) and
    // this throws instead of returning .unknown.
    func testUnknownActionDecodesToUnknownNotThrow() throws {
        let json = Data(
            #"{"dimension":"disk","band":"red","message":"9 GB free.","action":"openSomethingNewer","actionLabel":"Open a disk tool"}"#
                .utf8)
        let finding = try JSONDecoder().decode(Finding.self, from: json)
        XCTAssertEqual(finding.action, .unknown)
        XCTAssertEqual(finding.actionLabel, "Open a disk tool")
        XCTAssertEqual(finding.band, .red)
        // The same bytes predate the who-line: neither key present decodes as
        // "names nobody", not as a decode failure. Mutation-proof: `decode` in
        // place of `decodeIfPresent` for either key and this throws.
        XCTAssertEqual(finding.who, [])
        XCTAssertNil(finding.whoLine)
    }

    // Version-skew resilience for the other strict String enums a newer daemon can
    // legitimately extend (the thermal source was exactly this case before the
    // thermal dimension landed; "gpu" stands in for the next one). An unknown source or unit must
    // decode, not blank the verdict. Each case is mutation-proof against dropping the lenient init: with
    // the derived decoder, `Source("thermal")` throws.
    func testUnknownSourceDecodesToUnknownNotThrow() throws {
        let source = try Wire.decoder().decode(Source.self, from: Data(#""gpu""#.utf8))
        XCTAssertEqual(source, .unknown)
    }

    func testUnknownUnitDecodesToUnknownNotThrow() throws {
        let unit = try Wire.decoder().decode(ReadingUnit.self, from: Data(#""celsius""#.utf8))
        XCTAssertEqual(unit, .unknown)
    }

    /// The whole-verdict pin: a real pressure fixture with an unknown source AND an
    /// unknown unit injected still decodes as a `Pressure`, with every other field
    /// intact. This is the failure that matters — one foreign token in a 10-row
    /// verdict must not take the other nine rows down with it.
    func testPressureWithUnknownSourceAndUnitStillDecodes() throws {
        var json = try XCTUnwrap(String(data: Self.fixture("pressure-shrieking.json"), encoding: .utf8))
        XCTAssertTrue(json.contains(#""source": "memory""#), "fixture shape changed; re-pin")
        json = json.replacingOccurrences(of: #""source": "memory""#, with: #""source": "gpu""#)
        XCTAssertTrue(json.contains(#""unit": "ratio""#), "fixture shape changed; re-pin")
        json = json.replacingOccurrences(of: #""unit": "ratio""#, with: #""unit": "celsius""#)

        let p = try Wire.decoder().decode(Pressure.self, from: Data(json.utf8))
        XCTAssertEqual(p.source, .unknown)
        XCTAssertEqual(p.level, .shrieking, "everything else in the verdict survives")
        XCTAssertFalse(p.dimensions.isEmpty)
        XCTAssertTrue(p.dimensions.contains { $0.unit == .unknown })
    }

    /// Level and Band are DELIBERATELY strict (see the doc comment on `Level`): a
    /// verdict this build cannot rank must fail loudly, not be ranked by a guess.
    func testUnknownLevelAndBandStillThrow() {
        XCTAssertThrowsError(try Wire.decoder().decode(Level.self, from: Data(#""screaming""#.utf8)))
        XCTAssertThrowsError(try Wire.decoder().decode(Band.self, from: Data(#""purple""#.utf8)))
    }

    func testAnnouncementLineParses() {
        let url = DaemonService.parseAnnouncement(
            "banshee-api listening on http://127.0.0.1:63294\n"
        )
        XCTAssertEqual(url?.port, 63294)
        XCTAssertNil(DaemonService.parseAnnouncement("some other log line"))
        XCTAssertNil(DaemonService.parseAnnouncement(""))
    }

    /// The LAST announcement in a log wins, not the first.
    ///
    /// launchd appends across restarts, so the top of the file is whatever port the
    /// daemon bound days ago. Mutation-proof: return on the first match and this
    /// fails with the stale port — which is how you end up talking to something
    /// that has not served since Tuesday.
    func testLastAnnouncedURLWinsOverEarlierOnes() {
        let log = """
            banshee-api listening on http://127.0.0.1:18769
            some other log line
            banshee-api listening on http://127.0.0.1:18770
            trailing noise
            """
        XCTAssertEqual(DaemonService.lastAnnouncedURL(inLog: log)?.port, 18770)
        XCTAssertNil(DaemonService.lastAnnouncedURL(inLog: "nothing here"))
        XCTAssertNil(DaemonService.lastAnnouncedURL(inLog: ""))
    }

    /// The repair BUTTON runs exactly the command the advice tells the user to
    /// type — never a different repair. And it must target the gui domain of the
    /// CURRENT user; a hardcoded uid would kick another account's agent.
    func testRepairCommandMatchesTheAdviceAndTheLabel() {
        XCTAssertEqual(
            DaemonService.repairCommandArguments(uid: 503),
            ["kickstart", "-k", "gui/503/com.tedswinyar.banshee-api"]
        )
        XCTAssertEqual(
            DaemonService.repairCommandArguments(uid: 1201),
            ["kickstart", "-k", "gui/1201/com.tedswinyar.banshee-api"],
            "the uid must be the caller's, not a literal"
        )
        // `-k` is load-bearing: without it a RUNNING-but-wedged daemon is not
        // restarted, and installed-but-not-answering is exactly the state the
        // button exists for.
        XCTAssertTrue(DaemonService.repairCommandArguments().contains("-k"))
    }

    /// The repair advice is END-USER facing: it points at the on-screen action
    /// (Restart / Install), never a raw shell command a DMG user can't run, and
    /// never implies the machine is fine just because we can't see it.
    func testRepairAdviceIsUserFacingAndActionable() {
        let advice = DaemonService.repairAdvice()
        XCTAssertFalse(advice.isEmpty)
        XCTAssertTrue(
            advice.contains("Restart") || advice.contains("Install"),
            "advice must point at the on-screen action, got: \(advice)"
        )
        for shellism in ["launchctl", "kickstart", "make daemon", "$(", "./scripts"] {
            XCTAssertFalse(advice.contains(shellism), "advice leaks a shell command: \(shellism)")
        }
    }
}
