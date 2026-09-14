// Preferences, against a scratch UserDefaults suite.
//
// Never `UserDefaults.standard`: a test that leaves state there fails differently on
// the second run, which is the worst kind of flake — it passes locally, fails in a
// fresh checkout, and the difference is invisible in the code.

import XCTest
@testable import BansheeCore

final class PreferencesTests: XCTestCase {
    private var suiteName: String!
    private var defaults: UserDefaults!

    override func setUp() {
        super.setUp()
        suiteName = "banshee.tests.\(UUID().uuidString)"
        defaults = UserDefaults(suiteName: suiteName)
    }

    override func tearDown() {
        defaults.removePersistentDomain(forName: suiteName)
        super.tearDown()
    }

    /// Menu-bar-only by default: the glyph IS the product, and a Dock icon for an app
    /// you are not meant to look at is clutter.
    func testDockPresenceDefaultsToMenuBarOnly() {
        let prefs = Preferences(defaults: defaults)
        XCTAssertEqual(prefs.dockPresence, .menuBarOnly)
    }

    func testDockPresenceRoundTrips() {
        let prefs = Preferences(defaults: defaults)
        for presence in DockPresence.allCases {
            prefs.dockPresence = presence
            XCTAssertEqual(Preferences(defaults: defaults).dockPresence, presence)
        }
    }

    /// A junk stored value falls back to the default rather than crashing or leaving
    /// the app in no state at all. Preferences are user-editable via `defaults write`.
    func testAnUnknownStoredDockPresenceFallsBack() {
        defaults.set("hovercraft", forKey: "banshee.dockPresence")
        XCTAssertEqual(Preferences(defaults: defaults).dockPresence, .menuBarOnly)
    }

    /// **Notifications default to ON.** A monitor that has to be switched on before it
    /// can warn you is a monitor that was off during the incident.
    ///
    /// Mutation-proof, verified: replace the getter with a plain
    /// `defaults.bool(forKey:)` and this fails. `bool(forKey:)` returns false for BOTH
    /// "never set" and "set to false", so an unset preference would silently mean
    /// disabled — the trap this getter exists to avoid.
    func testNotificationsDefaultToEnabled() {
        XCTAssertTrue(Preferences(defaults: defaults).notificationsEnabled)
    }

    /// …and an explicit `false` is honoured, which is the other half of the same
    /// distinction: the getter must tell "unset" from "off".
    func testNotificationsCanBeExplicitlyDisabled() {
        let prefs = Preferences(defaults: defaults)
        prefs.notificationsEnabled = false
        XCTAssertFalse(Preferences(defaults: defaults).notificationsEnabled)

        prefs.notificationsEnabled = true
        XCTAssertTrue(Preferences(defaults: defaults).notificationsEnabled)
    }

    /// Two `Preferences` values over the same defaults are one setting, not two — it
    /// is a view onto storage, not a cache. Otherwise Settings would write somewhere
    /// the AppDelegate does not read.
    func testPreferencesAreAViewOntoStorageNotACopy() {
        let a = Preferences(defaults: defaults)
        let b = Preferences(defaults: defaults)
        a.dockPresence = .dockAndMenuBar
        XCTAssertEqual(b.dockPresence, .dockAndMenuBar)
    }
}
