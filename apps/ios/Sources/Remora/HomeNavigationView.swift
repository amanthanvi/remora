import Combine
import SwiftUI
import UIKit
import os

private let homeNavigationSignpostLog = OSLog(
    subsystem: Bundle.main.bundleIdentifier ?? "com.remora.app.ios",
    category: "HomeNavigation"
)

struct HomeNavigationView: View {
    @Environment(AppModel.self) private var appModel
    @Environment(VoiceRuntimeController.self) private var voiceRuntime
    @Environment(AppState.self) private var appState
    @Environment(ConversationWarmupCoordinator.self) private var conversationWarmup
    @AppStorage("workDir") private var workDir = FileManager.default.urls(for: .documentDirectory, in: .userDomainMask).first?.path ?? "/"
    @State private var experimentalFeatures = ExperimentalFeatures.shared
    @State private var actionCenter = RemoraActionCenter.shared
    @State private var homeDashboardModel = HomeDashboardModel()
    @State private var navigationPath: [HomeNavigationRoute] = []
    @State private var directoryPickerSheet: SessionLaunchSupport.DirectoryPickerSheetModel?
    @State private var showProjectPicker = false
    @State private var openingRecentSessionKey: ThreadKey?
    @State private var isStartingNewSession = false
    @State private var isStartingVoice = false
    @State private var actionErrorMessage: String?
    @State private var homeInputMode: HomeInputMode = .collapsed
    @State private var hydratingPinnedHomeThreadIds: Set<String> = []
    @State private var pinnedThreadListingRepairTasks: [String: Task<Bool, Never>] = [:]
    @State private var hasSeededInitialConversationRoute = false
    @State private var pendingWallpaperConfig: WallpaperConfig?
    @State private var pendingWallpaperImage: UIImage?
    @State private var navigationMode: RemoraNavigationMode = .compact
    let topInset: CGFloat
    let bottomInset: CGFloat

    private var connectedServerOptions: [DirectoryPickerServerOption] {
        homeDashboardModel.connectedServers.filter(\.canLaunchSessions).map { server in
            DirectoryPickerServerOption(
                id: server.id,
                name: server.displayName,
                sourceLabel: server.sourceLabel
            )
        }
    }

    private var isHomeRouteActive: Bool {
        navigationPath.isEmpty
    }

    private var terminalLauncher: (() -> Void)? {
        #if targetEnvironment(macCatalyst)
        return nil
        #else
        guard experimentalFeatures.isEnabled(.terminal) else { return nil }
        return { navigationPath.append(.terminal(preferredRemoraLinkHostId: nil)) }
        #endif
    }

    private var visibleConversationKey: ThreadKey? {
        navigationPath.last?.conversationKey
    }

    private var actionContext: RemoraActionNavigationContext {
        let navigationObservation = appModel.navigationObservation
        let activeKey = navigationObservation.activeThread
        let sessions = navigationObservation.sessionTargets
        let terminalOwnsKeyboard = navigationPath.last?.ownsTerminalKeyboard == true
        return RemoraActionNavigationContext(
            canStartThread: defaultNewSessionServerId(
                preferredServerId: homeDashboardModel.selectedServerId
            ) != nil,
            canSearchThreads: navigationMode == .split || navigationPath.isEmpty,
            canNavigateBack: !navigationPath.isEmpty,
            canNavigateForward: activeKey != nil && visibleConversationKey != activeKey,
            canCycleThreads: sessions.count > 1,
            canOpenTerminal: terminalLauncher != nil,
            isConversationVisible: visibleConversationKey != nil,
            terminalOwnsKeyboard: terminalOwnsKeyboard
        )
    }

    private var pinnedThreadHydrationSignature: String {
        let pins = homeDashboardModel.pinnedKeys
            .map { "\($0.serverId)/\($0.threadId)" }
            .joined(separator: "|")
        let pinnedSet = Set(homeDashboardModel.pinnedKeys)
        let servers = appModel.navigationObservation.servers
            .map { "\($0.serverId)=\(String(describing: $0.transportState)):\($0.port)" }
            .joined(separator: "|")
        let sessions = appModel.navigationObservation.sessionTargets
            .compactMap { summary -> String? in
                guard pinnedSet.contains(PinnedThreadKey(threadKey: summary.key)) else { return nil }
                return "\(homeHydrationId(summary.key)):\(summary.isResumed)"
            }
            .joined(separator: "|")
        return "\(pins)|\(servers)|\(sessions)"
    }

    @ViewBuilder
    private func rootNavigationContent(for mode: RemoraNavigationMode) -> some View {
        if mode == .split {
            splitRoot
        } else {
            primaryNavigationStack(isEmbeddedInSplit: false)
        }
    }

    private var splitRoot: some View {
        NavigationSplitView {
            sidebarDashboard
                // Apply Liquid Glass material explicitly to the sidebar
                // column. Catalyst 26 doesn't automatically paint the
                // sidebar with glass the way iPadOS does, so the column
                // comes through flat unless we install the material
                // ourselves. `.ultraThinMaterial` gives the proper
                // sidebar frosted-glass look with subtle vibrancy.
                .containerBackground(.ultraThinMaterial, for: .navigation)
        } detail: {
            primaryNavigationStack(isEmbeddedInSplit: true)
        }
    }

