import SwiftUI

struct PendingUserInputPromptView: View {
    let request: PendingUserInputRequest
    let onSubmit: ([String: [String]]) -> Void
    let onDismiss: () -> Void

    @State private var selectedAnswers: [String: String] = [:]
    @State private var otherAnswers: [String: String] = [:]

    private var promptTitle: String {
        let firstQuestion = request.questions.first?.question.lowercased() ?? ""
        if firstQuestion.contains("implement") && firstQuestion.contains("plan") {
            return "Implement Plan"
        }
        return "Input Required"
    }

    private var requesterLabel: String? {
        AgentLabelFormatter.format(
            nickname: request.requesterAgentNickname,
            role: request.requesterAgentRole
        )
    }

    private var unsupportedQuestions: [PendingUserInputQuestion] {
        request.questions.filter { question in
            question.isSecret || (!question.isOtherAllowed && question.options.isEmpty)
        }
    }

    private var canSubmit: Bool {
        unsupportedQuestions.isEmpty &&
        request.questions.allSatisfy { !resolvedAnswer(for: $0).isEmpty }
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack(spacing: 8) {
                Image(systemName: "questionmark.bubble.fill")
                    .foregroundColor(RemoraTheme.warning)
                Text(promptTitle)
                    .remoraFont(.caption, weight: .semibold)
                    .foregroundColor(RemoraTheme.textPrimary)
                Spacer()
                Button(action: onDismiss) {
                    Image(systemName: "xmark.circle.fill")
                        .remoraFont(.body)
                        .foregroundColor(RemoraTheme.textMuted)
                        .frame(
                            width: RemoraAccessibilityMetrics.minimumHitTarget,
                            height: RemoraAccessibilityMetrics.minimumHitTarget
                        )
                        .contentShape(Circle())
                }
                .buttonStyle(.plain)
                .accessibilityLabel("Dismiss input request")
            }

            if let requesterLabel {
                Text(requesterLabel)
                    .remoraFont(.caption2)
                    .foregroundColor(RemoraTheme.textMuted)
            }

            ForEach(request.questions, id: \.id) { question in
                VStack(alignment: .leading, spacing: 6) {
                    if let header = question.header, !header.isEmpty {
                        Text(header.uppercased())
                            .remoraFont(.caption2, weight: .bold)
                            .foregroundColor(RemoraTheme.textMuted)
                    }

                    Text(question.question)
                        .remoraFont(.caption)
                        .foregroundColor(RemoraTheme.textPrimary)

                    if question.isSecret || (!question.isOtherAllowed && question.options.isEmpty) {
                        Text("This prompt type is not fully supported in the current iOS client.")
                            .remoraFont(.caption2)
                            .foregroundColor(RemoraTheme.textSecondary)
                    } else {
                        VStack(alignment: .leading, spacing: 8) {
                            if !question.options.isEmpty {
                                // ViewThatFits + VStack fallback so long
                                // option labels wrap to a new row instead
                                // of squeezing a short option into a narrow
                                // column with character-by-character wrapping.
                                let optionButtons = ForEach(question.options, id: \.label) { option in
                                    let isSelected =
                                        selectedAnswers[question.id] == option.label &&
                                        trimmedOtherAnswer(for: question).isEmpty
                                    Button {
                                        selectedAnswers[question.id] = option.label
                                        otherAnswers[question.id] = ""
                                    } label: {
                                        Text(option.label)
                                            .remoraFont(.caption2, weight: .semibold)
                                            .foregroundColor(
                                                isSelected ? RemoraTheme.textOnAccent : RemoraTheme.textPrimary
                                            )
                                            .padding(.horizontal, 10)
                                            .frame(minHeight: RemoraAccessibilityMetrics.minimumHitTarget)
                                            .background(isSelected ? RemoraTheme.accent : RemoraTheme.surface.opacity(0.8))
                                            .clipShape(Capsule())
                                            .contentShape(Capsule())
                                    }
                                    .buttonStyle(.plain)
                                    .accessibilityAddTraits(isSelected ? .isSelected : [])
                                }
                                ViewThatFits(in: .horizontal) {
                                    HStack(spacing: 8) { optionButtons }
                                    VStack(alignment: .leading, spacing: 8) { optionButtons }
                                }
                            }

                            if question.isOtherAllowed {
                                TextField(
                                    question.options.isEmpty ? "Enter response" : "Other response",
                                    text: otherAnswerBinding(for: question)
                                )
                                .remoraFont(.caption2)
                                .foregroundColor(RemoraTheme.textPrimary)
                                .padding(.horizontal, 10)
                                .padding(.vertical, 8)
                                .frame(minHeight: RemoraAccessibilityMetrics.minimumHitTarget)
                                .background(RemoraTheme.surface.opacity(0.8))
                                .clipShape(RoundedRectangle(cornerRadius: 8))
                            }
                        }
                    }
                }
            }

            if canSubmit {
                Button {
                    let answers = request.questions.reduce(into: [String: [String]]()) { result, question in
                        let answer = resolvedAnswer(for: question)
                        guard !answer.isEmpty else { return }
                        result[question.id] = [answer]
                    }
                    onSubmit(answers)
                } label: {
                    Text("Submit")
                        .remoraFont(.caption, weight: .semibold)
                        .foregroundColor(RemoraTheme.textOnAccent)
                        .padding(.horizontal, 12)
                        .frame(minHeight: RemoraAccessibilityMetrics.minimumHitTarget)
                        .background(RemoraTheme.accent)
                        .clipShape(Capsule())
                        .contentShape(Capsule())
                }
                .buttonStyle(.plain)
                .accessibilityLabel("Submit answers")
            }
        }
        .padding(.horizontal, 14)
        .padding(.vertical, 12)
        .modifier(GlassRectModifier(cornerRadius: 14))
    }

    private func otherAnswerBinding(for question: PendingUserInputQuestion) -> Binding<String> {
        Binding(
            get: { otherAnswers[question.id, default: ""] },
            set: { newValue in
                otherAnswers[question.id] = newValue
            }
        )
    }

    private func trimmedOtherAnswer(for question: PendingUserInputQuestion) -> String {
        otherAnswers[question.id, default: ""]
            .trimmingCharacters(in: .whitespacesAndNewlines)
    }

    private func resolvedAnswer(for question: PendingUserInputQuestion) -> String {
        let other = trimmedOtherAnswer(for: question)
        if !other.isEmpty {
            return other
        }
        return selectedAnswers[question.id, default: ""]
            .trimmingCharacters(in: .whitespacesAndNewlines)
    }
}

