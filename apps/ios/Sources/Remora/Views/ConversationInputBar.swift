import SwiftUI
import PhotosUI
import UIKit
import os

struct ConversationInputBar: View {
    @Environment(AppState.self) private var appState
    @Environment(AppModel.self) private var appModel
    @State private var actionCenter = RemoraActionCenter.shared
    @State private var composerActionOwner = UUID()
    let snapshot: ConversationComposerSnapshot
    @AppStorage("workDir") private var workDir = FileManager.default.urls(for: .documentDirectory, in: .userDomainMask).first?.path ?? "/"
    @AppStorage("fastMode") private var fastMode = false

    let onSend: (
        String,
        UIImage?,
        [ComposerFileAttachment],
        [SkillMentionSelection],
        [PluginMentionSelection],
        @escaping (Bool) -> Void
    ) -> Void
    let onFileSearch: (String) async throws -> [FileSearchResult]
    var bottomInset: CGFloat = 0
    let showModeChip: Bool
    let onOpenModePicker: () -> Void
    let onOpenConversation: ((ThreadKey) -> Void)?
    let onResumeSessions: ((String) -> Void)?

    @Binding var inputText: String
    @Binding var attachedImage: UIImage?
    @State private var attachedFiles: [ComposerFileAttachment] = []
    @State private var showAttachMenu = false
    @State private var showPhotoPicker = false
    @State private var showCamera = false
    @State private var showFileImporter = false
    @State private var selectedPhoto: PhotosPickerItem?
    @State private var showSlashPopup = false
    @State private var activeSlashToken: ComposerSlashQueryContext?
    @State private var slashSuggestions: [ComposerSlashCommand] = []
    @State private var showFilePopup = false
    @State private var activeAtToken: ComposerTokenContext?
    @State private var showSkillPopup = false
    @State private var activeDollarToken: ComposerTokenContext?
    @State private var fileSearchLoading = false
    @State private var fileSearchError: String?
    @State private var fileSuggestions: [FileSearchResult] = []
    @State private var fileSearchGeneration = 0
    @State private var fileSearchTask: Task<Void, Never>?
    @State private var popupRefreshTask: Task<Void, Never>?
    @State private var showModelSelector = false
    @State private var showPermissionsSheet = false
    @State private var showExperimentalSheet = false
    @State private var showSkillsSheet = false
    @State private var showRenamePrompt = false
    @State private var renameCurrentThreadTitle = ""
    @State private var renameDraft = ""
    @State private var slashErrorMessage: String?
    @State private var experimentalFeatures: [ExperimentalFeature] = []
    @State private var experimentalFeaturesLoading = false
    @State private var skills: [SkillMetadata] = []
    @State private var skillsLoading = false
    @State private var mentionSkillPathsByName: [String: String] = [:]
    @State private var hasAttemptedSkillMentionLoad = false
    @State private var pluginCacheByCwd: [String: [PluginSummary]] = [:]
    @State private var pluginUnsupportedCwds: Set<String> = []
    @State private var pluginLoadingCwds: Set<String> = []
    @State private var pluginMentionSelections: [PluginMentionSelection] = []
    @State private var voiceManager = VoiceTranscriptionManager()
    @State private var showMicPermissionAlert = false
    @State private var hasLoggedFirstFocus = false
    @State private var hasLoggedKeyboardShown = false
    @State private var isComposerFocused = false
    @State private var composerSelectionRange = NSRange(location: 0, length: 0)

    private var pendingUserInputRequest: PendingUserInputRequest? {
        guard let request = snapshot.pendingUserInputRequest else { return nil }
        return appState.isPendingUserInputDismissed(id: request.id) ? nil : request
    }

    private var pendingModelOverride: String? {
        let trimmed = appState.selectedModel.trimmingCharacters(in: .whitespacesAndNewlines)
        return trimmed.isEmpty ? nil : trimmed
    }

    private var pendingAgentRuntimeKindOverride: AgentRuntimeKind? {
        pendingModelOverride == nil ? nil : appState.selectedAgentRuntimeKind
    }

    private var isTurnActive: Bool {
        snapshot.isTurnActive
    }

    private var activeTurnId: String? {
        guard let value = snapshot.activeTurnId?.trimmingCharacters(in: .whitespacesAndNewlines),
              !value.isEmpty else {
            return nil
        }
        return value
    }

    private var popupState: ConversationComposerPopupState {
        if showSlashPopup {
            return .slash(slashSuggestions)
        }
        if showFilePopup {
            return .file(
                loading: fileSearchLoading,
                error: fileSearchError,
                suggestions: fileSuggestions,
                plugins: pluginSuggestions
            )
        }
        if showSkillPopup {
            return .skill(loading: skillsLoading, suggestions: skillSuggestions)
        }
        return .none
    }

