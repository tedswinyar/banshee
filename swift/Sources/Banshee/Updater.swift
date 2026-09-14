// Auto-update via Sparkle (banshee-u0a5-sparkle).
//
// SPUStandardUpdaterController is Sparkle's turnkey controller: constructing it
// with `startingUpdater: true` reads SUFeedURL + SUPublicEDKey from Info.plist,
// begins the scheduled background checks, and drives the standard update UI. It
// must be a MAIN-THREAD object that lives for the whole app lifetime, so it is a
// @MainActor singleton owned here rather than a view-scoped value.
//
// Nothing about this contradicts ADR-0005: Sparkle updates the APP binary; it
// has nothing to do with the pressure verdict, which the daemon still owns.

import Sparkle

@MainActor
final class Updater {
    static let shared = Updater()

    private let controller: SPUStandardUpdaterController

    private init() {
        // startingUpdater:true → begins scheduled checks immediately. No custom
        // delegates: the standard driver's UI is exactly right for a menu-bar app.
        controller = SPUStandardUpdaterController(
            startingUpdater: true,
            updaterDelegate: nil,
            userDriverDelegate: nil
        )
    }

    /// User-initiated "Check for Updates…". Sparkle shows its own progress and
    /// "you're up to date" / "update available" UI.
    func checkForUpdates() {
        controller.updater.checkForUpdates()
    }

    /// Whether a manual check is currently allowed (Sparkle disables it briefly
    /// while a check is already running), so the menu item can be greyed out.
    var canCheckForUpdates: Bool {
        controller.updater.canCheckForUpdates
    }
}
