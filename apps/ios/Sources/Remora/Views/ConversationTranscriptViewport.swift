import SwiftUI
import os

let conversationViewSignpostLog = OSLog(
    subsystem: Bundle.main.bundleIdentifier ?? "com.remora.app.ios",
    category: "ConversationView"
)

struct ConversationMessageList: View {
    @Environment(\.horizontalSizeClass) private var horizontalSizeClass
    let items: [ConversationItem]
    let threadStatus: ConversationStatus
    let threadHasServerData: Bool
    let transcriptRenderDigest: Int
    let followScrollToken: Int
    let sendScrollToken: Int
    let activeThreadKey: ThreadKey
    let agentDirectoryVersion: UInt64
    var topInset: CGFloat = 0
    let olderTurnsCursor: String?
    let initialTurnsLoaded: Bool
    @Binding var textSizeStep: Int
    let resolveTargetLabel: (String) -> String?
    let onWidgetPrompt: (String) -> Void
    let onEditUserItem: (ConversationItem) -> Void
    let onForkFromUserItem: (ConversationItem) -> Void
    var onOpenConversation: ((ThreadKey) -> Void)? = nil
    let onLoadOlderTurns: (ThreadKey) -> Void
    @State private var isNearBottom = true
    @State private var autoFollowStreaming = true
    @State private var userIsDraggingScroll = false
    @State private var distanceFromBottom: CGFloat = 0
    @State private var waitingForDataExpired = false
    @State private var pinchBaseStep: Int?
    @State private var pinchAppliedDelta = 0
    @State private var transcriptTurns: [TranscriptTurn] = []
    @State private var transcriptBuildKey: Int?
    @State private var renderedTurns: [TranscriptTurn] = []
    @State private var renderedTurnsBuildKey: Int?
    @State private var expandedTurnIDs: Set<String> = []
    @State private var pendingAnimatedTurns: [TranscriptTurn]?
    @State private var turnInsertionAnimationInFlight = false
    @State private var followLayoutScrollScheduled = false
    @State private var initialBottomScrollThreadScopeID: String?
    @State private var programmaticBottomScrollSettling = false
    @State private var programmaticBottomScrollGeneration = 0
    @AppStorage("collapseTurns") private var collapseTurns = false
    private static let latestButtonShowDistance: CGFloat = 48
    private static let nearBottomRestoreDistance: CGFloat = 12
    private static let bottomScrollSettleDuration: TimeInterval = 0.3
    private static let bottomAnchorID = "conversation-message-list-bottom"
    private static let scrollCoordinateSpaceName = "conversation-message-list-scroll"

    private var expandedRecentTurnCount: Int {
        return collapseTurns ? 1 : .max
    }

    private var sourceTurns: [TranscriptTurn] {
        if transcriptTurns.isEmpty {
            return TranscriptTurn.build(
                from: items,
                threadStatus: threadStatus,
                expandedRecentTurnCount: expandedRecentTurnCount
            )
        }
        return transcriptTurns
    }

    private var lastTurnIsUserOnly: Bool {
        guard let lastTurn = sourceTurns.last else { return false }
        return lastTurn.items.allSatisfy { $0.isUserItem }
    }

    private var isStreamingLastTurn: Bool {
        if case .thinking = threadStatus { return true }
        return sourceTurns.last?.isLive == true
    }

    private var messageActionsDisabled: Bool {
        if case .thinking = threadStatus { return true }
        return false
    }

    private var isWaitingForData: Bool {
        items.isEmpty && threadHasServerData && !waitingForDataExpired
    }

    private var shouldShowScrollToBottom: Bool {
        !items.isEmpty && distanceFromBottom > Self.latestButtonShowDistance
    }

    private var activeThreadScopeID: String {
        "\(activeThreadKey.serverId)::\(activeThreadKey.threadId)"
    }

    private var isStreaming: Bool {
        if case .thinking = threadStatus { return true }
        return false
    }

    private var hasOlderTurns: Bool {
        if let cursor = olderTurnsCursor { return !cursor.isEmpty }
        return false
    }

    private var mergedRenderableTurns: [TranscriptTurn] {
        let turns = sourceTurns
        let buildKey = makeRenderedTurnsBuildKey(for: turns)
        if renderedTurnsBuildKey == buildKey { return renderedTurns }
        return TranscriptTurn.mergeConsecutiveExplorationTurnsForRendering(turns)
    }

