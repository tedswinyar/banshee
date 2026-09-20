// Rendering an emoji into something a status item will actually draw.
//
// This exists because two earlier attempts rendered NOTHING a person could see:
//
//  1. `MenuBarExtra { } label: { Text(glyph) }` — SwiftUI turns a MenuBarExtra label
//     into an image for the status item, and the emoji did not survive it.
//  2. `NSStatusItem`'s `button.title = glyph` — drawn as a MASK, which turned 💀🧠
//     into a single hollow ring. Confirmed by screenshot, next to another app's
//     status item rendering 👻 in full colour, which is what proved it was our
//     drawing and not the system.
//
// The fix is the last line of `image(for:)`: **`isTemplate = false`**. A template
// image is drawn as a silhouette so it can be tinted for light/dark menu bars, and
// that is exactly wrong for an emoji whose colour IS the information — 😴 and 💀 are
// the same shape in a mask.
//
// Lives in DesignKit because it is presentation, and because DesignKit has a test
// target: `isTemplate` is a one-word difference between working and invisible, and it
// needs a pin rather than a comment.

import AppKit

public enum MenuBarGlyph {
    /// Point size for the rendered glyph. Slightly under the 22pt menu bar height so
    /// two emoji sit on the baseline without touching the bar's edges.
    public static let pointSize: CGFloat = 15

    /// The update badge: a small filled dot in the glyph's top-right corner, drawn
    /// when a scheduled Sparkle check has found a newer version (banshee-xpu). Purple
    /// is the accent (`Palette.accent`), spelled as an `NSColor` because this image
    /// is drawn with AppKit. Sized to read as a badge, not as a third emoji.
    public static let badgeDiameter: CGFloat = 6
    public static let badgeColor = NSColor.systemPurple

    /// Render `glyph` as a full-colour image suitable for `NSStatusItem.button.image`.
    /// `badged` overlays the update dot; it never changes the image's size, so the
    /// status item does not jump when a reminder appears or clears.
    ///
    /// Returns nil rather than a zero-sized image whenever there is nothing to draw —
    /// an empty string, or anything else that measures to no pixels. A zero-sized
    /// image on a status item collapses it to nothing, which is the failure mode this
    /// whole file exists to stop and which looks identical to a crash.
    ///
    /// One guard, not two: an explicit `glyph.isEmpty` check was here and a mutation
    /// proved it dead — an empty string measures to zero, so the size guard below
    /// already returns nil for it. Two guards where one suffices is two things to keep
    /// true.
    public static func image(for glyph: String, badged: Bool = false) -> NSImage? {
        let attributed = NSAttributedString(
            string: glyph,
            attributes: [.font: NSFont.systemFont(ofSize: pointSize)]
        )
        var size = attributed.size()
        size.width = ceil(size.width)
        size.height = ceil(size.height)
        guard size.width > 0, size.height > 0 else { return nil }

        let image = NSImage(size: size)
        image.lockFocus()
        attributed.draw(at: .zero)
        if badged {
            let dot = NSRect(
                x: size.width - badgeDiameter, y: size.height - badgeDiameter,
                width: badgeDiameter, height: badgeDiameter)
            badgeColor.setFill()
            NSBezierPath(ovalIn: dot).fill()
        }
        image.unlockFocus()

        // THE line. Template = drawn as a mask = every level looks the same.
        image.isTemplate = false
        return image
    }
}
