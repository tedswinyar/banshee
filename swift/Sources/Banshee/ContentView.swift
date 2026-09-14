// The verdict window: glyph, level, the dimensions that are out of band, and the
// ranked worklist.
//
// This was the first share of the UI — enough to see the verdict the API serves.
// The menu bar item, the sparkline history, and the actions that DO something
// arrived later.
//
// House rules held here: DesignKit tokens only (raw point values and color
// literals are review findings), state via the model, errors always surfaced,
// accessibility wired — custom rows get a composed label and decorative glyphs are
// hidden.

import SwiftUI
import DesignKit
import BansheeCore

struct ContentView: View {
    @Environment(PressureModel.self) private var model
    /// Show every dimension, not just the ones out of band.
    @State private var showAll = false
    /// A repair is in flight; both buttons disable so it cannot be double-run.
    @State private var repairing = false
    @State private var installing = false
    @State private var installError: String?

    var body: some View {
        Group {
            switch model.connectionState {
            case .connecting where model.pressure == nil:
                ProgressView("Summoning the sentinel…")
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
            case .failed(let message) where model.pressure == nil:
                connectionError(message)
            case .unauthorized(let message) where model.pressure == nil:
                lockedOut(message)
            default:
                // A failure with a verdict still in hand shows the verdict AND the
                // error: a 15-second-old reading beats a blank pane.
                verdict
            }
        }
        // minWidth/minHeight are floors, not fixed sizes — content taller than
        // the floor (e.g. at large Dynamic Type sizes) grows the window rather
        // than clipping.
        .frame(minWidth: 520, minHeight: 420)
        .navigationTitle("Banshee")
        .toolbar {
            ToolbarItem(placement: .primaryAction) {
                Toggle(isOn: $showAll) {
                    Label("All dimensions", systemImage: "list.bullet")
                }
                .help("Show every dimension, including the ones that are fine")
            }
            ToolbarItem {
                Button {
                    Task { await model.refresh() }
                } label: {
                    Label("Refresh", systemImage: "arrow.clockwise")
                }
                .help("Re-read the verdict now")
            }
        }
    }

    private var verdict: some View {
        VStack(spacing: 0) {
            if let error = model.lastError {
                errorBanner(error)
            }
            if let pressure = model.pressure {
                ScrollView {
                    VStack(alignment: .leading, spacing: Spacing.lg) {
                        VerdictHeader(pressure: pressure, lastRefresh: model.lastRefresh)
                        if pressure.level == .checking {
                            checkingNotice
                        } else {
                            DimensionList(pressure: pressure, showAll: showAll)
                            if !pressure.findings.isEmpty {
                                FindingList(findings: pressure.findings) {
                                    Task { await model.refresh() }
                                }
                            }
                        }
                    }
                    .padding(Spacing.lg)
                    .frame(maxWidth: .infinity, alignment: .leading)
                }
                // Band changes and a growing worklist animate instead of snapping.
                .animation(.default, value: pressure)
            }
        }
    }

    /// The one thing this window must never imply is that a machine it has not
    /// measured is calm. So `checking` gets its own explicit copy rather than an
    /// empty dimension list, which would read as "nothing wrong".
    private var checkingNotice: some View {
        VStack(alignment: .leading, spacing: Spacing.sm) {
            Text("Not enough data yet.")
                .font(Typography.rowTitle)
            Text("The sentinel needs a few readings before it can judge. This is not a claim that the machine is calm.")
                .font(Typography.body)
                .foregroundStyle(Palette.textSecondary)
                .fixedSize(horizontal: false, vertical: true)
        }
    }

    private func errorBanner(_ message: String) -> some View {
        Text(message)
            .font(Typography.caption)
            .foregroundStyle(Palette.error)
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(Spacing.sm)
            .background(Palette.error.opacity(0.1))
            .overlay(alignment: .bottom) {
                Rectangle().fill(Palette.separator).frame(height: 1)
            }
    }

