import SwiftUI

struct ConversationExplorationGroupRow: View {
    @Environment(\.textScale) private var textScale

    let id: String
    let items: [ConversationItem]
    let showsCollapsedPreview: Bool
    let displayMode: ConversationDetailDisplayMode

    @State private var expanded = false

    var body: some View {
        let entries = explorationEntries

        VStack(alignment: .leading, spacing: 6) {
            Button(action: toggleExpanded) {
                HStack(spacing: 8) {
                    Image(systemName: "magnifyingglass")
                        .remoraFont(size: 12, weight: .semibold)
                        .foregroundColor(isActive ? RemoraTheme.warning : RemoraTheme.textSecondary)
                    Text(verbatim: summaryText)
                        .remoraFont(.caption)
                        .foregroundColor(RemoraTheme.textSystem)
                        .lineLimit(1)
                        .truncationMode(.tail)
                        .frame(maxWidth: .infinity, alignment: .leading)
                    Image(systemName: expanded ? "chevron.up" : "chevron.down")
                        .remoraFont(size: 11, weight: .medium)
                        .foregroundColor(RemoraTheme.textMuted)
                }
            }
            .buttonStyle(.plain)

            if expanded {
                VStack(alignment: .leading, spacing: 4) {
                    ForEach(entries) { entry in
                        HStack(alignment: .top, spacing: 8) {
                            Circle()
                                .fill(entry.isInProgress ? RemoraTheme.warning : RemoraTheme.textMuted)
                                .frame(width: explorationBulletSize, height: explorationBulletSize)
                                .padding(.top, explorationBulletTopPadding)
                            Text(verbatim: entry.label)
                                .remoraFont(.caption)
                                .foregroundColor(RemoraTheme.textSecondary)
                                .frame(maxWidth: .infinity, alignment: .leading)
                        }
                    }
                }
            } else if showsCollapsedPreview && !entries.isEmpty {
                ScrollViewReader { proxy in
                    ScrollView(.vertical, showsIndicators: false) {
                        VStack(alignment: .leading, spacing: 4) {
                            ForEach(entries) { entry in
                                HStack(alignment: .top, spacing: 8) {
                                    Circle()
                                        .fill(entry.isInProgress ? RemoraTheme.warning : RemoraTheme.textMuted)
                                        .frame(width: explorationBulletSize, height: explorationBulletSize)
                                        .padding(.top, explorationBulletTopPadding)
                                    Text(verbatim: displayedCollapsedLabel(for: entry))
                                        .remoraFont(.caption)
                                        .foregroundColor(RemoraTheme.textSecondary)
                                        .lineLimit(1)
                                        .truncationMode(.tail)
                                        .frame(maxWidth: .infinity, alignment: .leading)
                                }
                            }

                            Color.clear
                                .frame(height: 1)
                                .id(bottomAnchorId)
                        }
                        .padding(.horizontal, 8)
                        .padding(.vertical, 6)
                    }
                    .frame(maxHeight: collapsedPreviewHeight)
                    .background(RemoraTheme.surface.opacity(0.6))
                    .clipShape(RoundedRectangle(cornerRadius: 8, style: .continuous))
                    .overlay(alignment: .top) {
                        LinearGradient(
                            colors: [RemoraTheme.surface.opacity(0.92), RemoraTheme.surface.opacity(0)],
                            startPoint: .top,
                            endPoint: .bottom
                        )
                        .frame(height: 16)
                        .clipShape(RoundedRectangle(cornerRadius: 8, style: .continuous))
                        .allowsHitTesting(false)
                    }
                    .onAppear {
                        scrollToBottom(proxy)
                    }
                    .onChange(of: collapsedPreviewScrollSignature) { _, _ in
                        scrollToBottom(proxy, animated: true)
                    }
                }
            }
        }
        .padding(.horizontal, 12)
        .padding(.vertical, 6)
        .opacity(displayMode.rendersRows ? 1 : 0)
        .frame(height: displayMode.rendersRows ? nil : 0)
        .clipped()
        .onChange(of: showsCollapsedPreview) { _, newValue in
            guard !newValue else { return }
            expanded = false
        }
    }

    private var summaryText: String {
        let prefix = isActive ? "Exploring" : "Explored"
        return explorationSummaryText(prefix: prefix)
    }

    private var explorationBulletSize: CGFloat {
        6 * textScale
    }

    private var explorationBulletTopPadding: CGFloat {
        5 * textScale
    }

    private var collapsedPreviewHeight: CGFloat {
        (RemoraFont.uiMonoFont(size: 12 * textScale).lineHeight * 3) + 18
    }

    private var bottomAnchorId: String {
        "\(id)-exploration-bottom"
    }

    private var collapsedPreviewScrollSignature: String {
        explorationEntries
            .map { "\($0.id)|\($0.label)|\($0.isInProgress)" }
            .joined(separator: "\n")
    }