    var body: some View {
        ConversationComposerModalCoordinator(
            snapshot: snapshot,
            experimentalFeatures: experimentalFeatures,
            experimentalFeaturesLoading: experimentalFeaturesLoading,
            skills: skills,
            skillsLoading: skillsLoading,
            showAttachMenu: $showAttachMenu,
            showPhotoPicker: $showPhotoPicker,
            showCamera: $showCamera,
            showFileImporter: $showFileImporter,
            selectedPhoto: $selectedPhoto,
            attachedImage: $attachedImage,
            showModelSelector: $showModelSelector,
            showPermissionsSheet: $showPermissionsSheet,
            showExperimentalSheet: $showExperimentalSheet,
            showSkillsSheet: $showSkillsSheet,
            showRenamePrompt: $showRenamePrompt,
            renameCurrentThreadTitle: $renameCurrentThreadTitle,
            renameDraft: $renameDraft,
            slashErrorMessage: $slashErrorMessage,
            showMicPermissionAlert: $showMicPermissionAlert,
            onOpenSettings: openAppSettings,
            onLoadSelectedPhoto: loadSelectedPhoto,
            onLoadSelectedFile: { url in
                guard let picked = ConversationAttachmentSupport.loadPickedFile(at: url) else { return }
                applyPickedFile(picked)
            },
            onLoadExperimentalFeatures: loadExperimentalFeatures,
            onIsExperimentalFeatureEnabled: { featureId, fallback in
                isExperimentalFeatureEnabled(featureId, fallback: fallback)
            },
            onSetExperimentalFeature: { featureName, enabled in
                await setExperimentalFeature(named: featureName, enabled: enabled)
            },
            onLoadSkills: { forceReload, showErrors in
                await loadSkills(forceReload: forceReload, showErrors: showErrors)
            },
            onRenameThread: renameThread
        ) {
            composerSurface
        }
        .onChange(of: inputText) { _, next in
            scheduleComposerPopupRefresh(for: next)
        }
        .onChange(of: snapshot.composerPrefillRequest?.id) { _, _ in
            guard let prefill = snapshot.composerPrefillRequest else { return }
            inputText = prefill.text
            composerSelectionRange = NSRange(location: (prefill.text as NSString).length, length: 0)
            attachedImage = nil
            attachedFiles = []
            hideComposerPopups()
            appModel.clearComposerPrefill(id: prefill.id)
        }
        .onChange(of: isComposerFocused) { _, focused in
            actionCenter.setComposerFocused(focused, owner: composerActionOwner)
            if focused {
                guard !hasLoggedFirstFocus else { return }
                hasLoggedFirstFocus = true
                os_signpost(.event, log: conversationViewSignpostLog, name: "ComposerFirstFocus")
            }
        }
        .onReceive(NotificationCenter.default.publisher(for: UIResponder.keyboardDidShowNotification)) { _ in
            guard !hasLoggedKeyboardShown else { return }
            hasLoggedKeyboardShown = true
            os_signpost(.event, log: conversationViewSignpostLog, name: "KeyboardShown")
        }
        .onReceive(NotificationCenter.default.publisher(for: .remoraActionRequested)) { notification in
            guard let request = notification.object as? RemoraActionRequest,
                  request.id == .sendMessage else { return }
            // Multiple conversation views may stay alive during navigation or
            // warmup. Only the composer registered as the current focus owner
            // consumes the shared request; inactive composers must not race to
            // complete it with an error.
            guard actionCenter.composerOwnsFocus(composerActionOwner) else { return }
            guard isComposerFocused else {
                request.finish(errorMessage: "Focus this composer before sending.")
                return
            }
            let text = inputText.trimmingCharacters(in: .whitespacesAndNewlines)
            guard !text.isEmpty || attachedImage != nil || !attachedFiles.isEmpty else {
                request.finish(errorMessage: "Enter a message or attach a file before sending.")
                return
            }
            handleSend()
            request.finish()
        }
        .onDisappear {
            actionCenter.setComposerFocused(false, owner: composerActionOwner)
            if voiceManager.isRecording { voiceManager.cancelRecording() }
            popupRefreshTask?.cancel()
            popupRefreshTask = nil
            fileSearchTask?.cancel()
            fileSearchTask = nil
        }
    }

    private var composerSurface: some View {
        VStack(spacing: 0) {
            ConversationComposerContentView(
                attachedImage: attachedImage,
                attachedFiles: attachedFiles,
                collaborationMode: snapshot.collaborationMode,
                activePlanProgress: snapshot.activePlanProgress,
                pendingUserInputRequest: pendingUserInputRequest,
                hasPendingPlanImplementation: snapshot.pendingPlanImplementationPrompt != nil,
                activeTaskSummary: snapshot.activeTaskSummary,
                queuedFollowUps: snapshot.queuedFollowUps,
                pluginMentions: pluginMentionSelections,
                goal: snapshot.goal,
                goalActions: makeGoalCardActions(),
                rateLimits: snapshot.rateLimits,
                contextPercent: contextPercent(),
                isTurnActive: isTurnActive,
                showModeChip: showModeChip,
                voiceManager: voiceManager,
                showAttachMenu: $showAttachMenu,
                onClearAttachment: clearAttachment,
                onRemoveFileAttachment: removeFileAttachment,
                onRespondToPendingUserInput: respondToPendingUserInput,
                onDismissPendingUserInput: dismissPendingUserInput,
                onImplementPlan: { Task { await implementPlan() } },
                onDismissPlanImplementation: dismissPlanImplementationPrompt,
                onSteerQueuedFollowUp: steerQueuedFollowUp,
                onDeleteQueuedFollowUp: deleteQueuedFollowUp,
                onRemovePluginMention: removePluginMention,
                onPasteImage: { image in attachedImage = image },
                onOpenModePicker: onOpenModePicker,
                onSendText: handleSend,
                onStopRecording: stopVoiceRecording,
                onStartRecording: startVoiceRecording,
                onInterrupt: interruptActiveTurn,
                inputText: $inputText,
                isComposerFocused: $isComposerFocused,
                composerSelectionRange: $composerSelectionRange
            )
            .overlay(alignment: .bottom) {
                ConversationComposerPopupOverlayView(
                    state: popupState,
                    onApplySlashSuggestion: applySlashSuggestion,
                    onApplyFileSuggestion: applyFileSuggestion,
                    onApplySkillSuggestion: applySkillSuggestion,
                    onApplyPluginSuggestion: applyPluginSuggestion
                )
            }
        }
        .dropDestination(for: URL.self) { urls, _ in
            guard let picked = urls.lazy.compactMap({ ConversationAttachmentSupport.loadPickedFile(at: $0) }).first else {
                return false
            }
            applyPickedFile(picked)
            return true
        }
        .dropDestination(for: Data.self) { items, _ in
            guard let image = items.lazy.compactMap({ UIImage(data: $0) }).first else {
                return false
            }
            attachedImage = image
            return true
        }
    }

