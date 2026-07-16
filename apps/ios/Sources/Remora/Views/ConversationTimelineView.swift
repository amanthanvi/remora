import SwiftUI
import HairballUI
import UIKit

enum ConversationLiveDetailRetentionPolicy {
    static func retainedRichDetailItemIDs(for items: [ConversationItem]) -> Set<String> {
        var retained = Set<String>()

        if let active = items.last(where: { $0.liveDetailStatus == .inProgress }) {
            retained.insert(active.id)
        }

        if let latestCompleted = items.reversed().first(where: { item in
            guard let status = item.liveDetailStatus else { return false }
            return status != .inProgress
        }) {
            retained.insert(latestCompleted.id)
        }

        return retained
    }
}

struct ConversationTurnTimeline: View {
    @AppStorage(ConversationDisplayPreferenceKey.reasoning) private var reasoningDisplayModeRaw = ConversationDetailDisplayMode.collapsed.rawValue
    @AppStorage(ConversationDisplayPreferenceKey.commands) private var commandDisplayModeRaw = ConversationDetailDisplayMode.collapsed.rawValue
    @AppStorage(ConversationDisplayPreferenceKey.tools) private var toolDisplayModeRaw = ConversationDetailDisplayMode.collapsed.rawValue

    let items: [ConversationItem]
    let isLive: Bool
    let serverId: String
    let originThreadId: String?
    let agentDirectoryVersion: UInt64
    let messageActionsDisabled: Bool
    let onStreamingSnapshotRendered: (() -> Void)?
    let onLiveContentLayoutChanged: (() -> Void)?
    let resolveTargetLabel: (String) -> String?
    let onWidgetPrompt: (String) -> Void
    let onEditUserItem: (ConversationItem) -> Void
    let onForkFromUserItem: (ConversationItem) -> Void
    var onOpenConversation: ((ThreadKey) -> Void)? = nil

    var body: some View {
        timelineContent
    }

    private var timelineContent: some View {
        let rows = rowDescriptors
        let retainedRichDetailItemIDs = ConversationLiveDetailRetentionPolicy.retainedRichDetailItemIDs(for: items)
        let commandDisplayMode = ConversationDetailDisplayMode.resolve(commandDisplayModeRaw)
        let latestCommandExecutionItemId = rows.reversed().compactMap { row -> String? in
            guard case .item(let item) = row,
                  case .commandExecution(let data) = item.content,
                  !data.isPureExploration else { return nil }
            return item.id
        }.first

        return VStack(alignment: .leading, spacing: 10) {
            ForEach(Array(rows.enumerated()), id: \.element.id) { index, row in
                rowView(
                    row,
                    isLastRow: index == rows.indices.last,
                    isPreferredExpandedCommandRow: row.preferredExpandedCommandRow(
                        latestCommandExecutionItemId: latestCommandExecutionItemId,
                        commandDisplayMode: commandDisplayMode
                    ),
                    retainedRichDetailItemIDs: retainedRichDetailItemIDs
                )
                    .id(row.id)
                    .modifier(RowEntranceModifier(isAssistantRow: row.isAssistantRow))
                    .onGeometryChange(for: CGFloat.self) { geometry in
                        geometry.size.height
                    } action: { oldHeight, newHeight in
                        guard isLive, abs(newHeight - oldHeight) > 0.5 else { return }
                        onLiveContentLayoutChanged?()
                    }
            }
        }
    }

    private var rowDescriptors: [ConversationTimelineRowDescriptor] {
        ConversationTimelineRowDescriptor.mergeConsecutiveExplorationRows(
            ConversationTimelineRowDescriptor.build(from: items)
        )
        .filter {
            $0.isVisible(
                reasoningDisplayMode: reasoningDisplayMode,
                commandDisplayMode: commandDisplayMode,
                toolDisplayMode: toolDisplayMode
            )
        }
    }

    private var streamingAssistantItemId: String? {
        guard isLive else { return nil }
        return items.last(where: \.isAssistantItem)?.id
    }

    private var reasoningDisplayMode: ConversationDetailDisplayMode {
        ConversationDetailDisplayMode.resolve(reasoningDisplayModeRaw)
    }

