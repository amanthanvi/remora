import Foundation
import XCTest
@testable import Remora

@MainActor
final class AdaptiveNavigationAndActionsTests: XCTestCase {
    func testLayoutRequiresUsefulWidthAndHeightForSplitMode() {
        XCTAssertEqual(
            RemoraNavigationLayoutPolicy.mode(for: CGSize(width: 1024, height: 768)),
            .split
        )
        XCTAssertEqual(
            RemoraNavigationLayoutPolicy.mode(for: CGSize(width: 852, height: 393)),
            .compact,
            "A wide phone landscape must not become an unusably short split view."
        )
        XCTAssertEqual(
            RemoraNavigationLayoutPolicy.mode(for: CGSize(width: 700, height: 900)),
            .compact
        )
    }

    func testCompactConversationSelectionKeepsBackPath() {
        let first = ThreadKey(serverId: "server-a", threadId: "thread-a")
        let second = ThreadKey(serverId: "server-a", threadId: "thread-b")
        let path = HomeNavigationPathPolicy.selectingConversation(
            second,
            in: [.conversation(first)],
            mode: .compact
        )

        XCTAssertEqual(path, [.conversation(first), .conversation(second)])
    }

    func testSplitConversationSelectionReplacesPeerDetail() {
        let first = ThreadKey(serverId: "server-a", threadId: "thread-a")
        let second = ThreadKey(serverId: "server-b", threadId: "thread-b")
        let path = HomeNavigationPathPolicy.selectingConversation(
            second,
            in: [.conversation(first), .conversationInfo(first)],
            mode: .split
        )

        XCTAssertEqual(path, [.conversation(second)])
        XCTAssertEqual(path.last?.conversationKey, second)
    }

    func testTerminalRoutePreservesPreferredRemoraLinkHost() {
        let route = HomeNavigationRoute.terminal(preferredRemoraLinkHostId: "host-1")
        guard case let .terminal(hostId) = route else {
            return XCTFail("Expected a terminal route")
        }
        XCTAssertEqual(hostId, "host-1")
    }

    func testRemoraLinkTerminalEligibilityRequiresPairedExactShellAndConnectRuntime() {
        let eligible = remoraLinkHost()
        let wrongState = remoraLinkHost(id: "ready", state: .ready)
        let wrongCase = remoraLinkHost(id: "wrong-case", runtimeIds: ["Shell"])
        let missingShell = remoraLinkHost(id: "missing-shell", runtimeIds: ["codex"])
        let missingScope = remoraLinkHost(id: "missing-scope", scopes: [.inspectRuntimes])

        let result = RemoraLinkTerminalSupport.eligibleHosts(
            from: [eligible, wrongState, wrongCase, missingShell, missingScope]
        )

        XCTAssertEqual(result, [RemoraLinkTerminalHost(hostId: "host-1", displayName: "Studio")])
        XCTAssertEqual(RemoraLinkTerminalSupport.eligibleHostIds(from: [eligible]), Set(["host-1"]))
    }

    func testRemoraLinkTerminalBackendCarriesOnlyHostId() {
        let result = RemoraLinkTerminalSupport.eligibleHosts(from: [remoraLinkHost()])

        XCTAssertEqual(result.first?.backend, .remoteRemoraLink(hostId: "host-1", shell: nil))
    }

    func testRemoraLinkTerminalEmptyDisplayNameUsesNeutralFallback() {
        let result = RemoraLinkTerminalSupport.eligibleHosts(
            from: [remoraLinkHost(displayName: "  \n")]
        )

        XCTAssertEqual(result.first?.displayName, "Remote shell")
    }

    func testPreferredRemoraLinkHostFailsClosedWhenMissingOrEmpty() {
        let hosts = [
            RemoraLinkTerminalHost(hostId: "host-a", displayName: "A"),
            RemoraLinkTerminalHost(hostId: "host-b", displayName: "B")
        ]

        XCTAssertNil(RemoraLinkTerminalSupport.initialOption(
            preferredHostId: "missing",
            options: hosts,
            hostId: \.hostId
        ))
        XCTAssertNil(RemoraLinkTerminalSupport.initialOption(
            preferredHostId: "  ",
            options: hosts,
            hostId: \.hostId
        ))
        XCTAssertEqual(
            RemoraLinkTerminalSupport.initialOption(
                preferredHostId: nil,
                options: hosts,
                hostId: \.hostId
            ),
            hosts.first
        )
    }

    func testRemoraLinkEligibilityRejectsStaleCompletion() {
        var state = RemoraLinkTerminalEligibilityState()
        let staleGeneration = state.beginRequest()
        let currentGeneration = state.beginRequest()
        let hosts = [remoraLinkHost()]

        state.apply(hosts: hosts, serverId: "host-1", generation: staleGeneration)
        XCTAssertFalse(state.canOpenShell)

        state.apply(hosts: hosts, serverId: "host-1", generation: currentGeneration)
        XCTAssertTrue(state.canOpenShell)

        state.invalidate()
        XCTAssertFalse(state.canOpenShell)
        state.apply(hosts: hosts, serverId: "host-1", generation: currentGeneration)
        XCTAssertFalse(state.canOpenShell)
    }

