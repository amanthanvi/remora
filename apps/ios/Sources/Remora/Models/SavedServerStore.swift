import Foundation

extension Notification.Name {
    static let remoraSavedServersDidChange = Notification.Name("remoraSavedServersDidChange")
}

enum SavedServerStoreError: LocalizedError {
    case invalidTrustCleanupJournal
    case persistenceFailed
    case trustCleanupAlreadyPending
    case trustCleanupPending(String)

    var errorDescription: String? {
        switch self {
        case .invalidTrustCleanupJournal:
            return "The pending SSH trust cleanup record is invalid."
        case .persistenceFailed:
            return "The saved-server change could not be persisted."
        case .trustCleanupAlreadyPending:
            return "A previous SSH trust cleanup is still pending."
        case .trustCleanupPending(let detail):
            return "The server change was saved, but SSH trust cleanup is pending: \(detail)"
        }
    }
}

@MainActor
enum SavedServerStore {
    /// The versioned namespace is itself part of the 1.6 hard cut: a stale
    /// pre-cutover record can never be loaded even if physical deletion of the
    /// retired defaults key is interrupted.
    static let savedServersKey = "remora.savedServers.v2"
    static let sshTrustCleanupJournalKey = "remora.savedServers.sshTrustCleanup.v1"
    static var retiredSavedServersKey: String {
        // One-release deletion tombstone for the unsupported pre-1.6 key.
        // Reconstruct it only to destroy old state; never load or migrate it.
        String(
            bytes: [
                99, 111, 100, 101, 120, 95, 115, 97,
                118, 101, 100, 95, 115, 101, 114, 118,
                101, 114, 115,
            ],
            encoding: .utf8
        )!
    }
    private static let currentFields: Set<String> = [
        "id",
        "name",
        "hostname",
        "port",
        "codexPorts",
        "sshPort",
        "source",
        "hasCodexServer",
        "wakeMAC",
        "preferredConnectionMode",
        "preferredCodexPort",
        "websocketURL",
        "rememberedByUser",
        "sshBridgeRuntimeKinds",
    ]

    private struct SSHTrustCleanupJournal: Codable {
        let servers: [SavedServer]
        let host: String
        let port: UInt16
        let originalFingerprint: String?
        let fingerprintRecorded: Bool

        private enum CodingKeys: String, CodingKey {
            case servers
            case host
            case port
            case originalFingerprint
        }

        init(
            servers: [SavedServer],
            host: String,
            port: UInt16,
            originalFingerprint: String?,
            fingerprintRecorded: Bool = true
        ) {
            self.servers = servers
            self.host = host
            self.port = port
            self.originalFingerprint = originalFingerprint
            self.fingerprintRecorded = fingerprintRecorded
        }

        init(from decoder: Decoder) throws {
            let container = try decoder.container(keyedBy: CodingKeys.self)
            servers = try container.decode([SavedServer].self, forKey: .servers)
            host = try container.decode(String.self, forKey: .host)
            port = try container.decode(UInt16.self, forKey: .port)
            fingerprintRecorded = container.contains(.originalFingerprint)
            originalFingerprint = try container.decodeIfPresent(
                String.self,
                forKey: .originalFingerprint
            )
        }

        func encode(to encoder: Encoder) throws {
            var container = encoder.container(keyedBy: CodingKeys.self)
            try container.encode(servers, forKey: .servers)
            try container.encode(host, forKey: .host)
            try container.encode(port, forKey: .port)
            if fingerprintRecorded {
                try container.encode(originalFingerprint, forKey: .originalFingerprint)
            }
        }
    }

    static func save(_ servers: [SavedServer], to defaults: UserDefaults = .standard) {
        do {
            let trustStore = TerminalSshTrustStore(backend: SwiftSshTrustBackend.shared)
            try save(
                servers,
                to: defaults,
                pinned: { try trustStore.pinned(host: $0, port: $1) },
                pin: { try trustStore.pin(host: $0, port: $1, fingerprint: $2) }
            )
        } catch {
            LLog.error("saved-servers", "save failed", error: error)
        }
    }

