import CryptoKit
import Foundation
import Security

enum RemoraLinkKeyAssurance: Equatable, Sendable {
    case hardwareProtected
    case softwareDebugOnly
}

enum RemoraLinkKeyFailureReason: Equatable, Sendable {
    case invalidSlot
    case secureEnclaveUnavailable
    case softwareKeyNotAllowed
    case keychainFailure(OSStatus)
    case invalidPublicKey
    case signingFailure
}

enum RemoraLinkKeyStatus: Equatable, Sendable {
    case available(RemoraLinkKeyAssurance)
    case locked
    case missing
    case failed(RemoraLinkKeyFailureReason)
}

struct RemoraLinkPublicKey: Equatable, Sendable {
    /// SEC1/X9.63 uncompressed P-256 public key (`0x04 || X || Y`).
    let x963Representation: Data
    let assurance: RemoraLinkKeyAssurance
}

struct RemoraLinkSignature: Equatable, Sendable {
    /// ASN.1 DER ECDSA signature returned by Security.framework.
    let derRepresentation: Data
    let assurance: RemoraLinkKeyAssurance

    func withUnsafeBytes<Result>(
        _ body: (UnsafeRawBufferPointer) throws -> Result
    ) rethrows -> Result {
        try derRepresentation.withUnsafeBytes(body)
    }
}

enum RemoraLinkKeyDeletionOutcome: Equatable, Sendable {
    case deleted
    case alreadyMissing
}

enum RemoraLinkKeyStoreError: LocalizedError, Equatable {
    case invalidSlot
    case keychainLocked
    case keyNotFound
    case secureEnclaveUnavailable
    case softwareKeyNotAllowed
    case keychain(OSStatus)
    case invalidPublicKey
    case signingFailed

    var errorDescription: String? {
        switch self {
        case .invalidSlot:
            return "The pairing key reference is invalid."
        case .keychainLocked:
            return "Unlock this device to use the paired host."
        case .keyNotFound:
            return "The device pairing key is missing. Pair this host again."
        case .secureEnclaveUnavailable:
#if targetEnvironment(macCatalyst)
            return "This Mac cannot create the hardware-protected key required by Remora Link. Use SSH or Connected Computer instead."
#else
            return "This device cannot create the hardware-protected key required by Remora Link."
#endif
        case .softwareKeyNotAllowed:
            return "This pairing uses a development-only software key and cannot be used by this build. Pair the host again."
        case .keychain:
            return "The device pairing key is unavailable."
        case .invalidPublicKey:
            return "The device pairing key has an invalid public key."
        case .signingFailed:
            return "The device could not prove access to this paired host."
        }
    }
}

/// Key creation requirements expressed independently of Security.framework so
/// tests can assert the complete custody policy without manufacturing SecKey
/// instances. The system adapter maps this to a permanent P-256 private key,
/// `WhenUnlockedThisDeviceOnly`, and `privateKeyUsage` without user-presence
/// flags.
struct RemoraLinkKeyCreationPolicy: Equatable, Sendable {
    enum Protection: Equatable, Sendable {
        case secureEnclave
        case softwareDebugOnly
    }

    let protection: Protection
    let keySizeInBits: Int
    let whenUnlockedThisDeviceOnly: Bool
    let privateKeyUsageOnly: Bool
    let requiresUserPresence: Bool

    static func p256(_ protection: Protection) -> Self {
        Self(
            protection: protection,
            keySizeInBits: 256,
            whenUnlockedThisDeviceOnly: true,
            privateKeyUsageOnly: true,
            requiresUserPresence: false
        )
    }
}

enum RemoraLinkSecurityLookup {
    case found(handle: AnyObject, assurance: RemoraLinkKeyAssurance)
    case locked
    case missing
    case failed(OSStatus)
}

enum RemoraLinkSecurityCreation {
    case created(handle: AnyObject, assurance: RemoraLinkKeyAssurance)
    case failed(OSStatus)
}

enum RemoraLinkSecurityDataResult {
    case value(Data)
    case locked
    case failed(OSStatus?)
}

