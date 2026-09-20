// The status item, in AppKit.
//
// **Not `MenuBarExtra`.** SwiftUI's `MenuBarExtra` created a status item that existed
// in the accessibility tree — `AXTitle` read "Banshee: Shrieking, CPU" correctly — and
// rendered nothing a person could see. The label was `Text(glyph)`, and SwiftUI turns
// a `MenuBarExtra` label into an image for the status item; an emoji does not survive
// that, so the visible result was a meaningless blob. The failure is invisible from
// every angle a test can reach: the app runs, the item exists, the accessibility label
// is right, and the menu bar looks empty.
//
// `NSStatusItem` with `button.title` set to the glyph renders the emoji as TEXT, in
// colour, which is what the whole design depends on. Another project settled on the
// same shape for the same reason: AppKit rather than MenuBarExtra.
//
// It also buys verifiability. `statusItem.button.window.frame` gives real screen
// coordinates, so the rendered glyph can be screenshotted at a known position instead
// of hunted for — which is how this bug was finally confirmed rather than guessed at.

import AppKit
import SwiftUI
import BansheeCore
import DesignKit

@MainActor
final class MenuBarController: NSObject {
    static let shared = MenuBarController()

    private var statusItem: NSStatusItem!
    private var popover: NSPopover!
    private let model = PressureModel.shared
    private let updater = Updater.shared

    private override init() {
        super.init()
    }

    func setup() {
        setupStatusItem()
        setupPopover()
        updateTitle()
        observeVerdict()
    }

    /// The status item's screen frame, for verification.
    ///
    /// Exposed because "is the glyph actually visible" turned out to be the hardest
    /// question in this phase, and the answer needs coordinates rather than a search.
    var buttonFrame: NSRect? {
        statusItem?.button?.window?.frame
    }

    // MARK: - Setup

    private func setupStatusItem() {
        // Variable length, not `squareLength`: the glyph is one or two emoji depending
        // on whether a source is named, and a fixed square would clip the suffix — the
        // suffix being the half that says WHICH pressure is winning.
        statusItem = NSStatusBar.system.statusItem(withLength: NSStatusItem.variableLength)

        guard let button = statusItem.button else { return }
        button.imagePosition = .imageOnly
        button.action = #selector(handleClick)
        button.target = self
        button.sendAction(on: [.leftMouseUp, .rightMouseUp])
    }

    private func setupPopover() {
        popover = NSPopover()
        popover.contentSize = NSSize(width: 340, height: 320)
        // Transient: clicking away dismisses it, which is what a status-item panel
        // should do. A semi-transient popover would stay up over other apps.
        popover.behavior = .transient
        popover.animates = true
        popover.contentViewController = NSHostingController(
            rootView: MenuBarPanel().environment(model)
        )
    }

    // MARK: - The glyph

    /// Re-register observation after every change.
    ///
    /// `withObservationTracking` fires its `onChange` ONCE per registration, so the
    /// re-registration inside the handler is not optional — without it the glyph
    /// updates a single time and then freezes, which looks exactly like a working
    /// menu bar item on a calm machine.
    private func observeVerdict() {
        withObservationTracking {
            _ = model.menuBarGlyph
            _ = model.menuBarAccessibilityLabel
            _ = updater.availableVersion
        } onChange: { [weak self] in
            Task { @MainActor in
                self?.updateTitle()
                self?.observeVerdict()
            }
        }
    }

    private func updateTitle() {
        guard let button = statusItem?.button else { return }

        // An IMAGE, not `button.title`. A status item draws its title as a mask, which
        // turned 💀🧠 into a single hollow ring — see `MenuBarGlyph` for the two
        // implementations that rendered nothing visible and how each failed.
        //
        // A nil image (empty glyph) leaves the previous one in place rather than
        // emptying the menu bar, which would be indistinguishable from a crash.
        // The badge is the gentle update reminder (banshee-xpu): a scheduled check
        // found a newer version and Sparkle could not show it in focus.
        let available = updater.availableVersion
        if let image = MenuBarGlyph.image(for: model.menuBarGlyph, badged: available != nil) {
            button.image = image
        }

        // An emoji is invisible to VoiceOver, so the spoken label is the verdict's own
        // words — composed in core, so every surface says the same thing (ADR-0005).
        // This sets AXDescription, which VoiceOver prefers over AXTitle. The badge is
        // invisible to VoiceOver too, so the reminder is spoken as well.
        var label = model.menuBarAccessibilityLabel
        if let available {
            label += ". Update to \(available) available"
        }
        button.setAccessibilityLabel(label)
        button.toolTip = label
    }

    // MARK: - Interaction

    @objc private func handleClick() {
        guard let event = NSApp.currentEvent, event.type == .rightMouseUp else {
            togglePopover()
            return
        }
        showContextMenu()
    }

    func togglePopover() {
        if popover.isShown {
            popover.performClose(nil)
            model.panelClosed()
            return
        }
        guard let button = statusItem.button else { return }
        popover.show(relativeTo: button.bounds, of: button, preferredEdge: .minY)
        NSApp.activate(ignoringOtherApps: true)
        // The panel polls faster while it is open, and refreshes immediately on
        // opening so it never shows a reading that is nearly a whole cadence old.
        Task { await model.panelOpened() }
    }

    /// Right-click: quit, and nothing else.
    ///
    /// Deliberately not "Open Banshee" as well. Opening a SwiftUI `Window` scene from
    /// AppKit means either a registered URL scheme or a notification round-trip, and
    /// the popover one left-click away already has the button — via
    /// `@Environment(\.openWindow)`, which is the supported route. A second, worse
    /// path to the same window is not worth an Info.plist entry.
    private func showContextMenu() {
        let menu = NSMenu()
        let check = menu.addItem(
            withTitle: "Check for Updates…",
            action: #selector(checkForUpdates),
            keyEquivalent: ""
        )
        check.target = self
        // Grey it out while a check is already in flight (Sparkle's own state).
        check.isEnabled = Updater.shared.canCheckForUpdates
        menu.addItem(.separator())
        menu.addItem(
            withTitle: "Quit Banshee",
            action: #selector(quit),
            keyEquivalent: "q"
        ).target = self

        statusItem.menu = menu
        statusItem.button?.performClick(nil)
        // Detach immediately, or a LEFT click stops opening the popover and starts
        // opening this menu instead.
        statusItem.menu = nil
    }

    @objc private func checkForUpdates() {
        Updater.shared.checkForUpdates()
    }

    @objc private func quit() {
        NSApp.terminate(nil)
    }
}
