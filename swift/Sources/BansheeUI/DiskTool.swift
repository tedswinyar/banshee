// The hand-off ADR-0003 promises: Banshee never walks the filesystem to answer
// "what is taking the space" — that is a disk-usage tool's job. When such a
// tool is installed, the disk finding's action becomes a button that launches
// it; when it is not, the finding stays what it always was, the server's words
// as plain text.
//
// This is the ONLY place that names the disk tool's bundle id on the client. It
// reads NSWorkspace (a system boundary), so it is a thin seam over AppKit, not
// logic worth a unit test — the pin that matters is that the disk finding still
// ships action `openDiskTool` (core's `the_disk_finding_defers_to_disk_tool`).

import AppKit

enum DiskTool {
    /// The disk visualizer's bundle identifier — the ADR-0003 hand-off target,
    /// a sibling of `com.tedswinyar.banshee`.
    static let bundleID = "com.tedswinyar.phantom"

    /// Where the disk visualizer is installed, or `nil` if it is not. `nil` is
    /// the honest answer that makes the caller fall back to text — never a dead
    /// button.
    static var installedURL: URL? {
        NSWorkspace.shared.urlForApplication(withBundleIdentifier: bundleID)
    }

    /// Launch the disk visualizer if it is installed. A no-op (returning
    /// `false`) when it is not, so the caller can decide the affordance BEFORE
    /// offering a button.
    @discardableResult
    static func launch() -> Bool {
        guard let url = installedURL else { return false }
        NSWorkspace.shared.openApplication(at: url, configuration: .init())
        return true
    }
}
