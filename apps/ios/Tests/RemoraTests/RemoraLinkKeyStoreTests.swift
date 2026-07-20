import Foundation
import Security
import XCTest
@testable import Remora

final class RemoraLinkKeyStoreTests: XCTestCase {
    func testCreateUsesSecureEnclaveP256DeviceOnlyPrivateUsagePolicy() throws {
        let security = FakeRemoraLinkKeySecurity()
        let store = RemoraLinkKeyStore(security: security, softwareKeyPolicy: .disabled)

        let publicKey = try store.createIfNeeded(slot: "rust-slot-1")

        XCTAssertEqual(publicKey.assurance, .hardwareProtected)
        XCTAssertEqual(publicKey.x963Representation, security.validPublicKey)
        XCTAssertEqual(
            security.creationPolicies,
            [
                RemoraLinkKeyCreationPolicy(
                    protection: .secureEnclave,
                    keySizeInBits: 256,
                    whenUnlockedThisDeviceOnly: true,
                    privateKeyUsageOnly: true,
                    requiresUserPresence: false
                )
            ]
        )
    }

    func testSameSlotReusesKeyAndDifferentSlotsUseDifferentOpaqueTags() throws {
        let security = FakeRemoraLinkKeySecurity()
        let store = RemoraLinkKeyStore(security: security, softwareKeyPolicy: .disabled)

        _ = try store.createIfNeeded(slot: "host-a")
        _ = try store.createIfNeeded(slot: "host-a")
        _ = try store.createIfNeeded(slot: "host-b")

        XCTAssertEqual(security.creationPolicies.count, 2)
        XCTAssertEqual(Set(security.createdTags).count, 2)
        XCTAssertFalse(String(decoding: security.createdTags[0], as: UTF8.self).contains("host-a"))
        XCTAssertFalse(String(decoding: security.createdTags[1], as: UTF8.self).contains("host-b"))
        XCTAssertTrue(
            security.createdTags.allSatisfy {
                String(decoding: $0, as: UTF8.self)
                    .hasPrefix("com.remora.app.remora-link.v2.signing.")
            }
        )
    }

    func testSignPassesCanonicalMessageUnchangedAndReturnsDER() throws {
        let security = FakeRemoraLinkKeySecurity()
        let store = RemoraLinkKeyStore(security: security, softwareKeyPolicy: .disabled)
        _ = try store.createIfNeeded(slot: "signing-slot")
        let message = Data([0x00, 0x01, 0x7f, 0x80, 0xff])

        let signature = try store.sign(slot: "signing-slot", message: message)

        XCTAssertEqual(security.signedMessages, [message])
        XCTAssertEqual(signature.derRepresentation, security.derSignature)
        XCTAssertEqual(signature.assurance, .hardwareProtected)
    }

    func testReleasePolicyFailsClosedWhenSecureEnclaveIsUnavailable() {
        let security = FakeRemoraLinkKeySecurity()
        security.secureEnclaveCreationStatus = errSecNotAvailable
        let store = RemoraLinkKeyStore(security: security, softwareKeyPolicy: .disabled)

        XCTAssertThrowsError(try store.createIfNeeded(slot: "slot")) { error in
            XCTAssertEqual(error as? RemoraLinkKeyStoreError, .secureEnclaveUnavailable)
        }
        XCTAssertEqual(security.creationPolicies.map(\.protection), [.secureEnclave])
    }

    func testExplicitDebugPolicyFallsBackToMarkedSoftwareKey() throws {
        let security = FakeRemoraLinkKeySecurity()
        security.secureEnclaveCreationStatus = errSecNotAvailable
        let store = RemoraLinkKeyStore(security: security, softwareKeyPolicy: .debugBuildOnly)

        let publicKey = try store.createIfNeeded(slot: "slot")

        XCTAssertEqual(publicKey.assurance, .softwareDebugOnly)
        XCTAssertEqual(
            security.creationPolicies.map(\.protection),
            [.secureEnclave, .softwareDebugOnly]
        )
    }

