import SwiftUI
import UIKit
import os

private let conversationRouteSignpostLog = OSLog(
    subsystem: Bundle.main.bundleIdentifier ?? "com.remora.app.ios",
    category: "ConversationRoute"
)

struct ConversationDestinationScreen: View {
    @Environment(AppModel.self) private var appModel
    @Environment(AppState.self) private var appState
    @AppStorage("workDir") private var workDir = FileManager.default.urls(for: .documentDirectory, in: .userDomainMask).first?.path ?? "/"
    @State private var screenModel = ConversationScreenModel()
    @State private var conversationObservation: AppModelConversationObservation?
    let threadKey: ThreadKey
    let bottomInset: CGFloat
    let onResumeSessions: (String) -> Void
    let onOpenConversation: (ThreadKey) -> Void
    var onInfo: (() -> Void)?

    private var routeObservation: AppModelConversationObservation? {
        conversationObservation?.matching(threadKey: threadKey)
    }

    private var conversationThread: AppThreadSnapshot? {
        routeObservation?.thread
    }

    private var resolvedThreadKey: ThreadKey {
        conversationThread?.key ?? threadKey
    }

    private func bindScreenModel(for thread: AppThreadSnapshot) {
        guard let conversationObservation = routeObservation else { return }
        screenModel.bind(
            thread: thread,
            appModel: appModel,
            conversationObservation: conversationObservation
        )
    }

    private var navigationTitle: String {
        conversationThread?.displayTitle ?? "Conversation"
    }

