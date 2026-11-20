// The one-word difference between a visible menu bar and an empty one.

import XCTest
import AppKit
@testable import DesignKit

final class MenuBarGlyphTests: XCTestCase {
    /// **`isTemplate` must be false.** A template image is drawn as a silhouette so it
    /// can be tinted for light and dark menu bars — which erases the colour that IS
    /// the information here. 😴 and 💀 are the same shape in a mask.
    ///
    /// Mutation-proof, verified: set `isTemplate = true` and this fails. That mutation
    /// is the bug two earlier implementations shipped, and its only symptom was a
    /// hollow ring in the menu bar.
    func testTheGlyphImageIsNotATemplate() throws {
        let image = try XCTUnwrap(MenuBarGlyph.image(for: "💀🧠"))
        XCTAssertFalse(
            image.isTemplate,
            "a template image is drawn as a mask, so every level would look identical"
        )
    }

    /// A rendered glyph has real pixels. A zero-sized image collapses the status item
    /// to nothing, which looks exactly like the app having crashed.
    func testTheGlyphImageHasNonZeroSize() throws {
        for glyph in ["🫧", "😴", "😧🔥", "💀🧠", "😱🕸️"] {
            let image = try XCTUnwrap(MenuBarGlyph.image(for: glyph), glyph)
            XCTAssertGreaterThan(image.size.width, 0, glyph)
            XCTAssertGreaterThan(image.size.height, 0, glyph)
        }
    }

    /// A two-emoji glyph is WIDER than a one-emoji glyph, which is how the source
    /// suffix is visible at all. Mutation-proof: render only the first character and
    /// this fails.
    func testATwoEmojiGlyphIsWiderThanOne() throws {
        let one = try XCTUnwrap(MenuBarGlyph.image(for: "💀"))
        let two = try XCTUnwrap(MenuBarGlyph.image(for: "💀🧠"))
        XCTAssertGreaterThan(
            two.size.width, one.size.width,
            "the source suffix must occupy space, or it is not being drawn"
        )
    }

    /// Nothing to draw means nil, never a zero-sized image — that would silently empty
    /// the menu bar, and the caller keeps the previous glyph instead.
    ///
    /// Mutation-proof, verified: remove the `size.width > 0` guard and this fails with
    /// a zero-sized image. An earlier `guard !glyph.isEmpty` was ALSO here and a
    /// mutation showed it was dead code — an empty string measures to zero, so the size
    /// guard already covered it. It was removed rather than left looking load-bearing.
    func testNothingToDrawYieldsNil() {
        XCTAssertNil(MenuBarGlyph.image(for: ""))
        XCTAssertNil(MenuBarGlyph.image(for: "\u{200B}"), "a zero-width space draws nothing")
    }

    /// The update badge draws REAL pixels (banshee-xpu). Compared as rendered bytes,
    /// because the badge is the whole gentle reminder in the menu bar and an
    /// invisible reminder is the failure this file exists to catch.
    ///
    /// The byte comparison alone became a FALSE PIN when the knockout ring landed
    /// (banshee-v54.8): the ring's `.clear` fill changes the bytes by itself, so a
    /// badge that punched its hole and never filled the dot still "rendered
    /// differently". Two properties co-varied (the ring changes pixels, the dot
    /// changes pixels) and the fixture could not tell them apart. So the dot is
    /// asserted directly: its centre sits inside the cleared hole, and only the
    /// dot's own `fill()` can make that point opaque. Mutation-proof: remove the
    /// dot's `fill()` and the centre is transparent
    /// (`.mutations/menubar-glyph-badge-draws.mut`).
    func testABadgedGlyphRendersDifferentlyFromAnUnbadgedOne() throws {
        let plain = try XCTUnwrap(MenuBarGlyph.image(for: "😴"))
        let badged = try XCTUnwrap(MenuBarGlyph.image(for: "😴", badged: true))
        XCTAssertNotEqual(
            plain.tiffRepresentation, badged.tiffRepresentation,
            "the badge must change what is drawn, or it is not a reminder")
        let r = MenuBarGlyph.badgeDiameter / 2
        XCTAssertGreaterThan(
            alpha(try bitmap(badged), x: badged.size.width - r, y: badged.size.height - r), 0.9,
            "the dot itself must be drawn: a hole with nothing in it is not a reminder")
    }

    // ---- the knockout ring (banshee-v54.8) -----------------------------------

