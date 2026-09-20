// Auto-update via Sparkle (banshee-u0a5-sparkle), with gentle reminders (banshee-xpu).
//
// SPUStandardUpdaterController is Sparkle's turnkey controller: constructing it
// with `startingUpdater: true` reads SUFeedURL + SUPublicEDKey from Info.plist,
// begins the scheduled background checks, and drives the standard update UI. It
// must be a MAIN-THREAD object that lives for the whole app lifetime, so it is a
// @MainActor singleton owned here rather than a view-scoped value.
//
// **Gentle reminders.** Banshee is a background app (no Dock icon by default), and
// Sparkle logs on every launch that such an app which schedules checks but has no
// gentle reminders risks its update alert going unnoticed: the window appears
// behind whatever the user is doing and nothing bounces. So when a SCHEDULED check
// finds an update that Sparkle cannot show in immediate focus, this takes over the
// reminder: the menu bar glyph wears a badge and the popover footer names the
// version, and either one brings Sparkle's own alert forward. User-initiated
// checks are untouched — Sparkle always shows those itself.
//
// Nothing about this contradicts ADR-0005: Sparkle updates the APP binary; it
// has nothing to do with the pressure verdict, which the daemon still owns. It
// also does not update the DAEMON — see `DaemonAgreement` for how that gap is
// surfaced after an update lands.

import Observation
import Sparkle

@MainActor
@Observable
final class Updater {
    static let shared = Updater()

    /// The display version of an update a SCHEDULED check found and Sparkle did not
    /// show in focus. Non-nil is the reminder: badge the glyph, name it in the
    /// popover. Cleared when the user gives the alert attention or the session ends.
    private(set) var availableVersion: String?

    @ObservationIgnored private let controller: SPUStandardUpdaterController
    // Retained here: the controller holds its delegates weakly.
    @ObservationIgnored private let reminders: GentleReminderDelegate

    private init() {
        let reminders = GentleReminderDelegate()
        self.reminders = reminders
        // startingUpdater:true → begins scheduled checks immediately.
        controller = SPUStandardUpdaterController(
            startingUpdater: true,
            updaterDelegate: nil,
            userDriverDelegate: reminders
        )
        reminders.onUpdateAvailable = { [weak self] version in
            Task { @MainActor in self?.availableVersion = version }
        }
        reminders.onReminderDone = { [weak self] in
            Task { @MainActor in self?.availableVersion = nil }
        }
    }

    /// User-initiated "Check for Updates…", and also how a reminder is answered:
    /// Sparkle documents this call as the way to bring an already-found update's
    /// alert into focus.
    func checkForUpdates() {
        controller.updater.checkForUpdates()
    }

    /// Whether a manual check is currently allowed (Sparkle disables it briefly
    /// while a check is already running), so the menu item can be greyed out.
    var canCheckForUpdates: Bool {
        controller.updater.canCheckForUpdates
    }
}

/// Sparkle's user-driver delegate, reduced to the two gentle-reminder decisions.
///
/// A separate NSObject rather than `Updater` itself: the protocol is Objective-C
/// and its methods are called nonisolated, which does not mix with an `@Observable`
/// main-actor class. The closures hop back to the main actor.
private final class GentleReminderDelegate: NSObject, SPUStandardUserDriverDelegate {
    var onUpdateAvailable: ((String) -> Void)?
    var onReminderDone: (() -> Void)?

    var supportsGentleScheduledUpdateReminders: Bool { true }

    /// Let Sparkle show a scheduled update itself ONLY when it can do so in focus
    /// (app just launched, system idle). Otherwise the alert would land behind the
    /// user's windows, so this delegate handles the reminder instead.
    func standardUserDriverShouldHandleShowingScheduledUpdate(
        _ update: SUAppcastItem, andInImmediateFocus immediateFocus: Bool
    ) -> Bool {
        immediateFocus
    }

    func standardUserDriverWillHandleShowingUpdate(
        _ handleShowingUpdate: Bool, forUpdate update: SUAppcastItem, state: SPUUserUpdateState
    ) {
        // A user-initiated check is already in the user's face; only a scheduled find
        // needs a reminder. Remind even when Sparkle shows it (immediate focus): the
        // alert can still be dismissed by a stray click, and the badge costs nothing.
        guard !state.userInitiated else { return }
        onUpdateAvailable?(update.displayVersionString)
    }

    func standardUserDriverDidReceiveUserAttention(forUpdate update: SUAppcastItem) {
        onReminderDone?()
    }

    func standardUserDriverWillFinishUpdateSession() {
        onReminderDone?()
    }
}
