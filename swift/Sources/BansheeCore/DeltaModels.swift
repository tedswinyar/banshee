// The wire-format delta types — `GET /deltas`. Mirrors
// `rust/banshee-core/src/pressure/deltas.rs`; field names map 1:1 to the
// camelCase JSON keys.
//
// A delta answers "what changed since the episode started" and "why did freeing
// 4 GB not help": the newest census + verdict versus a lookback, per app group,
// per session program, per dimension. The reduction and the human `summary`
// sentence are composed ONCE in core (ADR-0005) and travel over the wire — this
// client renders them, it never re-derives a difference.
//
// ⚠️ A delta over an observation gap is arithmetic on a hole. `observationGapSecs`
// (top-level, and per dimension) is non-null exactly when nobody was watching for
// part of the compared range — and on macOS "used memory went down" is NOT
// "pressure went down". Trust the sentence, check the gap.

import Foundation

/// Which kind of thing a summarised consumer is. Lenient decode (like `Source`
/// and `Action`): the app never branches on severity here, so a kind a newer
/// daemon names costs nothing to carry as `.unknown` rather than failing the
/// whole delta decode.
public enum ConsumerKind: String, Codable, Sendable {
    /// A configured app process group (Chrome, Slack…), sized by RSS.
    case appGroup
    /// Interactive agent CLI sessions grouped by program (claude, kiro-cli…).
    case sessions
    /// One of the census's rollups: IDE agent helpers, orphaned helpers, managed
    /// agents.
    case rollup
    /// A kind this build does not recognise.
    case unknown

    public init(from decoder: Decoder) throws {
        let raw = try decoder.singleValueContainer().decode(String.self)
        self = ConsumerKind(rawValue: raw) ?? .unknown
    }
}

/// One named consumer and its size at one instant.
public struct ConsumerSize: Codable, Equatable, Sendable {
    public let name: String
    public let kind: ConsumerKind
    public let count: Int
    public let rssBytes: UInt64

    public init(name: String, kind: ConsumerKind, count: Int, rssBytes: UInt64) {
        self.name = name
        self.kind = kind
        self.count = count
        self.rssBytes = rssBytes
    }
}

/// A census reduced to the who-line's inputs, captured inside an episode at its
/// peak (`AlertEpisode.censusAtPeak`) and diffable against a live census.
public struct CensusSummary: Codable, Equatable, Sendable {
    public let takenAt: Date
    public let totalProcs: Int
    public let consumers: [ConsumerSize]
    public let orphanCount: Int
    /// The deduplicated managed-agent figure, never a sum of per-group percentages.
    public let monitorPercentOfOneCore: Double

    public init(
        takenAt: Date, totalProcs: Int, consumers: [ConsumerSize],
        orphanCount: Int, monitorPercentOfOneCore: Double
    ) {
        self.takenAt = takenAt
        self.totalProcs = totalProcs
        self.consumers = consumers
        self.orphanCount = orphanCount
        self.monitorPercentOfOneCore = monitorPercentOfOneCore
    }
}

/// The whole-machine verdict at both ends of the lookback.
public struct VerdictDelta: Codable, Equatable, Sendable {
    public let thenLevel: Level
    public let nowLevel: Level
    public let thenSource: Source?
    public let nowSource: Source?
    /// `pressure Restless → Wailing`, or `pressure unchanged at Restless`.
    public let detail: String

    public init(
        thenLevel: Level, nowLevel: Level, thenSource: Source?,
        nowSource: Source?, detail: String
    ) {
        self.thenLevel = thenLevel
        self.nowLevel = nowLevel
        self.thenSource = thenSource
        self.nowSource = nowSource
        self.detail = detail
    }

    enum CodingKeys: String, CodingKey {
        case thenLevel, nowLevel, thenSource, nowSource, detail
    }

