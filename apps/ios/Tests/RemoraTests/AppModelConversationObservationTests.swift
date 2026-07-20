import XCTest
import Observation
@testable import Remora

final class AppModelConversationObservationTests: XCTestCase {
    @MainActor
    func testChromeProjectionIgnoresStreamingPayloadButPublishesChromeChanges() {
        let selectedKey = ThreadKey(serverId: "server", threadId: "selected")
        let otherKey = ThreadKey(serverId: "server", threadId: "other")
        let selected = makeThread(key: selectedKey, title: "Selected")
        var other = makeThread(key: otherKey, title: "Other")
        let observation = AppModelChromeObservation()

        observation.refresh(snapshot: makeSnapshot(
            threads: [selected, other],
            activeThread: selectedKey
        ))
        let initialRevision = observation.revision

        var notificationCount = 0
        withObservationTracking {
            _ = observation.revision
        } onChange: {
            notificationCount += 1
        }

        other.hydratedConversationItems = [
            makeObservationHydratedAssistantItem(id: "streaming", text: "A streamed token")
        ]
        observation.refresh(snapshot: makeSnapshot(
            threads: [selected, other],
            activeThread: selectedKey
        ))

        XCTAssertEqual(observation.revision, initialRevision)
        XCTAssertEqual(notificationCount, 0)

        let approval = makePendingApproval(id: "approval")
        observation.refresh(snapshot: makeSnapshot(
            threads: [selected, other],
            activeThread: selectedKey,
            pendingApprovals: [approval]
        ))

        XCTAssertEqual(observation.revision, initialRevision + 1)
        XCTAssertEqual(notificationCount, 1)
        XCTAssertEqual(observation.pendingApproval, approval)
    }

    @MainActor
    func testNavigationProjectionIgnoresConversationActivityButPublishesRouteTargets() {
        let selectedKey = ThreadKey(serverId: "server", threadId: "selected")
        let otherKey = ThreadKey(serverId: "server", threadId: "other")
        let selected = makeThread(key: selectedKey, title: "Selected")
        var other = makeThread(key: otherKey, title: "Other")
        let selectedSummary = makeSessionSummary(
            key: selectedKey,
            cwd: "/selected",
            preview: "Initial"
        )
        var otherSummary = makeSessionSummary(
            key: otherKey,
            cwd: "/other",
            preview: "Initial"
        )
        let observation = AppModelNavigationObservation()

        observation.refresh(snapshot: makeSnapshot(
            threads: [selected, other],
            sessionSummaries: [selectedSummary, otherSummary],
            activeThread: selectedKey
        ))
        let initialRevision = observation.revision

        var notificationCount = 0
        withObservationTracking {
            _ = observation.revision
        } onChange: {
            notificationCount += 1
        }

        other.hydratedConversationItems = [
            makeObservationHydratedAssistantItem(id: "streaming", text: "New response text")
        ]
        otherSummary.preview = "New response text"
        otherSummary.hasActiveTurn = true
        otherSummary.recentToolLog = [
            AppToolLogEntry(tool: "shell", detail: "Streaming", status: "running")
        ]
        observation.refresh(snapshot: makeSnapshot(
            threads: [selected, other],
            sessionSummaries: [selectedSummary, otherSummary],
            activeThread: selectedKey
        ))

        XCTAssertEqual(observation.revision, initialRevision)
        XCTAssertEqual(notificationCount, 0)
        XCTAssertEqual(observation.threadKey(threadId: otherKey.threadId), otherKey)

        otherSummary.cwd = "/moved"
        observation.refresh(snapshot: makeSnapshot(
            threads: [selected, other],
            sessionSummaries: [selectedSummary, otherSummary],
            activeThread: selectedKey
        ))

        XCTAssertEqual(observation.revision, initialRevision + 1)
        XCTAssertEqual(notificationCount, 1)
        XCTAssertEqual(observation.sessionTarget(for: otherKey)?.cwd, "/moved")
    }

