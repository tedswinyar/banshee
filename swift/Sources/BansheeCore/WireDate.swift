// Canonical wire format for datetimes — the Swift twin of Rust's
// `wire_time` module. Both sides implement the same contract
// (docs/wire-format.md):
//
//   encode: exactly 6 fractional digits, Z suffix
//   decode: generous — any fractional length (incl. none), Z or ±HH:MM
//
// Neither ISO8601DateFormatter nor DateFormatter can honor this contract:
// ICU truncates fractional seconds to MILLISECONDS on parse and zero-pads
// on format, so `.123456` round-trips as `.123000`. The fractional part is
// therefore handled manually; ICU only ever sees whole seconds.

import Foundation

public enum WireDate {
    /// Whole-second formatter used for the date/time/zone portion only.
    private static let baseParser: DateFormatter = {
        let f = DateFormatter()
        f.locale = Locale(identifier: "en_US_POSIX")
        f.timeZone = TimeZone(identifier: "UTC")
        f.dateFormat = "yyyy-MM-dd'T'HH:mm:ssZZZZZ" // ZZZZZ accepts Z and ±HH:MM
        return f
    }()

    private static let baseEncoder: DateFormatter = {
        let f = DateFormatter()
        f.locale = Locale(identifier: "en_US_POSIX")
        f.timeZone = TimeZone(identifier: "UTC")
        f.dateFormat = "yyyy-MM-dd'T'HH:mm:ss"
        return f
    }()

    /// `^<base>(.fraction)?<zone>$` where zone is Z or ±HH:MM.
    ///
    /// Computed, not `static let`: a `Regex` is not `Sendable`, so a stored
    /// global would be a data-race hazard the Swift 6 concurrency checker
    /// rejects. Rebuilding it per-decode is cheap and race-free.
    ///
    /// The offset alternative requires the colon (`±HH:MM`), matching both
    /// the wire contract and `baseParser`'s documented `ZZZZZ` form. An
    /// earlier `:?` accepted colon-less `+0100`, which the contract does not
    /// sanction — the gate is now exactly the contract (L3).
    private static var shape: Regex<(Substring, Substring, Substring?, Substring)> {
        #/^(\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2})(?:\.(\d+))?(Z|[+-]\d{2}:\d{2})$/#
    }

    /// The canonical contract is a 4-digit year (`\d{4}`). This is the last
    /// representable canonical instant; encode saturates here rather than
    /// carry a rounded microsecond into year 10000 (adversarial finding,
    /// 2026-08-20 — Swift's rounding rolled 9999-…-59.999999 into a 5-digit
    /// year that violated the wire contract).
    ///
    /// There is NO symmetric lower-bound (pre-year-1000) guard, and it is not
    /// an oversight: `DateFormatter`'s `yyyy` field ZERO-PADS to a minimum of
    /// four digits and, with no era field, renders even BC instants as a
    /// positive 4-digit year (year 250 → "0250", a pre-epoch BC date → still
    /// four digits). Verified 2026-08-20 on Swift 6.3.3 / macOS 26: no input
    /// produces a 3-digit or leading-`-` year, so the only reachable `\d{4}`
    /// violation is the upper overflow. `testEncodePadsAncientYearsToFourDigits`
    /// pins this. (Review finding L4 predicted a 3-digit emission; it does not
    /// reproduce here — the underflow is a non-issue given the padding.)
    private static let maxCanonical = "9999-12-31T23:59:59.999999Z"

    /// Canonical encode: 2026-03-17T14:30:00.123456Z
    public static func encode(_ date: Date) -> String {
        let t = date.timeIntervalSince1970
        var whole = floor(t)
        var micros = Int(((t - whole) * 1_000_000).rounded())
        if micros == 1_000_000 {
            micros = 0
            whole += 1
        }
        let base = baseEncoder.string(from: Date(timeIntervalSince1970: whole))
        // A carry (or any input) that pushes past year 9999 would emit a
        // 5-digit year. Saturate at the max canonical instant instead. (The
        // year field is everything before the first date `-`; see the
        // maxCanonical note for why there is no lower-bound guard.)
        if base.prefix(while: { $0 != "-" }).count > 4 {
            return maxCanonical
        }
        return base + String(format: ".%06dZ", micros)
    }

    public static func decode(_ string: String) -> Date? {
        guard let match = string.wholeMatch(of: shape) else { return nil }
        let (_, base, fraction, zone) = match.output
        guard let baseDate = baseParser.date(from: String(base) + String(zone)) else { return nil }
        guard let fraction, !fraction.isEmpty else { return baseDate }
        guard let fractional = Double("0.\(fraction)") else { return nil }
        return baseDate.addingTimeInterval(fractional)
    }

    /// JSONDecoder strategy for all wire types.
    public static let decodingStrategy = JSONDecoder.DateDecodingStrategy.custom { decoder in
        let container = try decoder.singleValueContainer()
        let raw = try container.decode(String.self)
        guard let date = WireDate.decode(raw) else {
            throw DecodingError.dataCorruptedError(
                in: container,
                debugDescription: "unparseable wire datetime: \(raw)"
            )
        }
        return date
    }

    /// JSONEncoder strategy for all wire types.
    public static let encodingStrategy = JSONEncoder.DateEncodingStrategy.custom { date, encoder in
        var container = encoder.singleValueContainer()
        try container.encode(WireDate.encode(date))
    }
}