    // Hand-written: `thenSource`/`nowSource` are nullable and present-as-null
    // binds the encoder (the synthesized one drops a nil key). Pinned by
    // `testEncodeCoversEveryVerdictDeltaField`.
    public func encode(to encoder: Encoder) throws {
        var c = encoder.container(keyedBy: CodingKeys.self)
        try c.encode(thenLevel, forKey: .thenLevel)
        try c.encode(nowLevel, forKey: .nowLevel)
        if let thenSource {
            try c.encode(thenSource, forKey: .thenSource)
        } else {
            try c.encodeNil(forKey: .thenSource)
        }
        if let nowSource {
            try c.encode(nowSource, forKey: .nowSource)
        } else {
            try c.encodeNil(forKey: .nowSource)
        }
        try c.encode(detail, forKey: .detail)
    }
}

/// One dimension's value at both ends, and what lies between.
public struct DimensionDelta: Codable, Identifiable, Equatable, Sendable {
    public var id: String { key }

    /// A string, like `DimensionReading.dimension`, so a dimension added to the
    /// wire cannot make an old delta undecodable.
    public let dimension: String
    public let key: String
    public let label: String
    public let unit: ReadingUnit
    public let then: Double
    public let now: Double
    /// `now − then`, in the dimension's own unit.
    public let delta: Double
    public let thenAt: Date
    public let nowAt: Date
    /// The largest hole in THIS series between the two compared points. Present
    /// (non-null) means the delta is arithmetic across a period nobody watched.
    public let observationGapSecs: UInt64?
    /// `swap 12.0 GB → 13.3 GB (+1.3 GB)`, composed in core.
    public let detail: String

    public init(
        dimension: String, key: String, label: String, unit: ReadingUnit,
        then: Double, now: Double, delta: Double, thenAt: Date, nowAt: Date,
        observationGapSecs: UInt64? = nil, detail: String
    ) {
        self.dimension = dimension
        self.key = key
        self.label = label
        self.unit = unit
        self.then = then
        self.now = now
        self.delta = delta
        self.thenAt = thenAt
        self.nowAt = nowAt
        self.observationGapSecs = observationGapSecs
        self.detail = detail
    }

    enum CodingKeys: String, CodingKey {
        case dimension, key, label, unit, then, now, delta, thenAt, nowAt
        case observationGapSecs, detail
    }

    // Hand-written: `observationGapSecs` is nullable and present-as-null binds
    // the encoder. Pinned by `testEncodeCoversEveryDimensionDeltaField`.
    public func encode(to encoder: Encoder) throws {
        var c = encoder.container(keyedBy: CodingKeys.self)
        try c.encode(dimension, forKey: .dimension)
        try c.encode(key, forKey: .key)
        try c.encode(label, forKey: .label)
        try c.encode(unit, forKey: .unit)
        try c.encode(then, forKey: .then)
        try c.encode(now, forKey: .now)
        try c.encode(delta, forKey: .delta)
        try c.encode(thenAt, forKey: .thenAt)
        try c.encode(nowAt, forKey: .nowAt)
        if let observationGapSecs {
            try c.encode(observationGapSecs, forKey: .observationGapSecs)
        } else {
            try c.encodeNil(forKey: .observationGapSecs)
        }
        try c.encode(detail, forKey: .detail)
    }
}

/// One consumer at both ends. A consumer present at only one end has a zero count
/// and size at the other — "appeared" and "gone" are both differences.
public struct ConsumerDelta: Codable, Identifiable, Equatable, Sendable {
    public var id: String { "\(name)-\(kind.rawValue)" }

    public let name: String
    public let kind: ConsumerKind
    public let thenCount: Int
    public let nowCount: Int
    public let thenBytes: UInt64
    public let nowBytes: UInt64
    /// `nowBytes − thenBytes`, signed.
    public let deltaBytes: Int64
    /// `freed 4.0 GB from Chrome (×128 → ×34)`, composed in core.
    public let detail: String

    public init(
        name: String, kind: ConsumerKind, thenCount: Int, nowCount: Int,
        thenBytes: UInt64, nowBytes: UInt64, deltaBytes: Int64, detail: String
    ) {
        self.name = name
        self.kind = kind
        self.thenCount = thenCount
        self.nowCount = nowCount
        self.thenBytes = thenBytes
        self.nowBytes = nowBytes
        self.deltaBytes = deltaBytes
        self.detail = detail
    }
}

/// Which two censuses the consumer deltas compare.
public struct CensusPair: Codable, Equatable, Sendable {
    public let thenAt: Date
    public let nowAt: Date

