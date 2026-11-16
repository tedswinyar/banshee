// How the views reach the app's updater without this library linking Sparkle.
//
// The views live in a library so they can be drawn offscreen by tests
// (`banshee-v54.5`). Two of them offer "Check for Updates…", and the real updater
// is Sparkle's — which must not be constructed in a test process: it wants a
// bundle, an appcast and the network. So the views talk to this protocol, the app
// target's `Updater` conforms to it, and the app fills `UpdateControl.current` in
// at launch. Until it does (and in every test) the control is inert: nothing is
// available, nothing can be checked, and asking does nothing.

import Foundation

@MainActor
public protocol UpdateControlling: AnyObject {
    /// The version a scheduled check has found and not yet shown, if any.
    var availableVersion: String? { get }
    /// Whether a manual check is allowed right now.
    var canCheckForUpdates: Bool { get }
    func checkForUpdates()
}

@MainActor
public enum UpdateControl {
    /// The app's updater. Set once, at launch, by the app target.
    public static var current: any UpdateControlling = InertUpdateControl()
}

@MainActor
final class InertUpdateControl: UpdateControlling {
    let availableVersion: String? = nil
    let canCheckForUpdates = false
    func checkForUpdates() {}
}

/// Window identifiers, so `openWindow(id:)` and the scene cannot drift apart on a
/// string literal.
public enum WindowID {
    public static let detail = "banshee.detail"
}
