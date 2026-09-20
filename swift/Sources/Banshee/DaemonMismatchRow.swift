// The daemon and this app are different releases — say so, and offer the fix.
//
// A Sparkle update replaces the app and leaves the LaunchAgent where it was; the
// lenient wire format means the pair never errors, the app just quietly shows less
// than the daemon could tell it (banshee-b25). The message names both versions,
// and the button is the app's own installer (DaemonInstaller, from the daemon in
// Contents/Helpers) — the same path a DMG user's first install takes, so an UPDATE
// is not a second, different mechanism. When the daemon is the newer half, the
// fix is the app's update instead, and the button says so.

import SwiftUI
import BansheeCore
import DesignKit

struct DaemonMismatchRow: View {
    @Environment(PressureModel.self) private var model
    let agreement: DaemonAgreement

    @State private var updating = false
    @State private var updateError: String?

    var body: some View {
        VStack(alignment: .leading, spacing: Spacing.xs) {
            Text("Daemon and app disagree")
                .font(Typography.rowTitle)
                .foregroundStyle(Palette.warning)
            if let message = agreement.message {
                Text(message)
                    .font(Typography.caption)
                    .fixedSize(horizontal: false, vertical: true)
            }
            if let updateError {
                Text(updateError)
                    .font(Typography.caption)
                    .foregroundStyle(Palette.error)
                    .textSelection(.enabled)
                    .fixedSize(horizontal: false, vertical: true)
            }
            switch agreement {
            case .daemonBehind:
                if let bundled = DaemonInstaller.bundledBinaries(in: Bundle.main.bundleURL) {
                    Button(updating ? "Updating the daemon…" : "Update the daemon") {
                        Task {
                            updating = true
                            updateError = nil
                            let result = await DaemonInstaller(environment: .live())
                                .install(apiBinary: bundled.api, cliBinary: bundled.cli)
                            if case let .failure(error) = result {
                                updateError = error.userMessage
                            }
                            // Re-probe either way: the connect path is what re-reads
                            // the daemon's version and clears this row.
                            await model.connect()
                            updating = false
                        }
                    }
                    .buttonStyle(.borderedProminent)
                    .disabled(updating)
                } else {
                    // A dev build (`swift run`) has no bundle to install from; the
                    // honest offer is the command.
                    Text("make daemon-install")
                        .font(Typography.mono)
                        .textSelection(.enabled)
                }
            case .appBehind:
                Button("Check for Updates…") {
                    Updater.shared.checkForUpdates()
                }
                .buttonStyle(.borderedProminent)
            case .agree:
                EmptyView()
            }
        }
        .accessibilityElement(children: .combine)
        .accessibilityLabel("Daemon and app disagree. \(agreement.message ?? "")")
    }
}
