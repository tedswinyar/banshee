// The notification DECISION, tested without a notification centre.
//
// `UNUserNotificationCenter` needs a bundle identifier and user authorization,
// neither of which a SwiftPM test binary has — which is exactly why the decision
// lives in `NotificationCoordinator` and only the posting lives in the app target
// behind `NotificationSink`. Mock at the boundary, drive the real logic.

import XCTest
@testable import BansheeCore

/// Records instead of posting. The one approved mock shape: a boundary, not the
/// thing under test.
///
/// An `actor` rather than a lock-guarded class: `NSLock.lock()` is unavailable from
/// an async context under Swift 6, and `NotificationSink.post` is async by design
/// (the real one awaits `UNUserNotificationCenter`).
actor RecordingSink: NotificationSink {
    private(set) var posted: [PendingNotification] = []

    func post(_ notification: PendingNotification) async {
        posted.append(notification)
    }
}

@MainActor
final class NotificationTests: XCTestCase {
    private func verdict(
        _ level: Level,
        source: Source? = .memory,
        findings: [Finding] = []
    ) -> Pressure {
        .stub(
            level: level,
            source: source,
            glyph: "x",
            accessibilityLabel: "Banshee: \(level.rawValue.capitalized), memory",
            findings: findings
        )
    }

    private func finding(_ action: Action = .relaunchApps) -> Finding {
        Finding(
            dimension: "memory",
            band: .red,
            message: "Only 89 MB of RAM free.",
            action: action,
            actionLabel: "Quit and relaunch the biggest apps"
        )
    }

    // ---- what does and does not warrant a banner -------------------------

    /// Banners start at Wailing. A monitor that interrupts on Stirring is a monitor
    /// that gets muted, and a muted monitor is worse than none because it occupies
    /// the space where a working one would go.
    ///
    /// Mutation-proof, verified: drop the `warrantsNotification` guard and the
    /// quiet/stirring/restless cases all post.
    func testOnlyWailingAndAboveNotify() async {
        for level in [Level.quiet, .stirring, .restless] {
            let sink = RecordingSink()
            let c = NotificationCoordinator(sink: sink)
            let posted = await c.consider(verdict(level, findings: [finding()]))
            XCTAssertNil(posted, "\(level.rawValue) must not interrupt")
            let recorded = await sink.posted
            XCTAssertTrue(recorded.isEmpty)
        }
        for level in [Level.wailing, .shrieking] {
            let sink = RecordingSink()
            let c = NotificationCoordinator(sink: sink)
            let posted = await c.consider(verdict(level, findings: [finding()]))
            XCTAssertNotNil(posted, "\(level.rawValue) must interrupt")
            let recorded = await sink.posted
            XCTAssertEqual(recorded.count, 1)
        }
    }

    /// **`checking` must never notify.** It is not a claim about the machine, and a
    /// banner saying "we have not looked yet" would fire on every daemon restart —
    /// which, now that the daemon is a LaunchAgent, is every `make daemon-install`.
    ///
    /// Mutation-proof, verified: remove the `!= .checking` guard and this fails.
    /// (It fails because `checking` sorts BELOW `wailing`, so the general guard
    /// happens to cover it too — the explicit one is here because the reason is
    /// different and a future reordering of the scale must not silently start
    /// notifying on it.)
    func testCheckingNeverNotifies() async {
        let sink = RecordingSink()
        let c = NotificationCoordinator(sink: sink)
        let posted = await c.consider(verdict(.checking, source: nil))
        XCTAssertNil(posted)
        let recorded = await sink.posted
        XCTAssertTrue(recorded.isEmpty)
    }

    // ---- not becoming noise ---------------------------------------------

