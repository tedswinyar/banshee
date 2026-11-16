import AppKit
import XCTest
@testable import DesignKit

final class TokenTests: XCTestCase {
    // Invariant: spacing is a strictly increasing 4pt-grid scale. A token
    // edit that breaks monotonicity breaks every layout built on it.
    func testSpacingScaleIsStrictlyIncreasingOnTheGrid() {
        let scale = [Spacing.xs, Spacing.sm, Spacing.md, Spacing.lg, Spacing.xl]
        for (a, b) in zip(scale, scale.dropFirst()) {
            XCTAssertLessThan(a, b)
        }
        for step in scale {
            XCTAssertEqual(step.truncatingRemainder(dividingBy: 4), 0,
                           "spacing tokens stay on the 4pt grid")
        }
    }

    func testRadiiAreSaneForControls() {
        XCTAssertGreaterThan(Radius.card, Radius.control,
                             "cards read as larger surfaces than controls")
    }

    // ---- the accent (banshee-v54.2) ------------------------------------------

    /// WCAG 2.x relative luminance of an sRGB colour.
    private func luminance(_ color: NSColor) -> Double {
        func channel(_ v: CGFloat) -> Double {
            let c = Double(v)
            return c <= 0.03928 ? c / 12.92 : pow((c + 0.055) / 1.055, 2.4)
        }
        return 0.2126 * channel(color.redComponent) + 0.7152 * channel(color.greenComponent)
            + 0.0722 * channel(color.blueComponent)
    }

    private func contrast(_ a: NSColor, _ b: NSColor) -> Double {
        let (la, lb) = (luminance(a), luminance(b))
        return (max(la, lb) + 0.05) / (min(la, lb) + 0.05)
    }

    /// The accent as it resolves under one appearance, in sRGB.
    private func accent(in name: NSAppearance.Name) throws -> NSColor {
        let appearance = try XCTUnwrap(NSAppearance(named: name))
        var resolved: NSColor?
        appearance.performAsCurrentDrawingAppearance {
            resolved = Palette.accentNSColor.usingColorSpace(.sRGB)
        }
        return try XCTUnwrap(resolved)
    }

    private func grey(_ hex: Int) -> NSColor {
        let v = CGFloat(hex) / 255
        return NSColor(srgbRed: v, green: v, blue: v, alpha: 1)
    }

    /// The accent is TEXT in this app (link-style actions on a light card and on
    /// the light window), so in light appearance it must clear 4.5:1 against both.
    /// System purple does not — about 4.2:1 on white — which is why the light value
    /// is its own. Mutation-proof: return `.systemPurple` for both appearances in
    /// `Palette.accentNSColor` and both assertions fail.
    func testTheAccentIsLegibleAsTextInLightAppearance() throws {
        let light = try accent(in: .aqua)
        XCTAssertGreaterThanOrEqual(contrast(light, grey(0xFF)), 4.5, "on a white card")
        XCTAssertGreaterThanOrEqual(contrast(light, grey(0xEA)), 4.5, "on the light window ground")
    }

    /// Dark appearance keeps the system's purple (the value the sibling apps use),
    /// and it is a DIFFERENT colour from the light one: an accent that ignored the
    /// appearance would pass the light test by being dark everywhere. Mutation-proof:
    /// return `accentLight` for both appearances and the first assertion fails.
    func testTheAccentIsSystemPurpleInDarkAppearance() throws {
        let dark = try accent(in: .darkAqua)
        let darkAqua = try XCTUnwrap(NSAppearance(named: .darkAqua))
        var system: NSColor?
        darkAqua.performAsCurrentDrawingAppearance {
            system = NSColor.systemPurple.usingColorSpace(.sRGB)
        }
        XCTAssertEqual(dark, try XCTUnwrap(system))
        XCTAssertNotEqual(dark, try accent(in: .aqua), "light and dark resolve differently")
        XCTAssertGreaterThanOrEqual(contrast(dark, grey(0x1E)), 4.5, "on a dark card")
    }
}
