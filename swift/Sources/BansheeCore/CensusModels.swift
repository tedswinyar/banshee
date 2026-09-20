// The wire-format census types — the 5-minute tier: who is running, how long,
// and what it costs. Mirrors `rust/banshee-core/src/census/mod.rs`; field names
// map 1:1 to the camelCase JSON keys.
//
// This is the "what should I close?" payload, and the thing `perf-scan` was
// actually used for. Two rules travel with it:
//
// - **`tmuxAvailable: false` is not "zero sessions"** — it means the census
//   could not look, and a UI that conflates them reports a clean machine.
// - **Never sum the per-agent `percentOfOneCore` values.** Each divides by the
//   longest-lived process in its OWN group, so the denominators differ (a real
//   census summed to 59.9% against an honest 49.2%). `monitorTotal` is the
//   deduplicated figure against one global denominator — use it.

import Foundation

/// An interactive agent CLI session — one a person is sitting in.
public struct AgentSession: Codable, Identifiable, Equatable, Sendable {
    /// A census lists each pid once, so the pid is the identity within one census.
    public var id: Int { pid }

    public let pid: Int
    public let program: String
    public let rssBytes: UInt64
    public let ageSecs: UInt64
    public let ageDays: UInt64
    public let cpuSecs: Double
    public let tty: String
    public let isStale: Bool
    /// Present only for STALE sessions: `lsof` is not run for the rest. This is
    /// the field that answers "which project is that 5-day-old session sitting in".
    public let cwd: String?

    public init(
        pid: Int, program: String, rssBytes: UInt64, ageSecs: UInt64,
        ageDays: UInt64, cpuSecs: Double, tty: String, isStale: Bool,
        cwd: String?
    ) {
        self.pid = pid
        self.program = program
        self.rssBytes = rssBytes
        self.ageSecs = ageSecs
        self.ageDays = ageDays
        self.cpuSecs = cpuSecs
        self.tty = tty
        self.isStale = isStale
        self.cwd = cwd
    }

    enum CodingKeys: String, CodingKey {
        case pid, program, rssBytes, ageSecs, ageDays, cpuSecs, tty, isStale, cwd
    }

    // Hand-written: the synthesized encoder omits nil keys, violating
    // present-as-null. Hard-coded field list, pinned by
    // `testEncodeCoversEveryAgentSessionField`.
    public func encode(to encoder: Encoder) throws {
        var c = encoder.container(keyedBy: CodingKeys.self)
        try c.encode(pid, forKey: .pid)
        try c.encode(program, forKey: .program)
        try c.encode(rssBytes, forKey: .rssBytes)
        try c.encode(ageSecs, forKey: .ageSecs)
        try c.encode(ageDays, forKey: .ageDays)
        try c.encode(cpuSecs, forKey: .cpuSecs)
        try c.encode(tty, forKey: .tty)
        try c.encode(isStale, forKey: .isStale)
        if let cwd {
            try c.encode(cwd, forKey: .cwd)
        } else {
            try c.encodeNil(forKey: .cwd)
        }
    }
}

/// IDE-spawned agent helpers, rolled up rather than listed — one measurement
/// found 48 helpers burying the seven real sessions.
public struct HelperRollup: Codable, Equatable, Sendable {
    public let count: Int
    public let rssBytes: UInt64

    public init(count: Int, rssBytes: UInt64) {
        self.count = count
        self.rssBytes = rssBytes
    }
}

/// Agent helper processes and how many have been orphaned by a dead parent.
public struct OrphanCensus: Codable, Equatable, Sendable {
    public let totalCount: Int
    public let totalRssBytes: UInt64
    /// `ppid == 1`: the session that spawned them died and left them running.
    public let orphanCount: Int
    public let orphanRssBytes: UInt64
    /// Orphans grouped by program, descending — the actionable breakdown.
    public let orphansByProgram: [ProgramCount]

    public init(
        totalCount: Int, totalRssBytes: UInt64, orphanCount: Int,
        orphanRssBytes: UInt64, orphansByProgram: [ProgramCount]
    ) {
        self.totalCount = totalCount
        self.totalRssBytes = totalRssBytes
        self.orphanCount = orphanCount
        self.orphanRssBytes = orphanRssBytes
        self.orphansByProgram = orphansByProgram
    }
}

public struct ProgramCount: Codable, Identifiable, Equatable, Sendable {
    /// Grouped BY program, so the program is the identity.
    public var id: String { program }

