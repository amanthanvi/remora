import CryptoKit
import Foundation
import Observation
import Security
import UserNotifications

@MainActor
protocol NotificationPermissionClient: AnyObject {
    func settings() async -> NotificationPermissionState
    func requestVisibleAuthorization() async throws -> Bool
}

@MainActor
final class SystemNotificationPermissionClient: NotificationPermissionClient {
    private let center: UNUserNotificationCenter

    init(center: UNUserNotificationCenter? = nil) {
        self.center = center ?? .current()
    }

    func settings() async -> NotificationPermissionState {
        let settings = await center.notificationSettings()
        return NotificationPermissionState(
            authorization: settings.authorizationStatus.remoraAuthorization,
            alertsEnabled: settings.alertSetting == .enabled,
            soundsEnabled: settings.soundSetting == .enabled,
            badgesEnabled: settings.badgeSetting == .enabled
        )
    }

    func requestVisibleAuthorization() async throws -> Bool {
        // Deliberately no custom categories/actions. Visible permission is
        // requested only from an explicit in-context product surface.
        try await center.requestAuthorization(options: [.alert, .badge, .sound])
    }
}

private extension UNAuthorizationStatus {
    var remoraAuthorization: VisibleNotificationAuthorization {
        switch self {
        case .notDetermined:
            return .notDetermined
        case .denied:
            return .denied
        case .authorized:
            return .authorized
        case .provisional:
            return .provisional
        case .ephemeral:
            return .ephemeral
        @unknown default:
            return .unknown
        }
    }
}

struct APNsEnvironmentTokenLifecycleState: Codable, Equatable {
    var relayInstallationID: String?
    var tokenDigest: Data?
    var generation: UInt64
    var pendingTombstoneThroughGeneration: UInt64?
}

struct APNsTokenLifecycleState: Codable, Equatable {
    let clientInstanceID: String
    private(set) var environments: [String: APNsEnvironmentTokenLifecycleState]

    init(
        clientInstanceID: String,
        environments: [String: APNsEnvironmentTokenLifecycleState] = [:]
    ) {
        self.clientInstanceID = clientInstanceID
        self.environments = environments
    }

    func registration(
        for environment: APNsEnvironment
    ) -> APNsEnvironmentTokenLifecycleState {
        environments[environment.rawValue] ?? APNsEnvironmentTokenLifecycleState(
            relayInstallationID: nil,
            tokenDigest: nil,
            generation: 0,
            pendingTombstoneThroughGeneration: nil
        )
    }

    mutating func setRegistration(
        _ registration: APNsEnvironmentTokenLifecycleState,
        for environment: APNsEnvironment
    ) {
        environments[environment.rawValue] = registration
    }
}

@MainActor
protocol APNsTokenLifecycleStateStore: AnyObject {
    func load() throws -> APNsTokenLifecycleState?
    func save(_ state: APNsTokenLifecycleState) throws
}

enum APNsTokenLifecycleStoreError: Error {
    case invalidRecord
    case keychain(OSStatus)
}

@MainActor
final class KeychainAPNsTokenLifecycleStateStore: APNsTokenLifecycleStateStore {
    private let service = "com.remora.background-awareness"
    // V2 is an intentional hard cutover from the unpublished, unscoped
    // prototype record. Reusing that record could apply a sandbox generation
    // or revocation to production (or vice versa).
    private let account = "apns-token-lifecycle-v2"

    func load() throws -> APNsTokenLifecycleState? {
        var result: CFTypeRef?
        let status = SecItemCopyMatching(
            baseQuery().merging([
                kSecReturnData as String: true,
                kSecMatchLimit as String: kSecMatchLimitOne
            ]) { _, new in new } as CFDictionary,
            &result
        )
        switch status {
        case errSecSuccess:
            guard let data = result as? Data,
                  let state = try? JSONDecoder().decode(APNsTokenLifecycleState.self, from: data) else {
                throw APNsTokenLifecycleStoreError.invalidRecord
            }
            return state
        case errSecItemNotFound:
            return nil
        default:
            throw APNsTokenLifecycleStoreError.keychain(status)
        }
    }

