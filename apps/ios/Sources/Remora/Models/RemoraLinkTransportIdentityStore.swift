import Foundation
import Security

struct RemoraLinkTransportIdentityKey: Equatable, Hashable, Sendable {
    let service: String
    let account: String

    /// A hard v2 cutover. This namespace is intentionally unrelated to the
    /// former remote-pairing stores and is never populated by importing them.
    static let applicationV2 = Self(
        service: "com.remora.app.remora-link.v2.transport-identity",
        account: "application-transport-secret"
    )
}

struct RemoraLinkTransportIdentityStoragePolicy: Equatable, Sendable {
    enum Accessibility: Equatable, Sendable {
        case afterFirstUnlockThisDeviceOnly
        case unsupported
    }

    let accessibility: Accessibility
    let synchronizable: Bool

    static let backgroundCapableDeviceOnly = Self(
        accessibility: .afterFirstUnlockThisDeviceOnly,
        synchronizable: false
    )
}

/// Fixed-width application transport identity with intentionally redacted
/// textual representations. Callers can copy the bytes only at the narrow
/// native-to-Rust boundary.
struct RemoraLinkTransportIdentity: Equatable, Sendable,
    CustomStringConvertible, CustomDebugStringConvertible
{
    static let byteCount = 32

    private let storage: Data

    fileprivate init(validatedBytes: Data) {
        storage = validatedBytes
    }

    var count: Int { storage.count }

    func copyBytes() -> Data { storage }

    func withUnsafeBytes<Result>(
        _ body: (UnsafeRawBufferPointer) throws -> Result
    ) rethrows -> Result {
        try storage.withUnsafeBytes(body)
    }

    var description: String {
        "RemoraLinkTransportIdentity(<redacted \(storage.count) bytes>)"
    }

    var debugDescription: String { description }
}

enum RemoraLinkTransportIdentityStoreError: LocalizedError, Equatable, Sendable {
    case invalidCandidateLength
    case corruptStoredIdentity
    case entropyUnavailable(OSStatus)
    case keychainLocked
    case keychain(OSStatus)

    var errorDescription: String? {
        switch self {
        case .invalidCandidateLength:
            return "The Remora Link transport identity has an invalid length."
        case .corruptStoredIdentity:
            return "The stored Remora Link transport identity is corrupt. Pair hosts again after repairing secure storage."
        case .entropyUnavailable:
            return "The device could not create a Remora Link transport identity."
        case .keychainLocked:
            return "Unlock this device to use Remora Link."
        case .keychain:
            return "The Remora Link transport identity is unavailable."
        }
    }
}

struct RemoraLinkTransportIdentityStoredItem: Equatable, Sendable {
    let bytes: Data
    let policy: RemoraLinkTransportIdentityStoragePolicy
}

enum RemoraLinkTransportIdentityLookup {
    case found(RemoraLinkTransportIdentityStoredItem)
    case missing
    case unavailable(OSStatus)
}

enum RemoraLinkTransportIdentityCreateOutcome {
    case created
    case duplicate
    case unavailable(OSStatus)
}

protocol RemoraLinkTransportIdentitySecurity: Sendable {
    func load(key: RemoraLinkTransportIdentityKey) -> RemoraLinkTransportIdentityLookup
    func createIfAbsent(
        key: RemoraLinkTransportIdentityKey,
        candidate: UnsafeRawBufferPointer,
        policy: RemoraLinkTransportIdentityStoragePolicy
    ) -> RemoraLinkTransportIdentityCreateOutcome
}

enum RemoraLinkTransportIdentityEntropyResult: Sendable {
    case bytes(Data)
    case unavailable(OSStatus)
}

/// Keychain custody for one app-wide Remora Link v2 transport secret.
///
/// `SecItemAdd` is the atomic create primitive. A loser of a duplicate-item
/// race reloads and adopts the winner; it never overwrites an existing
/// identity. There is no host-scoped key and no legacy import path.
final class RemoraLinkTransportIdentityStore: Sendable {
    typealias EntropySource = @Sendable (Int) -> RemoraLinkTransportIdentityEntropyResult

    static let shared = RemoraLinkTransportIdentityStore()

    private let security: any RemoraLinkTransportIdentitySecurity
    private let entropy: EntropySource

