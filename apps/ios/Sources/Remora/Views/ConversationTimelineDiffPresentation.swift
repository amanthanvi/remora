import SwiftUI

struct ConversationPinnedContextStrip: View {
    let items: [ConversationItem]
    @State private var todoExpanded = false
    @State private var selectedDiff: PresentedDiff?
    @State private var cachedCombinedPinnedDiff: PresentedDiff?

    init(items: [ConversationItem]) {
        self.items = items
        _cachedCombinedPinnedDiff = State(
            initialValue: Self.buildCombinedPinnedDiff(from: items)
        )
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            if pinnedPlan != nil || cachedCombinedPinnedDiff != nil {
                if let plan = pinnedPlan, let diff = cachedCombinedPinnedDiff {
                    HStack(alignment: .top, spacing: 10) {
                        compactTodoAccordion(for: plan)
                            .layoutPriority(1)
                        diffIndicatorButton(for: diff)
                    }
                } else {
                    if let plan = pinnedPlan {
                        compactTodoAccordion(for: plan)
                    }

                    if let diff = cachedCombinedPinnedDiff {
                        diffIndicatorButton(for: diff)
                    }
                }
            }
        }
        .padding(.horizontal, 12)
        .padding(.top, 8)
        .sheet(item: $selectedDiff) { presentedDiff in
            ConversationDiffDetailSheet(
                title: presentedDiff.title,
                diff: presentedDiff.diff ?? "",
                sections: presentedDiff.sections
            )
        }
        .onChange(of: pinnedDiffTaskKey, initial: false) { _, _ in
            cachedCombinedPinnedDiff = Self.buildCombinedPinnedDiff(from: items)
        }
    }

    private var pinnedPlan: ConversationItem? {
        items.last(where: {
            if case .todoList(let data) = $0.content {
                return !data.steps.isEmpty
            }
            return false
        })
    }

    private var pinnedDiffTaskKey: [Int] {
        items.map(\.renderDigest)
    }

    private static func buildCombinedPinnedDiff(from items: [ConversationItem]) -> PresentedDiff? {
        let rawSections = items.flatMap { item -> [PresentedDiffSection] in
            switch item.content {
            case .fileChange(let data):
                return data.changes.compactMap { change in
                    let diff = change.diff.trimmingCharacters(in: .whitespacesAndNewlines)
                    guard !diff.isEmpty else { return nil }
                    return PresentedDiffSection(
                        title: workspaceTitle(for: change.path),
                        diff: diff
                    )
                }
            case .turnDiff(let data):
                let diff = data.diff.trimmingCharacters(in: .whitespacesAndNewlines)
                return diff.isEmpty ? [] : presentedDiffSections(from: diff)
            default:
                return []
            }
        }

        let sections = mergePresentedDiffSections(rawSections)
        guard !sections.isEmpty else { return nil }
        let stats = sections.reduce(into: DiffStats(additions: 0, deletions: 0)) { partial, section in
            let sectionStats = DiffStats(diff: section.diff)
            partial = DiffStats(
                additions: partial.additions + sectionStats.additions,
                deletions: partial.deletions + sectionStats.deletions
            )
        }
        return PresentedDiff(
            id: "session-diff",
            title: "Session Diff",
            diff: nil,
            stats: stats,
            sections: sections
        )
    }

    @ViewBuilder
    private func compactTodoAccordion(for item: ConversationItem) -> some View {
        if case .todoList(let data) = item.content {
            let completed = data.completedCount
            let total = data.steps.count
            let summary: String = {
                if completed == 0 {
                    return "To do list created with \(total) tasks"
                }
                return "\(completed) out of \(total) tasks completed"
            }()

            VStack(alignment: .leading, spacing: 0) {
                Button {
                    withAnimation(.easeInOut(duration: 0.2)) {
                        todoExpanded.toggle()
                    }
                } label: {
                    HStack(spacing: 8) {
                        Image(systemName: completed == total && total > 0 ? "checkmark.circle.fill" : "checklist")
                            .remoraFont(size: 11, weight: .semibold)
                            .foregroundColor(completed == total && total > 0 ? RemoraTheme.success : RemoraTheme.accent)
                        Text(summary)
                            .remoraFont(.caption, weight: .semibold)
                            .foregroundColor(RemoraTheme.textPrimary)
                            .lineLimit(2)
                            .frame(maxWidth: .infinity, alignment: .leading)
                        Image(systemName: "chevron.down")
                            .remoraFont(size: 11, weight: .medium)
                            .foregroundColor(RemoraTheme.textMuted)
                            .rotationEffect(.degrees(todoExpanded ? 180 : 0))
                    }
                    .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .padding(.horizontal, 12)
                .padding(.vertical, 10)

                if todoExpanded {
                    VStack(alignment: .leading, spacing: 8) {
                        ForEach(Array(data.steps.enumerated()), id: \.offset) { _, step in
                            HStack(alignment: .top, spacing: 8) {
                                compactTodoStatusView(for: step.status)
                                    .padding(.top, 2)
                                RemoraMarkdownView(
                                    markdown: step.step,
                                    style: .content,
                                    bodySize: 12,
                                    codeSize: 11
                                )
                                    .strikethrough(step.status == .completed, color: RemoraTheme.textMuted)
                                    .frame(maxWidth: .infinity, alignment: .leading)
                            }
                        }
                    }
                    .padding(.horizontal, 12)
                    .padding(.bottom, 10)
                    .transition(.sectionReveal)
                }
            }
        }
    }

    @ViewBuilder
    private func compactTodoStatusView(for status: HydratedPlanStepStatus) -> some View {
        switch status {
        case .pending:
            Image(systemName: "circle")
                .remoraFont(size: 10, weight: .semibold)
                .foregroundColor(RemoraTheme.textMuted)
        case .inProgress:
            ProgressView()
                .controlSize(.mini)
                .tint(RemoraTheme.warning)
                .frame(width: 10, height: 10)
        case .completed:
            Image(systemName: "checkmark.circle.fill")
                .remoraFont(size: 10, weight: .semibold)
                .foregroundColor(RemoraTheme.success)
        }
    }

    private func diffIndicatorButton(for presented: PresentedDiff) -> some View {
        Button {
            selectedDiff = presented
        } label: {
            DiffIndicatorLabel(
                additions: presented.stats.additions,
                deletions: presented.stats.deletions
            )
        }
        .buttonStyle(.plain)
        .fixedSize(horizontal: true, vertical: false)
    }

}