    private var commandDisplayMode: ConversationDetailDisplayMode {
        ConversationDetailDisplayMode.resolve(commandDisplayModeRaw)
    }

    private var toolDisplayMode: ConversationDetailDisplayMode {
        ConversationDetailDisplayMode.resolve(toolDisplayModeRaw)
    }

    // Returns AnyView rather than `some View` with @ViewBuilder so the result
    // type doesn't fan out to Group<_ConditionalContent<_ConditionalContent<…>, …>>.
    // Time Profiler showed 44% of main-thread CPU in `outlined destroy` of that
    // nested union; AnyView's per-node diff overhead is cheaper than destroying
    // the union every SwiftUI pass.
    private func rowView(
        _ row: ConversationTimelineRowDescriptor,
        isLastRow: Bool,
        isPreferredExpandedCommandRow: Bool,
        retainedRichDetailItemIDs: Set<String>
    ) -> AnyView {
        switch row {
        case .item(let item):
            return AnyView(
                ConversationTimelineItemRow(
                    item: item,
                    serverId: serverId,
                    originThreadId: originThreadId,
                    agentDirectoryVersion: agentDirectoryVersion,
                    isPreferredExpandedCommandRow: isPreferredExpandedCommandRow,
                    isLiveTurn: isLive,
                    isStreamingMessage: item.id == streamingAssistantItemId,
                    shouldPreserveRichDetail: retainedRichDetailItemIDs.contains(item.id),
                    reasoningDisplayMode: reasoningDisplayMode,
                    commandDisplayMode: commandDisplayMode,
                    toolDisplayMode: toolDisplayMode,
                    messageActionsDisabled: messageActionsDisabled,
                    onStreamingSnapshotRendered: item.id == streamingAssistantItemId ? onStreamingSnapshotRendered : nil,
                    onLiveContentLayoutChanged: onLiveContentLayoutChanged,
                    resolveTargetLabel: resolveTargetLabel,
                    onWidgetPrompt: onWidgetPrompt,
                    onEditUserItem: onEditUserItem,
                    onForkFromUserItem: onForkFromUserItem,
                    onOpenConversation: onOpenConversation
                )
                .equatable()
            )
        case .exploration(let id, let items):
            return AnyView(
                ConversationExplorationGroupRow(
                    id: id,
                    items: items,
                    showsCollapsedPreview: isLastRow,
                    displayMode: commandDisplayMode
                )
            )
        case .subagentGroup(_, let merged, _):
            return AnyView(
                SubagentCardView(
                    data: merged,
                    serverId: serverId
                )
            )
        }
    }
}

private enum ConversationTimelineRowDescriptor: Identifiable, Equatable {
    case item(ConversationItem)
    case exploration(id: String, items: [ConversationItem])
    case subagentGroup(id: String, merged: ConversationMultiAgentActionData, sourceItems: [ConversationItem])

    var id: String {
        switch self {
        case .item(let item):
            return item.id
        case .exploration(let id, _):
            return id
        case .subagentGroup(let id, _, _):
            return id
        }
    }

    var isAssistantRow: Bool {
        guard case .item(let item) = self else { return false }
        return item.isAssistantItem
    }

    func preferredExpandedCommandRow(
        latestCommandExecutionItemId: String?,
        commandDisplayMode: ConversationDetailDisplayMode
    ) -> Bool {
        guard commandDisplayMode == .collapsed else {
            return commandDisplayMode == .expanded
        }
        guard case .item(let item) = self,
              case .commandExecution(let data) = item.content,
              !data.isPureExploration else {
            return false
        }
        return item.id == latestCommandExecutionItemId
    }

    func isVisible(
        reasoningDisplayMode: ConversationDetailDisplayMode,
        commandDisplayMode: ConversationDetailDisplayMode,
        toolDisplayMode: ConversationDetailDisplayMode
    ) -> Bool {
        switch self {
        case .item(let item):
            return item.isVisible(
                reasoningDisplayMode: reasoningDisplayMode,
                commandDisplayMode: commandDisplayMode,
                toolDisplayMode: toolDisplayMode
            )
        case .exploration:
            return commandDisplayMode.rendersRows
        case .subagentGroup:
            return toolDisplayMode.rendersRows
        }
    }

