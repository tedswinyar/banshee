// The wire-format pressure types. Field names map 1:1 to the camelCase JSON keys
// — no CodingKeys remapping, so what you read here is what crosses the wire.
//
// **Nothing here computes a severity.** The level, the band, the normalised
// severity and the GLYPH are all decided once, in banshee-core, and travel over
// the wire (ADR-0005). A Swift `switch` mapping a level to a face would have to be
// kept in step by hand with a Rust `match`, and the whole point of the glyph is
// that it means the same thing on every surface. Vocabulary and layout still
// belong to DesignKit; the level→glyph mapping does not.

import Foundation

/// Severity, as a person distinguishes it.
///
/// `checking` is NOT `quiet`. "We have not looked yet" and "nothing is wrong" are
/// different answers, and only one of them is safe to put in a menu bar.
///
/// `Level` and `Band` decode STRICTLY, unlike `Source`/`ReadingUnit`/`Action`
/// — deliberately (bead banshee-dnk). They are the core model (ADR-0005): the app compares them
/// (`>= .wailing` decides whether to notify), so an `.unknown` level would need a
/// rank, and any rank is a severity decision made in the client — the thing
/// ADR-0005 forbids. Adding a level or band is a MAJOR version (VERSIONING.md),
/// and a client that cannot rank a verdict is right to say so rather than guess.
public enum Level: String, Codable, Sendable, Comparable, CaseIterable {
    case checking
    case quiet
    case stirring
    case restless
    case wailing
    case shrieking

    /// Ordered by severity, matching Rust's derived `Ord` on the same variants.
    /// Pinned by `testLevelOrderMatchesTheRustScale`.
    private var rank: Int {
        switch self {
        case .checking: return 0
        case .quiet: return 1
        case .stirring: return 2
        case .restless: return 3
        case .wailing: return 4
        case .shrieking: return 5
        }
    }

    public static func < (lhs: Level, rhs: Level) -> Bool { lhs.rank < rhs.rank }

    /// Whether this level should interrupt the user with a notification. Mirrors
    /// `Level::warrants_notification` in core; both are `>= .wailing`.
    public var warrantsNotification: Bool { self >= .wailing }
}

/// How bad one dimension is right now.
public enum Band: String, Codable, Sendable, Comparable, CaseIterable {
    case green
    case yellow
    case red

    private var rank: Int {
        switch self {
        case .green: return 0
        case .yellow: return 1
        case .red: return 2
        }
    }

    public static func < (lhs: Band, rhs: Band) -> Bool { lhs.rank < rhs.rank }
}

/// What a dimension's pressure comes FROM — the menu bar's suffix glyph.
///
/// The glyph itself travels over the wire (ADR-0005); this value is the NAME of
/// the source, and the app never switches on it. That is what makes the lenient
/// decode below safe: a source this build has not heard of costs nothing to carry.
public enum Source: String, Codable, Sendable {
    case cpu
    case memory
    case disk
    case sprawl
    case managedAgents
    /// Heat: the kernel is throttling the machine to cool it (schema v11).
    case thermal
    /// A source this build does not recognise — a daemon on a different version
    /// (Sparkle makes skew inevitable) naming a dimension family added later, e.g.
    /// the thermal dimension. Decoding to a fallback instead of throwing keeps ONE
    /// unknown source from failing the whole `Pressure` decode and blanking the
    /// verdict to "nothing is watching" — the `Action.unknown` reasoning, applied
    /// to the second strict String enum on the wire (banshee-dnk).
    case unknown

    public init(from decoder: Decoder) throws {
        let raw = try decoder.singleValueContainer().decode(String.self)
        self = Source(rawValue: raw) ?? .unknown
    }
}