    func save(_ state: APNsTokenLifecycleState) throws {
        let data = try JSONEncoder().encode(state)
        let query = baseQuery()
        let attributes = query.merging([
            kSecAttrAccessible as String: kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly,
            kSecValueData as String: data
        ]) { _, new in new }

        let status = SecItemAdd(attributes as CFDictionary, nil)
        if status == errSecDuplicateItem {
            let updateStatus = SecItemUpdate(
                query as CFDictionary,
                [
                    kSecAttrAccessible as String: kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly,
                    kSecValueData as String: data
                ] as CFDictionary
            )
            guard updateStatus == errSecSuccess else {
                throw APNsTokenLifecycleStoreError.keychain(updateStatus)
            }
            return
        }
        guard status == errSecSuccess else {
            throw APNsTokenLifecycleStoreError.keychain(status)
        }
    }

    private func baseQuery() -> [String: Any] {
        [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: account
        ]
    }
}

@MainActor
final class APNsTokenLifecycle {
    private let stateStore: any APNsTokenLifecycleStateStore
    private let environment: APNsEnvironment
    private var registry: (any PushTokenRegistry)?
    private var pendingRegistration: APNsTokenRegistration?

    init(
        stateStore: (any APNsTokenLifecycleStateStore)? = nil,
        registry: (any PushTokenRegistry)? = nil,
        environment: APNsEnvironment = .current
    ) {
        self.stateStore = stateStore ?? KeychainAPNsTokenLifecycleStateStore()
        self.registry = registry
        self.environment = environment
    }

    func installationID() throws -> String? {
        try state().registration(for: environment).relayInstallationID
    }

    func bind(registry: (any PushTokenRegistry)?) async -> PushTokenSyncState {
        self.registry = registry
        do {
            let pendingTombstones = try durablePendingTombstones()
            guard let registry else {
                if let latestGeneration = pendingTombstones.map(\.throughGeneration).max() {
                    return .pending(generation: latestGeneration)
                }
                return await flushPendingRegistration()
            }

            var latestTombstonedGeneration: UInt64?
            for tombstone in pendingTombstones {
                try await registry.tombstone(tombstone)

                var latestState = try self.state()
                var latestRegistration = latestState.registration(for: tombstone.environment)
                if latestRegistration.pendingTombstoneThroughGeneration
                    == tombstone.throughGeneration {
                    latestRegistration.pendingTombstoneThroughGeneration = nil
                    latestState.setRegistration(
                        latestRegistration,
                        for: tombstone.environment
                    )
                    try stateStore.save(latestState)
                }
                latestTombstonedGeneration = max(
                    latestTombstonedGeneration ?? 0,
                    tombstone.throughGeneration
                )
            }

            if pendingRegistration != nil {
                return await flushPendingRegistration()
            }
            if let latestTombstonedGeneration {
                return .tombstoned(generation: latestTombstonedGeneration)
            }
        } catch {
            return .failed(generation: 0)
        }

        return await flushPendingRegistration()
    }

    func retryPendingOperations() async -> PushTokenSyncState {
        do {
            let state = try state()
            let registration = state.registration(for: environment)
            if state.environments.values.contains(where: {
                $0.pendingTombstoneThroughGeneration != nil
            })
                || pendingRegistration != nil {
                return await bind(registry: registry)
            }
            if registration.tokenDigest != nil {
                return registration.relayInstallationID == nil
                    ? .pending(generation: registration.generation)
                    : .synced(generation: registration.generation)
            }
            return registration.generation > 0
                ? .tombstoned(generation: registration.generation)
                : .unavailable
        } catch {
            return .failed(generation: 0)
        }
    }

