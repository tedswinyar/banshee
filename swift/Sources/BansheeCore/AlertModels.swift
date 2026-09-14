// The wire-format alert-episode and stats types. Mirrors
// `rust/banshee-core/src/pressure/episode.rs` and the `/stats` body; field names
// map 1:1 to the camelCase JSON keys.
//
// Alerts are EPISODES, not point events (ADR-0009): one continuous incident on one
// dimension, with a start, an end (null while open), the worst moment, how many
// re-fires it absorbed, and where it is in its lifecycle. They are the LONG
// memory: persisted with a one-year retention (ADR-0006), so the interesting
// moments survive at full fidelity long after the samples behind a bad afternoon
// have been rolled up or dropped.

import Foundation

/// Where an episode is in its lifecycle. Carried on the wire rather than derived
/// here from `endedAt`/`recoveringSince` — two clients deriving it independently
/// would eventually disagree (ADR-0005).
public enum EpisodeState: String, Codable, Sendable {
    /// The dimension is red and the episode is live.
    case firing
    /// Below red, but the incident is not over until it stays there for the down
    /// window (Prometheus `keep_firing_for`).
    case recovering
    /// Over. `endedAt` is set; exactly one recovery notice went out.
    case closed
}

/// The worst moment of an episode.
public struct EpisodePeak: Codable, Equatable, Sendable {
    public let band: Band
    /// The whole-machine level at the peak, which is what routed delivery.
    public let level: Level
    public let severity: Double
    public let at: Date
    /// The finding's wording at that moment, rendered verbatim.
    public let message: String
    /// The finding's who-line at that moment — who was behind it at the worst
    /// point, kept on the record after the census that knew has been swept. Nil
    /// when the finding named nobody, and for episodes from before the line.
    public let whoLine: String?

    public init(
        band: Band, level: Level, severity: Double, at: Date, message: String,
        whoLine: String? = nil
    ) {
        self.band = band
        self.level = level
        self.severity = severity
        self.at = at
        self.message = message
        self.whoLine = whoLine
    }

    enum CodingKeys: String, CodingKey {
        case band, level, severity, at, message, whoLine
    }

    // Hand-written: a daemon from before the who-line (or a stored older peak) sends no
    // `whoLine`, and that decodes as nil rather than failing.
    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        band = try c.decode(Band.self, forKey: .band)
        level = try c.decode(Level.self, forKey: .level)
        severity = try c.decode(Double.self, forKey: .severity)
        at = try c.decode(Date.self, forKey: .at)
        message = try c.decode(String.self, forKey: .message)
        whoLine = try c.decodeIfPresent(String.self, forKey: .whoLine)
    }

    // Present-as-null binds the encoder; the synthesized one drops a nil key.
    public func encode(to encoder: Encoder) throws {
        var c = encoder.container(keyedBy: CodingKeys.self)
        try c.encode(band, forKey: .band)
        try c.encode(level, forKey: .level)
        try c.encode(severity, forKey: .severity)
        try c.encode(at, forKey: .at)
        try c.encode(message, forKey: .message)
        if let whoLine {
            try c.encode(whoLine, forKey: .whoLine)
        } else {
            try c.encodeNil(forKey: .whoLine)
        }
    }
}

/// One continuous incident on one dimension.
public struct AlertEpisode: Codable, Identifiable, Equatable, Sendable {
    public let id: UUID
    /// Which dimension. A string, like `DimensionReading.dimension`, so a
    /// dimension added to the wire cannot make old episodes undecodable.
    public let dimension: String
    /// When the red run began — not when the up delay was crossed.
    public let startedAt: Date
    /// Nullable on the wire: null means STILL OPEN, which is the most important
    /// fact on the payload.
    public let endedAt: Date?
    public let peak: EpisodePeak
    /// Re-fires absorbed inside the repeat interval — each one a notification the
    /// point-event model would have rate-limited away. `39` is forty storms.
    public let suppressed: Int
    public let state: EpisodeState
    /// When a notice last actually went out; null if every notice was inhibited.
    public let lastNotifiedAt: Date?
    /// When the dimension dropped below red, while recovering. Null while firing.
    public let recoveringSince: Date?
    /// The census summarised AT THE PEAK of this episode: the who-line's
    /// inputs frozen at the worst moment, so a delta can answer "what changed
    /// since it started" after the raw censuses behind the incident have been
    /// swept. Nullable on the wire; null for episodes from before deltas existed.
    public let censusAtPeak: CensusSummary?

    public init(
        id: UUID, dimension: String, startedAt: Date, endedAt: Date?,
        peak: EpisodePeak, suppressed: Int, state: EpisodeState,
        lastNotifiedAt: Date?, recoveringSince: Date?,
        censusAtPeak: CensusSummary? = nil
    ) {
        self.id = id
        self.dimension = dimension
        self.startedAt = startedAt
        self.endedAt = endedAt
        self.peak = peak
        self.suppressed = suppressed
        self.state = state
        self.lastNotifiedAt = lastNotifiedAt
        self.recoveringSince = recoveringSince
        self.censusAtPeak = censusAtPeak
    }

    public var isOpen: Bool { endedAt == nil }

    enum CodingKeys: String, CodingKey {
        case id, dimension, startedAt, endedAt, peak, suppressed, state
        case lastNotifiedAt, recoveringSince, censusAtPeak
    }

