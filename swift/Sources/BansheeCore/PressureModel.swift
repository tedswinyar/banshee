// The verdict view model. Lives in BansheeCore (not the app target) so it is
// unit-testable at the network boundary: tests drive it with MockAPIClient and
// assert the polling and error paths without a UI or a real server.
//
// Observation, not Combine: `@Observable` (macOS 14+) is the current idiom —
// finer-grained invalidation, no `@Published`/`ObservableObject` boilerplate.
// Views hold it via `@State` and read it via `@Environment(PressureModel.self)`.

import Foundation
import os
import Observation

/// View model for the pressure verdict. Talks to the API through the
/// APIClientProtocol boundary only.
///
/// **No optimistic updates here, unlike the template's notes model.** There is
/// nothing to be optimistic about: the user cannot change the machine's pressure
/// from this window, and every number is a measurement. What replaces it is
/// POLLING — the verdict goes stale on its own, which a document-editing app never
/// has to worry about.
@MainActor
@Observable
public final class PressureModel {
    private static let logger = Logger(subsystem: "com.tedswinyar.banshee", category: "PressureModel")

    /// The latest verdict, or `nil` before the first successful load.
    ///
    /// `nil` here means "this client has not fetched yet" — which is NOT the same
    /// as the server's `checking`, and the two must not be conflated. The server
    /// says `checking` when IT has not measured enough; this says `nil` when WE
    /// have not asked. A view showing "Checking" for a failed fetch would be
    /// reporting on the machine when it should be reporting on itself.
    public var pressure: Pressure?
    public var connectionState: ConnectionState = .connecting
    public var lastError: String?
    /// When the displayed verdict was fetched, so the UI can say how stale it is.
    public var lastRefresh: Date?

    public enum ConnectionState: Equatable {
        case connecting
        case connected
        case failed(String)
        /// The daemon is reachable and healthy but rejected our key, even after
        /// re-reading the key file (`banshee-dqh`).
        ///
        /// Separate from `failed` because the honest thing to tell the user is the
        /// OPPOSITE of what `failed` says. `failed` means nothing is watching the
        /// machine; this means the machine IS being watched and only this app is
        /// locked out. Collapsing them sent people off to restart a daemon that was
        /// working perfectly.
        case unauthorized(String)
    }

    /// How often to re-read the verdict for the MENU BAR LABEL.
    ///
    /// Matching the sampler's 15s cadence: longer would show a stale glyph, shorter
    /// would poll for a verdict that cannot have changed. The read is a single
    /// lock-and-clone of a value the sampler already computed — the API does not
    /// re-evaluate per request (ADR-0005) — so this is close to free.
    public static let refreshInterval: Duration = .seconds(15)

    /// How often to re-read while a panel is OPEN and being looked at.
    ///
    /// Faster than the label, because someone watching a number expects it to move,
    /// and 15 seconds of stillness reads as broken. Still bounded below by the
    /// sampler: polling faster than 15s cannot produce a new verdict, it just costs
    /// requests. 5s is a compromise — it catches a new sample within a third of a
    /// cadence, and the intermediate reads are cheap clones.
    public static let openPanelRefreshInterval: Duration = .seconds(5)

    /// How long a verdict may go un-refreshed before the menu bar stops presenting it
    /// as current (`banshee-6ds`).
    ///
    /// Four label cadences. A daemon restart (`make daemon-install`) or a laptop
    /// waking costs one or two missed reads, and for those the last reading is still
    /// the best answer anyone has. Past this the glyph is a claim about a machine
    /// nobody has measured in a minute, and the menu bar falls back to the
    /// placeholder. Time, not a failure count: the popover polls at 5s, and "three
    /// misses" would give up in 15 seconds there and 45 in the label.
    public static let staleAfter: TimeInterval = 60

    /// The clock `isStale` and `lastRefresh` use. A seam for tests, which cannot wait
    /// a real minute to watch a verdict age; production keeps `Date.init`.
    @ObservationIgnored var now: () -> Date = Date.init

    /// The displayed verdict is older than `staleAfter`, or there is none.
    public var isStale: Bool {
        guard let lastRefresh else { return true }
        return now().timeIntervalSince(lastRefresh) > Self.staleAfter
    }

