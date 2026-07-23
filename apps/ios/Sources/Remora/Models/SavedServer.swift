import Foundation

struct SavedServer: Codable, Identifiable, Equatable {
    let id: String
    let name: String
    let hostname: String
    let port: UInt16?
    let codexPorts: [UInt16]
    let sshPort: UInt16?
    let source: ServerSource
    let hasCodexServer: Bool
    let wakeMAC: String?
    let preferredConnectionMode: PreferredConnectionMode?
    let preferredCodexPort: UInt16?
    let websocketURL: String?
    let rememberedByUser: Bool
    /// `nil` is an ordinary direct/SSH server, `[]` probes all bridge runtimes,
    /// and a non-empty list reconnects only the selected bridge runtimes.
    let sshBridgeRuntimeKinds: [AgentRuntimeKind]?

    init(
        id: String,
        name: String,
        hostname: String,
        port: UInt16?,
        codexPorts: [UInt16],
        sshPort: UInt16?,
        source: ServerSource,
        hasCodexServer: Bool,
        wakeMAC: String?,
        preferredConnectionMode: PreferredConnectionMode?,
        preferredCodexPort: UInt16?,
        websocketURL: String?,
        rememberedByUser: Bool = false,
        sshBridgeRuntimeKinds: [AgentRuntimeKind]? = nil
    ) {
        self.id = id
        self.name = name
        self.hostname = hostname
        self.port = port
        self.codexPorts = codexPorts
        self.sshPort = sshPort
        self.source = source
        self.hasCodexServer = hasCodexServer
        self.wakeMAC = wakeMAC
        self.preferredConnectionMode = preferredConnectionMode
        self.preferredCodexPort = preferredCodexPort
        self.websocketURL = websocketURL
        self.rememberedByUser = rememberedByUser
        self.sshBridgeRuntimeKinds = sshBridgeRuntimeKinds
    }

    private enum CodingKeys: String, CodingKey {
        case id
        case name
        case hostname
        case port
        case codexPorts
        case sshPort
        case source
        case hasCodexServer
        case wakeMAC
        case preferredConnectionMode
        case preferredCodexPort
        case websocketURL
        case rememberedByUser
        case sshBridgeRuntimeKinds
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        let port = try container.decodeIfPresent(UInt16.self, forKey: .port)
        let hasCodexServer = try container.decode(Bool.self, forKey: .hasCodexServer)

        self.id = try container.decode(String.self, forKey: .id)
        self.name = try container.decode(String.self, forKey: .name)
        self.hostname = try container.decode(String.self, forKey: .hostname)
        self.port = port
        self.codexPorts = try container.decodeIfPresent([UInt16].self, forKey: .codexPorts)
            ?? (hasCodexServer ? (port.map { [$0] } ?? []) : [])
        self.sshPort = try container.decodeIfPresent(UInt16.self, forKey: .sshPort)
        self.source = try container.decode(ServerSource.self, forKey: .source)
        self.hasCodexServer = hasCodexServer
        self.wakeMAC = try container.decodeIfPresent(String.self, forKey: .wakeMAC)
        self.preferredConnectionMode = try container.decodeIfPresent(
            PreferredConnectionMode.self,
            forKey: .preferredConnectionMode
        )
        self.preferredCodexPort = try container.decodeIfPresent(UInt16.self, forKey: .preferredCodexPort)
        self.websocketURL = try container.decodeIfPresent(String.self, forKey: .websocketURL)
        self.rememberedByUser = try container.decodeIfPresent(Bool.self, forKey: .rememberedByUser) ?? true
        self.sshBridgeRuntimeKinds = try container
            .decodeIfPresent([AgentRuntimeKind].self, forKey: .sshBridgeRuntimeKinds)
            .map(Self.normalizedSSHBridgeRuntimeKinds)
    }

    func encode(to encoder: Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(id, forKey: .id)
        try container.encode(name, forKey: .name)
        try container.encode(hostname, forKey: .hostname)
        try container.encodeIfPresent(port, forKey: .port)
        try container.encode(codexPorts, forKey: .codexPorts)
        try container.encodeIfPresent(sshPort, forKey: .sshPort)
        try container.encode(source, forKey: .source)
        try container.encode(hasCodexServer, forKey: .hasCodexServer)
        try container.encodeIfPresent(wakeMAC, forKey: .wakeMAC)
        try container.encodeIfPresent(preferredConnectionMode, forKey: .preferredConnectionMode)
        try container.encodeIfPresent(preferredCodexPort, forKey: .preferredCodexPort)
        try container.encodeIfPresent(websocketURL, forKey: .websocketURL)
        try container.encode(rememberedByUser, forKey: .rememberedByUser)
        try container.encodeIfPresent(sshBridgeRuntimeKinds, forKey: .sshBridgeRuntimeKinds)
    }

