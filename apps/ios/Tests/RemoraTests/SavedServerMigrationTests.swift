import XCTest
@testable import Remora

@MainActor
final class SavedServerMigrationTests: XCTestCase {
    func testLegacySSHBridgeCSVDecodesToTypedNormalizedRuntimeKinds() throws {
        let server = try JSONDecoder().decode(
            SavedServer.self,
            from: serverJSON(
                extra: """
                "alleycatAgentName": " Codex, factory, CODEX, open-code ",
                "alleycatAgentWire": "ssh-bridge"
                """
            )
        )

        XCTAssertEqual(server.sshBridgeRuntimeKinds, ["codex", "droid", "opencode"])
    }

    func testLegacySSHBridgeWithEmptyCSVPreservesProbeAllSemantics() throws {
        let server = try JSONDecoder().decode(
            SavedServer.self,
            from: serverJSON(
                extra: """
                "alleycatAgentName": "  ",
                "alleycatAgentWire": "ssh-bridge"
                """
            )
        )

        XCTAssertEqual(server.sshBridgeRuntimeKinds, [])
    }

    func testHistoricalSSHBridgeIDWithoutCSVPreservesProbeAllSemantics() throws {
        let server = try JSONDecoder().decode(
            SavedServer.self,
            from: serverJSON(id: "ssh-bridge:studio.local", extra: "")
        )

        XCTAssertEqual(server.sshBridgeRuntimeKinds, [])
    }

    func testStoreDropsV1OnlyRowsAndRewritesSurvivingMixedRowsWithoutLegacyKeys() throws {
        let suiteName = "SavedServerMigrationTests.\(UUID().uuidString)"
        let defaults = try XCTUnwrap(UserDefaults(suiteName: suiteName))
        defer { defaults.removePersistentDomain(forName: suiteName) }

        let payload = """
        [
          \(serverJSONObject(id: "legacy-only", host: "legacy.invalid", port: 0, sshPort: nil, extra: "\"alleycatNodeId\": \"node-v1\", \"alleycatAgentWire\": \"jsonl\"")),
          \(serverJSONObject(id: "mixed-direct", host: "direct.local", port: 8390, sshPort: nil, extra: "\"alleycatNodeId\": \"old-node\"")),
          \(serverJSONObject(id: "mixed-legacy-ssh", host: "old-ssh.local", port: 2222, sshPort: nil, hasCodexServer: false, extra: "\"alleycatNodeId\": \"old-node\"")),
          \(serverJSONObject(id: "ordinary-ssh", host: "ssh.local", port: nil, sshPort: 22, hasCodexServer: false, extra: "")),
          \(serverJSONObject(id: "bridge", host: "bridge.local", port: nil, sshPort: 22, extra: "\"alleycatAgentName\": \"codex,droid\", \"alleycatAgentWire\": \"ssh-bridge\""))
        ]
        """
        defaults.set(try XCTUnwrap(payload.data(using: .utf8)), forKey: SavedServerStore.savedServersKey)

        let loaded = SavedServerStore.load(from: defaults)

        XCTAssertEqual(
            Set(loaded.map(\.id)),
            ["mixed-direct", "mixed-legacy-ssh", "ordinary-ssh", "bridge"]
        )
        XCTAssertNil(loaded.first { $0.id == "mixed-direct" }?.sshBridgeRuntimeKinds)
        XCTAssertEqual(loaded.first { $0.id == "bridge" }?.sshBridgeRuntimeKinds, ["codex", "droid"])

        let rewritten = try XCTUnwrap(defaults.data(forKey: SavedServerStore.savedServersKey))
        let json = try XCTUnwrap(String(data: rewritten, encoding: .utf8))
        XCTAssertFalse(json.contains("alleycat"))
        XCTAssertTrue(json.contains("sshBridgeRuntimeKinds"))
    }

    private func serverJSON(id: String = "bridge", extra: String) -> Data {
        Data(serverJSONObject(id: id, host: "bridge.local", port: nil, sshPort: 22, extra: extra).utf8)
    }

    private func serverJSONObject(
        id: String,
        host: String,
        port: UInt16?,
        sshPort: UInt16?,
        hasCodexServer: Bool = true,
        extra: String
    ) -> String {
        let optionalPort = port.map(String.init) ?? "null"
        let optionalSSHPort = sshPort.map(String.init) ?? "null"
        let suffix = extra.isEmpty ? "" : ",\n\(extra)"
        return """
        {
          "id": "\(id)",
          "name": "\(id)",
          "hostname": "\(host)",
          "port": \(optionalPort),
          "codexPorts": \(port.map { "[\($0)]" } ?? "[]"),
          "sshPort": \(optionalSSHPort),
          "source": "manual",
          "hasCodexServer": \(hasCodexServer),
          "rememberedByUser": true\(suffix)
        }
        """
    }
}