    private func primaryNavigationStack(isEmbeddedInSplit: Bool) -> some View {
        NavigationStack(path: $navigationPath) {
            Group {
                if isHomeRouteActive {
                    if isEmbeddedInSplit {
                        splitDetailRoot
                    } else {
                        homeDashboard
                    }
                } else {
                    RemoraTheme.backgroundGradient.ignoresSafeArea()
                }
            }
            .overlay(alignment: .bottomLeading) {
                if isHomeRouteActive,
                   experimentalFeatures.isEnabled(.realtimeVoice),
                   homeInputMode == .collapsed {
                    homeVoiceLauncher
                }
            }
            .navigationDestination(for: HomeNavigationRoute.self) { route in
                switch route {
                case .allSessions:
                    CommandCenterSessionsView(
                        serverId: nil,
                        onOpenConversation: openConversation
                    )
                case let .sessions(serverId, title):
                    SessionsScreen(
                        onOpenConversation: { key in
                            openConversation(key)
                        },
                        onInfo: {
                            navigationPath.append(.serverInfo(serverId: serverId))
                        }
                    )
                        .navigationTitle(title)
                        .navigationBarTitleDisplayMode(.inline)
                        .frame(maxWidth: .infinity, maxHeight: .infinity)
                        .background(RemoraTheme.backgroundGradient.ignoresSafeArea())
                        .onAppear {
                            appState.sessionsSelectedServerFilterId = serverId
                            appState.sessionsShowOnlyForks = false
                        }
                case let .conversation(threadKey):
                    ConversationDestinationScreen(
                        threadKey: threadKey,
                        bottomInset: bottomInset,
                        onResumeSessions: { showSessions(for: $0) },
                        onOpenConversation: { replaceTopConversation(with: $0) },
                        onInfo: { navigationPath.append(.conversationInfo(threadKey)) }
                    )
                case .newThread:
                    NewThreadHeroView(
                        project: homeDashboardModel.selectedProject,
                        connectedServers: homeDashboardModel.connectedServers,
                        selectedServerId: homeDashboardModel.selectedServerId,
                        onSelectServer: { serverId in
                            homeDashboardModel.selectedServerId = serverId
                        },
                        onOpenProjectPicker: { showProjectPicker = true },
                        onThreadCreated: { key in
                            homeDashboardModel.pinThread(key)
                            replaceHeroWithConversation(key: key)
                        },
                        onCancel: {
                            if case .newThread = navigationPath.last {
                                navigationPath.removeLast()
                            }
                        }
                    )
                case let .replayRecording(recordingUrl):
                    ReplayDestinationScreen(
                        recordingUrl: recordingUrl,
                        bottomInset: bottomInset
                    )
                case let .realtimeVoice(threadKey):
                    RealtimeVoiceScreen(
                        threadKey: threadKey,
                        onEnd: {
                            popCurrentRoute()
                            Task { await voiceRuntime.stopActiveVoiceSession() }
                        },
                        onToggleSpeaker: {
                            Task { try? await voiceRuntime.toggleActiveVoiceSessionSpeaker() }
                        }
                    )
                    .toolbar(.hidden, for: .navigationBar)
                    .background(RemoraTheme.backgroundGradient.ignoresSafeArea())
                case let .conversationInfo(threadKey):
                    ConversationInfoView(
                        threadKey: threadKey,
                        serverId: nil,
                        onOpenWallpaper: { navigationPath.append(.wallpaperSelection(threadKey)) },
                        onOpenConversation: { replaceTopConversation(with: $0) }
                    )
                case let .wallpaperSelection(threadKey):
                    WallpaperSelectionView(
                        threadKey: threadKey,
                        onSelectWallpaper: { config, image in
                            pendingWallpaperConfig = config
                            pendingWallpaperImage = image
                            navigationPath.append(.wallpaperAdjust(threadKey))
                        },
                        onClose: {
                            // Pop back to conversation info
                            popToConversationInfo()
                        }
                    )
                    .toolbar(.hidden, for: .navigationBar)
                    .background(RemoraTheme.backgroundGradient.ignoresSafeArea())
                case let .wallpaperAdjust(threadKey):
                    WallpaperAdjustView(
                        threadKey: threadKey,
                        initialConfig: pendingWallpaperConfig ?? WallpaperConfig(),
                        customImage: pendingWallpaperImage,
                        onDone: {
                            // Pop back to conversation info
                            popToConversationInfo()
                        }
                    )
                    .toolbar(.hidden, for: .navigationBar)
                    .background(RemoraTheme.backgroundGradient.ignoresSafeArea())
                case let .serverInfo(serverId):
                    RemoraLinkServerInfoDestination(
                        appModel: appModel,
                        serverId: serverId,
                        onOpenWallpaper: { navigationPath.append(.serverWallpaperSelection(serverId: serverId)) },
                        onOpenShell: remoteShellLauncher(for: serverId)
                    )
                case let .serverWallpaperSelection(serverId):
                    WallpaperSelectionView(
                        threadKey: nil,
                        serverId: serverId,
                        onSelectWallpaper: { config, image in
                            pendingWallpaperConfig = config
                            pendingWallpaperImage = image
                            navigationPath.append(.serverWallpaperAdjust(serverId: serverId))
                        },
                        onClose: {
                            popToServerInfo()
                        }
                    )
                    .toolbar(.hidden, for: .navigationBar)
                    .background(RemoraTheme.backgroundGradient.ignoresSafeArea())
                case let .serverWallpaperAdjust(serverId):
                    WallpaperAdjustView(
                        threadKey: nil,
                        serverId: serverId,
                        initialConfig: pendingWallpaperConfig ?? WallpaperConfig(),
                        customImage: pendingWallpaperImage,
                        onDone: {
                            popToServerInfo()
                        }
                    )
                    .toolbar(.hidden, for: .navigationBar)
                    .background(RemoraTheme.backgroundGradient.ignoresSafeArea())
                case let .terminal(preferredRemoraLinkHostId):
                    TerminalScreen(
                        cwd: preferredTerminalWorkingDirectory(),
                        preferredRemoraLinkHostId: preferredRemoraLinkHostId
                    )
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
                }
            }
        }
    }