    private func contextPercent() -> Int64? {
        guard let contextWindow = snapshot.modelContextWindow else { return nil }
        let baseline: Int64 = 12_000
        guard contextWindow > baseline else { return 0 }
        let totalTokens = snapshot.contextTokensUsed ?? baseline
        let effectiveWindow = contextWindow - baseline
        let usedTokens = max(0, totalTokens - baseline)
        let remainingTokens = max(0, effectiveWindow - usedTokens)
        let percent = Int64((Double(remainingTokens) / Double(effectiveWindow) * 100).rounded())
        return min(max(percent, 0), 100)
    }

    private func clearAttachment() {
        attachedImage = nil
    }

    private func removeFileAttachment(_ file: ComposerFileAttachment) {
        attachedFiles.removeAll { $0 == file }
    }

    private func applyPickedFile(_ picked: PickedComposerFile) {
        switch picked {
        case .image(let image):
            attachedImage = image
        case .file(let file):
            if !attachedFiles.contains(file) {
                attachedFiles.append(file)
            }
        }
    }

    private func respondToPendingUserInput(_ answers: [String: [String]]) {
        guard let pendingUserInputRequest else { return }
        let payload: [PendingUserInputAnswer] = pendingUserInputRequest.questions.compactMap { question in
            guard let selectedAnswers = answers[question.id], !selectedAnswers.isEmpty else { return nil }
            return PendingUserInputAnswer(questionId: question.id, answers: selectedAnswers)
        }
        Task {
            do {
                try await appModel.store.respondToUserInput(
                    requestId: pendingUserInputRequest.id,
                    answers: payload
                )
            } catch {
                slashErrorMessage = error.localizedDescription
            }
        }
    }

    private func steerQueuedFollowUp(_ preview: AppQueuedFollowUpPreview) {
        Task {
            do {
                try await appModel.store.steerQueuedFollowUp(
                    key: snapshot.threadKey,
                    previewId: preview.id
                )
            } catch {
                slashErrorMessage = error.localizedDescription
            }
        }
    }

    private func deleteQueuedFollowUp(_ preview: AppQueuedFollowUpPreview) {
        Task {
            do {
                try await appModel.store.deleteQueuedFollowUp(
                    key: snapshot.threadKey,
                    previewId: preview.id
                )
            } catch {
                slashErrorMessage = error.localizedDescription
            }
        }
    }

    private func handleSend() {
        let text = inputText.trimmingCharacters(in: .whitespacesAndNewlines)
        let image = attachedImage
        let files = attachedFiles
        guard !text.isEmpty || image != nil || !files.isEmpty else { return }
        if let request = snapshot.pendingUserInputRequest {
            appState.dismissPendingUserInput(id: request.id)
        }
        if image == nil,
           files.isEmpty,
           let invocation = parseSlashCommandInvocation(text) {
            inputText = ""
            attachedImage = nil
            attachedFiles = []
            hideComposerPopups()
            isComposerFocused = false
            executeSlashCommand(invocation.command, args: invocation.args)
            return
        }
        inputText = ""
        attachedImage = nil
        attachedFiles = []
        hideComposerPopups()
        isComposerFocused = false
        let skillMentions = collectSkillMentionsForSubmission(text)
        let pluginMentions = collectPluginMentionsForSubmission(text)
        pluginMentionSelections = []
        onSend(text, image, files, skillMentions, pluginMentions) { succeeded in
            guard !succeeded else { return }
            if inputText.isEmpty {
                inputText = text
            }
            if attachedImage == nil {
                attachedImage = image
            }
            if attachedFiles.isEmpty {
                attachedFiles = files
            }
            if pluginMentionSelections.isEmpty {
                pluginMentionSelections = pluginMentions
            }
        }
    }

    private func dismissPendingUserInput() {
        guard let request = snapshot.pendingUserInputRequest else { return }
        appState.dismissPendingUserInput(id: request.id)
    }

    private func collectPluginMentionsForSubmission(_ text: String) -> [PluginMentionSelection] {
        guard !pluginMentionSelections.isEmpty else { return [] }
        let lowered = text.lowercased()
        var seen = Set<String>()
        var resolved: [PluginMentionSelection] = []
        for selection in pluginMentionSelections {
            // Drop selections the user has since deleted from the input text.
            guard lowered.contains("@\(selection.name.lowercased())") else { continue }
            guard seen.insert(selection.path).inserted else { continue }
            resolved.append(selection)
        }
        return resolved
    }

    private func startVoiceRecording() {
        Task {
            let granted = await voiceManager.requestMicPermission()
            guard granted else {
                showMicPermissionAlert = true
                return
            }
            voiceManager.startRecording()
        }
    }

