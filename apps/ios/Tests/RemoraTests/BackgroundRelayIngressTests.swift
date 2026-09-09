import Foundation
import Security
import XCTest
@testable import Remora

@MainActor
final class BackgroundRelayIngressTests: XCTestCase {
    func testSecurityPreflightBlocksRuntimeAndProviderRegistration() async {
        let runtime = RelayRuntimeProbe()
        var ready = false
        let controller = BackgroundAwarenessController(runtime: runtime, securityReady: { ready })
        var registrations = 0
        controller.start { registrations += 1 }
        let blocked = await controller.handleRemoteNotification(userInfo: wake())
        XCTAssertEqual(blocked, .unavailable)
        XCTAssertEqual(runtime.configurations, 0)
        XCTAssertEqual(registrations, 0)
        ready = true
        let accepted = await controller.handleRemoteNotification(userInfo: wake())
        XCTAssertEqual(accepted, .newData)
        XCTAssertEqual(runtime.configurations, 1)
    }

    func testSilentRegistrationNeverRequestsVisiblePermission() async {
        let permission = RelayPermissionProbe()
        let controller = BackgroundAwarenessController(permissionClient: permission, securityReady: { true })
        var registrations = 0
        controller.start { registrations += 1 }
        await controller.refreshPermissionState()
        XCTAssertEqual(registrations, 1)
        XCTAssertEqual(permission.visibleRequests, 0)
        let granted = await controller.requestVisiblePermissionInContext()
        XCTAssertTrue(granted)
        XCTAssertEqual(permission.visibleRequests, 1)
    }

    func testInstallationValidationAndDeduplicationAreDelegatedToRust() async {
        let runtime = RelayRuntimeProbe()
        let controller = BackgroundAwarenessController(runtime: runtime, securityReady: { true })
        let first = await controller.handleRemoteNotification(userInfo: wake(installation: "installation_host_one", cursor: 999))
        let second = await controller.handleRemoteNotification(userInfo: wake(installation: "installation_host_two", cursor: 1))
        runtime.changed = false
        let duplicate = await controller.handleRemoteNotification(userInfo: wake(installation: "installation_host_two", cursor: 1))
        XCTAssertEqual(first, .newData)
        XCTAssertEqual(second, .newData)
        XCTAssertEqual(duplicate, .noData)
        XCTAssertEqual(runtime.hints.map(\.installationId), ["installation_host_one", "installation_host_two", "installation_host_two"])
        XCTAssertEqual(runtime.hints.map(\.cursor), [999, 1, 1])
    }

    func testMalformedAndVisibleWakeNeverReachRuntime() async {
        let runtime = RelayRuntimeProbe()
        let controller = BackgroundAwarenessController(runtime: runtime, securityReady: { true })
        var visible = wake()
        visible["aps"] = ["alert": "Content", "content-available": 1]
        let rejected = await controller.handleRemoteNotification(userInfo: visible)
        XCTAssertEqual(rejected, .noData)
        XCTAssertEqual(runtime.configurations, 0)
        XCTAssertTrue(runtime.hints.isEmpty)
    }

    func testUnknownInstallationCannotPoisonLaterValidWake() async {
        let runtime = RelayRuntimeProbe()
        runtime.ingestError = .UnknownInstallation
        let controller = BackgroundAwarenessController(runtime: runtime, securityReady: { true })
        let unknown = await controller.handleRemoteNotification(userInfo: wake(cursor: 999))
        XCTAssertEqual(unknown, .noData)
        runtime.ingestError = nil
        let known = await controller.handleRemoteNotification(userInfo: wake(cursor: 1))
        XCTAssertEqual(known, .newData)
        XCTAssertEqual(runtime.hints.map(\.cursor), [999, 1])
    }

