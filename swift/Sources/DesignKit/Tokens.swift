// DesignKit — the design tokens for Banshee. Views use ONLY these
// tokens; raw literals for spacing, radius, or color roles in view code
// are a review finding (skills/swift-conventions.md).

import SwiftUI

public enum Spacing {
    /// 4pt grid. Views compose these; they never hardcode point values.
    public static let xs: CGFloat = 4
    public static let sm: CGFloat = 8
    public static let md: CGFloat = 16
    public static let lg: CGFloat = 24
    public static let xl: CGFloat = 40
}

/// Element SIZES — distinct from Spacing. A dot's diameter is not a margin;
/// reusing a Spacing value as a size (the old `frame(width: Spacing.sm)`)
/// couples unrelated things — bump the spacing scale and the dot resizes
/// (design M6). Sizes get their own semantic tokens.
public enum Size {
    /// Diameter of the band status dot.
    public static let statusDot: CGFloat = 8
    /// Reserved width of the trailing metadata (date) column, so the date
    /// sits in a stable column regardless of per-row affordances (design H2).
    public static let metaColumn: CGFloat = 96
}

public enum Radius {
    public static let card: CGFloat = 10
    public static let control: CGFloat = 6
}

/// Semantic color roles. Views name the ROLE, not the color, so a theme
/// swap is a one-file change. Every color used in a view must have a role
/// here — a raw `.red` in view code is a review finding (design M5).
public enum Palette {
    /// The brand accent. Applied at the app root via `.tint(Palette.accent)`
    /// so prominent controls use it (design H4 — it was dead code before).
    public static let accent = Color.purple
    public static let background = Color(nsColor: .windowBackgroundColor)
    public static let cardBackground = Color(nsColor: .controlBackgroundColor)
    public static let textPrimary = Color.primary
    public static let textSecondary = Color.secondary
    /// Status roles. Reuse these for any semantic pass/fail signal so views never
    /// reach for a raw color.
    public static let success = Color.green
    public static let warning = Color.orange
    public static let error = Color.red
    public static let separator = Color(nsColor: .separatorColor)

    /// The colour roles for the three pressure bands.
    ///
    /// Named roles rather than a `band(_:)` function, because DesignKit must not
    /// depend on BansheeCore — the token layer has no app logic in it. The view
    /// switches over the wire enum and picks a role, and that switch is exhaustive,
    /// so a fourth band would be a compile error rather than a blank dot.
    ///
    /// These name a SEVERITY ROLE, not a severity decision: the band arrived over
    /// the wire, already decided in banshee-core with hysteresis applied
    /// (ADR-0005).
    ///
    /// Colour is never the only carrier of this meaning — every view that draws a
    /// band dot also composes the band name into its accessibility label, because
    /// a colour-only signal is invisible to VoiceOver and unreliable for the
    /// roughly one man in twelve who cannot separate the green from the red.
    public static let bandGreen = success
    public static let bandYellow = warning
    public static let bandRed = error
}

public enum Typography {
    public static let title = Font.title2.weight(.semibold)
    /// A row/list title: heavier than body so titles read as titles, lighter
    /// than the section `title` (design M2 — body-weight titles didn't read).
    public static let rowTitle = Font.body.weight(.semibold)
    public static let body = Font.body
    public static let caption = Font.caption
    public static let mono = Font.system(.body, design: .monospaced)
}
