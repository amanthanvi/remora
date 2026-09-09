import Foundation
import Security

final class NativeRelayJournalBackend: AppRelayJournalBackend {
    static let shared = NativeRelayJournalBackend()
    private let store: RemoraLinkJournalStore

    convenience init() {
        let root = FileManager.default.containerURL(
            forSecurityApplicationGroupIdentifier: "group.com.remora.app"
        )
        self.init(store: RemoraLinkJournalStore(
            directoryURL: root?.appendingPathComponent("RemoraRelay/v1", isDirectory: true),
            durabilityRootURL: root,
            excludeFromBackup: true
        ))
    }

    init(store: RemoraLinkJournalStore) {
        self.store = store
    }

    func discardForSecurityCutover() -> Bool {
        store.discardForSecurityCutover()
    }

    func load() async -> AppRelayJournalLoad {
        switch store.load() {
        case .missing: return .missing
        case .loaded(let snapshot):
            guard snapshot.payload.count <= 512 * 1_024 else { return .unavailable }
            return .loaded(snapshot: AppRelayJournalSnapshot(
                revision: snapshot.revision, payload: snapshot.payload
            ))
        case .corrupt, .unavailable: return .unavailable
        }
    }

    func compareAndSwap(
        expectedRevision: UInt64?, replacement: AppRelayJournalSnapshot
    ) async -> AppRelayJournalWriteOutcome {
        guard replacement.payload.count <= 512 * 1_024 else { return .unavailable }
        switch store.compareAndSwap(
            expectedRevision: expectedRevision,
            replacement: RemoraLinkJournalSnapshot(
                revision: replacement.revision, payload: replacement.payload
            )
        ) {
        case .stored: return .stored
        case .conflict: return .conflict
        case .unavailable, .invalidReplacement: return .unavailable
        }
    }
}

/// The passcode-only class is excluded from backup, escrow, and synchronization.
/// Locked devices defer relay work; never fall back to a restorable class.
/// This is backup-restore resistance, not a hardware monotonic counter.
final class NativeRelaySecretBackend: AppRelaySecretBackend {
    static let shared = NativeRelaySecretBackend()
    static let service = "com.remora.app.background-relay.v1"
    static let rollbackAnchor = "remora_relay_journal_anchor_v1"

    private let service: String

    init(service: String = NativeRelaySecretBackend.service) {
        self.service = service
    }

    func read(alias: String) async throws -> AppRelaySecretValue {
        try autoreleasepool {
            let item = try lookup(alias: alias, includeSecret: true)
            guard let item, !item.tombstone else { throw AppRelaySecretReadError.Missing }
            guard let value = item.attributes[kSecValueData as String],
                  CFGetTypeID(value as CFTypeRef) == CFDataGetTypeID() else {
                throw AppRelaySecretReadError.Unavailable
            }
            let data = value as! CFData
            let count = CFDataGetLength(data)
            guard (1...65_536).contains(count) else { throw AppRelaySecretReadError.Unavailable }
            // Borrow Security's result directly; no extra Data/Array plaintext copy.
            return AppRelaySecretValue(copying: UnsafeRawBufferPointer(
                start: CFDataGetBytePtr(data), count: count
            ))
        }
    }

    func revision(alias: String) async -> AppRelaySecretRevision {
        do {
            guard let item = try lookup(alias: alias) else { return .missing }
            return .found(revision: item.revision)
        } catch { return .unavailable }
    }

    func createIfAbsent(alias: String, value: AppRelaySecretValue) async -> AppRelaySecretCreateOutcome {
        defer { value.zeroize() }
        switch mutate(alias: alias, expected: nil, replacement: 1, value: value) {
        case .stored: return .created
        case .conflict: return .alreadyExists
        case .unavailable: return .unavailable
        }
    }

    func compareAndSwap(
        alias: String, expectedRevision: UInt64?, replacementRevision: UInt64,
        value: AppRelaySecretValue
    ) async -> AppRelaySecretCasOutcome {
        defer { value.zeroize() }
        return mutate(alias: alias, expected: expectedRevision, replacement: replacementRevision, value: value)
    }

    func compareAndTombstone(
        alias: String, expectedRevision: UInt64?, replacementRevision: UInt64
    ) async -> AppRelaySecretCasOutcome {
        guard alias != Self.rollbackAnchor else { return .unavailable }
        return mutate(alias: alias, expected: expectedRevision, replacement: replacementRevision, value: nil)
    }

    func write(alias: String, value: AppRelaySecretValue) async -> AppRelaySecretWriteOutcome {
        defer { value.zeroize() }
        guard alias != Self.rollbackAnchor else { return .unavailable }
        return replaceUnfenced(alias: alias, value: value)
    }

    func delete(alias: String) async -> AppRelaySecretWriteOutcome {
        guard alias != Self.rollbackAnchor else { return .unavailable }
        return replaceUnfenced(alias: alias, value: nil)
    }

