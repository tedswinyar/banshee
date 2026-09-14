// ReapActionModel — the preview → execute lifecycle, driven through the mocked
// network boundary. The safety property under test is that EXECUTE NEVER RUNS
// ON ITS OWN: it fires only from `.preview` and only when called, and it swaps
// the preview report for the execute result rather than re-showing stale rows.

import XCTest
@testable import BansheeCore

@MainActor
final class ReapActionModelTests: XCTestCase {
    private func sessionReport(
        executed: Bool, reapOutcome: String?
    ) -> SessionReapReport {
        SessionReapReport(
            action: "reapStaleSessions", executed: executed, tmuxAvailable: true,
            staleDays: 2,
            candidates: [
                SessionCandidate(
                    session: "old", paneCommand: "zsh", idleDays: 4.0, cwd: "/x",
                    verdict: .reap, reason: "idle, clean",
                    outcome: reapOutcome
                ),
                SessionCandidate(
                    session: "busy", paneCommand: "Python", idleDays: 4.0, cwd: "/y",
                    verdict: .spare, reason: "busy pane (Python)", outcome: nil
                ),
            ]
        )
    }

    /// The happy path: open on a preview (kills nothing), then execute swaps in
    /// the result. Call counters prove exactly one of each ran.
    func testPreviewThenExecuteSwapsTheReport() async {
        let mock = MockAPIClient()
        mock.sessionPreviewResult = sessionReport(executed: false, reapOutcome: nil)
        mock.sessionExecuteResult = sessionReport(executed: true, reapOutcome: "killed")
        let model = ReapActionModel(kind: .staleSessions, client: mock)

        await model.loadPreview()
        XCTAssertEqual(model.phase, .preview)
        XCTAssertEqual(mock.sessionPreviewCalls, 1)
        XCTAssertEqual(mock.sessionExecuteCalls, 0, "preview must not execute")
        if case .sessions(let r) = model.report {
            XCTAssertFalse(r.executed)
            XCTAssertNil(r.candidates[0].outcome, "preview outcomes are null")
        } else {
            XCTFail("expected a session report")
        }

        await model.execute()
        XCTAssertEqual(model.phase, .executed)
        XCTAssertEqual(mock.sessionExecuteCalls, 1)
        if case .sessions(let r) = model.report {
            XCTAssertTrue(r.executed)
            XCTAssertEqual(r.candidates[0].outcome, "killed", "execute result now shown")
        } else {
            XCTFail("expected a session report")
        }
    }

    /// EXECUTE CANNOT RUN before a preview, and cannot double-run. This is the
    /// guard that keeps a stray call or a double-press from firing a second kill.
    /// Mutation-proof: drop the `guard phase == .preview` in `execute()` and the
    /// first assertion (execute from `.loadingPreview`) sends a call.
    func testExecuteOnlyFiresFromPreview() async {
        let mock = MockAPIClient()
        mock.sessionPreviewResult = sessionReport(executed: false, reapOutcome: nil)
        mock.sessionExecuteResult = sessionReport(executed: true, reapOutcome: "killed")
        let model = ReapActionModel(kind: .staleSessions, client: mock)

        // Before any preview: execute is a no-op.
        await model.execute()
        XCTAssertEqual(mock.sessionExecuteCalls, 0, "execute before preview must not fire")

        await model.loadPreview()
        await model.execute()
        XCTAssertEqual(mock.sessionExecuteCalls, 1)

        // A second execute from `.executed` must not fire again.
        await model.execute()
        XCTAssertEqual(mock.sessionExecuteCalls, 1, "execute must not double-fire")
    }

    /// A failed preview surfaces the error and offers a retry — and never leaves
    /// a report that execute could act on.
    func testAFailedPreviewIsRecoverable() async {
        let mock = MockAPIClient()
        mock.failWith = .serverUnreachable("down")
        let model = ReapActionModel(kind: .orphans, client: mock)

        await model.loadPreview()
        guard case .failed = model.phase else {
            return XCTFail("expected failed phase, got \(model.phase)")
        }
        XCTAssertFalse(model.hasReapableCandidates)

        // Execute must not fire from a failed state.
        await model.execute()
        XCTAssertEqual(mock.orphanExecuteCalls, 0)

        // Recover: clear the fault and retry.
        mock.failWith = nil
        mock.orphanPreviewResult = OrphanReapReport(
            action: "reapOrphans", executed: false, termWaitSecs: 8,
            candidates: [OrphanCandidate(
                pid: 1, program: "mcp", args: "/x/mcp", rssBytes: 1, ageSecs: 1, outcome: nil
            )]
        )
        await model.loadPreview()
        XCTAssertEqual(model.phase, .preview)
        XCTAssertTrue(model.hasReapableCandidates)
    }

    /// A preview with nothing to reap disables the button: `hasReapableCandidates`
    /// is false when every session is spared, even though the list is non-empty.
    func testNothingToReapDisablesExecute() async {
        let mock = MockAPIClient()
        mock.sessionPreviewResult = SessionReapReport(
            action: "reapStaleSessions", executed: false, tmuxAvailable: true,
            staleDays: 2,
            candidates: [SessionCandidate(
                session: "busy", paneCommand: "cargo", idleDays: 9.0, cwd: "/x",
                verdict: .spare, reason: "busy pane (cargo)", outcome: nil
            )]
        )
        let model = ReapActionModel(kind: .staleSessions, client: mock)
        await model.loadPreview()
        XCTAssertEqual(model.phase, .preview)
        XCTAssertFalse(
            model.hasReapableCandidates,
            "a list of only spared sessions has nothing to reap"
        )
    }

    /// ReapKind ↔ Action round-trips, and non-reap actions produce no kind (so a
    /// relaunch/reboot/openDiskTool finding never renders a reap button).
    func testReapKindMapsOnlyReapActions() {
        XCTAssertEqual(ReapKind(action: .reapStaleSessions), .staleSessions)
        XCTAssertEqual(ReapKind(action: .reapOrphans), .orphans)
        XCTAssertNil(ReapKind(action: .relaunchApps))
        XCTAssertNil(ReapKind(action: .reboot))
        XCTAssertNil(ReapKind(action: .openDiskTool))
        XCTAssertNil(ReapKind(action: .none))
        XCTAssertEqual(ReapKind.staleSessions.action, .reapStaleSessions)
    }
}
