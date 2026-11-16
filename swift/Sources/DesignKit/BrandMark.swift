// The mark, in ONE colour: the skull under its wail, for places inside the app where
// the full-colour icon would shout (`banshee-v54.4`).
//
// The icon's wail is the severity scale — green to red — and the brand rule is that
// the scale is only ever used WHOLE. A single-colour mark sidesteps the question: it
// carries the shape and none of the severity, so it can sit beside "No alert episodes
// recorded." without saying anything about how the machine is doing. It takes the
// foreground style of wherever it is placed; callers make it faint, not this file.
//
// Drawn, not an image: it scales to any size, follows light and dark for free, and
// there is no asset to fall out of step with the icon's SVG. The proportions are the
// icon's (assets/AppIcon.svg), simplified the way a small size needs: two arcs
// rather than three, no nose, no teeth.

import SwiftUI

public struct BrandMark: View {
    private let size: CGFloat

    /// `size` is the side of the square the mark is drawn in, in points.
    public init(size: CGFloat) {
        self.size = size
    }

    public var body: some View {
        ZStack {
            WailArcs()
                .stroke(style: StrokeStyle(lineWidth: size * 0.045, lineCap: .round))
            // The sockets are cut OUT of the shape, so they are holes the background
            // shows through rather than a second colour this mark would have to choose.
            SkullShape()
        }
        .frame(width: size, height: size)
        // Decoration beside text that already says everything.
        .accessibilityHidden(true)
    }
}

/// All coordinates below are in a 100 × 100 design square, scaled to the rect.
private func unit(_ rect: CGRect) -> CGAffineTransform {
    CGAffineTransform(translationX: rect.minX, y: rect.minY)
        .scaledBy(x: rect.width / 100, y: rect.height / 100)
}

/// The two arcs of the wail, open at the bottom, over the skull.
struct WailArcs: Shape {
    func path(in rect: CGRect) -> Path {
        var path = Path()
        let centre = CGPoint(x: 50, y: 54)
        for radius in [31.0, 22.0] {
            path.move(to: CGPoint(x: centre.x - radius, y: centre.y))
            // SwiftUI's y axis points down, so 180° → 360° passes through the top.
            path.addArc(
                center: centre, radius: radius,
                startAngle: .degrees(180), endAngle: .degrees(360), clockwise: false)
        }
        return path.applying(unit(rect))
    }
}

/// The skull: a round cranium joined to a jaw, with two sockets cut out of it.
///
/// Built with path UNION and SUBTRACTION, not by adding four subpaths and filling
/// even-odd. The first version did that, and even-odd turned the strip where the
/// jaw overlaps the cranium into a hole as well — a gap across the skull — while
/// `Path.contains(_:eoFill:)` reported the sockets as holes whether or not they
/// had been drawn, so the test written to pin them passed with a socket deleted.
/// The mutation replay caught the false pin; this construction is what it led to.
struct SkullShape: Shape {
    func path(in rect: CGRect) -> Path {
        let cranium = Path(ellipseIn: CGRect(x: 36, y: 44, width: 28, height: 28))
        let jaw = Path(
            roundedRect: CGRect(x: 42, y: 64, width: 16, height: 12), cornerSize: CGSize(width: 3, height: 3))
        let leftSocket = Path(ellipseIn: CGRect(x: 40.5, y: 54, width: 8, height: 8))
        let rightSocket = Path(ellipseIn: CGRect(x: 51.5, y: 54, width: 8, height: 8))
        return cranium.union(jaw)
            .subtracting(leftSocket)
            .subtracting(rightSocket)
            .applying(unit(rect))
    }
}
