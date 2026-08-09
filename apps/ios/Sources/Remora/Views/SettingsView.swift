import SwiftUI

struct SettingsView: View {
    @Environment(AppModel.self) private var appModel
    @Environment(AppState.self) private var appState
    @Environment(\.dismiss) private var dismiss
    @Environment(\.textScale) private var textScale
    @AppStorage("fontFamily") private var fontFamily = FontFamilyOption.mono.rawValue
    @AppStorage("collapseTurns") private var collapseTurns = false
    @AppStorage(ConversationDisplayPreferenceKey.reasoning) private var reasoningDisplayMode = ConversationDetailDisplayMode.collapsed.rawValue
    @AppStorage(ConversationDisplayPreferenceKey.commands) private var commandDisplayMode = ConversationDetailDisplayMode.collapsed.rawValue
    @AppStorage(ConversationDisplayPreferenceKey.tools) private var toolDisplayMode = ConversationDetailDisplayMode.collapsed.rawValue
    @State private var activeServerSheet: SettingsServerSheet?
    @State private var serverEditError: String?

    private var settingsObservation: AppModelSettingsObservation {
        let observation = appModel.settingsObservation
        _ = observation.revision
        return observation
    }

    private var accountServer: AppServerSnapshot? {
        let observation = settingsObservation
        if let activeServerId = observation.activeServerId,
           let activeServer = observation.servers.first(where: { $0.serverId == activeServerId }),
           activeServer.isConnected,
           !activeServer.isLocal {
            return activeServer
        }
        return observation.servers.first(where: { $0.isConnected && !$0.isLocal })
    }

    private var connectedServers: [HomeDashboardServer] {
        let observation = settingsObservation
        return HomeDashboardSupport.sortedConnectedServers(
            from: observation.servers,
            savedServers: SavedServerStore.rememberedServers(),
            activeServerId: observation.activeServerId
        )
    }