    var body: some View {
        let navigationObservation = appModel.navigationObservation
        let _ = navigationObservation.revision

        GeometryReader { geometry in
            let resolvedMode = RemoraNavigationLayoutPolicy.mode(for: geometry.size)
            rootNavigationContent(for: resolvedMode)
                .onAppear {
                    navigationMode = resolvedMode
                }
                .onChange(of: resolvedMode) { _, nextMode in
                    // Route state is deliberately untouched. The selected
                    // detail therefore survives Stage Manager, rotation, and
                    // split-screen threshold crossings.
                    var transaction = Transaction()
                    transaction.disablesAnimations = true
                    withTransaction(transaction) {
                        navigationMode = nextMode
                    }
                }
        }
        .task {
            homeDashboardModel.bind(appModel: appModel)
            updateHomeDashboardActivity()
            hydratePinnedThreadsIfNeeded()
            seedInitialConversationIfNeeded(activeKey: navigationObservation.activeThread)
            syncActionContext()
        }
        .onChange(of: navigationObservation.activeThread) { _, newKey in
            seedInitialConversationIfNeeded(activeKey: newKey)
        }
        .onChange(of: navigationPath.count) { _, _ in
            updateHomeDashboardActivity()
            syncActionContext()
        }
        .onChange(of: navigationPath.last) { _, _ in
            syncActionContext()
        }
        .onChange(of: navigationMode) { _, _ in
            syncActionContext()
        }
        .onChange(of: actionContext) { _, _ in
            syncActionContext()
        }
        .onChange(of: pinnedThreadHydrationSignature) { _, _ in
            hydratePinnedThreadsIfNeeded()
        }
        .onChange(of: appState.pendingThreadNavigation) { _, newKey in
            if let newKey {
                appState.pendingThreadNavigation = nil
                replaceTopConversation(with: newKey)
            }
        }
        .onReceive(NotificationCenter.default.publisher(for: .remoraActionRequested)) { notification in
            guard let request = notification.object as? RemoraActionRequest else { return }
            handleActionRequest(request)
        }
        #if targetEnvironment(macCatalyst)
        .onReceive(NotificationCenter.default.publisher(for: .remoraCommandSelectSession)) { notification in
            guard let index = notification.userInfo?["index"] as? Int,
                  navigationObservation.sessionTargets.indices.contains(index) else { return }
            Task { @MainActor in
                _ = await openSessionAtIndex(navigationObservation.sessionTargets[index])
            }
        }
        #endif
        .sheet(item: $directoryPickerSheet) { _ in
            NavigationStack {
                DirectoryPickerView(
                    servers: connectedServerOptions,
                    selectedServerId: Binding(
                        get: { directoryPickerSheet?.selectedServerId ?? defaultNewSessionServerId() ?? "" },
                        set: { nextServerId in
                            guard var sheet = directoryPickerSheet else { return }
                            sheet.selectedServerId = nextServerId
                            directoryPickerSheet = sheet
                        }
                    ),
                    onServerChanged: { nextServerId in
                        guard var sheet = directoryPickerSheet else { return }
                        sheet.selectedServerId = nextServerId
                        directoryPickerSheet = sheet
                    },
                    onDirectorySelected: { serverId, cwd in
                        directoryPickerSheet = nil
                        createAndSelectProject(serverId: serverId, cwd: cwd)
                    },
                    onDismissRequested: {
                        directoryPickerSheet = nil
                    }
                )
            }
            .environment(appModel)
        }
        .sheet(isPresented: $showProjectPicker) {
            ProjectPickerSheet(
                projects: homeDashboardModel.projects,
                serverNamesById: Dictionary(
                    homeDashboardModel.connectedServers.map { ($0.id, $0.displayName) },
                    uniquingKeysWith: { _, latest in latest }
                ),
                onSelect: { project in
                    homeDashboardModel.selectedServerId = project.serverId
                    homeDashboardModel.selectedProject = project
                },
                onCreateNew: {
                    showProjectPicker = false
                    let defaultServerId = homeDashboardModel.selectedServerId ?? defaultNewSessionServerId()
                    if let defaultServerId {
                        directoryPickerSheet = SessionLaunchSupport.DirectoryPickerSheetModel(selectedServerId: defaultServerId)
                    } else {
                        appState.showServerPicker = true
                    }
                }
            )
            .environment(appModel)
        }
        .alert("Home Action Failed", isPresented: Binding(
            get: { actionErrorMessage != nil },
            set: { if !$0 { actionErrorMessage = nil } }
        )) {
            Button("OK", role: .cancel) { actionErrorMessage = nil }
        } message: {
            Text(actionErrorMessage ?? "Unknown error")
        }
    }

    private func defaultNewSessionServerId(preferredServerId: String? = nil) -> String? {
        SessionLaunchSupport.defaultConnectedServerId(
            connectedServerIds: connectedServerOptions.map(\.id),
            activeThreadKey: appModel.navigationObservation.activeThread,
            preferredServerId: preferredServerId
        )
    }

    private func syncActionContext() {
        actionCenter.updateNavigationContext(actionContext)
    }

    private func handleActionRequest(_ request: RemoraActionRequest) {
        // SwiftUI can coalesce route and snapshot observation. Refresh the
        // catalog from the view's current values before accepting a request
        // that may have been issued from an older palette row or menu state.
        syncActionContext()
        guard request.contextRevision == actionCenter.contextRevision,
              actionCenter.item(for: request.id).availability.isEnabled else {
            request.finish(errorMessage: "The active destination changed. Choose the action again.")
            return
        }

        switch request.id {
        case .showCommandPalette:
            actionCenter.presentPalette()
            request.finish()
        case .newThread:
            openNewThread()
            request.finish()
        case .searchThreads:
            if navigationMode == .compact {
                navigationPath.removeAll()
            }
            homeInputMode = .search
            request.finish()
        case .navigateBack:
            popCurrentRoute()
            request.finish()
        case .navigateForward:
            guard let activeKey = appModel.navigationObservation.activeThread else {
                request.finish(errorMessage: "There is no active thread to open.")
                return
            }
            navigationPath = HomeNavigationPathPolicy.selectingConversation(
                activeKey,
                in: navigationPath,
                mode: navigationMode
            )
            request.finish()
        case .previousThread:
            cycleThread(by: -1, request: request)
        case .nextThread:
            cycleThread(by: 1, request: request)
        case .sendMessage:
            // The focused composer is the deepest handler and completes the
            // request after it has revalidated its own input state.
            break
        case .openTerminal:
            terminalLauncher?()
            request.finish()
        case .showSettings:
            appState.showSettings = true
            request.finish()
        }
    }

    private func cycleThread(by offset: Int, request: RemoraActionRequest) {
        let sessions = appModel.navigationObservation.sessionTargets
        guard sessions.count > 1 else {
            request.finish(errorMessage: "Open at least two threads to cycle between them.")
            return
        }

        let currentKey = visibleConversationKey ?? appModel.navigationObservation.activeThread
        let currentIndex = currentKey.flatMap { key in
            sessions.firstIndex(where: { $0.key == key })
        } ?? (offset > 0 ? -1 : 0)
        let targetIndex = (currentIndex + offset + sessions.count) % sessions.count
        let target = sessions[targetIndex]

        Task { @MainActor in
            let errorMessage = await openSessionAtIndex(
                target,
                reportsError: request.source != .palette
            )
            request.finish(errorMessage: errorMessage)
        }
    }

    private func createAndSelectProject(serverId: String, cwd: String) {
        homeDashboardModel.selectFreshProject(serverId: serverId, cwd: cwd)
        RecentDirectoryStore.shared.record(path: cwd, for: serverId)
    }

    private func handleNewSessionTap() {
        if let defaultServerId = defaultNewSessionServerId(preferredServerId: appState.sessionsSelectedServerFilterId) {
            directoryPickerSheet = SessionLaunchSupport.DirectoryPickerSheetModel(selectedServerId: defaultServerId)
        } else {
            appState.showServerPicker = true
        }
    }

