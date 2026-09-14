// The wire-format ACTION types — the reap reports the daemon returns from
// the /actions/* routes. Mirrors `rust/banshee-core/src/actions.rs`; field names
// map 1:1 to the camelCase JSON keys.
//
// These are the app's first WRITE surface. Two things travel with them and both
// are load-bearing:
//
// - **A preview and its execute share one candidate set server-side**, so the
//   list a person approves in the sheet is the list that gets killed. The client
//   never sends a target list — execute takes no body (docs/wire-format.md).
// - **`outcome` is null on a preview** and a verb ("killed"/"terminated"/…) after
//   execute; **`idleDays` null is "unknown"**, not "idle forever"; a spared
//   candidate has a null `outcome` even after execute. All present-as-null.

import Foundation

/// What a stale-session candidate's fate is.
public enum SessionVerdict: String, Codable, Sendable {
    /// Execute would kill this session.
    case reap
    /// A safety rail matched; execute leaves it alone. `reason` says which.
    case spare
}

/// One stale-or-undatable tmux session, with the verdict and why.
public struct SessionCandidate: Codable, Identifiable, Equatable, Sendable {
    /// tmux session names are unique within a report.
    public var id: String { session }

    public let session: String
    /// The pane command as tmux reported it (capitalization intact). When a busy
    /// pane spared the session, this is THAT pane's command.
    public let paneCommand: String
    /// Days idle; nil when tmux had no activity timestamp — which is NOT "idle
    /// forever" and never makes a session reapable.
    public let idleDays: Double?
    /// The pane's working directory; the DIRTY one when that rail fired.
    public let cwd: String?
    public let verdict: SessionVerdict
    /// Human wording for the verdict, composed in core and rendered verbatim.
    public let reason: String
    /// Nil on a preview. After execute: "killed", "survived", or "failed: …".
    public let outcome: String?

    public init(
        session: String, paneCommand: String, idleDays: Double?, cwd: String?,
        verdict: SessionVerdict, reason: String, outcome: String?
    ) {
        self.session = session
        self.paneCommand = paneCommand
        self.idleDays = idleDays
        self.cwd = cwd
        self.verdict = verdict
        self.reason = reason
        self.outcome = outcome
    }

    enum CodingKeys: String, CodingKey {
        case session, paneCommand, idleDays, cwd, verdict, reason, outcome
    }

    // Hand-written: the synthesized encoder omits nil keys, violating
    // present-as-null. Hard-coded field list, pinned by
    // `testEncodeCoversEverySessionCandidateField`.
    public func encode(to encoder: Encoder) throws {
        var c = encoder.container(keyedBy: CodingKeys.self)
        try c.encode(session, forKey: .session)
        try c.encode(paneCommand, forKey: .paneCommand)
        if let idleDays {
            try c.encode(idleDays, forKey: .idleDays)
        } else {
            try c.encodeNil(forKey: .idleDays)
        }
        if let cwd {
            try c.encode(cwd, forKey: .cwd)
        } else {
            try c.encodeNil(forKey: .cwd)
        }
        try c.encode(verdict, forKey: .verdict)
        try c.encode(reason, forKey: .reason)
        if let outcome {
            try c.encode(outcome, forKey: .outcome)
        } else {
            try c.encodeNil(forKey: .outcome)
        }
    }
}

/// The result of a reap-stale-sessions preview or execute.
public struct SessionReapReport: Codable, Equatable, Sendable {
    /// "reapStaleSessions" — matches the `Action` vocabulary.
    public let action: String
    public let executed: Bool
    /// False means tmux could not be reached AT ALL — "could not look", never to
    /// be rendered as "nothing to reap" (the census's own rule).
    public let tmuxAvailable: Bool
    /// The staleness threshold the selection used, so the UI can say "idle ≥ N days".
    public let staleDays: UInt64
    public let candidates: [SessionCandidate]

    public init(
        action: String, executed: Bool, tmuxAvailable: Bool,
        staleDays: UInt64, candidates: [SessionCandidate]
    ) {
        self.action = action
        self.executed = executed
        self.tmuxAvailable = tmuxAvailable
        self.staleDays = staleDays
        self.candidates = candidates
    }

    /// The candidates execute would (or did) act on. Presentation, not a
    /// re-derivation — the verdicts came off the wire.
    public var toReap: [SessionCandidate] {
        candidates.filter { $0.verdict == .reap }
    }
}

/// One orphaned helper process (matches an orphan pattern, ppid 1).
public struct OrphanCandidate: Codable, Identifiable, Equatable, Sendable {
    /// A report lists each pid once.
    public var id: Int { pid }

    public let pid: Int
    public let program: String
    /// Full command line, so a person can tell WHICH mcp-server this was.
    public let args: String
    public let rssBytes: UInt64
    public let ageSecs: UInt64
    /// Nil on a preview. After execute: "terminated" (died on SIGTERM) or
    /// "killed" (survived the grace period and was SIGKILLed).
    public let outcome: String?

    public init(
        pid: Int, program: String, args: String, rssBytes: UInt64,
        ageSecs: UInt64, outcome: String?
    ) {
        self.pid = pid
        self.program = program
        self.args = args
        self.rssBytes = rssBytes
        self.ageSecs = ageSecs
        self.outcome = outcome
    }

    enum CodingKeys: String, CodingKey {
        case pid, program, args, rssBytes, ageSecs, outcome
    }

    // Hand-written for present-as-null on `outcome`; hard-coded field list,
    // pinned by `testEncodeCoversEveryOrphanCandidateField`.
    public func encode(to encoder: Encoder) throws {
        var c = encoder.container(keyedBy: CodingKeys.self)
        try c.encode(pid, forKey: .pid)
        try c.encode(program, forKey: .program)
        try c.encode(args, forKey: .args)
        try c.encode(rssBytes, forKey: .rssBytes)
        try c.encode(ageSecs, forKey: .ageSecs)
        if let outcome {
            try c.encode(outcome, forKey: .outcome)
        } else {
            try c.encodeNil(forKey: .outcome)
        }
    }
}

/// The result of a reap-orphans preview or execute.
public struct OrphanReapReport: Codable, Equatable, Sendable {
    /// "reapOrphans" — matches the `Action` vocabulary.
    public let action: String
    public let executed: Bool
    /// Seconds SIGTERM gets before survivors are SIGKILLed.
    public let termWaitSecs: UInt64
    public let candidates: [OrphanCandidate]

    public init(
        action: String, executed: Bool, termWaitSecs: UInt64,
        candidates: [OrphanCandidate]
    ) {
        self.action = action
        self.executed = executed
        self.termWaitSecs = termWaitSecs
        self.candidates = candidates
    }
}
