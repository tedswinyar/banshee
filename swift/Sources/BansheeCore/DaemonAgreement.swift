// DaemonAgreement — do the app and the daemon belong to the same release?
//
// The rule (banshee-ship-rules, 2026-09-13) is that daemon and app ship TOGETHER:
// `make install` does both halves, because the wire format is lenient and an app
// built before a daemon change never errors — it just quietly shows less than the
// CLI. Sparkle breaks that rule by design: it updates the app bundle and knows
// nothing about the LaunchAgent, so the first time it ran for real (0.1.4 → 0.1.5,
// 2026-09-20) the app came back new and the daemon stayed old, and the only sign
// was `/health` — which nobody reads (banshee-b25).
//
// This is the comparison, kept pure so it can be pinned. The versions compared are
// the marketing versions: the daemon's `CARGO_PKG_VERSION` from `/health` and the
// app's `Version.marketing`, which release.sh already forces to agree at tag time.

import Foundation

public enum DaemonAgreement: Equatable, Sendable {
    /// Same release. Nothing to say.
    case agree
    /// The daemon is older than this app — the Sparkle-update case. The fix is the
    /// app's own installer, from the daemon it carries in Contents/Helpers.
    case daemonBehind(daemon: String, app: String)
    /// The daemon is newer than this app: someone ran `make daemon-install` from a
    /// newer checkout, or an update was declined. The fix is updating the app.
    case appBehind(daemon: String, app: String)

    /// Compare two version strings numerically, component by component. Build
    /// metadata and pre-release tags (`0.1.5+dirty`, `0.1.6-rc1`) are dropped before
    /// comparing: a dirty dev daemon on 0.1.5 AGREES with a 0.1.5 app, because what
    /// this decides is whether the user is owed an install, not whether two binaries
    /// are bit-identical. Missing components read as zero, so `0.2` equals `0.2.0`.
    public static func compare(daemonVersion: String, appVersion: String) -> DaemonAgreement {
        let d = numericComponents(daemonVersion)
        let a = numericComponents(appVersion)
        let width = max(d.count, a.count)
        for i in 0..<width {
            let dv = i < d.count ? d[i] : 0
            let av = i < a.count ? a[i] : 0
            if dv < av { return .daemonBehind(daemon: daemonVersion, app: appVersion) }
            if dv > av { return .appBehind(daemon: daemonVersion, app: appVersion) }
        }
        return .agree
    }

    /// One sentence for the UI, or nil when there is nothing to say. The message
    /// names both versions, because "out of date" without numbers is not checkable.
    public var message: String? {
        switch self {
        case .agree:
            return nil
        case .daemonBehind(let daemon, let app):
            return "The background monitor is still \(daemon); this app is \(app). "
                + "Until it is updated the app may show less than the daemon can tell it."
        case .appBehind(let daemon, let app):
            return "This app is \(app) but the background monitor is already \(daemon). "
                + "Update the app so the two agree."
        }
    }

    private static func numericComponents(_ version: String) -> [Int] {
        // Everything before the first `+` or `-` is the release; the rest is metadata.
        let release = version.split(whereSeparator: { $0 == "+" || $0 == "-" }).first.map(String.init) ?? ""
        return release.split(separator: ".").map { Int($0) ?? 0 }
    }
}