    /// How many callers currently have a panel open.
    ///
    /// A COUNT, not a Bool: the popover and the detail window can both be open, and
    /// closing one must not slow the poll while the other is still being watched.
    /// A Bool here meant closing the popover dropped the window back to the 15s
    /// cadence.
    private var openPanels = 0

    /// The cadence to use right now.
    public var currentInterval: Duration {
        openPanels > 0 ? Self.openPanelRefreshInterval : Self.refreshInterval
    }

    /// Called when a panel opens. Refreshes IMMEDIATELY rather than waiting for the
    /// next tick — a popover that opens showing a 14-second-old reading and then
    /// jumps is worse than one that opens current.
    public func panelOpened() async {
        openPanels += 1
        await refresh()
    }

    public func panelClosed() {
        openPanels = max(0, openPanels - 1)
    }

    private var client: APIClientProtocol?
    /// What `connect()` chose, for the log lines — the protocol hides the transport.
    private var transportDescription = "injected client"
    /// Guards against a second poll loop. Two loops double the request rate and
    /// interleave writes to `pressure`, and the second one is invisible.
    private var pollTask: Task<Void, Never>?

    /// The instance the app runs.
    ///
    /// A shared instance because the menu bar label, every window, and the
    /// `AppDelegate` that starts polling at launch must all see the SAME verdict —
    /// and `AppDelegate` cannot reach a `@State` value held by the `App` struct.
    /// Tests construct their own via `init(client:)`, so this does not make the type
    /// untestable; it just names the one the process uses.
    public static let shared = PressureModel()

    public init() {}

    /// Test seam: inject a client and skip process supervision. `@testable`
    /// callers construct the model already "connected" to a mock.
    init(client: APIClientProtocol, connectionState: ConnectionState = .connected) {
        self.client = client
        self.connectionState = connectionState
    }

    /// Connect, then poll forever. Idempotent: calling it twice does not start a
    /// second loop.
    ///
    /// **This runs at LAUNCH, not when a window opens.** The menu bar glyph is the
    /// product, and a glyph that only updated while a window was open would be a
    /// worse version of the thing this app replaced.
    public func start(notifier: NotificationCoordinator? = nil) {
        guard pollTask == nil else { return }
        self.notifier = notifier
        pollTask = Task { [weak self] in
            await self?.connect()
            await self?.poll()
        }
    }

    /// Connect to the DAEMON — never start one.
    ///
    /// The app is a client of a launchd LaunchAgent (ADR-0004). It used to spawn
    /// `banshee-api` as a child, and that must not come back: with the agent
    /// installed, spawning gives two writing processes on one database file, which
    /// is the single invariant ADR-0001 rests on. The second instance would also
    /// sample in parallel, invisibly.
    ///
    /// So a missing daemon is a first-class state with a repair action, not
    /// something to paper over by starting one.
    public func connect() async {
        // Environment override wins — dev workflows and e2e tests point the app at
        // a server they manage.
        let client = APIClient.fromEnvironment()
        self.client = client
        transportDescription = client.transport.description
        Self.logger.notice("connecting over \(self.transportDescription, privacy: .public)")

        if await DaemonService.shared.probe(client) {
            await refresh()
            return
        }
        connectionState = .failed(DaemonService.repairAdvice())
        lastError = nil
    }

    public func refresh() async {
        guard let client else { return }
        do {
            let verdict = try await client.pressure()
            pressure = verdict
            lastRefresh = now()
            if connectionState != .connected {
                Self.logger.notice("connected over \(self.transportDescription, privacy: .public)")
            }
            connectionState = .connected
            lastError = nil
            await notifier?.consider(verdict)
        } catch {
            // **The last good verdict is KEPT.** Blanking it on a transient
            // failure would replace a 15-second-old reading with nothing, which is
            // strictly less information; the staleness is visible via
            // `lastRefresh` and the error is surfaced separately.
            if case APIError.unauthorized(let detail) = error {
                connectionState = .unauthorized(detail)
                Self.logger.error("locked out: \(detail, privacy: .public)")
            } else {
                connectionState = .failed(error.localizedDescription)
                Self.logger.error("refresh failed: \(error.localizedDescription, privacy: .public)")
            }
            lastError = error.localizedDescription
        }
    }

