// Deciding when to interrupt the user.
//
// **This lives in the app, not the daemon, and that is a platform constraint rather
// than a design choice** (ADR-0004): `UNUserNotificationCenter` requires a bundled
// app with a bundle identifier and user authorization, which a launchd agent has
// none of. So the daemon RECORDS alert events and the app POSTS them.
//
// The split does not fragment the severity decision. The daemon decides what is
// true; this file decides only whether the user has already been told. Nothing here
// re-derives a level, a band or a glyph (ADR-0005) — `warrantsNotification` is a
// property of `Level`, mirroring core, and the disk channel below reads the BAND
// core put on the wire. The daemon RECORDS alert episodes and the app POSTS
// banners for the verdict they produce.
//
// The hard part of a monitor is not detecting a problem, it is not becoming noise:
// every banner that fires when nothing needs doing spends the user's willingness to
// look at the next one, and a monitor nobody looks at is worse than none because it
// occupies the space where a working one would go.
//
// **But one dimension is different, and it earned its own channel (banshee-3ue).**
// The overall verdict names ONE source — the dimension with the highest severity —
// and on a machine where CPU, swap and thermal are red for hours at a time (35 alert
// episodes in one day, measured 2026-09-22) that source is almost never disk. So a
// red disk behind a redder CPU produced a banner titled "Wailing, CPU" whose body
// described swap, and the disk was never named. Disk is the only dimension where
// waiting makes the problem unfixable — swapfiles live on the same volume, so a full
// disk caps swap and then the kernel kills — which is why it may interrupt on its own
// account regardless of what else is red. That is a DELIVERY policy over a band the
// server already decided, not a second severity model.

import Foundation

/// How hard a banner tries to be seen. The app maps this onto
/// `UNNotificationInterruptionLevel`; it lives here, not in the sink, so the
/// decision is testable and so a banner whose whole point is to survive on screen
/// cannot silently be posted as one that does not.
///
/// There is deliberately no `critical` case. Critical alerts need the
/// `com.apple.developer.usernotifications.critical-alerts` entitlement, which is a
/// signing change Apple must approve; the maintainer ruled it out for now
/// (2026-09-22). If it is ever added, add the case here first so a test can pin
/// which banner gets it.
public enum Interruption: String, Equatable, Sendable {
    /// The system default: shown, then auto-dismissed after a few seconds, and
    /// withheld entirely under Focus. Fine for a condition that will still be true
    /// — and still visible in the menu bar — when the user next looks.
    case active
    /// Presented immediately and through Focus. For a condition that gets WORSE the
    /// longer it goes unread. Degrades to `.active` on a build without the
    /// time-sensitive capability, never to an error.
    case timeSensitive
}

/// One banner to post.
public struct PendingNotification: Equatable, Sendable {
    public let title: String
    public let body: String
    /// Stable per condition, so the system coalesces repeats of the same
    /// condition rather than stacking them.
    public let identifier: String
    public let interruption: Interruption

    public init(title: String, body: String, identifier: String, interruption: Interruption) {
        self.title = title
        self.body = body
        self.identifier = identifier
        self.interruption = interruption
    }
}

/// Where a decided notification goes. Mocked in tests; the real one talks to
/// `UNUserNotificationCenter` (see `UserNotificationSink` in the app target).
public protocol NotificationSink: Sendable {
    func post(_ notification: PendingNotification) async
}

/// Decides whether a verdict is worth a banner.
///
/// Deliberately a separate, pure-ish type rather than logic inside the view model:
/// "have we already said this" is the whole substance of not being noise, and it
/// needs to be testable without a notification centre, a bundle, or a UI.
@MainActor
public final class NotificationCoordinator {
    /// Notifications for the same level+source are suppressed for this long.
    ///
    /// One hour, matching the daemon's own episode repeat interval
    /// (`PressureConfig.episodeRepeatSecs`, ADR-0009), so a user reading a banner
    /// and a user reading `banshee alerts` see the same rhythm. Two different rate
    /// limits on one condition would be two things to explain.
    public static let cooldown: TimeInterval = 3600

    /// A red disk is re-said this often for as long as it stays red.
    ///
    /// Fifteen minutes. Red is "below 30 GB", which the maintainer calls having
    /// fallen out of the range he wants to hold at all times, and the measured drain
    /// under agent load is ~7 GB/hour — so each repeat is ~1.75 GB of headroom gone.
    /// Sixteen banners over the four hours from red to empty is the intended shape
    /// of an emergency nobody has answered; the identifier is stable, so they replace
    /// rather than stack.
    public static let diskRedRepeat: TimeInterval = 15 * 60

