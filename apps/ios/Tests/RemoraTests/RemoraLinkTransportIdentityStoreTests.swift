import Foundation
import Security
import XCTest
@testable import Remora

final class RemoraLinkTransportIdentityStoreTests: XCTestCase {
    func testCreatesAndReusesOneAppWideDeviceOnlyV2Identity() throws {
        let security = FakeTransportIdentitySecurity()
        let generated = Data(repeating: 0x42, count: RemoraLinkTransportIdentity.byteCount)
        let store = RemoraLinkTransportIdentityStore(
            security: security,
            entropy: { count in
                XCTAssertEqual(count, RemoraLinkTransportIdentity.byteCount)
                return .bytes(generated)
            }
        )

        let first = try store.loadOrCreate()
        let second = try store.loadOrCreate()

        XCTAssertEqual(first, second)
        XCTAssertEqual(first.copyBytes(), generated)
        XCTAssertEqual(security.acceptedCreates, 1)
        XCTAssertTrue(security.accessedKeys.allSatisfy { $0 == .applicationV2 })
        XCTAssertEqual(
            security.creationPolicies,
            [.backgroundCapableDeviceOnly]
        )
        XCTAssertEqual(RemoraLinkTransportIdentityKey.applicationV2.service,
                       "com.remora.app.remora-link.v2.transport-identity")
    }

    func testNeverReadsOrImportsLegacyIdentityRecords() throws {
        let security = FakeTransportIdentitySecurity()
        let legacyKey = RemoraLinkTransportIdentityKey(
            service: "com.example.legacy.remote-pairing",
            account: "legacy-client-key"
        )
        let legacyValue = Data(repeating: 0x11, count: RemoraLinkTransportIdentity.byteCount)
        security.seed(legacyValue, for: legacyKey)
        let candidate = Data(repeating: 0x22, count: RemoraLinkTransportIdentity.byteCount)
        let store = RemoraLinkTransportIdentityStore(security: security)

        let identity = try store.loadOrCreate(candidate: candidate)

        XCTAssertEqual(identity.copyBytes(), candidate)
        XCTAssertEqual(security.value(for: legacyKey), legacyValue)
        XCTAssertTrue(security.accessedKeys.allSatisfy { $0 == .applicationV2 })
    }

    func testConcurrentCandidatesAtomicallyAdoptOneWinner() throws {
        let security = RacingTransportIdentitySecurity(expectedInitialLoads: 2)
        let firstStore = RemoraLinkTransportIdentityStore(security: security)
        let secondStore = RemoraLinkTransportIdentityStore(security: security)
        let candidates = [
            Data(repeating: 0xa1, count: RemoraLinkTransportIdentity.byteCount),
            Data(repeating: 0xb2, count: RemoraLinkTransportIdentity.byteCount)
        ]
        let results = LockedTransportResults()
        let group = DispatchGroup()

        for (store, candidate) in zip([firstStore, secondStore], candidates) {
            group.enter()
            DispatchQueue.global(qos: .userInitiated).async {
                do {
                    results.append(.success(try store.loadOrCreate(candidate: candidate)))
                } catch {
                    results.append(.failure(error))
                }
                group.leave()
            }
        }
        XCTAssertEqual(group.wait(timeout: .now() + 10), .success)

        let values = try results.values.map { try $0.get() }
        XCTAssertEqual(values.count, 2)
        XCTAssertEqual(values[0], values[1])
        XCTAssertTrue(candidates.contains(values[0].copyBytes()))
        XCTAssertEqual(security.acceptedCreates, 1)
        XCTAssertGreaterThanOrEqual(security.duplicateCreates, 1)
    }

    func testRejectsInvalidCandidatesAndCorruptPersistedLengths() {
        let security = FakeTransportIdentitySecurity()
        let store = RemoraLinkTransportIdentityStore(security: security)

        for length in [0, 31, 33] {
            XCTAssertThrowsError(
                try store.loadOrCreate(candidate: Data(repeating: 0, count: length))
            ) { error in
                XCTAssertEqual(
                    error as? RemoraLinkTransportIdentityStoreError,
                    .invalidCandidateLength
                )
            }
        }
        XCTAssertTrue(security.accessedKeys.isEmpty)

        for length in [0, 31, 33] {
            let corruptSecurity = FakeTransportIdentitySecurity()
            corruptSecurity.seed(
                Data(repeating: 0, count: length),
                for: .applicationV2
            )
            let corruptStore = RemoraLinkTransportIdentityStore(security: corruptSecurity)
            XCTAssertThrowsError(
                try corruptStore.loadOrCreate(
                    candidate: Data(repeating: 1, count: RemoraLinkTransportIdentity.byteCount)
                )
            ) { error in
                XCTAssertEqual(
                    error as? RemoraLinkTransportIdentityStoreError,
                    .corruptStoredIdentity
                )
            }
            XCTAssertEqual(corruptSecurity.acceptedCreates, 0)
        }
    }

