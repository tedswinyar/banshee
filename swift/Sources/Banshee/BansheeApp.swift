import SwiftUI
import BansheeCore
import DesignKit
import UserNotifications

@main
struct BansheeApp: App {
    @NSApplicationDelegateAdaptor(AppDelegate.self) private var appDelegate
    // @Observable model held as @State (not @StateObject) and passed down via
    // the environment; children read it with @Environment(PressureModel.self).
    // ONE model behind both scenes — the menu bar label and every window read the
    // same verdict, so they cannot disagree about it (the same reason the glyph is
    // computed once in core, ADR-0005).
    // The SHARED instance, so `AppDelegate` (which starts polling at launch) and
    // every scene are looking at one verdict.
    private let model = PressureModel.shared
    private let preferences = Preferences()

    var body: some Scene {
        // No `MenuBarExtra`. It produced a status item that existed in the
        // accessibility tree and rendered nothing visible: SwiftUI turns a
        // MenuBarExtra label into an image, and an emoji does not survive that. The
        // status item is AppKit now (`MenuBarController`), which renders the glyph as
        // TEXT — see that file for the full account; another project reached the
        // same conclusion.
        //
        // A window, opened from the popover rather than shown at launch: this app's
        // primary surface is the menu bar, and a window appearing on login would be
        // the opposite of a background sentinel.
        Window("Banshee", id: WindowID.detail) {
            DetailWindow()
                .environment(model)
        }
        .defaultSize(width: 560, height: 460)

        Settings {
            // Passed explicitly rather than through the environment. An
            // `EnvironmentKey`'s `defaultValue` is a static, which Swift 6 requires
            // to be `Sendable` — and `UserDefaults` is not, so the environment route
            // costs an `@unchecked Sendable` claim to serve exactly one consumer.
            SettingsView(preferences: preferences)
        }
    }
}

/// Window identifiers, so `openWindow(id:)` and the scene cannot drift apart on a
/// string literal.
enum WindowID {
    static let detail = "banshee.detail"
}

/// Posts banners through the real notification centre.
///
/// The `NotificationSink` boundary exists because this type cannot work in tests: it
/// needs a bundle identifier and user authorization, and a SwiftPM test binary has
/// neither. The DECISION of whether to post is in `NotificationCoordinator`, which is
/// testable; this only carries it out.
struct UserNotificationSink: NotificationSink {
    func post(_ notification: PendingNotification) async {
        let content = UNMutableNotificationContent()
        content.title = notification.title
        content.body = notification.body
        // Sound, because the levels that reach here are the ones worth looking up
        // for: Wailing and above.
        content.sound = .default

        let request = UNNotificationRequest(
            identifier: notification.identifier,
            content: content,
            trigger: nil
        )
        do {
            try await UNUserNotificationCenter.current().add(request)
        } catch {
            // A refused or unauthorized notification must not be fatal, and must not
            // be silent either — the menu bar glyph still carries the state, so this
            // is a degraded surface rather than a lost one.
            NSLog("banshee: could not post notification: \(error.localizedDescription)")
        }
    }
}

final class AppDelegate: NSObject, NSApplicationDelegate {
    private let preferences = Preferences()

    func applicationDidFinishLaunching(_ notification: Notification) {
        applyDockPresence()
        MenuBarController.shared.setup()

        // Start Sparkle now: constructing the controller begins the scheduled
        // background update checks (banshee-u0a5-sparkle). Referencing the
        // singleton is what instantiates it; the menu item drives manual checks.
        _ = Updater.shared

        // Start polling HERE rather than from a view's `.task`. A `MenuBarExtra`
        // label is a view, but its lifecycle belongs to the status item — relying on
        // it to kick off the only poll loop would make "the glyph never updates"
        // depend on an implementation detail of a system control, and the failure
        // would be silent.
        let notifier = preferences.notificationsEnabled
            ? NotificationCoordinator(sink: UserNotificationSink())
            : nil
        PressureModel.shared.start(notifier: notifier)

        if preferences.notificationsEnabled {
            requestNotificationAuthorization()
        }
    }

    /// Dock presence at RUNTIME, not `LSUIElement` in Info.plist.
    ///
    /// A hardcoded `LSUIElement` makes the app menu-bar-only forever with no way
    /// back short of editing the bundle. Both answers are defensible for an app
    /// whose main surface is a glyph but which also has real windows, so the user
    /// picks, and the choice takes effect without a relaunch.
    private func applyDockPresence() {
        switch preferences.dockPresence {
        case .menuBarOnly:
            NSApp.setActivationPolicy(.accessory)
        case .dockAndMenuBar:
            NSApp.setActivationPolicy(.regular)
        }
    }

    private func requestNotificationAuthorization() {
        // Fire-and-forget: a refusal is a valid answer and must not block launch.
        // The glyph is the primary surface; banners are an escalation on top of it.
        UNUserNotificationCenter.current()
            .requestAuthorization(options: [.alert, .sound]) { granted, error in
                if let error {
                    NSLog("banshee: notification authorization failed: \(error.localizedDescription)")
                } else if !granted {
                    NSLog("banshee: notifications not authorized; the menu bar glyph still works")
                }
            }
    }

    /// **Quitting the app must NOT stop collection.** The daemon is a LaunchAgent
    /// this app does not own (ADR-0004); there is deliberately no teardown here.
    /// Pressure accumulates while nobody is watching, and a monitor that only
    /// samples while its window is open cannot compute the one thing this app exists
    /// to produce.
    func applicationShouldTerminateAfterLastWindowClosed(_ sender: NSApplication) -> Bool {
        // False, unlike the template: this is a menu-bar app, and closing the detail
        // window must leave the glyph in place.
        false
    }
}