    func register(token: Data, now: Date = Date()) async throws -> (APNsTokenRegistration, PushTokenSyncState) {
        var state = try state()
        var environmentState = state.registration(for: environment)
        let digest = Data(SHA256.hash(data: token))
        let replacesGeneration: UInt64?
        if environmentState.tokenDigest == digest, environmentState.generation > 0 {
            replacesGeneration = nil
        } else {
            replacesGeneration = environmentState.generation > 0
                ? environmentState.generation
                : nil
            environmentState.generation = try environmentState.generation.addingOne()
            environmentState.tokenDigest = digest
            state.setRegistration(environmentState, for: environment)
            try stateStore.save(state)
        }

        let registration = APNsTokenRegistration(
            clientInstanceID: state.clientInstanceID,
            installationID: environmentState.relayInstallationID,
            token: token,
            generation: environmentState.generation,
            replacesGeneration: replacesGeneration,
            provider: .apns,
            environment: environment,
            observedAt: now
        )
        pendingRegistration = registration
        return (registration, await flushPendingRegistration())
    }

    func tombstone(now: Date = Date()) async throws -> PushTokenSyncState {
        var state = try state()
        var environmentState = state.registration(for: environment)
        guard environmentState.generation > 0 else {
            return .tombstoned(generation: 0)
        }
        let tombstone = APNsTokenTombstone(
            clientInstanceID: state.clientInstanceID,
            installationID: environmentState.relayInstallationID,
            throughGeneration: environmentState.generation,
            provider: .apns,
            environment: environment,
            observedAt: now
        )
        // Persist the revocation intent before touching the network. A crash or
        // force-quit must never turn a failed logout/revocation into a usable
        // token on the next launch.
        environmentState.tokenDigest = nil
        environmentState.pendingTombstoneThroughGeneration = environmentState.generation
        state.setRegistration(environmentState, for: environment)
        try stateStore.save(state)
        pendingRegistration = nil
        guard let registry else {
            return .pending(generation: environmentState.generation)
        }

        do {
            try await registry.tombstone(tombstone)
            var latestState = try self.state()
            var latestRegistration = latestState.registration(for: environment)
            if latestRegistration.pendingTombstoneThroughGeneration
                == tombstone.throughGeneration {
                latestRegistration.pendingTombstoneThroughGeneration = nil
                latestState.setRegistration(latestRegistration, for: environment)
                try stateStore.save(latestState)
            }
            return .tombstoned(generation: environmentState.generation)
        } catch {
            // Keep the persisted, environment-scoped tombstone pending for a
            // later bind/foreground retry.
            throw error
        }
    }

    private func state() throws -> APNsTokenLifecycleState {
        if let existing = try stateStore.load() {
            return existing
        }
        let state = APNsTokenLifecycleState(
            clientInstanceID: UUID().uuidString.lowercased()
        )
        try stateStore.save(state)
        return state
    }

    private func durablePendingTombstones() throws -> [APNsTokenTombstone] {
        let state = try state()
        return state.environments.keys.sorted().compactMap { rawEnvironment in
            guard let environment = APNsEnvironment(rawValue: rawEnvironment),
                  let registration = state.environments[rawEnvironment],
                  let generation = registration.pendingTombstoneThroughGeneration else {
                return nil
            }
            return APNsTokenTombstone(
                clientInstanceID: state.clientInstanceID,
                installationID: registration.relayInstallationID,
                throughGeneration: generation,
                provider: .apns,
                environment: environment,
                observedAt: Date()
            )
        }
    }