    func testReleasePolicyRejectsPersistedSoftwareKey() {
        let security = FakeRemoraLinkKeySecurity()
        security.seed(slotTag: nil, assurance: .softwareDebugOnly)
        let store = RemoraLinkKeyStore(security: security, softwareKeyPolicy: .disabled)

        XCTAssertEqual(store.status(slot: "slot"), .failed(.softwareKeyNotAllowed))
        XCTAssertThrowsError(try store.createIfNeeded(slot: "slot")) { error in
            XCTAssertEqual(error as? RemoraLinkKeyStoreError, .softwareKeyNotAllowed)
        }
    }

    func testStatusDistinguishesAvailableLockedMissingAndFailed() throws {
        let security = FakeRemoraLinkKeySecurity()
        let store = RemoraLinkKeyStore(security: security, softwareKeyPolicy: .disabled)

        XCTAssertEqual(store.status(slot: "slot"), .missing)
        _ = try store.createIfNeeded(slot: "slot")
        XCTAssertEqual(store.status(slot: "slot"), .available(.hardwareProtected))

        security.lookupOverride = .locked
        XCTAssertEqual(store.status(slot: "slot"), .locked)

        security.lookupOverride = .failed(errSecNotAvailable)
        XCTAssertEqual(
            store.status(slot: "slot"),
            .failed(.keychainFailure(errSecNotAvailable))
        )
    }

    func testLockedKeyProducesActionableTransientError() throws {
        let security = FakeRemoraLinkKeySecurity()
        let store = RemoraLinkKeyStore(security: security, softwareKeyPolicy: .disabled)
        _ = try store.createIfNeeded(slot: "slot")
        security.lookupOverride = .locked

        XCTAssertThrowsError(try store.sign(slot: "slot", message: Data("proof".utf8))) { error in
            XCTAssertEqual(error as? RemoraLinkKeyStoreError, .keychainLocked)
            XCTAssertEqual(
                (error as? LocalizedError)?.errorDescription,
                "Unlock this device to use the paired host."
            )
        }
    }

    func testDeleteIsIdempotentAndScopedToOneOpaqueSlot() throws {
        let security = FakeRemoraLinkKeySecurity()
        let store = RemoraLinkKeyStore(security: security, softwareKeyPolicy: .disabled)
        _ = try store.createIfNeeded(slot: "slot-a")
        _ = try store.createIfNeeded(slot: "slot-b")

        XCTAssertEqual(try store.delete(slot: "slot-a"), .deleted)
        XCTAssertEqual(try store.delete(slot: "slot-a"), .alreadyMissing)

        XCTAssertEqual(store.status(slot: "slot-a"), .missing)
        XCTAssertEqual(store.status(slot: "slot-b"), .available(.hardwareProtected))
        XCTAssertEqual(security.deletedTags.count, 2)
        XCTAssertEqual(security.deletedTags[0], security.deletedTags[1])
    }

    func testLoadDoesNotCreateAMissingKey() throws {
        let security = FakeRemoraLinkKeySecurity()
        let store = RemoraLinkKeyStore(security: security, softwareKeyPolicy: .disabled)

        XCTAssertNil(try store.load(slot: "missing-slot"))
        XCTAssertTrue(security.creationPolicies.isEmpty)
        XCTAssertTrue(security.createdTags.isEmpty)

        let created = try store.createIfNeeded(slot: "missing-slot")
        XCTAssertEqual(try store.load(slot: "missing-slot"), created)
        XCTAssertEqual(security.creationPolicies.count, 1)
    }

    func testRejectsInvalidSlotAndMalformedPublicKey() {
        let security = FakeRemoraLinkKeySecurity()
        let store = RemoraLinkKeyStore(security: security, softwareKeyPolicy: .disabled)

        XCTAssertEqual(store.status(slot: ""), .failed(.invalidSlot))
        XCTAssertThrowsError(try store.createIfNeeded(slot: "")) { error in
            XCTAssertEqual(error as? RemoraLinkKeyStoreError, .invalidSlot)
        }

        security.validPublicKey = Data(repeating: 0x04, count: 64)
        XCTAssertThrowsError(try store.createIfNeeded(slot: "slot")) { error in
            XCTAssertEqual(error as? RemoraLinkKeyStoreError, .invalidPublicKey)
        }
    }