struct PlanImplementationPromptView: View {
    let onImplement: () -> Void
    let onDismiss: () -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack(spacing: 8) {
                Image(systemName: "list.bullet.clipboard.fill")
                    .foregroundColor(RemoraTheme.accentForegroundOnSurface)
                Text("Implement Plan")
                    .remoraFont(.caption, weight: .semibold)
                    .foregroundColor(RemoraTheme.textPrimary)
                Spacer()
            }

            Text("Switch to Default mode and implement the plan?")
                .remoraFont(.caption)
                .foregroundColor(RemoraTheme.textSecondary)

            ViewThatFits(in: .horizontal) {
                HStack(spacing: 8) {
                    implementButton
                    stayInPlanButton
                }
                VStack(alignment: .leading, spacing: 8) {
                    implementButton
                    stayInPlanButton
                }
            }
        }
        .padding(.horizontal, 14)
        .padding(.vertical, 12)
        .modifier(GlassRectModifier(cornerRadius: 14))
    }

    private var implementButton: some View {
        Button(action: onImplement) {
            Text("Implement")
                .remoraFont(.caption2, weight: .semibold)
                .foregroundColor(RemoraTheme.textOnAccent)
                .padding(.horizontal, 10)
                .frame(minHeight: RemoraAccessibilityMetrics.minimumHitTarget)
                .background(RemoraTheme.accent)
                .clipShape(Capsule())
                .contentShape(Capsule())
        }
        .buttonStyle(.plain)
        .accessibilityLabel("Implement plan")
    }

    private var stayInPlanButton: some View {
        Button(action: onDismiss) {
            Text("Stay in Plan")
                .remoraFont(.caption2, weight: .semibold)
                .foregroundColor(RemoraTheme.textPrimary)
                .padding(.horizontal, 10)
                .frame(minHeight: RemoraAccessibilityMetrics.minimumHitTarget)
                .background(RemoraTheme.surface.opacity(0.8))
                .clipShape(Capsule())
                .contentShape(Capsule())
        }
        .buttonStyle(.plain)
        .accessibilityLabel("Stay in plan mode")
    }
}