    var body: some View {
        let turns = mergedRenderableTurns
        let lastTurnID = turns.last?.id
        ScrollViewReader { proxy in
            GeometryReader { viewport in
            ZStack(alignment: .bottomTrailing) {
                ScrollView {
                    VStack(alignment: .leading, spacing: 0) {
                        LazyVStack(alignment: .leading, spacing: 10) {
                            if !initialTurnsLoaded && hasOlderTurns && !turns.isEmpty {
                                ConversationLoadingIndicator(label: "Loading earlier messages...")
                                    .frame(maxWidth: .infinity)
                                    .padding(.vertical, 12)
                            } else if hasOlderTurns {
                                Button {
                                    onLoadOlderTurns(activeThreadKey)
                                } label: {
                                    Text("Load earlier messages")
                                        .remoraFont(.caption, weight: .semibold)
                                        .foregroundColor(RemoraTheme.accentForegroundOnSurface)
                                        .frame(maxWidth: .infinity)
                                        .padding(.vertical, 8)
                                }
                                .buttonStyle(.plain)
                            }
                            ForEach(turns) { turn in
                                let isLastTurn = turn.id == lastTurnID
                                ConversationTurnRow(
                                    turn: turn,
                                    isExpanded: isTurnExpanded(turn),
                                    canCollapse: turn.isCollapsedByDefault,
                                    isLastTurn: isLastTurn,
                                    viewportHeight: viewport.size.height,
                                    showTypingIndicator: isLastTurn && {
                                        if case .thinking = threadStatus { return true }
                                        return false
                                    }(),
                                    serverId: activeThreadKey.serverId,
                                    originThreadId: activeThreadKey.threadId,
                                    agentDirectoryVersion: agentDirectoryVersion,
                                    messageActionsDisabled: messageActionsDisabled,
                                    onToggleExpansion: {
                                        toggleTurnExpansion(turn)
                                    },
                                    onStreamingSnapshotRendered: {
                                        requestFollowScrollAfterLayout(proxy)
                                    },
                                    onLiveContentLayoutChanged: {
                                        requestFollowScrollAfterLayout(proxy)
                                    },
                                    resolveTargetLabel: resolveTargetLabel,
                                    onWidgetPrompt: onWidgetPrompt,
                                    onEditUserItem: onEditUserItem,
                                    onForkFromUserItem: onForkFromUserItem,
                                    onOpenConversation: onOpenConversation
                                )
                                .equatable()
                                .turnDebugOverlay(turnId: turn.id)
                            }
                        }
                        .frame(maxWidth: RemoraPlatform.isRegularSurface(horizontalSizeClass: horizontalSizeClass) ? 760 : .infinity)
                        .frame(maxWidth: .infinity, alignment: .center)
                        .padding(.horizontal, 16)
                        .padding(.top, topInset + 56)
                        .animation(.spring(response: 0.22, dampingFraction: 0.9), value: textSizeStep)

                        if isWaitingForData {
                            ConversationLoadingIndicator(label: "Loading conversation...")
                                .frame(maxWidth: .infinity)
                                .padding(.top, 40)
                        }

                        Color.clear
                            .frame(height: 1)
                            .id(Self.bottomAnchorID)
                            .padding(.horizontal, 16)
                    }
                    .frame(maxWidth: .infinity, minHeight: viewport.size.height, alignment: .top)
                }
                .id(activeThreadScopeID)
                .scrollIndicators(.hidden)
                .scrollDismissesKeyboard(.interactively)
                .coordinateSpace(name: Self.scrollCoordinateSpaceName)
                .onScrollGeometryChange(for: CGFloat.self) { geometry in
                    max(0, geometry.contentSize.height - geometry.visibleRect.maxY)
                } action: { _, distance in
                    updateDistanceFromBottom(distance)
                }
                // Keep the chat initially bottom-aligned, but don't let keyboard-driven
                // viewport size changes force a fresh bottom jump with stale lazy heights.
                .defaultScrollAnchor(.bottom, for: .initialOffset)
                .simultaneousGesture(
                    MagnificationGesture(minimumScaleDelta: 0.03)
                        .onChanged { scale in handlePinchChanged(scale: scale) }
                        .onEnded { scale in finishPinch(scale: scale) }
                )
                .onScrollPhaseChange { _, newPhase in
                    switch newPhase {
                    case .tracking, .interacting:
                        programmaticBottomScrollSettling = false
                        userIsDraggingScroll = true
                        if isStreaming { autoFollowStreaming = false }
                    case .decelerating:
                        userIsDraggingScroll = true
                    default:
                        userIsDraggingScroll = false
                        if isNearBottom { autoFollowStreaming = true }
                    }
                }
                .onAppear {
                    autoFollowStreaming = true
                    syncTranscriptTurns()
                    requestInitialBottomScrollIfNeeded(proxy)
                }
                .onChange(of: activeThreadKey) {
                    autoFollowStreaming = true
                    isNearBottom = true
                    distanceFromBottom = 0
                    initialBottomScrollThreadScopeID = nil
                    waitingForDataExpired = false
                    syncTranscriptTurns(resetExpansion: true)
                    StreamingRendererCoordinator.shared.reset()
                    requestInitialBottomScrollIfNeeded(proxy)
                }
                .task(id: activeThreadKey) {
                    try? await Task.sleep(for: .seconds(1))
                    waitingForDataExpired = true
                }
                .onChange(of: items) { _, _ in
                    syncTranscriptTurns()
                    requestInitialBottomScrollIfNeeded(proxy)
                }
                .onChange(of: collapseTurns) {
                    syncTranscriptTurns(resetExpansion: true)
                }
                .onChange(of: followScrollToken) {
                    guard isStreaming, autoFollowStreaming, !userIsDraggingScroll else { return }
                    scrollToBottom(proxy)
                }
                .onChange(of: sendScrollToken) {
                    autoFollowStreaming = true
                    isNearBottom = true
                    distanceFromBottom = 0
                    scrollToBottom(proxy)
                    DispatchQueue.main.asyncAfter(deadline: .now() + 0.05) {
                        withAnimation(.interactiveSpring(response: 0.28, dampingFraction: 0.9)) {
                            scrollToBottom(proxy)
                        }
                    }
                }
                .onChange(of: threadStatus) { oldStatus, _ in
                    syncTranscriptTurns()
                    // When streaming ends, finish active renderers so they
                    // switch to static rendering (no re-animation on view rebuild).
                    let wasStreaming = { if case .thinking = oldStatus { return true }; return false }()
                    if wasStreaming && !isStreaming {
                        StreamingRendererCoordinator.shared.finishActive()
                    }
                    if wasStreaming && !isStreaming && autoFollowStreaming {
                        scrollToBottom(proxy)
                        DispatchQueue.main.asyncAfter(deadline: .now() + 0.05) {
                            scrollToBottom(proxy)
                        }
                    }
                }

                if shouldShowScrollToBottom {
                    ScrollToBottomIndicator {
                        autoFollowStreaming = true
                        isNearBottom = true
                        distanceFromBottom = 0
                        // Jump without animation first so LazyVStack realizes
                        // content near the bottom, then do an animated corrective
                        // scroll once layout has settled.  This avoids the
                        // overshoot caused by stale estimated heights.
                        scrollToBottom(proxy)
                        DispatchQueue.main.asyncAfter(deadline: .now() + 0.05) {
                            withAnimation(.interactiveSpring(response: 0.28, dampingFraction: 0.9)) {
                                scrollToBottom(proxy)
                            }
                        }
                    }
                    .padding(.trailing, 14)
                    .padding(.bottom, 10)
                    .transition(.move(edge: .bottom).combined(with: .opacity))
                }
            }
            }
        }
    }