    /// The image's pixels as RGBA, with the scale between points and pixels.
    private func bitmap(_ image: NSImage) throws -> (rep: NSBitmapImageRep, scale: CGFloat) {
        let data = try XCTUnwrap(image.tiffRepresentation)
        let rep = try XCTUnwrap(NSBitmapImageRep(data: data))
        return (rep, CGFloat(rep.pixelsWide) / image.size.width)
    }

    /// Alpha at a point given in the image's own coordinates (origin bottom-left,
    /// as the drawing code uses); the bitmap's rows run top-down.
    private func alpha(_ b: (rep: NSBitmapImageRep, scale: CGFloat), x: CGFloat, y: CGFloat) -> CGFloat {
        let px = Int((x * b.scale).rounded(.down))
        let py = b.rep.pixelsHigh - 1 - Int((y * b.scale).rounded(.down))
        guard px >= 0, py >= 0, px < b.rep.pixelsWide, py < b.rep.pixelsHigh else { return 0 }
        return b.rep.colorAt(x: px, y: py)?.alphaComponent ?? 0
    }

    /// Points on the middle of the ring (between the dot's edge and the ring's
    /// outer edge), every 10 degrees, that fall inside the image.
    private func ringPoints(in size: NSSize) -> [(x: CGFloat, y: CGFloat)] {
        let r = MenuBarGlyph.badgeDiameter / 2
        let cx = size.width - r, cy = size.height - r
        let radius = r + MenuBarGlyph.badgeRing / 2
        return stride(from: 0.0, to: 360.0, by: 10.0).compactMap { degrees in
            let a = degrees * .pi / 180
            let x = cx + radius * CGFloat(cos(a)), y = cy + radius * CGFloat(sin(a))
            return (x >= 0.5 && y >= 0.5 && x <= size.width - 0.5 && y <= size.height - 0.5) ? (x, y) : nil
        }
    }

    /// The ring is TRANSPARENT where the emoji used to be. 🟥 is the glyph on
    /// purpose: it is opaque right into its corner, so there are ring points with
    /// real emoji under them. A round face would leave that corner empty and this
    /// would pass with no ring at all — which the first assertion rules out.
    /// Mutation-proof: delete the `.clear` fill and the covered points stay opaque.
    func testTheBadgeRingIsKnockedOutOfTheEmoji() throws {
        let plain = try bitmap(try XCTUnwrap(MenuBarGlyph.image(for: "🟥")))
        let badgedImage = try XCTUnwrap(MenuBarGlyph.image(for: "🟥", badged: true))
        let badged = try bitmap(badgedImage)

        let covered = ringPoints(in: badgedImage.size).filter { alpha(plain, x: $0.x, y: $0.y) > 0.9 }
        XCTAssertGreaterThanOrEqual(
            covered.count, 3, "the fixture must have emoji under the ring, or this proves nothing")
        let stillOpaque = covered.filter { alpha(badged, x: $0.x, y: $0.y) > 0.2 }
        XCTAssertEqual(stillOpaque.count, 0, "ring points left with emoji under them: \(stillOpaque)")
    }

    /// And the dot itself is still drawn, opaque, in the hole: clearing after the
    /// dot instead of before would erase the badge. Mutation-proof: move the dot's
    /// fill above the `.clear` block and the centre is transparent.
    func testTheBadgeDotSurvivesItsOwnRing() throws {
        let image = try XCTUnwrap(MenuBarGlyph.image(for: "🟥", badged: true))
        let r = MenuBarGlyph.badgeDiameter / 2
        XCTAssertGreaterThan(alpha(try bitmap(image), x: image.size.width - r, y: image.size.height - r), 0.9)
    }

    /// The badge is an overlay: the image keeps its size, so the status item does not
    /// jump when a reminder appears or clears.
    func testTheBadgeDoesNotChangeTheImageSize() throws {
        let plain = try XCTUnwrap(MenuBarGlyph.image(for: "💀🧠"))
        let badged = try XCTUnwrap(MenuBarGlyph.image(for: "💀🧠", badged: true))
        XCTAssertEqual(plain.size, badged.size)
        XCTAssertFalse(badged.isTemplate, "a template badge would be a grey mask too")
    }

    /// The point size has to leave room in a 22pt menu bar for two emoji.
    func testThePointSizeFitsTheMenuBar() {
        XCTAssertLessThan(MenuBarGlyph.pointSize, 22)
        XCTAssertGreaterThan(MenuBarGlyph.pointSize, 10)
    }
}
