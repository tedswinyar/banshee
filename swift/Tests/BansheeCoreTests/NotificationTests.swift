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
    ///
    /// These verdicts deliberately carry NO dimensions, so the disk one has no disk
    /// reading for the disk channel to own and this exercises the OVERALL path — the
    /// case of a daemon naming a source this build cannot see a reading for. The
    /// disk channel's own behaviour is tested below (banshee-3ue).
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
            dimension: "managedAgents",
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

    // ---- the disk channel (banshee-3ue) ---------------------------------
    //
    // The maintainer reached under 1 GB free on 2026-09-22 with no banner he could
    // see, while the daemon had recorded a four-hour disk episode. Three mechanisms
    // each swallowed it: the overall-level gate, the level|source key, and `.active`
    // auto-dismissal. A test asserting only that a notification was COMPOSED passes
    // while all three still drop it, so these assert on delivery to the sink AND on
    // the content naming disk AND on the interruption level.

    private func disk(_ band: Band, free: String = "27.4 GB") -> DimensionReading {
        .stub(
            key: "disk", label: "Disk headroom", band: band, value: 27.4e9, unit: .bytes,
            severity: band == .red ? 1.2 : 0.5, trendPerSec: -4_510_195,
            detail: "\(free) free; full in 1.7 hours"
        )
    }

    private func diskFinding(_ band: Band = .red) -> Finding {
        .stub(
            dimension: "disk", band: band,
            message: "Only 27.4 GB free on the data volume; full in 1.7 hours.",
            action: .openDiskTool
        )
    }

    /// The machine as it actually is on a bad afternoon: Shrieking, dominated by
    /// CPU, findings ordered swap → kernel pressure → CPU → thermal (observed live
    /// 2026-09-22 16:03), with the disk red at the BACK of the list.
    private func redderCPU(diskBand: Band = .red) -> Pressure {
        .stub(
            level: .shrieking, source: .cpu, glyph: "💀🔥",
            accessibilityLabel: "Banshee: Shrieking, CPU",
            dimensions: [
                .stub(key: "cpu", band: .red),
                .stub(key: "swap", label: "Swap in use", band: .red, unit: .bytes),
                disk(diskBand),
            ],
            findings: [
                .stub(dimension: "swap", message: "35.6 GB of swap in use.", action: .relaunchApps),
                .stub(dimension: "kernelPressure", message: "Kernel memory pressure is critical.", action: .relaunchApps),
                .stub(dimension: "cpu", message: "CPU is saturated: cores are 98% busy.", action: .reapStaleSessions),
                .stub(dimension: "thermal", message: "The kernel is throttling for heat.", action: .none),
                diskFinding(),
            ]
        )
    }

    /// **Tier two, the red line (below 30 GB): a red disk behind a redder CPU is
    /// DELIVERED, NAMED, and time-sensitive.** This is the exact shape of the 03:16
    /// banner that said "Wailing, cpu" and described swap.
    ///
    /// Mutation-proof, verified: remove the `considerDisk` call and the one banner
    /// posted is the CPU one — title fails the `disk` check; make the disk banner
    /// `.active` and the interruption assertion fails.
    func testARedDiskIsNamedEvenWhenCPUIsRedder() async throws {
        let sink = RecordingSink()
        let c = NotificationCoordinator(sink: sink)
        let now = Date(timeIntervalSince1970: 1_788_100_000)

        await c.consider(redderCPU(), now: now)

        let recorded = await sink.posted
        XCTAssertEqual(recorded.count, 1, "one banner per evaluation")
        let posted = try XCTUnwrap(recorded.first)
        XCTAssertTrue(
            posted.title.localizedCaseInsensitiveContains("disk"),
            "the disk must be NAMED, got: \(posted.title)"
        )
        XCTAssertEqual(posted.title, "Banshee: Disk headroom is red")
        XCTAssertTrue(posted.body.contains("27.4 GB free"), "got: \(posted.body)")
        XCTAssertTrue(posted.body.contains("Open a disk-usage tool"), "got: \(posted.body)")
        XCTAssertFalse(posted.body.contains("swap"), "the body must not narrate swap, got: \(posted.body)")
        XCTAssertEqual(posted.interruption, .timeSensitive, "a red disk must survive Focus")
    }

    /// While red HOLDS, the disk is re-said every `diskRedRepeat`, and the CPU
    /// banner the overall verdict wants is not lost either — it simply goes second.
    /// One banner per evaluation, so the two never race for the same slot.
    ///
    /// Mutation-proof, verified: drop the `lastDiskPosted[band]` write and the disk
    /// posts on every evaluation (the CPU banner never gets its turn — count 3 at
    /// the second step); set the repeat to the hour cooldown and the fourth step
    /// posts nothing.
    func testARedDiskRepeatsWhileItHolds() async {
        let sink = RecordingSink()
        let c = NotificationCoordinator(sink: sink)
        let now = Date(timeIntervalSince1970: 1_788_100_000)

        await c.consider(redderCPU(), now: now)
        // Fifteen seconds later the disk is inside its repeat; the verdict's own
        // (CPU) banner takes the slot.
        let second = await c.consider(redderCPU(), now: now.addingTimeInterval(15))
        XCTAssertEqual(second?.title, "Banshee: Shrieking, CPU")
        // Thirty seconds: nothing — both are inside their intervals.
        let third = await c.consider(redderCPU(), now: now.addingTimeInterval(30))
        XCTAssertNil(third)
        // Past the disk repeat: the disk again, long before the CPU hour is up.
        let fourth = await c.consider(
            redderCPU(), now: now.addingTimeInterval(NotificationCoordinator.diskRedRepeat + 1)
        )
        XCTAssertEqual(fourth?.title, "Banshee: Disk headroom is red")
        XCTAssertEqual(fourth?.interruption, .timeSensitive)

        let recorded = await sink.posted
        XCTAssertEqual(recorded.map(\.title), [
            "Banshee: Disk headroom is red",
            "Banshee: Shrieking, CPU",
            "Banshee: Disk headroom is red",
        ])
    }

    /// **Tier one, the yellow line (below 60 GB): the disk speaks on ENTRY even
    /// though the overall level is far below Wailing, then rarely.** Yellow is
    /// "leaving the range the maintainer wants to hold and can still act", and
    /// before this the 30–60 GB window produced no notification at all.
    ///
    /// Mutation-proof, verified: gate the disk channel on `warrantsNotification`
    /// and the first assertion fails; drop the repeat check and the +60 s step posts.
    func testDiskYellowNotifiesOnEntryThenRarely() async throws {
        let sink = RecordingSink()
        let c = NotificationCoordinator(sink: sink)
        let now = Date(timeIntervalSince1970: 1_788_100_000)
        // A calm machine apart from the disk: Restless, driven by CPU, no findings
        // for disk at all (the daemon opens no episode on yellow — banshee-dok).
        let yellow = Pressure.stub(
            level: .restless, source: .cpu, accessibilityLabel: "Banshee: Restless, CPU",
            dimensions: [.stub(key: "cpu", band: .yellow), disk(.yellow, free: "48.2 GB")]
        )

        let entered = await c.consider(yellow, now: now)
        let entry = try XCTUnwrap(entered, "yellow must notify on entry")
        XCTAssertEqual(entry.title, "Banshee: Disk headroom is yellow")
        XCTAssertEqual(entry.body, "48.2 GB free; full in 1.7 hours", "no finding → the reading's detail")
        XCTAssertEqual(entry.interruption, .active, "yellow informs; it does not break through Focus")

        let soon = await c.consider(yellow, now: now.addingTimeInterval(60))
        XCTAssertNil(soon, "a minute later is the same yellow")
        let hourLater = await c.consider(yellow, now: now.addingTimeInterval(3600))
        XCTAssertNil(hourLater, "the overall hour cooldown is not the yellow rhythm")
        let rarely = await c.consider(
            yellow, now: now.addingTimeInterval(NotificationCoordinator.diskYellowRepeat + 1)
        )
        XCTAssertNotNil(rarely, "still yellow after the repeat interval is worth one more line")

        let recorded = await sink.posted
        XCTAssertEqual(recorded.count, 2)
        XCTAssertEqual(Set(recorded.map(\.identifier)), ["banshee.disk"])
    }

    /// Entering red from yellow breaks the yellow clock at once — the yellow banner
    /// a minute ago does not buy the red one any silence — and the red REPLACES it
    /// (same identifier) rather than stacking under it.
    ///
    /// Mutation-proof, verified: key `lastDiskPosted` on the dimension alone and the
    /// red is suppressed by the fresh yellow timestamp.
    func testEnteringRedBreaksTheYellowClock() async throws {
        let sink = RecordingSink()
        let c = NotificationCoordinator(sink: sink)
        let now = Date(timeIntervalSince1970: 1_788_100_000)
        let yellow = Pressure.stub(
            level: .restless, source: .cpu, dimensions: [disk(.yellow, free: "31.0 GB")]
        )
        await c.consider(yellow, now: now)
        let escalated = await c.consider(redderCPU(), now: now.addingTimeInterval(60))
        let red = try XCTUnwrap(escalated)
        XCTAssertEqual(red.title, "Banshee: Disk headroom is red")
        XCTAssertEqual(red.interruption, .timeSensitive)

        let recorded = await sink.posted
        XCTAssertEqual(recorded.count, 2)
        XCTAssertEqual(Set(recorded.map(\.identifier)).count, 1, "red replaces yellow: one identifier")
    }

    /// Getting better is not an interruption: red → yellow says nothing, and the
    /// yellow clock restarts from the recovery, so the NEXT evaluation does not
    /// re-post a stale yellow fifteen seconds into feeling better.
    ///
    /// Mutation-proof, verified: remove the `previous > band` branch and the
    /// recovery step posts a yellow.
    func testDiskRecoveryIsNotNews() async {
        let sink = RecordingSink()
        let c = NotificationCoordinator(sink: sink)
        let now = Date(timeIntervalSince1970: 1_788_100_000)
        let yellow = Pressure.stub(
            level: .restless, source: .cpu, dimensions: [disk(.yellow, free: "44.0 GB")]
        )
        await c.consider(yellow, now: now)                              // yellow entry
        await c.consider(redderCPU(), now: now.addingTimeInterval(600)) // red entry
        // Space freed: back to yellow, well past the yellow repeat since the entry
        // banner — and still nothing, because falling INTO yellow is recovery.
        let recoveredAt = now.addingTimeInterval(NotificationCoordinator.diskYellowRepeat + 600)
        let recovered = await c.consider(yellow, now: recoveredAt)
        XCTAssertNil(recovered, "red → yellow is getting better")
        let justAfter = await c.consider(yellow, now: recoveredAt.addingTimeInterval(15))
        XCTAssertNil(justAfter, "the yellow clock restarted at recovery")
        let green = await c.consider(
            Pressure.stub(level: .quiet, dimensions: [disk(.green, free: "88.0 GB")]),
            now: recoveredAt.addingTimeInterval(30)
        )
        XCTAssertNil(green)

        let recorded = await sink.posted
        XCTAssertEqual(recorded.map(\.title), [
            "Banshee: Disk headroom is yellow",
            "Banshee: Disk headroom is red",
        ])
    }

    /// When the disk IS the dominant source, it is still said once per evaluation,
    /// not once by the disk channel and again by the overall path — and inside the
    /// disk repeat the overall path stays quiet rather than filling the gap with a
    /// second voice on the same problem.
    ///
    /// Mutation-proof, verified: remove the `source != .disk` guard and the second
    /// step posts "Banshee: Wailing, disk".
    func testADiskDrivenVerdictDoesNotSpeakTwice() async {
        let sink = RecordingSink()
        let c = NotificationCoordinator(sink: sink)
        let now = Date(timeIntervalSince1970: 1_788_100_000)
        let diskWailing = Pressure.stub(
            level: .wailing, source: .disk, accessibilityLabel: "Banshee: Wailing, disk",
            dimensions: [disk(.red)], findings: [diskFinding()]
        )
        let first = await c.consider(diskWailing, now: now)
        XCTAssertEqual(first?.title, "Banshee: Disk headroom is red")
        let second = await c.consider(diskWailing, now: now.addingTimeInterval(15))
        XCTAssertNil(second, "the disk channel already spoke; the verdict must not echo it")
        let recorded = await sink.posted
        XCTAssertEqual(recorded.count, 1)
    }

    /// ONLY disk has a standalone channel. Every other dimension goes through the
    /// overall verdict, or a yellow swap would interrupt at Restless — the noise this
    /// coordinator exists to prevent.
    ///
    /// Mutation-proof, verified: match any non-green dimension instead of the disk
    /// key and this posts.
    func testOnlyTheDiskGetsAStandaloneBanner() async {
        let sink = RecordingSink()
        let c = NotificationCoordinator(sink: sink)
        let restless = Pressure.stub(
            level: .restless, source: .memory,
            dimensions: [.stub(key: "swap", label: "Swap in use", band: .red, unit: .bytes)],
            findings: [.stub(dimension: "swap", message: "35.6 GB of swap in use.", action: .relaunchApps)]
        )
        let posted = await c.consider(restless)
        XCTAssertNil(posted)
        let recorded = await sink.posted
        XCTAssertTrue(recorded.isEmpty)
    }

    /// The interruption level is part of the DECISION, pinned here per banner.
    /// Wailing informs (`.active`); Shrieking — the level the daemon already lets
    /// escape to Slack — is time-sensitive. Nothing is `.critical`: there is no such
    /// case to reach for (maintainer ruling 2026-09-22).
    ///
    /// Mutation-proof, verified: return `.active` unconditionally and the Shrieking
    /// assertion fails.
    func testTheVerdictBannersInterruptionFollowsTheLevel() async {
        let sink = RecordingSink()
        let c = NotificationCoordinator(sink: sink)
        let now = Date(timeIntervalSince1970: 1_788_100_000)
        let wailing = await c.consider(verdict(.wailing, findings: [finding()]), now: now)
        XCTAssertEqual(wailing?.interruption, .active)
        let shrieking = await c.consider(
            verdict(.shrieking, findings: [finding()]), now: now.addingTimeInterval(30)
        )
        XCTAssertEqual(shrieking?.interruption, .timeSensitive)
    }
}