    /// A yellow disk speaks on ENTRY, then at most this often while it holds.
    ///
    /// Six hours. Yellow (30–60 GB) is "drifting down, still cheap to fix", and the
    /// maintainer may sit in it for a whole day once build targets regrow; a
    /// monitor that nags hourly about that is a monitor that gets its notifications
    /// turned off. Twice a working day is a reminder, not a nag. Ruled 2026-09-22.
    public static let diskYellowRepeat: TimeInterval = 6 * 3600

    /// The one dimension that gets a banner on its own account, whatever the
    /// overall verdict says. Matched against `DimensionReading.key`, which is
    /// core's `Dimension::key()` for the disk dimension.
    static let standaloneDimension = "disk"

    private let sink: NotificationSink
    private var lastPosted: [String: Date] = [:]
    /// The level at the previous evaluation, so an ESCALATION can break the
    /// cooldown. Getting worse is news even if the last banner was recent.
    private var previousLevel: Level?
    /// The disk band at the previous evaluation, so ENTERING a band can be told
    /// apart from sitting in it (which repeats slowly) and from falling back into
    /// it from a worse one (which is recovery, and not news).
    private var previousDiskBand: Band?
    /// When the disk channel last spoke about each band.
    private var lastDiskPosted: [Band: Date] = [:]

    public init(sink: NotificationSink) {
        self.sink = sink
    }

    /// Consider a verdict. Posts at most one banner.
    ///
    /// Returns what was posted, for tests and for the caller's own logging — a
    /// decision that only shows up as a side effect is a decision nobody can assert
    /// on.
    @discardableResult
    public func consider(_ pressure: Pressure, now: Date = Date()) async -> PendingNotification? {
        let previous = previousLevel
        previousLevel = pressure.level

        // `Checking` must never notify. It is not a claim about the machine, and a
        // banner saying "we have not looked yet" is pure noise — it would fire on
        // every daemon restart.
        guard pressure.level != .checking else { return nil }

        // Disk FIRST, and independent of the overall level. This is the fix for a
        // red disk hiding behind a redder CPU: the overall path below can only ever
        // name the verdict's single dominant source.
        if let disk = considerDisk(pressure, now: now) {
            await sink.post(disk)
            return disk
        }

        guard pressure.level.warrantsNotification else { return nil }

        // Disk is owned by its channel above whenever the wire carries a disk
        // reading. A Wailing verdict DRIVEN by disk must not speak a second time here
        // fifteen seconds after the disk banner — and when the disk channel is inside
        // its repeat interval, silence is the decision, not an oversight. (A verdict
        // naming disk with NO disk reading — a daemon this build only half
        // understands — falls through, so the problem is still said once.)
        if pressure.source == .disk, Self.diskReading(in: pressure) != nil { return nil }

        let key = Self.key(for: pressure)
        // Getting worse breaks the cooldown. Note what this does NOT buy: a first
        // escalation posts anyway, because the key includes the level and a new level
        // is a fresh key. What it buys is RE-escalation — dropping to Wailing and
        // climbing back to Shrieking inside the hour, where the Shrieking key is
        // already in cooldown and the second peak would otherwise be silent.
        // (A mutation that removed this passed a test asserting only the first step;
        // see `testAnEscalationBreaksTheCooldown`.)
        let escalated = previous.map { pressure.level > $0 } ?? false

        if !escalated, let last = lastPosted[key], now.timeIntervalSince(last) < Self.cooldown {
            return nil
        }

        let notification = Self.compose(pressure)
        lastPosted[key] = now
        await sink.post(notification)
        return notification
    }