    public let program: String
    public let count: Int
    public let rssBytes: UInt64

    public init(program: String, count: Int, rssBytes: UInt64) {
        self.program = program
        self.count = count
        self.rssBytes = rssBytes
    }
}

public struct TmuxSessionInfo: Codable, Identifiable, Equatable, Sendable {
    /// tmux session names are unique within a server.
    public var id: String { name }

    public let name: String
    public let paneCommand: String
    /// Nil when the activity timestamp was unknown — which is NOT the same as
    /// "idle forever", and must never make a session look reapable.
    public let idleDays: Double?
    /// Mid-build or mid-test: never reap, whatever its age.
    public let isBusy: Bool
    /// Old enough to be a reap candidate. Requires a known idle age.
    public let isStale: Bool

    public init(name: String, paneCommand: String, idleDays: Double?, isBusy: Bool, isStale: Bool) {
        self.name = name
        self.paneCommand = paneCommand
        self.idleDays = idleDays
        self.isBusy = isBusy
        self.isStale = isStale
    }

    enum CodingKeys: String, CodingKey {
        case name, paneCommand, idleDays, isBusy, isStale
    }

    // Hand-written for present-as-null on `idleDays`; hard-coded field list,
    // pinned by `testEncodeCoversEveryTmuxSessionField`.
    public func encode(to encoder: Encoder) throws {
        var c = encoder.container(keyedBy: CodingKeys.self)
        try c.encode(name, forKey: .name)
        try c.encode(paneCommand, forKey: .paneCommand)
        if let idleDays {
            try c.encode(idleDays, forKey: .idleDays)
        } else {
            try c.encodeNil(forKey: .idleDays)
        }
        try c.encode(isBusy, forKey: .isBusy)
        try c.encode(isStale, forKey: .isStale)
    }
}

public struct AppGroup: Codable, Identifiable, Equatable, Sendable {
    public var id: String { name }

    public let name: String
    public let procCount: Int
    public let rssBytes: UInt64

    public init(name: String, procCount: Int, rssBytes: UInt64) {
        self.name = name
        self.procCount = procCount
        self.rssBytes = rssBytes
    }
}

/// A managed monitoring/security agent's real cost. `percentOfOneCore` is
/// `Σ CPU ÷ max(lifetime)` — noisy for short-lived processes, which is why
/// `longestLifeSecs` is exposed: a consumer can refuse to judge a figure
/// derived from a short life.
public struct MonitorAgent: Codable, Identifiable, Equatable, Sendable {
    public var id: String { name }

    public let name: String
    public let procCount: Int
    public let rssBytes: UInt64
    public let cpuSecsTotal: Double
    public let percentOfOneCore: Double
    public let longestLifeSecs: UInt64

    public init(
        name: String, procCount: Int, rssBytes: UInt64,
        cpuSecsTotal: Double, percentOfOneCore: Double, longestLifeSecs: UInt64
    ) {
        self.name = name
        self.procCount = procCount
        self.rssBytes = rssBytes
        self.cpuSecsTotal = cpuSecsTotal
        self.percentOfOneCore = percentOfOneCore
        self.longestLifeSecs = longestLifeSecs
    }
}

/// The whole managed-agent population, counted ONCE per process against one
/// global denominator. The honest answer to "what are the managed agents costing
/// me", and the number to show a user.
public struct MonitorTotal: Codable, Equatable, Sendable {
    public let procCount: Int
    public let rssBytes: UInt64
    public let cpuSecsTotal: Double
    /// Against ONE core. Divide by the core count for a share of the machine.
    public let percentOfOneCore: Double
    public let longestLifeSecs: UInt64

    public init(
        procCount: Int, rssBytes: UInt64, cpuSecsTotal: Double,
        percentOfOneCore: Double, longestLifeSecs: UInt64
    ) {
        self.procCount = procCount
        self.rssBytes = rssBytes
        self.cpuSecsTotal = cpuSecsTotal
        self.percentOfOneCore = percentOfOneCore
        self.longestLifeSecs = longestLifeSecs
    }
}

/// One consumer behind the CPU/thermal who-line, ranked by CPU burned SINCE the
/// previous census — a RATE over the last interval, not a lifetime total
/// (`banshee-aen`). Unlike `monitorAgents`, this covers ANY process the census
/// saw, not just stored agent sessions, so a desktop app or a build tool can be
/// named. All fields required; nothing here is present-as-null, so the synthesized
/// `Codable` is correct.
public struct CpuConsumer: Codable, Identifiable, Equatable, Sendable {
    public var id: String { name }

