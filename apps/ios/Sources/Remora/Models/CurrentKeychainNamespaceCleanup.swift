import Foundation
import Security
import UIKit

/// Enforces the Remora 1.6 security boundary before the shared runtime starts.
///
/// A missing marker means the installation may contain authority created by an
/// unsupported build (including Keychain data that survived uninstall). The
/// cutover therefore removes retired-namespace generic passwords, the current
/// Remora Link transport identity, and every signing key visible to this app.
/// Current Remora SSH and OAuth credentials remain intact. Saved hosts and the
/// Remora Link journal are cleared, and the marker is written only after every
/// step succeeds.
@MainActor
final class CurrentKeychainNamespaceCleanup: NSObject {
    typealias CopyItems = ([String: Any], UnsafeMutablePointer<CFTypeRef?>?) -> OSStatus
    typealias DeleteItems = ([String: Any]) -> OSStatus
    typealias ResetPersistentState = () -> Bool

    static let shared = CurrentKeychainNamespaceCleanup()
    static let completionMarkerKey = "remora.securityCutover.1_6.completed"

    private enum Attempt: Equatable {
        case complete
        case retryAfterProtectedData
        case failed
    }

    private let notificationCenter: NotificationCenter
    private let protectedDataNotification: Notification.Name
    private let defaults: UserDefaults
    private let copyItems: CopyItems
    private let deleteItems: DeleteItems
    private let resetPersistentState: ResetPersistentState
    private var isObservingProtectedData = false
    private var completion: (() -> Void)?

    init(
        notificationCenter: NotificationCenter = .default,
        protectedDataNotification: Notification.Name = UIApplication.protectedDataDidBecomeAvailableNotification,
        defaults: UserDefaults = .standard,
        copyItems: @escaping CopyItems = { query, result in
            SecItemCopyMatching(query as CFDictionary, result)
        },
        deleteItems: @escaping DeleteItems = { query in
            SecItemDelete(query as CFDictionary)
        },
        resetPersistentState: ResetPersistentState? = nil
    ) {
        self.notificationCenter = notificationCenter
        self.protectedDataNotification = protectedDataNotification
        self.defaults = defaults
        self.copyItems = copyItems
        self.deleteItems = deleteItems
        self.resetPersistentState = resetPersistentState ?? {
            let removedSavedServers =
                SavedServerStore.removeAllForSecurityCutover(from: defaults)
            let removedJournal =
                RemoraLinkJournalStore.shared.discardForSecurityCutover()
            return removedSavedServers && removedJournal
        }
    }

    var isComplete: Bool {
        defaults.bool(forKey: Self.completionMarkerKey)
    }

    /// Runs the cutover synchronously when possible. The completion is invoked
    /// exactly once after the marker is durable; locked protected data defers
    /// it until iOS reports availability.
    func start(onComplete: @escaping () -> Void = {}) {
        guard !isComplete else {
            onComplete()
            return
        }
        completion = onComplete
        switch attemptCutover() {
        case .complete:
            finish()
        case .retryAfterProtectedData:
            observeProtectedDataIfNeeded()
        case .failed:
            LLog.error(
                "security-cutover",
                "Remora 1.6 security cutover failed; Remora Link remains disabled"
            )
        }
    }

    @discardableResult
    func cleanNow() -> Bool {
        switch attemptCutover() {
        case .complete:
            return false
        case .retryAfterProtectedData:
            return true
        case .failed:
            return false
        }
    }

    private func attemptCutover() -> Attempt {
        guard !isComplete else { return .complete }

        let genericPasswordCleanup = clearUnsupportedAndPairingPasswords()
        guard genericPasswordCleanup == .complete else {
            return genericPasswordCleanup
        }
        let signingKeyCleanup = clearRemoraLinkSigningKeys()
        guard signingKeyCleanup == .complete else {
            return signingKeyCleanup
        }

        guard resetPersistentState() else {
            LLog.error("security-cutover", "persistent Remora Link reset failed")
            return .failed
        }
        defaults.set(true, forKey: Self.completionMarkerKey)
        return defaults.bool(forKey: Self.completionMarkerKey) ? .complete : .failed
    }

