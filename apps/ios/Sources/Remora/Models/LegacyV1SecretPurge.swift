import Foundation
import Security
import UIKit

/// Best-effort removal of credentials written by the retired v1 flow.
///
/// There is intentionally no completion marker: every cold launch repeats both
/// idempotent deletions, so an interrupted or locked-device launch self-heals.
@MainActor
final class LegacyV1SecretPurge: NSObject {
    typealias DeleteItem = ([String: Any]) -> OSStatus

    static let shared = LegacyV1SecretPurge()

    private let notificationCenter: NotificationCenter
    private let protectedDataNotification: Notification.Name
    private let deleteItem: DeleteItem
    private var isObservingProtectedData = false

    init(
        notificationCenter: NotificationCenter = .default,
        protectedDataNotification: Notification.Name = UIApplication.protectedDataDidBecomeAvailableNotification,
        deleteItem: @escaping DeleteItem = { query in
            SecItemDelete(query as CFDictionary)
        }
    ) {
        self.notificationCenter = notificationCenter
        self.protectedDataNotification = protectedDataNotification
        self.deleteItem = deleteItem
    }

    func start() {
        observeProtectedDataIfNeeded()
        if !purgeNow() {
            stopObservingProtectedData()
        }
    }

    /// Returns `true` only when a protected-data failure requires a retry.
    @discardableResult
    func purgeNow() -> Bool {
        let statuses = Self.queries.map(deleteItem)
        for status in statuses where !Self.acceptedStatuses.contains(status) {
            guard !Self.protectedDataUnavailableStatuses.contains(status) else { continue }
            LLog.error(
                "legacy-v1-purge",
                "keychain deletion failed",
                fields: ["status": String(status)]
            )
        }
        return statuses.contains { Self.protectedDataUnavailableStatuses.contains($0) }
    }

    @objc private func protectedDataDidBecomeAvailable() {
        if !purgeNow() {
            stopObservingProtectedData()
        }
    }

    private func observeProtectedDataIfNeeded() {
        guard !isObservingProtectedData else { return }
        notificationCenter.addObserver(
            self,
            selector: #selector(protectedDataDidBecomeAvailable),
            name: protectedDataNotification,
            object: nil
        )
        isObservingProtectedData = true
    }

    private func stopObservingProtectedData() {
        guard isObservingProtectedData else { return }
        notificationCenter.removeObserver(self, name: protectedDataNotification, object: nil)
        isObservingProtectedData = false
    }

    private static let acceptedStatuses: Set<OSStatus> = [errSecSuccess, errSecItemNotFound]
    private static let protectedDataUnavailableStatuses: Set<OSStatus> = [
        errSecInteractionNotAllowed,
        errSecNotAvailable,
    ]

    private static let queries: [[String: Any]] = [
        [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: "com.alleycat.token",
        ],
        [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: "com.alleycat.device_key",
            kSecAttrAccount as String: "__device_secret_key__",
        ],
    ]
}