    private func flushPendingRegistration() async -> PushTokenSyncState {
        guard let registration = pendingRegistration else {
            return .unavailable
        }
        guard let registry else {
            return .pending(generation: registration.generation)
        }
        do {
            let receipt = try await registry.upsert(registration)
            guard receipt.schemaVersion == PushTokenRegistrationReceipt.currentSchemaVersion,
                  receipt.provider == registration.provider,
                  receipt.environment == registration.environment,
                  Self.isValidOpaqueIdentifier(receipt.installationID),
                  Self.isValidOpaqueIdentifier(receipt.registrationID),
                  registration.installationID == nil
                    || registration.installationID == receipt.installationID else {
                return .failed(generation: registration.generation)
            }

            var latestState = try state()
            var latestRegistration = latestState.registration(for: registration.environment)
            guard latestState.clientInstanceID == registration.clientInstanceID,
                  latestRegistration.relayInstallationID == nil
                    || latestRegistration.relayInstallationID == receipt.installationID else {
                return .failed(generation: registration.generation)
            }
            latestRegistration.relayInstallationID = receipt.installationID
            let registrationDigest = Data(SHA256.hash(data: registration.token))
            if latestRegistration.tokenDigest == registrationDigest {
                // Relay generation is authoritative within this exact
                // provider/environment scope.
                latestRegistration.generation = receipt.generation
            } else {
                // A newer local token rotated while this request was in
                // flight. Preserve its pending generation, but never regress
                // below the generation the relay just acknowledged.
                latestRegistration.generation = max(
                    latestRegistration.generation,
                    receipt.generation
                )
            }
            latestState.setRegistration(
                latestRegistration,
                for: registration.environment
            )
            try stateStore.save(latestState)

            if self.pendingRegistration == registration {
                self.pendingRegistration = nil
            } else if let pendingRegistration = self.pendingRegistration,
                      pendingRegistration.installationID == nil,
                      pendingRegistration.environment == receipt.environment,
                      pendingRegistration.provider == receipt.provider {
                self.pendingRegistration = APNsTokenRegistration(
                    clientInstanceID: pendingRegistration.clientInstanceID,
                    installationID: receipt.installationID,
                    token: pendingRegistration.token,
                    generation: pendingRegistration.generation,
                    replacesGeneration: pendingRegistration.replacesGeneration,
                    provider: pendingRegistration.provider,
                    environment: pendingRegistration.environment,
                    observedAt: pendingRegistration.observedAt
                )
            }
            return .synced(generation: receipt.generation)
        } catch {
            return .failed(generation: registration.generation)
        }
    }

    private static func isValidOpaqueIdentifier(_ value: String) -> Bool {
        (16...128).contains(value.utf8.count)
            && value.unicodeScalars.allSatisfy { scalar in
                switch scalar.value {
                case 45, 48...57, 65...90, 95, 97...122:
                    return true
                default:
                    return false
                }
            }
    }
}

private extension UInt64 {
    func addingOne() throws -> UInt64 {
        let (value, overflow) = addingReportingOverflow(1)
        if overflow {
            throw APNsTokenLifecycleStoreError.invalidRecord
        }
        return value
    }
}

@MainActor
protocol WakeCursorStore: AnyObject {
    var lastReconciledCursor: UInt64 { get set }
}

@MainActor
final class UserDefaultsWakeCursorStore: WakeCursorStore {
    private let defaults: UserDefaults
    private let key = "remora.background-awareness.last-reconciled-cursor"

    init(defaults: UserDefaults = .standard) {
        self.defaults = defaults
    }

    var lastReconciledCursor: UInt64 {
        get {
            guard let rawValue = defaults.string(forKey: key) else { return 0 }
            return UInt64(rawValue) ?? 0
        }
        set {
            defaults.set(String(newValue), forKey: key)
        }
    }
}

@MainActor
final class OpaqueWakeReconciliationCoordinator {
    private weak var reconciler: (any BackgroundStateReconciling)?
    private let cursorStore: any WakeCursorStore
    private let timeout: Duration
    private var pendingCursor: UInt64?
    private var activeTargetCursor: UInt64?
    private var drainTask: Task<BackgroundReconciliationResult, Never>?
    private let clock = ContinuousClock()

    init(
        reconciler: (any BackgroundStateReconciling)? = nil,
        cursorStore: (any WakeCursorStore)? = nil,
        timeout: Duration = .seconds(27)
    ) {
        self.reconciler = reconciler
        self.cursorStore = cursorStore ?? UserDefaultsWakeCursorStore()
        self.timeout = timeout
    }

    func bind(reconciler: (any BackgroundStateReconciling)?) {
        self.reconciler = reconciler
    }

    func reconcile(_ payload: OpaqueWakePayload) async -> BackgroundReconciliationResult {
        guard payload.cursor > cursorStore.lastReconciledCursor else {
            return .noData
        }

        if let activeTargetCursor, payload.cursor <= activeTargetCursor {
            return await drainTask?.value ?? .noData
        }
        pendingCursor = max(pendingCursor ?? 0, payload.cursor)
        return await startOrJoinDrain(deadline: nil)
    }

