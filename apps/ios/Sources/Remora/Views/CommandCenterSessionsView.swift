import SwiftUI

private enum CommandCenterSessionFilter: String, CaseIterable, Hashable {
    case all
    case needsYou
    case active
    case failed

    var title: String {
        switch self {
        case .all: "All"
        case .needsYou: "Needs You"
        case .active: "Active"
        case .failed: "Failed"
        }
    }

    var status: SessionStatusV1? {
        switch self {
        case .active: .running
        case .failed: .failed
        case .all, .needsYou: nil
        }
    }

    var attention: SessionAttentionV1? {
        self == .needsYou ? .needsYou : nil
    }
}

struct CommandCenterSessionsView: View {
    private static let pageLimit: UInt32 = 50

    @Environment(AppModel.self) private var appModel
    let serverId: String?
    let onOpenConversation: (ThreadKey) -> Void

    @State private var query = ""
    @State private var selectedFilter = CommandCenterSessionFilter.all
    @State private var rows: [SessionListRowV1] = []
    @State private var nextCursor: String?
    @State private var totalCount: UInt32 = 0
    @State private var isLoading = false
    @State private var errorMessage: String?
    @State private var requestGeneration: UInt64 = 0
    @State private var mutatingKeys: Set<ThreadKey> = []
    @State private var pendingArchive: SessionListRowV1?
    @State private var actionError: String?

    private var requestKey: String {
        "\(serverId ?? "all")|\(selectedFilter.rawValue)|\(query)"
    }

