import Foundation
import Security

/// What this RUNNING build is entitled to — read from the code signature, not from a
/// build flag, because the two can disagree (banshee-jie).
///
/// `UNNotificationInterruptionLevel.timeSensitive` is honoured only when the signed
/// app carries `com.apple.developer.usernotifications.time-sensitive`; otherwise
/// macOS presents the banner as `.active` with no error. Ad-hoc builds (`make app`,
/// a fresh clone) never carry it — an ad-hoc binary that claims it is killed at
/// launch — and a Developer ID build carries it only when `scripts/build-app.sh` was
/// given the provisioning profile that authorises it. So the one honest answer is
/// to ask the kernel what this process was granted, and say so where the user can
/// read it (the launch log, Settings).
enum Capabilities {
    static let timeSensitiveNotificationsKey = "com.apple.developer.usernotifications.time-sensitive"

    /// True iff the running code's entitlements grant time-sensitive notifications.
    static var timeSensitiveNotifications: Bool {
        guard let task = SecTaskCreateFromSelf(nil) else { return false }
        let value = SecTaskCopyValueForEntitlement(task, timeSensitiveNotificationsKey as CFString, nil)
        return (value as? Bool) == true
    }

    /// One line for the launch log and the Settings caption.
    static var timeSensitiveNotificationsSummary: String {
        timeSensitiveNotifications
            ? "This build can break through Focus: Shrieking and a red disk are delivered as time-sensitive."
            : "This build has no time-sensitive entitlement (ad-hoc, or signed without the provisioning profile): Shrieking and red-disk banners are shown as ordinary ones and auto-dismiss."
    }
}
