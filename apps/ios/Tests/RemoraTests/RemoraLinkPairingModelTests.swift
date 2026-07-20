import XCTest
@testable import Remora

@MainActor
final class RemoraLinkPairingModelTests: XCTestCase {
    func testPairingUIUsesV2Command() {
        XCTAssertEqual(RemotePairingSheet.pairCommand, "npx --yes remora-link@latest pair")
    }

    func testPairingIngressMatchesPlatformCapabilities() {
        XCTAssertTrue(RemotePairingSheet.supportsQRScanning(rendersAsMacApp: false))
        XCTAssertFalse(RemotePairingSheet.supportsQRScanning(rendersAsMacApp: true))
    }

    func testAvailabilityMovesToIngressOnlyWhenNativeCustodyIsAvailable() {
        let model = makeModel()

        model.updateAvailability(.configuring)
        XCTAssertEqual(model.state, .availability(.configuring))

        model.updateAvailability(.unavailable)
        XCTAssertEqual(model.state, .availability(.unavailable))

        model.updateAvailability(.available)
        XCTAssertEqual(model.state, .ingress)
    }

    func testOversizedUTF8CodeFailsBeforeCallingFFI() async {
        var inspectCalls = 0
        let model = makeModel(inspect: { _ in
            inspectCalls += 1
            return .ready(offer: Self.offer)
        })

        model.inspect(codeText: String(repeating: "é", count: 2_065))
        await Task.yield()

        XCTAssertEqual(inspectCalls, 0)
        XCTAssertEqual(
            model.state,
            .failure("Pairing code is too large. Scan or paste a code up to 4,128 bytes.")
        )
    }

    func testInspectUsesSingleCarrierAndClearsItAfterUse() async throws {
        var retainedCarrier: AppRemoraLinkPairingCode?
        var inspectedText: String?
        let model = makeModel(inspect: { carrier in
            retainedCarrier = carrier
            inspectedText = carrier.withUnsafeBytes {
                String(decoding: $0.bindMemory(to: UInt8.self), as: UTF8.self)
            }
            return .ready(offer: Self.offer)
        })

        model.inspect(codeText: "secret-pairing-code")
        await waitUntil { model.state == .offer(Self.offer) }

        XCTAssertEqual(inspectedText, "secret-pairing-code")
        let remainingBytes = retainedCarrier?.withUnsafeBytes { Array($0) }
        XCTAssertEqual(remainingBytes, Array(repeating: 0, count: "secret-pairing-code".utf8.count))
    }

    func testLegacyInspectionMapsToExplicitRemoraLinkRepairState() async {
        let model = makeModel(inspect: { _ in
            .legacyRePairRequired(hostId: "host-v1", hostDisplayName: "Studio Mac")
        })

        model.inspect(codeText: "legacy-code")
        await waitUntil {
            model.state == .legacyRePair(hostId: "host-v1", hostDisplayName: "Studio Mac")
        }

        XCTAssertEqual(RemotePairingSheet.legacyRePairTitle, "Pair again with Remora Link")
        let message = RemotePairingSheet.legacyRePairMessage(hostDisplayName: "Studio Mac")
        XCTAssertTrue(message.contains("legacy invitation"))
        XCTAssertTrue(message.contains("new Remora Link pairing code"))
        XCTAssertFalse(message.contains("npx kittylitter"))
    }

    func testOfferUsesRustDefaultsAndKeepsRequiredScopesSelected() async {
        let model = makeModel(inspect: { _ in .ready(offer: Self.offer) })

        model.inspect(codeText: "code")
        await waitUntil { model.state == .offer(Self.offer) }

        XCTAssertEqual(model.selectedRuntimeIds, ["codex"])
        XCTAssertEqual(model.selectedScopes, [.inspectRuntimes, .connectRuntime])

        model.toggleScope(.inspectRuntimes)
        XCTAssertTrue(model.selectedScopes.contains(.inspectRuntimes), "Required scope must not toggle off")
        model.toggleRuntime("offline")
        XCTAssertFalse(model.selectedRuntimeIds.contains("offline"), "Unavailable runtime must not toggle on")
    }

