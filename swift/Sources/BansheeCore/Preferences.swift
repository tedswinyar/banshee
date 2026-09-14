// User preferences, and one that is load-bearing enough to explain.
//
// **Dock presence is a RUNTIME preference, not `LSUIElement` in Info.plist.**
// A hardcoded `LSUIElement` makes the app menu-bar-only forever, with no way back
// short of editing the bundle — and for an app whose main surface is a menu bar
// glyph but which also has real windows, both answers are defensible and the user
// should get to pick. `NSApp.setActivationPolicy(.accessory)` does the same job and
// can be changed while the app runs.

import Foundation

/// How the app appears in the Dock and the app switcher.
public enum DockPresence: String, CaseIterable, Sendable {
    /// Menu bar only: no Dock icon, no app switcher entry.
    case menuBarOnly
    /// A normal app, with a Dock icon.
    case dockAndMenuBar

    public var label: String {
        switch self {
        case .menuBarOnly: return "Menu bar only"
        case .dockAndMenuBar: return "Dock and menu bar"
        }
    }
}

/// Persisted preferences.
///
/// Backed by `UserDefaults` and injectable, so tests use a scratch suite instead of
/// mutating the developer's real defaults — a test that leaves state in
/// `UserDefaults.standard` fails differently on the second run, which is the worst
/// kind of flake.
/// Deliberately NOT `Sendable`: `UserDefaults` is not, under Swift 6's strict
/// checking. It is documented as thread-safe, so `@unchecked Sendable` would be
/// defensible — but nothing here needs to cross an isolation boundary (preferences
/// are read from `@MainActor` UI), and an unchecked conformance is a promise the
/// compiler stops checking. Cheaper to not make the claim.
public struct Preferences {
    private let defaults: UserDefaults

    private enum Key {
        static let dockPresence = "banshee.dockPresence"
        static let notificationsEnabled = "banshee.notificationsEnabled"
        static let loginItemRegistrationAttempted = "banshee.loginItemRegistrationAttempted"
    }

    public init(defaults: UserDefaults = .standard) {
        self.defaults = defaults
    }

    /// Defaults to menu-bar-only: the glyph IS the product, and a Dock icon for an
    /// app you are not meant to look at is clutter. Reversible in Settings.
    public var dockPresence: DockPresence {
        get {
            defaults.string(forKey: Key.dockPresence)
                .flatMap(DockPresence.init(rawValue:))
                ?? .menuBarOnly
        }
        nonmutating set { defaults.set(newValue.rawValue, forKey: Key.dockPresence) }
    }

    /// Defaults to ON. A monitor that has to be switched on before it can warn you
    /// is a monitor that was off during the incident.
    ///
    /// `object(forKey:) == nil` distinguishes "never set" from "set to false", which
    /// `bool(forKey:)` alone cannot — it returns false for both, so an unset
    /// preference would silently mean disabled.
    public var notificationsEnabled: Bool {
        get {
            guard defaults.object(forKey: Key.notificationsEnabled) != nil else {
                return true
            }
            return defaults.bool(forKey: Key.notificationsEnabled)
        }
        nonmutating set { defaults.set(newValue, forKey: Key.notificationsEnabled) }
    }

    /// Whether the app has ever tried to register itself as a login item.
    ///
    /// The login item's actual state lives with launchd (`SMAppService.mainApp.status`),
    /// not here — System Settings can change it behind the app's back and the Settings
    /// toggle reads launchd. What THIS remembers is only that the one-time first-launch
    /// registration has happened, so the app offers itself as a login item exactly
    /// once and thereafter respects whatever the user chose. Without it, a user who
    /// removed Banshee from Login Items would find it back after every launch.
    ///
    /// Defaults to false (never attempted).
    public var loginItemRegistrationAttempted: Bool {
        get { defaults.bool(forKey: Key.loginItemRegistrationAttempted) }
        nonmutating set { defaults.set(newValue, forKey: Key.loginItemRegistrationAttempted) }
    }
}