struct PresentedDiff: Identifiable {
    let id: String
    let title: String
    let diff: String?
    let stats: DiffStats
    let sections: [PresentedDiffSection]
}

struct PresentedDiffSection: Identifiable {
    let id: String
    let title: String
    let diff: String

    init(title: String, diff: String) {
        self.title = title
        self.diff = diff
        self.id = "\(title)|\(diff.hashValue)"
    }
}

struct DiffStats: Equatable {
    let additions: Int
    let deletions: Int

    var hasChanges: Bool {
        additions > 0 || deletions > 0
    }

    init(additions: Int, deletions: Int) {
        self.additions = additions
        self.deletions = deletions
    }

    /// Cheap stats-only parse — no per-line allocation.
    init(diff: String) {
        var adds = 0
        var dels = 0
        for line in diff.split(separator: "\n", omittingEmptySubsequences: false) {
            if line.hasPrefix("+"), !line.hasPrefix("+++") { adds += 1 }
            else if line.hasPrefix("-"), !line.hasPrefix("---") { dels += 1 }
        }
        self.additions = adds
        self.deletions = dels
    }
}

struct DiffIndicatorLabel: View {
    private let stats: DiffStats

    init(diff: String) {
        self.stats = DiffStats(diff: diff)
    }

    init(additions: Int, deletions: Int) {
        self.stats = DiffStats(additions: additions, deletions: deletions)
    }

    var body: some View {
        HStack(spacing: 8) {
            Image(systemName: "arrow.left.arrow.right")
                .remoraFont(size: 11, weight: .semibold)
                .foregroundColor(RemoraTheme.accent)

            if stats.hasChanges {
                HStack(spacing: 6) {
                    Text("+\(stats.additions)")
                        .remoraFont(.caption2, weight: .semibold)
                        .foregroundColor(RemoraTheme.success)
                    Text("-\(stats.deletions)")
                        .remoraFont(.caption2, weight: .semibold)
                        .foregroundColor(RemoraTheme.danger)
                }
            } else {
                Text("Diff")
                    .remoraFont(.caption2, weight: .semibold)
                    .foregroundColor(RemoraTheme.textSecondary)
            }
        }
        .padding(.horizontal, 10)
        .padding(.vertical, 8)
        .background(RemoraTheme.surface.opacity(0.72), in: Capsule())
        .fixedSize(horizontal: true, vertical: false)
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(accessibilityLabel)
    }

    private var accessibilityLabel: String {
        if stats.hasChanges {
            return "Show diff details. \(stats.additions) additions, \(stats.deletions) deletions."
        }
        return "Show diff details."
    }
}