    private var homeVoiceLauncher: some View {
        HomeVoiceOrbButton(
            session: voiceRuntime.activeVoiceSession,
            isAvailable: defaultNewSessionServerId(preferredServerId: homeDashboardModel.selectedServerId) != nil,
            isStarting: isStartingVoice,
            action: startHomeVoiceSession
        )
        // Match the bottom inset used by `HomeBottomBar` inside
        // `HomeDashboardView.bottomChrome` so the mic button sits on the
        // same horizontal line as the `+` and search pills on the right.
        .padding(.leading, 14)
        .padding(.bottom, 4)
    }

    private func startHomeVoiceSession() {
        guard !isStartingVoice else { return }
        isStartingVoice = true
        actionErrorMessage = nil

        Task {
            do {
                guard let serverId = defaultNewSessionServerId(
                    preferredServerId: homeDashboardModel.selectedServerId
                ) else {
                    throw NSError(
                        domain: "Remora",
                        code: 3301,
                        userInfo: [NSLocalizedDescriptionKey: "Connect a remote server before starting voice."]
                    )
                }
                let selectedModel = normalizedPreferredModel()
                let selectedEffort = appState.preferredReasoningEffort.trimmingCharacters(in: .whitespacesAndNewlines)
                voiceRuntime.handoffModel = selectedModel
                voiceRuntime.handoffEffort = selectedEffort.isEmpty ? nil : selectedEffort
                voiceRuntime.handoffFastMode = false
                let voicePermissions = await voicePermissionConfig(serverId: serverId)
                let voiceKey = try await voiceRuntime.startPinnedVoiceCall(
                    serverId: serverId,
                    cwd: preferredVoiceWorkingDirectory(),
                    model: selectedModel,
                    approvalPolicy: voicePermissions.approvalPolicy,
                    sandboxMode: voicePermissions.sandboxMode
                )
                await MainActor.run {
                    openRealtimeVoice(voiceKey)
                }
            } catch {
                await MainActor.run {
                    actionErrorMessage = error.localizedDescription
                }
            }
            await MainActor.run {
                isStartingVoice = false
            }
        }
    }

    private func normalizedPreferredModel() -> String? {
        let trimmed = appState.preferredModel.trimmingCharacters(in: .whitespacesAndNewlines)
        return trimmed.isEmpty ? nil : trimmed
    }

    private func preferredVoiceWorkingDirectory() -> String {
        let current = appState.currentCwd.trimmingCharacters(in: .whitespacesAndNewlines)
        if !current.isEmpty {
            return current
        }

        let stored = UserDefaults.standard.string(forKey: "workDir")?
            .trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
        if !stored.isEmpty {
            return stored
        }

        return "/"
    }

    private func preferredTerminalWorkingDirectory() -> String? {
        let current = appState.currentCwd.trimmingCharacters(in: .whitespacesAndNewlines)
        if !current.isEmpty { return current }

        let stored = workDir.trimmingCharacters(in: .whitespacesAndNewlines)
        if !stored.isEmpty { return stored }

        return nil
    }

    private func remoteShellLauncher(for serverId: String) -> (() -> Void)? {
        guard experimentalFeatures.isEnabled(.terminal) else { return nil }
        return {
            navigationPath.append(.terminal(preferredRemoraLinkHostId: serverId))
        }
    }

    private func normalizedNonEmpty(_ value: String?) -> String? {
        let trimmed = value?.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
        return trimmed.isEmpty ? nil : trimmed
    }

    private func openServerSessions(_ server: HomeDashboardServer) {
        appState.sessionsSelectedServerFilterId = server.id
        appState.sessionsShowOnlyForks = false
        hasSeededInitialConversationRoute = true
        navigationPath.append(.sessions(serverId: server.id, title: server.displayName))
    }

    private func openSessionAtIndex(
        _ summary: AppModelNavigationObservation.SessionTarget,
        reportsError: Bool = true
    ) async -> String? {
        guard openingRecentSessionKey == nil else {
            return "Another thread is still opening."
        }
        openingRecentSessionKey = summary.key
        actionErrorMessage = nil
        defer { openingRecentSessionKey = nil }

        await conversationWarmup.prewarmIfNeeded()
        workDir = summary.cwd
        appState.currentCwd = summary.cwd
        do {
            let resumeKey = await appModel.hydrateThreadPermissions(for: summary.key, appState: appState)
                ?? summary.key
            let nextKey = try await appModel.resumeThread(
                key: resumeKey,
                launchConfig: launchConfig(for: resumeKey),
                cwdOverride: summary.cwd
            )
            appModel.activateThread(nextKey)
            replaceTopConversation(with: nextKey)
            return nil
        } catch {
            if reportsError {
                actionErrorMessage = error.localizedDescription
            }
            return error.localizedDescription
        }
    }

    private func openRecentSession(_ thread: HomeDashboardRecentSession) async {
        guard openingRecentSessionKey == nil else { return }

        openingRecentSessionKey = thread.key
        actionErrorMessage = nil
        defer { openingRecentSessionKey = nil }

        await conversationWarmup.prewarmIfNeeded()
        workDir = thread.cwd
        appState.currentCwd = thread.cwd
        let openedKey: ThreadKey?
        do {
            let resumeKey = await appModel.hydrateThreadPermissions(for: thread.key, appState: appState)
                ?? thread.key
            let nextKey = try await appModel.resumeThread(
                key: resumeKey,
                launchConfig: launchConfig(for: resumeKey),
                cwdOverride: thread.cwd
            )
            appModel.activateThread(nextKey)
            openedKey = nextKey
        } catch {
            actionErrorMessage = error.localizedDescription
            openedKey = nil
        }
        guard let openedKey else {
            actionErrorMessage = actionErrorMessage ?? "Failed to open conversation."
            return
        }
        openConversation(openedKey)
    }

