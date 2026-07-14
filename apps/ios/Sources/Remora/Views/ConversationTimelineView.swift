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
            fileText.foregroundColor = RemoraTheme.accent
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

private struct ConversationReasoningRow: View {
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

private struct ConversationTodoListRow: View {
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

private struct ConversationProposedPlanRow: View {
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

private struct ConversationTurnDiffRow: View {
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

private struct ConversationUserInputResponseRow: View {
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

private struct ConversationDividerRow: View {
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

private struct ConversationCodeReviewRow: View {
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

private struct ConversationSystemCardRow: View {
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