    /// The daemon-not-running state, which is FIRST-CLASS rather than an error to
    /// hide (ADR-0004). The app does not start the daemon — doing so would put a
    /// second writer on the database — so the only honest thing to show is what to
    /// run.
    ///
    /// Leading-aligned and monospaced, because the message contains a command to
    /// copy. Centred prose with a shell command in the middle of it is unreadable
    /// and un-copyable.
    /// The daemon is fine; this app's key is not (`banshee-dqh`), or was never sent
    /// because the daemon offered no socket (ADR-0008).
    ///
    /// A deliberately DIFFERENT heading from `connectionError`'s "Nothing is
    /// watching", because that sentence would be false here and falseness is the one
    /// thing a monitor cannot afford. The daemon is running, sampling and recording
    /// alerts throughout; only this client is locked out. The message names the
    /// repair, and the app keeps polling, so the state clears by itself once the key
    /// or the daemon is updated — no relaunch needed.
    private func lockedOut(_ message: String) -> some View {
        VStack(alignment: .leading, spacing: Spacing.md) {
            HStack(spacing: Spacing.sm) {
                Image(systemName: "lock.circle")
                    .font(.largeTitle)
                    .foregroundStyle(Palette.warning)
                    .accessibilityHidden(true) // decorative; the message carries it
                Text("Banshee is watching — this app is locked out")
                    .font(Typography.title)
            }
            Text(message)
                .font(Typography.mono)
                .textSelection(.enabled)
                .fixedSize(horizontal: false, vertical: true)
            Text("Banshee keeps checking; this clears on its own once the key or the daemon is updated.")
                .font(Typography.body)
                .foregroundStyle(Palette.textSecondary)
                .fixedSize(horizontal: false, vertical: true)
        }
        .padding(Spacing.lg)
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
        .accessibilityElement(children: .combine)
        .accessibilityLabel("Banshee is watching, but this app is locked out. \(message)")
    }

    private func connectionError(_ message: String) -> some View {
        VStack(alignment: .leading, spacing: Spacing.md) {
            HStack(spacing: Spacing.sm) {
                Image(systemName: "bolt.horizontal.circle")
                    .font(.largeTitle)
                    .foregroundStyle(Palette.error)
                    .accessibilityHidden(true) // decorative; the message carries it
                Text("Nothing is watching")
                    .font(Typography.title)
            }
            Text(message)
                .font(Typography.mono)
                .textSelection(.enabled)
                .fixedSize(horizontal: false, vertical: true)
            if let installError {
                Text(installError)
                    .font(Typography.mono)
                    .foregroundStyle(Palette.error)
                    .textSelection(.enabled)
                    .fixedSize(horizontal: false, vertical: true)
            }
            HStack(spacing: Spacing.sm) {
                // Nothing installed, but the app bundle ships the daemon
                // (Contents/Helpers): a DMG user has no source checkout, so
                // `make daemon-install` is not runnable advice — the button IS
                // the install. Installing registers launchd as the supervisor
                // and steps away (ADR-0004); the app stays a client. The
                // source-checkout advice in the message above remains for dev
                // builds, where there is no bundle to install from.
                if !DaemonService.isInstalled,
                    let bundled = DaemonInstaller.bundledBinaries(in: Bundle.main.bundleURL)
                {
                    Button(installing ? "Installing…" : "Install the daemon") {
                        Task {
                            installing = true
                            installError = nil
                            let result = await DaemonInstaller(environment: .live())
                                .install(apiBinary: bundled.api, cliBinary: bundled.cli)
                            if case let .failure(error) = result {
                                installError = error.userMessage
                            }
                            // Re-probe either way — same posture as repair():
                            // the connect path is what reports health.
                            await model.connect()
                            installing = false
                        }
                    }
                    .buttonStyle(.borderedProminent)
                    .disabled(installing)
                }
                // The button runs the same `launchctl kickstart` the advice above
                // names — never a different repair. Only offered when a plist
                // exists: with nothing installed there is nothing to kick, and
                // the honest offer is the install button (bundle) or the install
                // command in the message (source checkout).
                if DaemonService.isInstalled {
                    Button(repairing ? "Restarting…" : "Restart the daemon") {
                        Task {
                            repairing = true
                            _ = await DaemonService.shared.repair()
                            // Re-probe either way: launchd accepting the kick is
                            // not the same as the daemon serving, and the connect
                            // path is what reports which.
                            await model.connect()
                            repairing = false
                        }
                    }
                    .buttonStyle(.borderedProminent)
                    .disabled(repairing)
                }
                Button("Try again") {
                    Task { await model.connect() }
                }
                .buttonStyle(.bordered)
                .disabled(repairing || installing)
            }
        }
        .padding(Spacing.xl)
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .center)
        .accessibilityElement(children: .combine)
        .accessibilityLabel("Nothing is watching. \(message)")
    }
}

