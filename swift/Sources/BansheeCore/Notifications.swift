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
// property of `Level`, mirroring core. The daemon RECORDS alert episodes and the
// app POSTS banners for the verdict they produce.
//
// The hard part of a monitor is not detecting a problem, it is not becoming noise:
// every banner that fires when nothing needs doing spends the user's willingness to
// look at the next one, and a monitor nobody looks at is worse than none because it
// occupies the space where a working one would go.

import Foundation

/// One banner to post.
public struct PendingNotification: Equatable, Sendable {
    public let title: String
    public let body: String
    /// Stable per level+source, so the system coalesces repeats of the same
    /// condition rather than stacking them.
    public let identifier: String
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

    private let sink: NotificationSink
    private var lastPosted: [String: Date] = [:]
    /// The level at the previous evaluation, so an ESCALATION can break the
    /// cooldown. Getting worse is news even if the last banner was recent.
    private var previousLevel: Level?

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
        guard pressure.level.warrantsNotification else { return nil }

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

    /// Coalescing key: level plus source.
    ///
    /// Not the level alone — moving from "wailing, memory" to "wailing, disk" is a
    /// different problem needing a different action, and suppressing the second
    /// would hide it. Not the whole verdict either, or a one-byte change in a
    /// findings message would defeat the cooldown.
    static func key(for pressure: Pressure) -> String {
        "\(pressure.level.rawValue)|\(pressure.source?.rawValue ?? "none")"
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
            // The who-line sits between the finding and the action —
            // the server's line, verbatim, and omitted when it named nobody.
            let who = action.whoLine.map { "\n\($0)" } ?? ""
            body = "\(action.message)\(who)\n→ \(action.actionLabel)"
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
            identifier: "banshee.\(key(for: pressure))"
        )
    }
}