    func testRejectsExistingIdentityWithWeakerStoragePolicy() {
        let security = FakeTransportIdentitySecurity()
        let bytes = Data(repeating: 0x34, count: RemoraLinkTransportIdentity.byteCount)
        security.seed(
            bytes,
            for: .applicationV2,
            policy: RemoraLinkTransportIdentityStoragePolicy(
                accessibility: .unsupported,
                synchronizable: false
            )
        )
        let store = RemoraLinkTransportIdentityStore(security: security)

        XCTAssertThrowsError(try store.loadOrCreate(candidate: bytes)) { error in
            XCTAssertEqual(
                error as? RemoraLinkTransportIdentityStoreError,
                .corruptStoredIdentity
            )
        }
        XCTAssertEqual(security.acceptedCreates, 0)
    }

    func testRejectsDuplicateWinnerWithWeakerStoragePolicy() {
        let bytes = Data(repeating: 0x56, count: RemoraLinkTransportIdentity.byteCount)
        let security = DuplicateWinnerTransportIdentitySecurity(
            winner: RemoraLinkTransportIdentityStoredItem(
                bytes: bytes,
                policy: RemoraLinkTransportIdentityStoragePolicy(
                    accessibility: .afterFirstUnlockThisDeviceOnly,
                    synchronizable: true
                )
            )
        )
        let store = RemoraLinkTransportIdentityStore(security: security)

        XCTAssertThrowsError(try store.loadOrCreate(candidate: bytes)) { error in
            XCTAssertEqual(
                error as? RemoraLinkTransportIdentityStoreError,
                .corruptStoredIdentity
            )
        }
        XCTAssertEqual(security.createAttempts, 1)
    }

    func testSystemLookupQueriesAllVariantsAndParsesPolicyBeforeAdoption() {
        let security = SystemRemoraLinkTransportIdentitySecurity()
        let query = security.lookupQuery(key: .applicationV2)

        XCTAssertEqual(
            query[kSecAttrSynchronizable as String] as? String,
            kSecAttrSynchronizableAny as String
        )
        XCTAssertEqual(
            query[kSecMatchLimit as String] as? String,
            kSecMatchLimitAll as String
        )

        let bytes = Data(repeating: 0x78, count: RemoraLinkTransportIdentity.byteCount)
        let synchronizableItem: [String: Any] = [
            kSecValueData as String: bytes,
            kSecAttrAccessible as String: kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly,
            kSecAttrSynchronizable as String: true
        ]
        switch security.decodeLookupResult([synchronizableItem] as CFArray) {
        case .found(let item):
            XCTAssertTrue(item.policy.synchronizable)
        case .missing, .unavailable:
            XCTFail("Expected the weak record to reach policy validation")
        }

        let localItem: [String: Any] = [
            kSecValueData as String: bytes,
            kSecAttrAccessible as String: kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly,
            kSecAttrSynchronizable as String: false
        ]
        switch security.decodeLookupResult([localItem, synchronizableItem] as CFArray) {
        case .unavailable(let status):
            XCTAssertEqual(status, errSecDuplicateItem)
        case .found, .missing:
            XCTFail("Expected ambiguous exact-key records to fail closed")
        }
    }

    func testRevalidatesAllExactKeyVariantsAfterSuccessfulCreate() {
        let security = AmbiguousAfterCreateTransportIdentitySecurity()
        let candidate = Data(repeating: 0x9a, count: RemoraLinkTransportIdentity.byteCount)
        let store = RemoraLinkTransportIdentityStore(security: security)

        XCTAssertThrowsError(try store.loadOrCreate(candidate: candidate)) { error in
            XCTAssertEqual(
                error as? RemoraLinkTransportIdentityStoreError,
                .keychain(errSecDuplicateItem)
            )
        }
        XCTAssertEqual(security.createAttempts, 1)
        XCTAssertEqual(security.loadAttempts, 2)
    }