    func testCatalogUsesLiveContextForPaletteAndShortcutAvailability() {
        var context = RemoraActionNavigationContext.empty
        context.canStartThread = true
        context.canSearchThreads = true
        context.isConversationVisible = true

        let unfocused = RemoraActionCatalog.items(context: context, composerIsFocused: false)
        let focused = RemoraActionCatalog.items(context: context, composerIsFocused: true)

        XCTAssertFalse(unfocused.first(where: { $0.id == .sendMessage })!.availability.isEnabled)
        XCTAssertTrue(focused.first(where: { $0.id == .sendMessage })!.availability.isEnabled)
        XCTAssertTrue(focused.first(where: { $0.id == .newThread })!.availability.isEnabled)
    }

    func testTerminalContextDisablesGlobalNavigationAndCyclingKeys() {
        var context = RemoraActionNavigationContext.empty
        context.canSearchThreads = true
        context.canCycleThreads = true
        context.canNavigateForward = true
        context.terminalOwnsKeyboard = true

        let items = RemoraActionCatalog.items(context: context, composerIsFocused: false)

        XCTAssertFalse(items.first(where: { $0.id == .searchThreads })!.availability.isEnabled)
        XCTAssertFalse(items.first(where: { $0.id == .nextThread })!.availability.isEnabled)
        XCTAssertFalse(items.first(where: { $0.id == .navigateForward })!.availability.isEnabled)
        XCTAssertTrue(items.first(where: { $0.id == .showSettings })!.availability.isEnabled)
    }

    func testPaletteFilteringIsBoundedToCatalogAndSearchTerms() {
        let items = RemoraActionCatalog.items(context: .empty, composerIsFocused: false)
        let results = RemoraActionCatalog.filteredItems(items, query: "shell")

        XCTAssertEqual(results.map(\.id), [.openTerminal])
        XCTAssertFalse(results.contains { $0.id == .showCommandPalette })
    }

    func testOnlyTheLatestFocusedComposerOwnsSendAction() {
        let center = RemoraActionCenter()
        let first = UUID()
        let second = UUID()

        center.setComposerFocused(true, owner: first)
        XCTAssertTrue(center.composerOwnsFocus(first))

        center.setComposerFocused(true, owner: second)
        center.setComposerFocused(false, owner: first)

        XCTAssertFalse(center.composerOwnsFocus(first))
        XCTAssertTrue(center.composerOwnsFocus(second))

        center.setComposerFocused(false, owner: second)
        XCTAssertFalse(center.composerOwnsFocus(second))
    }

    private func remoraLinkHost(
        id: String = "host-1",
        displayName: String = "Studio",
        state: AppRemoraLinkHostState = .paired,
        runtimeIds: [String] = ["codex", "shell"],
        scopes: [AppRemoraLinkScope] = [.inspectRuntimes, .connectRuntime]
    ) -> AppRemoraLinkHostSummary {
        AppRemoraLinkHostSummary(
            hostId: id,
            hostDisplayName: displayName,
            state: state,
            selectedRuntimeIds: runtimeIds,
            grantedScopes: scopes,
            pendingApproval: nil,
            pendingRestart: nil,
            hostRevocationStillRequired: false
        )
    }
}

@MainActor
final class TerminalSessionControllerRaceTests: XCTestCase {
    func testOpenCompletingAfterCloseDoesNotPublishOrRetainSession() async {
        let appStore = ControllableTerminalAppStore()
        let controller = TerminalSessionController(appStore: appStore)
        let openTask = Task {
            await controller.open(
                backend: .remoteRemoraLink(hostId: "closing-host", shell: nil)
            )
        }

        await appStore.waitForOpenCount(1)
        controller.close()
        appStore.completeOpen(number: 1, sessionId: "stale-session")
        await openTask.value

        XCTAssertEqual(controller.phase, .idle)
        XCTAssertNil(controller.sessionId)
        XCTAssertNil(appStore.activeTerminalId())
        XCTAssertFalse(appStore.hasLiveSession(id: "stale-session"))
        XCTAssertEqual(appStore.closedSessionIds, ["stale-session"])
    }

