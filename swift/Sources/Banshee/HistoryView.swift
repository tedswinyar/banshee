// The history chart, from `/rollups` — the read `banshee-8ea` exists to serve.
//
// Both series are plotted: the AVERAGE and the MAX. Carrying both is the whole
// reason a rollup is not a sample — averages hide the spike that matters, and a
// chart that plots only `load1mAvg` is a smoother, prettier lie about the same
// five minutes. The max is drawn first (underneath) so the average reads as the
// primary line.

import SwiftUI
import Charts
import DesignKit
import BansheeCore

/// Which reading the chart shows. Presentation state, not a wire concept — each
/// case names the ROLLUP FIELDS it reads verbatim.
enum HistoryMetric: String, CaseIterable, Identifiable {
    case load
    case swap

    var id: String { rawValue }

    var label: String {
        switch self {
        case .load: return "Load"
        case .swap: return "Swap"
        }
    }

    /// Axis label, with the unit the values are IN after `value(of:)`.
    var axisLabel: String {
        switch self {
        case .load: return "Load average (1m)"
        case .swap: return "Swap in use (GB)"
        }
    }

    func avg(of rollup: Rollup) -> Double {
        switch self {
        case .load: return rollup.load1mAvg
        case .swap: return Double(rollup.swapUsedAvg) / 1_000_000_000
        }
    }

    func max(of rollup: Rollup) -> Double {
        switch self {
        case .load: return rollup.load1mMax
        case .swap: return Double(rollup.swapUsedMax) / 1_000_000_000
        }
    }
}

struct HistoryView: View {
    @Environment(DetailModel.self) private var model
    @State private var metric: HistoryMetric = .load

    var body: some View {
        @Bindable var model = model
        VStack(alignment: .leading, spacing: Spacing.md) {
            HStack(spacing: Spacing.md) {
                Picker("Metric", selection: $metric) {
                    ForEach(HistoryMetric.allCases) { m in
                        Text(m.label).tag(m)
                    }
                }
                .pickerStyle(.segmented)
                .labelsHidden()
                .fixedSize()
                Picker("Window", selection: $model.historyWindow) {
                    ForEach(HistoryWindow.allCases) { w in
                        Text(w.label).tag(w)
                    }
                }
                .pickerStyle(.segmented)
                .labelsHidden()
                .fixedSize()
                Spacer()
            }

            if let error = model.lastError, model.rollups.isEmpty {
                Text(error)
                    .font(Typography.caption)
                    .foregroundStyle(Palette.error)
            }

            if model.rollups.isEmpty {
                emptyState
            } else {
                chart
                Text("Each point is a 5-minute bucket. The pale line is the bucket MAX — the spikes the average smooths away.")
                    .font(Typography.caption)
                    .foregroundStyle(Palette.textSecondary)
            }
        }
        .padding(Spacing.lg)
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
        // Re-fetch when the window changes, and on first appearance — `.task(id:)`
        // covers both without a second modifier.
        .task(id: model.historyWindow) {
            await model.refreshHistory()
        }
    }

    private var chart: some View {
        Chart(model.rollups) { rollup in
            LineMark(
                x: .value("Time", rollup.bucketStart),
                y: .value(metric.axisLabel, metric.max(of: rollup)),
                series: .value("Series", "max")
            )
            .foregroundStyle(Palette.warning.opacity(0.55))
            .lineStyle(StrokeStyle(lineWidth: 1))

            LineMark(
                x: .value("Time", rollup.bucketStart),
                y: .value(metric.axisLabel, metric.avg(of: rollup)),
                series: .value("Series", "avg")
            )
            .foregroundStyle(Palette.accent)
            .lineStyle(StrokeStyle(lineWidth: 2))
        }
        .chartForegroundStyleScale([
            "avg": Palette.accent,
            "max": Palette.warning.opacity(0.55),
        ])
        .chartLegend(position: .top, alignment: .trailing)
        .accessibilityLabel("\(metric.axisLabel) over \(model.historyWindow.label)")
        .accessibilityValue(accessibilitySummary)
    }

    /// The chart, in words: newest reading plus the window's peak. A line chart
    /// with no composed value is invisible to VoiceOver.
    private var accessibilitySummary: String {
        guard let newest = model.rollups.last else { return "no data" }
        let peak = model.rollups.map { metric.max(of: $0) }.max() ?? 0
        return String(
            format: "latest average %.2f, window peak %.2f",
            metric.avg(of: newest), peak
        )
    }

    private var emptyState: some View {
        VStack(alignment: .leading, spacing: Spacing.sm) {
            Text("No history in this window.")
                .font(Typography.rowTitle)
            Text("History is drawn from 5-minute buckets, each summarised as it closes; the first appears within five minutes of the daemon starting and they survive 30 days. If the daemon was just installed, history accrues from now.")
                .font(Typography.body)
                .foregroundStyle(Palette.textSecondary)
                .fixedSize(horizontal: false, vertical: true)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .center)
    }
}