    static func build(from items: [ConversationItem]) -> [ConversationTimelineRowDescriptor] {
        var rows: [ConversationTimelineRowDescriptor] = []
        var explorationBuffer: [ConversationItem] = []
        var subagentBuffer: [(item: ConversationItem, data: ConversationMultiAgentActionData)] = []
        var subagentTool: String?

        func flushExplorationBuffer() {
            guard !explorationBuffer.isEmpty else { return }
            let seed = explorationBuffer.first?.id ?? UUID().uuidString
            rows.append(.exploration(id: "exploration-\(seed)", items: explorationBuffer))
            explorationBuffer.removeAll(keepingCapacity: true)
        }

        func flushSubagentBuffer() {
            guard !subagentBuffer.isEmpty else { return }
            if subagentBuffer.count == 1 {
                rows.append(.item(subagentBuffer[0].item))
            } else {
                let seed = subagentBuffer.first?.item.id ?? UUID().uuidString
                // Merge all targets, threadIds, states, pick the latest status
                var mergedTargets: [String] = []
                var mergedThreadIds: [String] = []
                var mergedStates: [ConversationMultiAgentState] = []
                var mergedPrompts: [String] = []
                var latestStatus: AppOperationStatus = .completed
                let tool = subagentBuffer.first?.data.tool ?? "spawnAgent"

                for entry in subagentBuffer {
                    mergedTargets.append(contentsOf: entry.data.targets)
                    mergedThreadIds.append(contentsOf: entry.data.receiverThreadIds)
                    mergedStates.append(contentsOf: entry.data.agentStates)
                    if let p = entry.data.prompt, !p.isEmpty {
                        mergedPrompts.append(p)
                    }
                    if entry.data.isInProgress {
                        latestStatus = .inProgress
                    }
                }

                let merged = ConversationMultiAgentActionData(
                    tool: tool,
                    status: latestStatus,
                    prompt: nil,
                    targets: mergedTargets,
                    receiverThreadIds: mergedThreadIds,
                    agentStates: mergedStates,
                    perAgentPrompts: mergedPrompts
                )
                rows.append(.subagentGroup(
                    id: "subagent-group-\(seed)",
                    merged: merged,
                    sourceItems: subagentBuffer.map(\.item)
                ))
            }
            subagentBuffer.removeAll(keepingCapacity: true)
            subagentTool = nil
        }

        for item in items {
            if item.isVisuallyEmptyNeutralItem {
                continue
            } else if case .multiAgentAction(let data) = item.content {
                let tool = data.tool.lowercased()
                if let currentTool = subagentTool, currentTool == tool {
                    subagentBuffer.append((item, data))
                } else {
                    flushExplorationBuffer()
                    flushSubagentBuffer()
                    subagentBuffer.append((item, data))
                    subagentTool = tool
                }
            } else if case .commandExecution(let data) = item.content, data.isPureExploration {
                flushSubagentBuffer()
                explorationBuffer.append(item)
            } else {
                flushExplorationBuffer()
                flushSubagentBuffer()
                rows.append(.item(item))
            }
        }

        flushExplorationBuffer()
        flushSubagentBuffer()
        return rows
    }

    static func mergeConsecutiveExplorationRows(
        _ rows: [ConversationTimelineRowDescriptor]
    ) -> [ConversationTimelineRowDescriptor] {
        var mergedRows: [ConversationTimelineRowDescriptor] = []
        var explorationAccumulator: (id: String, items: [ConversationItem])?

        func flushAccumulator() {
            guard let accumulator = explorationAccumulator else { return }
            mergedRows.append(
                .exploration(
                    id: accumulator.id,
                    items: accumulator.items
                )
            )
            explorationAccumulator = nil
        }

        for row in rows {
            switch row {
            case .exploration(let id, let items):
                if var existing = explorationAccumulator {
                    existing.items.append(contentsOf: items)
                    explorationAccumulator = existing
                } else {
                    explorationAccumulator = (id: id, items: items)
                }
            case .item(let item) where item.isExplorationCommandItem:
                if var existing = explorationAccumulator {
                    existing.items.append(item)
                    explorationAccumulator = existing
                } else {
                    explorationAccumulator = (id: "exploration-\(item.id)", items: [item])
                }
            default:
                flushAccumulator()
                mergedRows.append(row)
            }
        }

        flushAccumulator()
        return mergedRows
    }
}