    func testStaleOpenCannotOverwriteNewerSwitchedSession() async {
        let appStore = ControllableTerminalAppStore()
        let controller = TerminalSessionController(appStore: appStore)
        let firstOpenTask = Task {
            await controller.open(
                backend: .remoteRemoraLink(hostId: "first-host", shell: nil)
            )
        }

        await appStore.waitForOpenCount(1)
        let switchTask = Task {
            await controller.switchBackend(
                .remoteRemoraLink(hostId: "current-host", shell: nil)
            )
        }
        await appStore.waitForOpenCount(2)

        appStore.completeOpen(number: 2, sessionId: "current-session")
        await switchTask.value
        appStore.completeOpen(number: 1, sessionId: "stale-session")
        await firstOpenTask.value

        XCTAssertEqual(controller.phase, .running)
        XCTAssertEqual(controller.sessionId, "current-session")
        XCTAssertEqual(appStore.activeTerminalId(), "current-session")
        XCTAssertTrue(appStore.hasLiveSession(id: "current-session"))
        XCTAssertFalse(appStore.hasLiveSession(id: "stale-session"))
        XCTAssertEqual(appStore.closedSessionIds, ["stale-session"])
    }
}

private final class ControllableTerminalAppStore: AppStore, @unchecked Sendable {
    private struct OpenWaiter {
        let count: Int
        let continuation: CheckedContinuation<Void, Never>
    }

    private let lock = NSLock()
    private var openCount = 0
    private var pendingOpens: [Int: CheckedContinuation<String, any Error>] = [:]
    private var openWaiters: [OpenWaiter] = []
    private var liveSessions: [String: TerminalSession] = [:]
    private var activeSessionId: String?
    private var recordedClosedSessionIds: [String] = []

    init() {
        super.init(noHandle: AppStore.NoHandle())
    }

    required init(unsafeFromHandle handle: UInt64) {
        fatalError("init(unsafeFromHandle:) is unavailable in tests")
    }

    override func openTerminalSession(
        kind: TerminalBackendKind,
        size: TerminalSize
    ) async throws -> String {
        try await enqueueOpen()
    }

    override func openTerminalSessionWithTrustStore(
        kind: TerminalBackendKind,
        size: TerminalSize,
        trustStore: TerminalSshTrustStore
    ) async throws -> String {
        try await enqueueOpen()
    }

    override func closeTerminalSession(id: String) async throws {
        lock.withLock {
            recordedClosedSessionIds.append(id)
            liveSessions[id] = nil
            if activeSessionId == id {
                activeSessionId = nil
            }
        }
    }

    override func activeTerminalId() -> String? {
        lock.withLock { activeSessionId }
    }

    override func setActiveTerminalId(id: String?) {
        lock.withLock {
            activeSessionId = id
        }
    }

    override func terminalSessionHandle(id: String) -> TerminalSession? {
        lock.withLock { liveSessions[id] }
    }

    var closedSessionIds: [String] {
        lock.withLock { recordedClosedSessionIds }
    }

    func hasLiveSession(id: String) -> Bool {
        lock.withLock { liveSessions[id] != nil }
    }

    func waitForOpenCount(_ expectedCount: Int) async {
        await withCheckedContinuation { continuation in
            let shouldResume = lock.withLock {
                if openCount >= expectedCount {
                    return true
                }
                openWaiters.append(OpenWaiter(count: expectedCount, continuation: continuation))
                return false
            }
            if shouldResume {
                continuation.resume()
            }
        }
    }

    func completeOpen(number: Int, sessionId: String) {
        let continuation = lock.withLock {
            liveSessions[sessionId] = FakeTerminalSession()
            return pendingOpens.removeValue(forKey: number)
        }
        guard let continuation else {
            return XCTFail("No pending terminal open numbered \(number)")
        }
        continuation.resume(returning: sessionId)
    }

    private func enqueueOpen() async throws -> String {
        try await withCheckedThrowingContinuation { continuation in
            let readyWaiters: [CheckedContinuation<Void, Never>] = lock.withLock {
                openCount += 1
                pendingOpens[openCount] = continuation
                let ready = openWaiters
                    .filter { $0.count <= openCount }
                    .map(\.continuation)
                openWaiters.removeAll { $0.count <= openCount }
                return ready
            }
            readyWaiters.forEach { $0.resume() }
        }
    }
}

private final class FakeTerminalSession: TerminalSession, @unchecked Sendable {
    init() {
        super.init(noHandle: TerminalSession.NoHandle())
    }

    required init(unsafeFromHandle handle: UInt64) {
        fatalError("init(unsafeFromHandle:) is unavailable in tests")
    }

    override func subscribeOutputEvents(
        listener: TerminalOutputEventListener
    ) -> TerminalOutputSubscription {
        FakeTerminalOutputSubscription()
    }
}

private final class FakeTerminalOutputSubscription: TerminalOutputSubscription, @unchecked Sendable {
    init() {
        super.init(noHandle: TerminalOutputSubscription.NoHandle())
    }

    required init(unsafeFromHandle handle: UInt64) {
        fatalError("init(unsafeFromHandle:) is unavailable in tests")
    }

    override func cancel() {}
}