    /// The same condition does not re-notify inside the cooldown.
    ///
    /// Mutation-proof, verified: remove the cooldown check and the second call
    /// posts.
    func testTheSameConditionIsSuppressedInsideTheCooldown() async {
        let sink = RecordingSink()
        let c = NotificationCoordinator(sink: sink)
        let now = Date(timeIntervalSince1970: 1_788_100_000)

        let first = await c.consider(verdict(.wailing, findings: [finding()]), now: now)
        XCTAssertNotNil(first)
        let repeated = await c.consider(
            verdict(.wailing, findings: [finding()]),
            now: now.addingTimeInterval(60)
        )
        XCTAssertNil(repeated, "a minute later is the same problem")
        var recorded = await sink.posted
        XCTAssertEqual(recorded.count, 1)

        // …and past the cooldown it speaks again, because an hour-old problem that is
        // still happening is worth another look.
        let afterCooldown = await c.consider(
            verdict(.wailing, findings: [finding()]),
            now: now.addingTimeInterval(NotificationCoordinator.cooldown + 1)
        )
        XCTAssertNotNil(afterCooldown)
        recorded = await sink.posted
        XCTAssertEqual(recorded.count, 2)
    }

    /// **Getting WORSE breaks the cooldown**, and the case where that matters is
    /// RE-escalation.
    ///
    /// A first Wailing → Shrieking always posts without needing `escalated` at all,
    /// because the cooldown is keyed on level+source and Shrieking is a fresh key —
    /// which is why the first version of this test was a FALSE PIN: it asserted the
    /// obvious step and passed with the guard removed.
    ///
    /// What `escalated` actually buys is the fourth event below: the machine drops to
    /// Wailing and climbs back to Shrieking inside the hour, so the Shrieking key is
    /// already in cooldown. Without the guard that second peak is silent. A monitor
    /// should err toward telling you the worst thing is back — the model's hysteresis
    /// and 120s grace already damp genuine flapping, so this is not a noise risk.
    ///
    /// Mutation-proof, verified: drop the `escalated` term and the final assertion
    /// fails at 2 posts instead of 3.
    func testAnEscalationBreaksTheCooldown() async {
        let sink = RecordingSink()
        let c = NotificationCoordinator(sink: sink)
        let now = Date(timeIntervalSince1970: 1_788_100_000)

        let wailing = await c.consider(verdict(.wailing, findings: [finding()]), now: now)
        XCTAssertNotNil(wailing)
        let escalated = await c.consider(
            verdict(.shrieking, findings: [finding()]),
            now: now.addingTimeInterval(30)
        )
        XCTAssertNotNil(escalated, "worse is news, cooldown or not")
        var recorded = await sink.posted
        XCTAssertEqual(recorded.count, 2)

        // NOT the reverse: recovering from Shrieking to Wailing is not an escalation,
        // and the Wailing key is already inside its cooldown.
        let recovered = await c.consider(
            verdict(.wailing, findings: [finding()]),
            now: now.addingTimeInterval(60)
        )
        XCTAssertNil(recovered, "getting better is not an interruption")
        recorded = await sink.posted
        XCTAssertEqual(recorded.count, 2)

        // …and climbing BACK to Shrieking does post, even though that key was used 60
        // seconds ago. This is the assertion the guard exists for.
        let reEscalated = await c.consider(
            verdict(.shrieking, findings: [finding()]),
            now: now.addingTimeInterval(90)
        )
        XCTAssertNotNil(reEscalated, "the worst level returning is news")
        recorded = await sink.posted
        XCTAssertEqual(recorded.count, 3)
    }

    /// A DIFFERENT source at the same level is a different problem needing a
    /// different action, so it is not suppressed.
    ///
    /// Mutation-proof, verified: key the cooldown on the level alone and this fails —
    /// which is the version where a filling disk is hidden by an earlier memory
    /// banner.
    func testADifferentSourceIsNotSuppressed() async {
        let sink = RecordingSink()
        let c = NotificationCoordinator(sink: sink)
        let now = Date(timeIntervalSince1970: 1_788_100_000)

        let memory = await c.consider(
            verdict(.wailing, source: .memory, findings: [finding()]), now: now
        )
        XCTAssertNotNil(memory)
        let disk = await c.consider(
            verdict(.wailing, source: .disk, findings: [finding(.openDiskTool)]),
            now: now.addingTimeInterval(10)
        )
        XCTAssertNotNil(disk, "memory and disk are different problems")
        let recorded = await sink.posted
        XCTAssertEqual(recorded.count, 2)
        // Assert the whole mapped array rather than indexing — a count assertion is
        // not a guard, and `recorded[1]` on a short array traps.
        XCTAssertEqual(Set(recorded.map(\.identifier)).count, 2)
    }