private struct RowEntranceModifier: ViewModifier {
    let isAssistantRow: Bool

    func body(content: Content) -> some View {
        if isAssistantRow {
            // The streaming markdown renderer scopes its own
            // `transaction { $0.animation = nil }` internally so that token
            // reveals don't replay on every snapshot. Leaving the row itself
            // unscoped lets sibling layout changes (e.g. a tool card collapse)
            // animate this row's position.
            content
        } else {
            content
                .transition(.asymmetric(
                    insertion: .rowEntranceReveal,
                    removal: .opacity
                ))
        }
    }
}

struct RowEntranceEffect: ViewModifier, Animatable {
    var progress: CGFloat
    var yOffset: CGFloat
    var minScale: CGFloat
    var maxBlur: CGFloat

    var animatableData: CGFloat {
        get { progress }
        set { progress = newValue }
    }

    func body(content: Content) -> some View {
        let clampedProgress = min(max(progress, 0), 1)
        let revealProgress = max(clampedProgress, 0.001)

        content
            .compositingGroup()
            .scaleEffect(
                x: 1,
                y: minScale + ((1 - minScale) * clampedProgress),
                anchor: .topLeading
            )
            .offset(y: yOffset * (1 - clampedProgress))
            .opacity(clampedProgress)
            .blur(radius: maxBlur * (1 - clampedProgress))
            .mask(alignment: .topLeading) {
                Rectangle()
                    .scaleEffect(x: 1, y: revealProgress, anchor: .topLeading)
            }
    }
}

extension AnyTransition {
    static var rowEntranceReveal: AnyTransition {
        .modifier(
            active: RowEntranceEffect(progress: 0, yOffset: 10, minScale: 0.965, maxBlur: 2.5),
            identity: RowEntranceEffect(progress: 1, yOffset: 0, minScale: 1, maxBlur: 0)
        )
    }

    static var sectionReveal: AnyTransition {
        .modifier(
            active: RowEntranceEffect(progress: 0, yOffset: 6, minScale: 0.985, maxBlur: 1.2),
            identity: RowEntranceEffect(progress: 1, yOffset: 0, minScale: 1, maxBlur: 0)
        )
    }
}

private struct ConversationTimelineItemRow: View, Equatable {
    private let renderCache = MessageRenderCache.shared
    @Environment(ThemeManager.self) private var themeManager

    let item: ConversationItem
    let serverId: String
    let originThreadId: String?
    let agentDirectoryVersion: UInt64
    let isPreferredExpandedCommandRow: Bool
    let isLiveTurn: Bool
    let isStreamingMessage: Bool
    let shouldPreserveRichDetail: Bool
    let reasoningDisplayMode: ConversationDetailDisplayMode
    let commandDisplayMode: ConversationDetailDisplayMode
    let toolDisplayMode: ConversationDetailDisplayMode
    let messageActionsDisabled: Bool
    let onStreamingSnapshotRendered: (() -> Void)?
    let onLiveContentLayoutChanged: (() -> Void)?
    let resolveTargetLabel: (String) -> String?
    let onWidgetPrompt: (String) -> Void
    let onEditUserItem: (ConversationItem) -> Void
    let onForkFromUserItem: (ConversationItem) -> Void
    var onOpenConversation: ((ThreadKey) -> Void)? = nil

