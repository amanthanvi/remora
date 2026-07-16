import SwiftUI

enum SavedAppToolbarLayout: Equatable {
    case inline
    case stacked
}

/// Fullscreen host for a single saved app. Loads the widget HTML + persisted
/// state from Rust on appear, renders through `WidgetWebView` in app-mode so
/// the `loadAppState` / `saveAppState` JS bridge is wired. State saves flow
/// through the onMessage handler into `SavedAppsStore.saveState`, which
/// debounces the write.
struct SavedAppDetailView: View {
    let appId: String

    @Environment(\.dismiss) private var dismiss
    @Environment(\.horizontalSizeClass) private var horizontalSizeClass
    @Environment(\.dynamicTypeSize) private var dynamicTypeSize
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @State private var store = SavedAppsStore.shared
    @State private var payload: SavedAppWithPayload?
    @State private var loadAttempted = false
    @State private var renameText: String = ""
    @State private var showRenameSheet = false
    @State private var showUpdateOverlay = false
    @State private var isUpdating = false
    @State private var updateError: String?
    @State private var updateSuccessMessage: String?
    @State private var reloadTick = 0
    @State private var pollingTask: Task<Void, Never>?
    @State private var showDeleteConfirm = false
    /// In-memory ephemeral thread id for this saved-app view. Rust owns
    /// the thread; we just cache the id so consecutive `structuredResponse`
    /// calls in this view reuse the same hidden thread. Dies with the
    /// view — that's the whole point (Option B in the plan).
    @State private var cachedStructuredThreadId: String?

    var body: some View {
        ZStack {
            Color.black.ignoresSafeArea()
            if let payload {
                ZStack {
                    WidgetWebView(
                        widgetHTML: payload.widgetHtml,
                        isFinalized: true,
                        allowsScrollAndZoom: true,
                        onMessage: handleMessage,
                        onStructuredRequest: handleStructuredRequest,
                        appMode: true,
                        initialAppState: payload.stateJson,
                        schemaVersion: Int(payload.app.schemaVersion)
                    )
                    .id("\(appId)-\(reloadTick)")
                    .ignoresSafeArea(edges: [.bottom, .horizontal])
                    .safeAreaInset(edge: .top, spacing: 0) {
                        topBar(for: payload.app)
                    }

                    if isUpdating {
                        shimmerOverlay
                    }
                }

            } else if loadAttempted {
                brokenAppPlaceholder
            } else {
                ProgressView()
                    .tint(RemoraTheme.accent)
            }

            if showUpdateOverlay {
                SavedAppUpdateOverlay(
                    isUpdating: $isUpdating,
                    errorMessage: $updateError,
                    onSubmit: { prompt in
                        Task { await runUpdate(prompt: prompt) }
                    },
                    onDismiss: {
                        showUpdateOverlay = false
                        updateError = nil
                    }
                )
                .transition(reduceMotion ? .opacity : .move(edge: .bottom).combined(with: .opacity))
            }

            if let message = updateSuccessMessage {
                VStack {
                    Spacer()
                    Text(message)
                        .remoraFont(.footnote, weight: .semibold)
                        .foregroundColor(RemoraTheme.textPrimary)
                        .padding(.horizontal, 14)
                        .padding(.vertical, 10)
                        .background(RemoraTheme.surfaceLight.opacity(0.9))
                        .clipShape(Capsule())
                        .padding(.bottom, 28)
                }
                .transition(reduceMotion ? .opacity : .move(edge: .bottom).combined(with: .opacity))
            }
        }
        .navigationBarBackButtonHidden(true)
        .onAppear(perform: reloadPayload)
        .onDisappear {
            pollingTask?.cancel()
            pollingTask = nil
        }
        .onChange(of: isUpdating) { _, updating in
            if updating {
                startPollingForUpdate()
            } else {
                pollingTask?.cancel()
                pollingTask = nil
            }
        }
        .sheet(isPresented: $showRenameSheet) {
            renameSheet
                .presentationDetents([.medium])
        }
        .alert(
            "Delete \"\(payload?.app.title ?? "")\"?",
            isPresented: $showDeleteConfirm
        ) {
            Button("Cancel", role: .cancel) {}
            Button("Delete", role: .destructive) {
                try? store.delete(id: appId)
                dismiss()
            }
        } message: {
            Text("This removes the app, its saved HTML, and its persisted state.")
        }
    }

    static func toolbarLayout(
        dynamicTypeSize: DynamicTypeSize,
        horizontalSizeClass: UserInterfaceSizeClass?
    ) -> SavedAppToolbarLayout {
        dynamicTypeSize.isAccessibilitySize || horizontalSizeClass == .compact
            ? .stacked
            : .inline
    }

