import SwiftUI

struct ConversationReasoningRow: View {
    let data: ConversationReasoningData
    let displayMode: ConversationDetailDisplayMode

    @State private var expanded: Bool

    init(data: ConversationReasoningData, displayMode: ConversationDetailDisplayMode) {
        self.data = data
        self.displayMode = displayMode
        _expanded = State(initialValue: displayMode.defaultExpanded())
    }

    var body: some View {
        VStack(alignment: .leading, spacing: expanded ? 8 : 0) {
            Button(action: toggleExpanded) {
                HStack(spacing: 8) {
                    Image(systemName: "brain.head.profile")
                        .remoraFont(size: 12, weight: .semibold)
                        .foregroundColor(RemoraTheme.textSecondary)
                    Text("Thinking")
                        .remoraFont(.caption, weight: .semibold)
                        .foregroundColor(RemoraTheme.textSecondary)
                    if !expanded {
                        Text(collapsedSummary)
                            .remoraFont(.caption)
                            .foregroundColor(RemoraTheme.textMuted)
                            .lineLimit(1)
                            .truncationMode(.tail)
                    }
                    Spacer(minLength: 8)
                    Image(systemName: expanded ? "chevron.up" : "chevron.down")
                        .remoraFont(size: 11, weight: .medium)
                        .foregroundColor(RemoraTheme.textMuted)
                }
            }
            .buttonStyle(.plain)

            if expanded {
                Text(reasoningText)
                    .remoraFont(.footnote)
                    .italic()
                    .foregroundColor(RemoraTheme.textSecondary)
                    .textSelection(.enabled)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .transition(.sectionReveal)
            }
        }
        .padding(.horizontal, 12)
        .padding(.vertical, 7)
        .animation(.spring(duration: 0.32, bounce: 0.12), value: expanded)
        .onChange(of: displayMode) { _, newValue in
            expanded = newValue.defaultExpanded()
        }
    }

    private var reasoningText: String {
        (data.summary + data.content)
            .filter { !$0.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty }
            .joined(separator: "\n\n")
    }

    private var collapsedSummary: String {
        let itemCount = (data.summary + data.content).filter {
            !$0.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
        }.count
        return itemCount == 1 ? "Internal reasoning" : "\(itemCount) reasoning notes"
    }

    private func toggleExpanded() {
        withAnimation(.easeInOut(duration: 0.2)) {
            expanded.toggle()
        }
    }
}

struct ConversationTodoListRow: View {
    let data: ConversationTodoListData
    private let bodySize: CGFloat = 13
    private let codeSize: CGFloat = 12
    @State private var expanded = true

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            Button(action: toggleExpanded) {
                HStack(spacing: 8) {
                    Image(systemName: headerIconName)
                        .remoraFont(size: 12, weight: .semibold)
                        .foregroundColor(headerTint)
                    Text("To Do")
                        .remoraFont(.caption, weight: .semibold)
                        .foregroundColor(RemoraTheme.textPrimary)
                    Text(summaryText)
                        .remoraFont(.caption2, weight: .semibold)
                        .foregroundColor(progressTint)
                    Spacer(minLength: 8)
                    Image(systemName: expanded ? "chevron.up" : "chevron.down")
                        .remoraFont(size: 11, weight: .medium)
                        .foregroundColor(RemoraTheme.textMuted)
                }
            }
            .buttonStyle(.plain)
            .padding(.horizontal, 12)
            .padding(.vertical, 10)

            if expanded {
                ScrollView(.vertical, showsIndicators: false) {
                    VStack(alignment: .leading, spacing: 10) {
                        ForEach(Array(data.steps.enumerated()), id: \.offset) { index, step in
                            HStack(alignment: .top, spacing: 8) {
                                todoStatusView(for: step.status)
                                    .padding(.top, 2)
                                Text("\(index + 1).")
                                    .remoraFont(.caption, weight: .semibold)
                                    .foregroundColor(RemoraTheme.textMuted)
                                    .padding(.top, 1)
                                RemoraMarkdownView(
                                    markdown: step.step,
                                    style: .content,
                                    bodySize: bodySize,
                                    codeSize: codeSize
                                )
                                    .strikethrough(step.status == .completed, color: RemoraTheme.textMuted)
                                    .opacity(step.status == .completed ? 0.78 : 1.0)
                                    .frame(maxWidth: .infinity, alignment: .leading)
                            }
                        }
                    }
                    .padding(10)
                }
                .frame(maxHeight: 160)
                .background(RemoraTheme.surface.opacity(0.45))
                .mask {
                    VStack(spacing: 0) {
                        Rectangle().fill(.black)
                        LinearGradient(colors: [.black, .clear], startPoint: .top, endPoint: .bottom)
                            .frame(height: 18)
                    }
                }
                .clipShape(RoundedRectangle(cornerRadius: 10, style: .continuous))
                .padding(.horizontal, 12)
                .padding(.bottom, 10)
                .transition(.sectionReveal)
            }
        }
    }

    private var completedCount: Int {
        data.completedCount
    }

    private var hasInProgressStep: Bool {
        data.steps.contains { $0.status == .inProgress }
    }

    private var headerIconName: String {
        if data.isComplete { return "checkmark.circle.fill" }
        if hasInProgressStep { return "checklist.checked" }
        return "checklist"
    }

    private var headerTint: Color {
        if data.isComplete { return RemoraTheme.success }
        if hasInProgressStep { return RemoraTheme.warning }
        return RemoraTheme.accent
    }

    private var summaryText: String {
        "\(completedCount) out of \(data.steps.count) task\(data.steps.count == 1 ? "" : "s") completed"
    }

    private var progressTint: Color {
        data.isComplete ? RemoraTheme.success : (hasInProgressStep ? RemoraTheme.warning : RemoraTheme.textSecondary)
    }

    private func toggleExpanded() {
        withAnimation(.easeInOut(duration: 0.2)) {
            expanded.toggle()
        }
    }

    @ViewBuilder
    private func todoStatusView(for status: HydratedPlanStepStatus) -> some View {
        switch status {
        case .pending:
            Image(systemName: "circle")
                .remoraFont(size: 11, weight: .semibold)
                .foregroundColor(RemoraTheme.textMuted)
        case .inProgress:
            ProgressView()
                .controlSize(.mini)
                .tint(RemoraTheme.warning)
                .frame(width: 11, height: 11)
        case .completed:
            Image(systemName: "checkmark.circle.fill")
                .remoraFont(size: 11, weight: .semibold)
                .foregroundColor(RemoraTheme.success)
        }
    }
}