    /// Poll until cancelled, at whatever cadence currently applies.
    ///
    /// The interval is re-read every iteration rather than captured once, so opening
    /// a panel speeds up the EXISTING loop instead of needing a second one. Two
    /// concurrent pollers would double the request rate and interleave writes to
    /// `pressure`.
    ///
    /// Cancellation is cooperative: `Task.sleep` throws on cancel, which ends the
    /// loop without leaving a request in flight.
    public func poll() async {
        while !Task.isCancelled {
            await refresh()
            do {
                try await Task.sleep(for: currentInterval)
            } catch {
                return
            }
        }
    }

    /// Hand each fresh verdict to the notification coordinator.
    ///
    /// Set by the app at launch. Nil in tests and when notifications are disabled,
    /// and `refresh()` simply does not call it — so the decision to notify lives in
    /// one place and "notifications off" is the absence of a collaborator rather
    /// than a flag checked in three.
    public var notifier: NotificationCoordinator?

    /// What the menu bar shows. The server's glyph VERBATIM, displaced only by states
    /// the server cannot have an opinion about because they are about THIS CLIENT:
    ///
    /// - 🫧 before the first fetch — "we have not asked", distinct from the server's
    ///   own `checking`, which means "the daemon has not measured enough";
    /// - 🔒 when the daemon has refused this app's key. Immediately, not after a
    ///   grace period: the daemon answered and said no, and the client has already
    ///   retried with a re-read key file before reporting it. Found running 0.1.3
    ///   against the Phase 3 daemon (`banshee-6ds`): every request refused for hours
    ///   while the menu bar kept saying "Wailing, memory" — its last accepted
    ///   verdict, presented as current;
    /// - 🫧 again when the daemon has been unreachable for longer than `staleAfter`.
    ///   A short outage keeps the last glyph (see `refresh`); a long one is the same
    ///   lie as the lock-out, told more slowly.
    ///
    /// The verdict itself is KEPT through both — the popover shows it as the last
    /// thing this app saw — but the glyph is the product, and a glyph that is hours
    /// old and looks current is the one thing the menu bar must never show.
    ///
    /// Mapping a level to a face here instead would be the local re-derivation
    /// ADR-0005 forbids: it is a Swift `switch` that has to stay in step with a
    /// Rust `match` by hand, and the whole point of the glyph is that it means the
    /// same thing everywhere. The two client-side glyphs are not level glyphs.
    public var menuBarGlyph: String {
        switch connectionState {
        case .unauthorized:
            return "🔒"
        case .failed where isStale:
            return "🫧"
        case .connecting, .connected, .failed:
            return pressure?.glyph ?? "🫧"
        }
    }

    /// Spoken by VoiceOver, from the same source and displaced by the same states.
    public var menuBarAccessibilityLabel: String {
        switch connectionState {
        case .unauthorized:
            return "Banshee: locked out of the daemon"
        case .failed where isStale:
            return "Banshee: daemon unreachable"
        case .connecting, .connected, .failed:
            return pressure?.accessibilityLabel ?? "Banshee: connecting"
        }
    }

    /// The word beside the glyph in the popover header: the server's level name, or
    /// the client state that has displaced it. Kept in step with `menuBarGlyph` here
    /// so a view cannot pair the lock with a level name.
    public var menuBarTitle: String {
        switch connectionState {
        case .unauthorized:
            return "Locked out"
        case .failed where isStale:
            return "Unreachable"
        case .connecting, .connected, .failed:
            return pressure?.levelName ?? "Checking"
        }
    }

    /// Why the glyph is not the verdict's, for the popover — nil whenever the glyph IS
    /// the verdict's. The lock-out text names the repair (the client composed it); the
    /// unreachable text is the failure as reported.
    public var connectionNotice: String? {
        switch connectionState {
        case .unauthorized(let detail):
            return detail
        case .failed(let message) where isStale:
            return message
        case .connecting, .connected, .failed:
            return nil
        }
    }
}