    private var toolbarLayout: SavedAppToolbarLayout {
        Self.toolbarLayout(
            dynamicTypeSize: dynamicTypeSize,
            horizontalSizeClass: horizontalSizeClass
        )
    }

    /// While a saved-app update is in flight, poll the on-disk HTML
    /// every 500ms. When the model-driven `apply_patch` lands a new
    /// version, reassign `payload` so `WidgetWebView` picks up the
    /// change through its existing morphdom debouncer. The final
    /// reassign after the RPC completes is handled by `reloadPayload`
    /// in `runUpdate`.
    private func startPollingForUpdate() {
        pollingTask?.cancel()
        let id = appId
        pollingTask = Task { @MainActor in
            while !Task.isCancelled {
                try? await Task.sleep(nanoseconds: 500_000_000)
                if Task.isCancelled { break }
                guard let fresh = store.getWithPayload(id: id) else { continue }
                if fresh.widgetHtml != payload?.widgetHtml {
                    payload = fresh
                }
            }
        }
    }

    private func topBar(for app: SavedApp) -> some View {
        Group {
            if toolbarLayout == .stacked {
                VStack(spacing: 8) {
                    HStack(spacing: 8) {
                        backButton
                        titleButton(for: app)
                        Spacer(minLength: 0)
                        optionsMenu(for: app)
                    }

                    HStack(spacing: 8) {
                        conversationButton(for: app)
                        Spacer(minLength: 0)
                        updateButton
                    }
                }
            } else {
                inlineTopBar(for: app)
            }
        }
        .padding(8)
        .background(
            RoundedRectangle(cornerRadius: 16, style: .continuous)
                .fill(RemoraTheme.surface)
        )
        .overlay(
            RoundedRectangle(cornerRadius: 16, style: .continuous)
                .stroke(RemoraTheme.border, lineWidth: 1)
                .allowsHitTesting(false)
        )
        .padding(.horizontal, 12)
        .padding(.top, 8)
    }

    private func inlineTopBar(for app: SavedApp) -> some View {
        HStack(spacing: 8) {
            backButton
            titleButton(for: app)
            Spacer(minLength: 0)
            optionsMenu(for: app)
            conversationButton(for: app)
            updateButton
        }
    }

    private var backButton: some View {
        Button {
            dismiss()
        } label: {
            Image(systemName: "chevron.left")
                .remoraControlIconFont(size: 17, weight: .semibold)
                .foregroundColor(RemoraTheme.textPrimary)
                .frame(
                    width: RemoraAccessibilityMetrics.minimumHitTarget,
                    height: RemoraAccessibilityMetrics.minimumHitTarget
                )
                .background(Circle().fill(RemoraTheme.surfaceLight))
                .contentShape(Circle())
        }
        .buttonStyle(.plain)
        .accessibilityLabel("Back")
    }

    private func titleButton(for app: SavedApp) -> some View {
        Button {
            renameText = app.title
            showRenameSheet = true
        } label: {
            Text(app.title)
                .remoraFont(.headline, weight: .semibold)
                .foregroundColor(RemoraTheme.textPrimary)
                .lineLimit(toolbarLayout == .stacked ? 2 : 1)
                .multilineTextAlignment(.leading)
                .padding(.horizontal, 12)
                .frame(minHeight: RemoraAccessibilityMetrics.minimumHitTarget)
                .background(
                    Capsule(style: .continuous)
                        .fill(RemoraTheme.surfaceLight)
                )
                .contentShape(Capsule())
        }
        .buttonStyle(.plain)
        .accessibilityLabel("Rename \(app.title)")
    }

    private func optionsMenu(for app: SavedApp) -> some View {
        Menu {
            Button {
                renameText = app.title
                showRenameSheet = true
            } label: {
                Label("Rename", systemImage: "pencil")
            }
            Button(role: .destructive) {
                showDeleteConfirm = true
            } label: {
                Label("Delete", systemImage: "trash")
            }
        } label: {
            Image(systemName: "ellipsis")
                .remoraControlIconFont(size: 17, weight: .semibold)
                .foregroundColor(RemoraTheme.textPrimary)
                .frame(
                    width: RemoraAccessibilityMetrics.minimumHitTarget,
                    height: RemoraAccessibilityMetrics.minimumHitTarget
                )
                .background(Circle().fill(RemoraTheme.surfaceLight))
                .contentShape(Circle())
        }
        .accessibilityLabel("App options")
    }