    private func isTurnExpanded(_ turn: TranscriptTurn) -> Bool {
        !turn.isCollapsedByDefault || expandedTurnIDs.contains(turn.id)
    }

    private func toggleTurnExpansion(_ turn: TranscriptTurn) {
        guard turn.isCollapsedByDefault else { return }
        withAnimation(.spring(response: 0.28, dampingFraction: 0.88)) {
            if expandedTurnIDs.contains(turn.id) {
                expandedTurnIDs.remove(turn.id)
            } else {
                expandedTurnIDs.insert(turn.id)
            }
        }
    }

    private func requestFollowScrollAfterLayout(_ proxy: ScrollViewProxy) {
        guard !followLayoutScrollScheduled else { return }
        followLayoutScrollScheduled = true
        DispatchQueue.main.async {
            followLayoutScrollScheduled = false
            guard isStreaming, autoFollowStreaming, !userIsDraggingScroll else { return }
            scrollToBottom(proxy)
        }
    }

    private func requestInitialBottomScrollIfNeeded(_ proxy: ScrollViewProxy) {
        guard !items.isEmpty else { return }
        let threadScopeID = activeThreadScopeID
        guard initialBottomScrollThreadScopeID != threadScopeID else { return }
        initialBottomScrollThreadScopeID = threadScopeID
        DispatchQueue.main.async {
            guard activeThreadScopeID == threadScopeID else { return }
            scrollToBottom(proxy)
            DispatchQueue.main.asyncAfter(deadline: .now() + 0.05) {
                guard activeThreadScopeID == threadScopeID else { return }
                scrollToBottom(proxy)
            }
        }
    }