    private func startNewSession(serverId: String, cwd: String) async {
        guard !isStartingNewSession else { return }
        let signpostID = OSSignpostID(log: homeNavigationSignpostLog)
        os_signpost(
            .begin,
            log: homeNavigationSignpostLog,
            name: "StartNewSession",
            signpostID: signpostID,
            "server=%{public}@ cwd=%{public}@",
            serverId,
            cwd
        )
        isStartingNewSession = true
        defer {
            isStartingNewSession = false
            os_signpost(.end, log: homeNavigationSignpostLog, name: "StartNewSession", signpostID: signpostID)
        }
        actionErrorMessage = nil
        let startedKey: ThreadKey
        do {
            await conversationWarmup.prewarmIfNeeded()
            workDir = cwd
            appState.currentCwd = cwd
            let key = try await appModel.client.startThread(
                serverId: serverId,
                params: launchConfig().threadStartRequest(
                    cwd: cwd,
                    dynamicTools: nil
                )
            )
            startedKey = key
            RecentDirectoryStore.shared.record(path: cwd, for: serverId)
            homeDashboardModel.pinThread(key)
            appModel.store.setActiveThread(key: startedKey)
            await appModel.refreshThreadSnapshot(key: startedKey)
        } catch {
            actionErrorMessage = error.localizedDescription
            return
        }

        guard let resolvedKey = await appModel.ensureThreadLoaded(key: startedKey)
            ?? appModel.snapshot?.threadSnapshot(for: startedKey)?.key else {
            actionErrorMessage = appModel.lastError ?? "Failed to load the new session."
            return
        }

        openConversation(resolvedKey)
    }

    private func seedInitialConversationIfNeeded(activeKey: ThreadKey?) {
        guard !hasSeededInitialConversationRoute,
              !isStartingVoice,
              navigationPath.isEmpty,
              let activeKey else { return }

        Task { @MainActor in
            await conversationWarmup.prewarmIfNeeded()
            guard !hasSeededInitialConversationRoute,
                  !isStartingVoice,
                  navigationPath.isEmpty,
                  appModel.navigationObservation.activeThread == activeKey else {
                return
            }
            hasSeededInitialConversationRoute = true
            navigationPath = [.conversation(activeKey)]
        }
    }

    private func launchConfig(for threadKey: ThreadKey? = nil) -> AppThreadLaunchConfig {
        let selectedModel = appState.selectedModel.trimmingCharacters(in: .whitespacesAndNewlines)
        let hasSelectedModel = !selectedModel.isEmpty
        return AppThreadLaunchConfig(
            agentRuntimeKind: hasSelectedModel ? appState.selectedAgentRuntimeKind : nil,
            model: hasSelectedModel ? selectedModel : nil,
            approvalPolicy: appState.launchApprovalPolicy(for: threadKey),
            sandbox: appState.launchSandboxMode(for: threadKey),
            developerInstructions: nil,
            persistExtendedHistory: true
        )
    }

    private func voicePermissionConfig(serverId: String) async -> (
        approvalPolicy: AppAskForApproval?,
        sandboxMode: AppSandboxMode?
    ) {
        let storedServerId = UserDefaults.standard.string(forKey: VoiceRuntimeController.persistedVoiceServerIDKey)?
            .trimmingCharacters(in: .whitespacesAndNewlines)
        let storedThreadId = UserDefaults.standard.string(forKey: VoiceRuntimeController.persistedVoiceThreadIDKey)?
            .trimmingCharacters(in: .whitespacesAndNewlines)
        let threadKey = storedThreadId.flatMap { threadId -> ThreadKey? in
            guard !threadId.isEmpty, storedServerId == serverId else { return nil }
            return ThreadKey(serverId: serverId, threadId: threadId)
        }
        let resolvedThreadKey: ThreadKey?
        if let threadKey {
            resolvedThreadKey = await appModel.hydrateThreadPermissions(for: threadKey, appState: appState)
                ?? threadKey
        } else {
            resolvedThreadKey = nil
        }
        return (
            approvalPolicy: appState.launchApprovalPolicy(for: resolvedThreadKey),
            sandboxMode: appState.launchSandboxMode(for: resolvedThreadKey)
        )
    }

    private func openConversation(_ key: ThreadKey) {
        hasSeededInitialConversationRoute = true
        appState.showModelSelector = false
        navigationPath = HomeNavigationPathPolicy.selectingConversation(
            key,
            in: navigationPath,
            mode: navigationMode
        )
    }

    private func openRealtimeVoice(_ key: ThreadKey) {
        hasSeededInitialConversationRoute = true
        appState.showModelSelector = false
        guard navigationPath.last != .realtimeVoice(key) else { return }
        navigationPath.append(.realtimeVoice(key))
    }

    private func popToConversationInfo() {
        // Pop wallpaper selection and/or adjust screens, back to conversation info
        while let last = navigationPath.last {
            if case .conversationInfo = last { break }
            navigationPath.removeLast()
        }
    }

    private func popToServerInfo() {
        while let last = navigationPath.last {
            if case .serverInfo = last { break }
            navigationPath.removeLast()
        }
    }

    private func replaceTopConversation(with key: ThreadKey) {
        hasSeededInitialConversationRoute = true
        appState.showModelSelector = false
        navigationPath = HomeNavigationPathPolicy.replacingTopConversation(
            with: key,
            in: navigationPath,
            mode: navigationMode
        )
    }

    /// Ambient hero-composer rendering for the split-view detail root. No
    /// auto-focus (so popping back from a conversation doesn't summon the
    /// keyboard) and no Cancel toolbar (there's nothing to cancel to at the
    /// root). On send it replaces itself with `.conversation(key)` via the
    /// same path as the pushed hero, so the handoff is identical.
    private var splitDetailRoot: some View {
        NewThreadHeroView(
            project: homeDashboardModel.selectedProject,
            connectedServers: homeDashboardModel.connectedServers,
            selectedServerId: homeDashboardModel.selectedServerId,
            onSelectServer: { serverId in
                homeDashboardModel.selectedServerId = serverId
            },
            onOpenProjectPicker: { showProjectPicker = true },
            onThreadCreated: { key in
                homeDashboardModel.pinThread(key)
                // Root is already the hero; just push the conversation on top.
                openConversation(key)
            },
            onCancel: nil,
            autoFocus: false
        )
    }

    /// Push the hero composer into the detail pane. On compact this pushes
    /// `.newThread` as a destination; on split it's a no-op because the
    /// detail root already *is* the hero view (just pop back to it).
    private func openNewThread() {
        if navigationMode == .split {
            if !navigationPath.isEmpty {
                navigationPath.removeAll()
            }
            return
        }
        if case .newThread = navigationPath.last { return }
        if case .conversation = navigationPath.last {
            navigationPath.removeLast()
        }
        navigationPath.append(.newThread)
    }

    /// Swap the hero composer out for the freshly-created conversation in
    /// a single animation frame so the composer's apparent position is
    /// preserved by the glass morph.
    private func replaceHeroWithConversation(key: ThreadKey) {
        if case .newThread = navigationPath.last {
            navigationPath.removeLast()
        }
        openConversation(key)
    }

    private func popCurrentRoute() {
        guard !navigationPath.isEmpty else { return }
        appState.showModelSelector = false
        navigationPath.removeLast()
    }