    @MainActor
    func testAppModelRoutesSnapshotsThroughStableChromeAndNavigationProjections() {
        let selectedKey = ThreadKey(serverId: "server", threadId: "selected")
        let otherKey = ThreadKey(serverId: "server", threadId: "other")
        let selected = makeThread(key: selectedKey, title: "Selected")
        var other = makeThread(key: otherKey, title: "Other")
        let selectedSummary = makeSessionSummary(key: selectedKey, cwd: "/selected")
        var otherSummary = makeSessionSummary(key: otherKey, cwd: "/other")
        let appModel = AppModel()

        appModel.applySnapshot(makeSnapshot(
            threads: [selected, other],
            sessionSummaries: [selectedSummary, otherSummary],
            activeThread: selectedKey
        ))
        let chromeObservation = appModel.chromeObservation
        let navigationObservation = appModel.navigationObservation
        let chromeRevision = chromeObservation.revision
        let navigationRevision = navigationObservation.revision
        let snapshotRevision = appModel.snapshotRevision

        other.hydratedConversationItems = [
            makeObservationHydratedAssistantItem(id: "streaming", text: "Streaming")
        ]
        otherSummary.preview = "Streaming"
        otherSummary.hasActiveTurn = true
        appModel.applySnapshot(makeSnapshot(
            threads: [selected, other],
            sessionSummaries: [selectedSummary, otherSummary],
            activeThread: selectedKey
        ))

        XCTAssertTrue(appModel.chromeObservation === chromeObservation)
        XCTAssertTrue(appModel.navigationObservation === navigationObservation)
        XCTAssertEqual(appModel.snapshotRevision, snapshotRevision + 1)
        XCTAssertEqual(chromeObservation.revision, chromeRevision)
        XCTAssertEqual(navigationObservation.revision, navigationRevision)

        appModel.applySnapshot(makeSnapshot(
            threads: [selected, other],
            sessionSummaries: [selectedSummary, otherSummary],
            activeThread: otherKey
        ))

        XCTAssertEqual(chromeObservation.revision, chromeRevision + 1)
        XCTAssertEqual(navigationObservation.revision, navigationRevision + 1)
        XCTAssertEqual(chromeObservation.activeThread, otherKey)
        XCTAssertEqual(navigationObservation.activeThread, otherKey)
    }

    @MainActor
    func testUnrelatedThreadUpdateDoesNotAdvanceRevision() {
        let selectedKey = ThreadKey(serverId: "server", threadId: "selected")
        let otherKey = ThreadKey(serverId: "server", threadId: "other")
        let selected = makeThread(key: selectedKey, title: "Selected")
        var other = makeThread(key: otherKey, title: "Other")
        let observation = AppModelConversationObservation(threadKey: selectedKey)

        observation.refresh(
            snapshot: makeSnapshot(threads: [selected, other]),
            cachedThread: nil,
            composerPrefillRequest: nil
        )
        let revision = observation.revision

        other.info.title = "Changed elsewhere"
        observation.refresh(
            snapshot: makeSnapshot(threads: [selected, other]),
            cachedThread: nil,
            composerPrefillRequest: nil
        )

        XCTAssertEqual(observation.revision, revision)
        XCTAssertEqual(observation.thread?.info.title, "Selected")
    }

    @MainActor
    func testEqualityGateDoesNotNotifyRevisionObserversForUnrelatedUpdate() {
        let selectedKey = ThreadKey(serverId: "server", threadId: "selected")
        let otherKey = ThreadKey(serverId: "server", threadId: "other")
        let selected = makeThread(key: selectedKey, title: "Selected")
        var other = makeThread(key: otherKey, title: "Other")
        let observation = AppModelConversationObservation(threadKey: selectedKey)
        observation.refresh(
            snapshot: makeSnapshot(threads: [selected, other]),
            cachedThread: nil,
            composerPrefillRequest: nil
        )

        var notificationCount = 0
        withObservationTracking {
            _ = observation.revision
        } onChange: {
            notificationCount += 1
        }

        other.info.title = "Changed elsewhere"
        observation.refresh(
            snapshot: makeSnapshot(threads: [selected, other]),
            cachedThread: nil,
            composerPrefillRequest: nil
        )
        XCTAssertEqual(notificationCount, 0)

        var updatedSelected = selected
        updatedSelected.info.title = "Changed here"
        observation.refresh(
            snapshot: makeSnapshot(threads: [updatedSelected, other]),
            cachedThread: nil,
            composerPrefillRequest: nil
        )
        XCTAssertEqual(notificationCount, 1)
    }