struct ConversationProposedPlanRow: View {
    let data: ConversationProposedPlanData

    private var trimmedContent: String? {
        let trimmed = data.content.trimmingCharacters(in: .whitespacesAndNewlines)
        return trimmed.isEmpty ? nil : trimmed
    }

    var body: some View {
        if let trimmedContent {
            VStack(alignment: .leading, spacing: 8) {
                HStack(spacing: 8) {
                    Image(systemName: "list.bullet.rectangle.portrait.fill")
                        .remoraFont(size: 12, weight: .semibold)
                        .foregroundColor(RemoraTheme.accent)
                    Text("Plan")
                        .remoraFont(.caption, weight: .semibold)
                        .foregroundColor(RemoraTheme.textPrimary)
                }

                RemoraMarkdownView(
                    markdown: trimmedContent,
                    style: .system
                )
            }
            .padding(.horizontal, 12)
            .padding(.vertical, 10)
        }
    }
}

struct ConversationTurnDiffRow: View {
    let data: ConversationTurnDiffData
    @State private var presented: PresentedDiff?

    var body: some View {
        Button {
            presented = PresentedDiff(
                id: "turn-diff",
                title: "Turn Diff",
                diff: data.diff,
                stats: DiffStats(additions: data.additions, deletions: data.deletions),
                sections: presentedDiffSections(from: data.diff)
            )
        } label: {
            DiffIndicatorLabel(additions: data.additions, deletions: data.deletions)
        }
        .buttonStyle(.plain)
        .sheet(item: $presented) { sheet in
            ConversationDiffDetailSheet(
                title: sheet.title,
                diff: sheet.diff ?? "",
                sections: sheet.sections
            )
        }
    }
}

struct ConversationUserInputResponseRow: View {
    let data: ConversationUserInputResponseData

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            ForEach(Array(data.questions.enumerated()), id: \.element.id) { _, question in
                HStack(alignment: .firstTextBaseline, spacing: 6) {
                    Image(systemName: "checkmark.circle.fill")
                        .remoraFont(size: 10, weight: .semibold)
                        .foregroundColor(RemoraTheme.accent)
                    VStack(alignment: .leading, spacing: 2) {
                        Text(question.header ?? question.question)
                            .remoraFont(.caption, weight: .semibold)
                            .foregroundColor(RemoraTheme.textSecondary)
                        Text(question.answer)
                            .remoraFont(.caption)
                            .foregroundColor(RemoraTheme.textPrimary)
                            .textSelection(.enabled)
                    }
                }
            }
        }
        .padding(.horizontal, 12)
        .padding(.vertical, 6)
    }
}

struct ConversationDividerRow: View {
    let kind: ConversationDividerKind
    let isLiveTurn: Bool

    var body: some View {
        HStack(spacing: 10) {
            Capsule()
                .fill(RemoraTheme.border)
                .frame(minWidth: 16, maxHeight: 1)
            dividerContent
                .layoutPriority(1)
            Capsule()
                .fill(RemoraTheme.border)
                .frame(minWidth: 16, maxHeight: 1)
        }
        .padding(.vertical, 4)
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(title)
    }

    @ViewBuilder
    private var dividerContent: some View {
        switch kind {
        case .contextCompaction:
            HStack(spacing: 6) {
                if effectiveContextCompactionComplete {
                    Image(systemName: "checkmark.circle.fill")
                        .remoraFont(size: 10, weight: .semibold)
                        .foregroundColor(RemoraTheme.success)
                } else {
                    ProgressView()
                        .controlSize(.mini)
                        .tint(RemoraTheme.warning)
                }

                Text(title)
                    .remoraFont(.caption2, weight: .semibold)
                    .foregroundColor(
                        effectiveContextCompactionComplete ? RemoraTheme.textMuted : RemoraTheme.warning
                    )
                    .lineLimit(1)
            }
        default:
            Text(title)
                .remoraFont(.caption2, weight: .semibold)
                .foregroundColor(RemoraTheme.textMuted)
                .lineLimit(1)
        }
    }