    func testReportsDeviceOnlyKeychainAndEntropyFailuresWithoutFallback() {
        let lockedSecurity = FakeTransportIdentitySecurity()
        lockedSecurity.createStatus = errSecInteractionNotAllowed
        let lockedStore = RemoraLinkTransportIdentityStore(security: lockedSecurity)

        XCTAssertThrowsError(
            try lockedStore.loadOrCreate(
                candidate: Data(repeating: 0x77, count: RemoraLinkTransportIdentity.byteCount)
            )
        ) { error in
            XCTAssertEqual(error as? RemoraLinkTransportIdentityStoreError, .keychainLocked)
        }
        XCTAssertEqual(lockedSecurity.creationPolicies, [.backgroundCapableDeviceOnly])
        XCTAssertNil(lockedSecurity.value(for: .applicationV2))

        let entropyStore = RemoraLinkTransportIdentityStore(
            security: FakeTransportIdentitySecurity(),
            entropy: { _ in .unavailable(errSecNotAvailable) }
        )
        XCTAssertThrowsError(try entropyStore.loadOrCreate()) { error in
            XCTAssertEqual(
                error as? RemoraLinkTransportIdentityStoreError,
                .entropyUnavailable(errSecNotAvailable)
            )
        }
    }

    func testIdentityDebugOutputNeverContainsSecretBytes() throws {
        let secretText = "identity-must-never-appear-00000"
        XCTAssertEqual(Data(secretText.utf8).count, RemoraLinkTransportIdentity.byteCount)
        let store = RemoraLinkTransportIdentityStore(security: FakeTransportIdentitySecurity())

        let identity = try store.loadOrCreate(candidate: Data(secretText.utf8))
        let description = String(describing: identity)
        let debug = String(reflecting: identity)

        XCTAssertFalse(description.contains(secretText))
        XCTAssertFalse(debug.contains(secretText))
        XCTAssertTrue(description.contains("<redacted 32 bytes>"))
        XCTAssertTrue(debug.contains("<redacted 32 bytes>"))
    }
}

private final class AmbiguousAfterCreateTransportIdentitySecurity:
    RemoraLinkTransportIdentitySecurity, @unchecked Sendable
{
    private let lock = NSLock()
    private var didCreate = false
    private var creates = 0
    private var loads = 0

    var createAttempts: Int {
        lock.withLock { creates }
    }

    var loadAttempts: Int {
        lock.withLock { loads }
    }

    func load(key: RemoraLinkTransportIdentityKey) -> RemoraLinkTransportIdentityLookup {
        precondition(key == .applicationV2)
        return lock.withLock {
            loads += 1
            return didCreate ? .unavailable(errSecDuplicateItem) : .missing
        }
    }

    func createIfAbsent(
        key: RemoraLinkTransportIdentityKey,
        candidate: UnsafeRawBufferPointer,
        policy: RemoraLinkTransportIdentityStoragePolicy
    ) -> RemoraLinkTransportIdentityCreateOutcome {
        precondition(key == .applicationV2)
        precondition(candidate.count == RemoraLinkTransportIdentity.byteCount)
        precondition(policy == .backgroundCapableDeviceOnly)
        return lock.withLock {
            creates += 1
            didCreate = true
            return .created
        }
    }
}

private final class DuplicateWinnerTransportIdentitySecurity:
    RemoraLinkTransportIdentitySecurity, @unchecked Sendable
{
    private let lock = NSLock()
    private let winner: RemoraLinkTransportIdentityStoredItem
    private var duplicateReported = false
    private var attempts = 0

    init(winner: RemoraLinkTransportIdentityStoredItem) {
        self.winner = winner
    }

    var createAttempts: Int {
        lock.withLock { attempts }
    }

    func load(key: RemoraLinkTransportIdentityKey) -> RemoraLinkTransportIdentityLookup {
        precondition(key == .applicationV2)
        return lock.withLock {
            duplicateReported ? .found(winner) : .missing
        }
    }

    func createIfAbsent(
        key: RemoraLinkTransportIdentityKey,
        candidate: UnsafeRawBufferPointer,
        policy: RemoraLinkTransportIdentityStoragePolicy
    ) -> RemoraLinkTransportIdentityCreateOutcome {
        precondition(key == .applicationV2)
        precondition(candidate.count == RemoraLinkTransportIdentity.byteCount)
        precondition(policy == .backgroundCapableDeviceOnly)
        return lock.withLock {
            attempts += 1
            duplicateReported = true
            return .duplicate
        }
    }
}

