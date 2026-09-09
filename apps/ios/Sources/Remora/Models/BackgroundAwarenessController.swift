import Foundation
import Observation
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
        try await center.requestAuthorization(options: [.alert, .badge, .sound])
    }
}

private extension UNAuthorizationStatus {
    var remoraAuthorization: VisibleNotificationAuthorization {
        switch self {
        case .notDetermined: return .notDetermined
        case .denied: return .denied
        case .authorized: return .authorized
        case .provisional: return .provisional
        case .ephemeral: return .ephemeral
        @unknown default: return .unknown
        }
    }
}

/// iOS owns OS ingress and its execution budget, not registration or cursor state.
@MainActor
@Observable
final class BackgroundAwarenessController {
    static let shared = BackgroundAwarenessController()

    private let permissionClient: any NotificationPermissionClient
    private let securityReady: @MainActor () -> Bool
    private let activateRuntime: @MainActor () async -> Void
    private let tokenCustody: NativeRelayProviderTokenCustody
    private let timeout: Duration
    private let sleep: @MainActor (Duration) async throws -> Void
    private var runtime: (any NativeBackgroundRelayRuntime)?
    private var configurationTask: (id: UUID, task: Task<Bool, Never>)?
    private var configurationID = UUID()
    private var configured = false
    private var tokenTask: Task<Void, Never>?
    private var tokenTaskID: UUID?
    private var registrationRequest: (() -> Void)?

    private(set) var permissionState = NotificationPermissionState.unknown
    private(set) var registrationState = RemoteNotificationRegistrationState.idle
    private(set) var relayStatus: AppRelayStatusSnapshot?
    private(set) var lastTokenSync: AppRelayFanoutReceipt?
    private(set) var lastOperationFailed = false

    init(
        permissionClient: (any NotificationPermissionClient)? = nil,
        runtime: (any NativeBackgroundRelayRuntime)? = nil,
        tokenCustody: NativeRelayProviderTokenCustody? = nil,
        securityReady: @escaping @MainActor () -> Bool = { CurrentKeychainNamespaceCleanup.shared.isComplete },
        activateRuntime: @escaping @MainActor () async -> Void = {
            await AppRuntimeController.shared.prepareBackgroundRuntimeIfSecurityReady()
        },
        timeout: Duration = .seconds(27),
        sleep: @escaping @MainActor (Duration) async throws -> Void = { try await Task.sleep(for: $0) }
    ) {
        self.permissionClient = permissionClient ?? SystemNotificationPermissionClient()
        self.runtime = runtime
        self.tokenCustody = tokenCustody ?? NativeRelayProviderTokenCustody()
        self.securityReady = securityReady
        self.activateRuntime = activateRuntime
        self.timeout = timeout
        self.sleep = sleep
    }

    func start(registerForRemoteNotifications: @escaping () -> Void) {
        registrationRequest = registerForRemoteNotifications
        requestCurrentProviderRegistration()
        Task { [weak self] in await self?.refreshPermissionState() }
    }

    func bind(runtime: any NativeBackgroundRelayRuntime) {
        guard self.runtime !== runtime else { return }
        configurationTask?.task.cancel()
        configurationTask = nil
        configurationID = UUID()
        self.runtime = runtime
        configured = false
        relayStatus = nil
        Task { [weak self] in _ = await self?.reconcile() }
    }