/// Injectable Security.framework seam. Private key handles remain opaque to
/// the store and are never returned from its product-facing API.
protocol RemoraLinkKeySecurity {
    func lookupPrivateKey(applicationTag: Data) -> RemoraLinkSecurityLookup
    func createPrivateKey(
        applicationTag: Data,
        policy: RemoraLinkKeyCreationPolicy
    ) -> RemoraLinkSecurityCreation
    func publicKeyX963(privateKey: AnyObject) -> RemoraLinkSecurityDataResult
    func signMessageP256SHA256(
        privateKey: AnyObject,
        message: UnsafeRawBufferPointer
    ) -> RemoraLinkSecurityDataResult
    func deletePrivateKey(applicationTag: Data) -> OSStatus
}

enum RemoraLinkSoftwareKeyPolicy: Equatable, Sendable {
    case disabled
    case debugBuildOnly

    static var buildDefault: Self {
#if DEBUG
        .debugBuildOnly
#else
        .disabled
#endif
    }

    var allowsSoftwareKey: Bool {
        self == .debugBuildOnly
    }
}

/// Apple-platform custody for one non-exportable P-256 signing key per opaque
/// Rust-owned slot. The slot is hashed before becoming a Keychain application
/// tag, so native persistence never stores a host ID, relay address, or other
/// pairing metadata.
final class RemoraLinkKeyStore: @unchecked Sendable {
    static let shared = RemoraLinkKeyStore()

    private static let namespace = "com.remora.app.remora-link.v2.signing"
    private static let maximumSlotBytes = 512

    private let security: any RemoraLinkKeySecurity
    private let softwareKeyPolicy: RemoraLinkSoftwareKeyPolicy

    init(
        security: any RemoraLinkKeySecurity = SystemRemoraLinkKeySecurity(),
        softwareKeyPolicy: RemoraLinkSoftwareKeyPolicy = .buildDefault
    ) {
        self.security = security
        self.softwareKeyPolicy = softwareKeyPolicy
    }

    func status(slot: String) -> RemoraLinkKeyStatus {
        guard let tag = applicationTag(for: slot) else { return .failed(.invalidSlot) }
        switch security.lookupPrivateKey(applicationTag: tag) {
        case .found(_, let assurance):
            guard assurance != .softwareDebugOnly || softwareKeyPolicy.allowsSoftwareKey else {
                return .failed(.softwareKeyNotAllowed)
            }
            return .available(assurance)
        case .locked:
            return .locked
        case .missing:
            return .missing
        case .failed(let status):
            return .failed(.keychainFailure(status))
        }
    }

    /// Return the fixed-width X9.63 public key for this slot, creating the
    /// private key when absent. Release policy never falls back from Secure
    /// Enclave to an exportable software private key.
    func createIfNeeded(slot: String) throws -> RemoraLinkPublicKey {
        guard let tag = applicationTag(for: slot) else {
            throw RemoraLinkKeyStoreError.invalidSlot
        }

        let resolved: (AnyObject, RemoraLinkKeyAssurance)
        switch security.lookupPrivateKey(applicationTag: tag) {
        case .found(let handle, let assurance):
            resolved = try accepted(handle: handle, assurance: assurance)
        case .locked:
            throw RemoraLinkKeyStoreError.keychainLocked
        case .failed(let status):
            throw RemoraLinkKeyStoreError.keychain(status)
        case .missing:
            resolved = try createPrivateKey(applicationTag: tag)
        }

        return try publicKey(handle: resolved.0, assurance: resolved.1)
    }

    /// Load the public half of an existing key without creating any Keychain
    /// material. A missing slot is a normal `nil` result; all custody and
    /// validation failures remain explicit errors.
    func load(slot: String) throws -> RemoraLinkPublicKey? {
        guard let tag = applicationTag(for: slot) else {
            throw RemoraLinkKeyStoreError.invalidSlot
        }
        switch security.lookupPrivateKey(applicationTag: tag) {
        case .found(let handle, let assurance):
            let accepted = try accepted(handle: handle, assurance: assurance)
            return try publicKey(handle: accepted.0, assurance: accepted.1)
        case .locked:
            throw RemoraLinkKeyStoreError.keychainLocked
        case .missing:
            return nil
        case .failed(let status):
            throw RemoraLinkKeyStoreError.keychain(status)
        }
    }