    private func updateDistanceFromBottom(_ distance: CGFloat) {
        let clampedDistance = max(0, distance)
        if programmaticBottomScrollSettling {
            if clampedDistance <= Self.nearBottomRestoreDistance {
                programmaticBottomScrollSettling = false
            } else {
                return
            }
        }

        distanceFromBottom = clampedDistance
        let nextIsNearBottom = clampedDistance <= Self.nearBottomRestoreDistance
        if nextIsNearBottom != isNearBottom { isNearBottom = nextIsNearBottom }
        if nextIsNearBottom {
            autoFollowStreaming = true
        } else if isStreaming && userIsDraggingScroll {
            autoFollowStreaming = false
        }
    }

    private func scrollToBottom(_ proxy: ScrollViewProxy) {
        isNearBottom = true
        distanceFromBottom = 0
        programmaticBottomScrollGeneration &+= 1
        let generation = programmaticBottomScrollGeneration
        programmaticBottomScrollSettling = true
        proxy.scrollTo(Self.bottomAnchorID, anchor: .bottom)
        DispatchQueue.main.asyncAfter(deadline: .now() + Self.bottomScrollSettleDuration) {
            guard programmaticBottomScrollGeneration == generation else { return }
            programmaticBottomScrollSettling = false
        }
    }

    private func syncTranscriptTurns(resetExpansion: Bool = false) {
        let nextBuildKey = makeTranscriptBuildKey()
        if transcriptBuildKey == nextBuildKey, !transcriptTurns.isEmpty {
            if resetExpansion { expandedTurnIDs.removeAll() }
            return
        }

        let nextTurns = TranscriptTurn.build(
            from: items,
            threadStatus: threadStatus,
            expandedRecentTurnCount: expandedRecentTurnCount
        )
        transcriptBuildKey = nextBuildKey
        if shouldAnimateNewTurnInsertion(from: transcriptTurns, to: nextTurns, resetExpansion: resetExpansion) {
            pendingAnimatedTurns = nextTurns
            guard !turnInsertionAnimationInFlight else { return }
            startNewTurnInsertionAnimation(from: transcriptTurns)
            return
        }

        if turnInsertionAnimationInFlight {
            pendingAnimatedTurns = nextTurns
            return
        }

        let lastTurnItemCountGrew = {
            guard let currentLast = transcriptTurns.last,
                  let nextLast = nextTurns.last,
                  currentLast.id == nextLast.id,
                  nextLast.items.count > currentLast.items.count else {
                return false
            }
            return true
        }()

        if lastTurnItemCountGrew {
            withAnimation(.spring(duration: 0.4, bounce: 0.08)) {
                applyTranscriptTurns(nextTurns, resetExpansion: resetExpansion)
            }
        } else {
            applyTranscriptTurns(nextTurns, resetExpansion: resetExpansion)
        }
    }

    private func makeTranscriptBuildKey() -> Int {
        var hasher = Hasher()
        hasher.combine(expandedRecentTurnCount)
        hasher.combine(transcriptRenderDigest)
        return hasher.finalize()
    }

    private func makeRenderedTurnsBuildKey(for turns: [TranscriptTurn]) -> Int {
        var hasher = Hasher()
        hasher.combine(turns.count)
        for turn in turns {
            hasher.combine(turn.id)
            hasher.combine(turn.renderDigest)
            hasher.combine(turn.isLive)
            hasher.combine(turn.isCollapsedByDefault)
        }
        return hasher.finalize()
    }

    private func layoutSignature(for turn: TranscriptTurn) -> Int {
        var hasher = Hasher()
        hasher.combine(turn.id)
        hasher.combine(turn.renderDigest)
        hasher.combine(turn.isLive)
        hasher.combine(turn.isCollapsedByDefault)
        return hasher.finalize()
    }

    private func handlePinchChanged(scale: CGFloat) {
        if pinchBaseStep == nil {
            pinchBaseStep = textSizeStep
            pinchAppliedDelta = 0
        }

        let candidateDelta: Int
        if scale >= 1.18 { candidateDelta = 2 }
        else if scale >= 1.03 { candidateDelta = 1 }
        else if scale <= 0.86 { candidateDelta = -2 }
        else if scale <= 0.97 { candidateDelta = -1 }
        else { candidateDelta = 0 }
        guard candidateDelta != 0 else { return }

        if pinchAppliedDelta == 0 {
            pinchAppliedDelta = candidateDelta
            return
        }

        let sameDirection = (pinchAppliedDelta > 0 && candidateDelta > 0) || (pinchAppliedDelta < 0 && candidateDelta < 0)
        if sameDirection {
            if abs(candidateDelta) > abs(pinchAppliedDelta) {
                pinchAppliedDelta = candidateDelta
            }
        } else {
            pinchAppliedDelta = candidateDelta
        }
    }