    func testOverlappingWakeConfigurationRunsOnceAndFailureCanRetry() async {
        let runtime = RelayRuntimeProbe()
        let gate = RelayTestGate()
        runtime.configureGate = gate
        let controller = BackgroundAwarenessController(runtime: runtime, securityReady: { true })
        let first = Task { await controller.handleRemoteNotification(userInfo: wake(cursor: 1)) }
        await waitUntil { gate.waiting }
        let second = Task { await controller.handleRemoteNotification(userInfo: wake(cursor: 2)) }
        await Task.yield()
        XCTAssertEqual(runtime.configurations, 1)
        gate.release()
        let firstResult = await first.value
        let secondResult = await second.value
        XCTAssertEqual(firstResult, .newData)
        XCTAssertEqual(secondResult, .newData)
        XCTAssertEqual(runtime.configurations, 1)

        let retryRuntime = RelayRuntimeProbe()
        retryRuntime.configureError = true
        let retryController = BackgroundAwarenessController(runtime: retryRuntime, securityReady: { true })
        let unavailable = await retryController.handleRemoteNotification(userInfo: wake())
        XCTAssertEqual(unavailable, .unavailable)
        retryRuntime.configureError = false
        let retried = await retryController.handleRemoteNotification(userInfo: wake())
        XCTAssertEqual(retried, .newData)
        XCTAssertEqual(retryRuntime.configurations, 2)
    }

    func testOSDeadlineIncludesConfigurationAndDoesNotStartLateIngest() async {
        let runtime = RelayRuntimeProbe()
        let configuration = RelayTestGate()
        let deadline = RelayTestGate()
        runtime.configureGate = configuration
        let controller = BackgroundAwarenessController(
            runtime: runtime, securityReady: { true }, sleep: { _ in await deadline.wait() }
        )
        let callback = Task { await controller.handleRemoteNotification(userInfo: wake()) }
        await waitUntil { configuration.waiting && deadline.waiting }
        deadline.release()
        let result = await callback.value
        XCTAssertEqual(result, .timedOut)
        XCTAssertTrue(runtime.hints.isEmpty)
        configuration.release()
        await waitUntil { !configuration.waiting }
        await Task.yield()
        XCTAssertTrue(runtime.hints.isEmpty)
    }

    func testUncooperativeLateRepairCannotAdvanceAnyNativeCursor() async {
        let runtime = RelayRuntimeProbe()
        let repair = RelayTestGate()
        let deadline = RelayTestGate()
        runtime.ingestGate = repair
        let controller = BackgroundAwarenessController(
            runtime: runtime, securityReady: { true }, sleep: { _ in await deadline.wait() }
        )
        let callback = Task { await controller.handleRemoteNotification(userInfo: wake(cursor: 7)) }
        await waitUntil { repair.waiting && deadline.waiting }
        deadline.release()
        let result = await callback.value
        XCTAssertEqual(result, .timedOut)
        repair.release()
        await waitUntil { !repair.waiting }
        XCTAssertNil(controller.relayStatus)
        runtime.ingestGate = nil
        let retryController = BackgroundAwarenessController(runtime: runtime, securityReady: { true })
        let retry = await retryController.handleRemoteNotification(userInfo: wake(cursor: 7))
        XCTAssertEqual(retry, .newData)
        XCTAssertEqual(runtime.hints.map(\.cursor), [7, 7])
    }

    func testCancelledCallbackReturnsWithoutAwaitingConfiguration() async {
        let runtime = RelayRuntimeProbe()
        let controller = BackgroundAwarenessController(runtime: runtime, securityReady: { true })
        let callback = Task { await controller.handleRemoteNotification(userInfo: wake()) }
        callback.cancel()
        let result = await callback.value
        XCTAssertEqual(result, .timedOut)
        XCTAssertTrue(runtime.hints.isEmpty)
    }

    func testColdRuntimeActivationSharesTheOSDeadline() async {
        let activation = RelayTestGate()
        let deadline = RelayTestGate()
        let controller = BackgroundAwarenessController(
            securityReady: { true }, activateRuntime: { await activation.wait() },
            sleep: { _ in await deadline.wait() }
        )
        let callback = Task { await controller.handleRemoteNotification(userInfo: wake()) }
        await waitUntil { activation.waiting && deadline.waiting }
        deadline.release()
        let result = await callback.value
        XCTAssertEqual(result, .timedOut)
        activation.release()
        await waitUntil { !activation.waiting }
    }

