// The census tab — the "what should I close?" screen, and the thing `perf-scan`
// was actually used for: stale sessions with their cwd, orphans by program, the
// biggest app groups, and what the managed agents cost.
//
// Two honesty rules, both off the wire contract:
// - `tmuxAvailable: false` renders as "could not look", never as a clean list.
// - The managed-agent figure is `monitorTotal`, NEVER a sum over
//   `monitorAgents` — the per-group percentages have different denominators,
//   and a real census summed to 59.9% against an honest 49.2%.

import SwiftUI
import DesignKit
import BansheeCore

struct CensusView: View {
    @Environment(DetailModel.self) private var model

    var body: some View {
        Group {
            if let census = model.census {
                loaded(census)
            } else if model.censusLoaded {
                noCensusYet
            } else if let error = model.lastError {
                Text(error)
                    .font(Typography.caption)
                    .foregroundStyle(Palette.error)
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
            } else {
                ProgressView("Reading the census…")
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
            }
        }
        .task {
            await model.refreshCensus()
        }
    }

    /// The server's 404: a real state during the first five minutes after
    /// install, and NOT a claim that the machine is idle.
    private var noCensusYet: some View {
        VStack(alignment: .leading, spacing: Spacing.sm) {
            Text("No census yet.")
                .font(Typography.rowTitle)
            Text("The census tier runs every 5 minutes; the first one is on its way. This is not a claim that nothing is running.")
                .font(Typography.body)
                .foregroundStyle(Palette.textSecondary)
                .fixedSize(horizontal: false, vertical: true)
        }
        .padding(Spacing.xl)
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .center)
    }

    private func loaded(_ census: Census) -> some View {
        ScrollView {
            VStack(alignment: .leading, spacing: Spacing.lg) {
                header(census)
                staleSessions(census)
                tmux(census)
                orphans(census)
                appGroups(census)
                monitors(census)
            }
            .padding(Spacing.lg)
            .frame(maxWidth: .infinity, alignment: .leading)
        }
    }

    private func header(_ census: Census) -> some View {
        HStack(alignment: .firstTextBaseline) {
            Text("Census")
                .font(Typography.title)
            Spacer()
            Text("\(census.totalProcs) processes · taken \(census.takenAt.formatted(.relative(presentation: .named)))")
                .font(Typography.caption)
                .foregroundStyle(Palette.textSecondary)
        }
        .accessibilityElement(children: .combine)
    }

    private func staleSessions(_ census: Census) -> some View {
        section("Stale agent sessions") {
            if census.staleSessions.isEmpty {
                quiet("No stale sessions.")
            } else {
                ForEach(census.staleSessions) { session in
                    row(
                        title: "\(session.program) · pid \(session.pid)",
                        subtitle: session.cwd ?? "working directory unknown",
                        meta: "\(session.ageDays)d · \(Format.bytes(session.rssBytes))"
                    )
                }
            }
        }
    }

    private func tmux(_ census: Census) -> some View {
        section("tmux") {
            if !census.tmuxAvailable {
                // "Could not look" — a different sentence from "zero sessions",
                // and conflating them reports a clean machine.
                quiet("tmux could not be checked. That is not the same as no sessions.")
            } else if census.tmuxSessions.isEmpty {
                quiet("No tmux sessions.")
            } else {
                ForEach(census.tmuxSessions) { session in
                    row(
                        title: session.name,
                        subtitle: tmuxDetail(session),
                        meta: session.paneCommand
                    )
                }
            }
        }
    }

    private func tmuxDetail(_ session: TmuxSessionInfo) -> String {
        if session.isBusy { return "busy — never reap" }
        guard let idle = session.idleDays else { return "idle time unknown" }
        let days = String(format: "idle %.1fd", idle)
        return session.isStale ? "\(days) — reap candidate" : days
    }

    private func orphans(_ census: Census) -> some View {
        section("Orphaned helpers") {
            let o = census.orphans
            if o.orphanCount == 0 {
                quiet("No orphans. \(o.totalCount) helpers alive, all parented (\(Format.bytes(o.totalRssBytes))).")
            } else {
                Text("\(o.orphanCount) of \(o.totalCount) helpers are orphaned (ppid 1), holding \(Format.bytes(o.orphanRssBytes)).")
                    .font(Typography.body)
                ForEach(o.orphansByProgram) { program in
                    row(
                        title: program.program,
                        subtitle: "\(program.count) orphaned",
                        meta: Format.bytes(program.rssBytes)
                    )
                }
            }
            if census.ideHelpers.count > 0 {
                quiet("Plus \(census.ideHelpers.count) IDE-spawned helpers (\(Format.bytes(census.ideHelpers.rssBytes))), rolled up.")
            }
        }
    }

    private func appGroups(_ census: Census) -> some View {
        section("Biggest apps") {
            if census.appGroups.isEmpty {
                quiet("No app groups reported.")
            } else {
                ForEach(census.appGroups) { group in
                    row(
                        title: group.name,
                        subtitle: "\(group.procCount) processes",
                        meta: Format.bytes(group.rssBytes)
                    )
                }
            }
        }
    }

    private func monitors(_ census: Census) -> some View {
        section("Managed agents") {
            let total = census.monitorTotal
            // The deduplicated figure against one global denominator — the
            // per-agent rows below are NOT summable (docs/wire-format.md).
            Text("\(total.procCount) processes · \(String(format: "%.1f%%", total.percentOfOneCore)) of one core · \(Format.bytes(total.rssBytes))")
                .font(Typography.body)
            ForEach(census.monitorAgents) { agent in
                row(
                    title: agent.name,
                    subtitle: "\(agent.procCount) processes · \(String(format: "%.1f%%", agent.percentOfOneCore)) of one core (own denominator — not summable)",
                    meta: Format.bytes(agent.rssBytes)
                )
            }
        }
    }

    // MARK: - Pieces

    private func section(_ title: String, @ViewBuilder content: () -> some View) -> some View {
        VStack(alignment: .leading, spacing: Spacing.sm) {
            Text(title)
                .font(Typography.rowTitle)
            content()
        }
    }

    private func quiet(_ text: String) -> some View {
        Text(text)
            .font(Typography.body)
            .foregroundStyle(Palette.textSecondary)
            .fixedSize(horizontal: false, vertical: true)
    }

    private func row(title: String, subtitle: String, meta: String) -> some View {
        HStack(alignment: .firstTextBaseline, spacing: Spacing.sm) {
            VStack(alignment: .leading, spacing: Spacing.xs) {
                Text(title)
                    .font(Typography.body)
                Text(subtitle)
                    .font(Typography.caption)
                    .foregroundStyle(Palette.textSecondary)
            }
            Spacer(minLength: Spacing.sm)
            Text(meta)
                .font(Typography.caption)
                .foregroundStyle(Palette.textSecondary)
                .frame(minWidth: Size.metaColumn, alignment: .trailing)
        }
        .padding(Spacing.sm)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Palette.cardBackground)
        .clipShape(RoundedRectangle(cornerRadius: Radius.card))
        .accessibilityElement(children: .combine)
        .accessibilityLabel("\(title). \(subtitle). \(meta)")
    }
}

/// Shared display formatting for wire values.
enum Format {
    /// Memory-style bytes (binary units), matching what Activity Monitor shows
    /// for RSS — the number the user will compare against.
    static func bytes(_ value: UInt64) -> String {
        ByteCountFormatter.string(fromByteCount: Int64(clamping: value), countStyle: .memory)
    }
}
