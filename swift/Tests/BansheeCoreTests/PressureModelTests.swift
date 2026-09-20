// PressureModel drives the API through the APIClientProtocol boundary, so these
// tests use MockAPIClient (the one approved mock) and exercise the real model —
// no UI, no server, no mocking of the thing under test.

import XCTest
@testable import BansheeCore

@MainActor
final class PressureModelTests: XCTestCase {
    func testRefreshLoadsTheVerdictAndMarksConnected() async {
        let mock = MockAPIClient()
        mock.verdict = .stub(level: .restless, source: .memory, glyph: "😧🧠")
        let model = PressureModel(client: mock)

        await model.refresh()

        XCTAssertEqual(model.pressure?.level, .restless)
        XCTAssertEqual(model.connectionState, .connected)
        XCTAssertNil(model.lastError)
        XCTAssertNotNil(model.lastRefresh)
    }

    /// After a Sparkle update the app is newer than the LaunchAgent and nothing
    /// errors (banshee-b25). The model reads the daemon's version on connection and
    /// says so. Mutation-proof: drop the `healthInfo()` read in `refresh()` and
    /// `daemonAgreement` stays nil; make `compare` return `.agree` and it says agree.
    func testADaemonLeftBehindByAnAppUpdateIsReported() async {
        let mock = MockAPIClient()
        mock.verdict = .stub(level: .quiet, source: nil, glyph: "😴")
        mock.healthInfoResult = HealthInfo(status: "ok", version: "0.1.4", gitRev: "abc1234")
        let model = PressureModel(client: mock, connectionState: .connecting)
        model.appVersion = "0.1.5"

        await model.refresh()

        XCTAssertEqual(model.daemonVersion, "0.1.4")
        XCTAssertEqual(model.daemonAgreement, .daemonBehind(daemon: "0.1.4", app: "0.1.5"))
    }

    /// Same release: nothing to say. Paired with the test above so the fixture can
    /// tell "compares versions" from "always complains".
    func testAMatchingDaemonAgrees() async {
        let mock = MockAPIClient()
        mock.verdict = .stub(level: .quiet, source: nil, glyph: "😴")
        mock.healthInfoResult = HealthInfo(status: "ok", version: "0.1.5", gitRev: "abc1234")
        let model = PressureModel(client: mock, connectionState: .connecting)
        model.appVersion = "0.1.5"

        await model.refresh()

        XCTAssertEqual(model.daemonAgreement, .agree)
    }

    /// The version is re-read on every RE-connection, not once: an install restarts
    /// the daemon, which shows as a failed refresh and then a connected transition,
    /// and the row must clear when the new daemon answers. Steady-state refreshes do
    /// not re-read it (that would be a second request every 15 seconds for nothing).
    func testTheDaemonVersionIsReReadOnReconnectionOnly() async {
        let mock = MockAPIClient()
        mock.verdict = .stub(level: .quiet, source: nil, glyph: "😴")
        mock.healthInfoResult = HealthInfo(status: "ok", version: "0.1.4", gitRev: "old")
        let model = PressureModel(client: mock, connectionState: .connecting)
        model.appVersion = "0.1.5"

        await model.refresh()
        await model.refresh()
        XCTAssertEqual(mock.healthInfoCalls, 1, "a steady connection does not re-read /health")

        // The daemon restarts under an update: one refresh fails, the next reconnects.
        mock.failWith = .httpError(status: 503, message: "restarting")
        await model.refresh()
        mock.failWith = nil
        mock.healthInfoResult = HealthInfo(status: "ok", version: "0.1.5", gitRev: "new")
        await model.refresh()

        XCTAssertEqual(mock.healthInfoCalls, 2)
        XCTAssertEqual(model.daemonAgreement, .agree)
    }

    /// Without the app's version there is nothing to compare against, and nil — not
    /// a false "agree" — is the honest answer.
    func testNoAppVersionMeansNoVerdictOnAgreement() async {
        let mock = MockAPIClient()
        mock.verdict = .stub(level: .quiet, source: nil, glyph: "😴")
        let model = PressureModel(client: mock, connectionState: .connecting)

        await model.refresh()

        XCTAssertNotNil(model.daemonVersion)
        XCTAssertNil(model.daemonAgreement)
    }

