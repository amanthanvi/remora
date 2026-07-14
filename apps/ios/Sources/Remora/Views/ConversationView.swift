import SwiftUI
import PhotosUI
import UIKit
import os
import HairballUI

struct ConversationView: View {
    @Environment(AppState.self) private var appState
    @Environment(AppModel.self) private var appModel
    let thread: AppThreadSnapshot
    let activeThreadKey: ThreadKey
    let transcript: ConversationTranscriptSnapshot
    let followScrollToken: Int
    let pinnedContextItems: [ConversationItem]
    let composer: ConversationComposerSnapshot
    @Binding var composerInputText: String
    @Binding var composerAttachedImage: UIImage?
    var topInset: CGFloat = 0
    var bottomInset: CGFloat = 0
    var onOpenConversation: ((ThreadKey) -> Void)? = nil
    var onResumeSessions: ((String) -> Void)? = nil
    var minigameOverlay: MinigameOverlayState = .idle
    var onTypingTap: (() -> Void)? = nil
    var onMinigameDismiss: (() -> Void)? = nil
    var onMinigameRetry: (() -> Void)? = nil
    @AppStorage("workDir") private var workDir = FileManager.default.urls(for: .documentDirectory, in: .userDomainMask).first?.path ?? "/"
    @AppStorage("conversationTextSizeStep") private var conversationTextSizeStep = ConversationTextSize.large.rawValue
    @AppStorage("fastMode") private var fastMode = false
    @State private var messageActionError: String?
    @State private var hasLoggedFirstRender = false
    @State private var localSendScrollToken = 0

    private var items: [ConversationItem] {
        transcript.items
    }

    private var threadStatus: ConversationStatus {
        transcript.threadStatus
    }

    private var agentDirectoryVersion: UInt64 {
        transcript.agentDirectoryVersion
    }

    private var pendingModelOverride: String? {
        let trimmed = appState.selectedModel.trimmingCharacters(in: .whitespacesAndNewlines)
        return trimmed.isEmpty ? nil : trimmed
    }

    private var pendingAgentRuntimeKindOverride: AgentRuntimeKind? {
        pendingModelOverride == nil ? nil : appState.selectedAgentRuntimeKind
    }

    private var pendingReasoningOverride: String? {
        if thread.ampReasoningEffortLocked {
            return nil
        }
        let trimmed = appState.reasoningEffort.trimmingCharacters(in: .whitespacesAndNewlines)
        return trimmed.isEmpty ? nil : trimmed
    }

    private var supportsTurnPagination: Bool {
        appModel.snapshot?
            .serverSnapshot(for: activeThreadKey.serverId)?
            .capabilities
            .supportsTurnPagination ?? false
    }

    var body: some View {
        ConversationMessageList(
            items: items,
            threadStatus: threadStatus,
            threadHasServerData: thread.hasPreviewOrTitle,
            transcriptRenderDigest: transcript.renderDigest,
            followScrollToken: followScrollToken,
            sendScrollToken: localSendScrollToken,
            activeThreadKey: activeThreadKey,
            agentDirectoryVersion: agentDirectoryVersion,
            topInset: thread.isSubagent ? topInset + 32 : topInset,
            olderTurnsCursor: thread.olderTurnsCursor,
            initialTurnsLoaded: thread.initialTurnsLoaded || !supportsTurnPagination,
            textSizeStep: $conversationTextSizeStep,
            resolveTargetLabel: resolveTargetLabel,
            onWidgetPrompt: sendWidgetPrompt,
            onEditUserItem: editMessage,
            onForkFromUserItem: forkFromMessage,
            onOpenConversation: onOpenConversation,
            onLoadOlderTurns: { key in
                Task { await appModel.loadOlderTurns(threadId: key) }
            }
        )
        .overlay(alignment: .bottomLeading) {
            if let onTypingTap,
               minigameOverlay == .idle,
               ExperimentalFeatures.shared.isEnabled(.thinkingMinigame) {
                MinigameLaunchButton(action: onTypingTap)
                    .padding(.leading, 12)
                    .padding(.bottom, 8)
                    .transition(.scale.combined(with: .opacity))
            }
        }
        .activeThreadKey(activeThreadKey)
        .background { ChatWallpaperBackground(threadKey: activeThreadKey) }
        .overlay(alignment: .top) {
            if thread.isSubagent {
                SubagentBreadcrumbBar(
                    thread: thread,
                    topInset: topInset,
                    onNavigateToParent: {
                        if let parentId = thread.info.parentThreadId {
                            onOpenConversation?(ThreadKey(serverId: thread.serverId, threadId: parentId))
                        }
                    }
                )
            }
        }
        .overlay(alignment: .topLeading) {
            if DebugSettings.shared.enabled {
                ConversationDebugButton(topInset: topInset, activeThreadKey: activeThreadKey)
            }
        }
        .safeAreaInset(edge: .bottom, spacing: 0) {
            if minigameOverlay == .idle {
                ConversationBottomChrome(
                    pinnedContextItems: pinnedContextItems,
                    composer: composer,
                    composerInputText: $composerInputText,
                    composerAttachedImage: $composerAttachedImage,
                    onSend: sendMessage,
                    onFileSearch: searchComposerFiles,
                    bottomInset: bottomInset,
                    onOpenConversation: onOpenConversation,
                    onResumeSessions: onResumeSessions
                )
            } else {
                MinigameOverlayView(
                    state: minigameOverlay,
                    onClose: { onMinigameDismiss?() },
                    onRetry: { onMinigameRetry?() }
                )
                .frame(height: UIScreen.main.bounds.height * 0.4)
                .padding(.horizontal, 8)
                .padding(.bottom, max(bottomInset, 8))
                .transition(.move(edge: .bottom).combined(with: .opacity))
            }
        }
        .alert("Conversation Action Error", isPresented: Binding(
            get: { messageActionError != nil },
            set: { if !$0 { messageActionError = nil } }
        )) {
            Button("OK", role: .cancel) { messageActionError = nil }
        } message: {
            Text(messageActionError ?? "Unknown error")
        }
        .onAppear {
            guard !hasLoggedFirstRender else { return }
            hasLoggedFirstRender = true
            os_signpost(.event, log: conversationViewSignpostLog, name: "ConversationFirstRender")
            appState.hydratePermissions(from: thread)
        }
        .onChange(of: thread) { _, newThread in
            appState.hydratePermissions(from: newThread)
        }
        .task(id: activeThreadKey) {
            await loadInitialTurnsIfNeeded()
        }
        .onChange(of: thread.initialTurnsLoaded) { _, _ in
            Task { await loadInitialTurnsIfNeeded() }
        }
    }