    private func replaceUnfenced(alias: String, value: AppRelaySecretValue?) -> AppRelaySecretWriteOutcome {
        for _ in 0..<8 {
            do {
                let item = try lookup(alias: alias)
                if value == nil, item == nil || item?.tombstone == true { return .applied }
                let (revision, overflow) = (item?.revision ?? 0).addingReportingOverflow(1)
                guard !overflow else { return .unavailable }
                switch mutate(alias: alias, expected: item?.revision, replacement: revision, value: value) {
                case .stored: return .applied
                case .conflict: continue
                case .unavailable: return .unavailable
                }
            } catch { return .unavailable }
        }
        return .unavailable
    }

    private struct Item {
        let revision: UInt64
        let tombstone: Bool
        let metadata: Data
        let attributes: [String: Any]
    }

    private func lookup(alias: String, includeSecret: Bool = false) throws -> Item? {
        guard validAlias(alias) else { throw AppRelaySecretReadError.Unavailable }
        var query = identity(alias: alias)
        query[kSecAttrSynchronizable as String] = kSecAttrSynchronizableAny
        query[kSecMatchLimit as String] = kSecMatchLimitAll
        query[kSecReturnAttributes as String] = true
        query[kSecReturnData as String] = includeSecret
        var result: CFTypeRef?
        let status = SecItemCopyMatching(query as CFDictionary, &result)
        if status == errSecItemNotFound { return nil }
        guard status == errSecSuccess,
              let matches = result as? [[String: Any]], matches.count == 1,
              let attributes = matches.first,
              attributes[kSecAttrAccessible as String] as? String
                == kSecAttrAccessibleWhenPasscodeSetThisDeviceOnly as String,
              (attributes[kSecAttrSynchronizable as String] as? NSNumber)?.boolValue != true,
              let metadata = attributes[kSecAttrGeneric as String] as? Data,
              metadata.count == 10, metadata[0] == 1, metadata[9] <= 1 else {
            throw AppRelaySecretReadError.Unavailable
        }
        let revision = metadata[1...8].reduce(UInt64(0)) { ($0 << 8) | UInt64($1) }
        guard revision > 0 else { throw AppRelaySecretReadError.Unavailable }
        return Item(revision: revision, tombstone: metadata[9] == 1, metadata: metadata, attributes: attributes)
    }

    private func mutate(
        alias: String, expected: UInt64?, replacement: UInt64, value: AppRelaySecretValue?
    ) -> AppRelaySecretCasOutcome {
        guard validAlias(alias), replacement > (expected ?? 0),
              value == nil || (1...65_536).contains(value!.count) else { return .unavailable }
        return autoreleasepool {
            do {
                let current = try lookup(alias: alias)
                guard current?.revision == expected else { return .conflict }
                var revision = replacement.bigEndian
                var metadata = Data([1])
                withUnsafeBytes(of: &revision) { metadata.append(contentsOf: $0) }
                metadata.append(value == nil ? 1 : 0)
                let store: (UnsafeRawBufferPointer) -> AppRelaySecretCasOutcome = { bytes in
                    guard let borrowed = CFDataCreateWithBytesNoCopy(
                        kCFAllocatorDefault, bytes.bindMemory(to: UInt8.self).baseAddress,
                        bytes.count, kCFAllocatorNull
                    ) else { return .unavailable }
                    let attributes: [String: Any] = [
                        kSecAttrGeneric as String: metadata,
                        kSecValueData as String: borrowed,
                        kSecAttrAccessible as String: kSecAttrAccessibleWhenPasscodeSetThisDeviceOnly,
                        kSecAttrSynchronizable as String: false
                    ]
                    let status: OSStatus
                    if let current {
                        var query = self.identity(alias: alias)
                        query[kSecAttrSynchronizable as String] = false
                        query[kSecAttrAccessible as String] = kSecAttrAccessibleWhenPasscodeSetThisDeviceOnly
                        query[kSecAttrGeneric as String] = current.metadata
                        status = SecItemUpdate(query as CFDictionary, attributes as CFDictionary)
                    } else {
                        status = SecItemAdd(self.identity(alias: alias).merging(attributes) { _, new in new } as CFDictionary, nil)
                    }
                    switch status {
                    case errSecSuccess:
                        // Detect a racing weak/synchronizable variant without adopting it.
                        guard (try? self.lookup(alias: alias)) != nil else { return .unavailable }
                        return .stored
                    case errSecDuplicateItem, errSecItemNotFound: return .conflict
                    default: return .unavailable
                    }
                }
                if let value { return value.withUnsafeBytes(store) }
                return store(UnsafeRawBufferPointer(start: nil, count: 0))
            } catch { return .unavailable }
        }
    }

    private func identity(alias: String) -> [String: Any] {
        [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: alias,
            kSecUseDataProtectionKeychain as String: true
        ]
    }

    private func validAlias(_ alias: String) -> Bool {
        !alias.isEmpty && alias.utf8.count <= 128 && alias.utf8.allSatisfy {
            ($0 >= 97 && $0 <= 122) || ($0 >= 48 && $0 <= 57) || $0 == 95
        }
    }
}