    static func == (lhs: ConversationTimelineItemRow, rhs: ConversationTimelineItemRow) -> Bool {
        let isAssistant = lhs.item.isAssistantItem
        // For assistant rows: the StreamingRendererCoordinator owns the
        // streaming→finished lifecycle.  Skip digest, richDetail, AND
        // isStreamingMessage so the bubble body never re-evaluates when
        // a tool call arrives and a new assistant message takes over as
        // the "streaming" item.  Re-rendering the bubble would recreate
        // StreamingMarkdownContentView and replay the token reveal.
        let result = lhs.item.id == rhs.item.id &&
            (isAssistant || lhs.item.renderDigest == rhs.item.renderDigest) &&
            (isAssistant || lhs.shouldPreserveRichDetail == rhs.shouldPreserveRichDetail) &&
            (isAssistant || lhs.isStreamingMessage == rhs.isStreamingMessage) &&
            lhs.serverId == rhs.serverId &&
            lhs.originThreadId == rhs.originThreadId &&
            lhs.agentDirectoryVersion == rhs.agentDirectoryVersion &&
            lhs.isPreferredExpandedCommandRow == rhs.isPreferredExpandedCommandRow &&
            lhs.isLiveTurn == rhs.isLiveTurn &&
            lhs.reasoningDisplayMode == rhs.reasoningDisplayMode &&
            lhs.commandDisplayMode == rhs.commandDisplayMode &&
            lhs.toolDisplayMode == rhs.toolDisplayMode &&
            lhs.messageActionsDisabled == rhs.messageActionsDisabled
        return result
    }

    // 16-case switch returns AnyView rather than `some View` so the body type
    // doesn't resolve to a 4-deep `Group<_ConditionalContent<…>>` nested union.
    // Time Profiler on 2026-04-18 showed that union's `outlined destroy` +
    // witness-table accessor accounting for ~49% of main-thread CPU on device.
    var body: AnyView {
        switch item.content {
        case .user(let data):
            return AnyView(userRow(data))
        case .assistant(let data):
            return AnyView(assistantRow(data))
        case .codeReview(let data):
            return AnyView(ConversationCodeReviewRow(data: data))
        case .reasoning(let data):
            guard reasoningDisplayMode.rendersRows else { return AnyView(EmptyView()) }
            return AnyView(ConversationReasoningRow(data: data, displayMode: reasoningDisplayMode))
        case .todoList(let data):
            return AnyView(ConversationTodoListRow(data: data))
        case .proposedPlan(let data):
            return AnyView(ConversationProposedPlanRow(data: data))
        case .commandExecution(let data):
            guard commandDisplayMode.rendersRows else { return AnyView(EmptyView()) }
            return AnyView(commandExecutionRow(data))
        case .fileChange(let data):
            guard toolDisplayMode.rendersRows else { return AnyView(EmptyView()) }
            return AnyView(toolCallRow(makeFileChangeModel(data)))
        case .turnDiff(let data):
            guard toolDisplayMode.rendersRows else { return AnyView(EmptyView()) }
            return AnyView(ConversationTurnDiffRow(data: data))
        case .mcpToolCall(let data):
            guard toolDisplayMode.rendersRows else { return AnyView(EmptyView()) }
            if let view = data.computerUse {
                return AnyView(
                    ComputerUseToolCallView(
                        data: data,
                        view: view,
                        externalExpanded: toolDefaultExpanded(isFailed: data.status == .failed)
                    )
                )
            } else {
                return AnyView(toolCallRow(makeMcpModel(data)))
            }
        case .dynamicToolCall(let data):
            guard toolDisplayMode.rendersRows else { return AnyView(EmptyView()) }
            if CrossServerTools.isRichTool(data.tool) {
                return AnyView(CrossServerToolResultView(data: data))
            } else {
                return AnyView(toolCallRow(makeDynamicToolModel(data)))
            }
        case .multiAgentAction(let data):
            guard toolDisplayMode.rendersRows else { return AnyView(EmptyView()) }
            return AnyView(
                SubagentCardView(
                    data: data,
                    serverId: serverId
                )
            )
        case .webSearch(let data):
            guard toolDisplayMode.rendersRows else { return AnyView(EmptyView()) }
            return AnyView(toolCallRow(makeWebSearchModel(data)))
        case .imageView(let data):
            guard toolDisplayMode.rendersRows else { return AnyView(EmptyView()) }
            return AnyView(toolCallRow(makeImageViewModel(data)))
        case .imageGeneration(let data):
            guard toolDisplayMode.rendersRows else { return AnyView(EmptyView()) }
            return AnyView(
                ImageGenerationToolCallView(
                    data: data,
                    externalExpanded: toolDefaultExpanded(isFailed: data.status == .failed)
                )
            )
        case .widget(let data):
            return AnyView(
                WidgetContainerView(
                    widget: data.widgetState,
                    originThreadId: originThreadId,
                    onMessage: handleWidgetMessage
                )
            )
        case .userInputResponse(let data):
            return AnyView(ConversationUserInputResponseRow(data: data))
        case .divider(let kind):
            return AnyView(ConversationDividerRow(kind: kind, isLiveTurn: isLiveTurn))
        case .error(let data):
            return AnyView(
                ConversationSystemCardRow(
                    title: data.title.isEmpty ? "Error" : data.title,
                    content: [data.message, data.details].compactMap { $0 }.joined(separator: "\n\n"),
                    accent: RemoraTheme.danger,
                    iconName: "exclamationmark.triangle.fill",
                )
            )
        case .note(let data):
            return AnyView(
                ConversationSystemCardRow(
                    title: data.title,
                    content: data.body,
                    accent: RemoraTheme.accent,
                    iconName: "info.circle.fill"
                )
            )
        }
    }

