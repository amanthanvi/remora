import CryptoKit
import Foundation
import Security
import XCTest
@testable import Remora

final class RemoraLinkNativeAdaptersTests: XCTestCase {
    func testJournalMapsNativeStatesAndNeverExposesCorruptPayloads() async {
        let snapshot = RemoraLinkJournalSnapshot(
            revision: 7,
            payload: Data([0x00, 0xff, 0x80])
        )
        let loaded = RemoraLinkJournalNativeAdapter(
            load: { .loaded(snapshot) },
            compareAndSwap: { expected, replacement in
                expected == 7 && replacement.revision == 8 ? .stored : .conflict
            }
        )

        let loadedResult = await loaded.load()
        XCTAssertEqual(
            loadedResult,
            .loaded(
                snapshot: AppRemoraLinkJournalSnapshot(
                    revision: 7,
                    payload: snapshot.payload
                )
            )
        )
        let storedResult = await loaded.compareAndSwap(
            expectedRevision: 7,
            replacement: AppRemoraLinkJournalSnapshot(
                revision: 8,
                payload: Data([1])
            )
        )
        XCTAssertEqual(storedResult, .stored)

        for nativeLoad in [RemoraLinkJournalLoadStatus.corrupt, .unavailable] {
            let adapter = RemoraLinkJournalNativeAdapter(
                load: { nativeLoad },
                compareAndSwap: { _, _ in .invalidReplacement }
            )
            let unavailableLoad = await adapter.load()
            XCTAssertEqual(unavailableLoad, .unavailable)
            let unavailableWrite = await adapter.compareAndSwap(
                expectedRevision: nil,
                replacement: AppRemoraLinkJournalSnapshot(
                    revision: 1,
                    payload: Data()
                )
            )
            XCTAssertEqual(unavailableWrite, .unavailable)
        }
    }