    @MainActor
    func testMatchingThreadAndAgentDirectoryUpdatesAdvanceRevision() {
        let key = ThreadKey(serverId: "server", threadId: "selected")
        var thread = makeThread(key: key, title: "Initial")
        let observation = AppModelConversationObservation(threadKey: key)

        observation.refresh(
            snapshot: makeSnapshot(threads: [thread]),
            cachedThread: nil,
            composerPrefillRequest: nil
        )
        let initialRevision = observation.revision

        thread.info.title = "Updated"
        observation.refresh(
            snapshot: makeSnapshot(threads: [thread], agentDirectoryVersion: 1),
            cachedThread: nil,
            composerPrefillRequest: nil
        )

        XCTAssertEqual(observation.revision, initialRevision + 1)
        XCTAssertEqual(observation.thread?.info.title, "Updated")
        XCTAssertEqual(observation.agentDirectoryVersion, 1)

        observation.refresh(
            snapshot: makeSnapshot(threads: [thread], agentDirectoryVersion: 1),
            cachedThread: nil,
            composerPrefillRequest: nil
        )
        XCTAssertEqual(observation.revision, initialRevision + 1)
    }

    @MainActor
    func testPendingInputServerAndPrefillAreScopedToSelectedThread() {
        let key = ThreadKey(serverId: "server", threadId: "selected")
        let otherKey = ThreadKey(serverId: "server", threadId: "other")
        let observation = AppModelConversationObservation(threadKey: key)
        var snapshot = makeSnapshot(threads: [makeThread(key: key, title: "Selected")])
        snapshot.pendingUserInputs = [makePendingInput(key: otherKey, id: "other-input")]

        observation.refresh(
            snapshot: snapshot,
            cachedThread: nil,
            composerPrefillRequest: AppModel.ComposerPrefillRequest(
                threadKey: otherKey,
                text: "Other prefill"
            )
        )

        XCTAssertNil(observation.pendingUserInputRequest)
        XCTAssertNil(observation.composerPrefillRequest)
        XCTAssertEqual(observation.server?.displayName, "Server")

        snapshot.pendingUserInputs = [makePendingInput(key: key, id: "selected-input")]
        snapshot.servers[0].displayName = "Renamed Server"
        let prefill = AppModel.ComposerPrefillRequest(threadKey: key, text: "Selected prefill")
        let revision = observation.revision
        observation.refresh(
            snapshot: snapshot,
            cachedThread: nil,
            composerPrefillRequest: prefill
        )

        XCTAssertEqual(observation.revision, revision + 1)
        XCTAssertEqual(observation.pendingUserInputRequest?.id, "selected-input")
        XCTAssertEqual(observation.server?.displayName, "Renamed Server")
        XCTAssertEqual(observation.composerPrefillRequest, prefill)

        snapshot.pendingUserInputs = [
            PendingUserInputRequest(
                id: "server-input",
                serverId: key.serverId,
                threadId: "",
                turnId: "turn",
                itemId: "item",
                questions: [],
                requesterAgentNickname: nil,
                requesterAgentRole: nil
            )
        ]
        observation.refresh(
            snapshot: snapshot,
            cachedThread: nil,
            composerPrefillRequest: prefill
        )
        XCTAssertEqual(observation.pendingUserInputRequest?.id, "server-input")
    }

