import Foundation
import Security

/// Keychain-backed implementation of the Rust `TerminalSshTrustBackend`
/// callback interface. Stores per-host SHA-256 fingerprints under a
/// dedicated service so they don't collide with the SSH credential
/// keychain entries (`SSHCredentialStore`).
final class SwiftSshTrustBackend: TerminalSshTrustBackend, @unchecked Sendable {
    static let shared = SwiftSshTrustBackend()

    private let service = "com.remora.app.ssh.trust"

    private init() {}

    /// Look up a pinned fingerprint.
    ///
    /// Only `errSecItemNotFound` means "this host is new". Every other
    /// keychain status — a locked keychain, a denied entitlement, a corrupt
    /// item — is a storage failure and must be thrown, not flattened to `nil`.
    /// Returning `nil` there would tell Rust the host is unknown and quietly
    /// downgrade an already-pinned host back to trust-on-first-use.
    func read(host: String, port: UInt16) throws -> String? {
        let query: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: account(host: host, port: port),
            kSecReturnData as String: true,
            kSecMatchLimit as String: kSecMatchLimitOne,
        ]
        var item: CFTypeRef?
        let status = SecItemCopyMatching(query as CFDictionary, &item)
        if status == errSecItemNotFound {
            return nil
        }
        guard status == errSecSuccess else {
            throw SshTrustStoreError.Unavailable(
                detail: "keychain read failed for \(host):\(port) (OSStatus \(status))"
            )
        }
        guard let data = item as? Data,
              let value = String(data: data, encoding: .utf8) else {
            throw SshTrustStoreError.Unavailable(
                detail: "keychain item for \(host):\(port) is not decodable UTF-8"
            )
        }
        return value
    }

    func write(host: String, port: UInt16, fingerprint: String) {
        let account = account(host: host, port: port)
        guard let data = fingerprint.data(using: .utf8) else { return }
        let addAttributes: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: account,
            kSecAttrAccessible as String: kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly,
            kSecValueData as String: data,
        ]
        let addStatus = SecItemAdd(addAttributes as CFDictionary, nil)
        if addStatus == errSecDuplicateItem {
            let query: [String: Any] = [
                kSecClass as String: kSecClassGenericPassword,
                kSecAttrService as String: service,
                kSecAttrAccount as String: account,
            ]
            let updates: [String: Any] = [
                kSecValueData as String: data,
                kSecAttrAccessible as String: kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly,
            ]
            SecItemUpdate(query as CFDictionary, updates as CFDictionary)
        }
    }

    func remove(host: String, port: UInt16) {
        let query: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: account(host: host, port: port),
        ]
        SecItemDelete(query as CFDictionary)
    }

    private func account(host: String, port: UInt16) -> String {
        "\(host.lowercased()):\(port)"
    }
}
