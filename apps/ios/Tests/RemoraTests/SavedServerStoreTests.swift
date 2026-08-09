import XCTest
@testable import Remora

@MainActor
final class SavedServerStoreTests: XCTestCase {
    func testCurrentPersistenceRoundTripsDirectAndSSHServers() throws {
        let (defaults, suiteName) = try makeDefaults()
        defer { defaults.removePersistentDomain(forName: suiteName) }

        let direct = makeServer(
            id: "direct",
            hostname: "direct.local",
            port: 8_390,
            sshPort: nil,
            hasCodexServer: true
        )
        let ssh = makeServer(
            id: "ssh",
            hostname: "ssh.local",
            port: nil,
            sshPort: 22,
            hasCodexServer: false
        )

        SavedServerStore.save([direct, ssh], to: defaults)

        XCTAssertEqual(SavedServerStore.load(from: defaults), [direct, ssh])
        let data = try XCTUnwrap(defaults.data(forKey: SavedServerStore.savedServersKey))
        let objects = try XCTUnwrap(JSONSerialization.jsonObject(with: data) as? [[String: Any]])
        XCTAssertEqual(objects.count, 2)
        XCTAssertEqual(Set(objects.flatMap(\.keys)).isSubset(of: Set([
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
        ])), true)
    }

    func testRetiredAndUnknownRecordShapesAreDiscardedWhileCurrentRecordsSurvive() throws {
        let (defaults, suiteName) = try makeDefaults()
        defer { defaults.removePersistentDomain(forName: suiteName) }
        let supported = makeServer(
            id: "supported",
            hostname: "supported.local",
            port: nil,
            sshPort: 22,
            hasCodexServer: false
        )
        let supportedData = try JSONEncoder().encode(supported)
        var unsupported = try XCTUnwrap(
            JSONSerialization.jsonObject(with: supportedData) as? [String: Any]
        )
        unsupported["unsupportedField"] = "discard"
        var retired = try XCTUnwrap(
            JSONSerialization.jsonObject(with: supportedData) as? [String: Any]
        )
        retired["sshPortForwardingEnabled"] = true
        let payload = try JSONSerialization.data(withJSONObject: [
            try XCTUnwrap(JSONSerialization.jsonObject(with: supportedData)),
            unsupported,
            retired,
        ])
        defaults.set(payload, forKey: SavedServerStore.savedServersKey)

        XCTAssertEqual(SavedServerStore.load(from: defaults), [supported])
        let rewritten = try XCTUnwrap(defaults.data(forKey: SavedServerStore.savedServersKey))
        let records = try XCTUnwrap(JSONSerialization.jsonObject(with: rewritten) as? [[String: Any]])
        XCTAssertEqual(records.count, 1)
        XCTAssertNil(records[0]["unsupportedField"])
    }