    static func save(
        _ servers: [SavedServer],
        to defaults: UserDefaults,
        pinned: (String, UInt16) throws -> String?,
        pin: (String, UInt16, String) throws -> Void
    ) throws {
        if let pending = try pendingTrustCleanup(from: defaults) {
            let refreshed = SSHTrustCleanupJournal(
                servers: servers,
                host: pending.host,
                port: pending.port,
                originalFingerprint: pending.originalFingerprint,
                fingerprintRecorded: pending.fingerprintRecorded
            )
            let journalData = try JSONEncoder().encode(refreshed)
            try persist(journalData, forKey: sshTrustCleanupJournalKey, to: defaults)
            if containsSSHTrustTarget(host: pending.host, port: pending.port, in: servers) {
                try restorePendingTrust(refreshed, pinned: pinned, pin: pin)
                try persistServers(servers, to: defaults)
                try clearTrustCleanupJournal(from: defaults)
                postSavedServersDidChange()
                return
            }
        }
        try persistServers(servers, to: defaults)
        postSavedServersDidChange()
    }

    static func load() -> [SavedServer] {
        let trustStore = TerminalSshTrustStore(backend: SwiftSshTrustBackend.shared)
        do {
            try resumePendingTrustCleanup(
                from: .standard,
                pinned: { try trustStore.pinned(host: $0, port: $1) },
                pin: { try trustStore.pin(host: $0, port: $1, fingerprint: $2) },
                unpin: { try trustStore.unpin(host: $0, port: $1) }
            )
        } catch {
            LLog.error("saved-servers", "pending SSH trust cleanup failed", error: error)
        }
        return load(from: .standard)
    }

    static func load(from defaults: UserDefaults) -> [SavedServer] {
        guard let data = defaults.data(forKey: savedServersKey) else { return [] }
        guard let objects = try? JSONSerialization.jsonObject(with: data) as? [Any] else {
            defaults.removeObject(forKey: savedServersKey)
            return []
        }
        let decoded = objects.compactMap { object -> SavedServer? in
            guard let fields = object as? [String: Any],
                  Set(fields.keys).isSubset(of: currentFields),
                  let recordData = try? JSONSerialization.data(withJSONObject: fields) else {
                return nil
            }
            return try? JSONDecoder().decode(SavedServer.self, from: recordData)
        }
        let normalized = decoded.compactMap { saved -> SavedServer? in
            guard saved.id != "local", saved.source != .local else { return nil }
            let server = saved.toDiscoveredServer()
            let restored = SavedServer
                .from(server, rememberedByUser: saved.rememberedByUser)
                .withSSHBridge(runtimeKinds: saved.sshBridgeRuntimeKinds)
            return restored
        }
        if decoded.count != objects.count || normalized != decoded {
            save(normalized, to: defaults)
        }
        return normalized
    }

    static func upsert(_ server: DiscoveredServer) {
        var saved = load(from: .standard)
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
        var saved = load(from: .standard)
        saved.removeAll { entry in matches(server, entry) }
        saved.append(SavedServer.from(server, rememberedByUser: true))
        save(saved)
    }