/// The glyph and the level, rendered VERBATIM from the wire (ADR-0005).
struct VerdictHeader: View {
    let pressure: Pressure
    let lastRefresh: Date?

    var body: some View {
        HStack(alignment: .firstTextBaseline, spacing: Spacing.md) {
            // The glyph is the whole product reduced to one character. It is
            // decorative HERE only because `accessibilityLabel` below carries the
            // same meaning in words — an emoji-only readout is invisible to
            // VoiceOver.
            Text(pressure.glyph)
                .font(.system(size: 44))
                .accessibilityHidden(true)
            VStack(alignment: .leading, spacing: Spacing.xs) {
                Text(pressure.levelName)
                    .font(Typography.title)
                Text(subtitle)
                    .font(Typography.caption)
                    .foregroundStyle(Palette.textSecondary)
            }
            Spacer()
        }
        .accessibilityElement(children: .combine)
        // The label core composed, spoken as-is.
        .accessibilityLabel(pressure.accessibilityLabel)
        .accessibilityValue(subtitle)
    }

    private var subtitle: String {
        var parts: [String] = []
        if pressure.level == .checking {
            parts.append("gathering readings")
        } else {
            let out = pressure.concerning.count
            parts.append(out == 0 ? "nothing out of band" : "\(out) out of band")
        }
        parts.append("\(pressure.sampleCount) samples")
        // The long memory, where people look (banshee-qoz): a calm verdict with a
        // busy day behind it must say so here, not only in the Alerts tab.
        let a = pressure.activity
        if a.open > 0 || a.lastDay > 0 {
            parts.append("\(a.open) open episode\(a.open == 1 ? "" : "s"), \(a.lastDay) in 24h")
        }
        if let lastRefresh {
            parts.append("read \(lastRefresh.formatted(.relative(presentation: .named)))")
        }
        return parts.joined(separator: " · ")
    }
}

struct DimensionList: View {
    let pressure: Pressure
    let showAll: Bool

    private var rows: [DimensionReading] {
        showAll ? pressure.dimensions : pressure.concerning
    }

    var body: some View {
        VStack(alignment: .leading, spacing: Spacing.sm) {
            Text(showAll ? "Every dimension" : "Out of band")
                .font(Typography.rowTitle)
            if rows.isEmpty {
                Text("Nothing is out of band.")
                    .font(Typography.body)
                    .foregroundStyle(Palette.textSecondary)
            } else {
                ForEach(rows) { reading in
                    DimensionRow(reading: reading)
                }
            }
        }
    }
}

struct DimensionRow: View {
    let reading: DimensionReading

    var body: some View {
        HStack(alignment: .firstTextBaseline, spacing: Spacing.sm) {
            // The dot is COLOR-ONLY and therefore decorative; the composed
            // accessibility label below names the band in words (a11y C1).
            Circle()
                .fill(Self.color(for: reading.band))
                .frame(width: Size.statusDot, height: Size.statusDot)
                .accessibilityHidden(true)
            VStack(alignment: .leading, spacing: Spacing.xs) {
                Text(reading.label)
                    .font(Typography.rowTitle)
                Text(reading.detail)
                    .font(Typography.caption)
                    .foregroundStyle(Palette.textSecondary)
            }
            Spacer(minLength: Spacing.sm)
            // minWidth pins the column's left edge so it doesn't jitter with
            // content length or Dynamic Type.
            Text(heldLabel)
                .font(Typography.caption)
                .foregroundStyle(Palette.textSecondary)
                .frame(minWidth: Size.metaColumn, alignment: .trailing)
        }
        .padding(.vertical, Spacing.xs)
        .accessibilityElement(children: .combine)
        .accessibilityLabel("\(reading.label): \(reading.band.rawValue)")
        .accessibilityValue(accessibilityValue)
    }

    private var heldLabel: String {
        // A recovering green was red a moment ago and its episode is not over
        // (ADR-0009); that is worth the column more than a held time is.
        if reading.recovering { return "recovering" }
        guard reading.band > .green else { return "" }
        return "held \(Self.duration(reading.heldSecs))"
    }

