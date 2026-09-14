// The reap sheet — the GUI's one-click reap, with the safety the whole feature
// rests on: it opens on the DRY-RUN preview and kills nothing until the person
// presses the destructive button against a set they have just read. That mirrors
// the CLI (`--execute`) and the server (preview/execute split); the sheet adds a
// confirmation surface, not a shortcut past one.
//
// House rules: DesignKit tokens only, the state is the model's, every outcome is
// surfaced, and decorative glyphs are hidden from VoiceOver while the row carries
// its meaning in words.

import SwiftUI
import DesignKit
import BansheeCore

struct ReapSheet: View {
    /// The finding's own wording, rendered verbatim (ADR-0005) as the title.
    let title: String
    let onClose: () -> Void
    /// Called after a successful execute, so the caller can re-read the verdict
    /// rather than waiting for the next poll tick.
    let onExecuted: () -> Void

    @State private var model: ReapActionModel

    init(
        kind: ReapKind,
        title: String,
        onClose: @escaping () -> Void,
        onExecuted: @escaping () -> Void
    ) {
        self.title = title
        self.onClose = onClose
        self.onExecuted = onExecuted
        _model = State(initialValue: ReapActionModel(kind: kind))
    }

    /// Test seam: inject a model built on a mock client.
    init(
        model: ReapActionModel,
        title: String,
        onClose: @escaping () -> Void = {},
        onExecuted: @escaping () -> Void = {}
    ) {
        self.title = title
        self.onClose = onClose
        self.onExecuted = onExecuted
        _model = State(initialValue: model)
    }

    var body: some View {
        VStack(alignment: .leading, spacing: Spacing.md) {
            header
            Divider()
            content
            Divider()
            footer
        }
        .padding(Spacing.lg)
        .frame(minWidth: 460, minHeight: 360)
        .task { await model.loadPreview() }
    }

    private var header: some View {
        VStack(alignment: .leading, spacing: Spacing.xs) {
            Text(title)
                .font(Typography.title)
            Text(subtitle)
                .font(Typography.caption)
                .foregroundStyle(Palette.textSecondary)
        }
    }

    private var subtitle: String {
        switch model.phase {
        case .loadingPreview: return "Looking at what could be reaped…"
        case .preview:
            return model.hasReapableCandidates
                ? "Dry run — nothing has been touched. Review, then reap."
                : "Dry run — nothing is safe to reap right now."
        case .executing: return "Reaping…"
        case .executed: return "Done."
        case .failed: return "Something went wrong."
        }
    }

    @ViewBuilder
    private var content: some View {
        switch model.phase {
        case .loadingPreview, .executing:
            ProgressView()
                .frame(maxWidth: .infinity, maxHeight: .infinity)
        case .failed(let message):
            failure(message)
        case .preview, .executed:
            ScrollView { candidateList }
        }
    }

    @ViewBuilder
    private var candidateList: some View {
        switch model.report {
        case .sessions(let report):
            SessionCandidateList(report: report)
        case .orphans(let report):
            OrphanCandidateList(report: report)
        case nil:
            EmptyView()
        }
    }

    private func failure(_ message: String) -> some View {
        VStack(alignment: .leading, spacing: Spacing.sm) {
            Text(message)
                .font(Typography.body)
                .foregroundStyle(Palette.error)
                .fixedSize(horizontal: false, vertical: true)
            Button("Try again") { Task { await model.loadPreview() } }
                .buttonStyle(.bordered)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
    }

    @ViewBuilder
    private var footer: some View {
        HStack {
            Spacer()
            switch model.phase {
            case .preview:
                Button("Cancel", role: .cancel) { onClose() }
                    .buttonStyle(.bordered)
                // Destructive, and disabled when there is nothing to reap: a
                // button that would kill nothing should not look armed.
                Button(reapButtonTitle, role: .destructive) {
                    Task {
                        await model.execute()
                        if case .executed = model.phase { onExecuted() }
                    }
                }
                .buttonStyle(.borderedProminent)
                .disabled(!model.hasReapableCandidates)
            case .executed, .failed:
                Button("Done") { onClose() }
                    .buttonStyle(.borderedProminent)
            case .loadingPreview, .executing:
                Button("Cancel", role: .cancel) { onClose() }
                    .buttonStyle(.bordered)
            }
        }
    }

    private var reapButtonTitle: String {
        switch model.report {
        case .sessions(let r):
            let n = r.toReap.count
            return n == 1 ? "Reap 1 session" : "Reap \(n) sessions"
        case .orphans(let r):
            let n = r.candidates.count
            return n == 1 ? "Reap 1 orphan" : "Reap \(n) orphans"
        case nil:
            return "Reap"
        }
    }
}

/// Stale-session candidates: what would be reaped, and every spared one with the
/// rail that saved it — the spared rows are the reassurance that makes the reap
/// button safe to press.
struct SessionCandidateList: View {
    let report: SessionReapReport

