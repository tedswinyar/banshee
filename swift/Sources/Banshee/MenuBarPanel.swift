// The popover behind the menu bar glyph.
//
// Shape rule from the plan: **one action per row, and everything else opens the
// window.** A popover that grows into a dashboard is a dashboard you cannot resize,
// cannot scroll comfortably, and which dismisses itself when you click the thing you
// were reading. So this shows the verdict, the dimensions that are out of band, and
// the single best action — and hands off.

import SwiftUI
import BansheeCore
import DesignKit

struct MenuBarPanel: View {
    @Environment(PressureModel.self) private var model
    @Environment(\.openWindow) private var openWindow
    @Environment(\.openSettings) private var openSettings

    var body: some View {
        VStack(alignment: .leading, spacing: Spacing.sm) {
            header

            if case .failed(let message) = model.connectionState, model.pressure == nil {
                daemonMissing(message)
            } else {
                // Locked out, or unreachable past `staleAfter`: say so ABOVE the verdict,
                // because the verdict below is the last one this app saw, not the
                // machine now. The menu bar has already stopped presenting it as
                // current (`banshee-6ds`); the popover must not quietly disagree.
                if let notice = model.connectionNotice {
                    connectionNotice(notice)
                }
                if let pressure = model.pressure {
                    if pressure.level == .checking {
                        checking
                    } else {
                        concerning(pressure)
                        if let action = pressure.findings.first(where: { $0.action != .none }) {
                            Divider()
                            bestAction(action)
                        }
                    }
                } else if model.connectionNotice == nil {
                    Text("Connecting…")
                        .font(Typography.caption)
                        .foregroundStyle(Palette.textSecondary)
                }
            }

            Divider()
            footer
        }
        .padding(Spacing.md)
        .frame(width: 320)
        // Poll FASTER while the popover is open, and stop when it closes. Someone
        // watching a number expects it to move; 15 seconds of stillness reads as
        // broken. `panelOpened` also refreshes immediately, so the popover never
        // opens showing a 14-second-old reading and then jumps.
        .task {
            await model.panelOpened()
        }
        .onDisappear {
            model.panelClosed()
        }
    }

    private var header: some View {
        HStack(alignment: .firstTextBaseline, spacing: Spacing.sm) {
            // Rendered verbatim from the wire, like the menu bar label itself.
            Text(model.menuBarGlyph)
                .font(.title)
                .accessibilityHidden(true) // the label below carries it in words
            VStack(alignment: .leading, spacing: 0) {
                // The model pairs this with the glyph, so a lock never sits beside a
                // level name the daemon has stopped vouching for.
                Text(model.menuBarTitle)
                    .font(Typography.rowTitle)
                if let subtitle {
                    Text(subtitle)
                        .font(Typography.caption)
                        .foregroundStyle(Palette.textSecondary)
                }
            }
            Spacer()
        }
        .accessibilityElement(children: .combine)
        .accessibilityLabel(model.menuBarAccessibilityLabel)
    }

    private var subtitle: String? {
        guard let pressure = model.pressure else { return nil }
        guard let refreshed = model.lastRefresh else { return nil }
        let count = pressure.concerning.count
        let state = count == 0 ? "nothing out of band" : "\(count) out of band"
        return "\(state) · \(refreshed.formatted(.relative(presentation: .named)))"
    }

    /// `checking` gets its own copy. An empty dimension list would read as "nothing
    /// wrong", which is the one thing this app must never imply about a machine it
    /// has not measured.
    private var checking: some View {
        Text("Not enough readings yet. This is not a claim that the machine is calm.")
            .font(Typography.caption)
            .foregroundStyle(Palette.textSecondary)
            .fixedSize(horizontal: false, vertical: true)
    }

