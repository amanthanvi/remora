import Foundation
import XCTest
@testable import Remora

final class OpaqueWakePayloadTests: XCTestCase {
    private let now = Date(timeIntervalSince1970: 2_000_000_000)

    func testDecodesOnlyTheContentFreeVersionedWakeSchema() throws {
        let payload = try OpaqueWakePayload(
            userInfo: makeWakePayload(cursor: 42),
            now: now
        )

        XCTAssertEqual(payload.installationID, "installation_0123456789abcdef")
        XCTAssertEqual(payload.eventID, "event_0123456789abcdef")
        XCTAssertEqual(payload.cursor, 42)
        XCTAssertEqual(payload.eventClass, .stateChanged)
        XCTAssertEqual(payload.expiresAt, now.addingTimeInterval(300))
    }

    func testRejectsHostThreadUserAndContentFields() {
        for forbiddenKey in ["host_id", "thread_id", "user_id", "content", "command", "approval"] {
            var userInfo = makeWakePayload(cursor: 1)
            userInfo[forbiddenKey] = "forbidden_0123456789"

            XCTAssertThrowsError(try OpaqueWakePayload(userInfo: userInfo, now: now)) { error in
                XCTAssertEqual(error as? OpaqueWakePayloadError, .invalidRootKeys)
            }
        }
    }

    func testRejectsVisibleAlertAndActionEnvelope() {
        var alert = makeWakePayload(cursor: 1)
        alert["aps"] = [
            "content-available": 1,
            "alert": "Approval required",
            "category": "APPROVE"
        ]

        XCTAssertThrowsError(try OpaqueWakePayload(userInfo: alert, now: now)) { error in
            XCTAssertEqual(error as? OpaqueWakePayloadError, .invalidBackgroundEnvelope)
        }
    }

    func testRejectsExpiredAndLongLivedWakeHints() {
        var expired = makeWakePayload(cursor: 1)
        expired["expires_at_ms"] = milliseconds(now.addingTimeInterval(-1))
        XCTAssertThrowsError(try OpaqueWakePayload(userInfo: expired, now: now)) { error in
            XCTAssertEqual(error as? OpaqueWakePayloadError, .expired)
        }

        var distant = makeWakePayload(cursor: 1)
        distant["expires_at_ms"] = milliseconds(
            now.addingTimeInterval(OpaqueWakePayload.maximumExpirationHorizon + 1)
        )
        XCTAssertThrowsError(try OpaqueWakePayload(userInfo: distant, now: now)) { error in
            XCTAssertEqual(error as? OpaqueWakePayloadError, .expirationTooDistant)
        }
    }

    func testRejectsBooleanAndFractionalIntegerFields() {
        var booleanCursor = makeWakePayload(cursor: 1)
        booleanCursor["cursor"] = true
        XCTAssertThrowsError(try OpaqueWakePayload(userInfo: booleanCursor, now: now)) { error in
            XCTAssertEqual(error as? OpaqueWakePayloadError, .invalidField("cursor"))
        }

        var fractionalExpiry = makeWakePayload(cursor: 1)
        fractionalExpiry["expires_at_ms"] = 2_000_000_300_000.5
        XCTAssertThrowsError(try OpaqueWakePayload(userInfo: fractionalExpiry, now: now)) { error in
            XCTAssertEqual(error as? OpaqueWakePayloadError, .invalidField("expires_at_ms"))
        }
    }

    func testRejectsUnknownEventClassAndOversizedPayload() {
        var unknownClass = makeWakePayload(cursor: 1)
        unknownClass["event_class"] = "approval_required"
        XCTAssertThrowsError(try OpaqueWakePayload(userInfo: unknownClass, now: now)) { error in
            XCTAssertEqual(error as? OpaqueWakePayloadError, .invalidField("event_class"))
        }

        var oversized = makeWakePayload(cursor: 1)
        oversized["event_id"] = String(repeating: "a", count: OpaqueWakePayload.maximumEncodedBytes)
        XCTAssertThrowsError(try OpaqueWakePayload(userInfo: oversized, now: now)) { error in
            XCTAssertEqual(error as? OpaqueWakePayloadError, .payloadTooLarge)
        }
    }

