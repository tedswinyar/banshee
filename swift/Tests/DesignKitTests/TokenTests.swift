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
}
