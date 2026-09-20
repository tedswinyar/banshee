// DaemonAgreement pins: the comparison that decides whether a Sparkle update left
// the daemon behind (banshee-b25).

import XCTest
@testable import BansheeCore

final class DaemonAgreementTests: XCTestCase {
    /// The case that happened: app updated by Sparkle, daemon still the old release.
    func testADaemonOlderThanTheAppIsBehind() {
        XCTAssertEqual(
            DaemonAgreement.compare(daemonVersion: "0.1.4", appVersion: "0.1.5"),
            .daemonBehind(daemon: "0.1.4", app: "0.1.5"))
    }

    /// The other direction is a different message with a different fix, so the two
    /// must not collapse into one "mismatch". Mutation-proof: swap the `<`/`>`
    /// branches and this fails while the behind test above still passes.
    func testADaemonNewerThanTheAppIsAppBehind() {
        XCTAssertEqual(
            DaemonAgreement.compare(daemonVersion: "0.2.0", appVersion: "0.1.9"),
            .appBehind(daemon: "0.2.0", app: "0.1.9"))
    }

    func testTheSameReleaseAgrees() {
        XCTAssertEqual(DaemonAgreement.compare(daemonVersion: "0.1.5", appVersion: "0.1.5"), .agree)
    }

    /// Numeric, not lexical: "0.1.10" is newer than "0.1.9". A string comparison
    /// would call the daemon behind here and offer a downgrade.
    func testComponentsCompareNumerically() {
        XCTAssertEqual(
            DaemonAgreement.compare(daemonVersion: "0.1.10", appVersion: "0.1.9"),
            .appBehind(daemon: "0.1.10", app: "0.1.9"))
    }

    /// A dirty dev daemon on the same release agrees: the question is whether an
    /// install is owed, not whether the binaries are identical. Same for pre-release
    /// tags and a missing trailing component.
    func testBuildMetadataAndMissingComponentsDoNotMakeAMismatch() {
        XCTAssertEqual(DaemonAgreement.compare(daemonVersion: "0.1.5+dirty", appVersion: "0.1.5"), .agree)
        XCTAssertEqual(DaemonAgreement.compare(daemonVersion: "0.1.5", appVersion: "0.1.5-rc1"), .agree)
        XCTAssertEqual(DaemonAgreement.compare(daemonVersion: "0.2", appVersion: "0.2.0"), .agree)
    }

    /// The message names both versions — "out of date" without numbers cannot be
    /// checked against `/health` or `banshee --version`.
    func testTheMessageNamesBothVersions() throws {
        let behind = try XCTUnwrap(
            DaemonAgreement.compare(daemonVersion: "0.1.4", appVersion: "0.1.5").message)
        XCTAssertTrue(behind.contains("0.1.4") && behind.contains("0.1.5"), behind)
        XCTAssertNil(DaemonAgreement.agree.message)
    }
}
