// The wire-format headroom types (ADR-0010): the verdict reduced to what an
// agent should DO. Field names map 1:1 to the camelCase JSON keys — with one
// deliberate exception, `Condition.type`, which is the Kubernetes spelling and
// a Swift keyword, so it is `kind` here and remapped in `CodingKeys`.
//
// **Nothing here computes a parallelism.** `shouldWait`, `recommendedParallelism`
// and `retryAfterSecs` are decided once, in banshee-core, from the same evaluation
// as the verdict, and travel over the wire. The popover's future "room for N
// more" renders `recommendedParallelism` verbatim — a Swift formula over cores
// and load would be the local re-derivation ADR-0005 forbids, and would disagree
// with the CLI and the MCP tool.

import Foundation

/// Kubernetes' tri-state, spelled as Kubernetes spells it on the wire.
///
/// Decodes LENIENTLY: a status this build does not know maps to `.unknown`,
/// which is the honest reading of "we cannot tell" — unlike `Level`, no client
/// ranks these, so leniency costs no severity decision (banshee-dnk).
public enum ConditionStatus: String, Codable, Sendable {
    case `true` = "True"
    case `false` = "False"
    case unknown = "Unknown"

    public init(from decoder: Decoder) throws {
        let raw = try decoder.singleValueContainer().decode(String.self)
        self = ConditionStatus(rawValue: raw) ?? .unknown
    }
}

/// One statement about the machine: `{type, status, reason, message, since}`.
public struct Condition: Codable, Identifiable, Equatable, Sendable {
    public var id: String { kind }

    /// `Ready`, or `<Source>Pressure`. The wire key is `type`.
    public let kind: String
    public let status: ConditionStatus
    /// A CamelCase token — `SwapRed`, `AllGreen`, `NoReading`. Stable; branch on it.
    public let reason: String
    /// The worst dimension's `detail` from the verdict, verbatim.
    public let message: String
    /// When the worst dimension entered its band. Nullable on the wire: null when
    /// the status is `Unknown`, and always null for `Ready`.
    public let since: Date?

    public init(kind: String, status: ConditionStatus, reason: String, message: String, since: Date?) {
        self.kind = kind
        self.status = status
        self.reason = reason
        self.message = message
        self.since = since
    }

    enum CodingKeys: String, CodingKey {
        case kind = "type"
        case status, reason, message, since
    }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        kind = try c.decode(String.self, forKey: .kind)
        status = try c.decode(ConditionStatus.self, forKey: .status)
        reason = try c.decode(String.self, forKey: .reason)
        message = try c.decode(String.self, forKey: .message)
        since = try c.decodeIfPresent(Date.self, forKey: .since)
    }

    // Hand-written: `since` is nullable and the synthesized encoder would DROP the
    // key. Present-as-null binds the encoder (docs/wire-format.md).
    public func encode(to encoder: Encoder) throws {
        var c = encoder.container(keyedBy: CodingKeys.self)
        try c.encode(kind, forKey: .kind)
        try c.encode(status, forKey: .status)
        try c.encode(reason, forKey: .reason)
        try c.encode(message, forKey: .message)
        if let since {
            try c.encode(since, forKey: .since)
        } else {
            try c.encodeNil(forKey: .since)
        }
    }
}

/// The decision.
public struct Headroom: Codable, Equatable, Sendable {
    public let evaluatedAt: Date
    /// The level this was derived from. `checking` means `shouldWait` — headroom
    /// is never invented before data exists.
    public let level: Level
    /// Do not start new work right now. Advice, not enforcement. Exactly
    /// `recommendedParallelism == 0` — core guarantees it, this client renders it.
    public let shouldWait: Bool
    /// How many MORE concurrent CPU-bound workers the machine can absorb right now.
    public let recommendedParallelism: Int
    /// Seconds before asking again. Present exactly when `shouldWait`; nil (an
    /// explicit null on the wire) when there is nothing to wait for.
    public let retryAfterSecs: Int?
    /// The core count the arithmetic used. Nil before the first sample.
    public let cores: Int?
    /// One sentence, composed in core. Render verbatim.
    public let reason: String
    /// `Ready`, then one per source in a fixed order — always seven.
    public let conditions: [Condition]

    public init(
        evaluatedAt: Date, level: Level, shouldWait: Bool, recommendedParallelism: Int,
        retryAfterSecs: Int?, cores: Int?, reason: String, conditions: [Condition]
    ) {
        self.evaluatedAt = evaluatedAt
        self.level = level
        self.shouldWait = shouldWait
        self.recommendedParallelism = recommendedParallelism
        self.retryAfterSecs = retryAfterSecs
        self.cores = cores
        self.reason = reason
        self.conditions = conditions
    }

    /// The conditions that are bad news: a pressure `True`, or `Ready` `False`.
    /// Presentation over flags that came off the wire — not a re-derivation.
    public var pressing: [Condition] {
        conditions.filter { c in
            c.kind == "Ready" ? c.status == .false : c.status == .true
        }
    }

    enum CodingKeys: String, CodingKey {
        case evaluatedAt, level, shouldWait, recommendedParallelism, retryAfterSecs
        case cores, reason, conditions
    }

    // Hand-written for the two nullable fields — `retryAfterSecs` and `cores` must
    // go out as present nulls. Add a field here and add a line here;
    // `testEncodeCoversEveryHeadroomField` enforces it.
    public func encode(to encoder: Encoder) throws {
        var c = encoder.container(keyedBy: CodingKeys.self)
        try c.encode(evaluatedAt, forKey: .evaluatedAt)
        try c.encode(level, forKey: .level)
        try c.encode(shouldWait, forKey: .shouldWait)
        try c.encode(recommendedParallelism, forKey: .recommendedParallelism)
        if let retryAfterSecs {
            try c.encode(retryAfterSecs, forKey: .retryAfterSecs)
        } else {
            try c.encodeNil(forKey: .retryAfterSecs)
        }
        if let cores {
            try c.encode(cores, forKey: .cores)
        } else {
            try c.encodeNil(forKey: .cores)
        }
        try c.encode(reason, forKey: .reason)
        try c.encode(conditions, forKey: .conditions)
    }
}