    var body: some View {
        Group {
            if let conversationThread {
                @Bindable var bindableScreenModel = screenModel
                ConversationView(
                    thread: conversationThread,
                    activeThreadKey: resolvedThreadKey,
                    transcript: screenModel.transcript,
                    followScrollToken: screenModel.followScrollToken,
                    pinnedContextItems: screenModel.pinnedContextItems,
                    composer: screenModel.composer,
                    composerInputText: $bindableScreenModel.composerInputText,
                    composerAttachedImage: $bindableScreenModel.composerAttachedImage,
                    topInset: 0,
                    bottomInset: bottomInset,
                    onOpenConversation: onOpenConversation,
                    onResumeSessions: onResumeSessions,
                    minigameOverlay: screenModel.minigameOverlay,
                    onTypingTap: { screenModel.requestMinigame() },
                    onMinigameDismiss: { screenModel.dismissMinigame() },
                    onMinigameRetry: {
                        screenModel.dismissMinigame()
                        screenModel.requestMinigame()
                    }
                )
                .onAppear {
                    bindScreenModel(for: conversationThread)
                }
                .onChange(of: conversationThread) { _, updatedThread in
                    bindScreenModel(for: updatedThread)
                }
            } else {
                VStack(spacing: 16) {
                    Spacer()
                    ProgressView()
                        .tint(RemoraTheme.accent)
                    Text("Loading thread...")
                        .remoraFont(.caption)
                        .foregroundColor(RemoraTheme.textMuted)
                    Spacer()
                }
                .frame(maxWidth: .infinity, maxHeight: .infinity)
                .background(RemoraTheme.backgroundGradient.ignoresSafeArea())
            }
        }
        .onChange(of: routeObservation?.revision) { _, _ in
            if let conversationThread {
                bindScreenModel(for: conversationThread)
            }
        }
        .navigationBarTitleDisplayMode(.inline)
        .toolbar {
            if let conversationThread {
                ToolbarItem(placement: .principal) {
                    HeaderView(thread: conversationThread)
                }
                ToolbarItem(placement: .topBarTrailing) {
                    ConversationToolbarControls(
                        thread: conversationThread,
                        control: .reload
                    )
                }
                if onInfo != nil {
                    ToolbarItem(placement: .topBarTrailing) {
                        ConversationToolbarControls(
                            thread: conversationThread,
                            control: .info,
                            onInfo: onInfo
                        )
                    }
                }
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .ignoresSafeArea(.container, edges: .bottom)
        .task(id: threadKey) {
            os_signpost(
                .event,
                log: conversationRouteSignpostLog,
                name: "ThreadOpenStarted",
                "server=%{public}@ thread=%{public}@",
                threadKey.serverId,
                threadKey.threadId
            )
            let observation = appModel.conversationObservation(for: threadKey)
            conversationObservation = observation
            appModel.activateThread(threadKey)
            if observation.thread == nil {
                _ = await appModel.ensureThreadLoaded(key: threadKey)
            }
            await appModel.loadConversationMetadataIfNeeded(serverId: threadKey.serverId)
            if let thread = observation.thread,
               let cwd = thread.info.cwd?.trimmingCharacters(in: .whitespacesAndNewlines),
               !cwd.isEmpty {
                workDir = cwd
                appState.currentCwd = cwd
            }
        }
    }
}

struct ReplayDestinationScreen: View {
    @Environment(AppModel.self) private var appModel
    let recordingUrl: URL
    let bottomInset: CGFloat
    @State private var screenModel = ConversationScreenModel()
    @State private var replayThreadKey: ThreadKey?
    @State private var conversationObservation: AppModelConversationObservation?
    @State private var recorder = MessageRecorder.shared

    private var routeObservation: AppModelConversationObservation? {
        conversationObservation?.matching(threadKey: replayThreadKey)
    }

    private var conversationThread: AppThreadSnapshot? {
        routeObservation?.thread
    }

    var body: some View {
        Group {
            if let thread = conversationThread,
               let key = replayThreadKey {
                @Bindable var bindableScreenModel = screenModel
                ConversationView(
                    thread: thread,
                    activeThreadKey: key,
                    transcript: screenModel.transcript,
                    followScrollToken: screenModel.followScrollToken,
                    pinnedContextItems: screenModel.pinnedContextItems,
                    composer: screenModel.composer,
                    composerInputText: $bindableScreenModel.composerInputText,
                    composerAttachedImage: $bindableScreenModel.composerAttachedImage,
                    topInset: 0,
                    bottomInset: bottomInset,
                    onOpenConversation: nil,
                    onResumeSessions: { _ in }
                )
                .onAppear { bindScreenModel(for: thread) }
                .onChange(of: thread) { _, t in bindScreenModel(for: t) }
            } else {
                VStack(spacing: 16) {
                    Spacer()
                    ProgressView()
                        .tint(RemoraTheme.accent)
                    Text(recorder.isReplaying ? "Replaying..." : "Starting replay...")
                        .remoraFont(.caption)
                        .foregroundColor(RemoraTheme.textMuted)
                    Spacer()
                }
                .frame(maxWidth: .infinity, maxHeight: .infinity)
                .background(RemoraTheme.backgroundGradient.ignoresSafeArea())
            }
        }
        .onChange(of: routeObservation?.revision) { _, _ in
            if let conversationThread {
                bindScreenModel(for: conversationThread)
            }
        }
        .navigationTitle("Replay")
        .navigationBarTitleDisplayMode(.inline)
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .ignoresSafeArea(.container, edges: .bottom)
        .task {
            let targetKey: ThreadKey
            if let server = appModel.snapshot?.servers.first {
                targetKey = ThreadKey(serverId: server.serverId, threadId: UUID().uuidString)
            } else {
                targetKey = ThreadKey(serverId: "replay", threadId: UUID().uuidString)
            }
            replayThreadKey = targetKey
            conversationObservation = appModel.conversationObservation(for: targetKey)
            appModel.activateThread(targetKey)
            recorder.startReplay(url: recordingUrl, store: appModel.store, targetKey: targetKey)
        }
        .onDisappear {
            recorder.stopReplay()
        }
    }

    private func bindScreenModel(for thread: AppThreadSnapshot) {
        guard let conversationObservation = routeObservation else { return }
        screenModel.bind(
            thread: thread,
            appModel: appModel,
            conversationObservation: conversationObservation
        )
    }
}

struct ApprovalPromptView: View {
    let approval: PendingApproval
    let onDecision: (ApprovalDecisionValue) -> Void
    var onViewThread: ((ThreadKey) -> Void)? = nil

    private var title: String {
        switch approval.kind {
        case .command:
            return "Command Approval Required"
        case .fileChange:
            return "File Change Approval Required"
        case .permissions:
            return "Permissions Approval Required"
        case .mcpElicitation:
            return "MCP Input Required"
        }
    }

    var body: some View {
        ZStack {
            Color.black.opacity(0.7)
                .ignoresSafeArea()

            VStack(alignment: .leading, spacing: 12) {
                Text(title)
                    .remoraFont(.headline)
                    .foregroundColor(RemoraTheme.textPrimary)

                ScrollView(.vertical, showsIndicators: true) {
                    VStack(alignment: .leading, spacing: 12) {
                        if let reason = approval.reason, !reason.isEmpty {
                            Text(reason)
                                .remoraFont(.footnote)
                                .foregroundColor(RemoraTheme.textSecondary)
                        }

                        if let threadId = approval.threadId, onViewThread != nil {
                            HStack {
                                Button {
                                    onViewThread?(ThreadKey(serverId: approval.serverId, threadId: threadId))
                                } label: {
                                    HStack(spacing: 3) {
                                        Text("View Thread")
                                            .remoraFont(.caption, weight: .medium)
                                        Image(systemName: "arrow.right")
                                            .remoraFont(size: 9, weight: .semibold)
                                    }
                                    .foregroundColor(RemoraTheme.accentForegroundOnSurface)
                                }
                                .buttonStyle(.plain)

                                Spacer()
                            }
                        }

                        if let command = approval.command, !command.isEmpty {
                            VStack(alignment: .leading, spacing: 6) {
                                Text("Command")
                                    .remoraFont(.caption)
                                    .foregroundColor(RemoraTheme.textMuted)
                                Text(command)
                                    .remoraFont(.footnote)
                                    .foregroundColor(RemoraTheme.textBody)
                                    .textSelection(.enabled)
                                    .padding(10)
                                    .frame(maxWidth: .infinity, alignment: .leading)
                                    .background(RemoraTheme.surface)
                                    .clipShape(RoundedRectangle(cornerRadius: 8))
                            }
                        }

                        if let cwd = approval.cwd, !cwd.isEmpty {
                            Text("CWD: \(cwd)")
                                .remoraFont(.caption)
                                .foregroundColor(RemoraTheme.textMuted)
                        }

                        if let grantRoot = approval.grantRoot, !grantRoot.isEmpty {
                            Text("Grant Root: \(grantRoot)")
                                .remoraFont(.caption)
                                .foregroundColor(RemoraTheme.textMuted)
                        }
                    }
                    .frame(maxWidth: .infinity, alignment: .leading)
                }

                VStack(spacing: 8) {
                    Button("Allow Once") { onDecision(.accept) }
                        .buttonStyle(.borderedProminent)
                        .tint(RemoraTheme.accent)
                        .frame(maxWidth: .infinity)

                    Button("Allow for Session") { onDecision(.acceptForSession) }
                        .buttonStyle(.bordered)
                        .frame(maxWidth: .infinity)

                    HStack(spacing: 8) {
                        Button("Deny") { onDecision(.decline) }
                            .buttonStyle(.bordered)
                            .foregroundColor(.red)
                            .frame(maxWidth: .infinity)

                        Button("Abort") { onDecision(.cancel) }
                            .buttonStyle(.bordered)
                            .frame(maxWidth: .infinity)
                    }
                }
                .remoraFont(.callout)
            }
            .padding(16)
            .frame(maxHeight: UIScreen.main.bounds.height * 0.8)
            .modifier(GlassRectModifier(cornerRadius: 14))
            .overlay(
                RoundedRectangle(cornerRadius: 14)
                    .stroke(RemoraTheme.border, lineWidth: 1)
            )
            .padding(.horizontal, 16)
        }
        .transition(.opacity)
    }
}

struct LaunchView: View {
    var body: some View {
        ZStack {
            RemoraTheme.backgroundGradient.ignoresSafeArea()
            VStack(spacing: 24) {
                RemoraLogo(size: 132)
                Text("AI coding agent on iOS")
                    .remoraFont(.body)
                    .foregroundColor(RemoraTheme.textMuted)
            }
        }
    }
}
