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

    // ---- the band dots (banshee-v54.3) ---------------------------------------

    private func resolved(_ color: NSColor, in name: NSAppearance.Name) throws -> NSColor {
        let appearance = try XCTUnwrap(NSAppearance(named: name))
        var out: NSColor?
        appearance.performAsCurrentDrawingAppearance { out = color.usingColorSpace(.sRGB) }
        return try XCTUnwrap(out)
    }

    private var bands: [(name: String, color: NSColor, system: NSColor)] {
        [
            ("green", Palette.bandGreenNSColor, .systemGreen),
            ("amber", Palette.bandAmberNSColor, .systemOrange),
            ("red", Palette.bandRedNSColor, .systemRed),
        ]
    }

    /// A status dot is a graphical object: 3:1 against what it sits on (WCAG
    /// 1.4.11). In light appearance that is the window ground and a white card.
    /// The system colours fail it there (about 1.9, 1.9 and 3.0), which is the
    /// defect this pins. Mutation-proof: make `band(dark:lightHex:)` return `dark`
    /// for both appearances and green and amber fail on both grounds.
    func testEveryBandDotClearsThreeToOneInLightAppearance() throws {
        for band in bands {
            let light = try resolved(band.color, in: .aqua)
            XCTAssertGreaterThanOrEqual(contrast(light, grey(0xEA)), 3.0, "\(band.name) on the light window")
            XCTAssertGreaterThanOrEqual(contrast(light, grey(0xFF)), 3.0, "\(band.name) on a white card")
        }
    }

    /// Dark appearance keeps the SYSTEM colours — they pass there, and they are
    /// what the icon's wail is drawn from. Without this the light test could be
    /// satisfied by using the dark shades everywhere, which would dim the dots on
    /// a dark window. Mutation-proof: return `light` for both appearances and the
    /// equality fails for all three.
    func testEveryBandDotIsTheSystemColourInDarkAppearance() throws {
        for band in bands {
            let dark = try resolved(band.color, in: .darkAqua)
            XCTAssertEqual(dark, try resolved(band.system, in: .darkAqua), band.name)
            XCTAssertGreaterThanOrEqual(contrast(dark, grey(0x1E)), 3.0, "\(band.name) on a dark card")
        }
    }

    /// Three bands, three colours, in each appearance: a light value pasted into
    /// the wrong band would still clear 3:1.
    func testTheThreeBandsAreDistinctInEachAppearance() throws {
        for name in [NSAppearance.Name.aqua, .darkAqua] {
            let colors = try bands.map { try resolved($0.color, in: name) }
            XCTAssertEqual(Set(colors).count, 3, "\(name.rawValue)")
        }
    }

    // ---- the one-colour mark (banshee-v54.4) ----------------------------------

    /// The skull has a cranium, a jaw, and two sockets that are HOLES — the mark is
    /// one colour, so the eyes can only be the background showing through. Checked
    /// on the shape's own geometry in its 100-point design square.
    ///
    /// The first version of this test asked `contains(_:eoFill: true)` of a
    /// four-subpath shape and passed with a socket deleted (the replay said so). It
    /// now asks a plainly filled shape, and also pins the strip where jaw meets
    /// cranium, which the even-odd fill had silently turned into a gap.
    /// Mutation-proof: drop `.subtracting(leftSocket)` and the left centre is filled.
    func testTheMarkSkullHasTwoSocketsCutOutOfIt() {
        let skull = SkullShape().path(in: CGRect(x: 0, y: 0, width: 100, height: 100))
        XCTAssertTrue(skull.contains(CGPoint(x: 50, y: 48)), "the cranium is filled")
        XCTAssertTrue(skull.contains(CGPoint(x: 50, y: 73)), "the jaw is filled")
        XCTAssertTrue(skull.contains(CGPoint(x: 50, y: 68)), "and so is where they overlap: one solid skull")
        XCTAssertFalse(skull.contains(CGPoint(x: 44.5, y: 58)), "the left socket is a hole")
        XCTAssertFalse(skull.contains(CGPoint(x: 55.5, y: 58)), "the right socket is a hole")
        XCTAssertFalse(skull.contains(CGPoint(x: 5, y: 5)), "and outside is outside")
    }

    /// The wail sits OVER the skull and is wider than it, which is what makes the
    /// shape read as the icon's and not as a face in a ring.
    func testTheMarkWailArchesOverTheSkull() {
        let square = CGRect(x: 0, y: 0, width: 100, height: 100)
        let wail = WailArcs().path(in: square).boundingRect
        let skull = SkullShape().path(in: square).boundingRect
        XCTAssertLessThan(wail.minY, skull.minY, "the wail starts above the skull")
        XCTAssertLessThan(wail.minX, skull.minX, "and reaches wider on the left")
        XCTAssertGreaterThan(wail.maxX, skull.maxX, "and on the right")
    }
}