    /// **The glyph is rendered VERBATIM.** The model must not map a level to a face
    /// — that mapping lives once, in banshee-core, and travels over the wire
    /// (ADR-0005). A Swift `switch` here would have to be kept in step with a Rust
    /// `match` by hand, and the whole point of the glyph is that it means the same
    /// thing on every surface.
    ///
    /// Mutation-proof, verified: replace `pressure?.glyph` in `menuBarGlyph` with a
    /// `switch` over `level` and this fails, because no local mapping would produce
    /// the source suffix.
    func testTheMenuBarGlyphComesStraightFromTheWire() async {
        let mock = MockAPIClient()
        // A level-only mapping cannot produce this: 😱 is Wailing's face and 🧠 is
        // the memory source's suffix, composed together in core.
        mock.verdict = .stub(
            level: .wailing, source: .memory, glyph: "😱🧠",
            accessibilityLabel: "Banshee: Wailing, memory"
        )
        let model = PressureModel(client: mock)

        await model.refresh()

        XCTAssertEqual(model.menuBarGlyph, "😱🧠")
        XCTAssertEqual(model.menuBarAccessibilityLabel, "Banshee: Wailing, memory")
    }

    /// Before the first fetch the model shows the CHECKING bubble, not a calm face.
    /// This is the client-side twin of the server's `checking` state, and the two
    /// are distinct: the server says `checking` when it has not measured enough,
    /// this says nothing-yet when we have not asked.
    ///
    /// Mutation-proof: default `menuBarGlyph` to "😴" and this fails. That is the
    /// bug that teaches a user to distrust the one glyph that must be trusted.
    func testBeforeTheFirstFetchTheGlyphIsNotACalmFace() {
        let model = PressureModel(client: MockAPIClient())
        XCTAssertNil(model.pressure)
        XCTAssertEqual(model.menuBarGlyph, "🫧")
        XCTAssertNotEqual(model.menuBarGlyph, "😴", "never claim calm before looking")
        XCTAssertEqual(model.menuBarAccessibilityLabel, "Banshee: connecting")
    }

    /// A failed refresh KEEPS the last good verdict. Blanking it would replace a
    /// 15-second-old reading with nothing, which is strictly less information — and
    /// during a transient failure the old reading is still the best answer anyone
    /// has.
    ///
    /// Mutation-proof, verified: add `pressure = nil` to the catch block and this
    /// fails on the retained level.
    func testAFailedRefreshKeepsTheLastGoodVerdict() async {
        let mock = MockAPIClient()
        mock.verdict = .stub(level: .stirring, glyph: "🫥")
        let model = PressureModel(client: mock)
        await model.refresh()
        XCTAssertEqual(model.pressure?.level, .stirring)

        mock.failWith = .serverUnreachable("connection refused")
        await model.refresh()

        XCTAssertEqual(model.pressure?.level, .stirring, "the last reading must survive")
        XCTAssertEqual(model.menuBarGlyph, "🫥")
        XCTAssertNotNil(model.lastError, "and the failure must be surfaced")
        if case .failed = model.connectionState {} else {
            XCTFail("expected .failed, got \(model.connectionState)")
        }
    }

    /// **A locked-out app must not wear its last verdict's glyph** (`banshee-6ds`).
    ///
    /// Observed 2026-09-07: the 0.1.3 app, against a daemon that refused every one of
    /// its requests, kept "Banshee: Wailing, memory" in the menu bar for hours — its
    /// last accepted verdict, presented as current. The verdict itself is KEPT (the
    /// popover shows it as the last thing seen), but the glyph — the product — switches
    /// to the lock at once: the daemon answered and said no, which is not a transient,
    /// and the client had already retried with a re-read key before reporting it.
    ///
    /// Mutation-proof: make the `.unauthorized` arm of `menuBarGlyph` return
    /// `pressure?.glyph ?? "🔒"` (the old behaviour whenever a verdict is in hand) and
    /// this fails on the glyph while the verdict assertion still passes — which is
    /// exactly the combination the bug was.
    func testALockedOutAppDoesNotWearItsLastVerdictGlyph() async {
        let mock = MockAPIClient()
        mock.verdict = .stub(
            level: .wailing, source: .memory, glyph: "😱🧠",
            accessibilityLabel: "Banshee: Wailing, memory"
        )
        let model = PressureModel(client: mock)
        await model.refresh()
        XCTAssertEqual(model.menuBarGlyph, "😱🧠")
        XCTAssertNil(model.connectionNotice, "nothing to explain while the glyph is the verdict's")

        mock.failWith = .unauthorized("The daemon rejected this app's key.")
        await model.refresh()

        XCTAssertEqual(model.pressure?.level, .wailing, "the last verdict is kept for the popover")
        XCTAssertEqual(model.menuBarGlyph, "🔒", "but the menu bar must not present it as current")
        XCTAssertEqual(model.menuBarAccessibilityLabel, "Banshee: locked out of the daemon")
        XCTAssertEqual(model.menuBarTitle, "Locked out")
        XCTAssertEqual(model.connectionNotice, "The daemon rejected this app's key.")

        // Not sticky: the next accepted read puts the verdict back.
        mock.failWith = nil
        await model.refresh()
        XCTAssertEqual(model.menuBarGlyph, "😱🧠")
        XCTAssertEqual(model.menuBarTitle, "Wailing")
        XCTAssertNil(model.connectionNotice)
    }

