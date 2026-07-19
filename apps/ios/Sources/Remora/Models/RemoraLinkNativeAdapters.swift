import CryptoKit
import Darwin
import Foundation

enum RemoraLinkNativeConfigurationStatus: Equatable, Sendable {
    case notConfigured
    case configuring
    case available
    case unavailable
}

@MainActor
protocol RemoraLinkReachabilityObserving: AnyObject {
    func bind(appModel: AppModel)
    func start()
}

extension NetworkReachabilityObserver: RemoraLinkReachabilityObserving {}

typealias RemoraLinkConfigurator = @MainActor @Sendable (
    _ client: AppClient,
    _ adapters: RemoraLinkNativeAdapters
) async throws -> Void

/// One owned allocation for callback plaintext. It never exposes an owning
/// `Data`, and every exit path wipes the exact allocation with `memset_s`
/// before deallocation. The observer is an instrumentation seam used to prove
/// address identity and post-wipe contents in tests.
final class RemoraLinkSensitiveBuffer: @unchecked Sendable {
    typealias WipeObserver = @Sendable (
        _ allocationAddress: UInt,
        _ byteCount: Int,
        _ isAllZero: Bool
    ) -> Void

    let count: Int

    private let allocation: UnsafeMutableRawPointer
    private let lock = NSLock()
    private let wipeObserver: WipeObserver?
    private var isWiped = false

    init(copying secret: AppRelaySecretValue, wipeObserver: WipeObserver? = nil) {
        count = secret.count
        allocation = UnsafeMutableRawPointer.allocate(
            byteCount: max(count, 1),
            alignment: MemoryLayout<UInt64>.alignment
        )
        self.wipeObserver = wipeObserver
        secret.withUnsafeBytes { source in
            if let baseAddress = source.baseAddress, source.count > 0 {
                allocation.copyMemory(from: baseAddress, byteCount: source.count)
            }
        }
        secret.zeroize()
    }

    deinit {
        wipe()
        allocation.deallocate()
    }

    func withUnsafeBytes<Result>(
        _ body: (UnsafeRawBufferPointer) throws -> Result
    ) rethrows -> Result {
        lock.lock()
        defer { lock.unlock() }
        precondition(!isWiped, "Remora Link sensitive buffer used after wipe")
        return try body(UnsafeRawBufferPointer(start: allocation, count: count))
    }

    func wipe() {
        lock.lock()
        guard !isWiped else {
            lock.unlock()
            return
        }
        if count > 0 {
            remoraLinkZeroizeMemory(allocation, byteCount: count)
        }
        let observer = wipeObserver
        let address = UInt(bitPattern: allocation)
        let allZero: Bool
        if observer == nil || count == 0 {
            allZero = true
        } else {
            allZero = UnsafeRawBufferPointer(start: allocation, count: count)
                .allSatisfy { $0 == 0 }
        }
        isWiped = true
        lock.unlock()
        observer?(address, count, allZero)
    }
}

@inline(never)
func remoraLinkZeroizeMemory(
    _ pointer: UnsafeMutableRawPointer,
    byteCount: Int
) {
    guard byteCount > 0 else { return }
    let status = memset_s(pointer, byteCount, 0, byteCount)
    precondition(status == 0, "failed to wipe Remora Link sensitive memory")
}

private final class RemoraLinkCancellationFlag: @unchecked Sendable {
    private let lock = NSLock()
    private var cancelled = false

    var isCancelled: Bool {
        lock.withLock { cancelled }
    }

    func cancel() {
        lock.withLock { cancelled = true }
    }
}

/// Runs blocking native custody operations away from cooperative Swift tasks
/// while preserving one total order across the three UniFFI callback surfaces.
/// Checked continuations guarantee every submitted operation resumes exactly
/// once, including throwing operations.
final class RemoraLinkSerializedExecutor: @unchecked Sendable {
    private let queue: DispatchQueue

    init(label: String = "com.remora.app.remora-link.v2.native-custody") {
        queue = DispatchQueue(label: label, qos: .userInitiated)
    }

    func perform<Value: Sendable>(
        _ operation: @escaping @Sendable () -> Value
    ) async -> Value {
        await withCheckedContinuation { continuation in
            queue.async {
                continuation.resume(returning: operation())
            }
        }
    }