    private func stopVoiceRecording() {
        Task {
            let auth = try? await appModel.client.authStatus(
                serverId: snapshot.threadKey.serverId,
                params: AuthStatusRequest(includeToken: true, refreshToken: false)
            )
            if let text = await voiceManager.stopAndTranscribe(
                authMethod: auth?.authMethod,
                authToken: auth?.authToken
            ), !text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
                insertTranscriptAtCursor(text)
                DispatchQueue.main.async {
                    isComposerFocused = true
                }
            }
        }
    }

    private func insertTranscriptAtCursor(_ transcript: String) {
        let insertion = transcript.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !insertion.isEmpty else { return }

        let nsText = inputText as NSString
        let textLength = nsText.length
        let location = min(max(composerSelectionRange.location, 0), textLength)
        let length = min(max(composerSelectionRange.length, 0), textLength - location)
        let range = NSRange(location: location, length: length)
        let replacement = composerInsertionText(insertion, in: nsText, replacing: range)
        let updated = nsText.replacingCharacters(in: range, with: replacement)
        inputText = updated
        let cursor = (updated as NSString).length - ((nsText.length - range.location - range.length))
        composerSelectionRange = NSRange(location: cursor, length: 0)
    }

    private func interruptActiveTurn() {
        guard let activeTurnId else {
            LLog.warn("conversation", "interrupt requested but no activeTurnId")
            return
        }
        let threadKey = snapshot.threadKey
        LLog.info(
            "conversation",
            "interrupt turn",
            fields: ["serverId": threadKey.serverId, "threadId": threadKey.threadId, "turnId": activeTurnId]
        )
        Task {
            do {
                _ = try await appModel.client.interruptTurn(
                    serverId: threadKey.serverId,
                    params: AppInterruptTurnRequest(
                        threadId: threadKey.threadId,
                        turnId: activeTurnId
                    )
                )
                LLog.info("conversation", "interrupt turn rpc ok")
            } catch {
                LLog.warn("conversation", "interrupt turn failed", fields: ["error": String(describing: error)])
                slashErrorMessage = error.localizedDescription
            }
        }
    }

    private func openAppSettings() {
        guard let url = URL(string: UIApplication.openSettingsURLString) else { return }
        UIApplication.shared.open(url)
    }

    private func loadSelectedPhoto(_ item: PhotosPickerItem) async {
        if let data = try? await item.loadTransferable(type: Data.self),
           let image = UIImage(data: data) {
            attachedImage = image
        }
        selectedPhoto = nil
    }

    private func dismissPlanImplementationPrompt() {
        appModel.store.dismissPlanImplementationPrompt(key: snapshot.threadKey)
    }

    private func implementPlan() async {
        do {
            try await appModel.store.implementPlan(key: snapshot.threadKey)
        } catch {
            slashErrorMessage = error.localizedDescription
        }
    }


    private func clearFileSearchState(incrementGeneration: Bool = true) {
        let hadTask = fileSearchTask != nil
        fileSearchTask?.cancel()
        fileSearchTask = nil
        if incrementGeneration && (hadTask || fileSearchLoading || fileSearchError != nil || !fileSuggestions.isEmpty) {
            fileSearchGeneration += 1
        }
        if fileSearchLoading {
            fileSearchLoading = false
        }
        if fileSearchError != nil {
            fileSearchError = nil
        }
        if !fileSuggestions.isEmpty {
            fileSuggestions = []
        }
    }

    private func hideComposerPopups() {
        popupRefreshTask?.cancel()
        popupRefreshTask = nil
        if showSlashPopup {
            showSlashPopup = false
        }
        if activeSlashToken != nil {
            activeSlashToken = nil
        }
        if !slashSuggestions.isEmpty {
            slashSuggestions = []
        }
        if showFilePopup {
            showFilePopup = false
        }
        if activeAtToken != nil {
            activeAtToken = nil
        }
        if showSkillPopup {
            showSkillPopup = false
        }
        if activeDollarToken != nil {
            activeDollarToken = nil
        }
        clearFileSearchState()
    }

    private func startFileSearch(_ query: String) {
        fileSearchTask?.cancel()
        fileSearchTask = nil
        let requestId = fileSearchGeneration + 1
        fileSearchGeneration = requestId
        if !fileSearchLoading {
            fileSearchLoading = true
        }
        if fileSearchError != nil {
            fileSearchError = nil
        }
        if !fileSuggestions.isEmpty {
            fileSuggestions = []
        }

        fileSearchTask = Task { @MainActor in
            try? await Task.sleep(nanoseconds: 140_000_000)
            guard !Task.isCancelled else { return }
            guard activeAtToken?.value == query else { return }

            do {
                let matches = try await onFileSearch(query)
                guard !Task.isCancelled else { return }
                guard requestId == fileSearchGeneration, activeAtToken?.value == query else { return }
                fileSuggestions = matches
                fileSearchLoading = false
                fileSearchError = nil
            } catch {
                guard !Task.isCancelled else { return }
                guard requestId == fileSearchGeneration, activeAtToken?.value == query else { return }
                fileSuggestions = []
                fileSearchLoading = false
                fileSearchError = error.localizedDescription
            }
        }
    }

    private func scheduleComposerPopupRefresh(for nextText: String) {
        popupRefreshTask?.cancel()
        let needsPopupEvaluation =
            showSlashPopup ||
            showFilePopup ||
            showSkillPopup ||
            activeSlashToken != nil ||
            activeAtToken != nil ||
            activeDollarToken != nil ||
            nextText.contains("/") ||
            nextText.contains("@") ||
            nextText.contains("$")

        guard needsPopupEvaluation else {
            hideComposerPopups()
            return
        }

        popupRefreshTask = Task { @MainActor in
            try? await Task.sleep(nanoseconds: 70_000_000)
            guard !Task.isCancelled else { return }
            refreshComposerPopups(for: nextText)
        }
    }

    private func refreshComposerPopups(for nextText: String) {
        let cursor = nextText.count
        if let atToken = currentPrefixedToken(
            text: nextText,
            cursor: cursor,
            prefix: "@",
            allowEmpty: true
        ) {
            if showSlashPopup {
                showSlashPopup = false
            }
            if activeSlashToken != nil {
                activeSlashToken = nil
            }
            if !slashSuggestions.isEmpty {
                slashSuggestions = []
            }
            if showSkillPopup {
                showSkillPopup = false
            }
            if activeDollarToken != nil {
                activeDollarToken = nil
            }
            if !showFilePopup {
                showFilePopup = true
            }
            if activeAtToken != atToken {
                activeAtToken = atToken
                startFileSearch(atToken.value)
                loadPluginsIfNeeded()
            }
            return
        }

        if activeAtToken != nil || showFilePopup || fileSearchTask != nil || fileSearchLoading || fileSearchError != nil || !fileSuggestions.isEmpty {
            activeAtToken = nil
            if showFilePopup {
                showFilePopup = false
            }
            clearFileSearchState()
        }

        if let dollarToken = currentPrefixedToken(
            text: nextText,
            cursor: cursor,
            prefix: "$",
            allowEmpty: true
        ), isMentionQueryValid(dollarToken.value) {
            if showSlashPopup {
                showSlashPopup = false
            }
            if activeSlashToken != nil {
                activeSlashToken = nil
            }
            if !slashSuggestions.isEmpty {
                slashSuggestions = []
            }
            if !showSkillPopup {
                showSkillPopup = true
            }
            if activeDollarToken != dollarToken {
                activeDollarToken = dollarToken
            }
            if !hasAttemptedSkillMentionLoad && !skillsLoading {
                hasAttemptedSkillMentionLoad = true
                Task { await loadSkills(showErrors: false) }
            }
            return
        }

        if activeDollarToken != nil || showSkillPopup {
            activeDollarToken = nil
            if showSkillPopup {
                showSkillPopup = false
            }
        }

        guard let slashToken = currentSlashQueryContext(text: nextText, cursor: cursor) else {
            if showSlashPopup {
                showSlashPopup = false
            }
            if activeSlashToken != nil {
                activeSlashToken = nil
            }
            if !slashSuggestions.isEmpty {
                slashSuggestions = []
            }
            return
        }

        if activeSlashToken != slashToken {
            activeSlashToken = slashToken
        }
        let suggestions = filterSlashCommands(slashToken.query)
        if slashSuggestions != suggestions {
            slashSuggestions = suggestions
        }
        let shouldShow = !suggestions.isEmpty
        if showSlashPopup != shouldShow {
            showSlashPopup = shouldShow
        }
    }

    private func applySlashSuggestion(_ command: ComposerSlashCommand) {
        showSlashPopup = false
        activeSlashToken = nil
        slashSuggestions = []
        inputText = ""
        attachedImage = nil
        attachedFiles = []
        isComposerFocused = false
        executeSlashCommand(command, args: nil)
    }

    private func executeSlashCommand(_ command: ComposerSlashCommand, args: String?) {
        switch command {
        case .plan:
            onOpenModePicker()
        case .model:
            showModelSelector = true
        case .permissions:
            showPermissionsSheet = true
        case .experimental:
            showExperimentalSheet = true
            Task { await loadExperimentalFeatures() }
        case .skills:
            showSkillsSheet = true
            Task { await loadSkills() }
        case .review:
            Task { await startReview() }
        case .goal:
            Task { await handleGoalCommand(args) }
        case .rename:
            let initialName = args?.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
            if initialName.isEmpty {
                let currentTitle = snapshot.threadPreview.trimmingCharacters(in: .whitespacesAndNewlines)
                renameCurrentThreadTitle = currentTitle.isEmpty ? "Untitled thread" : currentTitle
                renameDraft = ""
                showRenamePrompt = true
            } else {
                Task { await renameThread(initialName) }
            }
        case .new:
            appState.showServerPicker = true
        case .fork:
            Task { await forkConversation() }
        case .resume:
            onResumeSessions?(snapshot.threadKey.serverId)
        }
    }

    private func parseSlashCommandInvocation(_ text: String) -> (command: ComposerSlashCommand, args: String?)? {
        let firstLine = text.split(separator: "\n", maxSplits: 1, omittingEmptySubsequences: false).first.map(String.init) ?? ""
        let trimmed = firstLine.trimmingCharacters(in: .whitespacesAndNewlines)
        guard trimmed.hasPrefix("/") else { return nil }
        let commandAndArgs = trimmed.dropFirst()
        let commandName = commandAndArgs.split(separator: " ", maxSplits: 1, omittingEmptySubsequences: true).first.map(String.init) ?? ""
        guard let command = ComposerSlashCommand(rawCommand: commandName) else { return nil }
        let args = commandAndArgs.split(separator: " ", maxSplits: 1, omittingEmptySubsequences: true).dropFirst().first.map(String.init)
        return (command, args)
    }

    private func startReview() async {
        do {
            _ = try await appModel.client.startReview(
                serverId: snapshot.threadKey.serverId,
                params: AppStartReviewRequest(
                    threadId: snapshot.threadKey.threadId,
                    target: .uncommittedChanges,
                    delivery: "inline"
                )
            )
        } catch {
            slashErrorMessage = error.localizedDescription
        }
    }

    private func handleGoalCommand(_ args: String?) async {
        let raw = args?.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
        let lower = raw.lowercased()
        do {
            switch lower {
            case "":
                let goal = try await appModel.client.getThreadGoal(
                    serverId: snapshot.threadKey.serverId,
                    params: AppThreadGoalGetRequest(threadId: snapshot.threadKey.threadId)
                )
                guard let goal else {
                    slashErrorMessage = "No goal is set for this thread."
                    return
                }
                slashErrorMessage = goalSummary(goal)
            case "pause":
                _ = try await appModel.client.setThreadGoal(
                    serverId: snapshot.threadKey.serverId,
                    params: AppThreadGoalSetRequest(
                        threadId: snapshot.threadKey.threadId,
                        objective: nil,
                        status: .paused,
                        tokenBudget: nil
                    )
                )
            case "resume":
                _ = try await appModel.client.setThreadGoal(
                    serverId: snapshot.threadKey.serverId,
                    params: AppThreadGoalSetRequest(
                        threadId: snapshot.threadKey.threadId,
                        objective: nil,
                        status: .active,
                        tokenBudget: nil
                    )
                )
            case "clear":
                _ = try await appModel.client.clearThreadGoal(
                    serverId: snapshot.threadKey.serverId,
                    params: AppThreadGoalClearRequest(threadId: snapshot.threadKey.threadId)
                )
            default:
                _ = try await appModel.client.setThreadGoal(
                    serverId: snapshot.threadKey.serverId,
                    params: AppThreadGoalSetRequest(
                        threadId: snapshot.threadKey.threadId,
                        objective: raw,
                        status: .active,
                        tokenBudget: nil
                    )
                )
            }
        } catch {
            slashErrorMessage = error.localizedDescription
        }
    }

    private func goalSummary(_ goal: AppThreadGoal) -> String {
        var lines = [
            "Goal: \(goal.objective)",
            "Status: \(goalStatusLabel(goal.status))",
            "Tokens used: \(goal.tokensUsed)"
        ]
        if let tokenBudget = goal.tokenBudget {
            lines.append("Token budget: \(tokenBudget)")
        }
        return lines.joined(separator: "\n")
    }

    private func goalStatusLabel(_ status: AppThreadGoalStatus) -> String {
        switch status {
        case .active: return "active"
        case .paused: return "paused"
        case .blocked: return "blocked"
        case .usageLimited: return "limited by usage"
        case .budgetLimited: return "limited by budget"
        case .complete: return "complete"
        }
    }

    private func makeGoalCardActions() -> GoalCardActions {
        GoalCardActions(
            togglePause: {
                guard let current = snapshot.goal?.status else { return }
                let next: AppThreadGoalStatus
                switch current {
                case .active: next = .paused
                case .paused, .blocked, .usageLimited, .budgetLimited: next = .active
                case .complete: return
                }
                Task { await applyGoalUpdate(status: next) }
            },
            markComplete: {
                Task { await applyGoalUpdate(status: .complete) }
            },
            setObjective: { objective in
                Task { await applyGoalUpdate(objective: objective) }
            },
            setBudget: { value in
                let goal = snapshot.goal
                let resumeFromLimit = goal?.status == .budgetLimited
                    && (value ?? 0) > (goal?.tokensUsed ?? 0)
                Task {
                    await applyGoalUpdate(
                        status: resumeFromLimit ? .active : nil,
                        tokenBudget: value
                    )
                }
            },
            clear: {
                Task { await clearGoal() }
            }
        )
    }

    private func applyGoalUpdate(
        objective: String? = nil,
        status: AppThreadGoalStatus? = nil,
        tokenBudget: Int64? = nil
    ) async {
        do {
            _ = try await appModel.client.setThreadGoal(
                serverId: snapshot.threadKey.serverId,
                params: AppThreadGoalSetRequest(
                    threadId: snapshot.threadKey.threadId,
                    objective: objective,
                    status: status,
                    tokenBudget: tokenBudget
                )
            )
        } catch {
            slashErrorMessage = error.localizedDescription
        }
    }

    private func clearGoal() async {
        do {
            _ = try await appModel.client.clearThreadGoal(
                serverId: snapshot.threadKey.serverId,
                params: AppThreadGoalClearRequest(threadId: snapshot.threadKey.threadId)
            )
        } catch {
            slashErrorMessage = error.localizedDescription
        }
    }

    private func renameThread(_ newName: String) async {
        do {
            try await appModel.renameThread(
                serverId: snapshot.threadKey.serverId,
                threadId: snapshot.threadKey.threadId,
                title: newName
            )
            showRenamePrompt = false
            renameCurrentThreadTitle = ""
            renameDraft = ""
        } catch {
            slashErrorMessage = error.localizedDescription
        }
    }

    private func forkConversation() async {
        do {
            let nextKey = try await appModel.client.forkThread(
                serverId: snapshot.threadKey.serverId,
                params: AppThreadLaunchConfig(
                    agentRuntimeKind: pendingAgentRuntimeKindOverride,
                    model: pendingModelOverride,
                    approvalPolicy: appState.launchApprovalPolicy(for: snapshot.threadKey),
                    sandbox: appState.launchSandboxMode(for: snapshot.threadKey),
                    developerInstructions: nil,
                    persistExtendedHistory: true
                ).threadForkRequest(threadId: snapshot.threadKey.threadId, cwdOverride: workDir)
            )
            appModel.store.setActiveThread(key: nextKey)
            await appModel.refreshThreadSnapshot(key: nextKey)
            let nextCwd = workDir.trimmingCharacters(in: .whitespacesAndNewlines)
            if !nextCwd.isEmpty {
                workDir = nextCwd
                appState.currentCwd = nextCwd
            }
            onOpenConversation?(nextKey)
        } catch {
            slashErrorMessage = error.localizedDescription
        }
    }

    private func loadExperimentalFeatures() async {
        guard appModel.snapshot?.servers.first(where: { $0.serverId == snapshot.threadKey.serverId })?.canUseTransportActions == true else {
            experimentalFeatures = []
            slashErrorMessage = "Not connected to a server"
            return
        }
        experimentalFeaturesLoading = true
        defer { experimentalFeaturesLoading = false }
        do {
            let features = try await appModel.client.listExperimentalFeatures(
                serverId: snapshot.threadKey.serverId,
                params: AppListExperimentalFeaturesRequest(cursor: nil, limit: 200)
            )
            experimentalFeatures = features.sorted { lhs, rhs in
                let left = (lhs.displayName.flatMap { $0.isEmpty ? nil : $0 } ?? lhs.name).lowercased()
                let right = (rhs.displayName.flatMap { $0.isEmpty ? nil : $0 } ?? rhs.name).lowercased()
                return left < right
            }
        } catch {
            slashErrorMessage = error.localizedDescription
        }
    }

    private func isExperimentalFeatureEnabled(_ featureId: String, fallback: Bool) -> Bool {
        experimentalFeatures.first(where: { $0.id == featureId })?.enabled ?? fallback
    }

    private func setExperimentalFeature(named featureName: String, enabled: Bool) async {
        guard appModel.snapshot?.servers.first(where: { $0.serverId == snapshot.threadKey.serverId })?.canUseTransportActions == true else {
            slashErrorMessage = "Not connected to a server"
            return
        }
        guard let currentIndex = experimentalFeatures.firstIndex(where: { $0.name == featureName }) else {
            return
        }
        let currentFeature = experimentalFeatures[currentIndex]
        if currentFeature.enabled != enabled {
            experimentalFeatures[currentIndex] = ExperimentalFeature(
                name: currentFeature.name,
                stage: currentFeature.stage,
                displayName: currentFeature.displayName,
                description: currentFeature.description,
                announcement: currentFeature.announcement,
                enabled: enabled,
                defaultEnabled: currentFeature.defaultEnabled
            )
        }
        do {
            _ = try await appModel.client.writeConfigValue(
                serverId: snapshot.threadKey.serverId,
                params: AppWriteConfigValueRequest(
                    keyPath: "features.\(featureName)",
                    valueJson: enabled ? "true" : "false",
                    mergeStrategy: .upsert,
                    filePath: nil,
                    expectedVersion: nil
                )
            )
        } catch {
            slashErrorMessage = error.localizedDescription
            if let rollbackIndex = experimentalFeatures.firstIndex(where: { $0.name == currentFeature.name }) {
                experimentalFeatures[rollbackIndex] = ExperimentalFeature(
                    name: currentFeature.name,
                    stage: currentFeature.stage,
                    displayName: currentFeature.displayName,
                    description: currentFeature.description,
                    announcement: currentFeature.announcement,
                    enabled: currentFeature.enabled,
                    defaultEnabled: currentFeature.defaultEnabled
                )
            }
        }
    }

    private func loadSkills(forceReload: Bool = false) async {
        await loadSkills(forceReload: forceReload, showErrors: true)
    }

    private func loadSkills(forceReload: Bool = false, showErrors: Bool) async {
        guard appModel.snapshot?.servers.first(where: { $0.serverId == snapshot.threadKey.serverId })?.canUseTransportActions == true else {
            skills = []
            mentionSkillPathsByName = [:]
            if showErrors {
                slashErrorMessage = "Not connected to a server"
            }
            return
        }
        skillsLoading = true
        defer { skillsLoading = false }
        do {
            let fetchedSkills = try await appModel.client.listSkills(
                serverId: snapshot.threadKey.serverId,
                params: AppListSkillsRequest(
                    cwds: [workDir],
                    forceReload: forceReload
                )
            )
            let loadedSkills = fetchedSkills.sorted { $0.name.lowercased() < $1.name.lowercased() }
            skills = loadedSkills
            let validPaths = Set(loadedSkills.map { $0.path.value })
            mentionSkillPathsByName = mentionSkillPathsByName.filter { _, path in validPaths.contains(path) }
        } catch {
            if showErrors {
                slashErrorMessage = error.localizedDescription
            }
        }
    }

    private func applyFileSuggestion(_ match: FileSearchResult) {
        guard let token = activeAtToken else { return }
        let quotedPath = (match.path.contains(" ") && !match.path.contains("\"")) ? "\"\(match.path)\"" : match.path
        let replacement = "\(quotedPath) "
        guard let updated = replacingRange(
            in: inputText,
            with: token.range,
            replacement: replacement
        ) else { return }
        inputText = updated
        showFilePopup = false
        activeAtToken = nil
        clearFileSearchState()
    }

    private var pluginSuggestions: [PluginSummary] {
        guard let token = activeAtToken else { return [] }
        let plugins = pluginCacheByCwd[workDir] ?? []
        guard !plugins.isEmpty else { return [] }
        let query = token.value.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
        if query.isEmpty {
            return plugins
        }
        return plugins.filter { plugin in
            if plugin.name.lowercased().contains(query) { return true }
            if plugin.displayTitle.lowercased().contains(query) { return true }
            if let desc = plugin.interface?.shortDescription?.lowercased(), desc.contains(query) {
                return true
            }
            return plugin.marketplaceName.lowercased().contains(query)
        }
    }

    private func loadPluginsIfNeeded() {
        let cwd = workDir
        guard !pluginUnsupportedCwds.contains(cwd),
              pluginCacheByCwd[cwd] == nil,
              !pluginLoadingCwds.contains(cwd) else {
            return
        }
        pluginLoadingCwds.insert(cwd)
        Task {
            defer { pluginLoadingCwds.remove(cwd) }
            do {
                let plugins = try await appModel.client.listPlugins(
                    serverId: snapshot.threadKey.serverId,
                    params: AppListPluginsRequest(cwds: [cwd])
                )
                pluginCacheByCwd[cwd] = plugins
            } catch {
                pluginUnsupportedCwds.insert(cwd)
            }
        }
    }

    private func applyPluginSuggestion(_ plugin: PluginSummary) {
        guard let token = activeAtToken else { return }
        let replacement = "@\(plugin.name) "
        guard let updated = replacingRange(
            in: inputText,
            with: token.range,
            replacement: replacement
        ) else { return }
        inputText = updated
        let selection = PluginMentionSelection(
            name: plugin.name,
            marketplace: plugin.marketplaceName,
            displayName: plugin.interface?.displayName ?? plugin.displayTitle
        )
        if !pluginMentionSelections.contains(selection) {
            pluginMentionSelections.append(selection)
        }
        showFilePopup = false
        activeAtToken = nil
        clearFileSearchState()
    }

    private func removePluginMention(_ selection: PluginMentionSelection) {
        pluginMentionSelections.removeAll { $0 == selection }
        // Best-effort strip of the inline `@name` token from the input.
        let needle = "@\(selection.name)"
        if let range = inputText.range(of: needle) {
            var replaced = inputText
            replaced.removeSubrange(range)
            // Collapse any double-space artifact left behind.
            inputText = replaced.replacingOccurrences(of: "  ", with: " ")
        }
    }

    private var skillSuggestions: [SkillMetadata] {
        guard let token = activeDollarToken else { return [] }
        return filterSkillSuggestions(token.value)
    }

    private func filterSkillSuggestions(_ query: String) -> [SkillMetadata] {
        guard !skills.isEmpty else { return [] }
        guard !query.isEmpty else { return skills.sorted { lhs, rhs in lhs.name.lowercased() < rhs.name.lowercased() } }
        return skills
            .compactMap { skill -> (SkillMetadata, Int)? in
                let scoreFromName = fuzzyScore(candidate: skill.name, query: query)
                let scoreFromDescription = fuzzyScore(candidate: skill.description, query: query)
                let best = max(scoreFromName ?? Int.min, scoreFromDescription ?? Int.min)
                guard best != Int.min else { return nil }
                return (skill, best)
            }
            .sorted { lhs, rhs in
                if lhs.1 != rhs.1 {
                    return lhs.1 > rhs.1
                }
                return lhs.0.name.lowercased() < rhs.0.name.lowercased()
            }
            .map(\.0)
    }

    private func applySkillSuggestion(_ skill: SkillMetadata) {
        guard let token = activeDollarToken else { return }
        let replacement = "$\(skill.name) "
        guard let updated = replacingRange(
            in: inputText,
            with: token.range,
            replacement: replacement
        ) else { return }
        inputText = updated
        mentionSkillPathsByName[skill.name.lowercased()] = skill.path.value
        showSkillPopup = false
        activeDollarToken = nil
    }

    private func collectSkillMentionsForSubmission(_ text: String) -> [SkillMentionSelection] {
        guard !skills.isEmpty else { return [] }
        let mentionNames = extractMentionNames(text)
        guard !mentionNames.isEmpty else { return [] }

        let skillsByName = Dictionary(grouping: skills, by: { $0.name.lowercased() })
        let skillsByPath = Dictionary(grouping: skills, by: \.path.value)
        var seenPaths = Set<String>()
        var resolved: [SkillMentionSelection] = []

        for mentionName in mentionNames {
            let normalizedName = mentionName.lowercased()
            if let selectedPath = mentionSkillPathsByName[normalizedName], !selectedPath.isEmpty {
                if let selectedSkill = skillsByPath[selectedPath]?.first {
                    guard seenPaths.insert(selectedPath).inserted else { continue }
                    resolved.append(SkillMentionSelection(name: selectedSkill.name, path: selectedPath))
                    continue
                }
                mentionSkillPathsByName.removeValue(forKey: normalizedName)
            }

            guard let candidates = skillsByName[normalizedName], candidates.count == 1 else {
                continue
            }
            let match = candidates[0]
            guard seenPaths.insert(match.path.value).inserted else { continue }
            resolved.append(SkillMentionSelection(name: match.name, path: match.path.value))
        }
        return resolved
    }
}