    private func finishPinch(scale: CGFloat) {
        handlePinchChanged(scale: scale)
        let baseline = pinchBaseStep ?? textSizeStep
        let next = ConversationTextSize.clamped(rawValue: baseline + pinchAppliedDelta).rawValue
        if next != textSizeStep {
            withAnimation(.spring(response: 0.22, dampingFraction: 0.9)) {
                textSizeStep = next
            }
        }
        pinchBaseStep = nil
        pinchAppliedDelta = 0
    }

    private func shouldAnimateNewTurnInsertion(
        from currentTurns: [TranscriptTurn],
        to nextTurns: [TranscriptTurn],
        resetExpansion: Bool
    ) -> Bool {
        guard collapseTurns,
              !resetExpansion,
              !currentTurns.isEmpty,
              nextTurns.count == currentTurns.count + 1,
              currentTurns.last?.id != nextTurns.last?.id,
              let lastTurn = nextTurns.last,
              lastTurn.items.first?.isUserItem == true,
              lastTurn.items.first?.isFromUserTurnBoundary == true else {
            return false
        }

        for (currentTurn, nextTurn) in zip(currentTurns, nextTurns) {
            guard currentTurn.id == nextTurn.id else { return false }
        }

        return true
    }

    private func startNewTurnInsertionAnimation(from currentTurns: [TranscriptTurn]) {
        guard let previousLastTurnID = currentTurns.last?.id else {
            if let pendingAnimatedTurns {
                applyTranscriptTurns(pendingAnimatedTurns)
                self.pendingAnimatedTurns = nil
            }
            return
        }

        turnInsertionAnimationInFlight = true
        let collapsedTurns = currentTurns.map { turn in
            turn.id == previousLastTurnID ? turn.withCollapsedByDefault(true) : turn
        }

        withAnimation(.snappy(duration: 0.16, extraBounce: 0)) {
            applyTranscriptTurns(
                collapsedTurns,
                removeExpandedTurnID: previousLastTurnID
            )
        } completion: {
            let turnsToInsert = pendingAnimatedTurns ?? collapsedTurns
            withAnimation(.smooth(duration: 0.2)) {
                applyTranscriptTurns(turnsToInsert)
            } completion: {
                turnInsertionAnimationInFlight = false
                let latestTurns = pendingAnimatedTurns ?? turnsToInsert
                pendingAnimatedTurns = nil
                if latestTurns.map(layoutSignature(for:)) != transcriptTurns.map(layoutSignature(for:)) {
                    applyTranscriptTurns(latestTurns)
                }
            }
        }
    }

    private func applyTranscriptTurns(
        _ nextTurns: [TranscriptTurn],
        resetExpansion: Bool = false,
        removeExpandedTurnID: String? = nil
    ) {
        let nextTurnIDs = Set(nextTurns.map(\.id))
        let nextRenderedTurns = TranscriptTurn.mergeConsecutiveExplorationTurnsForRendering(nextTurns)
        transcriptTurns = nextTurns
        renderedTurns = nextRenderedTurns
        renderedTurnsBuildKey = makeRenderedTurnsBuildKey(for: nextTurns)
        if resetExpansion {
            expandedTurnIDs.removeAll()
        } else {
            expandedTurnIDs.formIntersection(nextTurnIDs)
        }
        if let removeExpandedTurnID {
            expandedTurnIDs.remove(removeExpandedTurnID)
        }
    }

}

private struct ScrollToBottomIndicator: View {
    let action: () -> Void
    @State private var bob = false

    var body: some View {
        Button(action: action) {
            HStack(spacing: 8) {
                Image(systemName: "arrow.down")
                    .remoraFont(.caption, weight: .bold)
                    .offset(y: bob ? 1.5 : -1.5)
                    .animation(.easeInOut(duration: 0.75).repeatForever(autoreverses: true), value: bob)
                Text("Latest")
                    .remoraFont(.caption, weight: .semibold)
            }
            .foregroundColor(RemoraTheme.textPrimary)
            .padding(.horizontal, 12)
            .padding(.vertical, 8)
            .modifier(GlassCapsuleModifier())
        }
        .contentShape(Capsule())
        .onAppear {
            bob = true
        }
    }
}