    private func clearUnsupportedAndPairingPasswords() -> Attempt {
        var result: CFTypeRef?
        let status = copyItems(Self.genericPasswordEnumerationQuery, &result)
        if Self.protectedDataUnavailableStatuses.contains(status) {
            return .retryAfterProtectedData
        }
        if status == errSecItemNotFound {
            return .complete
        }
        guard status == errSecSuccess else {
            LLog.error(
                "security-cutover",
                "credential enumeration failed",
                fields: ["status": String(status)]
            )
            return .failed
        }

        let items = (result as? [[String: Any]]) ?? []
        for item in items {
            guard let service = item[kSecAttrService as String] as? String,
                  let account = item[kSecAttrAccount as String] as? String,
                  !service.hasPrefix(Self.currentServicePrefix)
                    || service == RemoraLinkTransportIdentityKey.applicationV2.service else {
                continue
            }
            let deleteStatus = deleteItems([
                kSecClass as String: kSecClassGenericPassword,
                kSecAttrService as String: service,
                kSecAttrAccount as String: account,
            ])
            if Self.protectedDataUnavailableStatuses.contains(deleteStatus) {
                return .retryAfterProtectedData
            }
            guard deleteStatus == errSecSuccess || deleteStatus == errSecItemNotFound else {
                LLog.error(
                    "security-cutover",
                    "credential reset failed",
                    fields: ["status": String(deleteStatus)]
                )
                return .failed
            }
        }
        return .complete
    }

    private func clearRemoraLinkSigningKeys() -> Attempt {
        var result: CFTypeRef?
        let status = copyItems(Self.signingKeyEnumerationQuery, &result)
        if Self.protectedDataUnavailableStatuses.contains(status) {
            return .retryAfterProtectedData
        }
        if status == errSecItemNotFound {
            return .complete
        }
        guard status == errSecSuccess else {
            LLog.error(
                "security-cutover",
                "signing-key enumeration failed",
                fields: ["status": String(status)]
            )
            return .failed
        }

        let items = (result as? [[String: Any]]) ?? []
        for item in items {
            guard let applicationTag = item[kSecAttrApplicationTag as String] as? Data,
                  applicationTag.starts(
                    with: RemoraLinkKeyStore.securityCutoverApplicationTagPrefix
                  ) else {
                continue
            }
            let deleteStatus = deleteItems([
                kSecClass as String: kSecClassKey,
                kSecAttrApplicationTag as String: applicationTag,
                kSecAttrKeyType as String: kSecAttrKeyTypeECSECPrimeRandom,
                kSecAttrKeyClass as String: kSecAttrKeyClassPrivate,
            ])
            if Self.protectedDataUnavailableStatuses.contains(deleteStatus) {
                return .retryAfterProtectedData
            }
            guard deleteStatus == errSecSuccess || deleteStatus == errSecItemNotFound else {
                LLog.error(
                    "security-cutover",
                    "signing-key reset failed",
                    fields: ["status": String(deleteStatus)]
                )
                return .failed
            }
        }
        return .complete
    }

    @objc private func protectedDataDidBecomeAvailable() {
        switch attemptCutover() {
        case .complete:
            finish()
        case .retryAfterProtectedData:
            break
        case .failed:
            stopObservingProtectedData()
        }
    }

    private func finish() {
        stopObservingProtectedData()
        let callback = completion
        completion = nil
        callback?()
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

    private static let protectedDataUnavailableStatuses: Set<OSStatus> = [
        errSecInteractionNotAllowed,
        errSecNotAvailable,
    ]
    private static let currentServicePrefix = "com.remora."
    private static let genericPasswordEnumerationQuery: [String: Any] = [
        kSecClass as String: kSecClassGenericPassword,
        kSecReturnAttributes as String: true,
        kSecMatchLimit as String: kSecMatchLimitAll,
    ]
    private static let signingKeyEnumerationQuery: [String: Any] = [
        kSecClass as String: kSecClassKey,
        kSecReturnAttributes as String: true,
        kSecMatchLimit as String: kSecMatchLimitAll,
    ]
}
