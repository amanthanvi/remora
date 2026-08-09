import XCTest
@testable import Remora

@MainActor
final class SavedServerStoreTests: XCTestCase {
    func testRemovalCommitClassificationRetainsOnlyAmbiguousPersistence() {
        XCTAssertTrue(SavedServerStoreError.persistenceUncertain.removalMayHaveCommitted)
        XCTAssertTrue(
            SavedServerStoreError.trustCleanupPending("retry").removalMayHaveCommitted
        )
        XCTAssertFalse(
            SavedServerStoreError.persistenceFailed.removalMayHaveCommitted
        )
        XCTAssertFalse(
            SavedServerStoreError.invalidTrustCleanupJournal.removalMayHaveCommitted
        )
        XCTAssertFalse(
            SavedServerStoreError.trustCleanupAlreadyPending.removalMayHaveCommitted
        )
    }

    func testUnsynchronizedVisibleWriteIsPersistenceUncertain() throws {
        let (defaults, suiteName) = try makeControlledDefaults()
        defer { defaults.removePersistentDomain(forName: suiteName) }
        defaults.synchronizeResult = false
        let server = makeServer(
            id: "direct",
            hostname: "direct.local",
            port: 8_390,
            sshPort: nil,
            hasCodexServer: true
        )

        XCTAssertThrowsError(
            try SavedServerStore.save(
                [server],
                to: defaults,
                pinned: { _, _ in nil },
                pin: { _, _, _ in }
            )
        ) { error in
            guard let storeError = error as? SavedServerStoreError,
                  case .persistenceUncertain = storeError else {
                XCTFail("expected persistenceUncertain, got \(error)")
                return
            }
        }

        XCTAssertEqual(SavedServerStore.load(from: defaults), [server])
    }

    func testSynchronizedMismatchedReadbackIsPersistenceFailed() throws {
        let (defaults, suiteName) = try makeControlledDefaults()
        defer { defaults.removePersistentDomain(forName: suiteName) }
        defaults.dataOverrides[SavedServerStore.savedServersKey] = Data("stale".utf8)
        let server = makeServer(
            id: "direct",
            hostname: "direct.local",
            port: 8_390,
            sshPort: nil,
            hasCodexServer: true
        )

        XCTAssertThrowsError(
            try SavedServerStore.save(
                [server],
                to: defaults,
                pinned: { _, _ in nil },
                pin: { _, _, _ in }
            )
        ) { error in
            guard let storeError = error as? SavedServerStoreError,
                  case .persistenceFailed = storeError else {
                XCTFail("expected persistenceFailed, got \(error)")
                return
            }
        }
    }

    func testReadbackFailureAfterDurableCleanupJournalRemainsPending() throws {
        let (defaults, suiteName) = try makeControlledDefaults()
        defer { defaults.removePersistentDomain(forName: suiteName) }
        let server = makeServer(
            id: "ssh",
            hostname: "ssh.local",
            port: nil,
            sshPort: 22,
            hasCodexServer: false
        )
        SavedServerStore.save([server], to: defaults)
        defaults.dataOverrides[SavedServerStore.savedServersKey] = try XCTUnwrap(
            defaults.data(forKey: SavedServerStore.savedServersKey)
        )
        var unpinCalled = false

        XCTAssertThrowsError(
            try SavedServerStore.remove(
                serverId: server.id,
                from: defaults,
                pinned: { _, _ in "SHA256:original" },
                unpin: { _, _ in unpinCalled = true }
            )
        ) { error in
            guard let storeError = error as? SavedServerStoreError,
                  case .trustCleanupPending = storeError else {
                XCTFail("expected trustCleanupPending, got \(error)")
                return
            }
        }

        XCTAssertFalse(unpinCalled)
        XCTAssertNotNil(defaults.data(forKey: SavedServerStore.sshTrustCleanupJournalKey))
    }

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
        defaults.set(payload, forKey: SavedServerStore.sshTrustCleanupJournalKey)