    public init(thenAt: Date, nowAt: Date) {
        self.thenAt = thenAt
        self.nowAt = nowAt
    }
}

/// The answer: what changed, by how much, across what, and the sentence.
public struct Deltas: Codable, Equatable, Sendable {
    public let evaluatedAt: Date
    /// The lookback as requested — `1h`, `24h`, `episode` — echoed back.
    public let lookback: String
    /// `episode` anchors carry the episode's id; null otherwise.
    public let episodeId: UUID?
    public let requestedLookbackSecs: UInt64
    /// The sample actually compared against; null when there are no samples.
    public let anchorAt: Date?
    /// `evaluatedAt − anchorAt`; shorter than requested when history is short.
    public let actualLookbackSecs: UInt64?
    public let verdict: VerdictDelta?
    public let dimensions: [DimensionDelta]
    public let consumers: [ConsumerDelta]
    public let census: CensusPair?
    /// The largest `observationGapSecs` across `dimensions` — the one field to
    /// check before trusting any number here. Null when observed continuously.
    public let observationGapSecs: UInt64?
    /// The words a person reads, composed in core so every surface agrees.
    public let summary: String

    public init(
        evaluatedAt: Date, lookback: String, episodeId: UUID?,
        requestedLookbackSecs: UInt64, anchorAt: Date?,
        actualLookbackSecs: UInt64?, verdict: VerdictDelta?,
        dimensions: [DimensionDelta], consumers: [ConsumerDelta],
        census: CensusPair?, observationGapSecs: UInt64?, summary: String
    ) {
        self.evaluatedAt = evaluatedAt
        self.lookback = lookback
        self.episodeId = episodeId
        self.requestedLookbackSecs = requestedLookbackSecs
        self.anchorAt = anchorAt
        self.actualLookbackSecs = actualLookbackSecs
        self.verdict = verdict
        self.dimensions = dimensions
        self.consumers = consumers
        self.census = census
        self.observationGapSecs = observationGapSecs
        self.summary = summary
    }

    /// A delta compared across a period nobody was watching for part of.
    public var spansGap: Bool { observationGapSecs != nil }

    enum CodingKeys: String, CodingKey {
        case evaluatedAt, lookback, episodeId, requestedLookbackSecs, anchorAt
        case actualLookbackSecs, verdict, dimensions, consumers, census
        case observationGapSecs, summary
    }

    // Hand-written for the UUID's lowercase-out rule and the present-as-null
    // nullables (the synthesized encoder drops nil keys). Hard-coded field list,
    // pinned by `testEncodeCoversEveryDeltasField`.
    public func encode(to encoder: Encoder) throws {
        var c = encoder.container(keyedBy: CodingKeys.self)
        try c.encode(evaluatedAt, forKey: .evaluatedAt)
        try c.encode(lookback, forKey: .lookback)
        if let episodeId {
            try c.encode(episodeId.uuidString.lowercased(), forKey: .episodeId)
        } else {
            try c.encodeNil(forKey: .episodeId)
        }
        try c.encode(requestedLookbackSecs, forKey: .requestedLookbackSecs)
        if let anchorAt {
            try c.encode(anchorAt, forKey: .anchorAt)
        } else {
            try c.encodeNil(forKey: .anchorAt)
        }
        if let actualLookbackSecs {
            try c.encode(actualLookbackSecs, forKey: .actualLookbackSecs)
        } else {
            try c.encodeNil(forKey: .actualLookbackSecs)
        }
        if let verdict {
            try c.encode(verdict, forKey: .verdict)
        } else {
            try c.encodeNil(forKey: .verdict)
        }
        try c.encode(dimensions, forKey: .dimensions)
        try c.encode(consumers, forKey: .consumers)
        if let census {
            try c.encode(census, forKey: .census)
        } else {
            try c.encodeNil(forKey: .census)
        }
        if let observationGapSecs {
            try c.encode(observationGapSecs, forKey: .observationGapSecs)
        } else {
            try c.encodeNil(forKey: .observationGapSecs)
        }
        try c.encode(summary, forKey: .summary)
    }
}