private struct DiffLine: Identifiable {
    enum Kind {
        case addition, deletion, hunk, context

        var foregroundColor: Color {
            switch self {
            case .addition: RemoraTheme.success
            case .deletion: RemoraTheme.danger
            case .hunk: RemoraTheme.accentStrong
            case .context: RemoraTheme.textBody
            }
        }

        var backgroundColor: Color {
            switch self {
            case .addition: RemoraTheme.success.opacity(0.12)
            case .deletion: RemoraTheme.danger.opacity(0.12)
            case .hunk: RemoraTheme.accentStrong.opacity(0.12)
            case .context: RemoraTheme.codeBackground.opacity(0.72)
            }
        }
    }

    let id: Int
    let text: String
    let kind: Kind
}

struct ConversationDiffDetailSheet: View {
    let title: String
    let stats: DiffStats
    private let sections: [PresentedDiffSectionModel]
    @Environment(ThemeManager.self) private var themeManager
    @Environment(\.dismiss) private var dismiss
    @State private var collapsedSectionIDs: Set<String> = []
    private let fullDiffFontSize = RemoraFont.conversationDiffPointSize
    private let maxStickyDiffSections = 8
    private let maxStickyDiffCharacters = 20_000

    init(title: String, diff: String, sections: [PresentedDiffSection]) {
        self.title = title
        let sectionModels = sections.isEmpty
            ? [PresentedDiffSectionModel(PresentedDiffSection(title: "", diff: diff))]
            : sections.map(PresentedDiffSectionModel.init)
        self.stats = DiffStats(
            additions: sectionModels.reduce(0) { $0 + $1.stats.additions },
            deletions: sectionModels.reduce(0) { $0 + $1.stats.deletions }
        )
        self.sections = sectionModels
        _collapsedSectionIDs = State(
            initialValue: Set(
                sectionModels
                    .filter { !$0.title.isEmpty }
                    .map(\.id)
            )
        )
    }

    var body: some View {
        NavigationStack {
            VStack(alignment: .leading, spacing: 0) {
                HStack(spacing: 8) {
                    Text("+\(stats.additions)")
                        .remoraFont(.caption2, weight: .semibold)
                        .foregroundColor(RemoraTheme.success)
                    Text("-\(stats.deletions)")
                        .remoraFont(.caption2, weight: .semibold)
                        .foregroundColor(RemoraTheme.danger)
                }
                .padding(.horizontal, 16)
                .padding(.top, 12)
                .padding(.bottom, 8)

                ScrollView(.vertical) {
                    LazyVStack(
                        alignment: .leading,
                        spacing: 8,
                        pinnedViews: usesStickyHeaders ? [.sectionHeaders] : []
                    ) {
                        ForEach(sections) { section in
                            if section.title.isEmpty {
                                diffSectionBody(section)
                            } else {
                                Section {
                                    diffSectionBody(section)
                                } header: {
                                    diffSectionHeader(section)
                                }
                            }
                        }
                    }
                    .padding(.horizontal, 16)
                    .padding(.bottom, 16)
                }
            }
            .background(RemoraTheme.backgroundGradient.ignoresSafeArea())
            .navigationTitle(title)
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .topBarTrailing) {
                    Button("Done") {
                        dismiss()
                    }
                }
            }
        }
        .presentationDetents([.medium, .large])
        .id(themeManager.themeVersion)
    }

    private var usesStickyHeaders: Bool {
        guard sections.count <= maxStickyDiffSections else { return false }
        return sections.reduce(0) { $0 + $1.diff.count } <= maxStickyDiffCharacters
    }

    @ViewBuilder
    private func diffSection(_ section: PresentedDiffSectionModel) -> some View {
        let isExpanded = !collapsedSectionIDs.contains(section.id)

        VStack(alignment: .leading, spacing: 6) {
            if isExpanded {
                ScrollView(.horizontal, showsIndicators: true) {
                    SyntaxHighlightedDiffText(
                        diff: section.diff,
                        titleHint: section.title.isEmpty ? nil : section.title,
                        fontSize: fullDiffFontSize
                    )
                        .padding(.horizontal, 8)
                        .padding(.vertical, 6)
                }
                .background(RemoraTheme.codeBackground.opacity(0.72))
                .clipShape(RoundedRectangle(cornerRadius: 10, style: .continuous))
            }
        }
    }

    private func diffSectionHeader(_ section: PresentedDiffSectionModel) -> some View {
        let isExpanded = !collapsedSectionIDs.contains(section.id)

        return Button {
            withAnimation(.easeInOut(duration: 0.2)) {
                toggleSection(section.id)
            }
        } label: {
            HStack(spacing: 8) {
                Text(section.title)
                    .remoraFont(.caption2, weight: .bold)
                    .foregroundColor(RemoraTheme.textSecondary)
                    .textCase(.uppercase)
                Spacer(minLength: 0)
                Text("+\(section.stats.additions)")
                    .remoraFont(.caption2, weight: .semibold)
                    .foregroundColor(RemoraTheme.success)
                Text("-\(section.stats.deletions)")
                    .remoraFont(.caption2, weight: .semibold)
                    .foregroundColor(RemoraTheme.danger)
                Image(systemName: isExpanded ? "chevron.up" : "chevron.down")
                    .remoraFont(size: 10, weight: .medium)
                    .foregroundColor(RemoraTheme.textMuted)
            }
            .contentShape(Rectangle())
            .padding(.vertical, 6)
            .padding(.horizontal, 12)
            .background(RemoraTheme.backgroundGradient)
        }
        .buttonStyle(.plain)
    }

    private func diffSectionBody(_ section: PresentedDiffSectionModel) -> some View {
        diffSection(section)
    }

    private func toggleSection(_ id: String) {
        if collapsedSectionIDs.contains(id) {
            collapsedSectionIDs.remove(id)
        } else {
            collapsedSectionIDs.insert(id)
        }
    }
}