    private func concerning(_ pressure: Pressure) -> some View {
        VStack(alignment: .leading, spacing: Spacing.xs) {
            if pressure.concerning.isEmpty {
                Text("Nothing is out of band.")
                    .font(Typography.caption)
                    .foregroundStyle(Palette.textSecondary)
            } else {
                ForEach(pressure.concerning) { reading in
                    HStack(spacing: Spacing.sm) {
                        Circle()
                            .fill(DimensionRow.color(for: reading.band))
                            .frame(width: Size.statusDot, height: Size.statusDot)
                            .accessibilityHidden(true)
                        Text(reading.label)
                            .font(Typography.caption)
                        Spacer(minLength: Spacing.xs)
                        Text(reading.detail)
                            .font(Typography.caption)
                            .foregroundStyle(Palette.textSecondary)
                            .lineLimit(1)
                    }
                    .accessibilityElement(children: .combine)
                    .accessibilityLabel("\(reading.label), \(reading.band.rawValue): \(reading.detail)")
                }
            }
        }
    }

    /// ONE action. The findings are already ranked by measured impact in core, so
    /// "the first one with an action" is the best thing to do — no local sorting.
    private func bestAction(_ finding: Finding) -> some View {
        VStack(alignment: .leading, spacing: Spacing.xs) {
            Text(finding.message)
                .font(Typography.caption)
                .fixedSize(horizontal: false, vertical: true)
            // The who-line IS the finding's point: during the 2026-09-15 crisis
            // the panel offered one action and named no culprit, so the person
            // had a button and no reason to believe it (banshee-rad). One caption
            // line, verbatim from the wire like every other surface.
            if let whoLine = finding.whoLine {
                Text(whoLine)
                    .font(Typography.caption)
                    .foregroundStyle(Palette.textSecondary)
                    .fixedSize(horizontal: false, vertical: true)
            }
            Text(finding.actionLabel)
                .font(Typography.caption)
                .foregroundStyle(Palette.accent)
        }
        .accessibilityElement(children: .combine)
        .accessibilityLabel(
            "Suggested: \(finding.actionLabel). \(finding.message)"
                + (finding.whoLine.map { " \($0)" } ?? "")
        )
    }

    /// The glyph is not the verdict's, and this says why. Two states reach here: the
    /// daemon refused this app's key (it IS still watching — the opposite of
    /// `daemonMissing`), or it has not answered for longer than
    /// `PressureModel.staleAfter`. Either way the verdict below, if there is one, is
    /// the last accepted reading and is dated by the header's subtitle.
    private func connectionNotice(_ message: String) -> some View {
        VStack(alignment: .leading, spacing: Spacing.xs) {
            Text(lockedOut ? "Banshee is watching — this app is locked out" : "Nothing has answered")
                .font(Typography.rowTitle)
                .foregroundStyle(lockedOut ? Palette.warning : Palette.error)
            Text(message)
                .font(Typography.caption)
                .textSelection(.enabled)
                .fixedSize(horizontal: false, vertical: true)
            if model.pressure != nil {
                Text("Below: the last verdict this app was given.")
                    .font(Typography.caption)
                    .foregroundStyle(Palette.textSecondary)
            }
            Button("Try again") {
                Task { await model.refresh() }
            }
        }
    }

    private var lockedOut: Bool {
        if case .unauthorized = model.connectionState { return true }
        return false
    }

    /// The daemon-not-running state, first-class per ADR-0004. The app does not start
    /// one — that would put a second writer on the database — so the honest content
    /// is the command to run.
    private func daemonMissing(_ message: String) -> some View {
        VStack(alignment: .leading, spacing: Spacing.xs) {
            Text("Nothing is watching")
                .font(Typography.rowTitle)
                .foregroundStyle(Palette.error)
            Text(message)
                .font(Typography.mono)
                .textSelection(.enabled)
                .fixedSize(horizontal: false, vertical: true)
            Button("Try again") {
                Task { await model.connect() }
            }
        }
    }

    private var footer: some View {
        VStack(alignment: .leading, spacing: Spacing.xs) {
            Button("Open Banshee") {
                openWindow(id: WindowID.detail)
            }
            .keyboardShortcut("o")
            Button("Settings…") {
                openSettings()
            }
            .keyboardShortcut(",")
            Button("Quit Banshee") {
                NSApplication.shared.terminate(nil)
            }
            .keyboardShortcut("q")
        }
        .buttonStyle(.plain)
        .font(Typography.caption)
    }
}