    func toDiscoveredServer() -> DiscoveredServer {
        let codexPort = hasCodexServer ? (preferredCodexPort ?? port) : nil
        let resolvedSshPort = sshPort ?? (hasCodexServer ? nil : port)
        return DiscoveredServer(
            id: id,
            name: name,
            hostname: hostname,
            port: codexPort,
            codexPorts: resolvedCodexPorts,
            sshPort: resolvedSshPort,
            source: source,
            hasCodexServer: hasCodexServer,
            wakeMAC: wakeMAC,
            sshPortForwardingEnabled: false,
            websocketURL: websocketURL,
            preferredConnectionMode: preferredConnectionMode,
            preferredCodexPort: preferredCodexPort
        )
    }

    static func from(_ server: DiscoveredServer, rememberedByUser: Bool = false) -> SavedServer {
        SavedServer(
            id: server.id,
            name: server.name,
            hostname: server.hostname,
            port: server.port,
            codexPorts: server.codexPorts,
            sshPort: server.sshPort,
            source: server.source,
            hasCodexServer: server.hasCodexServer,
            wakeMAC: server.wakeMAC,
            preferredConnectionMode: server.preferredConnectionMode,
            preferredCodexPort: server.preferredCodexPort,
            websocketURL: server.websocketURL,
            rememberedByUser: rememberedByUser,
            sshBridgeRuntimeKinds: nil
        )
    }

    func withSSHBridge(runtimeKinds: [AgentRuntimeKind]?) -> SavedServer {
        SavedServer(
            id: id,
            name: name,
            hostname: hostname,
            port: port,
            codexPorts: codexPorts,
            sshPort: sshPort,
            source: source,
            hasCodexServer: hasCodexServer,
            wakeMAC: wakeMAC,
            preferredConnectionMode: preferredConnectionMode,
            preferredCodexPort: preferredCodexPort,
            websocketURL: websocketURL,
            rememberedByUser: rememberedByUser,
            sshBridgeRuntimeKinds: runtimeKinds
        )
    }

    func withName(_ name: String) -> SavedServer {
        SavedServer(
            id: id,
            name: name,
            hostname: hostname,
            port: port,
            codexPorts: codexPorts,
            sshPort: sshPort,
            source: source,
            hasCodexServer: hasCodexServer,
            wakeMAC: wakeMAC,
            preferredConnectionMode: preferredConnectionMode,
            preferredCodexPort: preferredCodexPort,
            websocketURL: websocketURL,
            rememberedByUser: rememberedByUser,
            sshBridgeRuntimeKinds: sshBridgeRuntimeKinds
        )
    }

    private var resolvedCodexPorts: [UInt16] {
        if !codexPorts.isEmpty {
            return codexPorts
        }
        if let port, hasCodexServer {
            return [port]
        }
        return []
    }

    func toRecord() -> SavedServerRecord {
        SavedServerRecord(
            id: id,
            name: name,
            hostname: hostname,
            port: port ?? 0,
            codexPorts: codexPorts,
            sshPort: sshPort,
            source: source.rawValue,
            hasCodexServer: hasCodexServer,
            wakeMac: wakeMAC,
            preferredConnectionMode: preferredConnectionMode?.rawValue,
            preferredCodexPort: preferredCodexPort,
            sshPortForwardingEnabled: nil,
            websocketUrl: websocketURL,
            rememberedByUser: rememberedByUser,
            sshBridgeRuntimeKinds: sshBridgeRuntimeKinds
        )
    }

    private static func normalizedSSHBridgeRuntimeKinds(
        _ runtimeKinds: [AgentRuntimeKind]
    ) -> [AgentRuntimeKind] {
        var seen: Set<String> = []
        return runtimeKinds.compactMap { raw -> AgentRuntimeKind? in
            let trimmed = raw.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
            let normalized: String
            switch trimmed {
            case "pi.dev", "pidev": normalized = "pi"
            case "ampcode", "amp-code", "amp_code", "amp code": normalized = "amp"
            case "open-code", "open_code", "open code": normalized = "opencode"
            case "claude-code", "claude_code", "claude code": normalized = "claude"
            case "factory", "factory-droid", "factory_droid", "factory droid": normalized = "droid"
            default: normalized = trimmed
            }
            guard !normalized.isEmpty, seen.insert(normalized).inserted else { return nil }
            return normalized
        }
    }
}