    private func loadInitialTurnsIfNeeded() async {
        guard !thread.initialTurnsLoaded else { return }
        await appModel.loadInitialTurnsIfNeeded(threadId: activeThreadKey)
    }

    private func sendMessage(
        _ text: String,
        attachmentImage: UIImage?,
        fileAttachments: [ComposerFileAttachment],
        skillMentions: [SkillMentionSelection],
        pluginMentions: [PluginMentionSelection]
    ) {
        localSendScrollToken &+= 1
        Task {
            do {
                NSLog(
                    "[ConversationView] sendMessage start server=%@ thread=%@ textLength=%ld",
                    activeThreadKey.serverId,
                    activeThreadKey.threadId,
                    text.count
                )
                let payload = try makeComposerPayload(
                    text: text,
                    attachmentImage: attachmentImage,
                    fileAttachments: fileAttachments,
                    skillMentions: skillMentions,
                    pluginMentions: pluginMentions
                )
                try await appModel.startTurn(key: activeThreadKey, payload: payload)
                NSLog(
                    "[ConversationView] sendMessage turnStart returned server=%@ thread=%@",
                    activeThreadKey.serverId,
                    activeThreadKey.threadId
                )
            } catch {
                NSLog(
                    "[ConversationView] sendMessage error server=%@ thread=%@ error=%@",
                    activeThreadKey.serverId,
                    activeThreadKey.threadId,
                    error.localizedDescription
                )
                messageActionError = error.localizedDescription
            }
        }
    }

    private func sendWidgetPrompt(_ text: String) {
        guard !text.isEmpty else { return }
        localSendScrollToken &+= 1
        Task {
            do {
                let payload = try makeComposerPayload(
                    text: text,
                    attachmentImage: nil,
                    fileAttachments: [],
                    skillMentions: [],
                    pluginMentions: []
                )
                try await appModel.startTurn(key: activeThreadKey, payload: payload)
            } catch {
                messageActionError = error.localizedDescription
            }
        }
    }

    private func resolveTargetLabel(_ target: String) -> String? {
        appModel.snapshot?.resolvedAgentTargetLabel(for: target, serverId: activeThreadKey.serverId)
    }

    /// Resolve the user-message position in the currently-loaded transcript.
    /// `forkThreadFromMessage` / `editMessage` on the Rust side expect an
    /// index into `thread.items` filtered to user messages — see
    /// `rollback_depth_for_turn` in `mobile_client/thread_projection.rs`.
    /// Recomputing from the live `items` keeps the index correct under
    /// pagination (older turns can shift positions; cached `sourceTurnIndex`
    /// from a prior hydrate would be stale).
    private func loadedUserItemIndex(for item: ConversationItem) -> Int? {
        var idx = 0
        for candidate in items {
            guard candidate.isUserItem else { continue }
            if candidate.id == item.id { return idx }
            idx += 1
        }
        return nil
    }