    func performThrowing<Value: Sendable>(
        _ operation: @escaping @Sendable () throws -> Value
    ) async throws -> Value {
        try await withCheckedThrowingContinuation { continuation in
            queue.async {
                continuation.resume(with: Result(catching: operation))
            }
        }
    }

    /// Cancellation-aware variant for secret-bearing operations. The queued
    /// closure always runs, allowing its `defer` to wipe owned plaintext; a
    /// cancellation observed before execution prevents the custody mutation.
    func performSensitiveThrowing<Value: Sendable>(
        _ operation: @escaping @Sendable (_ isCancelled: Bool) throws -> Value
    ) async throws -> Value {
        let cancellation = RemoraLinkCancellationFlag()
        return try await withTaskCancellationHandler {
            try await withCheckedThrowingContinuation { continuation in
                queue.async {
                    continuation.resume(
                        with: Result {
                            try operation(cancellation.isCancelled)
                        }
                    )
                }
            }
        } onCancel: {
            cancellation.cancel()
        }
    }
}

final class RemoraLinkJournalNativeAdapter: AppRemoraLinkJournalBackend, @unchecked Sendable {
    typealias Load = @Sendable () -> RemoraLinkJournalLoadStatus
    typealias CompareAndSwap = @Sendable (
        UInt64?,
        RemoraLinkJournalSnapshot
    ) -> RemoraLinkJournalWriteOutcome

    private let executor: RemoraLinkSerializedExecutor
    private let nativeLoad: Load
    private let nativeCompareAndSwap: CompareAndSwap

    convenience init(
        store: RemoraLinkJournalStore,
        executor: RemoraLinkSerializedExecutor = RemoraLinkSerializedExecutor()
    ) {
        self.init(
            executor: executor,
            load: { store.load() },
            compareAndSwap: { expectedRevision, replacement in
                store.compareAndSwap(
                    expectedRevision: expectedRevision,
                    replacement: replacement
                )
            }
        )
    }

    init(
        executor: RemoraLinkSerializedExecutor = RemoraLinkSerializedExecutor(),
        load: @escaping Load,
        compareAndSwap: @escaping CompareAndSwap
    ) {
        self.executor = executor
        nativeLoad = load
        nativeCompareAndSwap = compareAndSwap
    }

    func load() async -> AppRemoraLinkJournalLoad {
        let result = await executor.perform(nativeLoad)
        switch result {
        case .missing:
            return .missing
        case .loaded(let snapshot):
            return .loaded(
                snapshot: AppRemoraLinkJournalSnapshot(
                    revision: snapshot.revision,
                    payload: snapshot.payload
                )
            )
        case .corrupt, .unavailable:
            return .unavailable
        }
    }

    func compareAndSwap(
        expectedRevision: UInt64?,
        replacement: AppRemoraLinkJournalSnapshot
    ) async -> AppRemoraLinkJournalWriteOutcome {
        let nativeReplacement = RemoraLinkJournalSnapshot(
            revision: replacement.revision,
            payload: replacement.payload
        )
        let result = await executor.perform { [nativeCompareAndSwap] in
            nativeCompareAndSwap(expectedRevision, nativeReplacement)
        }
        switch result {
        case .stored:
            return .stored
        case .conflict:
            return .conflict
        case .unavailable, .invalidReplacement:
            return .unavailable
        }
    }
}