    /// Sign Rust-owned canonical transcript bytes in message mode exactly once.
    /// Security.framework performs SHA-256 internally and returns DER ECDSA.
    func sign(
        slot: String,
        message: UnsafeRawBufferPointer
    ) throws -> RemoraLinkSignature {
        guard let tag = applicationTag(for: slot) else {
            throw RemoraLinkKeyStoreError.invalidSlot
        }
        let handle: AnyObject
        let assurance: RemoraLinkKeyAssurance
        switch security.lookupPrivateKey(applicationTag: tag) {
        case .found(let foundHandle, let foundAssurance):
            (handle, assurance) = try accepted(handle: foundHandle, assurance: foundAssurance)
        case .locked:
            throw RemoraLinkKeyStoreError.keychainLocked
        case .missing:
            throw RemoraLinkKeyStoreError.keyNotFound
        case .failed(let status):
            throw RemoraLinkKeyStoreError.keychain(status)
        }

        switch security.signMessageP256SHA256(privateKey: handle, message: message) {
        case .value(let signature) where !signature.isEmpty:
            return RemoraLinkSignature(derRepresentation: signature, assurance: assurance)
        case .locked:
            throw RemoraLinkKeyStoreError.keychainLocked
        case .value, .failed:
            throw RemoraLinkKeyStoreError.signingFailed
        }
    }

    func sign(slot: String, message: Data) throws -> RemoraLinkSignature {
        try message.withUnsafeBytes { bytes in
            try sign(slot: slot, message: bytes)
        }
    }

    /// Idempotently delete one per-host private key. This removes only the
    /// Rust-selected slot and never scans or imports retired v1 stores.
    func delete(slot: String) throws -> RemoraLinkKeyDeletionOutcome {
        guard let tag = applicationTag(for: slot) else {
            throw RemoraLinkKeyStoreError.invalidSlot
        }
        let status = security.deletePrivateKey(applicationTag: tag)
        switch status {
        case errSecSuccess:
            return .deleted
        case errSecItemNotFound:
            return .alreadyMissing
        default:
            if status == errSecInteractionNotAllowed {
                throw RemoraLinkKeyStoreError.keychainLocked
            }
            throw RemoraLinkKeyStoreError.keychain(status)
        }
    }

    private func createPrivateKey(
        applicationTag: Data
    ) throws -> (AnyObject, RemoraLinkKeyAssurance) {
        switch security.createPrivateKey(
            applicationTag: applicationTag,
            policy: .p256(.secureEnclave)
        ) {
        case .created(let handle, let assurance):
            guard assurance == .hardwareProtected else {
                return try rejectUnexpectedSoftwareKey(applicationTag: applicationTag)
            }
            return (handle, assurance)
        case .failed:
            break
        }

        // A concurrent creator may have inserted the permanent key after the
        // initial lookup. Prefer that key before considering debug fallback.
        switch security.lookupPrivateKey(applicationTag: applicationTag) {
        case .found(let handle, let assurance):
            return try accepted(handle: handle, assurance: assurance)
        case .locked:
            throw RemoraLinkKeyStoreError.keychainLocked
        case .failed(let status):
            throw RemoraLinkKeyStoreError.keychain(status)
        case .missing:
            break
        }

        guard softwareKeyPolicy.allowsSoftwareKey else {
            throw RemoraLinkKeyStoreError.secureEnclaveUnavailable
        }
        switch security.createPrivateKey(
            applicationTag: applicationTag,
            policy: .p256(.softwareDebugOnly)
        ) {
        case .created(let handle, let assurance):
            guard assurance == .softwareDebugOnly else {
                return (handle, assurance)
            }
            return (handle, assurance)
        case .failed(let status):
            // The software create may also have lost a duplicate-item race.
            if case .found(let handle, let assurance) = security.lookupPrivateKey(
                applicationTag: applicationTag
            ) {
                return try accepted(handle: handle, assurance: assurance)
            }
            throw RemoraLinkKeyStoreError.keychain(status)
        }
    }

