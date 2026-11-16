// DesignKit — the design tokens for Banshee. Views use ONLY these
// tokens; raw literals for spacing, radius, or color roles in view code
// are a review finding (skills/swift-conventions.md).

import AppKit
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
    /// Stroke of a HOLLOW status dot — the reap preview's "spared" row, which is
    /// told from "will be reaped" by shape, not by a band colour.
    public static let statusRing: CGFloat = 1.5
    /// Side of the faint brand mark beside an empty state's words.
    public static let emptyStateMark: CGFloat = 48
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
    /// The brand accent, and it has ONE job: "you can click this" (`banshee-v54.2`).
    /// Controls take it from `.tint(Palette.accent)` at each of the three roots —
    /// the detail window, Settings, and the AppKit-hosted popover, which does not
    /// inherit a SwiftUI scene's tint. Text that is advice rather than an action is
    /// `textSecondary`, never this: a purple line reads as a link.
    ///
    /// This comment said "applied at the app root" for months while no `.tint(`
    /// call existed anywhere (found by an outside review). The scripts suite's
    /// `test-accent-applied.sh` now holds the claim to the source.
    ///
    /// Purple in dark appearance is the system's. In light appearance it is a
    /// darker value, because the accent is also TEXT here (link-style actions on
    /// a light card) and system purple on white is about 4.2:1; `TokenTests` pins
    /// the ratio rather than trusting this sentence.
    public static let accent = Color(nsColor: accentNSColor)
    public static let accentNSColor = NSColor(name: nil) { appearance in
        appearance.bestMatch(from: [.darkAqua, .aqua]) == .darkAqua ? .systemPurple : accentLight
    }
    /// #9B25AE.
    public static let accentLight = NSColor(
        srgbRed: 0x9B / 255.0, green: 0x25 / 255.0, blue: 0xAE / 255.0, alpha: 1)
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
    ///
    /// Their OWN values, not aliases of `success`/`warning`/`error`
    /// (`banshee-v54.2`). They were aliases, which meant retuning a band for
    /// light appearance would also have moved the lockout notice, the
    /// daemon-mismatch row and every error message — things that are orange or
    /// red because something is wrong with the APP, not because a dimension is
    /// in that band.
    ///
    /// Each is the system colour in dark appearance and a DARKER value in light
    /// (`banshee-v54.3`). The system green, orange and red measure about 1.9, 1.9
    /// and 3.0 to 1 against the light window ground — under the 3:1 a status dot
    /// needs (WCAG 1.4.11) — and on that ground the dot was the only thing telling
    /// the bands apart. The light values are Apple's accessible shades; `TokenTests`
    /// pins the ratios in both appearances.
    public static let bandGreen = Color(nsColor: bandGreenNSColor)
    public static let bandAmber = Color(nsColor: bandAmberNSColor)
    public static let bandRed = Color(nsColor: bandRedNSColor)

    public static let bandGreenNSColor = band(dark: .systemGreen, lightHex: 0x248A3D)
    public static let bandAmberNSColor = band(dark: .systemOrange, lightHex: 0xB25000)
    public static let bandRedNSColor = band(dark: .systemRed, lightHex: 0xD70015)

    private static func band(dark: NSColor, lightHex: Int) -> NSColor {
        let light = NSColor(
            srgbRed: CGFloat((lightHex >> 16) & 0xFF) / 255,
            green: CGFloat((lightHex >> 8) & 0xFF) / 255,
            blue: CGFloat(lightHex & 0xFF) / 255,
            alpha: 1)
        return NSColor(name: nil) { appearance in
            appearance.bestMatch(from: [.darkAqua, .aqua]) == .darkAqua ? dark : light
        }
    }
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