/// How a dimension's value should be rendered.
///
/// Named `ReadingUnit`, not `Unit`, because Foundation already exports a `Unit`
/// class: the bare name compiles inside this module (its own type shadows the
/// import) and is AMBIGUOUS from any target that imports both — which is the test
/// target, so the collision showed up only when the test suite was built. The wire
/// key is still `unit`; a Swift type name is not part of the contract.
public enum ReadingUnit: String, Codable, Sendable {
    case ratio
    case bytes
    case perSecond
    case count
    case percent
    case days
    /// A unit this build does not recognise (a newer daemon's new dimension, e.g.
    /// a thermal level or a temperature). The row still renders — core composes
    /// the human `detail` string, so nothing the person reads depends on the app
    /// knowing the unit — and only a unit-specific formatter falls back
    /// to a plain number (bead banshee-dnk). Throwing here would blank every dimension over one.
    case unknown

    public init(from decoder: Decoder) throws {
        let raw = try decoder.singleValueContainer().decode(String.self)
        self = ReadingUnit(rawValue: raw) ?? .unknown
    }
}

/// A recommended action, ranked by MEASURED impact in core.
public enum Action: String, Codable, Sendable {
    case reapStaleSessions
    case reapOrphans
    case relaunchApps
    case reboot
    case openDiskTool
    case none
    /// An action this build does not recognise — a newer or older daemon over
    /// the wire (Sparkle auto-update makes version skew inevitable). Decoding to
    /// a fallback instead of throwing keeps ONE unknown action from failing the
    /// entire verdict decode and blanking the UI to "nothing is watching" — the
    /// exact bug an installed daemon emitting a since-renamed action caused. The
    /// finding's human `actionLabel` still shows; only the actionable button is
    /// withheld for an action we can't safely perform.
    case unknown

    public init(from decoder: Decoder) throws {
        let raw = try decoder.singleValueContainer().decode(String.self)
        self = Action(rawValue: raw) ?? .unknown
    }
}

/// One dimension, as the UI receives it.
public struct DimensionReading: Codable, Identifiable, Equatable, Sendable {
    /// Stable across refreshes, which is what lets SwiftUI animate a row rather
    /// than replace it. The dimension key IS the identity — there is exactly one
    /// reading per dimension.
    public var id: String { key }

    public let dimension: String
    public let key: String
    public let label: String
    public let band: Band
    public let value: Double
    public let unit: ReadingUnit
    /// 0.0 at the yellow line, 1.0 at red, above 1.0 beyond, negative when green.
    public let severity: Double
    /// How long the band has held, counting back only as far as the observations are
    /// CONTINUOUS.
    public let heldSecs: UInt64
    /// The gap in observations that cut `heldSecs` short, when one did.
    ///
    /// Nullable on the wire. Present so a client can distinguish "red for 45
    /// seconds" from "red for 45 seconds, and nobody was watching for the 17 minutes
    /// before that" — silently rounding the second up to the first is what produced
    /// a false Shrieking verdict minutes after the daemon was first installed.
    public let observationGapSecs: UInt64?
    /// Nullable on the wire (`"trendPerSec": null`), never absent.
    public let trendPerSec: Double?
    /// Human phrasing composed in core, so every surface says the same words.
    public let detail: String
    public let advisory: Bool
    /// A band the newest readings imply but which has not been confirmed yet.
    public let pending: Band?
    /// This dimension has an open alert episode in its down window (ADR-0009): it
    /// was red, it has dropped below red, and the incident is not over yet. A
    /// MODIFIER, not a band — the band is honest about now; this is honest about
    /// the recent past.
    public let recovering: Bool

    public init(
        dimension: String, key: String, label: String, band: Band, value: Double,
        unit: ReadingUnit, severity: Double, heldSecs: UInt64,
        observationGapSecs: UInt64? = nil, trendPerSec: Double?,
        detail: String, advisory: Bool, pending: Band?, recovering: Bool = false
    ) {
        self.dimension = dimension
        self.key = key
        self.label = label
        self.band = band
        self.value = value
        self.unit = unit
        self.severity = severity
        self.heldSecs = heldSecs
        self.observationGapSecs = observationGapSecs
        self.trendPerSec = trendPerSec
        self.detail = detail
        self.advisory = advisory
        self.pending = pending
        self.recovering = recovering
    }

    enum CodingKeys: String, CodingKey {
        case dimension, key, label, band, value, unit, severity, heldSecs
        case observationGapSecs, trendPerSec, detail, advisory, pending, recovering
    }

