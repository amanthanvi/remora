import SwiftUI

struct ConversationTurnRow: View, Equatable {
    let turn: TranscriptTurn
    let isExpanded: Bool
    let canCollapse: Bool
    let isLastTurn: Bool
    let viewportHeight: CGFloat
    let showTypingIndicator: Bool
    let serverId: String
    let originThreadId: String?
    let agentDirectoryVersion: UInt64
    @Environment(\.textScale) private var textScale
    let messageActionsDisabled: Bool
    let onToggleExpansion: () -> Void
    let onStreamingSnapshotRendered: (() -> Void)?
    let onLiveContentLayoutChanged: (() -> Void)?
    let resolveTargetLabel: (String) -> String?
    let onWidgetPrompt: (String) -> Void
    let onEditUserItem: (ConversationItem) -> Void
    let onForkFromUserItem: (ConversationItem) -> Void
    var onOpenConversation: ((ThreadKey) -> Void)? = nil

    static func == (lhs: ConversationTurnRow, rhs: ConversationTurnRow) -> Bool {
        lhs.turn.id == rhs.turn.id &&
            lhs.turn.renderDigest == rhs.turn.renderDigest &&
            lhs.turn.isLive == rhs.turn.isLive &&
            lhs.isExpanded == rhs.isExpanded &&
            lhs.canCollapse == rhs.canCollapse &&
            lhs.isLastTurn == rhs.isLastTurn &&
            lhs.viewportHeight == rhs.viewportHeight &&
            lhs.showTypingIndicator == rhs.showTypingIndicator &&
            lhs.serverId == rhs.serverId &&
            lhs.originThreadId == rhs.originThreadId &&
            lhs.agentDirectoryVersion == rhs.agentDirectoryVersion &&
            lhs.messageActionsDisabled == rhs.messageActionsDisabled
    }

    var body: some View {
        if isExpanded {
            expandedContent
        } else {
            collapsedCard
        }
    }

    private var expandedContent: some View {
        VStack(alignment: .leading, spacing: 12) {
            ConversationTurnTimeline(
                items: turn.items,
                isLive: turn.isLive,
                serverId: serverId,
                originThreadId: originThreadId,
                agentDirectoryVersion: agentDirectoryVersion,
                messageActionsDisabled: messageActionsDisabled,
                onStreamingSnapshotRendered: onStreamingSnapshotRendered,
                onLiveContentLayoutChanged: onLiveContentLayoutChanged,
                resolveTargetLabel: resolveTargetLabel,
                onWidgetPrompt: onWidgetPrompt,
                onEditUserItem: onEditUserItem,
                onForkFromUserItem: onForkFromUserItem,
                onOpenConversation: onOpenConversation
            )

            TypingIndicator()
                .opacity(showTypingIndicator ? 1 : 0)
                .animation(nil)

            if canCollapse {
                Button("Show Less", systemImage: "chevron.up", action: onToggleExpansion)
                    .remoraFont(.caption, weight: .semibold)
                    .foregroundColor(RemoraTheme.textSecondary)
                    .buttonStyle(.plain)
                    .padding(.top, 2)
            }
        }
    }

    private var collapsedCard: some View {
        Button(action: onToggleExpansion) {
            previewTextBlock
                .frame(maxWidth: .infinity, alignment: .leading)
                .padding(.horizontal, 14)
                .padding(.top, 10)
                .padding(.bottom, collapsedFooterReservedInset)
                .modifier(GlassRectModifier(cornerRadius: 16, tint: RemoraTheme.surface.opacity(0.34)))
                .overlay(alignment: .bottomLeading) {
                    footerRow
                        .padding(.horizontal, 14)
                        .padding(.bottom, 10)
                }
        }
        .buttonStyle(.plain)
        .contentShape(RoundedRectangle(cornerRadius: 16))
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(accessibilitySummary)
    }

    private var previewTextBlock: some View {
        VStack(alignment: .leading, spacing: 4) {
            Text(verbatim: turn.preview.primaryText)
                .remoraFont(.body, weight: .semibold)
                .foregroundColor(RemoraTheme.textPrimary)
                .lineLimit(1)
                .truncationMode(.tail)
                .minimumScaleFactor(0.82)
                .allowsTightening(true)
                .multilineTextAlignment(.leading)
                .frame(maxWidth: .infinity, alignment: .leading)

            Text(verbatim: responsePreviewText)
                .remoraFont(.body)
                .foregroundColor(RemoraTheme.textSecondary)
                .lineLimit(2)
                .truncationMode(.tail)
                .multilineTextAlignment(.leading)
                .frame(
                    maxWidth: .infinity,
                    minHeight: collapsedResponseHeight,
                    maxHeight: collapsedResponseHeight,
                    alignment: .topLeading
                )
                .mask(responsePreviewMask)
        }
        .frame(maxWidth: .infinity, minHeight: collapsedPreviewHeight, maxHeight: collapsedPreviewHeight, alignment: .topLeading)
    }