    init(
        security: any RemoraLinkTransportIdentitySecurity = SystemRemoraLinkTransportIdentitySecurity(),
        entropy: @escaping EntropySource = RemoraLinkTransportIdentityStore.systemEntropy
    ) {
        self.security = security
        self.entropy = entropy
    }

    /// Load the existing identity or generate one candidate locally. Rust may
    /// instead use `loadOrCreate(candidate:)` so its entropy remains the source
    /// of the proposed bytes.
    func loadOrCreate() throws -> RemoraLinkTransportIdentity {
        switch security.load(key: .applicationV2) {
        case .found(let item):
            return try validated(item)
        case .unavailable(let status):
            throw mappedKeychainError(status)
        case .missing:
            break
        }

        var candidate: Data
        switch entropy(RemoraLinkTransportIdentity.byteCount) {
        case .bytes(let bytes):
            candidate = bytes
        case .unavailable(let status):
            throw RemoraLinkTransportIdentityStoreError.entropyUnavailable(status)
        }
        defer {
            candidate.withUnsafeMutableBytes { bytes in
                if let baseAddress = bytes.baseAddress, !bytes.isEmpty {
                    remoraLinkZeroizeMemory(baseAddress, byteCount: bytes.count)
                }
            }
        }
        return try candidate.withUnsafeBytes { candidateBytes in
            try createOrAdopt(candidate: candidateBytes)
        }
    }

    /// Atomically create the app identity from a caller-provided candidate, or
    /// return the already-persisted winner of a concurrent create race.
    func loadOrCreate(candidate: Data) throws -> RemoraLinkTransportIdentity {
        try candidate.withUnsafeBytes { candidateBytes in
            try loadOrCreate(candidate: candidateBytes)
        }
    }

    /// Raw borrowed form used by the UniFFI adapter so callback plaintext stays
    /// in its single explicitly wiped allocation through the synchronous
    /// Keychain operation.
    func loadOrCreate(
        candidate: UnsafeRawBufferPointer
    ) throws -> RemoraLinkTransportIdentity {
        guard candidate.count == RemoraLinkTransportIdentity.byteCount else {
            throw RemoraLinkTransportIdentityStoreError.invalidCandidateLength
        }
        switch security.load(key: .applicationV2) {
        case .found(let item):
            return try validated(item)
        case .unavailable(let status):
            throw mappedKeychainError(status)
        case .missing:
            return try createOrAdopt(candidate: candidate)
        }
    }

    private func createOrAdopt(
        candidate: UnsafeRawBufferPointer
    ) throws -> RemoraLinkTransportIdentity {
        guard candidate.count == RemoraLinkTransportIdentity.byteCount else {
            throw RemoraLinkTransportIdentityStoreError.invalidCandidateLength
        }
        switch security.createIfAbsent(
            key: .applicationV2,
            candidate: candidate,
            policy: .backgroundCapableDeviceOnly
        ) {
        case .created:
            return try reloadCreatedCandidate(candidate)
        case .duplicate:
            switch security.load(key: .applicationV2) {
            case .found(let winner):
                return try validated(winner)
            case .missing:
                throw RemoraLinkTransportIdentityStoreError.keychain(errSecItemNotFound)
            case .unavailable(let status):
                throw mappedKeychainError(status)
            }
        case .unavailable(let status):
            throw mappedKeychainError(status)
        }
    }

    /// Re-read across synchronizable variants after creation. The
    /// synchronizable attribute is part of the Keychain primary key, so a weak
    /// exact-key variant can race the add without making `SecItemAdd` fail.
    private func reloadCreatedCandidate(
        _ candidate: UnsafeRawBufferPointer
    ) throws -> RemoraLinkTransportIdentity {
        switch security.load(key: .applicationV2) {
        case .found(let item):
            guard candidate.elementsEqual(item.bytes) else {
                throw RemoraLinkTransportIdentityStoreError.corruptStoredIdentity
            }
            return try validated(item)
        case .missing:
            throw RemoraLinkTransportIdentityStoreError.keychain(errSecItemNotFound)
        case .unavailable(let status):
            throw mappedKeychainError(status)
        }
    }

    private func validated(
        _ item: RemoraLinkTransportIdentityStoredItem
    ) throws -> RemoraLinkTransportIdentity {
        guard item.policy == .backgroundCapableDeviceOnly,
              item.bytes.count == RemoraLinkTransportIdentity.byteCount
        else {
            throw RemoraLinkTransportIdentityStoreError.corruptStoredIdentity
        }
        return RemoraLinkTransportIdentity(validatedBytes: item.bytes)
    }