    func refreshPermissionState() async {
        permissionState = await permissionClient.settings()
    }

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
        enqueueTokenOperation { [weak self] in
            guard let self, self.securityReady() else { return }
            do {
                try await self.tokenCustody.observe(token: deviceToken)
                guard let runtime = await self.preparedRuntime() else { return }
                try await self.synchronizeToken(runtime: runtime)
            } catch { self.lastOperationFailed = true }
        }
    }

    func didFailToRegisterForRemoteNotifications() {
        registrationState = .failed
    }

    func tombstoneCurrentToken() async {
        let task = enqueueTokenOperation { [weak self] in
            guard let self, self.securityReady() else { return }
            do {
                try await self.tokenCustody.tombstone()
                guard let runtime = await self.preparedRuntime() else { return }
                try await self.synchronizeToken(runtime: runtime)
            } catch { self.lastOperationFailed = true }
        }
        await task.value
    }

    func handleRemoteNotification(
        userInfo: [AnyHashable: Any], now: Date = Date()
    ) async -> BackgroundReconciliationResult {
        guard let payload = try? OpaqueWakePayload(userInfo: userInfo, now: now) else { return .noData }
        return await BoundedRelayCallback().run(timeout: timeout, sleep: sleep) { [weak self] in
            guard let self, let runtime = await self.preparedRuntime(), !Task.isCancelled else {
                return .unavailable
            }
            do {
                // Rust validates installation membership, deduplicates, repairs and ACKs.
                let receipt = try await runtime.ingest(hint: payload.relayHint)
                await self.refreshStatus(runtime: runtime)
                return receipt.changed ? .newData : .noData
            } catch { return Self.result(for: error) }
        }
    }

    func applicationDidBecomeActive() {
        requestCurrentProviderRegistration()
        Task { [weak self] in
            guard let self else { return }
            await self.refreshPermissionState()
            _ = await self.reconcile()
        }
    }

    @discardableResult
    func reconcile() async -> BackgroundReconciliationResult {
        await BoundedRelayCallback().run(timeout: timeout, sleep: sleep) { [weak self] in
            guard let self, let runtime = await self.preparedRuntime(), !Task.isCancelled else {
                return .unavailable
            }
            do {
                let outcomes = try await runtime.reconcile()
                // Provisioning may have added hosts since the last OS token callback.
                let task = self.enqueueTokenOperation { [weak self] in
                    guard let self, self.runtime === runtime else { return }
                    do { try await self.synchronizeToken(runtime: runtime) }
                    catch { self.lastOperationFailed = true }
                }
                await task.value
                await self.refreshStatus(runtime: runtime)
                if outcomes.contains(where: {
                    if case .applied(let receipt) = $0 { return receipt.changed }
                    return false
                }) { return .newData }
                if outcomes.contains(where: {
                    if case .failed = $0 { return true }
                    return false
                }) { return .failed }
                return .noData
            } catch { return Self.result(for: error) }
        }
    }

    private func preparedRuntime() async -> (any NativeBackgroundRelayRuntime)? {
        guard securityReady() else {
            lastOperationFailed = true
            return nil
        }
        if runtime == nil { await activateRuntime() }
        guard let runtime, !Task.isCancelled else { return nil }
        if configured { return runtime }
        let id = configurationID
        let task: Task<Bool, Never>
        let attemptID: UUID
        if let configurationTask {
            task = configurationTask.task
            attemptID = configurationTask.id
        } else {
            attemptID = UUID()
            task = Task {
                do { try await runtime.configure(); return !Task.isCancelled }
                catch { return false }
            }
            configurationTask = (attemptID, task)
        }
        let succeeded = await task.value
        guard configurationID == id else { return nil }
        if configurationTask?.id == attemptID {
            configurationTask = nil
            configured = succeeded
            lastOperationFailed = !succeeded
        }
        return succeeded ? runtime : nil
    }

    private func requestCurrentProviderRegistration() {
        guard securityReady(), let registrationRequest else { return }
        registrationState = .registering
        registrationRequest()
    }

    @discardableResult
    private func enqueueTokenOperation(
        _ operation: @escaping @MainActor () async -> Void
    ) -> Task<Void, Never> {
        let previous = tokenTask
        let id = UUID()
        tokenTaskID = id
        let task = Task { [weak self] in
            await previous?.value
            await operation()
            if self?.tokenTaskID == id {
                self?.tokenTask = nil
                self?.tokenTaskID = nil
            }
        }
        tokenTask = task
        return task
    }

    private func synchronizeToken(runtime: any NativeBackgroundRelayRuntime) async throws {
        let receipt = try await tokenCustody.synchronize(runtime: runtime)
        guard self.runtime === runtime else { return }
        lastTokenSync = receipt
        lastOperationFailed = false
    }

    private func refreshStatus(runtime: any NativeBackgroundRelayRuntime) async {
        guard !Task.isCancelled else { return }
        let status = try? await runtime.status()
        guard self.runtime === runtime, !Task.isCancelled else { return }
        relayStatus = status
    }

    private static func result(for error: Error) -> BackgroundReconciliationResult {
        guard let error = error as? BackgroundRelayError else { return .failed }
        switch error {
        case .UnknownInstallation, .InvalidWake, .Tombstoned: return .noData
        case .NotConfigured, .JournalUnavailable, .SecureStorageUnavailable: return .unavailable
        case .DeadlineExceeded, .Cancelled: return .timedOut
        default: return .failed
        }
    }
}

/// A task group would wait for an uncooperative FFI child after cancellation.
/// Return the OS callback on time; Rust fences any late authoritative work.
@MainActor
private final class BoundedRelayCallback {
    private var continuation: CheckedContinuation<BackgroundReconciliationResult, Never>?
    private var operationTask: Task<Void, Never>?
    private var timeoutTask: Task<Void, Never>?
    private var resolvedResult: BackgroundReconciliationResult?

    func run(
        timeout: Duration,
        sleep: @escaping @MainActor (Duration) async throws -> Void,
        operation: @escaping @MainActor () async -> BackgroundReconciliationResult
    ) async -> BackgroundReconciliationResult {
        await withTaskCancellationHandler {
            await withCheckedContinuation { continuation in
                if let resolvedResult {
                    continuation.resume(returning: resolvedResult)
                    return
                }
                self.continuation = continuation
                if Task.isCancelled { resolve(.timedOut); return }
                operationTask = Task { [weak self] in self?.resolve(await operation()) }
                timeoutTask = Task { [weak self] in
                    do { try await sleep(timeout) }
                    catch { return }
                    self?.resolve(.timedOut)
                }
            }
        } onCancel: {
            Task { @MainActor [weak self] in self?.resolve(.timedOut) }
        }
    }

    private func resolve(_ result: BackgroundReconciliationResult) {
        guard resolvedResult == nil else { return }
        resolvedResult = result
        operationTask?.cancel()
        timeoutTask?.cancel()
        continuation?.resume(returning: result)
        continuation = nil
    }
}