    // Hand-written so a daemon from before ADR-0009 (no `recovering` key) still
    // decodes: Sparkle makes app/daemon version skew inevitable, and one missing
    // key must not blank the whole verdict to "nothing is watching" — the same
    // reasoning as `Action.unknown`. Every other field is required.
    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        dimension = try c.decode(String.self, forKey: .dimension)
        key = try c.decode(String.self, forKey: .key)
        label = try c.decode(String.self, forKey: .label)
        band = try c.decode(Band.self, forKey: .band)
        value = try c.decode(Double.self, forKey: .value)
        unit = try c.decode(ReadingUnit.self, forKey: .unit)
        severity = try c.decode(Double.self, forKey: .severity)
        heldSecs = try c.decode(UInt64.self, forKey: .heldSecs)
        observationGapSecs = try c.decodeIfPresent(UInt64.self, forKey: .observationGapSecs)
        trendPerSec = try c.decodeIfPresent(Double.self, forKey: .trendPerSec)
        detail = try c.decode(String.self, forKey: .detail)
        advisory = try c.decode(Bool.self, forKey: .advisory)
        pending = try c.decodeIfPresent(Band.self, forKey: .pending)
        recovering = try c.decodeIfPresent(Bool.self, forKey: .recovering) ?? false
    }

    // Hand-written on purpose: the synthesized encoder uses encodeIfPresent and
    // OMITS nil keys, silently violating the wire contract's present-as-null rule.
    // Do not "simplify" this back to the derived conformance — WireFormatTests
    // pins it.
    //
    // ⚠️ FOOT-GUN: this encoder is a HARD-CODED field list. Add a stored property
    // and the synthesized DECODER will read it while THIS encoder silently drops
    // it — an asymmetric round-trip that loses data on write with zero compiler
    // complaint. `testEncodeCoversEveryDimensionField` decodes a fully-populated
    // fixture, re-encodes, and fails the moment this list falls behind.
    public func encode(to encoder: Encoder) throws {
        var c = encoder.container(keyedBy: CodingKeys.self)
        try c.encode(dimension, forKey: .dimension)
        try c.encode(key, forKey: .key)
        try c.encode(label, forKey: .label)
        try c.encode(band, forKey: .band)
        try c.encode(value, forKey: .value)
        try c.encode(unit, forKey: .unit)
        try c.encode(severity, forKey: .severity)
        try c.encode(heldSecs, forKey: .heldSecs)
        if let observationGapSecs {
            try c.encode(observationGapSecs, forKey: .observationGapSecs)
        } else {
            try c.encodeNil(forKey: .observationGapSecs)
        }
        if let trendPerSec {
            try c.encode(trendPerSec, forKey: .trendPerSec)
        } else {
            try c.encodeNil(forKey: .trendPerSec)
        }
        try c.encode(detail, forKey: .detail)
        try c.encode(advisory, forKey: .advisory)
        if let pending {
            try c.encode(pending, forKey: .pending)
        } else {
            try c.encodeNil(forKey: .pending)
        }
        try c.encode(recovering, forKey: .recovering)
    }
}

/// One named consumer behind a finding: a process group and what it costs, in
/// the dimension's own terms (`7.7 GB`, `29% of one core`), worded in core.
public struct Consumer: Codable, Equatable, Sendable {
    public let name: String
    public let count: Int
    public let detail: String

    public init(name: String, count: Int, detail: String) {
        self.name = name
        self.count = count
        self.detail = detail
    }
}

/// Something worth telling the user, with what to do about it.
public struct Finding: Codable, Identifiable, Equatable, Sendable {
    /// Findings have no server-assigned id; a dimension raises at most one, so
    /// the pair identifies it well enough for a stable list row.
    public var id: String { "\(dimension)-\(action.rawValue)" }