    func testTransportIdentityMapsFailuresAndZeroizesCandidateCarrier() async throws {
        let bytes = Data(repeating: 0x42, count: RemoraLinkTransportIdentity.byteCount)
        let candidate = AppRelaySecretValue(copying: bytes)
        let retainedSourceAlias = candidate
        let wipeRecorder = SensitiveWipeRecorder()
        let adapter = RemoraLinkTransportIdentityNativeAdapter(
            inputWipeObserver: wipeRecorder.observer,
            loadOrCreate: { supplied in
                XCTAssertEqual(wipeRecorder.recordBorrow(supplied), bytes)
                return AppRelaySecretValue(copying: supplied)
            }
        )

        let result = try await adapter.loadOrCreate(candidate: candidate)

        XCTAssertEqual(secretBytes(result), bytes)
        // Source aliases observe the generated carrier's own wipe. The
        // allocation-address assertion independently proves the adapter-owned
        // allocation was wiped before this callback returned.
        XCTAssertEqual(secretBytes(candidate), Data(repeating: 0, count: bytes.count))
        XCTAssertEqual(secretBytes(retainedSourceAlias), Data(repeating: 0, count: bytes.count))
        wipeRecorder.assertSingleExactWipe(file: #filePath, line: #line)

        let failureWipeRecorder = SensitiveWipeRecorder()
        let unavailable = RemoraLinkTransportIdentityNativeAdapter(
            inputWipeObserver: failureWipeRecorder.observer,
            loadOrCreate: { supplied in
                _ = failureWipeRecorder.recordBorrow(supplied)
                throw RemoraLinkTransportIdentityStoreError.corruptStoredIdentity
            }
        )
        do {
            _ = try await unavailable.loadOrCreate(
                candidate: AppRelaySecretValue(copying: bytes)
            )
            XCTFail("Expected fail-closed transport identity error")
        } catch {
            XCTAssertEqual(error as? AppRemoraLinkTransportIdentityError, .Unavailable)
        }
        failureWipeRecorder.assertSingleExactWipe(file: #filePath, line: #line)
    }

    func testDeviceKeyDerivesBoundedOpaqueSlotFromAuthenticatedHostID() async throws {
        let recorder = DeviceKeyRecorder()
        let adapter = makeDeviceKeyAdapter(recorder: recorder)
        let hostID = "authenticated-host-id"
        var hasher = SHA256()
        hasher.update(data: Data("com.remora.app/remora-link-v2/device-key-slot".utf8))
        hasher.update(data: Data([0]))
        hasher.update(data: Data(hostID.utf8))
        let expectedDigest = Data(hasher.finalize())
            .base64EncodedString()
            .replacingOccurrences(of: "+", with: "-")
            .replacingOccurrences(of: "/", with: "_")
            .replacingOccurrences(of: "=", with: "")

        let key = try await adapter.ensureHardwareKey(hostId: hostID)

        XCTAssertEqual(key.slot, "remora-link:v2:ios:\(expectedDigest)")
        XCTAssertLessThanOrEqual(key.slot.utf8.count, 256)
        XCTAssertFalse(key.slot.contains(hostID))
        XCTAssertEqual(recorder.createdSlots, [key.slot])
        XCTAssertEqual(key.assurance, .secureEnclave)

        do {
            _ = try await adapter.ensureHardwareKey(hostId: String(repeating: "x", count: 1_025))
            XCTFail("Expected oversized authenticated host ID to fail closed")
        } catch {
            XCTAssertEqual(error as? AppRemoraLinkDeviceKeyError, .Unavailable)
        }
        do {
            _ = try await adapter.ensureHardwareKey(hostId: "bad\nhost")
            XCTFail("Expected control characters to fail closed")
        } catch {
            XCTAssertEqual(error as? AppRemoraLinkDeviceKeyError, .Unavailable)
        }
    }

    func testLoadIsNonCreatingAndDeletePreservesMissingDistinction() async throws {
        let recorder = DeviceKeyRecorder()
        recorder.loadedKey = nil
        recorder.deleteOutcome = .alreadyMissing
        let adapter = makeDeviceKeyAdapter(recorder: recorder)

        let load = try await adapter.loadHardwareKey(slot: "host.slot")
        XCTAssertEqual(load, .missing)
        XCTAssertTrue(recorder.createdSlots.isEmpty)
        let missingDelete = try await adapter.deleteHardwareKey(slot: "host.slot")
        XCTAssertEqual(missingDelete, .alreadyMissing)

        recorder.deleteOutcome = .deleted
        let deleted = try await adapter.deleteHardwareKey(slot: "host.slot")
        XCTAssertEqual(deleted, .deleted)
    }

    func testSignPassesCanonicalBytesOnceAndWipesTemporaryCarrier() async throws {
        let recorder = DeviceKeyRecorder()
        let wipeRecorder = SensitiveWipeRecorder()
        let adapter = makeDeviceKeyAdapter(recorder: recorder, wipeRecorder: wipeRecorder)
        let canonical = Data([0x00, 0x01, 0x7f, 0x80, 0xff])
        let message = AppRelaySecretValue(copying: canonical)
        let retainedSourceAlias = message

        let result = try await adapter.signMessage(slot: "host.slot", message: message)

        XCTAssertEqual(recorder.signedMessages, [canonical])
        XCTAssertEqual(secretBytes(result), recorder.signature.derRepresentation)
        XCTAssertEqual(secretBytes(message), Data(repeating: 0, count: canonical.count))
        XCTAssertEqual(secretBytes(retainedSourceAlias), Data(repeating: 0, count: canonical.count))
        wipeRecorder.assertSingleExactWipe(file: #filePath, line: #line)

        result.zeroize()
        XCTAssertEqual(
            recorder.signature.derRepresentation,
            Data([0x30, 0x06, 0x02, 0x01, 0x01, 0x02, 0x01, 0x01])
        )
    }

    func testSensitiveInputWipesExactAllocationOnErrorAndCancellation() async {
        let signingFailureWipe = SensitiveWipeRecorder()
        let failing = RemoraLinkDeviceKeyNativeAdapter(
            inputWipeObserver: signingFailureWipe.observer,
            create: { _ in throw RemoraLinkKeyStoreError.keyNotFound },
            load: { _ in nil },
            sign: { _, bytes in
                _ = signingFailureWipe.recordBorrow(bytes)
                throw RemoraLinkKeyStoreError.signingFailed
            },
            delete: { _ in .alreadyMissing }
        )
        do {
            _ = try await failing.signMessage(
                slot: "slot",
                message: AppRelaySecretValue(copying: Data([1, 2, 3]))
            )
            XCTFail("Expected signing failure")
        } catch {
            XCTAssertEqual(error as? AppRemoraLinkDeviceKeyError, .InvalidSignature)
        }
        signingFailureWipe.assertSingleExactWipe(file: #filePath, line: #line)

        let executor = RemoraLinkSerializedExecutor(label: "test.remora-link.cancelled-secret")
        let blockerStarted = expectation(description: "serial executor blocked")
        let releaseBlocker = DispatchSemaphore(value: 0)
        let blocker = Task {
            await executor.perform {
                blockerStarted.fulfill()
                releaseBlocker.wait()
            }
        }
        await fulfillment(of: [blockerStarted], timeout: 2)

        let cancellationWipe = SensitiveWipeRecorder()
        let recorder = DeviceKeyRecorder()
        let cancelledAdapter = makeDeviceKeyAdapter(
            recorder: recorder,
            wipeRecorder: cancellationWipe,
            executor: executor
        )
        let message = AppRelaySecretValue(copying: Data([4, 5, 6]))
        let operation = Task {
            try await cancelledAdapter.signMessage(slot: "slot", message: message)
        }
        await waitUntil(timeout: 2) {
            message.withUnsafeBytes { Data($0) } == Data(repeating: 0, count: 3)
        }
        operation.cancel()
        releaseBlocker.signal()
        await blocker.value
        do {
            _ = try await operation.value
            XCTFail("Expected cancelled callback to fail closed")
        } catch {
            XCTAssertEqual(error as? AppRemoraLinkDeviceKeyError, .Unavailable)
        }
        XCTAssertTrue(recorder.signedMessages.isEmpty)
        cancellationWipe.assertSingleWipeWithoutBorrow(file: #filePath, line: #line)
    }

    func testDeviceKeyErrorsMapFailClosed() async {
        let cases: [(RemoraLinkKeyStoreError, AppRemoraLinkDeviceKeyError)] = [
            (.invalidSlot, .Unavailable),
            (.keychainLocked, .Locked),
            (.keyNotFound, .Missing),
            (.secureEnclaveUnavailable, .HardwareUnavailable),
            (.softwareKeyNotAllowed, .HardwareUnavailable),
            (.keychain(errSecNotAvailable), .Unavailable),
            (.invalidPublicKey, .Invalidated),
            (.signingFailed, .InvalidSignature)
        ]

        for (nativeError, expected) in cases {
            let adapter = RemoraLinkDeviceKeyNativeAdapter(
                create: { _ in throw nativeError },
                load: { _ in throw nativeError },
                sign: { _, _ in throw nativeError },
                delete: { _ in throw nativeError }
            )
            do {
                _ = try await adapter.loadHardwareKey(slot: "slot")
                XCTFail("Expected \(expected)")
            } catch {
                XCTAssertEqual(error as? AppRemoraLinkDeviceKeyError, expected)
            }
        }
    }

    func testSharedExecutorSerializesConcurrentCallbacksAndResumesAllCallers() async {
        let executor = RemoraLinkSerializedExecutor(label: "test.remora-link.serial")
        let probe = SerializedCallbackProbe()
        let journal = RemoraLinkJournalNativeAdapter(
            executor: executor,
            load: {
                probe.run()
                return .missing
            },
            compareAndSwap: { _, _ in .conflict }
        )
        let deviceKeys = RemoraLinkDeviceKeyNativeAdapter(
            executor: executor,
            create: { _ in throw RemoraLinkKeyStoreError.keyNotFound },
            load: { _ in
                probe.run()
                return nil
            },
            sign: { _, _ in throw RemoraLinkKeyStoreError.signingFailed },
            delete: { _ in .alreadyMissing }
        )

        let completed = await withTaskGroup(of: Bool.self) { group in
            for index in 0..<40 {
                if index.isMultiple(of: 2) {
                    group.addTask {
                        await journal.load() == .missing
                    }
                } else {
                    group.addTask {
                        (try? await deviceKeys.loadHardwareKey(slot: "slot")) == .missing
                    }
                }
            }
            return await group.reduce(into: 0) { count, succeeded in
                if succeeded { count += 1 }
            }
        }

        XCTAssertEqual(completed, 40)
        XCTAssertEqual(probe.maximumConcurrentCallbacks, 1)
        XCTAssertEqual(probe.callbackCount, 40)
    }

    private func makeDeviceKeyAdapter(
        recorder: DeviceKeyRecorder,
        wipeRecorder: SensitiveWipeRecorder? = nil,
        executor: RemoraLinkSerializedExecutor = RemoraLinkSerializedExecutor()
    ) -> RemoraLinkDeviceKeyNativeAdapter {
        RemoraLinkDeviceKeyNativeAdapter(
            executor: executor,
            inputWipeObserver: wipeRecorder?.observer,
            create: { slot in recorder.create(slot: slot) },
            load: { slot in recorder.load(slot: slot) },
            sign: { slot, message in
                _ = wipeRecorder?.recordBorrow(message)
                return recorder.sign(slot: slot, message: message)
            },
            delete: { slot in recorder.delete(slot: slot) }
        )
    }

    private func secretBytes(_ value: AppRelaySecretValue) -> Data {
        value.withUnsafeBytes { Data($0) }
    }

    private func waitUntil(
        timeout: TimeInterval,
        condition: @escaping () -> Bool
    ) async {
        let deadline = Date(timeIntervalSinceNow: timeout)
        while Date() < deadline, !condition() {
            try? await Task.sleep(for: .milliseconds(10))
        }
        XCTAssertTrue(condition())
    }
}

private final class DeviceKeyRecorder: @unchecked Sendable {
    private let lock = NSLock()
    private var slots: [String] = []
    private var messages: [Data] = []

    var loadedKey: RemoraLinkPublicKey?
    var deleteOutcome: RemoraLinkKeyDeletionOutcome = .deleted
    let signature = RemoraLinkSignature(
        derRepresentation: Data([0x30, 0x06, 0x02, 0x01, 0x01, 0x02, 0x01, 0x01]),
        assurance: .hardwareProtected
    )

    var createdSlots: [String] {
        lock.withLock { slots }
    }

    var signedMessages: [Data] {
        lock.withLock { messages }
    }

    func create(slot: String) -> RemoraLinkPublicKey {
        lock.withLock { slots.append(slot) }
        return publicKey
    }

    func load(slot _: String) -> RemoraLinkPublicKey? {
        loadedKey
    }

    func sign(slot _: String, message: UnsafeRawBufferPointer) -> RemoraLinkSignature {
        lock.withLock { messages.append(Data(message)) }
        return signature
    }

    func delete(slot _: String) -> RemoraLinkKeyDeletionOutcome {
        deleteOutcome
    }

    private var publicKey: RemoraLinkPublicKey {
        RemoraLinkPublicKey(
            x963Representation: Data([0x04] + Array(repeating: 0x2a, count: 64)),
            assurance: .hardwareProtected
        )
    }
}

private final class SensitiveWipeRecorder: @unchecked Sendable {
    struct WipeEvent {
        let address: UInt
        let byteCount: Int
        let isAllZero: Bool
    }

    private let lock = NSLock()
    private var borrowedAddresses: [UInt] = []
    private var events: [WipeEvent] = []

    var observer: RemoraLinkSensitiveBuffer.WipeObserver {
        { [weak self] address, byteCount, isAllZero in
            self?.lock.withLock {
                self?.events.append(
                    WipeEvent(address: address, byteCount: byteCount, isAllZero: isAllZero)
                )
            }
        }
    }

    func recordBorrow(_ bytes: UnsafeRawBufferPointer) -> Data {
        lock.withLock {
            borrowedAddresses.append(UInt(bitPattern: bytes.baseAddress!))
        }
        return Data(bytes)
    }

    func assertSingleExactWipe(
        file: StaticString,
        line: UInt
    ) {
        let snapshot = lock.withLock { (borrowedAddresses, events) }
        XCTAssertEqual(snapshot.0.count, 1, file: file, line: line)
        XCTAssertEqual(snapshot.1.count, 1, file: file, line: line)
        XCTAssertEqual(snapshot.1.first?.address, snapshot.0.first, file: file, line: line)
        XCTAssertGreaterThan(snapshot.1.first?.byteCount ?? 0, 0, file: file, line: line)
        XCTAssertEqual(snapshot.1.first?.isAllZero, true, file: file, line: line)
    }

    func assertSingleWipeWithoutBorrow(
        file: StaticString,
        line: UInt
    ) {
        let snapshot = lock.withLock { (borrowedAddresses, events) }
        XCTAssertTrue(snapshot.0.isEmpty, file: file, line: line)
        XCTAssertEqual(snapshot.1.count, 1, file: file, line: line)
        XCTAssertGreaterThan(snapshot.1.first?.address ?? 0, 0, file: file, line: line)
        XCTAssertGreaterThan(snapshot.1.first?.byteCount ?? 0, 0, file: file, line: line)
        XCTAssertEqual(snapshot.1.first?.isAllZero, true, file: file, line: line)
    }
}

private final class SerializedCallbackProbe: @unchecked Sendable {
    private let lock = NSLock()
    private var active = 0
    private var maximumActive = 0
    private var count = 0

    var maximumConcurrentCallbacks: Int {
        lock.withLock { maximumActive }
    }

    var callbackCount: Int {
        lock.withLock { count }
    }

    func run() {
        lock.withLock {
            active += 1
            maximumActive = max(maximumActive, active)
            count += 1
        }
        Thread.sleep(forTimeInterval: 0.002)
        lock.withLock { active -= 1 }
    }
}