    private func makeWakePayload(cursor: UInt64) -> [AnyHashable: Any] {
        [
            "aps": ["content-available": 1],
            "schema_version": 1,
            "installation_id": "installation_0123456789abcdef",
            "event_id": "event_0123456789abcdef",
            "cursor": NSNumber(value: cursor),
            "event_class": "state_changed",
            "expires_at_ms": milliseconds(now.addingTimeInterval(300))
        ]
    }

    private func milliseconds(_ date: Date) -> NSNumber {
        NSNumber(value: UInt64(date.timeIntervalSince1970 * 1_000))
    }
}

@MainActor
final class APNsTokenLifecycleTests: XCTestCase {
    func testUpsertIsIdempotentAndRotationAdvancesGeneration() async throws {
        let stateStore = MemoryAPNsTokenStateStore()
        let registry = RecordingPushTokenRegistry()
        let lifecycle = APNsTokenLifecycle(stateStore: stateStore, registry: registry)
        let firstToken = Data([0x01, 0x02, 0x03])
        let rotatedToken = Data([0x04, 0x05, 0x06])

        let (first, firstState) = try await lifecycle.register(token: firstToken)
        let (same, sameState) = try await lifecycle.register(token: firstToken)
        let (rotated, rotatedState) = try await lifecycle.register(token: rotatedToken)

        XCTAssertEqual(first.generation, 1)
        XCTAssertNil(first.replacesGeneration)
        XCTAssertEqual(firstState, .synced(generation: 1))
        XCTAssertEqual(same.generation, 1)
        XCTAssertNil(same.replacesGeneration)
        XCTAssertEqual(sameState, .synced(generation: 1))
        XCTAssertEqual(rotated.generation, 2)
        XCTAssertEqual(rotated.replacesGeneration, 1)
        XCTAssertEqual(rotatedState, .synced(generation: 2))
        XCTAssertEqual(Set(registry.upserts.map(\.clientInstanceID)).count, 1)
        XCTAssertEqual(
            registry.upserts.map(\.installationID),
            [nil, registry.installationID, registry.installationID]
        )
        XCTAssertEqual(registry.upserts.map(\.generation), [1, 1, 2])
        let persisted = stateStore.record?.registration(for: .current)
        XCTAssertEqual(persisted?.relayInstallationID, registry.installationID)
        XCTAssertEqual(persisted?.tokenDigest?.count, 32)
        XCTAssertNotEqual(persisted?.tokenDigest, rotatedToken)
    }

    func testPendingRegistrationFlushesWhenRegistryIsBound() async throws {
        let lifecycle = APNsTokenLifecycle(stateStore: MemoryAPNsTokenStateStore())
        let registry = RecordingPushTokenRegistry()

        let (registration, pending) = try await lifecycle.register(token: Data([0x01]))
        let flushed = await lifecycle.bind(registry: registry)

        XCTAssertEqual(pending, .pending(generation: registration.generation))
        XCTAssertEqual(flushed, .synced(generation: registration.generation))
        XCTAssertEqual(registry.upserts.count, 1)
    }

    func testTombstoneSurvivesMissingRegistryAndFlushesLater() async throws {
        let stateStore = MemoryAPNsTokenStateStore()
        let registry = RecordingPushTokenRegistry()
        let lifecycleBeforeTermination = APNsTokenLifecycle(stateStore: stateStore)
        _ = try await lifecycleBeforeTermination.register(token: Data([0x01]))

        let pending = try await lifecycleBeforeTermination.tombstone()
        XCTAssertEqual(
            stateStore.record?.registration(for: .current).pendingTombstoneThroughGeneration,
            1
        )

        // Recreate the lifecycle to prove the revocation intent is durable
        // across process death, not merely retained by an in-memory property.
        let lifecycleAfterRelaunch = APNsTokenLifecycle(stateStore: stateStore)
        let flushed = await lifecycleAfterRelaunch.bind(registry: registry)

        XCTAssertEqual(pending, .pending(generation: 1))
        XCTAssertEqual(flushed, .tombstoned(generation: 1))
        XCTAssertEqual(registry.tombstones.map(\.throughGeneration), [1])
        let persisted = stateStore.record?.registration(for: .current)
        XCTAssertNil(persisted?.tokenDigest)
        XCTAssertNil(persisted?.pendingTombstoneThroughGeneration)
    }

