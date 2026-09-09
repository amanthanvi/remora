import Foundation
import Security
import XCTest
@testable import Remora

final class BackgroundRelayNativeStoresTests: XCTestCase {
    func testOpaqueJournalCASIsDurableAndExcludedFromBackup() async throws {
        let directory = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: directory) }
        let first = NativeRelayJournalBackend(store: RemoraLinkJournalStore(
            directoryURL: directory, excludeFromBackup: true
        ))
        let missing = await first.load()
        XCTAssertEqual(missing, .missing)
        let payload = Data([0, 255, 2, 0])
        let stored = await first.compareAndSwap(
            expectedRevision: nil, replacement: AppRelayJournalSnapshot(revision: 1, payload: payload)
        )
        XCTAssertEqual(stored, .stored)
        let second = NativeRelayJournalBackend(store: RemoraLinkJournalStore(
            directoryURL: directory, excludeFromBackup: true
        ))
        let loaded = await second.load()
        XCTAssertEqual(loaded, .loaded(snapshot: AppRelayJournalSnapshot(revision: 1, payload: payload)))
        let stale = await second.compareAndSwap(
            expectedRevision: nil, replacement: AppRelayJournalSnapshot(revision: 1, payload: Data())
        )
        XCTAssertEqual(stale, .conflict)
        XCTAssertEqual(try directory.resourceValues(forKeys: [.isExcludedFromBackupKey]).isExcludedFromBackup, true)
        let oversized = await first.compareAndSwap(
            expectedRevision: 1,
            replacement: AppRelayJournalSnapshot(revision: 2, payload: Data(count: 512 * 1_024 + 1))
        )
        XCTAssertEqual(oversized, .unavailable)
    }

    func testConcurrentJournalCreatesHaveOneWinnerAndCorruptionFailsClosed() async throws {
        let directory = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: directory) }
        let store = RemoraLinkJournalStore(directoryURL: directory, excludeFromBackup: true)
        let outcomes = await withTaskGroup(of: AppRelayJournalWriteOutcome.self) { group in
            for index in 0..<12 {
                group.addTask {
                    await NativeRelayJournalBackend(store: RemoraLinkJournalStore(
                        directoryURL: directory, excludeFromBackup: true
                    )).compareAndSwap(
                        expectedRevision: nil,
                        replacement: AppRelayJournalSnapshot(revision: 1, payload: Data([UInt8(index)]))
                    )
                }
            }
            var outcomes: [AppRelayJournalWriteOutcome] = []
            for await outcome in group { outcomes.append(outcome) }
            return outcomes
        }
        XCTAssertEqual(outcomes.filter { $0 == .stored }.count, 1)
        XCTAssertEqual(outcomes.filter { $0 == .conflict }.count, 11)
        try Data([0]).write(to: XCTUnwrap(store.journalFileURL))
        let corrupted = await NativeRelayJournalBackend(store: store).load()
        XCTAssertEqual(corrupted, .unavailable)
    }

    func testKeychainCASRetainsTombstoneAndWipesEverySuppliedValue() async throws {
        let service = testService()
        defer { removeTestService(service) }
        let backend = NativeRelaySecretBackend(service: service)
        let secret = AppRelaySecretValue(copying: [1, 2, 3])
        let created = await backend.compareAndSwap(
            alias: "relay_test", expectedRevision: nil, replacementRevision: 7, value: secret
        )
        XCTAssertEqual(created, .stored)
        XCTAssertTrue(secret.withUnsafeBytes { $0.allSatisfy { $0 == 0 } })
        let reread = try await NativeRelaySecretBackend(service: service).read(alias: "relay_test")
        XCTAssertTrue(reread.withUnsafeBytes { $0.elementsEqual([1, 2, 3]) })
        reread.zeroize()
        let tombstone = await backend.compareAndTombstone(
            alias: "relay_test", expectedRevision: 7, replacementRevision: 8
        )
        XCTAssertEqual(tombstone, .stored)
        let revision = await backend.revision(alias: "relay_test")
        XCTAssertEqual(revision, .found(revision: 8))
        do {
            _ = try await backend.read(alias: "relay_test")
            XCTFail("A tombstoned secret must be missing")
        } catch { XCTAssertEqual(error as? AppRelaySecretReadError, .Missing) }
        let stale = AppRelaySecretValue(copying: [9])
        let conflict = await backend.compareAndSwap(
            alias: "relay_test", expectedRevision: 7, replacementRevision: 9, value: stale
        )
        XCTAssertEqual(conflict, .conflict)
        XCTAssertTrue(stale.withUnsafeBytes { $0.allSatisfy { $0 == 0 } })
        let recreate = await backend.createIfAbsent(alias: "relay_test", value: AppRelaySecretValue(copying: [4]))
        XCTAssertEqual(recreate, .alreadyExists)
        let current = await backend.revision(alias: "relay_test")
        XCTAssertEqual(current, .found(revision: 8))
    }

    func testKeychainConcurrentCASAcrossInstancesHasExactlyOneWinner() async throws {
        let service = testService()
        defer { removeTestService(service) }
        let backend = NativeRelaySecretBackend(service: service)
        let created = await backend.createIfAbsent(alias: "relay_test", value: AppRelaySecretValue(copying: [1]))
        XCTAssertEqual(created, .created)
        let outcomes = await withTaskGroup(of: AppRelaySecretCasOutcome.self) { group in
            for index in 0..<16 {
                group.addTask {
                    await NativeRelaySecretBackend(service: service).compareAndSwap(
                        alias: "relay_test", expectedRevision: 1, replacementRevision: UInt64(index + 2),
                        value: AppRelaySecretValue(copying: [UInt8(index + 2)])
                    )
                }
            }
            var outcomes: [AppRelaySecretCasOutcome] = []
            for await outcome in group { outcomes.append(outcome) }
            return outcomes
        }
        XCTAssertEqual(outcomes.filter { $0 == .stored }.count, 1)
        XCTAssertEqual(outcomes.filter { $0 == .conflict }.count, 15)
    }

    func testRollbackAnchorRejectsUnfencedOverwriteDeleteAndTombstone() async throws {
        let service = testService()
        defer { removeTestService(service) }
        let backend = NativeRelaySecretBackend(service: service)
        let alias = NativeRelaySecretBackend.rollbackAnchor
        let created = await backend.createIfAbsent(alias: alias, value: AppRelaySecretValue(copying: [1]))
        XCTAssertEqual(created, .created)
        let overwritten = await backend.write(alias: alias, value: AppRelaySecretValue(copying: [2]))
        XCTAssertEqual(overwritten, .unavailable)
        let deleted = await backend.delete(alias: alias)
        XCTAssertEqual(deleted, .unavailable)
        let tombstoned = await backend.compareAndTombstone(alias: alias, expectedRevision: 1, replacementRevision: 2)
        XCTAssertEqual(tombstoned, .unavailable)
        let regressed = await backend.compareAndSwap(
            alias: alias, expectedRevision: 1, replacementRevision: 1, value: AppRelaySecretValue(copying: [2])
        )
        XCTAssertEqual(regressed, .unavailable)
        let revision = await backend.revision(alias: alias)
        XCTAssertEqual(revision, .found(revision: 1))
        var result: CFTypeRef?
        var query = testQuery(service)
        query[kSecReturnAttributes as String] = true
        XCTAssertEqual(SecItemCopyMatching(query as CFDictionary, &result), errSecSuccess)
        let attributes = try XCTUnwrap(result as? [String: Any])
        XCTAssertEqual(attributes[kSecAttrAccessible as String] as? String,
                       kSecAttrAccessibleWhenPasscodeSetThisDeviceOnly as String)
        XCTAssertNotEqual((attributes[kSecAttrSynchronizable as String] as? NSNumber)?.boolValue, true)
    }

    func testWeakerKeychainPolicyFailsClosed() async throws {
        let service = testService()
        defer { removeTestService(service) }
        var query = testQuery(service)
        query[kSecAttrAccount as String] = "relay_test"
        query[kSecAttrAccessible as String] = kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly
        query[kSecValueData as String] = Data([1])
        XCTAssertEqual(SecItemAdd(query as CFDictionary, nil), errSecSuccess)
        let backend = NativeRelaySecretBackend(service: service)
        let revision = await backend.revision(alias: "relay_test")
        XCTAssertEqual(revision, .unavailable)
        let outcome = await backend.createIfAbsent(alias: "relay_test", value: AppRelaySecretValue(copying: [2]))
        XCTAssertEqual(outcome, .unavailable)
        let invalidAlias = await backend.revision(alias: "../secret")
        XCTAssertEqual(invalidAlias, .unavailable)
    }

    func testMalformedMetadataCannotBeReplacedOrRead() async throws {
        let service = testService()
        defer { removeTestService(service) }
        var query = testQuery(service)
        query[kSecAttrAccount as String] = "relay_test"
        query[kSecAttrAccessible as String] = kSecAttrAccessibleWhenPasscodeSetThisDeviceOnly
        query[kSecAttrGeneric as String] = Data([1, 0])
        query[kSecValueData as String] = Data([1])
        XCTAssertEqual(SecItemAdd(query as CFDictionary, nil), errSecSuccess)
        let backend = NativeRelaySecretBackend(service: service)
        let revision = await backend.revision(alias: "relay_test")
        XCTAssertEqual(revision, .unavailable)
        let deleted = await backend.delete(alias: "relay_test")
        XCTAssertEqual(deleted, .unavailable)
        do {
            _ = try await backend.read(alias: "relay_test")
            XCTFail("Malformed custody must not return a secret")
        } catch { XCTAssertEqual(error as? AppRelaySecretReadError, .Unavailable) }
    }

    func testConcurrentKeychainCreateAndInvalidValueHaveNoOverwrite() async throws {
        let service = testService()
        defer { removeTestService(service) }
        let outcomes = await withTaskGroup(of: AppRelaySecretCreateOutcome.self) { group in
            for index in 0..<12 {
                group.addTask {
                    await NativeRelaySecretBackend(service: service).createIfAbsent(
                        alias: "relay_test", value: AppRelaySecretValue(copying: [UInt8(index + 1)])
                    )
                }
            }
            var outcomes: [AppRelaySecretCreateOutcome] = []
            for await outcome in group { outcomes.append(outcome) }
            return outcomes
        }
        XCTAssertEqual(outcomes.filter { $0 == .created }.count, 1)
        XCTAssertEqual(outcomes.filter { $0 == .alreadyExists }.count, 11)
        let backend = NativeRelaySecretBackend(service: service)
        let empty = await backend.compareAndSwap(
            alias: "relay_test", expectedRevision: 1, replacementRevision: 2,
            value: AppRelaySecretValue(copying: [UInt8]())
        )
        XCTAssertEqual(empty, .unavailable)
        let revision = await backend.revision(alias: "relay_test")
        XCTAssertEqual(revision, .found(revision: 1))
    }
}

private func testService() -> String {
    "com.remora.app.tests.background-relay.\(UUID().uuidString)"
}

private func testQuery(_ service: String) -> [String: Any] {
    [kSecClass as String: kSecClassGenericPassword,
     kSecAttrService as String: service,
     kSecUseDataProtectionKeychain as String: true]
}

private func removeTestService(_ service: String) {
    SecItemDelete(testQuery(service) as CFDictionary)
}