    /// The disk channel: a banner for the disk band alone.
    ///
    /// - Red: on entry, then every `diskRedRepeat` while it holds. Time-sensitive.
    /// - Yellow: on entry, then every `diskYellowRepeat` while it holds.
    /// - Falling from red to yellow says nothing — getting better is not an
    ///   interruption — but restarts the yellow clock, or the next evaluation would
    ///   re-post a stale yellow fifteen seconds into a recovery.
    /// - The first verdict this coordinator sees counts as entry. The app has just
    ///   started; if the disk is already yellow, that is exactly the thing the user
    ///   has not been told.
    private func considerDisk(_ pressure: Pressure, now: Date) -> PendingNotification? {
        let reading = Self.diskReading(in: pressure)
        let band = reading?.band
        let previous = previousDiskBand
        previousDiskBand = band

        guard let reading, let band, band > .green else { return nil }

        if let previous, previous > band {
            lastDiskPosted[band] = now
            return nil
        }
        let entered = previous.map { $0 < band } ?? true
        let repeatAfter = band == .red ? Self.diskRedRepeat : Self.diskYellowRepeat
        if !entered, let last = lastDiskPosted[band], now.timeIntervalSince(last) < repeatAfter {
            return nil
        }

        let notification = Self.composeDisk(reading, in: pressure)
        lastDiskPosted[band] = now
        return notification
    }

    static func diskReading(in pressure: Pressure) -> DimensionReading? {
        pressure.dimensions.first { $0.key == Self.standaloneDimension }
    }

    /// Coalescing key: level plus source.
    ///
    /// Not the level alone — moving from "wailing, memory" to "wailing, disk" is a
    /// different problem needing a different action, and suppressing the second
    /// would hide it. Not the whole verdict either, or a one-byte change in a
    /// findings message would defeat the cooldown.
    static func key(for pressure: Pressure) -> String {
        "\(pressure.level.rawValue)|\(pressure.source?.rawValue ?? "none")"
    }

    /// How hard the overall verdict's banner tries to be seen.
    ///
    /// Shrieking is time-sensitive: it is already the one level the daemon lets
    /// escape the machine (the Slack sink, ADR-0009), and a banner for it that is
    /// withheld under Focus and gone in five seconds is a banner shown to an empty
    /// room. Wailing stays `.active` — it is the level a person should look up for,
    /// not one that should break through a meeting, and the glyph carries it.
    static func interruption(for level: Level) -> Interruption {
        level >= .shrieking ? .timeSensitive : .active
    }

    /// The banner's words, taken from the server wherever the server has them.
    ///
    /// `accessibilityLabel` is already "Banshee: Wailing, memory" — composed in core
    /// so every surface says the same thing — and the top finding is already the
    /// most impactful thing to do, ranked by measured impact. Composing new prose
    /// here would be a second voice describing the same state.
    static func compose(_ pressure: Pressure) -> PendingNotification {
        let action = pressure.findings.first(where: { $0.action != .none })
        let body: String
        if let action {
            body = Self.body(for: action)
        } else if let first = pressure.findings.first {
            body = first.message
        } else {
            // Reachable: a level can be high with every finding suppressed (an
            // advisory-only dimension). Say the level rather than nothing.
            body = pressure.accessibilityLabel
        }
        return PendingNotification(
            title: pressure.accessibilityLabel,
            body: body,
            identifier: "banshee.\(key(for: pressure))",
            interruption: interruption(for: pressure.level)
        )
    }

    /// The disk banner's words. The title names the dimension by core's own label
    /// and the band core decided; the body is the disk finding when there is one
    /// (message, who-line, action — the same shape as every other banner) and the
    /// reading's core-composed detail line ("41.6 GB free; full in 2.6 hours") when
    /// there is not, so a yellow with no finding still says what is true.
    ///
    /// ONE identifier for both bands, so the red banner REPLACES the yellow one in
    /// Notification Center rather than sitting under it — the newest word on the
    /// disk is the only one worth keeping.
    static func composeDisk(_ reading: DimensionReading, in pressure: Pressure) -> PendingNotification {
        let finding = pressure.findings.first(where: { $0.dimension == reading.key })
        let body: String
        if let finding, finding.action != .none {
            body = Self.body(for: finding)
        } else if let finding {
            body = finding.message
        } else {
            body = reading.detail
        }
        return PendingNotification(
            title: "Banshee: \(reading.label) is \(reading.band.rawValue)",
            body: body,
            identifier: "banshee.\(reading.key)",
            interruption: reading.band == .red ? .timeSensitive : .active
        )
    }

    /// Finding, then the who-line (the server's line, verbatim, omitted when it
    /// named nobody), then the action.
    private static func body(for finding: Finding) -> String {
        let who = finding.whoLine.map { "\n\($0)" } ?? ""
        return "\(finding.message)\(who)\n→ \(finding.actionLabel)"
    }
}