    func testOverlappingRotationCannotEraseNewerFailedRegistration() async throws {
        let stateStore = MemoryAPNsTokenStateStore()
        let racingRegistry = RacingPushTokenRegistry()
        let lifecycle = APNsTokenLifecycle(stateStore: stateStore, registry: racingRegistry)

        let firstTask = Task { try await lifecycle.register(token: Data([0x01])) }
        await racingRegistry.waitForUpsertCount(1)
        let rotatedTask = Task { try await lifecycle.register(token: Data([0x02])) }
        await racingRegistry.waitForUpsertCount(2)
        racingRegistry.resumeFirstUpsert()

        let (_, firstState) = try await firstTask.value
        let (_, rotatedState) = try await rotatedTask.value
        XCTAssertEqual(firstState, .synced(generation: 1))
        XCTAssertEqual(rotatedState, .failed(generation: 2))

        let recoveryRegistry = RecordingPushTokenRegistry()
        let recoveredState = await lifecycle.bind(registry: recoveryRegistry)

        XCTAssertEqual(recoveredState, .synced(generation: 2))
        XCTAssertEqual(recoveryRegistry.upserts.map(\.generation), [2])
        XCTAssertEqual(recoveryRegistry.upserts.first?.token, Data([0x02]))
    }

    func testGenerationsAndDurableTombstonesAreScopedByEnvironment() async throws {
        let stateStore = MemoryAPNsTokenStateStore()
        let sandboxRegistry = RecordingPushTokenRegistry()
        let sandboxLifecycle = APNsTokenLifecycle(
            stateStore: stateStore,
            registry: sandboxRegistry,
            environment: .sandbox
        )
        _ = try await sandboxLifecycle.register(token: Data([0x01]))
        _ = await sandboxLifecycle.bind(registry: nil)
        let pending = try await sandboxLifecycle.tombstone()

        let productionRegistry = RecordingPushTokenRegistry()
        let productionLifecycle = APNsTokenLifecycle(
            stateStore: stateStore,
            registry: productionRegistry,
            environment: .production
        )
        let (productionRegistration, productionState) = try await productionLifecycle.register(
            token: Data([0x02])
        )
        let flushed = await productionLifecycle.bind(registry: productionRegistry)

        XCTAssertEqual(pending, .pending(generation: 1))
        XCTAssertEqual(productionRegistration.generation, 1)
        XCTAssertEqual(productionState, .synced(generation: 1))
        XCTAssertEqual(flushed, .tombstoned(generation: 1))
        XCTAssertEqual(productionRegistry.tombstones.map(\.environment), [.sandbox])
        XCTAssertEqual(productionRegistry.tombstones.map(\.provider), [.apns])

        let sandbox = stateStore.record?.registration(for: .sandbox)
        let production = stateStore.record?.registration(for: .production)
        XCTAssertEqual(sandbox?.generation, 1)
        XCTAssertNil(sandbox?.tokenDigest)
        XCTAssertNil(sandbox?.pendingTombstoneThroughGeneration)
        XCTAssertEqual(production?.generation, 1)
        XCTAssertNotNil(production?.tokenDigest)
    }
}

@MainActor
final class OpaqueWakeReconciliationCoordinatorTests: XCTestCase {
    func testDuplicateAndReorderedCursorsDoNotRepeatReconciliation() async throws {
        let cursorStore = MemoryWakeCursorStore()
        let reconciler = RecordingBackgroundReconciler(results: [.changed])
        let coordinator = OpaqueWakeReconciliationCoordinator(
            reconciler: reconciler,
            cursorStore: cursorStore,
            timeout: .seconds(1)
        )
        let now = Date(timeIntervalSince1970: 2_000_000_000)
        let payload = try makePayload(cursor: 10, now: now)

        let initialResult = await coordinator.reconcile(payload)
        let duplicateResult = await coordinator.reconcile(payload)
        let reorderedPayload = try makePayload(cursor: 8, now: now)
        let reorderedResult = await coordinator.reconcile(reorderedPayload)

        XCTAssertEqual(initialResult, .newData)
        XCTAssertEqual(duplicateResult, .noData)
        XCTAssertEqual(reorderedResult, .noData)
        XCTAssertEqual(reconciler.cursors, [10])
        XCTAssertEqual(cursorStore.lastReconciledCursor, 10)
    }