    private func mappedKeychainError(_ status: OSStatus) -> RemoraLinkTransportIdentityStoreError {
        status == errSecInteractionNotAllowed ? .keychainLocked : .keychain(status)
    }

    private static let systemEntropy: EntropySource = { count in
        var bytes = Data(repeating: 0, count: count)
        let status = bytes.withUnsafeMutableBytes { buffer -> OSStatus in
            guard let baseAddress = buffer.baseAddress else { return errSecParam }
            return SecRandomCopyBytes(kSecRandomDefault, count, baseAddress)
        }
        guard status == errSecSuccess else { return .unavailable(status) }
        return .bytes(bytes)
    }
}

struct SystemRemoraLinkTransportIdentitySecurity: RemoraLinkTransportIdentitySecurity {
    func load(key: RemoraLinkTransportIdentityKey) -> RemoraLinkTransportIdentityLookup {
        var result: CFTypeRef?
        let status = SecItemCopyMatching(
            lookupQuery(key: key) as CFDictionary,
            &result
        )
        switch status {
        case errSecSuccess:
            return decodeLookupResult(result)
        case errSecItemNotFound:
            return .missing
        default:
            return .unavailable(status)
        }
    }

    func createIfAbsent(
        key: RemoraLinkTransportIdentityKey,
        candidate: UnsafeRawBufferPointer,
        policy: RemoraLinkTransportIdentityStoragePolicy
    ) -> RemoraLinkTransportIdentityCreateOutcome {
        guard policy == .backgroundCapableDeviceOnly else {
            return .unavailable(errSecParam)
        }
        guard let candidateData = CFDataCreateWithBytesNoCopy(
            kCFAllocatorDefault,
            candidate.bindMemory(to: UInt8.self).baseAddress,
            candidate.count,
            kCFAllocatorNull
        ) else {
            return .unavailable(errSecAllocate)
        }
        let attributes = identityQuery(key: key).merging([
            kSecAttrSynchronizable as String: false,
            kSecAttrAccessible as String: kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly,
            kSecValueData as String: candidateData
        ]) { _, new in new }
        let status = SecItemAdd(attributes as CFDictionary, nil)
        switch status {
        case errSecSuccess:
            return .created
        case errSecDuplicateItem:
            return .duplicate
        default:
            return .unavailable(status)
        }
    }

    /// Query all exact-key variants so a synchronizable or duplicate record is
    /// observed and rejected instead of being hidden by a policy filter.
    func lookupQuery(key: RemoraLinkTransportIdentityKey) -> [String: Any] {
        identityQuery(key: key).merging([
            kSecAttrSynchronizable as String: kSecAttrSynchronizableAny,
            kSecReturnData as String: true,
            kSecReturnAttributes as String: true,
            kSecMatchLimit as String: kSecMatchLimitAll
        ]) { _, new in new }
    }

    /// `kSecMatchLimitAll` returns an array even when there is one match. More
    /// than one exact-key record is ambiguous and must never be adopted.
    func decodeLookupResult(_ result: CFTypeRef?) -> RemoraLinkTransportIdentityLookup {
        guard let matches = result as? [Any] else {
            return .unavailable(errSecDecode)
        }
        guard matches.count == 1 else {
            return .unavailable(matches.isEmpty ? errSecDecode : errSecDuplicateItem)
        }
        guard let item = matches[0] as? [String: Any],
              let bytes = item[kSecValueData as String] as? Data,
              let accessibility = item[kSecAttrAccessible as String] as? String
        else {
            return .unavailable(errSecDecode)
        }
        let storedAccessibility: RemoraLinkTransportIdentityStoragePolicy.Accessibility =
            accessibility == kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly as String
            ? .afterFirstUnlockThisDeviceOnly
            : .unsupported
        let synchronizable =
            (item[kSecAttrSynchronizable as String] as? NSNumber)?.boolValue ?? false
        return .found(
            RemoraLinkTransportIdentityStoredItem(
                bytes: bytes,
                policy: RemoraLinkTransportIdentityStoragePolicy(
                    accessibility: storedAccessibility,
                    synchronizable: synchronizable
                )
            )
        )
    }

    private func identityQuery(key: RemoraLinkTransportIdentityKey) -> [String: Any] {
        [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: key.service,
            kSecAttrAccount as String: key.account
        ]
    }
}