    func testNonArrayPayloadIsDiscarded() throws {
        let (defaults, suiteName) = try makeDefaults()
        defer { defaults.removePersistentDomain(forName: suiteName) }
        defaults.set(Data(#"{"servers":[]}"#.utf8), forKey: SavedServerStore.savedServersKey)

        XCTAssertEqual(SavedServerStore.load(from: defaults), [])
        XCTAssertNil(defaults.data(forKey: SavedServerStore.savedServersKey))
    }

    func testRetiredNamespaceIsNeverLoaded() throws {
        let (defaults, suiteName) = try makeDefaults()
        defer { defaults.removePersistentDomain(forName: suiteName) }
        let server = makeServer(
            id: "retired",
            hostname: "retired.local",
            port: 8_390,
            sshPort: nil,
            hasCodexServer: true
        )
        defaults.set(
            try JSONEncoder().encode([server]),
            forKey: SavedServerStore.retiredSavedServersKey
        )

        XCTAssertEqual(SavedServerStore.load(from: defaults), [])
    }

    func testSecurityCutoverDurablyRemovesCurrentAndRetiredNamespaces() throws {
        let (defaults, suiteName) = try makeDefaults()
        defer { defaults.removePersistentDomain(forName: suiteName) }
        let payload = Data("retained".utf8)
        defaults.set(payload, forKey: SavedServerStore.savedServersKey)
        defaults.set(payload, forKey: SavedServerStore.retiredSavedServersKey)

        XCTAssertTrue(SavedServerStore.removeAllForSecurityCutover(from: defaults))
        XCTAssertNil(defaults.object(forKey: SavedServerStore.savedServersKey))
        XCTAssertNil(defaults.object(forKey: SavedServerStore.retiredSavedServersKey))
    }

    func testRemovingLastSSHServerUnpinsItsExactTrustTarget() throws {
        let (defaults, suiteName) = try makeDefaults()
        defer { defaults.removePersistentDomain(forName: suiteName) }
        let ssh = makeServer(
            id: "ssh",
            hostname: "HOST.EXAMPLE",
            port: nil,
            sshPort: 2_222,
            hasCodexServer: false
        )
        SavedServerStore.save([ssh], to: defaults)
        var unpinned: [(String, UInt16)] = []

        try SavedServerStore.remove(serverId: ssh.id, from: defaults) { host, port in
            unpinned.append((host, port))
        }

        XCTAssertTrue(SavedServerStore.load(from: defaults).isEmpty)
        XCTAssertEqual(unpinned.count, 1)
        XCTAssertEqual(unpinned.first?.0, "HOST.EXAMPLE")
        XCTAssertEqual(unpinned.first?.1, 2_222)
    }

    func testRemovingSharedSSHTargetKeepsPinUntilLastReferenceIsGone() throws {
        let (defaults, suiteName) = try makeDefaults()
        defer { defaults.removePersistentDomain(forName: suiteName) }
        let first = makeServer(
            id: "ssh-1",
            hostname: "HOST.EXAMPLE",
            port: nil,
            sshPort: 22,
            hasCodexServer: false
        )
        let second = makeServer(
            id: "ssh-2",
            hostname: "host.example",
            port: nil,
            sshPort: 22,
            hasCodexServer: false
        )
        SavedServerStore.save([first, second], to: defaults)
        var unpinned: [(String, UInt16)] = []

        try SavedServerStore.remove(serverId: first.id, from: defaults) { host, port in
            unpinned.append((host, port))
        }

        XCTAssertEqual(SavedServerStore.load(from: defaults), [second])
        XCTAssertTrue(unpinned.isEmpty)

        try SavedServerStore.remove(serverId: second.id, from: defaults) { host, port in
            unpinned.append((host, port))
        }

        XCTAssertTrue(SavedServerStore.load(from: defaults).isEmpty)
        XCTAssertEqual(unpinned.count, 1)
        XCTAssertEqual(unpinned.first?.0, "host.example")
        XCTAssertEqual(unpinned.first?.1, 22)
    }

    func testFailedPinRemovalRetainsSavedServerForRetry() throws {
        let (defaults, suiteName) = try makeDefaults()
        defer { defaults.removePersistentDomain(forName: suiteName) }
        let ssh = makeServer(
            id: "ssh",
            hostname: "host.example",
            port: nil,
            sshPort: 22,
            hasCodexServer: false
        )
        SavedServerStore.save([ssh], to: defaults)

        XCTAssertThrowsError(
            try SavedServerStore.remove(serverId: ssh.id, from: defaults) { _, _ in
                throw NSError(domain: "SavedServerStoreTests", code: 1)
            }
        )

        XCTAssertEqual(SavedServerStore.load(from: defaults), [ssh])
    }

    private func makeServer(
        id: String,
        hostname: String,
        port: UInt16?,
        sshPort: UInt16?,
        hasCodexServer: Bool
    ) -> SavedServer {
        SavedServer(
            id: id,
            name: id,
            hostname: hostname,
            port: port,
            codexPorts: port.map { [$0] } ?? [],
            sshPort: sshPort,
            source: .manual,
            hasCodexServer: hasCodexServer,
            wakeMAC: nil,
            preferredConnectionMode: nil,
            preferredCodexPort: nil,
            websocketURL: nil,
            rememberedByUser: true
        )
    }

    private func makeDefaults() throws -> (UserDefaults, String) {
        let suiteName = "SavedServerStoreTests.\(UUID().uuidString)"
        return (try XCTUnwrap(UserDefaults(suiteName: suiteName)), suiteName)
    }
}