    private var footerRow: some View {
        HStack(alignment: .center, spacing: 10) {
            if !footerMetadataItems.isEmpty {
                HStack(spacing: 10) {
                    ForEach(footerMetadataItems, id: \.id) { item in
                        CollapsedTurnMetaItem(systemImage: item.systemImage, text: item.text)
                    }
                }
                .frame(maxWidth: .infinity, alignment: .leading)
            }
            Spacer(minLength: 8)
            Image(systemName: "chevron.down")
                .remoraFont(size: 11, weight: .semibold)
                .foregroundColor(RemoraTheme.textMuted)
        }
        .padding(.horizontal, 2)
        .padding(.bottom, 2)
    }

    private var collapsedPreviewHeight: CGFloat { collapsedPrimaryLineHeight + collapsedResponseHeight + 4 }
    private var collapsedFooterReservedInset: CGFloat { collapsedFooterHeight + 10 }
    private var collapsedFooterHeight: CGFloat { max(UIFont.preferredFont(forTextStyle: .caption1).lineHeight * textScale, 14) }

    private var responsePreviewMask: some View {
        LinearGradient(
            stops: [
                .init(color: .white, location: 0),
                .init(color: .white, location: 0.55),
                .init(color: .white.opacity(0.58), location: 0.82),
                .init(color: .white.opacity(0.24), location: 1),
            ],
            startPoint: .top,
            endPoint: .bottom
        )
    }

    private var collapsedPrimaryLineHeight: CGFloat { collapsedPreviewLineHeight }
    private var collapsedResponseHeight: CGFloat { (collapsedPreviewLineHeight * 2) + 2 }
    private var collapsedPreviewLineHeight: CGFloat { UIFont.preferredFont(forTextStyle: .body).lineHeight * textScale }

    private var footerMetadataItems: [CollapsedTurnMeta] {
        var items: [CollapsedTurnMeta] = []
        if let durationText = turn.preview.durationText {
            items.append(CollapsedTurnMeta(id: "duration", systemImage: "clock", text: durationText))
        }
        if turn.preview.toolCallCount > 0 {
            items.append(CollapsedTurnMeta(id: "tools", systemImage: "chevron.left.forwardslash.chevron.right", text: "\(turn.preview.toolCallCount)"))
        }
        if turn.preview.eventCount > 0 {
            items.append(CollapsedTurnMeta(id: "events", systemImage: "sparkles", text: "\(turn.preview.eventCount)"))
        }
        if turn.preview.widgetCount > 0 {
            items.append(CollapsedTurnMeta(id: "widgets", systemImage: "rectangle.3.group", text: "\(turn.preview.widgetCount)"))
        }
        if turn.preview.imageCount > 0 {
            items.append(CollapsedTurnMeta(id: "images", systemImage: "photo", text: "\(turn.preview.imageCount)"))
        }
        return items
    }

    private var secondaryPreviewText: String? {
        guard let secondaryText = turn.preview.secondaryText, secondaryText != turn.preview.primaryText else { return nil }
        return secondaryText
    }

    private var responsePreviewText: String { secondaryPreviewText ?? turn.preview.primaryText }

    private var accessibilitySummary: String {
        var parts = [turn.preview.primaryText]
        if let secondaryPreviewText { parts.append(secondaryPreviewText) }
        if let durationText = turn.preview.durationText { parts.append("Duration \(durationText)") }
        if turn.preview.toolCallCount > 0 { parts.append("\(turn.preview.toolCallCount) tool \(turn.preview.toolCallCount == 1 ? "call" : "calls")") }
        if turn.preview.widgetCount > 0 { parts.append("\(turn.preview.widgetCount) \(turn.preview.widgetCount == 1 ? "widget" : "widgets")") }
        if turn.preview.eventCount > 0 { parts.append("\(turn.preview.eventCount) \(turn.preview.eventCount == 1 ? "event" : "events")") }
        if turn.preview.imageCount > 0 { parts.append("\(turn.preview.imageCount) \(turn.preview.imageCount == 1 ? "image" : "images")") }
        return parts.joined(separator: ". ")
    }
}

private struct CollapsedTurnMeta: Identifiable {
    let id: String
    let systemImage: String
    let text: String
}

private struct CollapsedTurnMetaItem: View {
    let systemImage: String
    let text: String

    var body: some View {
        HStack(spacing: 4) {
            Image(systemName: systemImage)
                .remoraFont(size: 9, weight: .medium)
                .foregroundColor(RemoraTheme.textMuted)
            Text(verbatim: text)
                .remoraMonoFont(size: 10)
                .foregroundColor(RemoraTheme.textSecondary)
                .lineLimit(1)
        }
    }
}