    func testAcceptShowsPendingSASThenCompletesPairing() async {
        var receivedAcceptance: AppRemoraLinkAcceptance?
        let releaseAwait = AsyncGate()
        let model = makeModel(
            inspect: { _ in .ready(offer: Self.offer) },
            accept: { acceptance in
                receivedAcceptance = acceptance
                return .awaitingHostApproval(
                    hostId: "host-1",
                    sas: "142 857",
                    expiresAtUnixMs: 10_000,
                    selectedRuntimeIds: acceptance.selectedRuntimeIds,
                    requestedScopes: acceptance.requestedScopes
                )
            },
            awaitPairing: { hostId, code in
                XCTAssertEqual(hostId, "host-1")
                XCTAssertNil(code, "The inspected single-use code must not be retained for in-process await")
                await releaseAwait.wait()
                return .paired(
                    hostId: hostId,
                    sas: "142 857",
                    selectedRuntimeIds: ["codex"],
                    grantedScopes: [.inspectRuntimes, .connectRuntime],
                    createdAtUnixMs: 10_001
                )
            }
        )

        model.inspect(codeText: "code")
        await waitUntil { model.state == .offer(Self.offer) }
        model.deviceDisplayName = "Aman's iPhone"
        model.acceptOffer()
        await waitUntil {
            if case .awaiting = model.state { return true }
            return false
        }

        XCTAssertEqual(receivedAcceptance?.deviceDisplayName, "Aman's iPhone")
        XCTAssertEqual(receivedAcceptance?.selectedRuntimeIds, ["codex"])
        XCTAssertEqual(receivedAcceptance?.requestedScopes, [.inspectRuntimes, .connectRuntime])

        releaseAwait.open()
        await waitUntil {
            if case .success = model.state { return true }
            return false
        }
    }

    func testCancellationOutcomeUnknownIsExplicit() async {
        let pending = AppRemoraLinkPendingApproval(
            sas: "123 456",
            expiresAtUnixMs: 20_000,
            requestedScopes: [.connectRuntime],
            deviceDisplayName: "iPhone"
        )
        let model = makeModel(
            awaitPairing: { _, _ in
                try await Task.sleep(for: .seconds(30))
                throw CancellationError()
            },
            cancel: { _ in .outcomeUnknown }
        )

        model.resume(hostId: "host-1", pendingApproval: pending)
        model.cancelPairing()
        await waitUntil {
            if case .outcomeUnknown = model.state { return true }
            return false
        }
    }

    func testDeviceNameTruncationPreservesUnicodeBoundary() {
        let value = String(repeating: "🦞", count: 21)
        let truncated = RemoraLinkPairingModel.truncatedUTF8(value, maximumByteCount: 80)

        XCTAssertEqual(truncated, String(repeating: "🦞", count: 20))
        XCTAssertEqual(truncated.utf8.count, 80)
    }

    private func makeModel(
        inspect: @escaping @MainActor (AppRemoraLinkPairingCode) async throws -> AppRemoraLinkInspection = { _ in
            .ready(offer: RemoraLinkPairingModelTests.offer)
        },
        accept: @escaping @MainActor (AppRemoraLinkAcceptance) async throws -> AppRemoraLinkPairingOutcome = { acceptance in
            .alreadyPaired(
                hostId: "host-1",
                selectedRuntimeIds: acceptance.selectedRuntimeIds,
                grantedScopes: acceptance.requestedScopes
            )
        },
        awaitPairing: @escaping @MainActor (String, AppRemoraLinkPairingCode?) async throws -> AppRemoraLinkPairingOutcome = { hostId, _ in
            .alreadyPaired(hostId: hostId, selectedRuntimeIds: ["codex"], grantedScopes: [.connectRuntime])
        },
        cancel: @escaping @MainActor (String) async throws -> AppRemoraLinkPairingCancellationOutcome = { _ in .cancelled }
    ) -> RemoraLinkPairingModel {
        RemoraLinkPairingModel(
            operations: RemoraLinkPairingOperations(
                inspect: inspect,
                accept: accept,
                awaitPairing: awaitPairing,
                cancel: cancel
            ),
            initialState: .availability(.configuring),
            deviceDisplayName: "iPhone"
        )
    }

    private func waitUntil(
        timeout: Duration = .seconds(2),
        _ condition: @escaping @MainActor () -> Bool
    ) async {
        let clock = ContinuousClock()
        let deadline = clock.now.advanced(by: timeout)
        while !condition(), clock.now < deadline {
            await Task.yield()
        }
        XCTAssertTrue(condition(), "Timed out waiting for pairing state")
    }

    private static let offer = AppRemoraLinkOffer(
        offerId: "offer-1",
        hostId: "host-1",
        hostDisplayName: "Studio Mac",
        expiresAtUnixMs: 9_999,
        confirmationMode: .interactive,
        runtimeOffers: [
            AppRemoraLinkRuntimeOffer(runtimeId: "codex", displayName: "Codex", available: true, recommended: true),
            AppRemoraLinkRuntimeOffer(runtimeId: "offline", displayName: "Offline", available: false, recommended: false),
        ],
        maximumScopes: [.inspectRuntimes, .connectRuntime, .restartRuntime],
        requiredScopes: [.inspectRuntimes],
        defaultScopes: [.connectRuntime],
        defaultRuntimeIds: ["codex", "offline"]
    )
}

@MainActor
private final class AsyncGate {
    private var continuation: CheckedContinuation<Void, Never>?
    private var isOpen = false

    func wait() async {
        if isOpen { return }
        await withCheckedContinuation { continuation = $0 }
    }

    func open() {
        isOpen = true
        continuation?.resume()
        continuation = nil
    }
}