    private func rejectUnexpectedSoftwareKey(
        applicationTag: Data
    ) throws -> (AnyObject, RemoraLinkKeyAssurance) {
        // A provider reporting software assurance for a Secure Enclave request
        // is a failed-closed contract violation. Delete the unexpected key so a
        // later retry cannot accidentally adopt it.
        _ = security.deletePrivateKey(applicationTag: applicationTag)
        throw RemoraLinkKeyStoreError.secureEnclaveUnavailable
    }

    private func accepted(
        handle: AnyObject,
        assurance: RemoraLinkKeyAssurance
    ) throws -> (AnyObject, RemoraLinkKeyAssurance) {
        if assurance == .softwareDebugOnly, !softwareKeyPolicy.allowsSoftwareKey {
            throw RemoraLinkKeyStoreError.softwareKeyNotAllowed
        }
        return (handle, assurance)
    }

    private func publicKey(
        handle: AnyObject,
        assurance: RemoraLinkKeyAssurance
    ) throws -> RemoraLinkPublicKey {
        switch security.publicKeyX963(privateKey: handle) {
        case .value(let data) where data.count == 65 && data.first == 0x04:
            return RemoraLinkPublicKey(x963Representation: data, assurance: assurance)
        case .locked:
            throw RemoraLinkKeyStoreError.keychainLocked
        case .value, .failed:
            throw RemoraLinkKeyStoreError.invalidPublicKey
        }
    }

    private func applicationTag(for slot: String) -> Data? {
        guard !slot.isEmpty,
              let slotData = slot.data(using: .utf8),
              slotData.count <= Self.maximumSlotBytes,
              !slot.unicodeScalars.contains(where: { $0.value == 0 }) else {
            return nil
        }
        let digest = SHA256.hash(data: slotData)
        let suffix = digest.map { String(format: "%02x", $0) }.joined()
        return Data("\(Self.namespace).\(suffix)".utf8)
    }
}

final class SystemRemoraLinkKeySecurity: RemoraLinkKeySecurity {
    func lookupPrivateKey(applicationTag: Data) -> RemoraLinkSecurityLookup {
        var result: CFTypeRef?
        let status = SecItemCopyMatching(
            privateKeyQuery(applicationTag: applicationTag, returnReference: true) as CFDictionary,
            &result
        )
        switch status {
        case errSecSuccess:
            guard let result,
                  CFGetTypeID(result) == SecKeyGetTypeID() else {
                return .failed(errSecInternalError)
            }
            let key = result as! SecKey
            return .found(handle: key, assurance: assurance(for: key))
        case errSecInteractionNotAllowed:
            return .locked
        case errSecItemNotFound:
            return .missing
        default:
            return .failed(status)
        }
    }

    func createPrivateKey(
        applicationTag: Data,
        policy: RemoraLinkKeyCreationPolicy
    ) -> RemoraLinkSecurityCreation {
        guard policy.keySizeInBits == 256,
              policy.whenUnlockedThisDeviceOnly,
              policy.privateKeyUsageOnly,
              !policy.requiresUserPresence else {
            return .failed(errSecParam)
        }

        var accessError: Unmanaged<CFError>?
        guard let accessControl = SecAccessControlCreateWithFlags(
            nil,
            kSecAttrAccessibleWhenUnlockedThisDeviceOnly,
            [.privateKeyUsage],
            &accessError
        ) else {
            return .failed(osStatus(from: accessError?.takeRetainedValue()) ?? errSecInternalError)
        }

        var privateAttributes: [CFString: Any] = [
            kSecAttrIsPermanent: true,
            kSecAttrApplicationTag: applicationTag,
            kSecAttrAccessControl: accessControl
        ]
        if policy.protection == .softwareDebugOnly {
            // Access control already carries the device-only accessibility.
            // Keep this branch explicit so production cannot silently remove
            // the Secure Enclave token identifier.
            privateAttributes[kSecAttrSynchronizable] = false
        }

        var attributes: [CFString: Any] = [
            kSecAttrKeyType: kSecAttrKeyTypeECSECPrimeRandom,
            kSecAttrKeySizeInBits: policy.keySizeInBits,
            kSecPrivateKeyAttrs: privateAttributes
        ]
        if policy.protection == .secureEnclave {
            attributes[kSecAttrTokenID] = kSecAttrTokenIDSecureEnclave
        }

        var error: Unmanaged<CFError>?
        guard let key = SecKeyCreateRandomKey(attributes as CFDictionary, &error) else {
            return .failed(osStatus(from: error?.takeRetainedValue()) ?? errSecInternalError)
        }
        return .created(handle: key, assurance: assurance(for: key))
    }