    private var isActive: Bool {
        explorationEntries.contains(where: \.isInProgress)
    }

    private func toggleExpanded() {
        withAnimation(.easeInOut(duration: 0.2)) {
            expanded.toggle()
        }
    }

    private func displayedCollapsedLabel(for entry: ExplorationDisplayEntry) -> String {
        let collapsed = entry.label
            .replacingOccurrences(of: "\n", with: " ")
            .replacingOccurrences(of: "\r", with: " ")
            .trimmingCharacters(in: .whitespacesAndNewlines)
        if collapsed.count <= 140 {
            return collapsed
        }
        let cutoff = collapsed.index(collapsed.startIndex, offsetBy: 140)
        return "\(collapsed[..<cutoff])..."
    }

    private var explorationEntries: [ExplorationDisplayEntry] {
        items.flatMap { item -> [ExplorationDisplayEntry] in
            guard case .commandExecution(let data) = item.content else { return [] }
            if data.actions.isEmpty {
                return [
                    ExplorationDisplayEntry(
                        id: "\(item.id)-command",
                        label: data.command,
                        isInProgress: data.isInProgress
                    )
                ]
            }
            return data.actions.enumerated().map { index, action in
                ExplorationDisplayEntry(
                    id: "\(item.id)-\(index)",
                    label: explorationLabel(for: action, fallback: data.command),
                    isInProgress: data.isInProgress
                )
            }
        }
    }

    private func explorationSummaryText(prefix: String) -> String {
        var readCount = 0
        var searchCount = 0
        var listingCount = 0
        var fallbackCount = 0

        for item in items {
            guard case .commandExecution(let data) = item.content else { continue }
            if data.actions.isEmpty {
                fallbackCount += 1
                continue
            }
            for action in data.actions {
                switch action.kind {
                case .read:
                    readCount += 1
                case .search:
                    searchCount += 1
                case .listFiles:
                    listingCount += 1
                case .unknown:
                    fallbackCount += 1
                }
            }
        }

        var parts: [String] = []
        if readCount > 0 {
            parts.append("\(readCount) \(readCount == 1 ? "file" : "files")")
        }
        if searchCount > 0 {
            parts.append("\(searchCount) \(searchCount == 1 ? "search" : "searches")")
        }
        if listingCount > 0 {
            parts.append("\(listingCount) \(listingCount == 1 ? "listing" : "listings")")
        }
        if fallbackCount > 0 {
            parts.append("\(fallbackCount) \(fallbackCount == 1 ? "step" : "steps")")
        }
        if parts.isEmpty {
            let count = explorationEntries.count
            return count == 1 ? "\(prefix) 1 exploration step" : "\(prefix) \(count) exploration steps"
        }
        return "\(prefix) \(parts.joined(separator: ", "))"
    }

    private func explorationLabel(for action: ConversationCommandAction, fallback: String) -> String {
        let suffix = explorationCommandSuffix(for: action)
        switch action.kind {
        case .read:
            return action.path.map { "Read \(workspaceTitle(for: $0))\(suffix)" } ?? fallback
        case .search:
            if let query = action.query, let path = action.path {
                return "Searched for \(query) in \(workspaceTitle(for: path))\(suffix)"
            }
            if let query = action.query {
                return "Searched for \(query)\(suffix)"
            }
            return fallback
        case .listFiles:
            return action.path.map { "Listed files in \(workspaceTitle(for: $0))\(suffix)" } ?? fallback
        case .unknown:
            return fallback
        }
    }

    private func explorationCommandSuffix(for action: ConversationCommandAction) -> String {
        let command = action.command.trimmingCharacters(in: .whitespacesAndNewlines)
        guard command.hasSuffix(")"),
              let start = command.range(of: " (", options: .backwards)?.lowerBound else {
            return ""
        }
        return String(command[start...])
    }

    private func scrollToBottom(_ proxy: ScrollViewProxy, animated: Bool = false) {
        DispatchQueue.main.async {
            if animated {
                withAnimation(.easeOut(duration: 0.16)) {
                    proxy.scrollTo(bottomAnchorId, anchor: .bottom)
                }
            } else {
                proxy.scrollTo(bottomAnchorId, anchor: .bottom)
            }
        }
    }
}

private struct ExplorationDisplayEntry: Identifiable {
    let id: String
    let label: String
    let isInProgress: Bool
}

struct ConversationCommandExecutionRow: View {
    let data: ConversationCommandExecutionData
    let isPreferredExpanded: Bool
    let displayMode: ConversationDetailDisplayMode

    @State private var expanded: Bool