    /// Sidebar projection of the home dashboard used inside
    /// `NavigationSplitView`. Same data + callbacks as `homeDashboard`, but
    /// renders with `.sidebar` chrome (no animated logo, no zoom, no bottom
    /// composer) and exposes an `onNewThread` hook that pushes the hero
    /// composer into the detail pane.
    private var sidebarDashboard: some View {
        HomeDashboardView(
            chrome: .sidebar,
            recentSessions: homeDashboardModel.recentSessions,
            allSessions: homeDashboardModel.allSessions,
            missionControl: homeDashboardModel.missionControl,
            pinnedThreadKeys: homeDashboardModel.pinnedKeys,
            hiddenThreadKeys: homeDashboardModel.hiddenKeys,
            connectedServers: homeDashboardModel.connectedServers,
            projects: homeDashboardModel.projects,
            selectedServerId: homeDashboardModel.selectedServerId,
            selectedProject: homeDashboardModel.selectedProject,
            openingRecentSessionKey: openingRecentSessionKey,
            onOpenRecentSession: openRecentSession,
            onSelectServer: handleSelectServer,
            onAddServer: { appState.showServerPicker = true },
            onOpenProjectPicker: { showProjectPicker = true },
            onShowSessions: { navigationPath.append(.allSessions) },
            onThreadCreated: { key in homeDashboardModel.pinThread(key) },
            onShowSettings: { appState.showSettings = true },
            onShowCommandPalette: {
                _ = actionCenter.perform(.showCommandPalette, source: .toolbar)
            },
            onShowTerminal: terminalLauncher,
            onPinThread: pinThread,
            onUnpinThread: unpinThread,
            onHideThread: hideThread,
            onNewThread: { openNewThread() },
            onHydrateThread: { key, loadInitialTurns in
                await hydrateThread(key, loadInitialTurns: loadInitialTurns)
            },
            onDeleteThread: deleteThread,
            onReconnectServer: reconnectServer,
            onRestartAppServer: restartAppServer,
            onDisconnectServer: disconnectServer,
            onRenameServer: renameServer,
            onOpenRecording: { url in
                navigationPath.append(.replayRecording(url))
            },
            onSendReply: sendQuickReply,
            onCancelThread: cancelThread,
            onForkThread: forkSessionFromHome,
            onInputModeChange: { mode in
                homeInputMode = mode
            },
            requestedInputMode: homeInputMode,
            onSearchThreads: loadSearchThreads
        )
    }

    private var homeDashboard: some View {
        HomeDashboardView(
            recentSessions: homeDashboardModel.recentSessions,
            allSessions: homeDashboardModel.allSessions,
            missionControl: homeDashboardModel.missionControl,
            pinnedThreadKeys: homeDashboardModel.pinnedKeys,
            hiddenThreadKeys: homeDashboardModel.hiddenKeys,
            connectedServers: homeDashboardModel.connectedServers,
            projects: homeDashboardModel.projects,
            selectedServerId: homeDashboardModel.selectedServerId,
            selectedProject: homeDashboardModel.selectedProject,
            openingRecentSessionKey: openingRecentSessionKey,
            onOpenRecentSession: openRecentSession,
            onSelectServer: handleSelectServer,
            onAddServer: { appState.showServerPicker = true },
            onOpenProjectPicker: { showProjectPicker = true },
            onShowSessions: { navigationPath.append(.allSessions) },
            onThreadCreated: { key in homeDashboardModel.pinThread(key) },
            onShowSettings: { appState.showSettings = true },
            onShowCommandPalette: {
                _ = actionCenter.perform(.showCommandPalette, source: .toolbar)
            },
            onShowTerminal: terminalLauncher,
            onPinThread: pinThread,
            onUnpinThread: unpinThread,
            onHideThread: hideThread,
            onHydrateThread: { key, loadInitialTurns in
                await hydrateThread(key, loadInitialTurns: loadInitialTurns)
            },
            onDeleteThread: deleteThread,
            onReconnectServer: reconnectServer,
            onRestartAppServer: restartAppServer,
            onDisconnectServer: disconnectServer,
            onRenameServer: renameServer,
            onOpenRecording: { url in
                navigationPath.append(.replayRecording(url))
            },
            onSendReply: sendQuickReply,
            onCancelThread: cancelThread,
            onForkThread: forkSessionFromHome,
            onInputModeChange: { mode in
                homeInputMode = mode
            },
            requestedInputMode: homeInputMode,
            onSearchThreads: loadSearchThreads
        )
    }

    private func handleSelectServer(_ server: HomeDashboardServer) {
        guard server.canLaunchSessions else {
            reconnectServer(server)
            return
        }
        if homeDashboardModel.selectedServerId == server.id {
            homeDashboardModel.clearScope()
        } else {
            homeDashboardModel.selectedServerId = server.id
        }
    }

    private func pinThread(_ key: ThreadKey) {
        let shouldUnsubscribeDisplacedRecent = homeDashboardModel.pinnedKeys.isEmpty
        let displacedKeys = shouldUnsubscribeDisplacedRecent
            ? Set(homeDashboardModel.recentSessions.map(\.key)).subtracting([key])
            : []
        homeDashboardModel.pinThread(key)
        unsubscribeHomeThreads(Array(displacedKeys))
    }

    private func unpinThread(_ key: ThreadKey) {
        homeDashboardModel.unpinThread(key)
    }

    private func hideThread(_ key: ThreadKey) {
        homeDashboardModel.hideThread(key)
        unsubscribeHomeThreads([key])
    }

    private func unsubscribeHomeThreads(_ keys: [ThreadKey]) {
        let uniqueKeys = Array(Set(keys))
        guard !uniqueKeys.isEmpty else { return }
        Task {
            for key in uniqueKeys {
                do {
                    try await appModel.store.unsubscribeThread(key: key)
                } catch {
                    LLog.warn(
                        "transport",
                        "failed to unsubscribe hidden/displaced home thread",
                        fields: [
                            "serverId": key.serverId,
                            "threadId": key.threadId,
                            "error": String(describing: error)
                        ]
                    )
                }
            }
        }
    }

    private func homeHydrationId(_ key: ThreadKey) -> String {
        "\(key.serverId)/\(key.threadId)"
    }