    @MainActor
    func testCachedInitializationAndRemoval() {
        let key = ThreadKey(serverId: "server", threadId: "selected")
        let cached = makeThread(key: key, title: "Cached")
        let observation = AppModelConversationObservation(threadKey: key)

        observation.refresh(
            snapshot: nil,
            cachedThread: cached,
            composerPrefillRequest: nil
        )

        XCTAssertEqual(observation.thread, cached)
        let cachedRevision = observation.revision

        observation.refresh(
            snapshot: makeSnapshot(threads: []),
            cachedThread: nil,
            composerPrefillRequest: nil
        )

        XCTAssertNil(observation.thread)
        XCTAssertEqual(observation.revision, cachedRevision + 1)
    }

    @MainActor
    func testAppModelRoutesSnapshotPrefillCacheAndRemovalIntoStableObservation() {
        let key = ThreadKey(serverId: "server", threadId: "selected")
        let otherKey = ThreadKey(serverId: "server", threadId: "other")
        let selected = makeThread(key: key, title: "Selected")
        var other = makeThread(key: otherKey, title: "Other")
        let appModel = AppModel()
        appModel.applySnapshot(makeSnapshot(threads: [selected, other]))
        let observation = appModel.conversationObservation(for: key)
        let initialRevision = observation.revision

        XCTAssertTrue(appModel.conversationObservation(for: key) === observation)

        other.info.title = "Changed elsewhere"
        appModel.applySnapshot(makeSnapshot(threads: [selected, other]))
        XCTAssertEqual(observation.revision, initialRevision)

        appModel.queueComposerPrefill(threadKey: key, text: "Prefill")
        XCTAssertEqual(observation.composerPrefillRequest?.text, "Prefill")
        XCTAssertEqual(observation.revision, initialRevision + 1)

        appModel.applySnapshot(nil)
        XCTAssertEqual(observation.thread, selected)

        appModel.applySnapshot(makeSnapshot(threads: []))
        appModel.removeThreadSnapshot(for: key)
        XCTAssertNil(observation.thread)
    }

    @MainActor
    func testWeakRegistryReleasesUnusedCellsAndRouteMatchingRejectsStaleCell() {
        let oldKey = ThreadKey(serverId: "server", threadId: "old")
        let newKey = ThreadKey(serverId: "server", threadId: "new")
        let appModel = AppModel()
        weak var releasedObservation: AppModelConversationObservation?

        autoreleasepool {
            let observation = appModel.conversationObservation(for: oldKey)
            releasedObservation = observation
            XCTAssertTrue(observation.matching(threadKey: oldKey) === observation)
            XCTAssertNil(observation.matching(threadKey: newKey))
        }

        XCTAssertNil(releasedObservation)
        let replacement = appModel.conversationObservation(for: oldKey)
        XCTAssertEqual(replacement.threadKey, oldKey)
    }
}

private func makeSnapshot(
    threads: [AppThreadSnapshot],
    agentDirectoryVersion: UInt64 = 0,
    sessionSummaries: [AppSessionSummary] = [],
    activeThread: ThreadKey? = nil,
    pendingApprovals: [PendingApproval] = [],
    pendingUserInputs: [PendingUserInputRequest] = [],
    servers: [AppServerSnapshot]? = nil
) -> AppSnapshotRecord {
    AppSnapshotRecord(
        servers: servers ?? [makeServer()],
        threads: threads,
        sessionSummaries: sessionSummaries,
        agentDirectoryVersion: agentDirectoryVersion,
        activeThread: activeThread,
        pendingApprovals: pendingApprovals,
        pendingUserInputs: pendingUserInputs,
        voiceSession: AppVoiceSessionSnapshot(
            activeThread: nil,
            sessionId: nil,
            phase: nil,
            lastError: nil,
            transcriptEntries: [],
            handoffThreadKey: nil
        ),
        terminalSessions: [],
        activeTerminalId: nil
    )
}