    init(
        data: ConversationCommandExecutionData,
        isPreferredExpanded: Bool,
        displayMode: ConversationDetailDisplayMode
    ) {
        self.data = data
        self.isPreferredExpanded = isPreferredExpanded
        self.displayMode = displayMode
        _expanded = State(initialValue: isPreferredExpanded)
    }

    var body: some View {
        VStack(alignment: .leading, spacing: expanded ? 8 : 0) {
            shellHeader
            if expanded {
                ConversationCommandOutputViewport(
                    output: renderedOutput,
                    status: data.status.toolCallStatus,
                    durationText: nil
                )
            }
        }
        .padding(.horizontal, 12)
        .padding(.vertical, 9)
        .background(RemoraTheme.surface)
        .overlay(
            RoundedRectangle(cornerRadius: 12, style: .continuous)
                .stroke(RemoraTheme.border, lineWidth: 0.5)
        )
        .clipShape(RoundedRectangle(cornerRadius: 12, style: .continuous))
        .animation(.spring(duration: 0.35, bounce: 0.15), value: expanded)
        .onChange(of: isPreferredExpanded) { _, newValue in
            expanded = newValue
        }
        .onChange(of: displayMode) { _, newValue in
            expanded = newValue == .expanded || data.isInProgress || data.status == .failed
        }
    }

    private var shellHeader: some View {
        HStack(alignment: .firstTextBaseline, spacing: 8) {
            Text("$")
                .remoraMonoFont(size: 12, weight: .semibold)
                .foregroundColor(RemoraTheme.warning)

            Text(expanded ? displayedCommand : collapsedCommand)
                .remoraMonoFont(size: 12)
                .foregroundColor(RemoraTheme.textSystem)
                .textSelection(.enabled)
                .lineLimit(expanded ? nil : 1)
                .truncationMode(.tail)
                .frame(maxWidth: .infinity, alignment: .leading)

            if let durationText = formatDuration(data.durationMs), !durationText.isEmpty {
                Text(durationText)
                    .remoraFont(.caption2)
                    .foregroundColor(statusColor)
                    .padding(.horizontal, 7)
                    .padding(.vertical, 2)
                    .background(
                        Capsule(style: .continuous)
                            .fill(statusColor.opacity(0.10))
                    )
                    .overlay(
                        Capsule(style: .continuous)
                            .stroke(statusColor.opacity(0.22), lineWidth: 0.5)
                    )
                    .accessibilityLabel(durationAccessibilityLabel(durationText))
            }

            Image(systemName: expanded ? "chevron.up" : "chevron.down")
                .remoraFont(size: 11, weight: .medium)
                .foregroundColor(RemoraTheme.textMuted)
        }
        .contentShape(Rectangle())
        .onTapGesture {
            withAnimation(.easeInOut(duration: 0.2)) {
                expanded.toggle()
            }
        }
    }

    private var renderedOutput: String {
        let trimmed = data.output?.trimmingCharacters(in: .newlines) ?? ""
        if !trimmed.isEmpty {
            return trimmed
        }
        return data.isInProgress ? "Waiting for output…" : "No output"
    }

    private var displayedCommand: String {
        let trimmed = data.command.trimmingCharacters(in: .whitespacesAndNewlines)
        return trimmed.isEmpty ? "command" : trimmed
    }

    private var collapsedCommand: String {
        let collapsed = displayedCommand
            .components(separatedBy: .whitespacesAndNewlines)
            .filter { !$0.isEmpty }
            .joined(separator: " ")
        return collapsed.isEmpty ? "command" : collapsed
    }

    private var statusColor: Color { data.status.toolCallStatus.themeColor }

    private func durationAccessibilityLabel(_ duration: String) -> String {
        switch data.status.toolCallStatus {
        case .completed:
            return "\(duration), completed"
        case .inProgress:
            return "\(duration), in progress"
        case .failed:
            return "\(duration), failed"
        case .unknown:
            return duration
        }
    }
}

private struct ConversationCommandOutputViewport: View {
    let output: String
    let status: ToolCallStatus
    let durationText: String?
    @Environment(\.textScale) private var textScale
    @State private var expandedLongOutput = false

    private let bottomAnchorId = "command-output-bottom"
    private let maxVisibleTextCharacters = 2_000

    private var lineFontSize: CGFloat {
        11 * textScale
    }

    private var maxViewportHeight: CGFloat {
        (RemoraFont.uiMonoFont(size: lineFontSize).lineHeight * 3) + 16
    }

    private var viewportHeight: CGFloat {
        let lh = RemoraFont.uiMonoFont(size: lineFontSize).lineHeight
        let lines = max(1, visibleOutput.split(separator: "\n", omittingEmptySubsequences: false).count)
        let natural = (lh * CGFloat(min(lines, 3))) + 16
        return min(natural, maxViewportHeight)
    }