    var body: some View {
        NavigationStack {
            ZStack {
                RemoraTheme.backgroundGradient.ignoresSafeArea()
                Form {
                    supportSection
                    appearanceSection
                    fontSection
                    conversationSection
                    experimentalSection
                    accountSection
                    remoraLinkHostsSection
                    serversSection
                }
                .scrollContentBackground(.hidden)
            }
            .navigationTitle("Settings")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .topBarTrailing) {
                    Button("Done") { dismiss() }
                        .foregroundColor(RemoraTheme.accentForegroundOnSurface)
                }
            }
            .sheet(item: $activeServerSheet) { sheet in
                switch sheet {
                case .add:
                    NavigationStack {
                        DiscoveryView(onServerSelected: { _ in
                            activeServerSheet = nil
                        })
                    }
                    .environment(appModel)
                    .environment(appState)
                    .environment(\.textScale, textScale)
                case .edit(let server):
                    SettingsServerConnectionEditor(
                        server: server,
                        onSave: { configuration in
                            saveServerConfiguration(configuration, reconnect: false)
                            activeServerSheet = nil
                        },
                        onReconnect: { configuration in
                            activeServerSheet = nil
                            saveServerConfiguration(configuration, reconnect: true)
                        }
                    )
                    .environment(\.textScale, textScale)
                case .sshReconnect(let server):
                    SSHLoginSheet(server: server) { target in
                        activeServerSheet = nil
                        if case .sshThenRemote(let host, let credentials) = target {
                            Task { await reconnectViaSSH(server: server, host: host, credentials: credentials) }
                        }
                    }
                }
            }
            .alert("Server Update Failed", isPresented: Binding(
                get: { serverEditError != nil },
                set: { if !$0 { serverEditError = nil } }
            )) {
                Button("OK") { serverEditError = nil }
            } message: {
                Text(serverEditError ?? "Unable to update this server.")
            }
        }
    }

    // MARK: - Appearance Section

    private var appearanceSection: some View {
        Section {
            NavigationLink {
                AppearanceSettingsView()
            } label: {
                HStack(spacing: 10) {
                    Image(systemName: "paintbrush")
                        .foregroundColor(RemoraTheme.accentForegroundOnSurface)
                        .frame(width: 20)
                    Text("Appearance")
                        .remoraFont(.subheadline)
                        .foregroundColor(RemoraTheme.textPrimary)
                }
            }
            .listRowBackground(RemoraTheme.surface.opacity(0.6))
        } header: {
            Text("Theme")
                .foregroundColor(RemoraTheme.textSecondary)
        }
    }

    // MARK: - Conversation Section

    private var conversationSection: some View {
        Section {
            Toggle(isOn: $collapseTurns) {
                HStack(spacing: 10) {
                    Image(systemName: "rectangle.compress.vertical")
                        .foregroundColor(RemoraTheme.accentForegroundOnSurface)
                        .frame(width: 20)
                    VStack(alignment: .leading, spacing: 2) {
                        Text("Collapse Turns")
                            .remoraFont(.subheadline)
                            .foregroundColor(RemoraTheme.textPrimary)
                        Text("Collapse previous turns into cards")
                            .remoraFont(.caption)
                            .foregroundColor(RemoraTheme.textSecondary)
                    }
                }
            }
            .tint(RemoraTheme.accent)
            .listRowBackground(RemoraTheme.surface.opacity(0.6))

            transcriptDisplayPicker(
                title: "Internal Thinking",
                subtitle: "Reasoning and analysis blocks",
                systemImage: "brain.head.profile",
                selection: $reasoningDisplayMode
            )

            transcriptDisplayPicker(
                title: "Commands",
                subtitle: "Shell commands and command output",
                systemImage: "terminal",
                selection: $commandDisplayMode
            )

            transcriptDisplayPicker(
                title: "Tools",
                subtitle: "MCP, web, image, and file-change cards",
                systemImage: "wrench.and.screwdriver",
                selection: $toolDisplayMode
            )
        } header: {
            Text("Conversation")
                .foregroundColor(RemoraTheme.textSecondary)
        }
    }

    private func transcriptDisplayPicker(
        title: String,
        subtitle: String,
        systemImage: String,
        selection: Binding<String>
    ) -> some View {
        Picker(selection: selection) {
            ForEach(ConversationDetailDisplayMode.allCases) { mode in
                Text(mode.displayName).tag(mode.rawValue)
            }
        } label: {
            HStack(spacing: 10) {
                Image(systemName: systemImage)
                    .foregroundColor(RemoraTheme.accentForegroundOnSurface)
                    .frame(width: 20)
                VStack(alignment: .leading, spacing: 2) {
                    Text(title)
                        .remoraFont(.subheadline)
                        .foregroundColor(RemoraTheme.textPrimary)
                    Text(subtitle)
                        .remoraFont(.caption)
                        .foregroundColor(RemoraTheme.textSecondary)
                }
            }
        }
        .pickerStyle(.menu)
        .tint(RemoraTheme.accent)
        .listRowBackground(RemoraTheme.surface.opacity(0.6))
    }

    // MARK: - Font Section

    private var fontSection: some View {
        Section {
            ForEach(FontFamilyOption.allCases) { option in
                Button {
                    fontFamily = option.rawValue
                    ThemeManager.shared.syncFontPreference()
                } label: {
                    HStack {
                        VStack(alignment: .leading, spacing: 3) {
                            Text(option.displayName)
                                .remoraFont(.subheadline)
                                .foregroundColor(RemoraTheme.textPrimary)
                            Text("The quick brown fox")
                                .font(RemoraFont.sampleFont(family: option, size: 14))
                                .foregroundColor(RemoraTheme.textSecondary)
                        }
                        Spacer()
                        if fontFamily == option.rawValue {
                            Image(systemName: "checkmark")
                                .remoraFont(.subheadline, weight: .semibold)
                                .foregroundColor(RemoraTheme.accentForegroundOnSurface)
                        }
                    }
                }
                .listRowBackground(RemoraTheme.surface.opacity(0.6))
            }
        } header: {
            Text("Font")
                .foregroundColor(RemoraTheme.textSecondary)
        }
    }

    // MARK: - Experimental Section

    private var experimentalSection: some View {
        Section {
            NavigationLink {
                ExperimentalFeaturesView()
            } label: {
                HStack(spacing: 10) {
                    Image(systemName: "flask")
                        .foregroundColor(RemoraTheme.accentForegroundOnSurface)
                        .frame(width: 20)
                    Text("Experimental Features")
                        .remoraFont(.subheadline)
                        .foregroundColor(RemoraTheme.textPrimary)
                }
            }
            .listRowBackground(RemoraTheme.surface.opacity(0.6))
        } header: {
            Text("Experimental")
                .foregroundColor(RemoraTheme.textSecondary)
        }
    }

    // MARK: - Support Section

    private var supportSection: some View {
        Section {
            NavigationLink {
                TipJarView()
            } label: {
                HStack(spacing: 10) {
                    Image(systemName: "sparkles")
                        .foregroundColor(RemoraTheme.accentForegroundOnSurface)
                        .frame(width: 20)
                    Text("Support Remora")
                        .remoraFont(.subheadline)
                        .foregroundColor(RemoraTheme.textPrimary)
                }
            }
            .listRowBackground(RemoraTheme.surface.opacity(0.6))
        } header: {
            Text("Support")
                .foregroundColor(RemoraTheme.textSecondary)
        }
    }

    // MARK: - Account Section (inline, no nested sheet)

    private var accountSection: some View {
        Group {
            if let accountServer {
                SettingsConnectionAccountSection(server: accountServer)
            } else {
                SettingsDisconnectedAccountSection()
            }
        }
    }

    // MARK: - Remora Link Hosts

    private var remoraLinkHostsSection: some View {
        Section {
            NavigationLink {
                RemoraLinkHostsSettingsView(appModel: appModel)
            } label: {
                HStack(spacing: 10) {
                    Image(systemName: "link.badge.plus")
                        .foregroundColor(RemoraTheme.accentForegroundOnSurface)
                        .frame(width: 20)
                    VStack(alignment: .leading, spacing: 2) {
                        Text("Remora Link Hosts")
                            .remoraFont(.subheadline)
                            .foregroundColor(RemoraTheme.textPrimary)
                        Text("Pairing trust, revocation, and cleanup")
                            .remoraFont(.caption)
                            .foregroundColor(RemoraTheme.textSecondary)
                    }
                }
                .frame(minHeight: 44)
            }
            .accessibilityHint("Shows hosts paired through Remora Link")
            .listRowBackground(RemoraTheme.surface.opacity(0.6))
        } header: {
            Text("Remora Link")
                .foregroundColor(RemoraTheme.textSecondary)
        }
    }

    // MARK: - Servers Section

    private var serversSection: some View {
        Section {
            if connectedServers.isEmpty {
                Text("No servers connected")
                    .remoraFont(.footnote)
                    .foregroundColor(RemoraTheme.textMuted)
                    .listRowBackground(RemoraTheme.surface.opacity(0.6))
            } else {
                ForEach(connectedServers, id: \.id) { conn in
                    HStack {
                        Button {
                            activeServerSheet = .edit(conn)
                        } label: {
                            HStack {
                                Image(systemName: conn.isLocal ? "iphone" : "server.rack")
                                    .foregroundColor(RemoraTheme.accentForegroundOnSurface)
                                    .frame(width: 20)
                                VStack(alignment: .leading, spacing: 2) {
                                    Text(conn.displayName)
                                        .remoraFont(.footnote)
                                        .foregroundColor(RemoraTheme.textPrimary)
                                    Text(conn.health.displayLabel)
                                        .remoraFont(.caption)
                                        .foregroundColor(conn.health.accentColor)
                                }
                                Spacer()
                            }
                        }
                        .buttonStyle(.plain)
                        Button("Remove") {
                            removeServer(conn)
                        }
                        .remoraFont(.caption)
                        .foregroundColor(RemoraTheme.danger)
                        .buttonStyle(.borderless)
                    }
                    .listRowBackground(RemoraTheme.surface.opacity(0.6))
                }
            }

            Button {
                activeServerSheet = .add
            } label: {
                HStack {
                    Image(systemName: "plus.circle.fill")
                        .foregroundColor(RemoraTheme.accentForegroundOnSurface)
                        .frame(width: 20)
                    Text("Add Server")
                        .remoraFont(.footnote)
                        .foregroundColor(RemoraTheme.accentForegroundOnSurface)
                    Spacer()
                }
            }
            .listRowBackground(RemoraTheme.surface.opacity(0.6))
        } header: {
            Text("Servers")
                .foregroundColor(RemoraTheme.textSecondary)
        }
    }

    private func removeServer(_ server: HomeDashboardServer) {
        Task {
            guard let removalLease = await appModel.reconnectController.prepareServerRemoval(
                serverId: server.id
            ) else {
                serverEditError = "Unable to stop reconnecting to this server. Try again."
                return
            }
            await SshSessionStore.shared.close(serverId: server.id, ssh: appModel.ssh)
            do {
                try SavedServerStore.remove(serverId: server.id)
            } catch {
                guard savedServerMutationMayHaveCommitted(despite: error) else {
                    appModel.reconnectController.rollbackServerRemoval(
                        serverId: server.id,
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

    private func saveServerConfiguration(
        _ configuration: SettingsServerConnectionConfiguration,
        reconnect: Bool
    ) {
        do {
            try SavedServerStore.replace(configuration.savedServer)
        } catch {
            guard savedServerMutationMayHaveCommitted(despite: error) else { return }
        }
        appModel.reconnectController.allowServerReconnect(
            serverId: configuration.savedServer.id
        )
        appModel.reconnectController.syncSavedServers(
            servers: SavedServerStore.reconnectRecords()
        )
        appModel.store.renameServer(
            serverId: configuration.savedServer.id,
            displayName: configuration.savedServer.name
        )

        guard reconnect else { return }
        reconnectServer(using: configuration)
    }

    private func savedServerMutationMayHaveCommitted(despite error: Error) -> Bool {
        serverEditError = error.localizedDescription
        guard let storeError = error as? SavedServerStoreError else { return false }
        return storeError.mutationMayHaveCommitted
    }

    private func reconnectServer(using configuration: SettingsServerConnectionConfiguration) {
        let server = configuration.discoveredServer

        // For SSH we keep the existing connection alive until the user actually
        // submits credentials, so a cancelled credential sheet does not leave
        // them disconnected.
        if case .ssh = configuration.connectionMode {
            activeServerSheet = .sshReconnect(server)
            return
        }

        Task {
            await SshSessionStore.shared.close(serverId: server.id, ssh: appModel.ssh)
            appModel.serverBridge.disconnectServer(serverId: server.id)

            do {
                switch configuration.connectionMode {
                case .directCodex:
                    guard let port = server.resolvedDirectCodexPort else {
                        throw SettingsServerConnectionError.missingCodexPort
                    }
                    _ = try await appModel.serverBridge.connectRemoteServer(
                        serverId: server.id,
                        displayName: server.name,
                        host: server.hostname,
                        port: port
                    )
                    await appModel.refreshSnapshot()
                case .websocket:
                    guard let websocketURL = server.websocketURL else {
                        throw SettingsServerConnectionError.invalidWebsocketURL
                    }
                    if isSettingsSlingshotURL(websocketURL) {
                        let tokens = try await ChatGPTOAuth.loadStoredOrRefreshedTokens()
                        do {
                            _ = try await appModel.serverBridge.connectRemoteSlingshotUrlServer(
                                serverId: server.id,
                                displayName: server.name,
                                connectionUrl: websocketURL,
                                accessToken: tokens.accessToken,
                                accountId: tokens.accountID,
                                stepUpToken: ""
                            )
                        } catch {
                            guard ChatGPTOAuth.isRemoteControlAuthorizationRequired(error) else {
                                throw error
                            }
                            let stepUpToken = try await ChatGPTOAuth.remoteControlEnrollmentStepUpToken()
                            _ = try await appModel.serverBridge.connectRemoteSlingshotUrlServer(
                                serverId: server.id,
                                displayName: server.name,
                                connectionUrl: websocketURL,
                                accessToken: tokens.accessToken,
                                accountId: tokens.accountID,
                                stepUpToken: stepUpToken
                            )
                        }
                    } else {
                        _ = try await appModel.serverBridge.connectRemoteUrlServer(
                            serverId: server.id,
                            displayName: server.name,
                            websocketUrl: websocketURL
                        )
                    }
                    await appModel.refreshSnapshot()
                case .ssh:
                    break
                }
            } catch {
                serverEditError = error.localizedDescription
            }
        }
    }

    private func reconnectViaSSH(
        server: DiscoveredServer,
        host: String,
        credentials: SSHCredentials
    ) async {
        await SshSessionStore.shared.close(serverId: server.id, ssh: appModel.ssh)
        appModel.serverBridge.disconnectServer(serverId: server.id)

        do {
            _ = try await startRemoteOverSSH(
                serverId: server.id,
                displayName: server.name,
                host: host,
                port: server.resolvedSSHPort,
                credentials: credentials
            )
            await appModel.refreshSnapshot()
        } catch {
            serverEditError = error.localizedDescription
        }
    }

    private func startRemoteOverSSH(
        serverId: String,
        displayName: String,
        host: String,
        port: UInt16,
        credentials: SSHCredentials
    ) async throws -> String {
        switch credentials {
        case .password(let username, let password, let unlockMacosKeychain):
            return try await appModel.serverBridge.startRemoteOverSshConnect(
                serverId: serverId,
                displayName: displayName,
                host: host,
                port: port,
                username: username,
                password: password,
                privateKeyPem: nil,
                passphrase: nil,
                unlockMacosKeychain: unlockMacosKeychain,
                acceptUnknownHost: true,
                workingDir: nil
            )
        case .key(let username, let privateKey, let passphrase):
            return try await appModel.serverBridge.startRemoteOverSshConnect(
                serverId: serverId,
                displayName: displayName,
                host: host,
                port: port,
                username: username,
                password: nil,
                privateKeyPem: privateKey,
                passphrase: passphrase,
                unlockMacosKeychain: false,
                acceptUnknownHost: true,
                workingDir: nil
            )
        }
    }

}

private enum SettingsServerSheet: Identifiable {
    case add
    case edit(HomeDashboardServer)
    case sshReconnect(DiscoveredServer)

    var id: String {
        switch self {
        case .add:
            return "add"
        case .edit(let server):
            return "edit-\(server.id)"
        case .sshReconnect(let server):
            return "ssh-\(server.id)"
        }
    }
}

private enum SettingsServerConnectionMode: String, CaseIterable, Identifiable {
    case ssh
    case directCodex
    case websocket

    var id: String { rawValue }

    var label: String {
        switch self {
        case .ssh:
            return "SSH"
        case .directCodex:
            return "Codex"
        case .websocket:
            return "WebSocket"
        }
    }

    var formHeader: String {
        switch self {
        case .ssh:
            return "SSH Host"
        case .directCodex:
            return "Codex Server"
        case .websocket:
            return "Codex URL"
        }
    }
}

private enum SettingsServerConnectionError: LocalizedError {
    case emptyName
    case emptyHost
    case invalidCodexPort
    case missingCodexPort
    case invalidSSHPort
    case invalidWakeMAC
    case invalidWebsocketURL

    var errorDescription: String? {
        switch self {
        case .emptyName:
            return "Server name cannot be empty."
        case .emptyHost:
            return "Host cannot be empty."
        case .invalidCodexPort, .missingCodexPort:
            return "Codex port must be a valid number."
        case .invalidSSHPort:
            return "SSH port must be a valid number."
        case .invalidWakeMAC:
            return "Wake MAC must look like aa:bb:cc:dd:ee:ff."
        case .invalidWebsocketURL:
            return "Enter a valid ws:// or wss:// URL."
        }
    }
}

private struct SettingsServerConnectionConfiguration {
    let savedServer: SavedServer
    let discoveredServer: DiscoveredServer
    let connectionMode: SettingsServerConnectionMode
}

private struct SettingsServerConnectionEditor: View {
    let server: HomeDashboardServer
    let onSave: (SettingsServerConnectionConfiguration) -> Void
    let onReconnect: (SettingsServerConnectionConfiguration) -> Void

    @Environment(\.dismiss) private var dismiss
    @State private var displayName: String
    @State private var connectionMode: SettingsServerConnectionMode
    @State private var host: String
    @State private var codexPort: String
    @State private var websocketURL: String
    @State private var sshPort: String
    @State private var wakeMAC: String
    @State private var validationError: String?

    private let originalSavedServer: SavedServer?

    @MainActor
    init(
        server: HomeDashboardServer,
        onSave: @escaping (SettingsServerConnectionConfiguration) -> Void,
        onReconnect: @escaping (SettingsServerConnectionConfiguration) -> Void
    ) {
        self.server = server
        self.onSave = onSave
        self.onReconnect = onReconnect

        let saved = SavedServerStore.load().first { $0.id == server.id }
        self.originalSavedServer = saved

        let resolvedMode: SettingsServerConnectionMode
        if saved?.websocketURL != nil {
            resolvedMode = .websocket
        } else if saved?.preferredConnectionMode == .ssh || saved?.sshPort != nil && saved?.hasCodexServer == false {
            resolvedMode = .ssh
        } else {
            resolvedMode = .directCodex
        }

        let name = saved?.name.trimmingCharacters(in: .whitespacesAndNewlines)
        let resolvedHost = saved?.hostname.trimmingCharacters(in: .whitespacesAndNewlines)
        let resolvedCodexPort = saved?.preferredCodexPort ?? saved?.port ?? (server.port == 0 ? nil : server.port)
        let resolvedSSHPort = saved?.sshPort ?? (resolvedMode == .ssh ? server.port : nil) ?? 22

        _displayName = State(initialValue: name.flatMap { $0.isEmpty ? nil : $0 } ?? server.displayName)
        _connectionMode = State(initialValue: resolvedMode)
        _host = State(initialValue: resolvedHost.flatMap { $0.isEmpty ? nil : $0 } ?? server.host)
        _codexPort = State(initialValue: resolvedCodexPort.map(String.init) ?? "8390")
        _websocketURL = State(initialValue: saved?.websocketURL ?? "")
        _sshPort = State(initialValue: String(resolvedSSHPort))
        _wakeMAC = State(initialValue: saved?.wakeMAC ?? "")
    }

    private var availableModes: [SettingsServerConnectionMode] {
        [.ssh, .directCodex, .websocket]
    }

    private var isSpecialPairedServer: Bool {
        originalSavedServer?.sshBridgeRuntimeKinds != nil
    }

    var body: some View {
        NavigationStack {
            ZStack {
                RemoraTheme.backgroundGradient.ignoresSafeArea()
                Form {
                    nameSection
                    connectionSection
                    actionSection
                }
                .scrollContentBackground(.hidden)
            }
            .navigationTitle("Edit Server")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .topBarTrailing) {
                    Button("Cancel") { dismiss() }
                        .foregroundColor(RemoraTheme.accentForegroundOnSurface)
                }
            }
            .alert("Invalid Server", isPresented: Binding(
                get: { validationError != nil },
                set: { if !$0 { validationError = nil } }
            )) {
                Button("OK") { validationError = nil }
            } message: {
                Text(validationError ?? "Check the server details.")
            }
        }
    }

    private var nameSection: some View {
        Section {
            TextField("Server name", text: $displayName)
                .remoraFont(.footnote)
                .foregroundColor(RemoraTheme.textPrimary)
        } header: {
            Text("Name")
                .foregroundColor(RemoraTheme.textSecondary)
        }
        .listRowBackground(RemoraTheme.surface.opacity(0.6))
    }

    private var connectionSection: some View {
        Section {
            if isSpecialPairedServer {
                Text("This paired server uses saved pairing metadata. Edit its display name here, or remove and add it again to change the pairing.")
                    .remoraFont(.caption)
                    .foregroundColor(RemoraTheme.textSecondary)
            } else {
                Picker("Connection Type", selection: $connectionMode) {
                    ForEach(availableModes) { mode in
                        Text(mode.label).tag(mode)
                    }
                }
                .pickerStyle(.segmented)

                switch connectionMode {
                case .ssh:
                    hostField
                    TextField("ssh port", text: $sshPort)
                        .remoraFont(.footnote)
                        .foregroundColor(RemoraTheme.textPrimary)
                        .keyboardType(.numberPad)
                    TextField("wake MAC (optional)", text: $wakeMAC)
                        .remoraFont(.footnote)
                        .foregroundColor(RemoraTheme.textPrimary)
                        .textInputAutocapitalization(.never)
                        .autocorrectionDisabled(true)
                case .directCodex:
                    hostField
                    TextField("codex port", text: $codexPort)
                        .remoraFont(.footnote)
                        .foregroundColor(RemoraTheme.textPrimary)
                        .keyboardType(.numberPad)
                case .websocket:
                    TextField("ws://host:port or wss://...", text: $websocketURL)
                        .remoraFont(.footnote)
                        .foregroundColor(RemoraTheme.textPrimary)
                        .textInputAutocapitalization(.never)
                        .autocorrectionDisabled(true)
                        .keyboardType(.URL)
                }
            }
        } header: {
            Text(connectionMode.formHeader)
                .foregroundColor(RemoraTheme.textSecondary)
        } footer: {
            if !isSpecialPairedServer, connectionMode == .websocket {
                Text("Prefer SSH when possible. If you run codex manually, bind loopback and tunnel it yourself; do not expose it directly to the internet unless you know what you are doing.")
                    .remoraFont(.caption2)
                    .foregroundColor(RemoraTheme.textMuted)
            }
        }
        .listRowBackground(RemoraTheme.surface.opacity(0.6))
    }

    private var hostField: some View {
        TextField("hostname or IP", text: $host)
            .remoraFont(.footnote)
            .foregroundColor(RemoraTheme.textPrimary)
            .textInputAutocapitalization(.never)
            .autocorrectionDisabled(true)
    }

    private var actionSection: some View {
        Section {
            Button("Save") {
                submit(reconnect: false)
            }
            .foregroundColor(RemoraTheme.accentForegroundOnSurface)
            .remoraFont(.subheadline)

            if !isSpecialPairedServer {
                Button("Save & Reconnect") {
                    submit(reconnect: true)
                }
                .foregroundColor(RemoraTheme.accentForegroundOnSurface)
                .remoraFont(.subheadline)
            }
        }
        .listRowBackground(RemoraTheme.surface.opacity(0.6))
    }

    private func submit(reconnect: Bool) {
        do {
            let configuration = try buildConfiguration()
            if reconnect {
                onReconnect(configuration)
            } else {
                onSave(configuration)
            }
        } catch {
            validationError = error.localizedDescription
        }
    }

    private func buildConfiguration() throws -> SettingsServerConnectionConfiguration {
        let name = displayName.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !name.isEmpty else { throw SettingsServerConnectionError.emptyName }

        if isSpecialPairedServer, let originalSavedServer {
            let updated = originalSavedServer.withName(name)
            return SettingsServerConnectionConfiguration(
                savedServer: updated,
                discoveredServer: updated.toDiscoveredServer(),
                connectionMode: connectionMode
            )
        }

        switch connectionMode {
        case .ssh:
            let resolvedHost = try validatedHost()
            let resolvedWakeMAC = try validatedWakeMAC()
            guard let resolvedSSHPort = UInt16(sshPort.trimmingCharacters(in: .whitespacesAndNewlines)) else {
                throw SettingsServerConnectionError.invalidSSHPort
            }
            let saved = SavedServer(
                id: server.id,
                name: name,
                hostname: resolvedHost,
                port: nil,
                codexPorts: [],
                sshPort: resolvedSSHPort,
                source: .manual,
                hasCodexServer: false,
                wakeMAC: resolvedWakeMAC,
                preferredConnectionMode: .ssh,
                preferredCodexPort: nil,
                websocketURL: nil,
                rememberedByUser: true
            )
            return SettingsServerConnectionConfiguration(
                savedServer: saved,
                discoveredServer: saved.toDiscoveredServer(),
                connectionMode: .ssh
            )
        case .directCodex:
            let resolvedHost = try validatedHost()
            guard let resolvedCodexPort = UInt16(codexPort.trimmingCharacters(in: .whitespacesAndNewlines)) else {
                throw SettingsServerConnectionError.invalidCodexPort
            }
            let saved = SavedServer(
                id: server.id,
                name: name,
                hostname: resolvedHost,
                port: resolvedCodexPort,
                codexPorts: [resolvedCodexPort],
                sshPort: nil,
                source: .manual,
                hasCodexServer: true,
                wakeMAC: nil,
                preferredConnectionMode: .directCodex,
                preferredCodexPort: resolvedCodexPort,
                websocketURL: nil,
                rememberedByUser: true
            )
            return SettingsServerConnectionConfiguration(
                savedServer: saved,
                discoveredServer: saved.toDiscoveredServer(),
                connectionMode: .directCodex
            )
        case .websocket:
            let rawURL = websocketURL.trimmingCharacters(in: .whitespacesAndNewlines)
            guard let url = URL(string: rawURL),
                  let scheme = url.scheme?.lowercased(),
                  (scheme == "ws" || scheme == "wss"),
                  let resolvedHost = url.host,
                  !resolvedHost.isEmpty else {
                throw SettingsServerConnectionError.invalidWebsocketURL
            }
            let resolvedPort = url.port.flatMap { UInt16(exactly: $0) }
            let saved = SavedServer(
                id: server.id,
                name: name,
                hostname: resolvedHost,
                port: resolvedPort,
                codexPorts: resolvedPort.map { [$0] } ?? [],
                sshPort: nil,
                source: .manual,
                hasCodexServer: true,
                wakeMAC: nil,
                preferredConnectionMode: .directCodex,
                preferredCodexPort: resolvedPort,
                websocketURL: rawURL,
                rememberedByUser: true
            )
            return SettingsServerConnectionConfiguration(
                savedServer: saved,
                discoveredServer: saved.toDiscoveredServer(),
                connectionMode: .websocket
            )
        }
    }

    private func validatedHost() throws -> String {
        let resolvedHost = host.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !resolvedHost.isEmpty else { throw SettingsServerConnectionError.emptyHost }
        return resolvedHost
    }

    private func validatedWakeMAC() throws -> String? {
        let wakeInput = wakeMAC.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !wakeInput.isEmpty else { return nil }
        guard let normalized = DiscoveredServer.normalizeWakeMAC(wakeInput) else {
            throw SettingsServerConnectionError.invalidWakeMAC
        }
        return normalized
    }
}

private struct SettingsConnectionAccountSection: View {
    @Environment(AppModel.self) private var appModel
    let server: AppServerSnapshot
    @State private var isAuthWorking = false
    @State private var authError: String?

    var body: some View {
        Section {
            HStack(spacing: 12) {
                Circle()
                    .fill(authColor)
                    .frame(width: 10, height: 10)
                VStack(alignment: .leading, spacing: 2) {
                    Text(authTitle)
                        .remoraFont(.subheadline)
                        .foregroundColor(RemoraTheme.textPrimary)
                    if let sub = authSubtitle {
                        Text(sub)
                            .remoraFont(.caption)
                            .foregroundColor(RemoraTheme.textSecondary)
                    }
                    Text(server.displayName)
                        .remoraFont(.caption)
                        .foregroundColor(RemoraTheme.textMuted)
                }
                Spacer()
                if server.account != nil {
                    Button("Logout") {
                        Task { await logout() }
                    }
                    .remoraFont(.caption)
                    .foregroundColor(RemoraTheme.danger)
                }
            }
            .listRowBackground(RemoraTheme.surface.opacity(0.6))

            if !isChatGPTAccount {
                Button {
                    Task {
                        isAuthWorking = true
                        await loginWithChatGPT()
                        isAuthWorking = false
                    }
                } label: {
                    HStack {
                        if isAuthWorking {
                            ProgressView().tint(RemoraTheme.textPrimary).scaleEffect(0.8)
                        }
                        Image(systemName: "person.crop.circle.badge.checkmark")
                        Text("Login with ChatGPT")
                            .remoraFont(.subheadline)
                    }
                    .foregroundColor(RemoraTheme.accentForegroundOnSurface)
                }
                .disabled(isAuthWorking)
                .listRowBackground(RemoraTheme.surface.opacity(0.6))
            }

            if let authError {
                Text(authError)
                    .remoraFont(.caption)
                    .foregroundColor(RemoraTheme.danger)
                    .listRowBackground(RemoraTheme.surface.opacity(0.6))
            }
        } header: {
            Text("Account")
                .foregroundColor(RemoraTheme.textSecondary)
        }
        .task(id: server.serverId) {
            await refreshAccount()
        }
    }

    private var isChatGPTAccount: Bool {
        if case .chatgpt? = server.account {
            return true
        }
        return false
    }

    private var authColor: Color {
        switch server.account {
        case .chatgpt?:
            return RemoraTheme.accent
        case .apiKey?:
            return Color(hex: "#00AAFF")
        case nil:
            return RemoraTheme.textMuted
        }
    }

    private var authTitle: String {
        switch server.account {
        case .chatgpt(let email, _)?:
            return email.isEmpty ? "ChatGPT" : email
        case .apiKey?:
            return "API Key"
        case nil:
            return "Not logged in"
        }
    }

    private var authSubtitle: String? {
        switch server.account {
        case .chatgpt?:
            return "ChatGPT account"
        case .apiKey?:
            return "OpenAI API key"
        case nil:
            return nil
        }
    }

    private func loginWithChatGPT() async {
        do {
            authError = nil
            try await appModel.loginChatGPTAccount(serverId: server.serverId)
        } catch ChatGPTOAuthError.cancelled {
            return
        } catch {
            authError = error.localizedDescription
        }
    }

    private func refreshAccount() async {
        do {
            _ = try await appModel.client.refreshAccount(
                serverId: server.serverId,
                params: AppRefreshAccountRequest(refreshToken: false)
            )
            await appModel.refreshSnapshot()
            authError = nil
        } catch {
            authError = error.localizedDescription
        }
    }

    private func logout() async {
        do {
            _ = try await appModel.client.logoutAccount(serverId: server.serverId)
            await appModel.refreshSnapshot()
            authError = nil
        } catch {
            authError = error.localizedDescription
        }
    }
}

private struct SettingsDisconnectedAccountSection: View {
    var body: some View {
        Section {
            Text("Connect a remote server to manage its ChatGPT account.")
                .remoraFont(.caption)
                .foregroundColor(RemoraTheme.textMuted)
                .listRowBackground(RemoraTheme.surface.opacity(0.6))
        } header: {
            Text("Account")
                .foregroundColor(RemoraTheme.textSecondary)
        }
    }
}

private func isSettingsSlingshotURL(_ rawURL: String) -> Bool {
    URL(string: rawURL)?.scheme?.lowercased() == "slingshot"
}

#if DEBUG
#Preview("Settings") {
    RemoraPreviewScene(includeBackground: false) {
        SettingsView()
    }
}
#endif