    private func hydratePinnedThreadsIfNeeded() {
        let connectedServerIds = Set(
            appModel.navigationObservation.servers
                .filter(\.isConnected)
                .map(\.serverId)
        )
        guard !connectedServerIds.isEmpty else { return }

        for pin in homeDashboardModel.pinnedKeys {
            let key = pin.threadKey
            guard connectedServerIds.contains(key.serverId) else { continue }
            let id = homeHydrationId(key)
            if appModel.navigationObservation.sessionTarget(for: key)?.isResumed == true { continue }
            guard !hydratingPinnedHomeThreadIds.contains(id) else { continue }
            hydratingPinnedHomeThreadIds.insert(id)

            Task {
                LLog.info(
                    "home",
                    "hydrating pinned thread",
                    fields: ["serverId": key.serverId, "threadId": key.threadId]
                )
                if !(await hydrateThread(key, loadInitialTurns: true)) {
                    let refreshed = await refreshPinnedThreadListing(serverId: key.serverId)
                    guard refreshed else {
                        await MainActor.run {
                            _ = hydratingPinnedHomeThreadIds.remove(id)
                        }
                        return
                    }
                    _ = await hydrateThread(key, loadInitialTurns: true)
                }
                await MainActor.run {
                    _ = hydratingPinnedHomeThreadIds.remove(id)
                }
            }
        }
    }

    @discardableResult
    private func hydrateThread(_ key: ThreadKey, loadInitialTurns: Bool) async -> Bool {
        // Resume rather than just read: `external_resume_thread` attaches a
        // server-side conversation listener for this connection, so we get
        // live `TurnStarted` / `ItemStarted` / `MessageDelta` /
        // `TurnCompleted` events. Pinned home rows also load the latest turn
        // window so their previews have recent message content.
        //
        // For pinned home rows, resuming preemptively avoids the "first
        // half-second of a stream is missed while we set up a subscription"
        // latency window that an active-only subscription strategy would
        // have. `externalResume` short-circuits to a no-op when the thread's
        // items are already populated, so warm paths are cheap.
        let resumed = (try? await appModel.store.externalResumeThread(key: key, hostId: nil)) != nil
        if resumed, loadInitialTurns {
            await appModel.loadInitialTurnsIfNeeded(threadId: key)
        }
        await appModel.refreshThreadSnapshot(key: key)
        return resumed
    }

    private func refreshPinnedThreadListing(serverId: String) async -> Bool {
        let task = await MainActor.run {
            if let existing = pinnedThreadListingRepairTasks[serverId] {
                return existing
            }

            let task = Task { () -> Bool in
                LLog.info(
                    "home",
                    "repairing pinned thread listing",
                    fields: ["serverId": serverId, "limit": 80]
                )
                do {
                    try await appModel.client.listThreads(
                        serverId: serverId,
                        params: AppListThreadsRequest(
                            cursor: nil,
                            limit: 80,
                            sortKey: .updatedAt,
                            sortDirection: .desc,
                            modelProviders: nil,
                            sourceKinds: [.cli, .vsCode, .appServer],
                            archived: false,
                            cwd: nil,
                            searchTerm: nil,
                            useStateDbOnly: false,
                            runtimeKinds: nil
                        )
                    )
                    return true
                } catch {
                    LLog.warn(
                        "home",
                        "pinned thread listing repair failed",
                        fields: ["serverId": serverId, "error": String(describing: error)]
                    )
                    return false
                }
            }
            pinnedThreadListingRepairTasks[serverId] = task
            return task
        }

        let refreshed = await task.value
        await MainActor.run {
            pinnedThreadListingRepairTasks[serverId] = nil
        }
        return refreshed
    }

    private func deleteThread(_ key: ThreadKey) async {
        _ = try? await appModel.client.archiveThread(
            serverId: key.serverId,
            params: AppArchiveThreadRequest(threadId: key.threadId)
        )
        await appModel.refreshThreadSnapshot(key: key)
    }

    /// Long-press → "Fork" on a home session card. Head-of-thread fork:
    /// duplicates the full thread server-side (no rollback) and navigates
    /// to the new copy. Mirrors `ConversationInfoView.forkConversation`.
    @MainActor
    private func forkSessionFromHome(_ session: HomeDashboardRecentSession) async {
        let threadKey = session.key
        do {
            let sourceKey = await appModel.hydrateThreadPermissions(for: threadKey, appState: appState) ?? threadKey
            let source = appModel.snapshot?.threadSnapshot(for: sourceKey)
            let newKey = try await appModel.client.forkThread(
                serverId: sourceKey.serverId,
                params: AppThreadLaunchConfig(
                    model: source?.model,
                    approvalPolicy: appState.launchApprovalPolicy(for: sourceKey),
                    sandbox: appState.launchSandboxMode(for: sourceKey),
                    developerInstructions: nil,
                    persistExtendedHistory: true
                ).threadForkRequest(threadId: sourceKey.threadId, cwdOverride: source?.info.cwd)
            )
            appModel.store.setActiveThread(key: newKey)
            await appModel.refreshThreadSnapshot(key: newKey)
            openConversation(newKey)
        } catch {
            actionErrorMessage = error.localizedDescription
        }
    }

    @MainActor
    private func cancelThread(_ threadKey: ThreadKey) async {
        // Look up the thread's active turn id — interrupt requires both.
        guard let thread = appModel.snapshot?.threadSnapshot(for: threadKey),
              let turnId = thread.activeTurnId?
                .trimmingCharacters(in: .whitespacesAndNewlines),
              !turnId.isEmpty else {
            return
        }
        do {
            _ = try await appModel.client.interruptTurn(
                serverId: threadKey.serverId,
                params: AppInterruptTurnRequest(
                    threadId: threadKey.threadId,
                    turnId: turnId
                )
            )
            await appModel.refreshThreadSnapshot(key: threadKey)
        } catch {
            actionErrorMessage = error.localizedDescription
        }
    }

    @MainActor
    private func sendQuickReply(_ threadKey: ThreadKey, text: String) async throws {
        let trimmed = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { return }
        let connected = appModel.snapshot?
            .serverSnapshot(for: threadKey.serverId)?
            .isConnected == true
        var activeKey = threadKey
        if connected {
            // Cold-launch snapshots are not necessarily registered with the
            // live upstream session, so the connected path still resumes.
            let resumeKey = await appModel.hydrateThreadPermissions(
                for: threadKey,
                appState: appState
            ) ?? threadKey
            activeKey = try await appModel.resumeThread(
                key: resumeKey,
                launchConfig: launchConfig(for: resumeKey),
                cwdOverride: nil
            )
        }
        let payload = AppComposerPayload(
            text: trimmed,
            additionalInputs: [],
            approvalPolicy: appState.launchApprovalPolicy(for: activeKey),
            sandboxPolicy: appState.turnSandboxPolicy(for: activeKey),
            model: nil,
            effort: nil,
            serviceTier: nil
        )
        _ = try await appModel.submitComposerTurn(key: activeKey, payload: payload)
        if connected {
            await appModel.refreshThreadSnapshot(key: activeKey)
        }
    }