    func testCursorGapSchedulesOneAuthoritativeReconciliation() async throws {
        let cursorStore = MemoryWakeCursorStore(lastReconciledCursor: 2)
        let reconciler = RecordingBackgroundReconciler(results: [.unchanged])
        let coordinator = OpaqueWakeReconciliationCoordinator(
            reconciler: reconciler,
            cursorStore: cursorStore,
            timeout: .seconds(1)
        )
        let now = Date(timeIntervalSince1970: 2_000_000_000)

        let payload = try makePayload(cursor: 100, now: now)
        let result = await coordinator.reconcile(payload)

        XCTAssertEqual(result, .noData)
        XCTAssertEqual(reconciler.cursors, [100])
        XCTAssertEqual(cursorStore.lastReconciledCursor, 100)
    }

    func testFailedWakeRemainsPendingForForegroundRetry() async throws {
        let cursorStore = MemoryWakeCursorStore()
        let reconciler = RecordingBackgroundReconciler(results: [.failed, .changed])
        let coordinator = OpaqueWakeReconciliationCoordinator(
            reconciler: reconciler,
            cursorStore: cursorStore,
            timeout: .seconds(1)
        )
        let now = Date(timeIntervalSince1970: 2_000_000_000)

        let payload = try makePayload(cursor: 7, now: now)
        let result = await coordinator.reconcile(payload)

        XCTAssertEqual(result, .failed)
        coordinator.retryPendingOnForeground()
        await reconciler.waitForCallCount(2)
        for _ in 0..<100 where cursorStore.lastReconciledCursor < 7 {
            await Task.yield()
        }

        XCTAssertEqual(reconciler.cursors, [7, 7])
        XCTAssertEqual(cursorStore.lastReconciledCursor, 7)
    }

    func testReconciliationReturnsAtDeadlineWithoutAdvancingCursor() async throws {
        let cursorStore = MemoryWakeCursorStore()
        let reconciler = SuspendingBackgroundReconciler()
        let coordinator = OpaqueWakeReconciliationCoordinator(
            reconciler: reconciler,
            cursorStore: cursorStore,
            timeout: .milliseconds(10)
        )
        let now = Date(timeIntervalSince1970: 2_000_000_000)
        let payload = try makePayload(cursor: 12, now: now)
        let clock = ContinuousClock()

        let start = clock.now
        let result = await coordinator.reconcile(payload)
        let elapsed = start.duration(to: clock.now)

        XCTAssertEqual(result, .timedOut)
        XCTAssertEqual(cursorStore.lastReconciledCursor, 0)
        XCTAssertTrue(elapsed < .milliseconds(250))
    }

    func testHigherCursorArrivingDuringDrainIsNotStranded() async throws {
        let cursorStore = MemoryWakeCursorStore()
        let reconciler = GatedBackgroundReconciler()
        let coordinator = OpaqueWakeReconciliationCoordinator(
            reconciler: reconciler,
            cursorStore: cursorStore,
            timeout: .seconds(1)
        )
        let now = Date(timeIntervalSince1970: 2_000_000_000)
        let firstPayload = try makePayload(cursor: 10, now: now)
        let newerPayload = try makePayload(cursor: 20, now: now)

        let firstTask = Task { await coordinator.reconcile(firstPayload) }
        await reconciler.waitForCallCount(1)
        let newerTask = Task { await coordinator.reconcile(newerPayload) }
        await Task.yield()
        reconciler.resumeFirstCall()

        _ = await firstTask.value
        _ = await newerTask.value

        XCTAssertEqual(reconciler.cursors, [10, 20])
        XCTAssertEqual(cursorStore.lastReconciledCursor, 20)
    }