    public let dimension: String
    public let band: Band
    public let message: String
    public let action: Action
    /// The action's wording, composed in core and rendered VERBATIM.
    ///
    /// A local `switch action { … }` would be a Swift table kept in step with a
    /// Rust one by hand — the duplication ADR-0005 rejects for the glyph, for the
    /// same reason. `action` is still here because a client may want to branch on
    /// it (an icon, a keyboard shortcut, eventually a button that does the thing);
    /// what it must not do is invent the words.
    public let actionLabel: String
    /// The top consumers behind this finding, biggest first, from the daemon's
    /// own census. Empty when nothing is attributable (disk, uptime, no census).
    public let who: [Consumer]
    /// `who` as one line — `who: Chrome ×115 at 7.7 GB, claude ×8 at 2.1 GB` —
    /// composed in core so this popover, the CLI and the alert text name the same
    /// consumers in the same words. Rendered VERBATIM; nil exactly when `who` is
    /// empty, so a view shows nothing rather than a bare `who:`.
    public let whoLine: String?

    public init(
        dimension: String, band: Band, message: String, action: Action,
        actionLabel: String, who: [Consumer] = [], whoLine: String? = nil
    ) {
        self.dimension = dimension
        self.band = band
        self.message = message
        self.action = action
        self.actionLabel = actionLabel
        self.who = who
        self.whoLine = whoLine
    }

    enum CodingKeys: String, CodingKey {
        case dimension, band, message, action, actionLabel, who, whoLine
    }

    // Hand-written: a daemon from before the who-line sends neither key, and that
    // must decode as "names nobody", not fail and blank the whole verdict.
    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        dimension = try c.decode(String.self, forKey: .dimension)
        band = try c.decode(Band.self, forKey: .band)
        message = try c.decode(String.self, forKey: .message)
        action = try c.decode(Action.self, forKey: .action)
        actionLabel = try c.decode(String.self, forKey: .actionLabel)
        who = try c.decodeIfPresent([Consumer].self, forKey: .who) ?? []
        whoLine = try c.decodeIfPresent(String.self, forKey: .whoLine)
    }

    // Hand-written because `whoLine` is nullable and present-as-null binds the
    // encoder; the synthesized one drops the key. `testEncodeCoversEveryFindingField`
    // enforces the list.
    public func encode(to encoder: Encoder) throws {
        var c = encoder.container(keyedBy: CodingKeys.self)
        try c.encode(dimension, forKey: .dimension)
        try c.encode(band, forKey: .band)
        try c.encode(message, forKey: .message)
        try c.encode(action, forKey: .action)
        try c.encode(actionLabel, forKey: .actionLabel)
        try c.encode(who, forKey: .who)
        if let whoLine {
            try c.encode(whoLine, forKey: .whoLine)
        } else {
            try c.encodeNil(forKey: .whoLine)
        }
    }
}

/// The long memory, summarised (ADR-0009, `banshee-qoz`): alert episodes open
/// now, and active in the last hour and day. Counts ACTIVITY, not onsets.
public struct RecentActivity: Codable, Equatable, Sendable {
    public let open: Int
    public let lastHour: Int
    public let lastDay: Int

    public init(open: Int, lastHour: Int, lastDay: Int) {
        self.open = open
        self.lastHour = lastHour
        self.lastDay = lastDay
    }

    public static let none = RecentActivity(open: 0, lastHour: 0, lastDay: 0)
}

/// The whole verdict.
public struct Pressure: Codable, Equatable, Sendable {
    public let evaluatedAt: Date
    public let level: Level
    /// The display name core chose. Rendered verbatim rather than derived from
    /// `level`, so the two can never disagree.
    public let levelName: String
    /// Nullable on the wire: `"source": null` when nothing is worth naming.
    public let source: Source?
    /// **The menu bar renders this string verbatim** (ADR-0005).
    public let glyph: String
    /// Spoken by VoiceOver. An emoji-only menu bar is otherwise invisible to it.
    public let accessibilityLabel: String
    public let dimensions: [DimensionReading]
    /// Ranked by measured impact, worst band first within a rank.
    public let findings: [Finding]
    public let sampleCount: Int
    public let censusCount: Int
    /// Some dimension is `recovering` (ADR-0009). Not a seventh level: core
    /// decorates the glyph and label for it when the machine is otherwise calm,
    /// and this client renders both verbatim as always.
    public let recovering: Bool
    /// Alert-episode counts, so every surface shows the long memory.
    public let activity: RecentActivity