    // Hand-written for the UUID's lowercase-out rule and the present-as-null
    // nullables (the synthesized encoder drops nil keys); hard-coded field list,
    // pinned by `testEncodeCoversEveryAlertEpisodeField`.
    public func encode(to encoder: Encoder) throws {
        var c = encoder.container(keyedBy: CodingKeys.self)
        try c.encode(id.uuidString.lowercased(), forKey: .id)
        try c.encode(dimension, forKey: .dimension)
        try c.encode(startedAt, forKey: .startedAt)
        if let endedAt {
            try c.encode(endedAt, forKey: .endedAt)
        } else {
            try c.encodeNil(forKey: .endedAt)
        }
        try c.encode(peak, forKey: .peak)
        try c.encode(suppressed, forKey: .suppressed)
        try c.encode(state, forKey: .state)
        if let lastNotifiedAt {
            try c.encode(lastNotifiedAt, forKey: .lastNotifiedAt)
        } else {
            try c.encodeNil(forKey: .lastNotifiedAt)
        }
        if let recoveringSince {
            try c.encode(recoveringSince, forKey: .recoveringSince)
        } else {
            try c.encodeNil(forKey: .recoveringSince)
        }
        if let censusAtPeak {
            try c.encode(censusAtPeak, forKey: .censusAtPeak)
        } else {
            try c.encodeNil(forKey: .censusAtPeak)
        }
    }
}

/// What the daemon is storing and what it costs — `GET /stats`.
///
/// `dbSizeBytes` is the file's HIGH-WATER MARK, not the current row count:
/// SQLite marks freed pages reusable rather than returning them to the OS, so
/// the file does not shrink after a retention sweep.
public struct Stats: Codable, Equatable, Sendable {
    public let schemaVersion: Int
    public let samples: Int
    public let rollups: Int
    public let censuses: Int
    public let alerts: Int
    public let dbSizeBytes: UInt64
    /// Sampler operational health (banshee-4r1). `lastEvaluatedAt` lets a client
    /// tell a live verdict from one frozen by a stuck evaluation; a non-zero
    /// `consecutiveSweepFailures` means the DB may be growing unswept. Nullable
    /// timestamps are present-as-null before anything has run.
    public let lastEvaluatedAt: Date?
    public let consecutiveEvalFailures: UInt64
    public let lastSweptAt: Date?
    public let consecutiveSweepFailures: UInt64
    /// What the daemon ITSELF costs: physical footprint (Activity
    /// Monitor's "Memory" figure, not RSS) and CPU as a lifetime average over its
    /// own uptime — the sustained number a burst `%CPU` cannot give. The percent
    /// is computed in core and served, never re-derived here (ADR-0005).
    public let selfFootprintBytes: UInt64
    public let selfCpuSecs: Double
    public let selfUptimeSecs: Double
    public let selfCpuPercent: Double

    public init(
        schemaVersion: Int, samples: Int, rollups: Int, censuses: Int,
        alerts: Int, dbSizeBytes: UInt64,
        lastEvaluatedAt: Date? = nil, consecutiveEvalFailures: UInt64 = 0,
        lastSweptAt: Date? = nil, consecutiveSweepFailures: UInt64 = 0,
        selfFootprintBytes: UInt64 = 0, selfCpuSecs: Double = 0,
        selfUptimeSecs: Double = 0, selfCpuPercent: Double = 0
    ) {
        self.schemaVersion = schemaVersion
        self.samples = samples
        self.rollups = rollups
        self.censuses = censuses
        self.alerts = alerts
        self.dbSizeBytes = dbSizeBytes
        self.lastEvaluatedAt = lastEvaluatedAt
        self.consecutiveEvalFailures = consecutiveEvalFailures
        self.lastSweptAt = lastSweptAt
        self.consecutiveSweepFailures = consecutiveSweepFailures
        self.selfFootprintBytes = selfFootprintBytes
        self.selfCpuSecs = selfCpuSecs
        self.selfUptimeSecs = selfUptimeSecs
        self.selfCpuPercent = selfCpuPercent
    }

    enum CodingKeys: String, CodingKey {
        case schemaVersion, samples, rollups, censuses, alerts, dbSizeBytes
        case lastEvaluatedAt, consecutiveEvalFailures, lastSweptAt, consecutiveSweepFailures
        case selfFootprintBytes, selfCpuSecs, selfUptimeSecs, selfCpuPercent
    }

    // Hand-written so a daemon from before the self-cost fields still decodes
    // (Sparkle makes app/daemon skew inevitable; the store footer must not vanish
    // over four missing keys). The self-cost fields default to 0 when absent —
    // and 0 renders as "—" in the footer, never as a claim of costing nothing.
    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        schemaVersion = try c.decode(Int.self, forKey: .schemaVersion)
        samples = try c.decode(Int.self, forKey: .samples)
        rollups = try c.decode(Int.self, forKey: .rollups)
        censuses = try c.decode(Int.self, forKey: .censuses)
        alerts = try c.decode(Int.self, forKey: .alerts)
        dbSizeBytes = try c.decode(UInt64.self, forKey: .dbSizeBytes)
        lastEvaluatedAt = try c.decodeIfPresent(Date.self, forKey: .lastEvaluatedAt)
        consecutiveEvalFailures = try c.decode(UInt64.self, forKey: .consecutiveEvalFailures)
        lastSweptAt = try c.decodeIfPresent(Date.self, forKey: .lastSweptAt)
        consecutiveSweepFailures = try c.decode(UInt64.self, forKey: .consecutiveSweepFailures)
        selfFootprintBytes = try c.decodeIfPresent(UInt64.self, forKey: .selfFootprintBytes) ?? 0
        selfCpuSecs = try c.decodeIfPresent(Double.self, forKey: .selfCpuSecs) ?? 0
        selfUptimeSecs = try c.decodeIfPresent(Double.self, forKey: .selfUptimeSecs) ?? 0
        selfCpuPercent = try c.decodeIfPresent(Double.self, forKey: .selfCpuPercent) ?? 0
    }

    /// The daemon has reported its own cost (an older daemon has not).
    public var reportsSelfCost: Bool { selfFootprintBytes > 0 }
}
