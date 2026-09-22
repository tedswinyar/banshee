// Settings. Three preferences, all of which change behaviour immediately — a
// preference that needs a relaunch is a preference people set wrong once and never
// revisit.

import SwiftUI
import BansheeCore
import DesignKit
import UserNotifications
import ServiceManagement

struct SettingsView: View {
    let preferences: Preferences
    /// Mirrors of the persisted values, so the controls are bindable. `Preferences`
    /// is a value type over `UserDefaults`; the pickers write through on change.
    @State private var dockPresence: DockPresence = .menuBarOnly
    @State private var notificationsEnabled = true
    @State private var openAtLogin = false
    @State private var loginItemNeedsApproval = false
    @State private var loginItemError: String?

    var body: some View {
        Form {
            Section {
                Picker("Show in", selection: $dockPresence) {
                    ForEach(DockPresence.allCases, id: \.self) { presence in
                        Text(presence.label).tag(presence)
                    }
                }
                .onChange(of: dockPresence) { _, new in
                    preferences.dockPresence = new
                    // Applied NOW, not at next launch. This is the whole reason the
                    // preference is a runtime activation policy rather than
                    // LSUIElement in Info.plist.
                    NSApp.setActivationPolicy(new == .menuBarOnly ? .accessory : .regular)
                }
                Text("The menu bar glyph is always shown. A Dock icon is optional.")
                    .font(Typography.caption)
                    .foregroundStyle(Palette.textSecondary)
            }

            Section {
                Toggle("Notify at Wailing and above, and when the disk leaves green", isOn: $notificationsEnabled)
                    .onChange(of: notificationsEnabled) { _, new in
                        preferences.notificationsEnabled = new
                        if new { requestAuthorizationIfNeeded() }
                    }
                Text("Banners only for a red that has held past its grace period, at most one an hour per condition. The daemon records every incident as an episode either way — `banshee alerts` has the full history, including how many re-fires each one absorbed.")
                    .font(Typography.caption)
                    .foregroundStyle(Palette.textSecondary)
                    .fixedSize(horizontal: false, vertical: true)
            }

            Section {
                Toggle("Open at login", isOn: $openAtLogin)
                    .onChange(of: openAtLogin) { _, new in
                        loginItemError = nil
                        do {
                            try LoginItem.set(enabled: new)
                        } catch {
                            loginItemError = error.localizedDescription
                        }
                        // Re-read launchd's answer: register() can succeed and still
                        // leave the item awaiting approval in System Settings.
                        openAtLogin = LoginItem.isEnabled
                        loginItemNeedsApproval = LoginItem.requiresApproval
                    }
                if loginItemNeedsApproval {
                    HStack(spacing: Spacing.sm) {
                        Text("Login Items is waiting for your approval in System Settings.")
                            .font(Typography.caption)
                            .foregroundStyle(Palette.textSecondary)
                        Button("Open Login Items") { SMAppService.openSystemSettingsLoginItems() }
                            .font(Typography.caption)
                    }
                } else if let loginItemError {
                    Text(loginItemError)
                        .font(Typography.caption)
                        .foregroundStyle(Palette.textSecondary)
                } else {
                    Text("The menu bar glyph comes back after a restart. The daemon starts at login regardless — it is a background service this app only reads from.")
                        .font(Typography.caption)
                        .foregroundStyle(Palette.textSecondary)
                        .fixedSize(horizontal: false, vertical: true)
                }
            }

            Section {
                Text("Sampling runs as a background service and does not stop when you quit this app.")
                    .font(Typography.caption)
                    .foregroundStyle(Palette.textSecondary)
                    .fixedSize(horizontal: false, vertical: true)
                Text("make daemon-status")
                    .font(Typography.mono)
                    .textSelection(.enabled)
            }
        }
        .formStyle(.grouped)
        .frame(width: 420)
        .onAppear {
            // Read persisted values into the local mirrors once, rather than binding
            // straight through — a Picker bound to a computed property that writes to
            // UserDefaults re-reads on every render and fights itself.
            dockPresence = preferences.dockPresence
            notificationsEnabled = preferences.notificationsEnabled
            openAtLogin = LoginItem.isEnabled
            loginItemNeedsApproval = LoginItem.requiresApproval
        }
    }

    private func requestAuthorizationIfNeeded() {
        UNUserNotificationCenter.current()
            .requestAuthorization(options: [.alert, .sound]) { granted, _ in
                if !granted {
                    NSLog("banshee: notifications not authorized in System Settings")
                }
            }
    }
}