    /// An UNREACHABLE daemon is the other case, and it is time-bound where the lock-out
    /// is immediate. A short outage — a `make daemon-install` restart, a laptop waking —
    /// keeps the last glyph, because the old reading is still the best answer anyone
    /// has. Past `staleAfter` it is a claim about a machine nobody has measured in a
    /// minute, and the menu bar falls back to the placeholder.
    ///
    /// The fixture distinguishes the implementations that would also pass a simpler
    /// one: "never stale" fails the second half; "stale at once" and an inverted
    /// comparison fail the first (40 seconds would read as stale); and two failures
    /// inside the window rule out a failure COUNT masquerading as elapsed time.
    func testAnUnreachableDaemonKeepsTheGlyphOnlyUntilTheVerdictIsStale() async {
        let mock = MockAPIClient()
        mock.verdict = .stub(level: .stirring, glyph: "🫥", accessibilityLabel: "Banshee: Stirring")
        let model = PressureModel(client: mock)
        let read = Date(timeIntervalSince1970: 1_788_100_000)
        model.now = { read }
        await model.refresh()
        XCTAssertEqual(model.menuBarGlyph, "🫥")

        mock.failWith = .serverUnreachable("connection refused")
        model.now = { read.addingTimeInterval(20) }
        await model.refresh()
        model.now = { read.addingTimeInterval(40) }
        await model.refresh()
        XCTAssertEqual(model.menuBarGlyph, "🫥", "40s of failures is a transient; the reading stands")
        XCTAssertEqual(model.menuBarAccessibilityLabel, "Banshee: Stirring")
        XCTAssertEqual(model.menuBarTitle, "Stirring")
        XCTAssertNil(model.connectionNotice, "a transient needs no notice; the error banner already shows")

        model.now = { read.addingTimeInterval(PressureModel.staleAfter + 1) }
        await model.refresh()
        XCTAssertEqual(model.pressure?.level, .stirring, "the verdict is still kept for the popover")
        XCTAssertEqual(model.menuBarGlyph, "🫧", "but the menu bar no longer presents it as current")
        XCTAssertEqual(model.menuBarAccessibilityLabel, "Banshee: daemon unreachable")
        XCTAssertEqual(model.menuBarTitle, "Unreachable")
        XCTAssertEqual(
            model.connectionNotice, "Cannot reach the Banshee server: connection refused",
            "and the popover is told why the glyph is not the verdict's"
        )
    }

    /// The staleness window must outlast a daemon restart but not a coffee break: it is
    /// several label cadences, and it is measured in TIME so the 5s popover cadence
    /// cannot give up on a verdict three times faster than the label does.
    func testTheStalenessWindowIsSeveralCadencesAndMeasuredInTime() {
        XCTAssertEqual(PressureModel.staleAfter, 60)
        XCTAssertGreaterThanOrEqual(PressureModel.staleAfter, 2 * 15, "at least two label cadences")
    }

    /// **A rejected key must NOT read as "nothing is watching"** (`banshee-dqh`).
    ///
    /// The two states look alike from here — neither yields a verdict — but they are
    /// opposite claims about the machine. `.failed` drives a pane that says "Nothing
    /// is watching"; a rotated key means the daemon IS watching, sampling and
    /// recording alerts throughout, and only this client is locked out. Collapsing
    /// them tells the user their monitoring has stopped when it has not, and sends
    /// them to restart a service that is working.
    ///
    /// This is the pin the APIClient tests could not provide: they prove the client
    /// throws `.unauthorized`, and this proves the model does not launder it back into
    /// `.failed` on the way to the UI. Mutation-proof: drop the `unauthorized` arm in
    /// `refresh()` and this fails while every client-side test still passes.
    func testARejectedKeyIsDistinctFromAnUnreachableDaemon() async {
        let mock = MockAPIClient()
        mock.failWith = .unauthorized("The daemon rejected this app's key. … still watching …")
        let model = PressureModel(client: mock)
        await model.refresh()

        guard case .unauthorized = model.connectionState else {
            return XCTFail(
                "a rejected key must be .unauthorized, not \(model.connectionState) — "
                    + ".failed renders as 'Nothing is watching', which would be false here")
        }
        XCTAssertNotNil(model.lastError, "and it must still be surfaced")
    }