    private func editMessage(_ item: ConversationItem) {
        Task {
            do {
                guard item.isUserItem, item.isFromUserTurnBoundary,
                      let selectedTurnIndex = loadedUserItemIndex(for: item) else {
                    throw NSError(
                        domain: "Remora",
                        code: 1020,
                        userInfo: [NSLocalizedDescriptionKey: "Only user messages can be edited"]
                    )
                }
                let result = try await appModel.store.editMessage(
                    key: activeThreadKey,
                    selectedTurnIndex: UInt32(selectedTurnIndex)
                )
                appModel.queueComposerPrefill(threadKey: activeThreadKey, text: result)
            } catch {
                messageActionError = error.localizedDescription
            }
        }
    }

    private func forkFromMessage(_ item: ConversationItem) {
        Task {
            do {
                guard item.isUserItem, item.isFromUserTurnBoundary,
                      let selectedTurnIndex = loadedUserItemIndex(for: item) else {
                    throw NSError(
                        domain: "Remora",
                        code: 1016,
                        userInfo: [NSLocalizedDescriptionKey: "Fork from here is only supported for user messages"]
                    )
                }
                let nextKey = try await appModel.store.forkThreadFromMessage(
                    key: activeThreadKey,
                    selectedTurnIndex: UInt32(selectedTurnIndex),
                    params: launchConfig().forkThreadFromMessageRequest(
                        cwdOverride: thread.info.cwd
                    )
                )
                await appModel.refreshThreadSnapshot(key: nextKey)
                let nextCwd = thread.info.cwd?.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
                if !nextCwd.isEmpty {
                    workDir = nextCwd
                    appState.currentCwd = nextCwd
                }
                onOpenConversation?(nextKey)
            } catch {
                messageActionError = error.localizedDescription
            }
        }
    }

    private func searchComposerFiles(_ query: String) async throws -> [FileSearchResult] {
        let searchRoot = workDir.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty ? "/" : workDir
        return try await appModel.client.searchFiles(
            serverId: activeThreadKey.serverId,
            params: AppSearchFilesRequest(
                query: query,
                roots: [searchRoot],
                cancellationToken: "ios-composer-file-search"
            )
        )
    }

    private func makeComposerPayload(
        text: String,
        attachmentImage: UIImage?,
        fileAttachments: [ComposerFileAttachment],
        skillMentions: [SkillMentionSelection],
        pluginMentions: [PluginMentionSelection]
    ) throws -> AppComposerPayload {
        let preparedAttachment = attachmentImage.flatMap(ConversationAttachmentSupport.prepareImage)
        var additionalInputs = skillMentions.map { mention in
            AppUserInput.skill(name: mention.name, path: AbsolutePath(value: mention.path))
        }
        for mention in pluginMentions {
            additionalInputs.append(
                AppUserInput.mention(name: mention.name, path: mention.path)
            )
        }
        if let preparedAttachment {
            additionalInputs.append(preparedAttachment.userInput)
        }
        return AppComposerPayload(
            text: text,
            additionalInputs: additionalInputs,
            fileAttachments: fileAttachments,
            approvalPolicy: appState.launchApprovalPolicy(for: activeThreadKey),
            sandboxPolicy: appState.turnSandboxPolicy(for: activeThreadKey),
            model: pendingModelOverride,
            effort: ReasoningEffort(wireValue: pendingReasoningOverride),
            serviceTier: ServiceTier(wireValue: fastMode ? "fast" : nil)
        )
    }

    private func launchConfig() -> AppThreadLaunchConfig {
        AppThreadLaunchConfig(
            agentRuntimeKind: pendingAgentRuntimeKindOverride,
            model: pendingModelOverride,
            approvalPolicy: appState.launchApprovalPolicy(for: activeThreadKey),
            sandbox: appState.launchSandboxMode(for: activeThreadKey),
            developerInstructions: nil,
            persistExtendedHistory: true
        )
    }
}

extension AppThreadSnapshot {
    var serverId: String { key.serverId }
    var isSubagent: Bool {
        info.parentThreadId?.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty == false
            && ((info.agentNickname?.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty == false)
                || (info.agentRole?.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty == false))
    }

    var agentDisplayLabel: String? {
        let nickname = info.agentNickname?.trimmingCharacters(in: .whitespacesAndNewlines)
        let role = info.agentRole?.trimmingCharacters(in: .whitespacesAndNewlines)
        if let nickname, !nickname.isEmpty { return nickname }
        if let role, !role.isEmpty { return role }
        return nil
    }
}

#if DEBUG
#Preview("Conversation") {
    RemoraPreviewScene(appModel: RemoraPreviewData.makeConversationAppModel(messages: RemoraPreviewData.longConversation)) {
        ContentView()
    }
}
#endif