    func testTokenBeforePairingAndSecondHostReplaysSameDurableInput() async throws {
        let service = relayTestService()
        defer { removeRelayTestService(service) }
        let secrets = NativeRelaySecretBackend(service: service)
        let custody = NativeRelayProviderTokenCustody(secrets: secrets, environment: .sandbox)
        try await custody.observe(token: Data([1, 2, 3]))
        try await custody.observe(token: Data([1, 2, 3]))
        let runtime = RelayRuntimeProbe()
        runtime.hostCount = 0
        let noHosts = try await custody.synchronize(runtime: runtime)
        XCTAssertEqual(noHosts?.attempted, 0)

        let relaunchedCustody = NativeRelayProviderTokenCustody(secrets: secrets, environment: .sandbox)
        let controller = BackgroundAwarenessController(
            runtime: runtime, tokenCustody: relaunchedCustody, securityReady: { true }
        )
        runtime.hostCount = 1
        _ = await controller.reconcile()
        XCTAssertEqual(controller.lastTokenSync?.attempted, 1)
        runtime.hostCount = 2
        _ = await controller.reconcile()
        XCTAssertEqual(controller.lastTokenSync?.attempted, 2)
        XCTAssertEqual(controller.relayStatus?.bindings.count, 2)
        XCTAssertEqual(runtime.observations.map(\.localGeneration), [1, 1, 1])
        XCTAssertEqual(runtime.observedTokens, [[1, 2, 3], [1, 2, 3], [1, 2, 3]])
    }

    func testProviderCallbacksAndTombstoneRemainOrderedAcrossNetworkFailureAndRestart() async throws {
        let service = relayTestService()
        defer { removeRelayTestService(service) }
        let secrets = NativeRelaySecretBackend(service: service)
        let runtime = RelayRuntimeProbe()
        runtime.tokenError = true
        let controller = BackgroundAwarenessController(
            runtime: runtime,
            tokenCustody: NativeRelayProviderTokenCustody(secrets: secrets, environment: .sandbox),
            securityReady: { true }
        )
        controller.didRegisterForRemoteNotifications(deviceToken: Data([1]))
        controller.didRegisterForRemoteNotifications(deviceToken: Data([2]))
        await controller.tombstoneCurrentToken()
        XCTAssertEqual(runtime.tokenOperations, ["observe:1", "observe:2", "tombstone:3"])

        runtime.tokenError = false
        let relaunched = NativeRelayProviderTokenCustody(secrets: secrets, environment: .sandbox)
        _ = try await relaunched.synchronize(runtime: runtime)
        XCTAssertEqual(runtime.tokenOperations.last, "tombstone:3")
        try await relaunched.observe(token: Data([3]))
        _ = try await relaunched.synchronize(runtime: runtime)
        XCTAssertEqual(runtime.tokenOperations.last, "observe:4")
        let production = NativeRelayProviderTokenCustody(secrets: secrets, environment: .production)
        try await production.observe(token: Data([4]))
        _ = try await production.synchronize(runtime: runtime)
        XCTAssertEqual(runtime.observations.last?.environment, .production)
        XCTAssertEqual(runtime.observations.last?.localGeneration, 1)
    }

    private func waitUntil(_ condition: @MainActor () -> Bool) async {
        let deadline = ContinuousClock.now.advanced(by: .seconds(2))
        while !condition(), ContinuousClock.now < deadline { await Task.yield() }
        XCTAssertTrue(condition())
    }
}

@MainActor
private final class RelayTestGate {
    private var continuation: CheckedContinuation<Void, Never>?
    private(set) var waiting = false
    func wait() async {
        waiting = true
        await withCheckedContinuation { continuation = $0 }
        waiting = false
    }
    func release() {
        continuation?.resume()
        continuation = nil
    }
}