    /// The other half: an unreachable daemon must NOT be mistaken for a key problem,
    /// or the UI would reassure the user that monitoring continues while it has in
    /// fact stopped. Neither test alone pins the split.
    func testAnUnreachableDaemonIsNotReportedAsAKeyProblem() async {
        let mock = MockAPIClient()
        mock.failWith = .serverUnreachable("connection refused")
        let model = PressureModel(client: mock)
        await model.refresh()

        guard case .failed = model.connectionState else {
            return XCTFail("an unreachable daemon must stay .failed, got \(model.connectionState)")
        }
    }

    /// …and recovering clears the error rather than leaving a stale banner up.
    func testASuccessfulRefreshClearsAPreviousError() async {
        let mock = MockAPIClient()
        mock.failWith = .serverUnreachable("down")
        let model = PressureModel(client: mock)
        await model.refresh()
        XCTAssertNotNil(model.lastError)

        mock.failWith = nil
        mock.verdict = .stub()
        await model.refresh()

        XCTAssertNil(model.lastError)
        XCTAssertEqual(model.connectionState, .connected)
    }

    /// The polling loop refreshes and then honours cancellation, so a closed window
    /// does not leave a request looping forever.
    ///
    /// Mutation-proof: swap `Task.sleep`'s `catch { return }` for `catch {}` and the
    /// loop spins on a cancelled task instead of exiting; the call count then keeps
    /// climbing after cancellation.
    func testPollingRefreshesThenStopsOnCancellation() async {
        let mock = MockAPIClient()
        mock.verdict = .stub()
        let model = PressureModel(client: mock)

        let task = Task { await model.poll() }
        // Wait for the first refresh rather than sleeping a fixed time: a fixed
        // sleep encodes an assumption about machine speed and is wrong in both
        // directions.
        for _ in 0..<200 where model.pressure == nil {
            try? await Task.sleep(for: .milliseconds(5))
        }
        XCTAssertNotNil(model.pressure, "the first poll must load a verdict")

        task.cancel()
        _ = await task.result
        let callsAtCancel = mock.pressureCalls
        try? await Task.sleep(for: .milliseconds(50))
        XCTAssertEqual(
            mock.pressureCalls, callsAtCancel,
            "no request may be issued after cancellation"
        )
    }

    /// `concerning` filters on the band the SERVER decided; it does not compare
    /// severities or values. Mutation-proof: filter on `severity > 0` instead and
    /// the hysteresis-held row (red band, severity below the red line because the
    /// raw value has already recovered) disappears — which is the case where a band
    /// and a severity genuinely come apart.
    func testConcerningUsesTheServersBandNotALocalThreshold() {
        let held = DimensionReading.stub(
            key: "disk", label: "Disk headroom", band: .red, severity: 0.80
        )
        let climbing = DimensionReading.stub(key: "cpu", band: .green, severity: -0.10)
        let verdict = Pressure.stub(dimensions: [climbing, held])

        XCTAssertEqual(verdict.concerning.map(\.key), ["disk"])
    }

    /// The level scale must order the way core's derived `Ord` does, or a client
    /// comparing `level >= .wailing` to decide whether to notify gets it backwards.
    /// Mutation-proof: swap any two ranks and this fails.
    func testLevelOrderMatchesTheRustScale() {
        XCTAssertEqual(
            Level.allCases,
            [.checking, .quiet, .stirring, .restless, .wailing, .shrieking],
            "declaration order is the severity order"
        )
        XCTAssertTrue(Level.shrieking > .wailing)
        XCTAssertTrue(Level.wailing > .restless)
        XCTAssertTrue(Level.restless > .stirring)
        XCTAssertTrue(Level.stirring > .quiet)
        XCTAssertTrue(Level.quiet > .checking, "checking is the BOTTOM of the scale")

        // Banners at Wailing and above; a monitor that interrupts on Stirring gets
        // muted, and a muted monitor is worse than none.
        XCTAssertFalse(Level.restless.warrantsNotification)
        XCTAssertTrue(Level.wailing.warrantsNotification)
        XCTAssertTrue(Level.shrieking.warrantsNotification)
    }

    func testBandOrderPutsRedWorstAndGreenBest() {
        XCTAssertEqual(Band.allCases, [.green, .yellow, .red])
        XCTAssertTrue(Band.red > .yellow)
        XCTAssertTrue(Band.yellow > .green)
    }

    /// The refresh cadence must not outrun the sampler. Polling faster than the
    /// sampler publishes wastes requests on a verdict that cannot have changed;
    /// slower shows a stale glyph.
    func testTheRefreshIntervalMatchesTheSamplerCadence() {
        XCTAssertEqual(
            PressureModel.refreshInterval, .seconds(15),
            "the cheap tier samples every 15s (docs/signal-collection.md)"
        )
    }
}