    var body: some View {
        VStack(spacing: 0) {
            searchField
            filterBar
            Divider().opacity(0.25)
            sessionsList
        }
        .background(RemoraTheme.backgroundGradient.ignoresSafeArea())
        .navigationTitle("Sessions")
        .navigationBarTitleDisplayMode(.inline)
        .task(id: requestKey) {
            let generation = resetRequest()
            if !query.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
                try? await Task.sleep(nanoseconds: 250_000_000)
                guard !Task.isCancelled else { return }
            }
            await loadPage(cursor: nil, generation: generation)
        }
        .confirmationDialog(
            "Archive Session?",
            isPresented: Binding(
                get: { pendingArchive != nil },
                set: { if !$0 { pendingArchive = nil } }
            ),
            titleVisibility: .visible
        ) {
            if let pendingArchive {
                Button("Archive", role: .destructive) {
                    Task { await archive(pendingArchive) }
                }
            }
            Button("Cancel", role: .cancel) { pendingArchive = nil }
        } message: {
            Text("This moves the session into archived history on its Host.")
        }
        .alert(
            "Session Action Failed",
            isPresented: Binding(
                get: { actionError != nil },
                set: { if !$0 { actionError = nil } }
            )
        ) {
            Button("OK") { actionError = nil }
        } message: {
            Text(actionError ?? "Try again after reconnecting the Host.")
        }
    }

    private var searchField: some View {
        HStack(spacing: 8) {
            Image(systemName: "magnifyingglass")
                .foregroundStyle(RemoraTheme.textMuted)
            TextField("Search sessions", text: $query)
                .textInputAutocapitalization(.never)
                .autocorrectionDisabled()
            if !query.isEmpty {
                Button {
                    query = ""
                } label: {
                    Image(systemName: "xmark.circle.fill")
                        .foregroundStyle(RemoraTheme.textMuted)
                }
                .buttonStyle(.plain)
                .accessibilityLabel("Clear search")
            }
        }
        .remoraFont(.body)
        .padding(.horizontal, 12)
        .frame(minHeight: RemoraAccessibilityMetrics.minimumHitTarget)
        .background(RemoraTheme.surface.opacity(0.78), in: RoundedRectangle(cornerRadius: 12))
        .padding(.horizontal, 14)
        .padding(.top, 8)
    }

    private var filterBar: some View {
        HStack(spacing: 8) {
            ScrollView(.horizontal, showsIndicators: false) {
                HStack(spacing: 8) {
                    ForEach(CommandCenterSessionFilter.allCases, id: \.self) { filter in
                        filterButton(filter)
                    }
                }
            }
            Spacer(minLength: 4)
            Text("\(rows.count)/\(totalCount)")
                .remoraMonoFont(size: 11, weight: .medium)
                .foregroundStyle(RemoraTheme.textMuted)
                .accessibilityLabel("Showing \(rows.count) of \(totalCount) sessions")
        }
        .padding(.horizontal, 14)
        .padding(.vertical, 8)
    }

    private func filterButton(_ filter: CommandCenterSessionFilter) -> some View {
        let isSelected = selectedFilter == filter
        return Button(filter.title) {
            selectedFilter = filter
        }
        .remoraFont(.caption)
        .foregroundStyle(isSelected ? RemoraTheme.textOnAccent : RemoraTheme.textSecondary)
        .padding(.horizontal, 10)
        .frame(minHeight: RemoraAccessibilityMetrics.minimumHitTarget)
        .background(isSelected ? RemoraTheme.accent : RemoraTheme.surface.opacity(0.72))
        .clipShape(Capsule())
        .buttonStyle(.plain)
        .accessibilityAddTraits(isSelected ? .isSelected : [])
    }

    @ViewBuilder
    private var sessionsList: some View {
        if rows.isEmpty, isLoading {
            ProgressView("Loading sessions…")
                .tint(RemoraTheme.accent)
                .foregroundStyle(RemoraTheme.textMuted)
                .frame(maxWidth: .infinity, maxHeight: .infinity)
        } else if rows.isEmpty {
            ContentUnavailableView(
                errorMessage == nil ? "No Sessions" : "Sessions Unavailable",
                systemImage: errorMessage == nil ? "tray" : "exclamationmark.triangle",
                description: Text(errorMessage ?? "No sessions match these filters.")
            )
            .foregroundStyle(RemoraTheme.textSecondary)
        } else {
            List {
                ForEach(rows, id: \.key) { row in
                    sessionRow(row)
                        .listRowBackground(RemoraTheme.surface.opacity(0.42))
                        .swipeActions(edge: .leading, allowsFullSwipe: false) {
                            if row.canAcknowledge {
                                Button("Acknowledge") {
                                    Task { await acknowledge(row) }
                                }
                                .tint(RemoraTheme.success)
                            }
                        }
                        .swipeActions(edge: .trailing, allowsFullSwipe: false) {
                            if row.archiveAvailability.state == .available {
                                Button("Archive", role: .destructive) {
                                    pendingArchive = row
                                }
                            }
                            if row.attention == .needsYou {
                                Button("Snooze 1h") {
                                    Task { await snooze(row) }
                                }
                                .tint(RemoraTheme.warning)
                            }
                        }
                        .disabled(mutatingKeys.contains(row.key))
                        .onAppear {
                            guard row.key == rows.last?.key, nextCursor != nil else { return }
                            Task { await loadNextPage() }
                        }
                }
                if isLoading {
                    HStack {
                        Spacer()
                        ProgressView().tint(RemoraTheme.accent)
                        Spacer()
                    }
                    .listRowBackground(Color.clear)
                } else if let errorMessage {
                    Button("Retry: \(errorMessage)") {
                        Task { await loadNextPage() }
                    }
                    .foregroundStyle(RemoraTheme.warning)
                    .listRowBackground(Color.clear)
                }
            }
            .listStyle(.plain)
            .scrollContentBackground(.hidden)
            .refreshable { await reloadImmediately() }
        }
    }

    private func sessionRow(_ row: SessionListRowV1) -> some View {
        Button {
            onOpenConversation(row.key)
        } label: {
            VStack(alignment: .leading, spacing: 6) {
                HStack(alignment: .firstTextBaseline, spacing: 8) {
                    Text(row.title)
                        .remoraFont(size: 14, weight: .semibold)
                        .foregroundStyle(RemoraTheme.textPrimary)
                        .lineLimit(1)
                    Spacer(minLength: 8)
                    statusLabel(row)
                }
                if let preview = row.preview, !preview.isEmpty {
                    Text(preview)
                        .remoraFont(.footnote)
                        .foregroundStyle(RemoraTheme.textSecondary)
                        .lineLimit(2)
                }
                HStack(spacing: 5) {
                    Text(row.hostLabel)
                    if let project = row.projectLabel {
                        Text("·")
                        Text(project)
                    }
                    Text("·")
                    Text(row.runtimeId)
                    if let updatedAtMs = row.updatedAtMs {
                        Text("·")
                        Text(relativeDate(updatedAtMs / 1_000))
                    }
                }
                .remoraMonoFont(size: 10, weight: .regular)
                .foregroundStyle(RemoraTheme.textMuted)
                .lineLimit(1)
            }
            .padding(.vertical, 6)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .accessibilityLabel("\(row.title), \(statusText(row))")
        .accessibilityHint("Open session")
    }

    private func statusLabel(_ row: SessionListRowV1) -> some View {
        Text(statusText(row))
            .remoraMonoFont(size: 10, weight: .semibold)
            .foregroundStyle(statusColor(row))
            .padding(.horizontal, 7)
            .padding(.vertical, 3)
            .background(statusColor(row).opacity(0.12), in: Capsule())
    }

    private func statusText(_ row: SessionListRowV1) -> String {
        if row.attention == .needsYou { return "Needs You" }
        if row.snoozedUntilMs != nil { return "Snoozed" }
        return switch row.status {
        case .running: "Running"
        case .waiting: "Waiting"
        case .failed: "Failed"
        case .idle: "Idle"
        case .unknown: "Unknown"
        }
    }

    private func statusColor(_ row: SessionListRowV1) -> Color {
        if row.attention == .needsYou { return RemoraTheme.warning }
        if row.snoozedUntilMs != nil { return RemoraTheme.accentForeground }
        return switch row.status {
        case .running: RemoraTheme.accentForeground
        case .failed: RemoraTheme.danger
        case .waiting: RemoraTheme.warning
        case .idle: RemoraTheme.textSecondary
        case .unknown: RemoraTheme.textMuted
        }
    }

    private func makeFilter() -> SessionFilterV1 {
        SessionFilterV1(
            query: query,
            serverId: serverId,
            projectLabel: nil,
            runtimeId: nil,
            status: selectedFilter.status,
            attention: selectedFilter.attention,
            updatedAfterMs: nil
        )
    }

    @MainActor
    private func resetRequest() -> UInt64 {
        requestGeneration &+= 1
        rows = []
        nextCursor = nil
        totalCount = 0
        isLoading = true
        errorMessage = nil
        return requestGeneration
    }

    @MainActor
    private func reloadImmediately() async {
        let generation = resetRequest()
        await loadPage(cursor: nil, generation: generation)
    }

    @MainActor
    private func loadNextPage() async {
        guard !isLoading, let cursor = nextCursor else { return }
        isLoading = true
        await loadPage(cursor: cursor, generation: requestGeneration)
    }

    @MainActor
    private func loadPage(cursor: String?, generation: UInt64) async {
        errorMessage = nil
        let filter = makeFilter()
        let store = appModel.store
        let limit = Self.pageLimit
        do {
            let page = try await Task.detached(priority: .userInitiated) {
                try store.sessionsPageFiltered(
                    filter: filter,
                    cursor: cursor,
                    limit: limit
                )
            }.value
            guard generation == requestGeneration, !Task.isCancelled else { return }
            let existing = Set(rows.map(\.key))
            rows.append(contentsOf: page.rows.filter { !existing.contains($0.key) })
            nextCursor = page.nextCursor
            totalCount = page.totalCount
        } catch {
            guard generation == requestGeneration, !Task.isCancelled else { return }
            errorMessage = error.localizedDescription
        }
        if generation == requestGeneration {
            isLoading = false
        }
    }

    @MainActor
    private func acknowledge(_ row: SessionListRowV1) async {
        await mutate(row) { store, key in
            try store.acknowledgeThreadAttention(key: key)
        }
    }

    @MainActor
    private func snooze(_ row: SessionListRowV1) async {
        await mutate(row) { store, key in
            try store.snoozeThreadAttention(key: key)
        }
    }

    @MainActor
    private func mutate(
        _ row: SessionListRowV1,
        action: @escaping @Sendable (AppStore, ThreadKey) throws -> Void
    ) async {
        guard mutatingKeys.insert(row.key).inserted else { return }
        defer { mutatingKeys.remove(row.key) }
        let store = appModel.store
        do {
            try await Task.detached(priority: .userInitiated) {
                try action(store, row.key)
            }.value
            await reloadImmediately()
        } catch {
            actionError = error.localizedDescription
        }
    }

    @MainActor
    private func archive(_ row: SessionListRowV1) async {
        pendingArchive = nil
        guard mutatingKeys.insert(row.key).inserted else { return }
        defer { mutatingKeys.remove(row.key) }
        do {
            if appModel.snapshot?.activeThread == row.key {
                appModel.activateThread(nil)
            }
            try await appModel.client.archiveThread(
                serverId: row.key.serverId,
                params: AppArchiveThreadRequest(threadId: row.key.threadId)
            )
            await appModel.refreshSnapshot()
            await reloadImmediately()
        } catch {
            actionError = error.localizedDescription
        }
    }
}
