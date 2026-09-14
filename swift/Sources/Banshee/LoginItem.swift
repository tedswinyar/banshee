// Open at login — the app half of "it starts by itself".
//
// The daemon already survives a reboot: it is a LaunchAgent with RunAtLoad (ADR-0004).
// Without this file the menu bar glyph did not: after a restart the sentinel kept
// sampling and nobody could see it until they opened the app by hand, which for a
// glance-at-it monitor is the same as being off.
//
// `SMAppService.mainApp` (macOS 13+) registers THIS bundle as a login item with launchd.
// launchd owns the truth — System Settings › General › Login Items can change it behind
// the app's back — so the Settings toggle reads `status` rather than a preference, and
// the only thing persisted here is that the one-time first-launch registration has
// happened (`Preferences.loginItemRegistrationAttempted`).

import AppKit
import BansheeCore
import ServiceManagement

enum LoginItem {
    /// True when launchd will start the app at login.
    static var isEnabled: Bool {
        SMAppService.mainApp.status == .enabled
    }

    /// The user turned it off in System Settings and must turn it on there; the app
    /// cannot override that choice, only point at it.
    static var requiresApproval: Bool {
        SMAppService.mainApp.status == .requiresApproval
    }

    /// Register or unregister. Errors are reported to the caller for the Settings UI;
    /// nothing here is fatal to the app.
    static func set(enabled: Bool) throws {
        if enabled {
            try SMAppService.mainApp.register()
        } else {
            try SMAppService.mainApp.unregister()
        }
    }

    /// Offer the app as a login item exactly once, on first launch, and only when it is
    /// running from /Applications — registering a `build/Banshee.app` from a source
    /// checkout would make a developer's throwaway build start at login. macOS shows
    /// its own "added as a login item" notice, so the user is told. Afterwards the flag
    /// is set whether or not registration succeeded, so a refusal is respected too.
    static func registerOnFirstLaunch(preferences: Preferences) {
        guard !preferences.loginItemRegistrationAttempted else { return }
        guard Bundle.main.bundleURL.path.hasPrefix("/Applications/") else { return }
        preferences.loginItemRegistrationAttempted = true
        do {
            try SMAppService.mainApp.register()
        } catch {
            NSLog("banshee: could not register as a login item: \(error.localizedDescription)")
        }
    }
}