    func publicKeyX963(privateKey: AnyObject) -> RemoraLinkSecurityDataResult {
        guard CFGetTypeID(privateKey) == SecKeyGetTypeID() else {
            return .failed(errSecParam)
        }
        let privateKey = privateKey as! SecKey
        guard let publicKey = SecKeyCopyPublicKey(privateKey) else {
            return .failed(nil)
        }
        var error: Unmanaged<CFError>?
        guard let data = SecKeyCopyExternalRepresentation(publicKey, &error) as Data? else {
            let failure = error?.takeRetainedValue()
            if osStatus(from: failure) == errSecInteractionNotAllowed {
                return .locked
            }
            return .failed(osStatus(from: failure))
        }
        return .value(data)
    }

    func signMessageP256SHA256(
        privateKey: AnyObject,
        message: UnsafeRawBufferPointer
    ) -> RemoraLinkSecurityDataResult {
        guard CFGetTypeID(privateKey) == SecKeyGetTypeID() else {
            return .failed(errSecParam)
        }
        let privateKey = privateKey as! SecKey
        let algorithm = SecKeyAlgorithm.ecdsaSignatureMessageX962SHA256
        guard SecKeyIsAlgorithmSupported(privateKey, .sign, algorithm) else {
            return .failed(errSecParam)
        }
        guard let messageData = CFDataCreateWithBytesNoCopy(
            kCFAllocatorDefault,
            message.bindMemory(to: UInt8.self).baseAddress,
            message.count,
            kCFAllocatorNull
        ) else {
            return .failed(errSecAllocate)
        }
        var error: Unmanaged<CFError>?
        guard let signature = SecKeyCreateSignature(
            privateKey,
            algorithm,
            messageData,
            &error
        ) as Data? else {
            let failure = error?.takeRetainedValue()
            if osStatus(from: failure) == errSecInteractionNotAllowed {
                return .locked
            }
            return .failed(osStatus(from: failure))
        }
        return .value(signature)
    }

    func deletePrivateKey(applicationTag: Data) -> OSStatus {
        SecItemDelete(
            privateKeyQuery(applicationTag: applicationTag, returnReference: false) as CFDictionary
        )
    }

    private func privateKeyQuery(
        applicationTag: Data,
        returnReference: Bool
    ) -> [CFString: Any] {
        var query: [CFString: Any] = [
            kSecClass: kSecClassKey,
            kSecAttrApplicationTag: applicationTag,
            kSecAttrKeyType: kSecAttrKeyTypeECSECPrimeRandom,
            kSecAttrKeyClass: kSecAttrKeyClassPrivate
        ]
        if returnReference {
            query[kSecReturnRef] = true
            query[kSecMatchLimit] = kSecMatchLimitOne
        }
        return query
    }

    private func assurance(for key: SecKey) -> RemoraLinkKeyAssurance {
        guard let attributes = SecKeyCopyAttributes(key) as? [CFString: Any],
              let tokenID = attributes[kSecAttrTokenID] as? String,
              tokenID == (kSecAttrTokenIDSecureEnclave as String) else {
            return .softwareDebugOnly
        }
        return .hardwareProtected
    }

    private func osStatus(from error: CFError?) -> OSStatus? {
        guard let error else { return nil }
        guard CFErrorGetDomain(error) as String == NSOSStatusErrorDomain else { return nil }
        return OSStatus(CFErrorGetCode(error))
    }
}
