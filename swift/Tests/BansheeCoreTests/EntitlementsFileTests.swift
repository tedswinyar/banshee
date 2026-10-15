import XCTest
@testable import BansheeCore

/// Pins the checked-in entitlements file against the interruption levels the
/// coordinator asks for (banshee-jie). The two live in different languages and
/// different directories, and only agree by discipline: `Interruption.timeSensitive`
/// is a request the system grants only when `Banshee.entitlements` carries the
/// time-sensitive key into the Developer ID seal. A test that read only the Swift
/// side would pass with the file deleted — and the banner would silently auto-dismiss.
final class EntitlementsFileTests: XCTestCase {
    static let timeSensitiveKey = "com.apple.developer.usernotifications.time-sensitive"
    static let criticalKey = "com.apple.developer.usernotifications.critical-alerts"

    /// swift/Tests/BansheeCoreTests/<this file> → repo root. `#filePath` is the real
    /// path in a debug (test) build; the release remap in build-app.sh never touches
    /// the test target.
    private func entitlements() throws -> [String: Any] {
        let root = URL(fileURLWithPath: #filePath)
            .deletingLastPathComponent()  // BansheeCoreTests
            .deletingLastPathComponent()  // Tests
            .deletingLastPathComponent()  // swift
            .deletingLastPathComponent()  // repo root
        let url = root.appendingPathComponent("Banshee.entitlements")
        let data = try Data(contentsOf: url)
        let plist = try PropertyListSerialization.propertyList(from: data, format: nil)
        return try XCTUnwrap(plist as? [String: Any], "Banshee.entitlements is not a dictionary plist")
    }

    func testTheEntitlementsFileGrantsTimeSensitiveNotifications() throws {
        let dict = try entitlements()
        XCTAssertEqual(dict[Self.timeSensitiveKey] as? Bool, true,
                       "a Shrieking or red-disk banner is only time-sensitive if the seal says so")
    }

    func testTheEntitlementsFileClaimsNothingElse() throws {
        // The identifier entitlements are composed at build time from the signing
        // identity (scripts/lib/entitlements.sh), never written here; and no
        // critical-alerts key, per the 2026-09-22 ruling that `Interruption` has no
        // `.critical` case to carry it.
        let dict = try entitlements()
        XCTAssertEqual(Array(dict.keys), [Self.timeSensitiveKey])
        XCTAssertNil(dict[Self.criticalKey])
    }

    /// The request side: the levels that ask for the capability are exactly the two
    /// the notes promise (Shrieking overall, red disk), and nothing lower does.
    @MainActor  // the coordinator is main-actor isolated
    func testOnlyShriekingAsksForTheCapability() {
        XCTAssertEqual(NotificationCoordinator.interruption(for: .shrieking), .timeSensitive)
        XCTAssertEqual(NotificationCoordinator.interruption(for: .wailing), .active)
        XCTAssertEqual(NotificationCoordinator.interruption(for: .restless), .active)
    }
}