final class RemoraLinkTransportIdentityNativeAdapter:
    AppRemoraLinkTransportIdentityBackend, @unchecked Sendable
{
    typealias LoadOrCreate = @Sendable (UnsafeRawBufferPointer) throws -> AppRelaySecretValue

    private let executor: RemoraLinkSerializedExecutor
    private let nativeLoadOrCreate: LoadOrCreate
    private let inputWipeObserver: RemoraLinkSensitiveBuffer.WipeObserver?

    convenience init(
        store: RemoraLinkTransportIdentityStore,
        executor: RemoraLinkSerializedExecutor = RemoraLinkSerializedExecutor()
    ) {
        self.init(
            executor: executor,
            loadOrCreate: { candidate in
                let identity = try store.loadOrCreate(candidate: candidate)
                return identity.withUnsafeBytes { AppRelaySecretValue(copying: $0) }
            }
        )
    }

    init(
        executor: RemoraLinkSerializedExecutor = RemoraLinkSerializedExecutor(),
        inputWipeObserver: RemoraLinkSensitiveBuffer.WipeObserver? = nil,
        loadOrCreate: @escaping LoadOrCreate
    ) {
        self.executor = executor
        self.inputWipeObserver = inputWipeObserver
        nativeLoadOrCreate = loadOrCreate
    }

    func loadOrCreate(candidate: AppRelaySecretValue) async throws -> AppRelaySecretValue {
        let ownedCandidate = RemoraLinkSensitiveBuffer(
            copying: candidate,
            wipeObserver: inputWipeObserver
        )

        do {
            return try await executor.performSensitiveThrowing { [nativeLoadOrCreate] cancelled in
                defer { ownedCandidate.wipe() }
                guard !cancelled else { throw CancellationError() }
                return try ownedCandidate.withUnsafeBytes { candidateBytes in
                    let result = try nativeLoadOrCreate(candidateBytes)
                    guard result.count == RemoraLinkTransportIdentity.byteCount else {
                        result.zeroize()
                        throw RemoraLinkTransportIdentityStoreError.corruptStoredIdentity
                    }
                    return result
                }
            }
        } catch {
            throw AppRemoraLinkTransportIdentityError.Unavailable
        }
    }
}

final class RemoraLinkDeviceKeyNativeAdapter: AppRemoraLinkDeviceKeyBackend, @unchecked Sendable {
    typealias Create = @Sendable (String) throws -> RemoraLinkPublicKey
    typealias Load = @Sendable (String) throws -> RemoraLinkPublicKey?
    typealias Sign = @Sendable (String, UnsafeRawBufferPointer) throws -> RemoraLinkSignature
    typealias Delete = @Sendable (String) throws -> RemoraLinkKeyDeletionOutcome

    private static let maximumAuthenticatedHostIDBytes = 1_024
    private static let slotPrefix = "remora-link:v2:ios:"
    private static let slotDomain = Data(
        "com.remora.app/remora-link-v2/device-key-slot".utf8
    )

    private let executor: RemoraLinkSerializedExecutor
    private let nativeCreate: Create
    private let nativeLoad: Load
    private let nativeSign: Sign
    private let nativeDelete: Delete
    private let inputWipeObserver: RemoraLinkSensitiveBuffer.WipeObserver?

    convenience init(
        store: RemoraLinkKeyStore,
        executor: RemoraLinkSerializedExecutor = RemoraLinkSerializedExecutor()
    ) {
        self.init(
            executor: executor,
            create: { try store.createIfNeeded(slot: $0) },
            load: { try store.load(slot: $0) },
            sign: { try store.sign(slot: $0, message: $1) },
            delete: { try store.delete(slot: $0) }
        )
    }

    init(
        executor: RemoraLinkSerializedExecutor = RemoraLinkSerializedExecutor(),
        inputWipeObserver: RemoraLinkSensitiveBuffer.WipeObserver? = nil,
        create: @escaping Create,
        load: @escaping Load,
        sign: @escaping Sign,
        delete: @escaping Delete
    ) {
        self.executor = executor
        self.inputWipeObserver = inputWipeObserver
        nativeCreate = create
        nativeLoad = load
        nativeSign = sign
        nativeDelete = delete
    }

    func ensureHardwareKey(hostId: String) async throws -> AppRemoraLinkHardwareKey {
        let slot: String
        do {
            slot = try Self.slot(authenticatedHostID: hostId)
        } catch {
            throw AppRemoraLinkDeviceKeyError.Unavailable
        }

        do {
            let key = try await executor.performThrowing { [nativeCreate] in
                try nativeCreate(slot)
            }
            return Self.hardwareKey(slot: slot, key: key)
        } catch {
            throw Self.callbackError(for: error)
        }
    }

    func loadHardwareKey(slot: String) async throws -> AppRemoraLinkHardwareKeyLoad {
        do {
            let key = try await executor.performThrowing { [nativeLoad] in
                try nativeLoad(slot)
            }
            guard let key else { return .missing }
            return .loaded(key: Self.hardwareKey(slot: slot, key: key))
        } catch {
            throw Self.callbackError(for: error)
        }
    }