    /// Retries only a wake invalidation that failed or timed out. The ordinary
    /// foreground lifecycle independently performs a full authoritative repair
    /// even when APNs dropped every hint.
    func retryPendingOnForeground() {
        guard pendingCursor != nil, drainTask == nil else { return }
        Task { @MainActor [weak self] in
            _ = await self?.startOrJoinDrain(deadline: nil)
        }
    }

    private func startOrJoinDrain(
        deadline existingDeadline: ContinuousClock.Instant?
    ) async -> BackgroundReconciliationResult {
        if let drainTask {
            return await drainTask.value
        }
        let deadline = existingDeadline ?? clock.now.advanced(by: timeout)
        let task = Task { @MainActor [weak self] in
            await self?.drain(deadline: deadline) ?? .unavailable
        }
        drainTask = task
        let result = await task.value
        drainTask = nil
        activeTargetCursor = nil
        guard pendingCursor != nil else { return result }
        switch result {
        case .newData, .noData:
            // A higher cursor can arrive after `drain()` completes but before
            // this owner clears the completed task. Drain it immediately so
            // that completion-window race cannot strand pending work. Reuse
            // the original absolute deadline: one APNs callback never receives
            // a fresh 27-second budget merely because another hint arrived.
            let trailingResult = await startOrJoinDrain(deadline: deadline)
            if result == .newData, trailingResult == .noData {
                return .newData
            }
            return trailingResult
        case .timedOut, .unavailable, .failed:
            return result
        }
    }

    private func drain(
        deadline: ContinuousClock.Instant
    ) async -> BackgroundReconciliationResult {
        var observedNewData = false
        while let targetCursor = pendingCursor {
            pendingCursor = nil
            activeTargetCursor = targetCursor
            let remaining = clock.now.duration(to: deadline)
            guard remaining > .zero else {
                pendingCursor = max(pendingCursor ?? 0, targetCursor)
                return .timedOut
            }
            let result = await reconcileBounded(
                expectedCursor: targetCursor,
                timeout: remaining
            )
            switch result {
            case .newData:
                observedNewData = true
                cursorStore.lastReconciledCursor = max(
                    cursorStore.lastReconciledCursor,
                    targetCursor
                )
            case .noData:
                cursorStore.lastReconciledCursor = max(
                    cursorStore.lastReconciledCursor,
                    targetCursor
                )
            case .timedOut, .unavailable, .failed:
                pendingCursor = max(pendingCursor ?? 0, targetCursor)
                return result
            }
        }
        return observedNewData ? .newData : .noData
    }

    private func reconcileBounded(
        expectedCursor: UInt64,
        timeout: Duration
    ) async -> BackgroundReconciliationResult {
        guard let reconciler else { return .unavailable }
        let race = BoundedReconciliationRace()
        return await race.run(timeout: timeout) {
            switch await reconciler.reconcileBackgroundState(expectedCursor: expectedCursor) {
            case .changed:
                return .newData
            case .unchanged:
                return .noData
            case .failed:
                return .failed
            }
        }
    }
}

/// An unstructured race is intentional here. Swift task groups wait for every
/// child before returning, even after cancellation; UniFFI work may not observe
/// cancellation promptly. APNs requires the completion callback within its
/// short execution window, so this gate returns at the deadline while the
/// canceled authenticated repair winds down independently.
@MainActor
private final class BoundedReconciliationRace {
    private var continuation: CheckedContinuation<BackgroundReconciliationResult, Never>?
    private var operationTask: Task<Void, Never>?
    private var timeoutTask: Task<Void, Never>?
    private var resolved = false

    func run(
        timeout: Duration,
        operation: @escaping @MainActor () async -> BackgroundReconciliationResult
    ) async -> BackgroundReconciliationResult {
        await withCheckedContinuation { continuation in
            self.continuation = continuation
            operationTask = Task { @MainActor [weak self] in
                guard let self else { return }
                self.resolve(await operation())
            }
            timeoutTask = Task { [weak self] in
                try? await Task.sleep(for: timeout)
                await MainActor.run {
                    self?.resolve(.timedOut)
                }
            }
        }
    }

