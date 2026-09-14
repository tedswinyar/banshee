// The reap action's view model — one lifecycle: load a PREVIEW, let the person
// look, then (only on their explicit press) EXECUTE. Lives in BansheeCore so it
// is unit-testable at the network boundary with MockAPIClient; the sheet that
// drives it holds no logic beyond rendering.
//
// The safety posture is the whole design: a reap is never one press away. The
// sheet opens on the preview (kills nothing), and execute is a second, deliberate
// action against a set the person has just seen. That mirrors the CLI's
// dry-run-unless-`--execute` and the server's preview/execute split — three
// surfaces, one rule.

import Foundation
import Observation

/// Which reap this model drives. `Identifiable` so a view can present it as a
/// `.sheet(item:)` directly.
public enum ReapKind: String, Identifiable, Sendable {
    case staleSessions
    case orphans

    public var id: String { rawValue }

    /// The `Action` this corresponds to, so a `Finding` can pick the right kind.
    public var action: Action {
        switch self {
        case .staleSessions: return .reapStaleSessions
        case .orphans: return .reapOrphans
        }
    }

    public init?(action: Action) {
        switch action {
        case .reapStaleSessions: self = .staleSessions
        case .reapOrphans: self = .orphans
        default: return nil
        }
    }
}

/// A preview or execute result, either shape. The sheet switches on this to
/// render the matching rows.
public enum ReapReport: Equatable, Sendable {
    case sessions(SessionReapReport)
    case orphans(OrphanReapReport)
}

@MainActor
@Observable
public final class ReapActionModel {
    /// Where we are in the load → preview → execute → done flow.
    public enum Phase: Equatable, Sendable {
        case loadingPreview
        case preview
        case executing
        case executed
        case failed(String)
    }

    public let kind: ReapKind
    public private(set) var phase: Phase = .loadingPreview
    /// The current report: the preview while previewing, replaced by the execute
    /// result once executed. Nil only before the first load or after a failure.
    public private(set) var report: ReapReport?

    private let client: APIClientProtocol

    public init(kind: ReapKind, client: APIClientProtocol) {
        self.kind = kind
        self.client = client
    }

    /// Convenience for the app: resolve the client from the environment the same
    /// way every other model does.
    public convenience init(kind: ReapKind) {
        self.init(kind: kind, client: APIClient.fromEnvironment())
    }

    /// Load the dry-run. Kills nothing. Safe to call again to retry after a
    /// failure — it returns to `loadingPreview` first so a stale error clears.
    public func loadPreview() async {
        phase = .loadingPreview
        do {
            report = try await fetchPreview()
            phase = .preview
        } catch {
            phase = .failed(error.localizedDescription)
        }
    }

    /// Execute. Only meaningful from `.preview`; the sheet only offers the button
    /// there, but the guard keeps a double-press or an out-of-order call from
    /// firing a second kill.
    public func execute() async {
        guard phase == .preview else { return }
        phase = .executing
        do {
            report = try await fetchExecute()
            phase = .executed
        } catch {
            phase = .failed(error.localizedDescription)
        }
    }

    /// Whether execute has anything to do. A sheet with zero reap candidates
    /// shows the preview but disables the button — there is nothing to confirm.
    public var hasReapableCandidates: Bool {
        switch report {
        case .sessions(let r): return !r.toReap.isEmpty
        case .orphans(let r): return !r.candidates.isEmpty
        case nil: return false
        }
    }

    private func fetchPreview() async throws -> ReapReport {
        switch kind {
        case .staleSessions: return .sessions(try await client.reapStaleSessionsPreview())
        case .orphans: return .orphans(try await client.reapOrphansPreview())
        }
    }

    private func fetchExecute() async throws -> ReapReport {
        switch kind {
        case .staleSessions: return .sessions(try await client.reapStaleSessionsExecute())
        case .orphans: return .orphans(try await client.reapOrphansExecute())
        }
    }
}