    func testQueuedCursorsShareOneAbsoluteCallbackDeadline() async throws {
        let cursorStore = MemoryWakeCursorStore()
        let reconciler = DelayedThenSuspendingBackgroundReconciler()
        let coordinator = OpaqueWakeReconciliationCoordinator(
            reconciler: reconciler,
            cursorStore: cursorStore,
            timeout: .milliseconds(100)
        )
        let now = Date(timeIntervalSince1970: 2_000_000_000)
        let firstPayload = try makePayload(cursor: 10, now: now)
        let newerPayload = try makePayload(cursor: 20, now: now)
        let clock = ContinuousClock()

        let start = clock.now
        let firstTask = Task { await coordinator.reconcile(firstPayload) }
        await reconciler.waitForCallCount(1)
        let newerTask = Task { await coordinator.reconcile(newerPayload) }
        let firstResult = await firstTask.value
        let newerResult = await newerTask.value
        let elapsed = start.duration(to: clock.now)

        XCTAssertEqual(firstResult, .timedOut)
        XCTAssertEqual(newerResult, .timedOut)
        XCTAssertEqual(reconciler.cursors, [10, 20])
        XCTAssertEqual(cursorStore.lastReconciledCursor, 10)
        XCTAssertTrue(elapsed < .milliseconds(150))
    }

    private func makePayload(cursor: UInt64, now: Date) throws -> OpaqueWakePayload {
        try OpaqueWakePayload(
            userInfo: [
                "aps": ["content-available": 1],
                "schema_version": 1,
                "installation_id": "installation_0123456789abcdef",
                "event_id": "event_0123456789abcdef",
                "cursor": NSNumber(value: cursor),
                "event_class": "connection_changed",
                "expires_at_ms": NSNumber(value: UInt64((now.timeIntervalSince1970 + 60) * 1_000))
            ],
            now: now
        )
    }
}

@MainActor
final class BackgroundAwarenessPermissionTests: XCTestCase {
    func testStartRegistersWithoutPromptingForVisiblePermission() async {
        let permissionClient = RecordingPermissionClient()
        let controller = BackgroundAwarenessController(
            permissionClient: permissionClient,
            tokenLifecycle: APNsTokenLifecycle(stateStore: MemoryAPNsTokenStateStore()),
            reconciliation: OpaqueWakeReconciliationCoordinator(
                cursorStore: MemoryWakeCursorStore()
            )
        )
        var registrationRequests = 0

        controller.start { registrationRequests += 1 }
        await Task.yield()

        XCTAssertEqual(registrationRequests, 1)
        XCTAssertEqual(permissionClient.settingsRequests, 1)
        XCTAssertEqual(permissionClient.authorizationRequests, 0)
        XCTAssertEqual(controller.permissionState.authorization, .notDetermined)
    }

    func testVisiblePermissionIsRequestedOnlyThroughInContextMethod() async {
        let permissionClient = RecordingPermissionClient()
        let controller = BackgroundAwarenessController(
            permissionClient: permissionClient,
            tokenLifecycle: APNsTokenLifecycle(stateStore: MemoryAPNsTokenStateStore()),
            reconciliation: OpaqueWakeReconciliationCoordinator(
                cursorStore: MemoryWakeCursorStore()
            )
        )

        let granted = await controller.requestVisiblePermissionInContext()

        XCTAssertTrue(granted)
        XCTAssertEqual(permissionClient.authorizationRequests, 1)
        XCTAssertEqual(permissionClient.settingsRequests, 1)
    }
}

@MainActor
private final class MemoryAPNsTokenStateStore: APNsTokenLifecycleStateStore {
    var record: APNsTokenLifecycleState?

    func load() throws -> APNsTokenLifecycleState? { record }
    func save(_ state: APNsTokenLifecycleState) throws { record = state }
}

@MainActor
private final class RecordingPushTokenRegistry: PushTokenRegistry {
    let installationID = "inst_0123456789abcdef"
    var upserts: [APNsTokenRegistration] = []
    var tombstones: [APNsTokenTombstone] = []

    func upsert(_ registration: APNsTokenRegistration) async throws -> PushTokenRegistrationReceipt {
        upserts.append(registration)
        return PushTokenRegistrationReceipt(
            schemaVersion: PushTokenRegistrationReceipt.currentSchemaVersion,
            installationID: installationID,
            registrationID: "device_0123456789abcdef",
            provider: registration.provider,
            environment: registration.environment,
            generation: registration.generation,
            replaced: registration.replacesGeneration != nil
        )
    }

    func tombstone(_ tombstone: APNsTokenTombstone) async throws {
        tombstones.append(tombstone)
    }
}

@MainActor
private final class RacingPushTokenRegistry: PushTokenRegistry {
    private var firstContinuation: CheckedContinuation<Void, Never>?
    private(set) var upserts: [APNsTokenRegistration] = []