    @ViewBuilder
    private func commandExecutionRow(_ data: ConversationCommandExecutionData) -> some View {
        ConversationCommandExecutionRow(
            data: data,
            isPreferredExpanded: commandDefaultExpanded(data),
            displayMode: commandDisplayMode
        )
    }

    @ViewBuilder
    private func toolCallRow(_ model: ToolCallCardModel) -> some View {
        ToolCallCardView(
            model: model,
            serverId: serverId,
            externalExpanded: toolDefaultExpanded(isFailed: model.status == .failed)
        )
    }

    private func toolDefaultExpanded(isFailed: Bool) -> Bool {
        if toolDisplayMode == .collapsed,
           !isLiveTurn,
           shouldPreserveRichDetail {
            return true
        }
        return toolDisplayMode.defaultExpanded(isFailed: isFailed)
    }

    private func commandDefaultExpanded(_ data: ConversationCommandExecutionData) -> Bool {
        switch commandDisplayMode {
        case .expanded:
            return true
        case .collapsed:
            return data.isInProgress || data.status == .failed
        case .hidden:
            return false
        }
    }

    private func userRow(_ data: ConversationUserMessageData) -> some View {
        UserBubble(text: data.text, images: data.images)
            .contextMenu {
                if !data.text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
                    Button("Copy") {
                        UIPasteboard.general.string = data.text
                    }
                }

                if item.isFromUserTurnBoundary {
                    Button("Edit Message") {
                        onEditUserItem(item)
                    }
                    .disabled(messageActionsDisabled)

                    Button("Fork From Here") {
                        onForkFromUserItem(item)
                    }
                    .disabled(messageActionsDisabled)
                }
            }
    }

    @ViewBuilder
    private func assistantRow(_ data: ConversationAssistantMessageData) -> some View {
        let assistantLabel = AgentLabelFormatter.format(
            nickname: data.agentNickname,
            role: data.agentRole
        )

        StreamingAssistantBubble(
            itemId: item.id,
            text: data.text,
            isStreaming: isStreamingMessage,
            label: assistantLabel,
            themeVersion: themeManager.themeVersion,
            onSnapshotRendered: isStreamingMessage ? onStreamingSnapshotRendered : nil
        )
    }

    private func handleWidgetMessage(_ body: Any) {
        guard let dict = body as? [String: Any],
              let type = dict["_type"] as? String else { return }
        switch type {
        case "sendPrompt":
            if let text = dict["text"] as? String, !text.isEmpty {
                onWidgetPrompt(text)
            }
        case "openLink":
            if let urlString = dict["url"] as? String, let url = URL(string: urlString) {
                UIApplication.shared.open(url)
            }
        default:
            break
        }
    }

    private func makeFileChangeModel(_ data: ConversationFileChangeData) -> ToolCallCardModel {
        let changedPaths = data.changes.map(\.path)
        let summary = fileChangeSummary(for: data)

        let diffSections = data.changes.compactMap { change -> ToolCallSection? in
            guard !change.diff.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else { return nil }
            let label = data.changes.count > 1 ? workspaceTitle(for: change.path) : ""
            return .diff(label: label, content: change.diff)
        }

        var sections: [ToolCallSection] = []
        if diffSections.isEmpty, !changedPaths.isEmpty {
            sections.append(.list(label: "Files", items: changedPaths.map(workspaceTitle(for:))))
        }
        sections.append(contentsOf: diffSections)
        if let outputDelta = data.outputDelta?.trimmingCharacters(in: .whitespacesAndNewlines), !outputDelta.isEmpty {
            sections.append(.text(label: "Output", content: outputDelta))
        }

        return ToolCallCardModel(
            kind: .fileChange,
            title: "File Change",
            summary: summary.plainText,
            attributedSummary: summary.attributedText,
            status: data.status.toolCallStatus,
            duration: nil,
            sections: sections
        )
    }

    private func fileChangeSummary(for data: ConversationFileChangeData) -> (plainText: String, attributedText: AttributedString?) {
        guard !data.changes.isEmpty else {
            return ("File changes", nil)
        }

        let additions = data.changes.reduce(0) { $0 + $1.additions }
        let deletions = data.changes.reduce(0) { $0 + $1.deletions }
        let hasCountSummary = additions > 0 || deletions > 0

        if data.changes.count == 1, let change = data.changes.first {
            let verb = fileChangeVerb(for: change.kind)
            let filename = workspaceTitle(for: change.path)
            guard hasCountSummary else {
                return ("\(verb) \(filename)", nil)
            }

            let plainText = "\(verb) \(filename) +\(additions) -\(deletions)"

            var attributed = AttributedString()

            var verbText = AttributedString("\(verb) ")
            verbText.foregroundColor = RemoraTheme.textSecondary
            attributed.append(verbText)

            var fileText = AttributedString(filename)
            fileText.foregroundColor = RemoraTheme.accentForegroundOnSurface
            attributed.append(fileText)

            var additionsText = AttributedString(" +\(additions)")
            additionsText.foregroundColor = RemoraTheme.success
            attributed.append(additionsText)

            var deletionsText = AttributedString(" -\(deletions)")
            deletionsText.foregroundColor = RemoraTheme.danger
            attributed.append(deletionsText)

            return (plainText, attributed)
        }

        guard hasCountSummary else {
            return ("Changed \(data.changes.count) files", nil)
        }

        let plainText = "Changed \(data.changes.count) files +\(additions) -\(deletions)"
        var attributed = AttributedString("Changed \(data.changes.count) files")
        attributed.foregroundColor = RemoraTheme.textSystem

        var additionsText = AttributedString(" +\(additions)")
        additionsText.foregroundColor = RemoraTheme.success
        attributed.append(additionsText)

        var deletionsText = AttributedString(" -\(deletions)")
        deletionsText.foregroundColor = RemoraTheme.danger
        attributed.append(deletionsText)

        return (plainText, attributed)
    }

    private func fileChangeVerb(for kind: String) -> String {
        switch kind.lowercased() {
        case "add":
            return "Added"
        case "delete":
            return "Deleted"
        case "update":
            return "Edited"
        default:
            return "Changed"
        }
    }

    private func makeMcpModel(_ data: ConversationMcpToolCallData) -> ToolCallCardModel {
        var sections: [ToolCallSection] = []
        if let arguments = data.argumentsJSON, !arguments.isEmpty {
            sections.append(.json(label: "Arguments", content: arguments))
        }
        if let contentSummary = data.contentSummary, !contentSummary.isEmpty {
            sections.append(.text(label: "Result", content: contentSummary))
        }
        if let structured = data.structuredContentJSON, !structured.isEmpty {
            sections.append(.json(label: "Structured", content: structured))
        }
        if let raw = data.rawOutputJSON, !raw.isEmpty {
            sections.append(.json(label: "Raw Output", content: raw))
        }
        if !data.progressMessages.isEmpty {
            sections.append(.progress(label: "Progress", items: data.progressMessages))
        }
        if let error = data.errorMessage, !error.isEmpty {
            sections.append(.text(label: "Error", content: error))
        }

        let summary = data.server.isEmpty
            ? data.tool
            : "\(data.server).\(data.tool)"

        return ToolCallCardModel(
            kind: .mcpToolCall,
            title: "MCP Tool Call",
            summary: summary,
            status: data.status.toolCallStatus,
            duration: formatDuration(data.durationMs),
            sections: sections
        )
    }

    private func makeDynamicToolModel(_ data: ConversationDynamicToolCallData) -> ToolCallCardModel {
        var sections: [ToolCallSection] = []
        var metadata: [ToolCallKeyValue] = []
        if let display = data.display {
            metadata.append(contentsOf: display.metadata.map {
                ToolCallKeyValue(key: $0.key, value: $0.value)
            })
        }
        if let namespace = data.namespace, !namespace.isEmpty {
            metadata.append(ToolCallKeyValue(key: "Namespace", value: namespace))
        }
        if let success = data.success {
            metadata.append(ToolCallKeyValue(key: "Success", value: success ? "true" : "false"))
        }
        if !metadata.isEmpty {
            sections.append(.kv(label: "Metadata", entries: metadata))
        }
        if let arguments = data.argumentsJSON, !arguments.isEmpty {
            sections.append(.json(label: "Arguments", content: arguments))
        }
        if let contentSummary = data.contentSummary, !contentSummary.isEmpty {
            sections.append(.text(label: "Result", content: contentSummary))
        }
        let title = data.display?.title ?? "Dynamic Tool Call"
        let summary = data.display?.summary
            ?? data.namespace.map { "\($0).\(data.tool)" }
            ?? data.tool

        return ToolCallCardModel(
            kind: .mcpToolCall,
            title: title,
            summary: summary,
            status: data.status.toolCallStatus,
            duration: formatDuration(data.durationMs),
            sections: sections
        )
    }

    private func makeWebSearchModel(_ data: ConversationWebSearchData) -> ToolCallCardModel {
        var sections: [ToolCallSection] = []
        if !data.query.isEmpty {
            sections.append(.text(label: "Query", content: data.query))
        }
        if let action = data.actionJSON, !action.isEmpty {
            sections.append(.json(label: "Action", content: action))
        }
        return ToolCallCardModel(
            kind: .webSearch,
            title: "Web Search",
            summary: data.query.isEmpty ? "Web search" : "Web search for \(data.query)",
            status: data.isInProgress ? .inProgress : .completed,
            duration: nil,
            sections: sections
        )
    }

    private func makeImageViewModel(_ data: ConversationImageViewData) -> ToolCallCardModel {
        let trimmedPath = data.path.trimmingCharacters(in: .whitespacesAndNewlines)
        let displayName = workspaceTitle(for: trimmedPath)
        return ToolCallCardModel(
            kind: .imageView,
            title: "Image View",
            summary: displayName.isEmpty ? "Image" : displayName,
            status: .completed,
            duration: nil,
            sections: [
                .kv(
                    label: "Metadata",
                    entries: [ToolCallKeyValue(key: "Path", value: trimmedPath)]
                )
            ],
            initiallyExpanded: true
        )
    }
}

private extension ConversationItem {
    var liveDetailStatus: ToolCallStatus? {
        switch content {
        case .commandExecution(let data):
            return data.status.toolCallStatus
        case .fileChange(let data):
            return data.status.toolCallStatus
        case .mcpToolCall(let data):
            return data.status.toolCallStatus
        case .dynamicToolCall(let data):
            return data.status.toolCallStatus
        case .webSearch(let data):
            return data.isInProgress ? .inProgress : .completed
        case .imageView:
            return .completed
        case .imageGeneration(let data):
            return data.status.toolCallStatus
        default:
            return nil
        }
    }
}