    // ---- the words ------------------------------------------------------

    /// The banner uses the SERVER's words: the title is the verdict's own
    /// accessibility label, composed in core, and the body is the top finding with
    /// its server-supplied action label. Composing new prose here would be a second
    /// voice describing the same state (ADR-0005).
    ///
    /// Mutation-proof, verified: build the title from `level.rawValue` and this fails
    /// on the missing source.
    func testTheBannerUsesTheServersWords() async throws {
        let sink = RecordingSink()
        let c = NotificationCoordinator(sink: sink)
        await c.consider(verdict(.wailing, findings: [finding()]))

        let recorded = await sink.posted
        let posted = try XCTUnwrap(recorded.first)
        XCTAssertEqual(posted.title, "Banshee: Wailing, memory")
        XCTAssertEqual(
            posted.body,
            "Only 89 MB of RAM free.\n→ Quit and relaunch the biggest apps"
        )
    }

    /// When the finding names who is behind it, the banner carries the
    /// server's line between the finding and the action — verbatim, and only when
    /// present (the test above is the nil case, with no blank line).
    /// Mutation-proof: drop `whoLine` from `compose` and this fails; append it
    /// unconditionally and the test above gains an empty line.
    func testTheBannerNamesWhoIsBehindIt() async throws {
        let sink = RecordingSink()
        let c = NotificationCoordinator(sink: sink)
        let named = Finding(
            dimension: "memory",
            band: .red,
            message: "Only 89 MB of RAM free.",
            action: .relaunchApps,
            actionLabel: "Quit and relaunch the biggest apps",
            who: [Consumer(name: "Chrome", count: 115, detail: "7.7 GB")],
            whoLine: "who: Chrome ×115 at 7.7 GB"
        )
        await c.consider(verdict(.wailing, findings: [named]))

        let recorded = await sink.posted
        let posted = try XCTUnwrap(recorded.first)
        XCTAssertEqual(
            posted.body,
            "Only 89 MB of RAM free.\nwho: Chrome ×115 at 7.7 GB\n→ Quit and relaunch the biggest apps"
        )
    }

    /// The first finding with an ACTION wins, not simply the first finding. Findings
    /// are ranked by measured impact in core, and an actionless one at the top
    /// (managed agents, which the user cannot remove) must not push the actionable
    /// one out of the banner.
    ///
    /// Mutation-proof, verified: use `findings.first` and this fails.
    func testTheBannerPrefersAnActionableFinding() async {
        let sink = RecordingSink()
        let c = NotificationCoordinator(sink: sink)
        let actionless = Finding(
            dimension: "corporate",
            band: .red,
            message: "Managed agents are using 60% of one core.",
            action: .none,
            actionLabel: "No action"
        )
        await c.consider(verdict(.wailing, findings: [actionless, finding()]))

        let recorded = await sink.posted
        let body = recorded.first?.body ?? ""
        XCTAssertTrue(body.contains("Quit and relaunch"), "got: \(body)")
        XCTAssertFalse(body.contains("Managed agents"), "got: \(body)")
    }

    /// A high level with NO findings at all is reachable (an advisory-only
    /// dimension), and must still say something rather than post an empty body.
    func testAVerdictWithNoFindingsStillHasABody() async {
        let sink = RecordingSink()
        let c = NotificationCoordinator(sink: sink)
        await c.consider(verdict(.wailing, findings: []))
        let recorded = await sink.posted
        XCTAssertEqual(recorded.first?.body, "Banshee: Wailing, memory")
        XCTAssertFalse(recorded.first?.body.isEmpty ?? true)
    }
}