private func makeServer() -> AppServerSnapshot {
    AppServerSnapshot(
        serverId: "server",
        displayName: "Server",
        host: "server.local",
        port: 8390,
        wakeMac: nil,
        isLocal: false,
        health: .connected,
        transportState: .connected,
        capabilities: AppServerCapabilities(
            canUseTransportActions: true,
            canBrowseDirectories: true,
            canStartThreads: true,
            canResumeThreads: true,
            supportsTurnPagination: false
        ),
        account: nil,
        requiresOpenaiAuth: false,
        rateLimits: nil,
        rateLimitsByRuntime: [],
        availableModels: nil,
        agentRuntimes: [],
        connectionProgress: nil,
        usageStats: nil,
        codexVersion: nil
    )
}

private func makeThread(key: ThreadKey, title: String) -> AppThreadSnapshot {
    AppThreadSnapshot(
        key: key,
        info: ThreadInfo(
            id: key.threadId,
            title: title,
            model: nil,
            status: .idle,
            preview: nil,
            cwd: "/tmp",
            path: nil,
            modelProvider: nil,
            agentNickname: nil,
            agentRole: nil,
            parentThreadId: nil,
            forkedFromId: nil,
            agentStatus: nil,
            createdAt: nil,
            updatedAt: nil
        ),
        agentRuntimeKind: .codex,
        collaborationMode: .default,
        model: nil,
        reasoningEffort: nil,
        effectiveApprovalPolicy: nil,
        effectiveSandboxPolicy: nil,
        hydratedConversationItems: [],
        queuedFollowUps: [],
        activeTurnId: nil,
        activePlanProgress: nil,
        pendingPlanImplementationPrompt: nil,
        contextTokensUsed: nil,
        modelContextWindow: nil,
        rateLimits: nil,
        realtimeSessionId: nil,
        goal: nil,
        stats: nil,
        tokenUsage: nil,
        olderTurnsCursor: nil,
        initialTurnsLoaded: true
    )
}

private func makePendingInput(key: ThreadKey, id: String) -> PendingUserInputRequest {
    PendingUserInputRequest(
        id: id,
        serverId: key.serverId,
        threadId: key.threadId,
        turnId: "turn",
        itemId: "item",
        questions: [],
        requesterAgentNickname: nil,
        requesterAgentRole: nil
    )
}

private func makePendingApproval(id: String) -> PendingApproval {
    PendingApproval(
        id: id,
        serverId: "server",
        kind: .command,
        threadId: "selected",
        turnId: "turn",
        itemId: "item",
        command: "echo ok",
        path: nil,
        grantRoot: nil,
        cwd: "/tmp",
        reason: nil
    )
}

private func makeSessionSummary(
    key: ThreadKey,
    cwd: String,
    preview: String = ""
) -> AppSessionSummary {
    AppSessionSummary(
        key: key,
        agentRuntimeKind: "codex",
        serverDisplayName: "Server",
        serverHost: "server.local",
        title: key.threadId,
        preview: preview,
        cwd: cwd,
        model: "",
        modelProvider: "",
        parentThreadId: nil,
        forkedFromId: nil,
        agentNickname: nil,
        agentRole: nil,
        agentDisplayLabel: nil,
        agentStatus: .unknown,
        updatedAt: nil,
        hasActiveTurn: false,
        isResumed: false,
        isSubagent: false,
        isFork: false,
        lastResponsePreview: nil,
        lastResponseTurnId: nil,
        lastUserMessage: nil,
        lastToolLabel: nil,
        recentToolLog: [],
        lastTurnStartMs: nil,
        lastTurnEndMs: nil,
        stats: nil,
        tokenUsage: nil,
        goal: nil
    )
}

private func makeObservationHydratedAssistantItem(
    id: String,
    text: String
) -> HydratedConversationItem {
    HydratedConversationItem(
        id: id,
        content: .assistant(
            HydratedAssistantMessageData(
                text: text,
                agentNickname: nil,
                agentRole: nil,
                phase: nil
            )
        ),
        sourceTurnId: "turn",
        sourceTurnIndex: 0,
        timestamp: 1,
        isFromUserTurnBoundary: false
    )
}