    private var accessibilityValue: String {
        var parts = [reading.detail]
        if reading.band > .green {
            parts.append("held for \(Self.duration(reading.heldSecs))")
        }
        if reading.recovering {
            parts.append("recovering, episode still open")
        }
        // A pending band is the honest "about to change" signal; hiding it makes
        // hysteresis look like lag.
        if let pending = reading.pending {
            parts.append("moving to \(pending.rawValue)")
        }
        if reading.advisory {
            parts.append("advisory only")
        }
        return parts.joined(separator: ", ")
    }

    /// Which colour ROLE a band wears. Exhaustive on purpose — no `default:` arm,
    /// so a band added to the wire format is a compile error here rather than a
    /// dot that silently renders in the wrong colour.
    static func color(for band: Band) -> Color {
        switch band {
        case .green: return Palette.bandGreen
        case .yellow: return Palette.bandYellow
        case .red: return Palette.bandRed
        }
    }

    static func duration(_ secs: UInt64) -> String {
        if secs < 60 { return "\(secs)s" }
        if secs < 3600 { return "\(secs / 60)m" }
        if secs < 86_400 { return "\(secs / 3600)h" }
        return "\(secs / 86_400)d"
    }
}

/// The worklist, in the order core ranked it — by MEASURED impact, not by
/// severity. Re-sorting here would throw away the one thing the ranking knows.
struct FindingList: View {
    let findings: [Finding]
    /// Called after a reap executes, so the verdict can be re-read rather than
    /// waiting for the next poll tick.
    var onReaped: () -> Void = {}

    /// The reap sheet currently open, if any. A reap finding opens it on the
    /// PREVIEW; nothing is killed until the person confirms inside it.
    @State private var activeReap: ActiveReap?

    var body: some View {
        VStack(alignment: .leading, spacing: Spacing.sm) {
            Text("Findings, best action first")
                .font(Typography.rowTitle)
            ForEach(Array(findings.enumerated()), id: \.element.id) { index, finding in
                VStack(alignment: .leading, spacing: Spacing.xs) {
                    Text("\(index + 1). \(finding.message)")
                        .font(Typography.body)
                        .fixedSize(horizontal: false, vertical: true)
                    // Who is behind it: core's line, verbatim. Nil when
                    // the finding named nobody, so nothing renders rather than a
                    // bare "who:".
                    if let who = finding.whoLine {
                        Text(who)
                            .font(Typography.caption)
                            .foregroundStyle(Palette.textSecondary)
                            .fixedSize(horizontal: false, vertical: true)
                    }
                    actionAffordance(for: finding)
                }
                .frame(maxWidth: .infinity, alignment: .leading)
                .padding(Spacing.sm)
                .background(Palette.cardBackground)
                .clipShape(RoundedRectangle(cornerRadius: Radius.card))
                .accessibilityElement(children: .combine)
            }
        }
        .sheet(item: $activeReap) { active in
            ReapSheet(
                kind: active.kind,
                title: active.label,
                onClose: { activeReap = nil },
                onExecuted: {
                    // The reap changed the machine; re-read so the window is not
                    // showing a pre-reap verdict.
                    onReaped()
                }
            )
        }
    }

    /// A reap finding gets a BUTTON that opens the preview sheet; every other
    /// action still shows its wording as text (the server's words, verbatim —
    /// `Finding.actionLabel`, ADR-0005). The button carries no verb of its own.
    @ViewBuilder
    private func actionAffordance(for finding: Finding) -> some View {
        if let kind = ReapKind(action: finding.action) {
            Button {
                activeReap = ActiveReap(kind: kind, label: finding.actionLabel)
            } label: {
                Label(finding.actionLabel, systemImage: "scissors")
            }
            .buttonStyle(.link)
            .font(Typography.caption)
            .accessibilityHint("Opens a dry run you can review before anything is reaped")
        } else if finding.action != .none {
            Text(finding.actionLabel)
                .font(Typography.caption)
                .foregroundStyle(Palette.accent)
        }
    }
}

/// The reap sheet's presentation item: the kind plus the finding's own wording
/// for the title. `Identifiable` for `.sheet(item:)`; the kind is unique per
/// open sheet.
private struct ActiveReap: Identifiable {
    let kind: ReapKind
    let label: String
    var id: String { kind.id }
}