    var body: some View {
        VStack(alignment: .leading, spacing: Spacing.sm) {
            if !report.tmuxAvailable {
                // "Could not look" is not "nothing to reap" (the census's rule).
                Text("tmux could not be inspected — no sessions were examined.")
                    .font(Typography.body)
                    .foregroundStyle(Palette.textSecondary)
            } else if report.candidates.isEmpty {
                Text("No sessions are idle past \(report.staleDays) days.")
                    .font(Typography.body)
                    .foregroundStyle(Palette.textSecondary)
            } else {
                ForEach(report.candidates) { c in
                    ReapRow(
                        title: c.session,
                        subtitle: c.reason,
                        detail: idleLabel(c),
                        verdictReap: c.verdict == .reap,
                        outcome: c.outcome
                    )
                }
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    private func idleLabel(_ c: SessionCandidate) -> String {
        guard let days = c.idleDays else { return "idle unknown" }
        return String(format: "%.1f days idle", days)
    }
}

/// Orphan candidates. The preview list is NOT the kill list — execute recomputes
/// server-side (PIDs recycle) — so this shows what exists, not a promise of what
/// dies, and the copy says so.
struct OrphanCandidateList: View {
    let report: OrphanReapReport

    var body: some View {
        VStack(alignment: .leading, spacing: Spacing.sm) {
            if report.candidates.isEmpty {
                Text("No orphaned helper processes right now.")
                    .font(Typography.body)
                    .foregroundStyle(Palette.textSecondary)
            } else {
                if !report.executed {
                    Text("Recomputed at reap time — this is what exists now, not a fixed kill list.")
                        .font(Typography.caption)
                        .foregroundStyle(Palette.textSecondary)
                }
                ForEach(report.candidates) { c in
                    ReapRow(
                        title: "\(c.program) (pid \(c.pid))",
                        subtitle: c.args,
                        detail: ByteFormat.short(c.rssBytes),
                        verdictReap: true,
                        outcome: c.outcome
                    )
                }
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }
}

/// One candidate row, shared by both lists: a status dot (reap vs spared, or the
/// outcome once executed), a title, the reason/args, and a trailing metric.
struct ReapRow: View {
    let title: String
    let subtitle: String
    let detail: String
    let verdictReap: Bool
    let outcome: String?

    var body: some View {
        HStack(alignment: .firstTextBaseline, spacing: Spacing.sm) {
            // Color-only, therefore decorative — the accessibility label below
            // names the state in words.
            Circle()
                .fill(dotColor)
                .frame(width: Size.statusDot, height: Size.statusDot)
                .accessibilityHidden(true)
            VStack(alignment: .leading, spacing: Spacing.xs) {
                Text(title)
                    .font(Typography.rowTitle)
                Text(subtitle)
                    .font(Typography.caption)
                    .foregroundStyle(Palette.textSecondary)
                    .fixedSize(horizontal: false, vertical: true)
            }
            Spacer(minLength: Spacing.sm)
            Text(trailing)
                .font(Typography.caption)
                .foregroundStyle(Palette.textSecondary)
                .frame(minWidth: Size.metaColumn, alignment: .trailing)
        }
        .padding(Spacing.sm)
        .background(Palette.cardBackground)
        .clipShape(RoundedRectangle(cornerRadius: Radius.card))
        .accessibilityElement(children: .combine)
        .accessibilityLabel("\(title): \(stateWord)")
        .accessibilityValue(subtitle)
    }

    /// Once executed, the outcome drives the colour; before that, the verdict.
    private var dotColor: Color {
        if let outcome {
            switch outcome {
            case "killed", "terminated": return Palette.success
            case "survived": return Palette.warning
            default: return Palette.error // "failed: …"
            }
        }
        return verdictReap ? Palette.warning : Palette.textSecondary
    }

    private var stateWord: String {
        if let outcome { return outcome }
        return verdictReap ? "will be reaped" : "spared"
    }

    private var trailing: String {
        if let outcome { return outcome }
        return detail
    }
}

/// Byte sizes at a human scale, matching the CLI's `fmt_bytes`. Local to the app
/// target — a formatter is presentation, and the wire carries raw bytes.
enum ByteFormat {
    static func short(_ bytes: UInt64) -> String {
        let b = Double(bytes)
        if b >= 1e9 { return String(format: "%.1f GB", b / 1e9) }
        if b >= 1e6 { return String(format: "%.0f MB", b / 1e6) }
        if b >= 1e3 { return String(format: "%.0f KB", b / 1e3) }
        return "\(bytes) B"
    }
}