    @ViewBuilder
    private func conversationButton(for app: SavedApp) -> some View {
        if let threadId = app.originThreadId, threadExists(threadId) {
            Button {
                SavedAppsNavigation.shared.requestConversation(threadId: threadId)
                dismiss()
            } label: {
                Image(systemName: "text.bubble.fill")
                    .remoraControlIconFont(size: 15, weight: .semibold)
                    .foregroundColor(RemoraTheme.textPrimary)
                    .frame(
                        width: RemoraAccessibilityMetrics.minimumHitTarget,
                        height: RemoraAccessibilityMetrics.minimumHitTarget
                    )
                    .background(Circle().fill(RemoraTheme.surfaceLight))
                    .contentShape(Circle())
            }
            .buttonStyle(.plain)
            .accessibilityLabel("View Conversation")
        }
    }

    private var updateButton: some View {
        Button {
            showUpdateOverlay = true
            updateError = nil
        } label: {
            HStack(spacing: 10) {
                Image(systemName: "arrow.triangle.2.circlepath")
                    .remoraFont(size: 12, weight: .semibold)
                Text("Update")
                    .remoraFont(size: 13, weight: .semibold)
            }
            .foregroundColor(RemoraTheme.accentForegroundOnSurface)
            .padding(.horizontal, 14)
            .frame(minHeight: RemoraAccessibilityMetrics.minimumHitTarget)
            .background(Capsule(style: .continuous).fill(RemoraTheme.surfaceLight))
            .overlay(
                Capsule(style: .continuous)
                    .stroke(RemoraTheme.accent, lineWidth: 1)
                    .allowsHitTesting(false)
            )
            .contentShape(Capsule())
        }
        .buttonStyle(.plain)
        .disabled(isUpdating)
        .opacity(isUpdating ? 0.6 : 1)
        .accessibilityLabel("Update app")
    }

    private func threadExists(_ threadId: String) -> Bool {
        guard let threads = AppModel.shared.snapshot?.threads else { return false }
        return threads.contains(where: { $0.key.threadId == threadId })
    }

    private var shimmerOverlay: some View {
        // Subtle dim + shimmer over the running widget while the update is in
        // flight. The widget itself stays interactive; the shimmer is purely
        // a visual hint that a regeneration is happening in the background.
        ZStack {
            Color.black.opacity(0.25)
                .ignoresSafeArea()
            ShimmerStrip()
                .frame(maxWidth: .infinity)
                .frame(height: 2)
                .padding(.top, 0)
                .frame(maxHeight: .infinity, alignment: .top)
        }
        .allowsHitTesting(false)
    }

    private var brokenAppPlaceholder: some View {
        VStack(spacing: 14) {
            Image(systemName: "exclamationmark.triangle")
                .remoraFont(.largeTitle)
                .foregroundColor(RemoraTheme.warning)
            Text("This app's files are missing")
                .remoraFont(.title3, weight: .semibold)
                .foregroundColor(.white)
            Text("Delete it to clear the entry.")
                .remoraFont(.footnote)
                .foregroundColor(.white.opacity(0.7))

            Button(role: .destructive) {
                try? store.delete(id: appId)
                dismiss()
            } label: {
                Text("Delete App")
                    .remoraFont(.body, weight: .semibold)
                    .padding(.horizontal, 16)
                    .frame(minHeight: RemoraAccessibilityMetrics.minimumHitTarget)
                    .background(RemoraTheme.danger.opacity(0.2))
                    .clipShape(Capsule())
                    .foregroundColor(RemoraTheme.danger)
            }
        }
        .padding(32)
    }