    private var title: String {
        switch kind {
        case .contextCompaction:
            return effectiveContextCompactionComplete ? "Context compacted" : "Compacting context"
        case .modelRerouted(let fromModel, let toModel, let reason):
            let base = fromModel.map { "\($0) -> \(toModel)" } ?? "Routed to \(toModel)"
            if let reason, !reason.isEmpty {
                return "\(base) · \(reason)"
            }
            return base
        case .reviewEntered(let review):
            return review.isEmpty ? "Entered review" : "Entered review: \(review)"
        case .reviewExited(let review):
            return review.isEmpty ? "Exited review" : "Exited review: \(review)"
        case .workedFor(let duration):
            return duration
        case .generic(let title, let detail):
            if let detail, !detail.isEmpty {
                return "\(title): \(detail)"
            }
            return title
        }
    }

    private var effectiveContextCompactionComplete: Bool {
        guard case .contextCompaction(let isComplete) = kind else { return true }
        return isComplete && !isLiveTurn
    }
}

struct ConversationCodeReviewRow: View {
    let data: ConversationCodeReviewData
    @State private var dismissedFindingIndices: Set<Int> = []

    private var visibleFindings: [(index: Int, finding: ConversationCodeReviewFinding)] {
        data.findings.enumerated().compactMap { index, finding in
            dismissedFindingIndices.contains(index) ? nil : (index, finding)
        }
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            ForEach(visibleFindings, id: \.index) { entry in
                ConversationCodeReviewFindingCard(
                    finding: entry.finding,
                    onDismiss: { dismissedFindingIndices.insert(entry.index) }
                )
            }
        }
    }
}

private struct ConversationCodeReviewFindingCard: View {
    let finding: ConversationCodeReviewFinding
    let onDismiss: () -> Void

    private var priorityLabel: String? {
        finding.priority.map { "P\($0)" }
    }

    private var priorityTint: Color {
        switch finding.priority {
        case 0?, 1?:
            return RemoraTheme.danger
        case 2?:
            return RemoraTheme.warning
        case 3?:
            return RemoraTheme.textSecondary
        default:
            return RemoraTheme.textSecondary
        }
    }

    private var locationText: String? {
        guard let location = finding.codeLocation else { return nil }
        guard let lineRange = location.lineRange else { return location.absoluteFilePath }
        if lineRange.start == lineRange.end {
            return "\(location.absoluteFilePath):\(lineRange.start)"
        }
        return "\(location.absoluteFilePath):\(lineRange.start)-\(lineRange.end)"
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            HStack(alignment: .center, spacing: 10) {
                if let priorityLabel {
                    Text(priorityLabel)
                        .remoraFont(.caption2, weight: .bold)
                        .foregroundColor(priorityTint)
                        .padding(.horizontal, 10)
                        .padding(.vertical, 6)
                        .background(priorityTint.opacity(0.12), in: Capsule())
                }

                Text(finding.title)
                    .remoraFont(.headline, weight: .semibold)
                    .foregroundColor(RemoraTheme.textPrimary)
                    .frame(maxWidth: .infinity, alignment: .leading)

                Button("Dismiss", action: onDismiss)
                    .buttonStyle(.plain)
                    .remoraFont(.callout, weight: .medium)
                    .foregroundColor(RemoraTheme.textSecondary)
            }

            RemoraMarkdownView(markdown: finding.body, style: .content, selectionEnabled: true)

            if let locationText, !locationText.isEmpty {
                Text(locationText)
                    .remoraFont(.footnote)
                    .foregroundColor(RemoraTheme.textSecondary)
                    .textSelection(.enabled)
                    .frame(maxWidth: .infinity, alignment: .leading)
            }
        }
        .padding(20)
        .background(RemoraTheme.surface.opacity(0.72), in: RoundedRectangle(cornerRadius: 22))
        .overlay(
            RoundedRectangle(cornerRadius: 22)
                .stroke(RemoraTheme.border.opacity(0.7), lineWidth: 1)
        )
    }
}
struct ConversationSystemCardRow: View {
    let title: String
    let content: String
    let accent: Color
    let iconName: String

    var bodyView: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack(spacing: 6) {
                Image(systemName: iconName)
                    .remoraFont(size: 11, weight: .semibold)
                    .foregroundColor(accent)
                Text(title.uppercased())
                    .remoraFont(.caption2, weight: .bold)
                    .foregroundColor(accent)
            }
            if !content.isEmpty {
                RemoraMarkdownView(
                    markdown: content,
                    style: .system
                )
            }
        }
        .padding(.horizontal, 12)
        .padding(.vertical, 6)
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    var body: some View { bodyView }
}
