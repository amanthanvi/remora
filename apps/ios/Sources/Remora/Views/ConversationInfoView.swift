import SwiftUI

struct ConversationInfoView: View {
    @Environment(AppModel.self) private var appModel
    @Environment(AppState.self) private var appState
    @Environment(ThemeManager.self) private var themeManager
    @Environment(\.dismiss) private var dismiss

    /// When nil, the screen shows server-only info (no session-specific sections).
    let threadKey: ThreadKey?
    /// Server ID used when threadKey is nil (server-only mode).
    let serverId: String?
    var onOpenWallpaper: (() -> Void)?
    var onOpenConversation: ((ThreadKey) -> Void)?
    var onOpenShell: (() -> Void)?

    /// Whether we're in server-only mode (no specific thread).
    private var isServerOnly: Bool { threadKey == nil }

    private var resolvedServerId: String? {
        threadKey?.serverId ?? serverId
    }

    @State private var renameText = ""
    @State private var isRenaming = false
    @State private var stats: AppConversationStats?

    private var thread: AppThreadSnapshot? {
        guard let threadKey else { return nil }
        return appModel.snapshot?.threads.first { $0.key == threadKey }
    }

    private var server: AppServerSnapshot? {
        guard let sid = resolvedServerId else { return nil }
        return appModel.snapshot?.servers.first { $0.serverId == sid }
    }

    private var allServerThreads: [AppThreadSnapshot] {
        guard let snapshot = appModel.snapshot, let sid = resolvedServerId else { return [] }
        return snapshot.threads.filter { $0.key.serverId == sid }
    }

    var body: some View {
        ScrollView {
            VStack(spacing: 0) {
                if !isServerOnly {
                    // Hero header
                    heroSection
                        .padding(.bottom, 20)

                    // Action buttons row (Telegram-style)
                    actionButtonsRow
                        .padding(.horizontal, 16)
                        .padding(.bottom, 24)

                    // Thin divider
                    Rectangle()
                        .fill(RemoraTheme.separator.opacity(0.4))
                        .frame(height: 0.5)
                        .padding(.horizontal, 24)
                        .padding(.bottom, 20)
                }

                // Content sections
                VStack(spacing: 16) {
                    if isServerOnly {
                        // Server-only mode: just wallpaper button at top
                        serverOnlyActionRow
                            .padding(.top, 8)
                    }
                    if !isServerOnly {
                        contextWindowSection
                        conversationStatsSection
                    }
                    if let rateLimits = server?.rateLimits {
                        rateLimitSection(rateLimits)
                    }
                    serverInfoSection
                }
                .padding(.horizontal, 16)
                .padding(.bottom, 40)
            }
        }
        .background(RemoraTheme.backgroundGradient)
        .navigationBarTitleDisplayMode(.inline)
        .toolbar {
            ToolbarItem(placement: .principal) {
                Text(isServerOnly ? "Server Info" : "Info")
                    .remoraFont(size: 16, weight: .semibold)
                    .foregroundStyle(RemoraTheme.textPrimary)
            }
        }
        .onAppear { computeData() }
        .onChange(of: thread?.hydratedConversationItems.count) { computeData() }
        .alert("Rename Thread", isPresented: $isRenaming) {
            TextField("Thread name", text: $renameText)
            Button("Save") { saveRename() }
            Button("Cancel", role: .cancel) { }
        }
    }

    // MARK: - Server-Only Action Row

    private var serverOnlyActionRow: some View {
        HStack(spacing: 0) {
            actionCircle(icon: "paintbrush", label: "Appearance") {
                onOpenWallpaper?()
            }
            if let onOpenShell {
                actionCircle(icon: "terminal", label: "Shell") {
                    onOpenShell()
                }
            }
        }
    }

    // MARK: - Hero Section