    private var renameSheet: some View {
        VStack(alignment: .leading, spacing: 16) {
            Text("Rename App")
                .remoraFont(.title3, weight: .semibold)
                .foregroundColor(RemoraTheme.textPrimary)

            TextField("Title", text: $renameText)
                .remoraFont(size: 15)
                .padding(10)
                .frame(minHeight: RemoraAccessibilityMetrics.minimumHitTarget)
                .background(RemoraTheme.surfaceLight.opacity(0.6))
                .clipShape(RoundedRectangle(cornerRadius: 8))
                .foregroundColor(RemoraTheme.textPrimary)

            HStack {
                Button("Cancel") { showRenameSheet = false }
                    .frame(minHeight: RemoraAccessibilityMetrics.minimumHitTarget)
                    .foregroundColor(RemoraTheme.textSecondary)
                Spacer()
                Button("Save") {
                    let trimmed = renameText.trimmingCharacters(in: .whitespacesAndNewlines)
                    guard !trimmed.isEmpty else { showRenameSheet = false; return }
                    _ = try? store.rename(id: appId, title: trimmed)
                    showRenameSheet = false
                    reloadPayload()
                }
                .frame(minHeight: RemoraAccessibilityMetrics.minimumHitTarget)
                .foregroundColor(RemoraTheme.accentForegroundOnSurface)
                .disabled(renameText.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
            }
            Spacer()
        }
        .padding(20)
        .background(RemoraTheme.surface.ignoresSafeArea())
    }

    // MARK: - Actions

    private func reloadPayload() {
        payload = store.getWithPayload(id: appId)
        loadAttempted = true
        reloadTick &+= 1
    }

    private func handleMessage(_ body: Any) {
        guard let dict = body as? [String: Any],
              let type = dict["_type"] as? String else { return }
        switch type {
        case "saveAppState":
            guard let value = dict["value"] as? String else { return }
            let schema = (dict["schema"] as? Int).map(UInt32.init) ?? (payload?.app.schemaVersion ?? 1)
            store.saveState(id: appId, stateJson: value, schemaVersion: schema)
        default:
            break
        }
    }

    private func handleStructuredRequest(
        requestId: String,
        prompt: String,
        responseFormatJSON: String,
        respond: @escaping (String, String?, String?) -> Void
    ) {
        guard let serverId = SavedAppDetailView.resolveActiveServerId() else {
            respond(requestId, nil, "No connected server available")
            return
        }
        let cached = cachedStructuredThreadId
        Task.detached {
            let result = await AppModel.shared.client.structuredResponse(
                serverId: serverId,
                cachedThreadId: cached,
                prompt: prompt,
                outputSchemaJson: responseFormatJSON
            )
            switch result {
            case .success(let threadId, let responseJson):
                await MainActor.run {
                    cachedStructuredThreadId = threadId
                }
                respond(requestId, responseJson, nil)
            case .error(let message):
                respond(requestId, nil, message)
            }
        }
    }

    private func runUpdate(prompt: String) async {
        guard let serverId = SavedAppDetailView.resolveActiveServerId() else {
            updateError = "No connected server available"
            return
        }
        isUpdating = true
        defer { isUpdating = false }
        do {
            _ = try await store.requestUpdate(id: appId, serverId: serverId, prompt: prompt)
            showUpdateOverlay = false
            updateError = nil
            reloadPayload()
            withAnimation(RemoraMotionPolicy.animation(.easeInOut(duration: 0.2), reduceMotion: reduceMotion)) {
                updateSuccessMessage = "Updated"
            }
            Task {
                try? await Task.sleep(nanoseconds: 2_500_000_000)
                withAnimation(RemoraMotionPolicy.animation(.easeInOut(duration: 0.2), reduceMotion: reduceMotion)) {
                    updateSuccessMessage = nil
                }
            }
        } catch {
            updateError = error.localizedDescription
        }
    }

    @MainActor
    private static func resolveActiveServerId() -> String? {
        let snapshot = AppModel.shared.snapshot
        // Prefer the active thread's server; fall back to any known server
        // id. If nothing is connected, the update will fail loudly in the
        // overlay.
        if let serverId = snapshot?.activeThread?.serverId { return serverId }
        return snapshot?.servers.first?.serverId
    }
}

/// Thin animated shimmer strip used at the top of the widget during an
/// in-flight update. Repeats a gradient wipe indefinitely; the parent
/// decides when to show/hide it.
private struct ShimmerStrip: View {
    @State private var phase: CGFloat = -1
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    var body: some View {
        GeometryReader { geo in
            if reduceMotion {
                RemoraTheme.accent.opacity(0.45)
            } else {
                LinearGradient(
                    colors: [
                        RemoraTheme.accent.opacity(0.0),
                        RemoraTheme.accent.opacity(0.9),
                        RemoraTheme.accent.opacity(0.0),
                    ],
                    startPoint: .leading,
                    endPoint: .trailing
                )
                .frame(width: geo.size.width)
                .offset(x: phase * geo.size.width)
                .task {
                    var resetTransaction = Transaction()
                    resetTransaction.disablesAnimations = true
                    withTransaction(resetTransaction) {
                        phase = -1
                    }
                    await Task.yield()
                    guard !Task.isCancelled, !reduceMotion else { return }
                    withAnimation(
                        .linear(duration: 1.2).repeatForever(autoreverses: false)
                    ) {
                        phase = 1
                    }
                }
            }
        }
    }
}