struct QueuedFollowUpsPreviewView: View {
    let previews: [AppQueuedFollowUpPreview]
    let onSteer: (AppQueuedFollowUpPreview) -> Void
    let onDelete: (AppQueuedFollowUpPreview) -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack(spacing: 8) {
                Image(systemName: "clock.arrow.circlepath")
                    .font(.system(size: 13, weight: .semibold))
                    .foregroundColor(RemoraTheme.accentForegroundOnSurface)
                Text("Queued Next")
                    .remoraFont(.caption, weight: .semibold)
                    .foregroundColor(RemoraTheme.textPrimary)
                Spacer()
                Text("\(previews.count)")
                    .remoraFont(.caption2, weight: .semibold)
                    .foregroundColor(RemoraTheme.textSecondary)
                    .padding(.horizontal, 8)
                    .padding(.vertical, 4)
                    .background(RemoraTheme.surface.opacity(0.9))
                    .clipShape(Capsule())
            }

            ForEach(previews, id: \.id) { preview in
                let style = QueuedFollowUpPreviewStyle.forKind(preview.kind)

                HStack(alignment: .center, spacing: 12) {
                    VStack(alignment: .leading, spacing: 8) {
                        HStack(spacing: 6) {
                            Image(systemName: style.symbol)
                                .font(.system(size: 11, weight: .semibold))
                            Text(style.title)
                                .remoraFont(.caption2, weight: .semibold)
                        }
                        .foregroundColor(style.tint)
                        .padding(.horizontal, 8)
                        .padding(.vertical, 5)
                        .background(style.tint.opacity(0.14))
                        .clipShape(Capsule())

                        Text(preview.text)
                            .remoraFont(.caption)
                            .foregroundColor(RemoraTheme.textSecondary)
                            .lineLimit(4)
                    }
                    .frame(maxWidth: .infinity, alignment: .leading)

                    if preview.kind == .message || preview.kind == .pendingSteer {
                        Button(action: { onSteer(preview) }) {
                            HStack(spacing: 6) {
                                if preview.kind == .pendingSteer {
                                    Image(systemName: "checkmark")
                                        .font(.system(size: 12, weight: .semibold))
                                    Text("Steering")
                                        .remoraFont(.caption, weight: .semibold)
                                } else {
                                    Image(systemName: "arrow.turn.down.right")
                                        .font(.system(size: 12, weight: .semibold))
                                    Text("Steer")
                                        .remoraFont(.caption, weight: .semibold)
                                }
                            }
                            .foregroundColor(
                                preview.kind == .pendingSteer
                                    ? RemoraTheme.accentForegroundOnSurface
                                    : RemoraTheme.textPrimary
                            )
                            .padding(.horizontal, 12)
                            .frame(minHeight: RemoraAccessibilityMetrics.minimumHitTarget)
                            .background(RemoraTheme.surface.opacity(0.96))
                            .clipShape(Capsule())
                            .contentShape(Capsule())
                        }
                        .buttonStyle(.plain)
                        .disabled(preview.kind == .pendingSteer)
                        .accessibilityLabel(
                            preview.kind == .pendingSteer ? "Steer queued" : "Steer with this queued message"
                        )
                    }

                    Button(action: { onDelete(preview) }) {
                        Image(systemName: "trash")
                            .font(.system(size: 13, weight: .semibold))
                            .foregroundColor(RemoraTheme.textSecondary)
                            .frame(
                                width: RemoraAccessibilityMetrics.minimumHitTarget,
                                height: RemoraAccessibilityMetrics.minimumHitTarget
                            )
                    }
                    .buttonStyle(.plain)
                }
                .padding(12)
                .background(style.background)
                .overlay(
                    RoundedRectangle(cornerRadius: 14)
                        .stroke(style.border, lineWidth: 1)
                )
                .clipShape(RoundedRectangle(cornerRadius: 14))
            }
        }
        .padding(12)
        .background(RemoraTheme.codeBackground.opacity(0.92))
        .clipShape(RoundedRectangle(cornerRadius: 14))
    }
}

private struct QueuedFollowUpPreviewStyle {
    let title: String
    let symbol: String
    let tint: Color
    let background: Color
    let border: Color

    static func forKind(_ kind: AppQueuedFollowUpKind) -> Self {
        switch kind {
        case .message:
            let tint = RemoraTheme.accent
            return Self(
                title: "Queued message",
                symbol: "text.bubble.fill",
                tint: tint,
                background: tint.opacity(0.08),
                border: tint.opacity(0.24)
            )
        case .pendingSteer:
            let tint = RemoraTheme.accentStrong
            return Self(
                title: "Steer queued",
                symbol: "arrowshape.turn.up.right.fill",
                tint: tint,
                background: tint.opacity(0.10),
                border: tint.opacity(0.28)
            )
        case .retryingSteer:
            let tint = RemoraTheme.warning
            return Self(
                title: "Retrying steer",
                symbol: "arrow.clockwise",
                tint: tint,
                background: tint.opacity(0.10),
                border: tint.opacity(0.28)
            )
        }
    }
}