        XCTAssertTrue(SavedServerStore.removeAllForSecurityCutover(from: defaults))
        XCTAssertNil(defaults.object(forKey: SavedServerStore.savedServersKey))
        XCTAssertNil(defaults.object(forKey: SavedServerStore.retiredSavedServersKey))
        XCTAssertNil(defaults.object(forKey: SavedServerStore.sshTrustCleanupJournalKey))
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
            XCTAssertTrue(SavedServerStore.load(from: defaults).isEmpty)
            XCTAssertNotNil(defaults.data(forKey: SavedServerStore.sshTrustCleanupJournalKey))
            unpinned.append((host, port))
        }

        XCTAssertTrue(SavedServerStore.load(from: defaults).isEmpty)
        XCTAssertNil(defaults.data(forKey: SavedServerStore.sshTrustCleanupJournalKey))
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

    func testRemovingDuplicateIdsDurablyCleansEveryUniqueSSHTrustTarget() throws {
        let (defaults, suiteName) = try makeDefaults()
        defer { defaults.removePersistentDomain(forName: suiteName) }
        let first = makeServer(
            id: "duplicate",
            hostname: "first.example",
            port: nil,
            sshPort: 22,
            hasCodexServer: false
        )
        let second = makeServer(
            id: "duplicate",
            hostname: "second.example",
            port: nil,
            sshPort: 2_222,
            hasCodexServer: false
        )
        SavedServerStore.save([first, second], to: defaults)
        var initialAttempts: [(String, UInt16)] = []

        XCTAssertThrowsError(
            try SavedServerStore.remove(
                serverId: first.id,
                from: defaults,
                pinned: { host, _ in "SHA256:\(host)" }
            ) { host, port in
                initialAttempts.append((host, port))
                if host == second.hostname {
                    throw NSError(domain: "SavedServerStoreTests", code: 8)
                }
            }
        )

        XCTAssertTrue(SavedServerStore.load(from: defaults).isEmpty)
        XCTAssertEqual(initialAttempts.map(\.0), [first.hostname, second.hostname])
        XCTAssertNotNil(defaults.data(forKey: SavedServerStore.sshTrustCleanupJournalKey))

        var replayed: [(String, UInt16)] = []
        XCTAssertTrue(
            try SavedServerStore.resumePendingTrustCleanup(from: defaults) { host, port in
                replayed.append((host, port))
            }
        )
        XCTAssertEqual(replayed.map(\.0), [first.hostname, second.hostname])
        XCTAssertEqual(replayed.map(\.1), [first.sshPort, second.sshPort])
        XCTAssertNil(defaults.data(forKey: SavedServerStore.sshTrustCleanupJournalKey))
    }

    func testFailedPinRemovalRetainsDurableJournalAndReplaysExactlyOnce() throws {
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

        XCTAssertTrue(SavedServerStore.load(from: defaults).isEmpty)
        XCTAssertNotNil(defaults.data(forKey: SavedServerStore.sshTrustCleanupJournalKey))

        var replayed: [(String, UInt16)] = []
        XCTAssertTrue(
            try SavedServerStore.resumePendingTrustCleanup(from: defaults) { host, port in
                replayed.append((host, port))
            }
        )
        XCTAssertEqual(replayed.count, 1)
        XCTAssertEqual(replayed.first?.0, "host.example")
        XCTAssertEqual(replayed.first?.1, 22)
        XCTAssertNil(defaults.data(forKey: SavedServerStore.sshTrustCleanupJournalKey))
        XCTAssertFalse(
            try SavedServerStore.resumePendingTrustCleanup(from: defaults) { host, port in
                replayed.append((host, port))
            }
        )
        XCTAssertEqual(replayed.count, 1)
    }

    func testPendingTrustCleanupRefusesAnotherTrustTargetMutation() throws {
        let (defaults, suiteName) = try makeDefaults()
        defer { defaults.removePersistentDomain(forName: suiteName) }
        let first = makeServer(
            id: "ssh-1",
            hostname: "first.example",
            port: nil,
            sshPort: 22,
            hasCodexServer: false
        )
        let second = makeServer(
            id: "ssh-2",
            hostname: "second.example",
            port: nil,
            sshPort: 22,
            hasCodexServer: false
        )
        SavedServerStore.save([first, second], to: defaults)

        XCTAssertThrowsError(
            try SavedServerStore.remove(serverId: first.id, from: defaults) { _, _ in
                throw NSError(domain: "SavedServerStoreTests", code: 2)
            }
        )
        var secondCleanupAttempted = false
        XCTAssertThrowsError(
            try SavedServerStore.remove(serverId: second.id, from: defaults) { _, _ in
                secondCleanupAttempted = true
            }
        )

        XCTAssertFalse(secondCleanupAttempted)
        XCTAssertEqual(SavedServerStore.load(from: defaults), [second])
        XCTAssertNotNil(defaults.data(forKey: SavedServerStore.sshTrustCleanupJournalKey))
    }

    func testSaveDuringPendingTrustCleanupRefreshesJournalAndSurvivesRecovery() throws {
        let (defaults, suiteName) = try makeDefaults()
        defer { defaults.removePersistentDomain(forName: suiteName) }
        let removed = makeServer(
            id: "ssh-1",
            hostname: "first.example",
            port: nil,
            sshPort: 22,
            hasCodexServer: false
        )
        let remaining = makeServer(
            id: "ssh-2",
            hostname: "second.example",
            port: nil,
            sshPort: 22,
            hasCodexServer: false
        )
        SavedServerStore.save([removed, remaining], to: defaults)
        XCTAssertThrowsError(
            try SavedServerStore.remove(serverId: removed.id, from: defaults) { _, _ in
                throw NSError(domain: "SavedServerStoreTests", code: 4)
            }
        )

        let renamed = makeServer(
            id: remaining.id,
            name: "Renamed",
            hostname: remaining.hostname,
            port: remaining.port,
            sshPort: remaining.sshPort,
            hasCodexServer: remaining.hasCodexServer
        )
        let changed = expectation(
            forNotification: .remoraSavedServersDidChange,
            object: nil,
            handler: nil
        )
        SavedServerStore.save([renamed], to: defaults)
        wait(for: [changed], timeout: 1)
        XCTAssertEqual(SavedServerStore.load(from: defaults), [renamed])

        var replayed: [(String, UInt16)] = []
        XCTAssertTrue(
            try SavedServerStore.resumePendingTrustCleanup(from: defaults) { host, port in
                replayed.append((host, port))
            }
        )
        XCTAssertEqual(replayed.count, 1)
        XCTAssertEqual(replayed.first?.0, removed.hostname)
        XCTAssertEqual(replayed.first?.1, removed.sshPort)
        XCTAssertEqual(SavedServerStore.load(from: defaults), [renamed])
        XCTAssertNil(defaults.data(forKey: SavedServerStore.sshTrustCleanupJournalKey))
    }

    func testReaddingPendingTrustTargetRestoresPinAfterAmbiguousUnpin() throws {
        let (defaults, suiteName) = try makeDefaults()
        defer { defaults.removePersistentDomain(forName: suiteName) }
        let removed = makeServer(
            id: "ssh-1",
            hostname: "[First.Example]",
            port: nil,
            sshPort: 22,
            hasCodexServer: false
        )
        SavedServerStore.save([removed], to: defaults)
        let originalFingerprint = "SHA256:original"
        var storedFingerprint: String? = originalFingerprint
        XCTAssertThrowsError(
            try SavedServerStore.remove(
                serverId: removed.id,
                from: defaults,
                pinned: { _, _ in storedFingerprint }
            ) { _, _ in
                storedFingerprint = nil
                throw NSError(domain: "SavedServerStoreTests", code: 5)
            }
        )

        let readded = makeServer(
            id: "ssh-2",
            hostname: "first.example",
            port: nil,
            sshPort: 22,
            hasCodexServer: false
        )
        var restored: [(String, UInt16, String)] = []
        try SavedServerStore.save(
            [readded],
            to: defaults,
            pinned: { _, _ in storedFingerprint },
            pin: { host, port, fingerprint in
                restored.append((host, port, fingerprint))
                storedFingerprint = fingerprint
            }
        )

        XCTAssertEqual(SavedServerStore.load(from: defaults), [readded])
        XCTAssertEqual(storedFingerprint, originalFingerprint)
        XCTAssertEqual(restored.count, 1)
        XCTAssertEqual(restored.first?.0, "[First.Example]")
        XCTAssertEqual(restored.first?.1, 22)
        XCTAssertEqual(restored.first?.2, originalFingerprint)
        XCTAssertNil(defaults.data(forKey: SavedServerStore.sshTrustCleanupJournalKey))
        var unpinned: [(String, UInt16)] = []
        XCTAssertFalse(
            try SavedServerStore.resumePendingTrustCleanup(from: defaults) { host, port in
                unpinned.append((host, port))
            }
        )
        XCTAssertTrue(unpinned.isEmpty)
    }

    func testRecoveryBlocksLegacyJournalForReferencedTrustTarget() throws {
        let (defaults, suiteName) = try makeDefaults()
        defer { defaults.removePersistentDomain(forName: suiteName) }
        let active = makeServer(
            id: "ssh",
            hostname: "first.example",
            port: nil,
            sshPort: 22,
            hasCodexServer: false
        )
        let encodedServers = try JSONSerialization.jsonObject(
            with: JSONEncoder().encode([active])
        )
        let journal = try JSONSerialization.data(withJSONObject: [
            "servers": encodedServers,
            "host": "[FIRST.EXAMPLE]",
            "port": 22,
        ])
        defaults.set(journal, forKey: SavedServerStore.sshTrustCleanupJournalKey)

        var unpinned: [(String, UInt16)] = []
        XCTAssertThrowsError(
            try SavedServerStore.resumePendingTrustCleanup(from: defaults) { host, port in
                unpinned.append((host, port))
            }
        )

        XCTAssertTrue(unpinned.isEmpty)
        XCTAssertEqual(SavedServerStore.load(from: defaults), [])
        XCTAssertNotNil(defaults.data(forKey: SavedServerStore.sshTrustCleanupJournalKey))
    }

    func testChangedPinBlocksReaddAndRetainsCleanupJournal() throws {
        let (defaults, suiteName) = try makeDefaults()
        defer { defaults.removePersistentDomain(forName: suiteName) }
        let removed = makeServer(
            id: "ssh-1",
            hostname: "first.example",
            port: nil,
            sshPort: 22,
            hasCodexServer: false
        )
        SavedServerStore.save([removed], to: defaults)
        XCTAssertThrowsError(
            try SavedServerStore.remove(
                serverId: removed.id,
                from: defaults,
                pinned: { _, _ in "SHA256:original" }
            ) { _, _ in
                throw NSError(domain: "SavedServerStoreTests", code: 7)
            }
        )
        let readded = makeServer(
            id: "ssh-2",
            hostname: "FIRST.EXAMPLE",
            port: nil,
            sshPort: 22,
            hasCodexServer: false
        )
        var pinAttempted = false

        XCTAssertThrowsError(
            try SavedServerStore.save(
                [readded],
                to: defaults,
                pinned: { _, _ in "SHA256:changed" },
                pin: { _, _, _ in pinAttempted = true }
            )
        )

        XCTAssertFalse(pinAttempted)
        XCTAssertEqual(SavedServerStore.load(from: defaults), [])
        XCTAssertNotNil(defaults.data(forKey: SavedServerStore.sshTrustCleanupJournalKey))
    }

    func testReaddingPendingHostOnDifferentPortStillCleansOriginalTarget() throws {
        let (defaults, suiteName) = try makeDefaults()
        defer { defaults.removePersistentDomain(forName: suiteName) }
        let removed = makeServer(
            id: "ssh-1",
            hostname: "First.Example",
            port: nil,
            sshPort: 22,
            hasCodexServer: false
        )
        SavedServerStore.save([removed], to: defaults)
        XCTAssertThrowsError(
            try SavedServerStore.remove(serverId: removed.id, from: defaults) { _, _ in
                throw NSError(domain: "SavedServerStoreTests", code: 6)
            }
        )

        let readded = makeServer(
            id: "ssh-2",
            hostname: "first.example",
            port: nil,
            sshPort: 2_222,
            hasCodexServer: false
        )
        SavedServerStore.save([readded], to: defaults)

        var unpinned: [(String, UInt16)] = []
        XCTAssertTrue(
            try SavedServerStore.resumePendingTrustCleanup(from: defaults) { host, port in
                unpinned.append((host, port))
            }
        )
        XCTAssertEqual(unpinned.count, 1)
        XCTAssertEqual(unpinned.first?.0, "First.Example")
        XCTAssertEqual(unpinned.first?.1, 22)
        XCTAssertEqual(SavedServerStore.load(from: defaults), [readded])
        XCTAssertNil(defaults.data(forKey: SavedServerStore.sshTrustCleanupJournalKey))
    }

    func testReplacingSSHEndpointUnpinsPreviousTrustTarget() throws {
        let (defaults, suiteName) = try makeDefaults()
        defer { defaults.removePersistentDomain(forName: suiteName) }
        let previous = makeServer(
            id: "ssh",
            hostname: "old.example",
            port: nil,
            sshPort: 22,
            hasCodexServer: false
        )
        let replacement = makeServer(
            id: "ssh",
            hostname: "new.example",
            port: nil,
            sshPort: 2_222,
            hasCodexServer: false
        )
        SavedServerStore.save([previous], to: defaults)
        var unpinned: [(String, UInt16)] = []

        try SavedServerStore.replace(replacement, from: defaults) { host, port in
            XCTAssertEqual(SavedServerStore.load(from: defaults), [replacement])
            XCTAssertNotNil(defaults.data(forKey: SavedServerStore.sshTrustCleanupJournalKey))
            unpinned.append((host, port))
        }

        XCTAssertEqual(SavedServerStore.load(from: defaults), [replacement])
        XCTAssertNil(defaults.data(forKey: SavedServerStore.sshTrustCleanupJournalKey))
        XCTAssertEqual(unpinned.count, 1)
        XCTAssertEqual(unpinned.first?.0, "old.example")
        XCTAssertEqual(unpinned.first?.1, 22)
    }

    func testReplacingSharedSSHEndpointKeepsReferencedPin() throws {
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
        let replacement = makeServer(
            id: "ssh-1",
            hostname: "new.example",
            port: nil,
            sshPort: 22,
            hasCodexServer: false
        )
        SavedServerStore.save([first, second], to: defaults)
        var unpinned: [(String, UInt16)] = []

        try SavedServerStore.replace(replacement, from: defaults) { host, port in
            unpinned.append((host, port))
        }

        XCTAssertEqual(SavedServerStore.load(from: defaults), [replacement, second])
        XCTAssertTrue(unpinned.isEmpty)
    }

    func testFailedEndpointReplacementUnpinRetainsReplacementAndJournal() throws {
        let (defaults, suiteName) = try makeDefaults()
        defer { defaults.removePersistentDomain(forName: suiteName) }
        let previous = makeServer(
            id: "ssh",
            hostname: "old.example",
            port: nil,
            sshPort: 22,
            hasCodexServer: false
        )
        let replacement = makeServer(
            id: "ssh",
            hostname: "new.example",
            port: nil,
            sshPort: 22,
            hasCodexServer: false
        )
        SavedServerStore.save([previous], to: defaults)

        XCTAssertThrowsError(
            try SavedServerStore.replace(replacement, from: defaults) { _, _ in
                throw NSError(domain: "SavedServerStoreTests", code: 3)
            }
        )

        XCTAssertEqual(SavedServerStore.load(from: defaults), [replacement])
        XCTAssertNotNil(defaults.data(forKey: SavedServerStore.sshTrustCleanupJournalKey))
    }

    private func makeServer(
        id: String,
        name: String? = nil,
        hostname: String,
        port: UInt16?,
        sshPort: UInt16?,
        hasCodexServer: Bool
    ) -> SavedServer {
        SavedServer(
            id: id,
            name: name ?? id,
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

    private func makeControlledDefaults() throws -> (ControlledUserDefaults, String) {
        let suiteName = "SavedServerStoreTests.\(UUID().uuidString)"
        return (try XCTUnwrap(ControlledUserDefaults(suiteName: suiteName)), suiteName)
    }
}

private final class ControlledUserDefaults: UserDefaults {
    var synchronizeResult = true
    var dataOverrides: [String: Data] = [:]

    override func synchronize() -> Bool {
        synchronizeResult
    }

    override func data(forKey defaultName: String) -> Data? {
        dataOverrides[defaultName] ?? super.data(forKey: defaultName)
    }
}