    static func rememberSSHBridge(_ server: DiscoveredServer, runtimeKinds: [AgentRuntimeKind]) {
        var saved = load(from: .standard)
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

    static func replace(_ server: SavedServer) throws {
        let trustStore = TerminalSshTrustStore(backend: SwiftSshTrustBackend.shared)
        try resumePendingTrustCleanup(
            from: .standard,
            pinned: { try trustStore.pinned(host: $0, port: $1) },
            pin: { try trustStore.pin(host: $0, port: $1, fingerprint: $2) },
            unpin: { try trustStore.unpin(host: $0, port: $1) }
        )
        try replace(
            server,
            from: .standard,
            pinned: { try trustStore.pinned(host: $0, port: $1) },
            unpin: { try trustStore.unpin(host: $0, port: $1) }
        )
    }

    static func replace(
        _ server: SavedServer,
        from defaults: UserDefaults,
        pinned: (String, UInt16) throws -> String? = { _, _ in nil },
        unpin: (String, UInt16) throws -> Void
    ) throws {
        guard try pendingTrustCleanup(from: defaults) == nil else {
            throw SavedServerStoreError.trustCleanupAlreadyPending
        }
        var saved = load(from: defaults)
        guard let index = saved.firstIndex(where: { $0.id == server.id }) else {
            saved.append(server)
            try persistServers(saved, to: defaults)
            postSavedServersDidChange()
            return
        }

        let previous = saved[index]
        saved[index] = server
        var cleanupTarget: (host: String, port: UInt16)?
        if let target = sshTrustTarget(for: previous) {
            let identity = sshTrustIdentity(host: target.host, port: target.port)
            let stillReferenced = saved
                .compactMap(sshTrustTarget)
                .contains { sshTrustIdentity(host: $0.host, port: $0.port) == identity }
            if !stillReferenced {
                cleanupTarget = target
            }
        }
        if let cleanupTarget {
            try commitTrustCleanup(
                servers: saved,
                target: cleanupTarget,
                to: defaults,
                pinned: pinned,
                unpin: unpin
            )
        } else {
            try persistServers(saved, to: defaults)
            postSavedServersDidChange()
        }
    }

    static func remove(serverId: String) throws {
        let trustStore = TerminalSshTrustStore(backend: SwiftSshTrustBackend.shared)
        try resumePendingTrustCleanup(
            from: .standard,
            pinned: { try trustStore.pinned(host: $0, port: $1) },
            pin: { try trustStore.pin(host: $0, port: $1, fingerprint: $2) },
            unpin: { try trustStore.unpin(host: $0, port: $1) }
        )
        try remove(
            serverId: serverId,
            from: .standard,
            pinned: { try trustStore.pinned(host: $0, port: $1) },
            unpin: { try trustStore.unpin(host: $0, port: $1) }
        )
    }

    static func remove(
        serverId: String,
        from defaults: UserDefaults,
        pinned: (String, UInt16) throws -> String? = { _, _ in nil },
        unpin: (String, UInt16) throws -> Void
    ) throws {
        guard try pendingTrustCleanup(from: defaults) == nil else {
            throw SavedServerStoreError.trustCleanupAlreadyPending
        }
        var saved = load(from: defaults)
        let removed = saved.first { $0.id == serverId }
        saved.removeAll { $0.id == serverId }
        var cleanupTarget: (host: String, port: UInt16)?
        if let target = removed.flatMap(sshTrustTarget) {
            let identity = sshTrustIdentity(host: target.host, port: target.port)
            let stillReferenced = saved
                .compactMap(sshTrustTarget)
                .contains { sshTrustIdentity(host: $0.host, port: $0.port) == identity }
            if !stillReferenced {
                cleanupTarget = target
            }
        }
        if let cleanupTarget {
            try commitTrustCleanup(
                servers: saved,
                target: cleanupTarget,
                to: defaults,
                pinned: pinned,
                unpin: unpin
            )
        } else {
            try persistServers(saved, to: defaults)
            postSavedServersDidChange()
        }
    }

    @discardableResult
    static func resumePendingTrustCleanup(
        from defaults: UserDefaults,
        pinned: (String, UInt16) throws -> String? = { _, _ in nil },
        pin: (String, UInt16, String) throws -> Void = { _, _, _ in },
        unpin: (String, UInt16) throws -> Void
    ) throws -> Bool {
        guard let journal = try pendingTrustCleanup(from: defaults) else { return false }
        do {
            if containsSSHTrustTarget(
                host: journal.host,
                port: journal.port,
                in: journal.servers
            ) {
                try restorePendingTrust(journal, pinned: pinned, pin: pin)
                try persistServers(journal.servers, to: defaults)
                postSavedServersDidChange()
                try clearTrustCleanupJournal(from: defaults)
                return true
            }
            try persistServers(journal.servers, to: defaults)
            postSavedServersDidChange()
            try unpin(journal.host, journal.port)
            try clearTrustCleanupJournal(from: defaults)
        } catch {
            throw SavedServerStoreError.trustCleanupPending(error.localizedDescription)
        }
        return true
    }

    @discardableResult
    static func removeAllForSecurityCutover(from defaults: UserDefaults = .standard) -> Bool {
        defaults.removeObject(forKey: savedServersKey)
        defaults.removeObject(forKey: retiredSavedServersKey)
        defaults.removeObject(forKey: sshTrustCleanupJournalKey)
        let synchronized = defaults.synchronize()
        let removed =
            defaults.object(forKey: savedServersKey) == nil
            && defaults.object(forKey: retiredSavedServersKey) == nil
            && defaults.object(forKey: sshTrustCleanupJournalKey) == nil
        NotificationCenter.default.post(name: .remoraSavedServersDidChange, object: nil)
        return synchronized && removed
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

    private static func sshTrustTarget(for server: SavedServer) -> (host: String, port: UInt16)? {
        let discovered = server.toDiscoveredServer()
        guard discovered.canConnectViaSSH else { return nil }
        return (server.hostname, discovered.resolvedSSHPort)
    }

    private static func sshTrustIdentity(
        host: String,
        port: UInt16
    ) -> String {
        "\(normalizedHost(host)):\(port)"
    }

    private static func containsSSHTrustTarget(
        host: String,
        port: UInt16,
        in servers: [SavedServer]
    ) -> Bool {
        let identity = sshTrustIdentity(host: host, port: port)
        return servers
            .compactMap(sshTrustTarget)
            .contains { sshTrustIdentity(host: $0.host, port: $0.port) == identity }
    }

    private static func commitTrustCleanup(
        servers: [SavedServer],
        target: (host: String, port: UInt16),
        to defaults: UserDefaults,
        pinned: (String, UInt16) throws -> String?,
        unpin: (String, UInt16) throws -> Void
    ) throws {
        let originalFingerprint = try pinned(target.host, target.port)
        let journal = SSHTrustCleanupJournal(
            servers: servers,
            host: target.host,
            port: target.port,
            originalFingerprint: originalFingerprint
        )
        let journalData = try JSONEncoder().encode(journal)
        try persist(journalData, forKey: sshTrustCleanupJournalKey, to: defaults)
        try persistServers(servers, to: defaults)
        postSavedServersDidChange()
        do {
            try unpin(target.host, target.port)
            try clearTrustCleanupJournal(from: defaults)
        } catch {
            throw SavedServerStoreError.trustCleanupPending(error.localizedDescription)
        }
    }

    private static func restorePendingTrust(
        _ journal: SSHTrustCleanupJournal,
        pinned: (String, UInt16) throws -> String?,
        pin: (String, UInt16, String) throws -> Void
    ) throws {
        guard journal.fingerprintRecorded else {
            throw SavedServerStoreError.invalidTrustCleanupJournal
        }
        guard let expected = journal.originalFingerprint else { return }
        let current = try pinned(journal.host, journal.port)
        guard current == nil || current == expected else {
            throw SavedServerStoreError.trustCleanupPending(
                "the SSH host fingerprint changed while cleanup was pending"
            )
        }
        guard current == nil else { return }

        do {
            try pin(journal.host, journal.port, expected)
        } catch {
            guard (try? pinned(journal.host, journal.port)) == expected else {
                throw error
            }
            return
        }
        guard try pinned(journal.host, journal.port) == expected else {
            throw SavedServerStoreError.trustCleanupPending(
                "the restored SSH host fingerprint could not be verified"
            )
        }
    }

    private static func pendingTrustCleanup(
        from defaults: UserDefaults
    ) throws -> SSHTrustCleanupJournal? {
        guard let data = defaults.data(forKey: sshTrustCleanupJournalKey) else { return nil }
        guard let journal = try? JSONDecoder().decode(SSHTrustCleanupJournal.self, from: data) else {
            throw SavedServerStoreError.invalidTrustCleanupJournal
        }
        return journal
    }

    private static func persistServers(
        _ servers: [SavedServer],
        to defaults: UserDefaults
    ) throws {
        let data = try JSONEncoder().encode(servers)
        try persist(data, forKey: savedServersKey, to: defaults)
    }

    private static func persist(
        _ data: Data,
        forKey key: String,
        to defaults: UserDefaults
    ) throws {
        defaults.set(data, forKey: key)
        guard defaults.synchronize(), defaults.data(forKey: key) == data else {
            throw SavedServerStoreError.persistenceFailed
        }
    }

    private static func clearTrustCleanupJournal(from defaults: UserDefaults) throws {
        defaults.removeObject(forKey: sshTrustCleanupJournalKey)
        guard defaults.synchronize(), defaults.object(forKey: sshTrustCleanupJournalKey) == nil else {
            throw SavedServerStoreError.persistenceFailed
        }
    }

    private static func postSavedServersDidChange() {
        NotificationCenter.default.post(name: .remoraSavedServersDidChange, object: nil)
    }

}