private final class FakeTransportIdentitySecurity: RemoraLinkTransportIdentitySecurity,
    @unchecked Sendable
{
    private let lock = NSLock()
    private var records: [RemoraLinkTransportIdentityKey: RemoraLinkTransportIdentityStoredItem] = [:]
    private var keys: [RemoraLinkTransportIdentityKey] = []
    private var policies: [RemoraLinkTransportIdentityStoragePolicy] = []
    private var createCount = 0

    var createStatus: OSStatus = errSecSuccess

    var accessedKeys: [RemoraLinkTransportIdentityKey] {
        lock.withLock { keys }
    }

    var creationPolicies: [RemoraLinkTransportIdentityStoragePolicy] {
        lock.withLock { policies }
    }

    var acceptedCreates: Int {
        lock.withLock { createCount }
    }

    func seed(
        _ value: Data,
        for key: RemoraLinkTransportIdentityKey,
        policy: RemoraLinkTransportIdentityStoragePolicy = .backgroundCapableDeviceOnly
    ) {
        lock.withLock {
            records[key] = RemoraLinkTransportIdentityStoredItem(bytes: value, policy: policy)
        }
    }

    func value(for key: RemoraLinkTransportIdentityKey) -> Data? {
        lock.withLock { records[key]?.bytes }
    }

    func load(key: RemoraLinkTransportIdentityKey) -> RemoraLinkTransportIdentityLookup {
        lock.withLock {
            keys.append(key)
            return records[key].map(RemoraLinkTransportIdentityLookup.found) ?? .missing
        }
    }

    func createIfAbsent(
        key: RemoraLinkTransportIdentityKey,
        candidate: UnsafeRawBufferPointer,
        policy: RemoraLinkTransportIdentityStoragePolicy
    ) -> RemoraLinkTransportIdentityCreateOutcome {
        lock.withLock {
            keys.append(key)
            policies.append(policy)
            guard createStatus == errSecSuccess else { return .unavailable(createStatus) }
            guard records[key] == nil else { return .duplicate }
            records[key] = RemoraLinkTransportIdentityStoredItem(
                bytes: Data(candidate),
                policy: policy
            )
            createCount += 1
            return .created
        }
    }
}

private final class RacingTransportIdentitySecurity: RemoraLinkTransportIdentitySecurity,
    @unchecked Sendable
{
    private let condition = NSCondition()
    private let expectedInitialLoads: Int
    private var record: RemoraLinkTransportIdentityStoredItem?
    private var initialLoads = 0
    private(set) var acceptedCreates = 0
    private(set) var duplicateCreates = 0

    init(expectedInitialLoads: Int) {
        self.expectedInitialLoads = expectedInitialLoads
    }

    func load(key: RemoraLinkTransportIdentityKey) -> RemoraLinkTransportIdentityLookup {
        precondition(key == .applicationV2)
        condition.lock()
        defer { condition.unlock() }
        if record == nil, initialLoads < expectedInitialLoads {
            initialLoads += 1
            if initialLoads == expectedInitialLoads {
                condition.broadcast()
            } else {
                while initialLoads < expectedInitialLoads {
                    condition.wait()
                }
            }
            return .missing
        }
        return record.map(RemoraLinkTransportIdentityLookup.found) ?? .missing
    }

    func createIfAbsent(
        key: RemoraLinkTransportIdentityKey,
        candidate: UnsafeRawBufferPointer,
        policy: RemoraLinkTransportIdentityStoragePolicy
    ) -> RemoraLinkTransportIdentityCreateOutcome {
        precondition(key == .applicationV2)
        precondition(policy == .backgroundCapableDeviceOnly)
        condition.lock()
        defer { condition.unlock() }
        guard record == nil else {
            duplicateCreates += 1
            return .duplicate
        }
        record = RemoraLinkTransportIdentityStoredItem(
            bytes: Data(candidate),
            policy: policy
        )
        acceptedCreates += 1
        return .created
    }
}

private final class LockedTransportResults: @unchecked Sendable {
    private let lock = NSLock()
    private var storage: [Result<RemoraLinkTransportIdentity, Error>] = []

    var values: [Result<RemoraLinkTransportIdentity, Error>] {
        lock.withLock { storage }
    }

    func append(_ result: Result<RemoraLinkTransportIdentity, Error>) {
        lock.withLock { storage.append(result) }
    }
}