    /// The census's label: an app-group name if the process matched one, else its
    /// program basename. ONE label per process, so rates never double-count.
    public let name: String
    public let procCount: Int
    /// Against ONE core, over the last census interval. Can exceed 100 for a
    /// multi-process group on a multi-core machine.
    public let percentOfOneCore: Double

    public init(name: String, procCount: Int, percentOfOneCore: Double) {
        self.name = name
        self.procCount = procCount
        self.percentOfOneCore = percentOfOneCore
    }
}

/// One census cycle.
public struct Census: Codable, Identifiable, Equatable, Sendable {
    public let id: UUID
    public let takenAt: Date
    public let totalProcs: Int
    public let agentSessions: [AgentSession]
    public let ideHelpers: HelperRollup
    public let orphans: OrphanCensus
    public let tmuxSessions: [TmuxSessionInfo]
    public let appGroups: [AppGroup]
    public let monitorAgents: [MonitorAgent]
    /// Deduplicated total across all monitor patterns. **Use this, not the sum
    /// of `monitorAgents`.**
    public let monitorTotal: MonitorTotal
    /// True when `tmux` could be reached at all. False is "could not look",
    /// which is a different sentence from "zero sessions".
    public let tmuxAvailable: Bool
    /// The CPU/thermal who-line's recent-rate consumers, ranked. Empty on the
    /// first census after a start (no predecessor to difference).
    public let cpuConsumers: [CpuConsumer]

    public init(
        id: UUID, takenAt: Date, totalProcs: Int,
        agentSessions: [AgentSession], ideHelpers: HelperRollup,
        orphans: OrphanCensus, tmuxSessions: [TmuxSessionInfo],
        appGroups: [AppGroup], monitorAgents: [MonitorAgent],
        monitorTotal: MonitorTotal, tmuxAvailable: Bool,
        cpuConsumers: [CpuConsumer]
    ) {
        self.id = id
        self.takenAt = takenAt
        self.totalProcs = totalProcs
        self.agentSessions = agentSessions
        self.ideHelpers = ideHelpers
        self.orphans = orphans
        self.tmuxSessions = tmuxSessions
        self.appGroups = appGroups
        self.monitorAgents = monitorAgents
        self.monitorTotal = monitorTotal
        self.tmuxAvailable = tmuxAvailable
        self.cpuConsumers = cpuConsumers
    }

    /// Sessions worth reaping — presentation, not a re-derivation: the flags
    /// came off the wire (core decided staleness).
    public var staleSessions: [AgentSession] {
        agentSessions.filter(\.isStale)
    }

    /// tmux sessions safe to reap: stale, and not in a busy pane. Mirrors
    /// `Census::reapable_tmux_sessions` in core.
    public var reapableTmuxSessions: [TmuxSessionInfo] {
        tmuxSessions.filter { $0.isStale && !$0.isBusy }
    }

    enum CodingKeys: String, CodingKey {
        case id, takenAt, totalProcs, agentSessions, ideHelpers, orphans
        case tmuxSessions, appGroups, monitorAgents, monitorTotal, tmuxAvailable
        case cpuConsumers
    }

    // Hand-written for the UUID's lowercase-out rule; hard-coded field list,
    // pinned by `testEncodeCoversEveryCensusField`.
    public func encode(to encoder: Encoder) throws {
        var c = encoder.container(keyedBy: CodingKeys.self)
        try c.encode(id.uuidString.lowercased(), forKey: .id)
        try c.encode(takenAt, forKey: .takenAt)
        try c.encode(totalProcs, forKey: .totalProcs)
        try c.encode(agentSessions, forKey: .agentSessions)
        try c.encode(ideHelpers, forKey: .ideHelpers)
        try c.encode(orphans, forKey: .orphans)
        try c.encode(tmuxSessions, forKey: .tmuxSessions)
        try c.encode(appGroups, forKey: .appGroups)
        try c.encode(monitorAgents, forKey: .monitorAgents)
        try c.encode(monitorTotal, forKey: .monitorTotal)
        try c.encode(tmuxAvailable, forKey: .tmuxAvailable)
        try c.encode(cpuConsumers, forKey: .cpuConsumers)
    }
}