    public init(
        evaluatedAt: Date, level: Level, levelName: String, source: Source?,
        glyph: String, accessibilityLabel: String,
        dimensions: [DimensionReading], findings: [Finding],
        sampleCount: Int, censusCount: Int,
        recovering: Bool = false, activity: RecentActivity = .none
    ) {
        self.evaluatedAt = evaluatedAt
        self.level = level
        self.levelName = levelName
        self.source = source
        self.glyph = glyph
        self.accessibilityLabel = accessibilityLabel
        self.dimensions = dimensions
        self.findings = findings
        self.sampleCount = sampleCount
        self.censusCount = censusCount
        self.recovering = recovering
        self.activity = activity
    }

    /// Dimensions worth showing: anything out of band, plus a green that is still
    /// RECOVERING — the one green worth seeing, because it was red a moment ago.
    /// Presentation, not a re-derivation — the bands and the flag came off the wire.
    public var concerning: [DimensionReading] {
        dimensions.filter { $0.band > .green || $0.recovering }
    }

    enum CodingKeys: String, CodingKey {
        case evaluatedAt, level, levelName, source, glyph, accessibilityLabel
        case dimensions, findings, sampleCount, censusCount, recovering, activity
    }

    // Hand-written for the same version-skew reason as `DimensionReading`: a
    // pre-ADR-0009 daemon sends neither `recovering` nor `activity`, and that
    // must decode as "none", not as "nothing is watching".
    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        evaluatedAt = try c.decode(Date.self, forKey: .evaluatedAt)
        level = try c.decode(Level.self, forKey: .level)
        levelName = try c.decode(String.self, forKey: .levelName)
        source = try c.decodeIfPresent(Source.self, forKey: .source)
        glyph = try c.decode(String.self, forKey: .glyph)
        accessibilityLabel = try c.decode(String.self, forKey: .accessibilityLabel)
        dimensions = try c.decode([DimensionReading].self, forKey: .dimensions)
        findings = try c.decode([Finding].self, forKey: .findings)
        sampleCount = try c.decode(Int.self, forKey: .sampleCount)
        censusCount = try c.decode(Int.self, forKey: .censusCount)
        recovering = try c.decodeIfPresent(Bool.self, forKey: .recovering) ?? false
        activity = try c.decodeIfPresent(RecentActivity.self, forKey: .activity) ?? .none
    }

    // Hand-written for the same reason as `DimensionReading.encode` — `source` is
    // nullable and the synthesized encoder would drop the key. Add a field here
    // and add a line here; `testEncodeCoversEveryPressureField` enforces it.
    public func encode(to encoder: Encoder) throws {
        var c = encoder.container(keyedBy: CodingKeys.self)
        try c.encode(evaluatedAt, forKey: .evaluatedAt)
        try c.encode(level, forKey: .level)
        try c.encode(levelName, forKey: .levelName)
        if let source {
            try c.encode(source, forKey: .source)
        } else {
            try c.encodeNil(forKey: .source)
        }
        try c.encode(glyph, forKey: .glyph)
        try c.encode(accessibilityLabel, forKey: .accessibilityLabel)
        try c.encode(dimensions, forKey: .dimensions)
        try c.encode(findings, forKey: .findings)
        try c.encode(sampleCount, forKey: .sampleCount)
        try c.encode(censusCount, forKey: .censusCount)
        try c.encode(recovering, forKey: .recovering)
        try c.encode(activity, forKey: .activity)
    }
}

public enum Wire {
    /// The one decoder/encoder pair every wire type goes through.
    /// Constructing ad-hoc JSONDecoders in views or clients is a review
    /// finding — date handling would silently diverge.
    public static func decoder() -> JSONDecoder {
        let d = JSONDecoder()
        d.dateDecodingStrategy = WireDate.decodingStrategy
        return d
    }

    public static func encoder() -> JSONEncoder {
        let e = JSONEncoder()
        e.dateEncodingStrategy = WireDate.encodingStrategy
        return e
    }
}