    private func resolve(_ result: BackgroundReconciliationResult) {
        guard !resolved else { return }
        resolved = true
        if result == .timedOut {
            operationTask?.cancel()
        } else {
            timeoutTask?.cancel()
        }
        continuation?.resume(returning: result)
        continuation = nil
    }
}

@MainActor
@Observable
final class BackgroundAwarenessController {
    static let shared = BackgroundAwarenessController()

    private let permissionClient: any NotificationPermissionClient
    private let tokenLifecycle: APNsTokenLifecycle
    private let reconciliation: OpaqueWakeReconciliationCoordinator

    private(set) var permissionState = NotificationPermissionState.unknown
    private(set) var registrationState = RemoteNotificationRegistrationState.idle
    private(set) var tokenSyncState = PushTokenSyncState.unavailable

    init(
        permissionClient: (any NotificationPermissionClient)? = nil,
        tokenLifecycle: APNsTokenLifecycle? = nil,
        reconciliation: OpaqueWakeReconciliationCoordinator? = nil
    ) {
        self.permissionClient = permissionClient ?? SystemNotificationPermissionClient()
        self.tokenLifecycle = tokenLifecycle ?? APNsTokenLifecycle()
        self.reconciliation = reconciliation ?? OpaqueWakeReconciliationCoordinator()
    }

    func start(registerForRemoteNotifications: () -> Void) {
        registrationState = .registering
        registerForRemoteNotifications()
        Task { @MainActor [weak self] in
            await self?.refreshPermissionState()
        }
    }

    func bind(
        reconciler: (any BackgroundStateReconciling)?,
        tokenRegistry: (any PushTokenRegistry)? = nil
    ) {
        reconciliation.bind(reconciler: reconciler)
        Task { @MainActor [weak self] in
            guard let self else { return }
            self.tokenSyncState = await self.tokenLifecycle.bind(registry: tokenRegistry)
            self.reconciliation.retryPendingOnForeground()
        }
    }

    func refreshPermissionState() async {
        permissionState = await permissionClient.settings()
    }

    /// Call only from a UI surface that has explained the value of visible
    /// notifications. APNs background registration itself never invokes this.
    @discardableResult
    func requestVisiblePermissionInContext() async -> Bool {
        do {
            let granted = try await permissionClient.requestVisibleAuthorization()
            await refreshPermissionState()
            return granted
        } catch {
            await refreshPermissionState()
            return false
        }
    }

    func didRegisterForRemoteNotifications(deviceToken: Data) {
        registrationState = .registered
        Task { @MainActor [weak self] in
            guard let self else { return }
            do {
                let (_, syncState) = try await self.tokenLifecycle.register(token: deviceToken)
                self.tokenSyncState = syncState
            } catch {
                self.tokenSyncState = .failed(generation: 0)
            }
        }
    }

    func didFailToRegisterForRemoteNotifications() {
        registrationState = .failed
    }

    func tombstoneCurrentToken() async {
        do {
            tokenSyncState = try await tokenLifecycle.tombstone()
        } catch {
            tokenSyncState = .failed(generation: 0)
        }
    }

    func handleRemoteNotification(
        userInfo: [AnyHashable: Any],
        now: Date = Date()
    ) async -> BackgroundReconciliationResult {
        do {
            let payload = try OpaqueWakePayload(userInfo: userInfo, now: now)
            guard let installationID = try tokenLifecycle.installationID(),
                  payload.installationID == installationID else {
                return .noData
            }
            return await reconciliation.reconcile(payload)
        } catch {
            return .noData
        }
    }

    func applicationDidBecomeActive() {
        Task { @MainActor [weak self] in
            guard let self else { return }
            await self.refreshPermissionState()
            self.tokenSyncState = await self.tokenLifecycle.retryPendingOperations()
        }
        reconciliation.retryPendingOnForeground()
    }
}