private struct PresentedDiffSectionModel: Identifiable {
    let id: String
    let title: String
    let diff: String
    let stats: DiffStats

    init(_ section: PresentedDiffSection) {
        self.id = section.id
        self.title = section.title
        self.diff = section.diff
        self.stats = DiffStats(diff: section.diff)
    }
}

func presentedDiffSections(from diff: String) -> [PresentedDiffSection] {
    let normalized = diff.trimmingCharacters(in: .whitespacesAndNewlines)
    guard !normalized.isEmpty else { return [] }

    let lines = normalized.components(separatedBy: .newlines)
    let splitIndices = lines.enumerated().compactMap { index, line -> Int? in
        line.hasPrefix("diff --git ") ? index : nil
    }

    if !splitIndices.isEmpty {
        return splitIndices.enumerated().compactMap { offset, start in
            let end = offset + 1 < splitIndices.count ? splitIndices[offset + 1] : lines.count
            let chunk = Array(lines[start..<end]).joined(separator: "\n").trimmingCharacters(in: .whitespacesAndNewlines)
            guard !chunk.isEmpty else { return nil }
            return PresentedDiffSection(title: diffSectionTitle(from: chunk), diff: chunk)
        }
    }

    return [PresentedDiffSection(title: diffSectionTitle(from: normalized), diff: normalized)]
}

private func mergePresentedDiffSections(_ sections: [PresentedDiffSection]) -> [PresentedDiffSection] {
    var orderedTitles: [String] = []
    var mergedByTitle: [String: String] = [:]
    var passthrough: [PresentedDiffSection] = []

    for section in sections {
        let normalizedTitle = section.title.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !normalizedTitle.isEmpty else {
            passthrough.append(section)
            continue
        }

        if let existing = mergedByTitle[normalizedTitle] {
            mergedByTitle[normalizedTitle] = existing + "\n\n" + section.diff
        } else {
            orderedTitles.append(normalizedTitle)
            mergedByTitle[normalizedTitle] = section.diff
        }
    }

    let merged = orderedTitles.compactMap { title -> PresentedDiffSection? in
        guard let diff = mergedByTitle[title] else { return nil }
        return PresentedDiffSection(title: title, diff: diff)
    }

    return merged + passthrough
}

private func diffSectionTitle(from diff: String) -> String {
    for line in diff.components(separatedBy: .newlines) {
        if line.hasPrefix("diff --git ") {
            let parts = line.split(separator: " ")
            if let candidate = parts.last {
                return stripDiffPathPrefix(String(candidate))
            }
        }
        if line.hasPrefix("+++ ") {
            let candidate = String(line.dropFirst(4))
            if candidate != "/dev/null" {
                return stripDiffPathPrefix(candidate)
            }
        }
        if line.hasPrefix("--- ") {
            let candidate = String(line.dropFirst(4))
            if candidate != "/dev/null" {
                return stripDiffPathPrefix(candidate)
            }
        }
    }
    return ""
}

private func stripDiffPathPrefix(_ path: String) -> String {
    if path.hasPrefix("a/") || path.hasPrefix("b/") {
        return String(path.dropFirst(2))
    }
    return path
}
