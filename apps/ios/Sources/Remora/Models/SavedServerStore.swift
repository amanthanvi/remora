import Foundation

extension Notification.Name {
    static let remoraSavedServersDidChange = Notification.Name("remoraSavedServersDidChange")
}

@MainActor
enum SavedServerStore {
    static let savedServersKey = "codex_saved_servers"

    static func save(_ servers: [SavedServer], to defaults: UserDefaults = .standard) {
        guard let data = try? JSONEncoder().encode(servers) else { return }
        defaults.set(data, forKey: savedServersKey)
        NotificationCenter.default.post(name: .remoraSavedServersDidChange, object: nil)
    }

    static func load(from defaults: UserDefaults = .standard) -> [SavedServer] {
        guard let data = defaults.data(forKey: savedServersKey) else { return [] }
        let decoded = (try? JSONDecoder().decode([SavedServer].self, from: data)) ?? []
        let migrated = decoded.compactMap { saved -> SavedServer? in
            guard saved.id != "local", saved.source != .local else { return nil }
            guard !saved.containsLegacyV1Metadata
                || saved.sshBridgeRuntimeKinds != nil
                || saved.hasViableDirectOrSSHPath else {
                return nil
            }
            let server = saved.toDiscoveredServer()
            let restored = SavedServer
                .from(server, rememberedByUser: saved.rememberedByUser)
                .withSSHBridge(runtimeKinds: saved.sshBridgeRuntimeKinds)
            return restored
        }
        if migrated != decoded {
            save(migrated, to: defaults)
        }
        return migrated
    }

    static func upsert(_ server: DiscoveredServer) {
        var saved = load()
        let existing = existingMatch(for: server, in: saved)
        saved.removeAll { entry in matches(server, entry) }
        saved.append(
            SavedServer.from(
                server,
                rememberedByUser: existing?.rememberedByUser ?? false
            )
            .withSSHBridge(runtimeKinds: existing?.sshBridgeRuntimeKinds)
        )
        save(saved)
    }

    static func remember(_ server: DiscoveredServer) {
        var saved = load()
        saved.removeAll { entry in matches(server, entry) }
        saved.append(SavedServer.from(server, rememberedByUser: true))
        save(saved)
    }

    static func rememberSSHBridge(_ server: DiscoveredServer, runtimeKinds: [AgentRuntimeKind]) {
        var saved = load()
        saved.removeAll { entry in matches(server, entry) }
        saved.append(
            SavedServer
                .from(server, rememberedByUser: true)
                .withSSHBridge(runtimeKinds: runtimeKinds)
        )
        save(saved)
    }

    static func rememberedServers() -> [SavedServer] {
        load().filter(\.rememberedByUser)
    }

    static func reconnectRecords(rememberedOnly: Bool = false) -> [SavedServerRecord] {
        let saved = rememberedOnly ? rememberedServers() : load()
        return saved.map { $0.toRecord() }
    }

    static func remove(serverId: String) {
        var saved = load()
        saved.removeAll { $0.id == serverId }
        save(saved)
    }

    static func rename(serverId: String, newName: String) {
        var saved = load()
        guard let index = saved.firstIndex(where: { $0.id == serverId }) else { return }
        let old = saved[index]
        saved[index] = SavedServer(
            id: old.id,
            name: newName,
            hostname: old.hostname,
            port: old.port,
            codexPorts: old.codexPorts,
            sshPort: old.sshPort,
            source: old.source,
            hasCodexServer: old.hasCodexServer,
            wakeMAC: old.wakeMAC,
            preferredConnectionMode: old.preferredConnectionMode,
            preferredCodexPort: old.preferredCodexPort,
            sshPortForwardingEnabled: old.sshPortForwardingEnabled,
            websocketURL: old.websocketURL,
            rememberedByUser: old.rememberedByUser,
            sshBridgeRuntimeKinds: old.sshBridgeRuntimeKinds
        )
        save(saved)
    }

    static func updateWakeMAC(serverId: String, host: String, wakeMAC: String?) {
        guard let normalizedWakeMAC = DiscoveredServer.normalizeWakeMAC(wakeMAC) else { return }

        var saved = load()
        guard let index = saved.firstIndex(where: { entry in
            entry.id == serverId || normalizedHost(entry.hostname) == normalizedHost(host)
        }) else {
            return
        }

        let existing = saved[index]
        guard existing.wakeMAC != normalizedWakeMAC else { return }

        saved[index] = SavedServer(
            id: existing.id,
            name: existing.name,
            hostname: existing.hostname,
            port: existing.port,
            codexPorts: existing.codexPorts,
            sshPort: existing.sshPort,
            source: existing.source,
            hasCodexServer: existing.hasCodexServer,
            wakeMAC: normalizedWakeMAC,
            preferredConnectionMode: existing.preferredConnectionMode,
            preferredCodexPort: existing.preferredCodexPort,
            sshPortForwardingEnabled: existing.sshPortForwardingEnabled,
            websocketURL: existing.websocketURL,
            rememberedByUser: existing.rememberedByUser,
            sshBridgeRuntimeKinds: existing.sshBridgeRuntimeKinds
        )
        save(saved)
    }

    private static func existingMatch(for server: DiscoveredServer, in saved: [SavedServer]) -> SavedServer? {
        saved.first { matches(server, $0) }
    }

    private static func matches(_ server: DiscoveredServer, _ savedServer: SavedServer) -> Bool {
        savedServer.id == server.id || savedServer.toDiscoveredServer().deduplicationKey == server.deduplicationKey
    }

    private static func normalizedHost(_ host: String) -> String {
        var normalized = host
            .trimmingCharacters(in: .whitespacesAndNewlines)
            .trimmingCharacters(in: CharacterSet(charactersIn: "[]"))
            .replacingOccurrences(of: "%25", with: "%")

        if !normalized.contains(":"), let scopeIndex = normalized.firstIndex(of: "%") {
            normalized = String(normalized[..<scopeIndex])
        }

        return normalized.lowercased()
    }

}
