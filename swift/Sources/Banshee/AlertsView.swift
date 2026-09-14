// The alert log — the LONG memory. Alerts keep a year of retention (ADR-0006),
// so the interesting moments survive after the samples behind a bad afternoon
// are gone. Served newest first; rendered in the server's order.

import SwiftUI
import DesignKit
import BansheeCore

struct AlertsView: View {
    @Environment(DetailModel.self) private var model

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            if let error = model.lastError, model.alerts.isEmpty {
                Text(error)
                    .font(Typography.caption)
                    .foregroundStyle(Palette.error)
                    .padding(Spacing.lg)
            }
            if model.alerts.isEmpty {
                emptyState
            } else {
                list
            }
            storeFooter
        }
        .task {
            await model.refreshAlerts()
            await model.refreshStats()
        }
    }

    private var list: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: Spacing.sm) {
                ForEach(model.alerts) { alert in
                    AlertRow(alert: alert)
                }
            }
            .padding(Spacing.lg)
            .frame(maxWidth: .infinity, alignment: .leading)
        }
    }

    private var emptyState: some View {
        VStack(alignment: .leading, spacing: Spacing.sm) {
            Text("No alert episodes recorded.")
                .font(Typography.rowTitle)
            Text("An episode opens when a dimension has held red past its up delay, stays open through brief dips, and closes with one recovery notice. Quiet is an answer.")
                .font(Typography.body)
                .foregroundStyle(Palette.textSecondary)
                .fixedSize(horizontal: false, vertical: true)
        }
        .padding(Spacing.xl)
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .center)
    }

    /// The daemon's own footprint and lifetime-average CPU. "—" when the
    /// daemon predates the fields — never "0 MB", which would claim it costs
    /// nothing.
    private func selfCost(_ stats: Stats) -> String {
        guard stats.reportsSelfCost else { return "—" }
        return "\(Format.bytes(stats.selfFootprintBytes)), \(String(format: "%.2f", stats.selfCpuPercent))% of one core"
    }

    /// What the store holds and what it costs — a monitor that will not say
    /// what it costs has no standing to complain about anything else (ADR-0006).
    /// The size is the file's high-water mark, not the current row count.
    private var storeFooter: some View {
        Group {
            if let stats = model.stats {
                Text("Store: \(stats.samples) samples · \(stats.rollups) rollups · \(stats.censuses) censuses · \(stats.alerts) alerts · \(Format.bytes(stats.dbSizeBytes)) on disk (high-water) · Banshee itself: \(selfCost(stats))")
                    .font(Typography.caption)
                    .foregroundStyle(Palette.textSecondary)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .padding(Spacing.sm)
                    .overlay(alignment: .top) {
                        Rectangle().fill(Palette.separator).frame(height: 1)
                    }
            }
        }
    }
}

struct AlertRow: View {
    let alert: AlertEpisode

    var body: some View {
        HStack(alignment: .firstTextBaseline, spacing: Spacing.sm) {
            Circle()
                .fill(DimensionRow.color(for: alert.peak.band))
                .frame(width: Size.statusDot, height: Size.statusDot)
                .accessibilityHidden(true) // decorative; the label names the band
            VStack(alignment: .leading, spacing: Spacing.xs) {
                Text(alert.peak.message)
                    .font(Typography.body)
                    .fixedSize(horizontal: false, vertical: true)
                // Who was behind it at the peak — core's line, verbatim,
                // and nothing when the finding named nobody.
                if let who = alert.peak.whoLine {
                    Text(who)
                        .font(Typography.caption)
                        .foregroundStyle(Palette.textSecondary)
                        .fixedSize(horizontal: false, vertical: true)
                }
                Text(Self.detail(for: alert))
                    .font(Typography.caption)
                    .foregroundStyle(Palette.textSecondary)
            }
            Spacer(minLength: Spacing.sm)
            Text(alert.startedAt.formatted(date: .abbreviated, time: .shortened))
                .font(Typography.caption)
                .foregroundStyle(Palette.textSecondary)
                .frame(minWidth: Size.metaColumn, alignment: .trailing)
        }
        .padding(Spacing.sm)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Palette.cardBackground)
        .clipShape(RoundedRectangle(cornerRadius: Radius.card))
        .accessibilityElement(children: .combine)
        .accessibilityLabel("\(alert.state.rawValue) episode, \(alert.dimension): \(alert.peak.message)")
        .accessibilityValue(Self.detail(for: alert))
    }

    /// The episode's shape in one line: dimension, peak level, how long it ran (or
    /// that it is still open), and "+N more" — the re-fires it absorbed, each of
    /// which the old one-per-hour model would have swallowed without a trace.
    /// Kept in the view (not the model) because it is presentation; the numbers
    /// and states came off the wire.
    static func detail(for e: AlertEpisode) -> String {
        var parts = ["\(e.dimension) · peaked at \(e.peak.level.rawValue)"]
        switch (e.state, e.endedAt) {
        case (.closed, let end?):
            parts.append("ran \(DimensionRow.duration(UInt64(max(0, end.timeIntervalSince(e.startedAt)))))")
        case (.recovering, _):
            parts.append("recovering")
        default:
            parts.append("open")
        }
        if e.suppressed > 0 {
            parts.append("+\(e.suppressed) more")
        }
        return parts.joined(separator: " · ")
    }
}