    private func reconnectServer(_ server: HomeDashboardServer) {
        Task {
            await AppRuntimeController.shared.reconnectServer(serverId: server.id)
        }
    }

    private func restartAppServer(_ server: HomeDashboardServer) {
        Task {
            do {
                try await appModel.serverBridge.restartAppServer(serverId: server.id)
                await AppRuntimeController.shared.reconnectServer(serverId: server.id)
                await appModel.refreshSnapshot()
            } catch {
                actionErrorMessage = error.localizedDescription
            }
        }
    }

    private func disconnectServer(_ serverId: String) {
        Task {
            guard let removalLease = await appModel.reconnectController.prepareServerRemoval(
                serverId: serverId
            ) else {
                actionErrorMessage = "Unable to stop reconnecting to this server. Try again."
                return
            }
            await SshSessionStore.shared.close(serverId: serverId, ssh: appModel.ssh)
            do {
                try SavedServerStore.remove(serverId: serverId)
            } catch {
                actionErrorMessage = error.localizedDescription
                guard let storeError = error as? SavedServerStoreError,
                      storeError.removalMayHaveCommitted else {
                    appModel.reconnectController.rollbackServerRemoval(
                        serverId: serverId,
                        lease: removalLease
                    )
                    return
                }
            }
            appModel.reconnectController.syncSavedServers(
                servers: SavedServerStore.reconnectRecords()
            )
            await appModel.refreshSnapshot()
        }
    }

    private func renameServer(_ serverId: String, newName: String) {
        SavedServerStore.rename(serverId: serverId, newName: newName)
        appModel.reconnectController.syncSavedServers(
            servers: SavedServerStore.reconnectRecords()
        )
        appModel.store.renameServer(serverId: serverId, displayName: newName)
    }

    @Sendable
    private func loadSearchThreads(
        query: String,
        runtimeKind: AgentRuntimeKind?,
        serverId selectedServerId: String?,
        forceRepair: Bool
    ) async {
        let trimmedQuery = query.trimmingCharacters(in: .whitespacesAndNewlines)
        let sourceKinds: [AppThreadSourceKind] = [.cli, .vsCode, .appServer]
        let selectedServerFilterId = selectedServerId?.trimmingCharacters(in: .whitespacesAndNewlines)
        await withTaskGroup(of: Void.self) { group in
            for server in homeDashboardModel.connectedServers {
                if let selectedServerFilterId, !selectedServerFilterId.isEmpty, server.id != selectedServerFilterId {
                    continue
                }
                if let runtimeKind,
                   !server.agentRuntimes.contains(where: { $0.available && $0.kind == runtimeKind }) {
                    continue
                }
                let serverId = server.id
                group.addTask {
                    _ = try? await appModel.client.listThreads(
                        serverId: serverId,
                        params: AppListThreadsRequest(
                            cursor: nil,
                            limit: 80,
                            sortKey: .updatedAt,
                            sortDirection: .desc,
                            modelProviders: nil,
                            sourceKinds: sourceKinds,
                            archived: false,
                            cwd: nil,
                            searchTerm: trimmedQuery.isEmpty ? nil : trimmedQuery,
                            useStateDbOnly: !forceRepair,
                            runtimeKinds: runtimeKind.map { [$0] }
                        )
                    )
                }
            }
        }
    }

    private func updateHomeDashboardActivity() {
        if isHomeRouteActive {
            homeDashboardModel.activate()
        } else {
            homeDashboardModel.deactivate()
        }
    }

    private func showSessions(for serverId: String) {
        appState.sessionsSelectedServerFilterId = serverId
        appState.sessionsShowOnlyForks = false
        appState.showModelSelector = false
        hasSeededInitialConversationRoute = true

        if let existingIndex = navigationPath.lastIndex(where: { route in
            guard case let .sessions(id, _) = route else { return false }
            return id == serverId
        }) {
            navigationPath = Array(navigationPath.prefix(through: existingIndex))
            return
        }

        if case .conversation = navigationPath.last {
            navigationPath.removeLast()
        } else if case .realtimeVoice = navigationPath.last {
            navigationPath.removeLast()
        }
        navigationPath.append(.sessions(serverId: serverId, title: serverTitle(for: serverId)))
    }

    private func serverTitle(for serverId: String) -> String {
        if let server = homeDashboardModel.connectedServers.first(where: { $0.id == serverId }) {
            return server.displayName
        }
        if let thread = homeDashboardModel.recentSessions.first(where: { $0.serverId == serverId }) {
            return thread.serverDisplayName
        }
        return "Sessions"
    }
}

private struct RemoraLinkServerInfoDestination: View {
    private struct RefreshKey: Hashable {
        let serverId: String
        let snapshotRevision: UInt64
        let remoraLinkAvailable: Bool
        let terminalEnabled: Bool
    }

    let appModel: AppModel
    let serverId: String
    let onOpenWallpaper: () -> Void
    let onOpenShell: (() -> Void)?

    @State private var eligibility = RemoraLinkTerminalEligibilityState()

    private var refreshKey: RefreshKey {
        RefreshKey(
            serverId: serverId,
            snapshotRevision: appModel.snapshotRevision,
            remoraLinkAvailable: AppRuntimeController.shared.remoraLinkStatus == .available,
            terminalEnabled: onOpenShell != nil
        )
    }

    var body: some View {
        ConversationInfoView(
            threadKey: nil,
            serverId: serverId,
            onOpenWallpaper: onOpenWallpaper,
            onOpenShell: eligibility.canOpenShell ? onOpenShell : nil
        )
        .onChange(of: refreshKey, initial: true) { _, key in
            beginEligibilityRefresh(for: key)
        }
        .onDisappear {
            eligibility.invalidate()
        }
    }

    private func beginEligibilityRefresh(for key: RefreshKey) {
        let generation = eligibility.beginRequest()
        guard key.remoraLinkAvailable, key.terminalEnabled else { return }
        Task {
            guard let hosts = try? await appModel.client.remoraLinkHosts() else { return }
            guard !Task.isCancelled, refreshKey == key else { return }
            eligibility.apply(hosts: hosts, serverId: key.serverId, generation: generation)
        }
    }
}