    func upsert(_ registration: APNsTokenRegistration) async throws -> PushTokenRegistrationReceipt {
        upserts.append(registration)
        if upserts.count == 1 {
            await withCheckedContinuation { continuation in
                firstContinuation = continuation
            }
        } else {
            throw RacingRegistryError.expectedFailure
        }
        return PushTokenRegistrationReceipt(
            schemaVersion: PushTokenRegistrationReceipt.currentSchemaVersion,
            installationID: "inst_0123456789abcdef",
            registrationID: "device_0123456789abcdef",
            provider: registration.provider,
            environment: registration.environment,
            generation: registration.generation,
            replaced: false
        )
    }

    func tombstone(_ tombstone: APNsTokenTombstone) async throws {}

    func waitForUpsertCount(_ count: Int) async {
        for _ in 0..<100 where upserts.count < count {
            await Task.yield()
        }
    }

    func resumeFirstUpsert() {
        firstContinuation?.resume()
        firstContinuation = nil
    }
}

private enum RacingRegistryError: Error {
    case expectedFailure
}

@MainActor
private final class MemoryWakeCursorStore: WakeCursorStore {
    var lastReconciledCursor: UInt64

    init(lastReconciledCursor: UInt64 = 0) {
        self.lastReconciledCursor = lastReconciledCursor
    }
}

@MainActor
private final class RecordingBackgroundReconciler: BackgroundStateReconciling {
    private var results: [AuthenticatedBackgroundStateResult]
    private(set) var cursors: [UInt64] = []

    init(results: [AuthenticatedBackgroundStateResult]) {
        self.results = results
    }

    func reconcileBackgroundState(expectedCursor: UInt64) async -> AuthenticatedBackgroundStateResult {
        cursors.append(expectedCursor)
        guard !results.isEmpty else { return .unchanged }
        return results.removeFirst()
    }

    func waitForCallCount(_ count: Int) async {
        for _ in 0..<100 where cursors.count < count {
            await Task.yield()
        }
    }
}

@MainActor
private final class SuspendingBackgroundReconciler: BackgroundStateReconciling {
    func reconcileBackgroundState(expectedCursor: UInt64) async -> AuthenticatedBackgroundStateResult {
        try? await Task.sleep(for: .seconds(10))
        return .changed
    }
}

@MainActor
private final class GatedBackgroundReconciler: BackgroundStateReconciling {
    private var firstContinuation: CheckedContinuation<Void, Never>?
    private(set) var cursors: [UInt64] = []

    func reconcileBackgroundState(expectedCursor: UInt64) async -> AuthenticatedBackgroundStateResult {
        cursors.append(expectedCursor)
        if cursors.count == 1 {
            await withCheckedContinuation { continuation in
                firstContinuation = continuation
            }
        }
        return .changed
    }

    func waitForCallCount(_ count: Int) async {
        for _ in 0..<100 where cursors.count < count {
            await Task.yield()
        }
    }

    func resumeFirstCall() {
        firstContinuation?.resume()
        firstContinuation = nil
    }
}

@MainActor
private final class DelayedThenSuspendingBackgroundReconciler: BackgroundStateReconciling {
    private(set) var cursors: [UInt64] = []

    func reconcileBackgroundState(expectedCursor: UInt64) async -> AuthenticatedBackgroundStateResult {
        cursors.append(expectedCursor)
        if cursors.count == 1 {
            try? await Task.sleep(for: .milliseconds(80))
            return .changed
        }
        try? await Task.sleep(for: .seconds(10))
        return .changed
    }

    func waitForCallCount(_ count: Int) async {
        for _ in 0..<100 where cursors.count < count {
            await Task.yield()
        }
    }
}

@MainActor
private final class RecordingPermissionClient: NotificationPermissionClient {
    var settingsRequests = 0
    var authorizationRequests = 0

    func settings() async -> NotificationPermissionState {
        settingsRequests += 1
        return NotificationPermissionState(
            authorization: .notDetermined,
            alertsEnabled: false,
            soundsEnabled: false,
            badgesEnabled: false
        )
    }

    func requestVisibleAuthorization() async throws -> Bool {
        authorizationRequests += 1
        return true
    }
}