    func testConcurrentCreationRaceAdoptsTheWinner() throws {
        let security = FakeRemoraLinkKeySecurity()
        security.createRace = true
        let store = RemoraLinkKeyStore(security: security, softwareKeyPolicy: .disabled)

        let result = try store.createIfNeeded(slot: "slot")

        XCTAssertEqual(result.assurance, .hardwareProtected)
        XCTAssertEqual(security.creationPolicies.count, 1)
    }
}

private final class FakeKeyHandle: NSObject {}

private final class FakeRemoraLinkKeySecurity: RemoraLinkKeySecurity {
    var validPublicKey = Data([0x04] + Array(repeating: 0x2a, count: 64))
    let derSignature = Data([0x30, 0x06, 0x02, 0x01, 0x01, 0x02, 0x01, 0x01])

    var secureEnclaveCreationStatus: OSStatus = errSecSuccess
    var lookupOverride: RemoraLinkSecurityLookup?
    var createRace = false

    private var keys: [Data: (FakeKeyHandle, RemoraLinkKeyAssurance)] = [:]
    private var deferredSeedAssurance: RemoraLinkKeyAssurance?

    private(set) var creationPolicies: [RemoraLinkKeyCreationPolicy] = []
    private(set) var createdTags: [Data] = []
    private(set) var deletedTags: [Data] = []
    private(set) var signedMessages: [Data] = []

    func seed(slotTag: Data?, assurance: RemoraLinkKeyAssurance) {
        if let slotTag {
            keys[slotTag] = (FakeKeyHandle(), assurance)
        } else {
            deferredSeedAssurance = assurance
        }
    }

    func lookupPrivateKey(applicationTag: Data) -> RemoraLinkSecurityLookup {
        if let lookupOverride { return lookupOverride }
        if let deferredSeedAssurance {
            let handle = FakeKeyHandle()
            keys[applicationTag] = (handle, deferredSeedAssurance)
            self.deferredSeedAssurance = nil
            return .found(handle: handle, assurance: deferredSeedAssurance)
        }
        guard let key = keys[applicationTag] else { return .missing }
        return .found(handle: key.0, assurance: key.1)
    }

    func createPrivateKey(
        applicationTag: Data,
        policy: RemoraLinkKeyCreationPolicy
    ) -> RemoraLinkSecurityCreation {
        creationPolicies.append(policy)
        createdTags.append(applicationTag)
        if createRace {
            keys[applicationTag] = (FakeKeyHandle(), .hardwareProtected)
            createRace = false
            return .failed(errSecDuplicateItem)
        }
        if policy.protection == .secureEnclave,
           secureEnclaveCreationStatus != errSecSuccess {
            return .failed(secureEnclaveCreationStatus)
        }
        let assurance: RemoraLinkKeyAssurance = policy.protection == .secureEnclave
            ? .hardwareProtected
            : .softwareDebugOnly
        let handle = FakeKeyHandle()
        keys[applicationTag] = (handle, assurance)
        return .created(handle: handle, assurance: assurance)
    }

    func publicKeyX963(privateKey _: AnyObject) -> RemoraLinkSecurityDataResult {
        .value(validPublicKey)
    }

    func signMessageP256SHA256(
        privateKey _: AnyObject,
        message: UnsafeRawBufferPointer
    ) -> RemoraLinkSecurityDataResult {
        signedMessages.append(Data(message))
        return .value(derSignature)
    }

    func deletePrivateKey(applicationTag: Data) -> OSStatus {
        deletedTags.append(applicationTag)
        return keys.removeValue(forKey: applicationTag) == nil ? errSecItemNotFound : errSecSuccess
    }
}