    var body: some View {
        ScrollViewReader { proxy in
            VStack(alignment: .leading, spacing: 6) {
                ScrollView(.vertical, showsIndicators: false) {
                    VStack(alignment: .leading, spacing: 0) {
                        Text(verbatim: visibleOutput)
                            .remoraMonoFont(size: 12)
                            .foregroundColor(RemoraTheme.textSecondary)
                            .textSelection(.enabled)
                            .frame(maxWidth: .infinity, alignment: .leading)

                        Color.clear
                            .frame(height: 1)
                            .id(bottomAnchorId)
                    }
                    .padding(.horizontal, 10)
                    .padding(.top, 8)
                    .padding(.bottom, 12)
                }
                .frame(height: viewportHeight)
                .background(RemoraTheme.codeBackground.opacity(0.78))
                .clipShape(RoundedRectangle(cornerRadius: 10, style: .continuous))
                .overlay(alignment: .top) {
                    LinearGradient(
                        colors: [RemoraTheme.codeBackground.opacity(0.96), RemoraTheme.codeBackground.opacity(0)],
                        startPoint: .top,
                        endPoint: .bottom
                    )
                    .frame(height: 18)
                    .clipShape(RoundedRectangle(cornerRadius: 10, style: .continuous))
                    .allowsHitTesting(false)
                }
                .overlay(alignment: .bottomTrailing) {
                    if let durationText, !durationText.isEmpty {
                        Text(durationText)
                            .foregroundColor(statusColor)
                            .accessibilityLabel(durationAccessibilityLabel(durationText))
                            .remoraFont(.caption2)
                            .padding(.horizontal, 10)
                            .padding(.vertical, 6)
                            .background(alignment: .bottom) {
                                LinearGradient(
                                    colors: [.clear, RemoraTheme.codeBackground.opacity(0.94)],
                                    startPoint: .top,
                                    endPoint: .bottom
                                )
                            }
                        }
                    }
                .overlay {
                    RoundedRectangle(cornerRadius: 10, style: .continuous)
                        .stroke(RemoraTheme.border.opacity(0.35), lineWidth: 1)
                }
                .onAppear {
                    scrollToBottom(proxy)
                }
                .onChange(of: output) { _, _ in
                    expandedLongOutput = false
                    scrollToBottom(proxy, animated: true)
                }
                .onChange(of: expandedLongOutput) { _, _ in
                    scrollToBottom(proxy, animated: true)
                }

                if shouldLimitOutput {
                    Button {
                        withAnimation(.easeInOut(duration: 0.18)) {
                            expandedLongOutput.toggle()
                        }
                    } label: {
                        Text(expandedLongOutput ? "Show less" : "Show more")
                            .remoraFont(.caption2, weight: .semibold)
                            .foregroundColor(RemoraTheme.accentForegroundOnSurface)
                    }
                    .buttonStyle(.plain)
                    .accessibilityLabel(expandedLongOutput ? "Show less command output" : "Show more command output")
                }
            }
        }
    }

    private var visibleOutput: String {
        guard shouldLimitOutput, !expandedLongOutput else {
            return output
        }
        if usesTailPreview {
            return String(output.suffix(maxVisibleTextCharacters))
        }
        return String(output.prefix(maxVisibleTextCharacters))
    }

    private var shouldLimitOutput: Bool {
        output.count > maxVisibleTextCharacters
    }

    private var usesTailPreview: Bool {
        switch status {
        case .inProgress:
            return true
        case .completed, .failed, .unknown:
            return false
        }
    }

    private var statusColor: Color { status.themeColor }

    private func durationAccessibilityLabel(_ duration: String) -> String {
        switch status {
        case .completed:
            return "\(duration), completed"
        case .inProgress:
            return "\(duration), in progress"
        case .failed:
            return "\(duration), failed"
        case .unknown:
            return duration
        }
    }

    private func scrollToBottom(_ proxy: ScrollViewProxy, animated: Bool = false) {
        DispatchQueue.main.async {
            if animated {
                withAnimation(.easeOut(duration: 0.16)) {
                    proxy.scrollTo(bottomAnchorId, anchor: .bottom)
                }
            } else {
                proxy.scrollTo(bottomAnchorId, anchor: .bottom)
            }
        }
    }
}

func formatDuration(_ durationMs: Int?) -> String? {
    guard let durationMs, durationMs >= 0 else { return nil }
    if durationMs >= 1_000 {
        return String(format: "%.1fs", Double(durationMs) / 1_000.0)
    }
    return "\(durationMs)ms"
}

private extension ToolCallStatus {
    var themeColor: Color {
        switch self {
        case .completed:
            return RemoraTheme.success
        case .inProgress:
            return RemoraTheme.warning
        case .failed:
            return RemoraTheme.danger
        case .unknown:
            return RemoraTheme.textSecondary
        }
    }
}