@MainActor
private final class RelayRuntimeProbe: NativeBackgroundRelayRuntime {
    var configurations = 0
    var configureError = false
    var configureGate: RelayTestGate?
    var ingestGate: RelayTestGate?
    var ingestError: BackgroundRelayError?
    var tokenError = false
    var changed = true
    var hostCount = 1
    var hints: [AppRelayWakeHint] = []
    var observations: [AppRelayPushTokenObservation] = []
    var observedTokens: [[UInt8]] = []
    var tokenOperations: [String] = []

    func configure() async throws {
        configurations += 1
        if let configureGate { await configureGate.wait() }
        if configureError { throw BackgroundRelayError.SecureStorageUnavailable }
    }
    func observe(observation: AppRelayPushTokenObservation, token: AppRelaySecretValue) async throws -> AppRelayFanoutReceipt {
        observations.append(observation)
        observedTokens.append(token.withUnsafeBytes { Array($0) })
        tokenOperations.append("observe:\(observation.localGeneration)")
        if tokenError { throw BackgroundRelayError.Retryable }
        return fanout()
    }
    func tombstone(tombstone: AppRelayPushTokenTombstone) async throws -> AppRelayFanoutReceipt {
        tokenOperations.append("tombstone:\(tombstone.throughLocalGeneration)")
        if tokenError { throw BackgroundRelayError.Retryable }
        return fanout()
    }
    func ingest(hint: AppRelayWakeHint) async throws -> AppRelayReconcileReceipt {
        hints.append(hint)
        if let ingestGate { await ingestGate.wait() }
        if let ingestError { throw ingestError }
        return AppRelayReconcileReceipt(hostId: "host", appliedThroughCursor: hint.cursor,
                                       acknowledgedThroughCursor: hint.cursor, changed: changed)
    }
    func reconcile() async throws -> [AppRelayReconcileOutcome] { [] }
    func status() async throws -> AppRelayStatusSnapshot {
        AppRelayStatusSnapshot(configured: true, bindings: (0..<hostCount).map { index in
            AppRelayBindingStatus(hostId: "host-\(index)", installationId: "installation_host_\(index)",
                                  state: .active, highestSeenCursor: 0, appliedCursor: 0,
                                  pendingAckCursor: 0, providerRegistrationCount: 1, hasPendingProviderSync: false)
        })
    }
    private func fanout() -> AppRelayFanoutReceipt {
        AppRelayFanoutReceipt(attempted: UInt32(hostCount), synchronized: UInt32(hostCount),
                              pendingRetry: 0, rePairRequired: 0, rejected: 0)
    }
}

@MainActor
private final class RelayPermissionProbe: NotificationPermissionClient {
    var visibleRequests = 0
    func settings() async -> NotificationPermissionState {
        NotificationPermissionState(authorization: .notDetermined, alertsEnabled: false,
                                    soundsEnabled: false, badgesEnabled: false)
    }
    func requestVisibleAuthorization() async throws -> Bool { visibleRequests += 1; return true }
}

private func wake(installation: String = "installation_0123456789abcdef", cursor: UInt64 = 1) -> [AnyHashable: Any] {
    ["aps": ["content-available": 1], "schema_version": 1,
     "installation_id": installation, "event_id": "event_0123456789abcdef",
     "cursor": NSNumber(value: cursor), "event_class": "state_changed",
     "expires_at_ms": NSNumber(value: UInt64(Date().addingTimeInterval(300).timeIntervalSince1970 * 1_000))]
}

private func relayTestService() -> String { "com.remora.app.tests.relay-ingress.\(UUID().uuidString)" }

private func removeRelayTestService(_ service: String) {
    SecItemDelete([kSecClass as String: kSecClassGenericPassword,
                   kSecAttrService as String: service,
                   kSecUseDataProtectionKeychain as String: true] as CFDictionary)
}
