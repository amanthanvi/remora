import Foundation
import XCTest
@testable import Remora

@MainActor
final class AppRuntimeControllerRemoraLinkTests: XCTestCase {
    func testTransientConfigurationFailureCanRetry() async {
        let script = RuntimeConfigurationScript(mode: .failFirstImmediately)
        let controller = makeController(script: script)
        let client = AppClient(noHandle: .init())

        controller.configureRemoraLinkIfNeeded(client: client)
        await waitUntil { controller.remoraLinkStatus == .unavailable }

        controller.configureRemoraLinkIfNeeded(client: client)
        await waitUntil { controller.remoraLinkStatus == .available }

        XCTAssertEqual(script.attempts, 2)
    }

    func testOverlappingBindRequestLatchesRetryWhenActiveAttemptFails() async {
        let script = RuntimeConfigurationScript(mode: .suspendThenFailFirst)
        let controller = makeController(script: script)
        let client = AppClient(noHandle: .init())

        controller.configureRemoraLinkIfNeeded(client: client)
        await waitUntil { script.isFirstAttemptSuspended }

        // This is the configuration request a second overlapping `bind`
        // issues. It must not start a duplicate concurrent preflight, but it
        // must survive the in-flight attempt's failure.
        controller.configureRemoraLinkIfNeeded(client: client)
        XCTAssertEqual(script.attempts, 1)
        XCTAssertEqual(controller.remoraLinkStatus, .configuring)

        script.releaseFirstAttempt()
        await waitUntil { controller.remoraLinkStatus == .available }

        XCTAssertEqual(script.attempts, 2)
        XCTAssertEqual(script.maximumConcurrentAttempts, 1)
    }

    func testSuccessfulOverlappingConfigurationFinishesLatestClientBeforeAvailable() async {
        let script = RuntimeConfigurationScript(mode: .suspendThenSucceedFirst)
        let controller = makeController(script: script)
        let firstClient = AppClient(noHandle: .init())
        let latestClient = AppClient(noHandle: .init())

        controller.configureRemoraLinkIfNeeded(client: firstClient)
        await waitUntil { script.isFirstAttemptSuspended }

        controller.configureRemoraLinkIfNeeded(client: latestClient)
        XCTAssertEqual(script.attempts, 1)
        XCTAssertEqual(controller.remoraLinkStatus, .configuring)

        script.releaseFirstAttempt()
        await waitUntil { controller.remoraLinkStatus == .available }

        XCTAssertEqual(
            script.configuredClientIDs,
            [ObjectIdentifier(firstClient), ObjectIdentifier(latestClient)]
        )
        XCTAssertEqual(script.attempts, 2)
        XCTAssertEqual(script.maximumConcurrentAttempts, 1)
    }

    func testForegroundRetriesUnavailableConfiguration() async {
        let script = RuntimeConfigurationScript(mode: .failFirstImmediately)
        let controller = makeController(script: script)
        let client = AppClient(noHandle: .init())

        controller.configureRemoraLinkIfNeeded(client: client)
        await waitUntil { controller.remoraLinkStatus == .unavailable }

        controller.retryRemoraLinkOnForegroundIfNeeded()
        await waitUntil { controller.remoraLinkStatus == .available }

        XCTAssertEqual(script.attempts, 2)
    }

    func testForegroundWhileConfiguringRetriesOnlyIfAttemptFails() async {
        let failingScript = RuntimeConfigurationScript(mode: .suspendThenFailFirst)
        let failingController = makeController(script: failingScript)
        let failingClient = AppClient(noHandle: .init())

        failingController.configureRemoraLinkIfNeeded(client: failingClient)
        await waitUntil { failingScript.isFirstAttemptSuspended }
        failingController.retryRemoraLinkOnForegroundIfNeeded()
        failingScript.releaseFirstAttempt()
        await waitUntil { failingController.remoraLinkStatus == .available }

        XCTAssertEqual(failingScript.attempts, 2)
        XCTAssertEqual(failingScript.maximumConcurrentAttempts, 1)

        let successfulScript = RuntimeConfigurationScript(mode: .suspendThenSucceedFirst)
        let successfulController = makeController(script: successfulScript)
        let successfulClient = AppClient(noHandle: .init())

        successfulController.configureRemoraLinkIfNeeded(client: successfulClient)
        await waitUntil { successfulScript.isFirstAttemptSuspended }
        successfulController.retryRemoraLinkOnForegroundIfNeeded()
        successfulScript.releaseFirstAttempt()
        await waitUntil { successfulController.remoraLinkStatus == .available }

        XCTAssertEqual(successfulScript.attempts, 1)
        XCTAssertEqual(successfulScript.maximumConcurrentAttempts, 1)
    }

    func testReachabilityMonitorStartsOnlyOnceAcrossRepeatedBinds() {
        let reachability = RuntimeReachabilityProbe()
        let controller = AppRuntimeController(
            reachability: reachability,
            remoraLinkConfigurator: { _, _ in }
        )

        controller.startReachabilityIfNeeded()
        controller.startReachabilityIfNeeded()
        controller.startReachabilityIfNeeded()

        XCTAssertEqual(reachability.startCount, 1)
    }

    private func makeController(
        script: RuntimeConfigurationScript
    ) -> AppRuntimeController {
        AppRuntimeController(
            reachability: RuntimeReachabilityProbe(),
            remoraLinkConfigurator: { client, _ in
                try await script.configure(client: client)
            }
        )
    }

    private func waitUntil(
        timeout: TimeInterval = 2,
        condition: @escaping @MainActor () -> Bool
    ) async {
        let deadline = Date(timeIntervalSinceNow: timeout)
        while Date() < deadline, !condition() {
            try? await Task.sleep(for: .milliseconds(10))
        }
        XCTAssertTrue(condition())
    }
}

@MainActor
private final class RuntimeReachabilityProbe: RemoraLinkReachabilityObserving {
    private(set) var startCount = 0

    func bind(appModel _: AppModel) {}

    func start() {
        startCount += 1
    }
}

@MainActor
private final class RuntimeConfigurationScript {
    enum Mode {
        case failFirstImmediately
        case suspendThenFailFirst
        case suspendThenSucceedFirst
    }

    private let mode: Mode
    private var activeAttempts = 0
    private var firstContinuation: CheckedContinuation<Void, Never>?

    private(set) var attempts = 0
    private(set) var maximumConcurrentAttempts = 0
    private(set) var configuredClientIDs: [ObjectIdentifier] = []

    var isFirstAttemptSuspended: Bool {
        firstContinuation != nil
    }

    init(mode: Mode) {
        self.mode = mode
    }

    func configure(client: AppClient) async throws {
        attempts += 1
        activeAttempts += 1
        maximumConcurrentAttempts = max(maximumConcurrentAttempts, activeAttempts)
        configuredClientIDs.append(ObjectIdentifier(client))
        defer { activeAttempts -= 1 }

        guard attempts == 1 else { return }
        switch mode {
        case .failFirstImmediately:
            throw RuntimeConfigurationFailure.transient
        case .suspendThenFailFirst:
            await withCheckedContinuation { continuation in
                firstContinuation = continuation
            }
            throw RuntimeConfigurationFailure.transient
        case .suspendThenSucceedFirst:
            await withCheckedContinuation { continuation in
                firstContinuation = continuation
            }
        }
    }

    func releaseFirstAttempt() {
        let continuation = firstContinuation
        firstContinuation = nil
        continuation?.resume()
    }
}

private enum RuntimeConfigurationFailure: Error {
    case transient
}