    func signMessage(
        slot: String,
        message: AppRelaySecretValue
    ) async throws -> AppRelaySecretValue {
        let ownedMessage = RemoraLinkSensitiveBuffer(
            copying: message,
            wipeObserver: inputWipeObserver
        )

        do {
            // The native store receives the canonical message in message mode
            // once. Security.framework performs the sole SHA-256 operation.
            return try await executor.performSensitiveThrowing { [nativeSign] cancelled in
                defer { ownedMessage.wipe() }
                guard !cancelled else { throw CancellationError() }
                let signature = try ownedMessage.withUnsafeBytes { canonicalBytes in
                    try nativeSign(slot, canonicalBytes)
                }
                return signature.withUnsafeBytes { AppRelaySecretValue(copying: $0) }
            }
        } catch {
            throw Self.callbackError(for: error)
        }
    }

    func deleteHardwareKey(slot: String) async throws -> AppRemoraLinkKeyDeletionStatus {
        do {
            let result = try await executor.performThrowing { [nativeDelete] in
                try nativeDelete(slot)
            }
            switch result {
            case .deleted:
                return .deleted
            case .alreadyMissing:
                return .alreadyMissing
            }
        } catch {
            throw Self.callbackError(for: error)
        }
    }

    static func slot(authenticatedHostID: String) throws -> String {
        let bytes = Data(authenticatedHostID.utf8)
        guard !bytes.isEmpty,
              bytes.count <= maximumAuthenticatedHostIDBytes,
              !authenticatedHostID.unicodeScalars.contains(
                  where: CharacterSet.controlCharacters.contains
              ) else {
            throw RemoraLinkKeyStoreError.invalidSlot
        }
        var hasher = SHA256()
        hasher.update(data: slotDomain)
        hasher.update(data: Data([0]))
        hasher.update(data: bytes)
        let encoded = Data(hasher.finalize())
            .base64EncodedString()
            .replacingOccurrences(of: "+", with: "-")
            .replacingOccurrences(of: "/", with: "_")
            .replacingOccurrences(of: "=", with: "")
        return slotPrefix + encoded
    }

    private static func hardwareKey(
        slot: String,
        key: RemoraLinkPublicKey
    ) -> AppRemoraLinkHardwareKey {
        AppRemoraLinkHardwareKey(
            slot: slot,
            publicKeySec1: key.x963Representation,
            assurance: assurance(for: key.assurance)
        )
    }

    private static func assurance(
        for native: RemoraLinkKeyAssurance
    ) -> AppRemoraLinkKeyAssurance {
        switch native {
        case .hardwareProtected:
            return .secureEnclave
        case .softwareDebugOnly:
            return .softwareDebugOnly
        }
    }

    private static func callbackError(for error: Error) -> AppRemoraLinkDeviceKeyError {
        if let callbackError = error as? AppRemoraLinkDeviceKeyError {
            return callbackError
        }
        guard let error = error as? RemoraLinkKeyStoreError else {
            return .Unavailable
        }
        switch error {
        case .invalidSlot:
            return .Unavailable
        case .invalidPublicKey:
            return .Invalidated
        case .keychainLocked:
            return .Locked
        case .keyNotFound:
            return .Missing
        case .secureEnclaveUnavailable, .softwareKeyNotAllowed:
            return .HardwareUnavailable
        case .keychain:
            return .Unavailable
        case .signingFailed:
            return .InvalidSignature
        }
    }
}

/// Process-wide strong ownership for the three objects registered with Rust.
/// The shared serial executor also prevents a callback from observing a
/// partially completed operation on another native custody surface.
final class RemoraLinkNativeAdapters: @unchecked Sendable {
    static let shared = RemoraLinkNativeAdapters()

    let journal: RemoraLinkJournalNativeAdapter
    let transportIdentity: RemoraLinkTransportIdentityNativeAdapter
    let deviceKeys: RemoraLinkDeviceKeyNativeAdapter

    init(
        journalStore: RemoraLinkJournalStore = .shared,
        transportIdentityStore: RemoraLinkTransportIdentityStore = .shared,
        keyStore: RemoraLinkKeyStore = .shared
    ) {
        let executor = RemoraLinkSerializedExecutor()
        journal = RemoraLinkJournalNativeAdapter(store: journalStore, executor: executor)
        transportIdentity = RemoraLinkTransportIdentityNativeAdapter(
            store: transportIdentityStore,
            executor: executor
        )
        deviceKeys = RemoraLinkDeviceKeyNativeAdapter(store: keyStore, executor: executor)
    }
}