    private var heroSection: some View {
        VStack(spacing: 12) {
            // Status dot + title
            HStack(spacing: 8) {
                Circle()
                    .fill(statusColor)
                    .frame(width: 10, height: 10)
                Text(thread?.displayTitle ?? "Untitled session")
                    .remoraFont(size: 22, weight: .bold)
                    .foregroundStyle(RemoraTheme.textPrimary)
                    .lineLimit(2)
            }

            // Model + reasoning badges
            HStack(spacing: 8) {
                if let model = thread?.displayModelLabel,
                   !model.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
                    Text(model)
                        .remoraFont(size: 13, weight: .medium)
                        .foregroundStyle(RemoraTheme.accentForegroundOnSurface)
                        .padding(.horizontal, 10)
                        .padding(.vertical, 5)
                        .modifier(GlassRectModifier(cornerRadius: 8))
                }
                if let effort = thread?.reasoningEffort {
                    Text(effort)
                        .remoraFont(size: 12, weight: .regular)
                        .foregroundStyle(RemoraTheme.textSecondary)
                        .padding(.horizontal, 8)
                        .padding(.vertical, 5)
                        .modifier(GlassRectModifier(cornerRadius: 8))
                }
            }

            // Metadata row: cwd + timestamps
            VStack(spacing: 6) {
                if let cwd = thread?.info.cwd {
                    HStack(spacing: 5) {
                        Image(systemName: "folder.fill")
                            .font(.system(size: 10))
                            .foregroundStyle(RemoraTheme.textMuted)
                        Text(abbreviatePath(cwd))
                            .remoraFont(size: 12)
                            .foregroundStyle(RemoraTheme.textSecondary)
                            .lineLimit(1)
                            .truncationMode(.middle)
                    }
                }

                if let tid = threadKey?.threadId {
                    HStack(spacing: 5) {
                        Image(systemName: "number")
                            .font(.system(size: 10))
                            .foregroundStyle(RemoraTheme.textMuted)
                        Text(tid)
                            .remoraFont(size: 11)
                            .foregroundStyle(RemoraTheme.textSecondary)
                            .lineLimit(1)
                            .truncationMode(.middle)
                            .textSelection(.enabled)
                    }
                }

                HStack(spacing: 12) {
                    if let created = thread?.info.createdAt {
                        HStack(spacing: 3) {
                            Image(systemName: "clock")
                                .font(.system(size: 9))
                                .foregroundStyle(RemoraTheme.textMuted)
                            Text(relativeDate(created))
                                .remoraFont(size: 11)
                                .foregroundStyle(RemoraTheme.textMuted)
                        }
                    }
                    if let updated = thread?.info.updatedAt {
                        HStack(spacing: 3) {
                            Image(systemName: "arrow.clockwise")
                                .font(.system(size: 9))
                                .foregroundStyle(RemoraTheme.textMuted)
                            Text(relativeDate(updated))
                                .remoraFont(size: 11)
                                .foregroundStyle(RemoraTheme.textMuted)
                        }
                    }
                }
            }
        }
        .padding(.top, 16)
    }

    private func abbreviatePath(_ path: String) -> String {
        PathDisplay.display(path, isLocal: server?.isLocal == true)
    }

    // MARK: - Action Buttons Row (Telegram-style)

    private var actionButtonsRow: some View {
        HStack(spacing: 0) {
            actionCircle(icon: "paintbrush", label: "Appearance") {
                onOpenWallpaper?()
            }
            actionCircle(icon: "arrow.branch", label: "Fork") {
                Task { await forkConversation() }
            }
            actionCircle(icon: "pencil", label: "Rename") {
                renameText = thread?.info.title ?? ""
                isRenaming = true
            }
        }
    }

    private func actionCircle(icon: String, label: String, action: @escaping () -> Void) -> some View {
        Button(action: action) {
            VStack(spacing: 6) {
                Image(systemName: icon)
                    .font(.system(size: 16, weight: .medium))
                    .foregroundStyle(RemoraTheme.accentForegroundOnSurface)
                    .frame(width: 52, height: 52)
                    .modifier(GlassRectModifier(cornerRadius: 14))
                Text(label)
                    .remoraFont(size: 11, weight: .medium)
                    .foregroundStyle(RemoraTheme.textSecondary)
            }
        }
        .buttonStyle(.plain)
        .frame(maxWidth: .infinity)
    }

    private func timestampLabel(_ label: String, timestamp: Int64) -> some View {
        VStack(alignment: .leading, spacing: 2) {
            Text(label)
                .remoraFont(size: 10, weight: .medium)
                .foregroundStyle(RemoraTheme.textMuted)
            Text(relativeDate(timestamp))
                .remoraFont(size: 12)
                .foregroundStyle(RemoraTheme.textSecondary)
        }
    }

    private var statusColor: Color {
        switch thread?.info.status {
        case .active: return RemoraTheme.success
        case .idle: return RemoraTheme.textMuted
        case .systemError: return RemoraTheme.danger
        case .notLoaded: return RemoraTheme.textMuted
        default: return RemoraTheme.textMuted
        }
    }

    private var statusLabel: String {
        switch thread?.info.status {
        case .active: return "Active"
        case .idle: return "Idle"
        case .systemError: return "Error"
        case .notLoaded: return "Not Loaded"
        default: return "Unknown"
        }
    }

    // MARK: - Context Window

    private var contextWindowSection: some View {
        Group {
            if let used = thread?.contextTokensUsed, let window = thread?.modelContextWindow, window > 0 {
                let percent = Double(used) / Double(window)
                VStack(spacing: 8) {
                    HStack {
                        Text("Context Window")
                            .remoraFont(size: 14, weight: .semibold)
                            .foregroundStyle(RemoraTheme.textPrimary)
                        Spacer()
                        Text("\(Int(percent * 100))%")
                            .remoraFont(size: 14, weight: .bold)
                            .foregroundStyle(contextColor(percent: percent))
                    }

                    GeometryReader { geo in
                        ZStack(alignment: .leading) {
                            RoundedRectangle(cornerRadius: 4)
                                .fill(RemoraTheme.border)
                                .frame(height: 8)
                            RoundedRectangle(cornerRadius: 4)
                                .fill(contextColor(percent: percent))
                                .frame(width: geo.size.width * min(1, percent), height: 8)
                        }
                    }
                    .frame(height: 8)

                    HStack {
                        Text(formatTokens(used))
                            .remoraFont(size: 11)
                            .foregroundStyle(RemoraTheme.textMuted)
                        Spacer()
                        Text(formatTokens(window))
                            .remoraFont(size: 11)
                            .foregroundStyle(RemoraTheme.textMuted)
                    }
                }
                .padding(16)
                .modifier(GlassRectModifier(cornerRadius: 12))
            }
        }
    }

    private func contextColor(percent: Double) -> Color {
        if percent >= 0.8 { return RemoraTheme.danger }
        if percent >= 0.6 { return RemoraTheme.warning }
        return RemoraTheme.accent
    }

    private func formatTokens(_ tokens: UInt64) -> String {
        if tokens >= 1_000_000 {
            return String(format: "%.1fM", Double(tokens) / 1_000_000)
        } else if tokens >= 1_000 {
            return String(format: "%.1fK", Double(tokens) / 1_000)
        }
        return "\(tokens)"
    }

    // MARK: - Per-Conversation Stats

    private var conversationStatsSection: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("Conversation Stats")
                .remoraFont(size: 14, weight: .semibold)
                .foregroundStyle(RemoraTheme.textPrimary)

            LazyVGrid(columns: [
                GridItem(.flexible(), spacing: 12),
                GridItem(.flexible(), spacing: 12)
            ], spacing: 12) {
                statCard("Messages", value: "\(stats?.totalMessages ?? 0)", detail: "\(stats?.userMessageCount ?? 0) user · \(stats?.assistantMessageCount ?? 0) assistant")
                statCard("Turns", value: "\(stats?.turnCount ?? 0)")
                statCard("Commands", value: "\(stats?.commandsExecuted ?? 0)", detail: "\(stats?.commandsSucceeded ?? 0) ok · \(stats?.commandsFailed ?? 0) fail")
                statCard("Files Changed", value: "\(stats?.filesChanged ?? 0)", detail: "+\(stats?.diffAdditions ?? 0) / -\(stats?.diffDeletions ?? 0)")
                statCard("MCP Calls", value: "\(stats?.mcpToolCallCount ?? 0)")
                statCard("Exec Time", value: formatDuration(Int64(stats?.totalCommandDurationMs ?? 0)))
            }
        }
        .padding(16)
        .modifier(GlassRectModifier(cornerRadius: 12))
    }

    private func statCard(_ title: String, value: String, detail: String? = nil) -> some View {
        VStack(alignment: .leading, spacing: 4) {
            Text(value)
                .remoraFont(size: 20, weight: .bold)
                .foregroundStyle(RemoraTheme.accentForegroundOnSurface)
            Text(title)
                .remoraFont(size: 12, weight: .medium)
                .foregroundStyle(RemoraTheme.textSecondary)
            if let detail {
                Text(detail)
                    .remoraFont(size: 10)
                    .foregroundStyle(RemoraTheme.textMuted)
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(12)
        .modifier(GlassRectModifier(cornerRadius: 8))
    }

    private func formatDuration(_ ms: Int64) -> String {
        if ms < 1000 { return "\(ms)ms" }
        let secs = Double(ms) / 1000
        if secs < 60 { return String(format: "%.1fs", secs) }
        let mins = Int(secs / 60)
        let remainSecs = Int(secs) % 60
        return "\(mins)m \(remainSecs)s"
    }

    private func rateLimitSection(_ rateLimits: RateLimitSnapshot) -> some View {
        VStack(alignment: .leading, spacing: 16) {
            Text("Rate Limits")
                .remoraFont(size: 14, weight: .semibold)
                .foregroundStyle(RemoraTheme.textPrimary)
            rateLimitGauge(rateLimits)
        }
        .padding(16)
        .modifier(GlassRectModifier(cornerRadius: 12))
    }

    private func rateLimitGauge(_ rateLimits: RateLimitSnapshot) -> some View {
        VStack(alignment: .leading, spacing: 8) {
            Text("Rate Limits")
                .remoraFont(size: 12, weight: .medium)
                .foregroundStyle(RemoraTheme.textSecondary)

            HStack(spacing: 16) {
                if let primary = rateLimits.primary {
                    rateLimitRing(label: "Primary", window: primary)
                }
                if let secondary = rateLimits.secondary {
                    rateLimitRing(label: "Secondary", window: secondary)
                }
            }
        }
    }

    private func rateLimitRing(label: String, window: RateLimitWindow) -> some View {
        VStack(spacing: 6) {
            ZStack {
                Circle()
                    .stroke(RemoraTheme.border, lineWidth: 4)
                Circle()
                    .trim(from: 0, to: Double(window.usedPercent) / 100)
                    .stroke(rateLimitColor(percent: Int(window.usedPercent)), style: StrokeStyle(lineWidth: 4, lineCap: .round))
                    .rotationEffect(.degrees(-90))
                Text("\(window.usedPercent)%")
                    .remoraFont(size: 12, weight: .bold)
                    .foregroundStyle(RemoraTheme.textPrimary)
            }
            .frame(width: 56, height: 56)

            Text(label)
                .remoraFont(size: 10)
                .foregroundStyle(RemoraTheme.textMuted)
        }
    }

    private func rateLimitColor(percent: Int) -> Color {
        if percent >= 80 { return RemoraTheme.danger }
        if percent >= 60 { return RemoraTheme.warning }
        return RemoraTheme.accent
    }

    // MARK: - Section C: Server Info

    private var serverInfoSection: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("Server")
                .remoraFont(size: 14, weight: .semibold)
                .foregroundStyle(RemoraTheme.textPrimary)

            if let server {
                infoRow("Name", value: server.displayName)
                infoRow("Address", value: "\(server.host):\(server.port)")
                infoRow("Mode", value: server.connectionModeLabel)

                HStack(spacing: 6) {
                    Text("Health")
                        .remoraFont(size: 12)
                        .foregroundStyle(RemoraTheme.textMuted)
                    Spacer()
                    Circle()
                        .fill(healthColor(server.health))
                        .frame(width: 8, height: 8)
                    Text(healthLabel(server.health))
                        .remoraFont(size: 12)
                        .foregroundStyle(RemoraTheme.textSecondary)
                }

                if let account = server.account {
                    accountRow(account)
                }

                if let models = server.availableModels, !models.isEmpty {
                    VStack(alignment: .leading, spacing: 4) {
                        Text("Available Models")
                            .remoraFont(size: 12)
                            .foregroundStyle(RemoraTheme.textMuted)
                        ForEach(models.prefix(8), id: \.id) { model in
                            Text(model.displayName)
                                .remoraFont(size: 12)
                                .foregroundStyle(RemoraTheme.textSecondary)
                        }
                        if models.count > 8 {
                            Text("+\(models.count - 8) more")
                                .remoraFont(size: 11)
                                .foregroundStyle(RemoraTheme.textMuted)
                        }
                    }
                }

            }
        }
        .padding(16)
        .modifier(GlassRectModifier(cornerRadius: 12))
    }

    private func infoRow(_ label: String, value: String) -> some View {
        HStack {
            Text(label)
                .remoraFont(size: 12)
                .foregroundStyle(RemoraTheme.textMuted)
            Spacer()
            Text(value)
                .remoraFont(size: 12)
                .foregroundStyle(RemoraTheme.textSecondary)
        }
    }

    private func healthColor(_ health: AppServerHealth) -> Color {
        switch health {
        case .connected: return RemoraTheme.success
        case .connecting: return RemoraTheme.warning
        case .disconnected, .unresponsive: return RemoraTheme.danger
        case .unknown: return RemoraTheme.textMuted
        }
    }

    private func healthLabel(_ health: AppServerHealth) -> String {
        switch health {
        case .connected: return "Connected"
        case .connecting: return "Connecting"
        case .disconnected: return "Disconnected"
        case .unresponsive: return "Unresponsive"
        case .unknown: return "Unknown"
        }
    }

    private func accountRow(_ account: Account) -> some View {
        HStack {
            Text("Account")
                .remoraFont(size: 12)
                .foregroundStyle(RemoraTheme.textMuted)
            Spacer()
            switch account {
            case .apiKey:
                Text("API Key")
                    .remoraFont(size: 12)
                    .foregroundStyle(RemoraTheme.textSecondary)
            case .chatgpt(let email, let planType):
                VStack(alignment: .trailing, spacing: 2) {
                    Text(email)
                        .remoraFont(size: 12)
                        .foregroundStyle(RemoraTheme.textSecondary)
                    Text(planTypeLabel(planType))
                        .remoraFont(size: 10)
                        .foregroundStyle(RemoraTheme.textMuted)
                }
            }
        }
    }

    private func planTypeLabel(_ planType: PlanType) -> String {
        switch planType {
        case .free: return "Free"
        case .go: return "Go"
        case .plus: return "Plus"
        case .pro: return "Pro"
        case .team: return "Team"
        case .business: return "Business"
        case .enterprise: return "Enterprise"
        case .edu: return "Edu"
        case .unknown: return "Unknown"
        }
    }

    // (Actions are now in the hero section's actionButtonsRow)

    // MARK: - Actions

    private func forkConversation() async {
        guard let threadKey else { return }
        do {
            let sourceKey = await appModel.hydrateThreadPermissions(for: threadKey, appState: appState)
                ?? threadKey
            let newKey = try await appModel.client.forkThread(
                serverId: sourceKey.serverId,
                params: AppThreadLaunchConfig(
                    model: thread?.model,
                    approvalPolicy: appState.launchApprovalPolicy(for: sourceKey),
                    sandbox: appState.launchSandboxMode(for: sourceKey),
                    developerInstructions: nil,
                    persistExtendedHistory: true
                ).threadForkRequest(threadId: sourceKey.threadId, cwdOverride: thread?.info.cwd)
            )
            appModel.store.setActiveThread(key: newKey)
            await appModel.refreshThreadSnapshot(key: newKey)
            onOpenConversation?(newKey)
        } catch {
            LLog.error("info", "failed to fork thread", error: error)
        }
    }

    private func saveRename() {
        guard let threadKey else { return }
        let title = renameText.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !title.isEmpty else { return }
        isRenaming = false
        Task {
            do {
                try await appModel.renameThread(
                    serverId: threadKey.serverId,
                    threadId: threadKey.threadId,
                    title: title
                )
            } catch {
                LLog.error("info", "failed to rename thread", error: error)
            }
        }
    }

    private func computeData() {
        if let thread {
            stats = thread.stats
        }
    }
}
