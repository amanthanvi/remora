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
