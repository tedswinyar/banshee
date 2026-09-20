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
    /// invisible reminder is the failure this file exists to catch. Mutation-proof:
    /// remove the `fill()` and the two renderings are identical.
    func testABadgedGlyphRendersDifferentlyFromAnUnbadgedOne() throws {
        let plain = try XCTUnwrap(MenuBarGlyph.image(for: "😴"))
        let badged = try XCTUnwrap(MenuBarGlyph.image(for: "😴", badged: true))
        XCTAssertNotEqual(
            plain.tiffRepresentation, badged.tiffRepresentation,
            "the badge must change what is drawn, or it is not a reminder")
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
