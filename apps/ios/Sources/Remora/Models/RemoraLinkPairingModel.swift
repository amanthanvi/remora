import Foundation
import Observation
#if canImport(UIKit)
import UIKit
#endif

struct RemoraLinkPairingOperations {
    var inspect: @MainActor (AppRemoraLinkPairingCode) async throws -> AppRemoraLinkInspection
    var accept: @MainActor (AppRemoraLinkAcceptance) async throws -> AppRemoraLinkPairingOutcome
    var awaitPairing: @MainActor (String, AppRemoraLinkPairingCode?) async throws -> AppRemoraLinkPairingOutcome
    var cancel: @MainActor (String) async throws -> AppRemoraLinkPairingCancellationOutcome

    @MainActor
    init(client: AppClient) {
        inspect = { try await client.inspectRemoraLinkCode(code: $0) }
        accept = { try await client.acceptRemoraLinkOffer(acceptance: $0) }
        awaitPairing = { try await client.awaitRemoraLinkPairing(hostId: $0, code: $1) }
        cancel = { try await client.cancelRemoraLinkPairing(hostId: $0) }
    }

    init(
        inspect: @escaping @MainActor (AppRemoraLinkPairingCode) async throws -> AppRemoraLinkInspection,
        accept: @escaping @MainActor (AppRemoraLinkAcceptance) async throws -> AppRemoraLinkPairingOutcome,
        awaitPairing: @escaping @MainActor (String, AppRemoraLinkPairingCode?) async throws -> AppRemoraLinkPairingOutcome,
        cancel: @escaping @MainActor (String) async throws -> AppRemoraLinkPairingCancellationOutcome
    ) {
        self.inspect = inspect
        self.accept = accept
        self.awaitPairing = awaitPairing
        self.cancel = cancel
    }
}

enum RemoraLinkPairingAvailability: Equatable {
    case configuring
    case unavailable
}

struct RemoraLinkPendingPairing: Equatable {
    let hostId: String
    let sas: String
    let expiresAtUnixMs: UInt64
    let selectedRuntimeIds: [String]
    let requestedScopes: [AppRemoraLinkScope]
}

struct RemoraLinkPairingSuccess: Equatable {
    let hostId: String
    let sas: String?
    let selectedRuntimeIds: [String]
    let grantedScopes: [AppRemoraLinkScope]
    let wasAlreadyPaired: Bool
}

enum RemoraLinkPairingState: Equatable {
    case availability(RemoraLinkPairingAvailability)
    case ingress
    case inspecting
    case legacyRePair(hostId: String, hostDisplayName: String)
    case offer(AppRemoraLinkOffer)
    case accepting
    case awaiting(RemoraLinkPendingPairing)
    case cancelling
    case outcomeUnknown(String)
    case success(RemoraLinkPairingSuccess)
    case failure(String)
}

@MainActor
@Observable
final class RemoraLinkPairingModel {
    static let maximumCodeByteCount = AppRemoraLinkPairingCode.maximumByteCount
    static let maximumDeviceNameByteCount = 80

    private let operations: RemoraLinkPairingOperations
    private(set) var state: RemoraLinkPairingState
    private(set) var selectedRuntimeIds: Set<String> = []
    private(set) var selectedScopes: Set<AppRemoraLinkScope> = []
    var deviceDisplayName: String

    @ObservationIgnored private var operationTask: Task<Void, Never>?
    @ObservationIgnored private var operationGeneration = 0

    init(
        operations: RemoraLinkPairingOperations,
        initialState: RemoraLinkPairingState = .availability(.configuring),
        deviceDisplayName: String? = nil
    ) {
        self.operations = operations
        self.state = initialState
        self.deviceDisplayName = Self.truncatedUTF8(
            deviceDisplayName ?? Self.defaultDeviceDisplayName,
            maximumByteCount: Self.maximumDeviceNameByteCount
        )
    }

    convenience init(client: AppClient) {
        self.init(operations: RemoraLinkPairingOperations(client: client))
    }

    deinit {
        operationTask?.cancel()
    }

    func updateAvailability(_ status: RemoraLinkNativeConfigurationStatus) {
        guard case .availability = state else { return }
        switch status {
        case .available:
            state = .ingress
        case .notConfigured, .configuring:
            state = .availability(.configuring)
        case .unavailable:
            state = .availability(.unavailable)
        }
    }

    func inspect(codeText: String) {
        guard codeText.utf8.count <= Self.maximumCodeByteCount else {
            state = .failure("Pairing code is too large. Scan or paste a code up to 4,128 bytes.")
            return
        }
        guard !codeText.isEmpty else {
            state = .failure("No pairing code was found.")
            return
        }

        var bytes = Array(codeText.utf8)
        let carrier: AppRemoraLinkPairingCode
        do {
            carrier = try AppRemoraLinkPairingCode(copying: bytes)
        } catch {
            Self.wipe(&bytes)
            state = .failure("That Remora Link pairing code is invalid.")
            return
        }
        Self.wipe(&bytes)

        state = .inspecting
        beginOperation { [weak self, carrier] generation in
            defer { carrier.zeroize() }
            guard let self else { return }
            do {
                let inspection = try await self.operations.inspect(carrier)
                guard self.isCurrent(generation) else { return }
                self.apply(inspection)
            } catch {
                guard self.isCurrent(generation) else { return }
                self.apply(error)
            }
        }
    }

    func toggleRuntime(_ runtimeId: String) {
        guard case .offer(let offer) = state,
              let runtime = offer.runtimeOffers.first(where: { $0.runtimeId == runtimeId }),
              runtime.available else { return }
        if selectedRuntimeIds.contains(runtimeId) {
            selectedRuntimeIds.remove(runtimeId)
        } else {
            selectedRuntimeIds.insert(runtimeId)
        }
    }

    func toggleScope(_ scope: AppRemoraLinkScope) {
        guard case .offer(let offer) = state,
              offer.maximumScopes.contains(scope),
              !offer.requiredScopes.contains(scope) else { return }
        if selectedScopes.contains(scope) {
            selectedScopes.remove(scope)
        } else {
            selectedScopes.insert(scope)
        }
    }

    func acceptOffer() {
        guard case .offer(let offer) = state else { return }
        let trimmedName = deviceDisplayName.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmedName.isEmpty else {
            state = .failure("Enter a name for this device.")
            return
        }
        guard trimmedName.utf8.count <= Self.maximumDeviceNameByteCount else {
            state = .failure("Device name must be 80 bytes or fewer.")
            return
        }

        let runtimeIds = offer.runtimeOffers
            .filter { $0.available && selectedRuntimeIds.contains($0.runtimeId) }
            .map(\.runtimeId)
        guard !runtimeIds.isEmpty else {
            state = .failure("Choose at least one available runtime.")
            return
        }

        let scopes = offer.maximumScopes.filter { selectedScopes.contains($0) }
        guard Set(offer.requiredScopes).isSubset(of: Set(scopes)) else {
            state = .failure("The host's required permissions must remain enabled.")
            return
        }

        let acceptance = AppRemoraLinkAcceptance(
            offerId: offer.offerId,
            deviceDisplayName: trimmedName,
            selectedRuntimeIds: runtimeIds,
            requestedScopes: scopes
        )
        state = .accepting
        beginOperation { [weak self] generation in
            guard let self else { return }
            do {
                let outcome = try await self.operations.accept(acceptance)
                guard self.isCurrent(generation) else { return }
                await self.apply(outcome, generation: generation)
            } catch {
                guard self.isCurrent(generation) else { return }
                self.apply(error)
            }
        }
    }

    func resume(hostId: String, pendingApproval: AppRemoraLinkPendingApproval) {
        let pending = RemoraLinkPendingPairing(
            hostId: hostId,
            sas: pendingApproval.sas,
            expiresAtUnixMs: pendingApproval.expiresAtUnixMs,
            selectedRuntimeIds: [],
            requestedScopes: pendingApproval.requestedScopes
        )
        state = .awaiting(pending)
        beginAwait(hostId: hostId)
    }

    func cancelPairing() {
        guard case .awaiting(let pending) = state else { return }
        operationGeneration &+= 1
        operationTask?.cancel()
        state = .cancelling
        beginOperation { [weak self] generation in
            guard let self else { return }
            do {
                let outcome = try await self.operations.cancel(pending.hostId)
                guard self.isCurrent(generation) else { return }
                switch outcome {
                case .cancelled:
                    self.state = .ingress
                case .outcomeUnknown:
                    self.state = .outcomeUnknown(
                        "The cancellation result is unknown. Check Remora Link Hosts before trying again."
                    )
                }
            } catch {
                guard self.isCurrent(generation) else { return }
                self.apply(error)
            }
        }
    }

    func startOver() {
        operationGeneration &+= 1
        operationTask?.cancel()
        selectedRuntimeIds = []
        selectedScopes = []
        state = .ingress
    }

    var canAcceptOffer: Bool {
        guard case .offer(let offer) = state else { return false }
        let name = deviceDisplayName.trimmingCharacters(in: .whitespacesAndNewlines)
        let hasRuntime = offer.runtimeOffers.contains {
            $0.available && selectedRuntimeIds.contains($0.runtimeId)
        }
        return !name.isEmpty
            && name.utf8.count <= Self.maximumDeviceNameByteCount
            && hasRuntime
            && Set(offer.requiredScopes).isSubset(of: selectedScopes)
    }

    var deviceNameByteCount: Int { deviceDisplayName.utf8.count }

    static var defaultDeviceDisplayName: String {
        #if canImport(UIKit)
        return truncatedUTF8(UIDevice.current.name, maximumByteCount: maximumDeviceNameByteCount)
        #else
        return "Remora"
        #endif
    }

    static func truncatedUTF8(_ value: String, maximumByteCount: Int) -> String {
        guard value.utf8.count > maximumByteCount else { return value }
        var result = ""
        for character in value {
            let candidate = result + String(character)
            guard candidate.utf8.count <= maximumByteCount else { break }
            result = candidate
        }
        return result
    }

    private func apply(_ inspection: AppRemoraLinkInspection) {
        switch inspection {
        case .ready(let offer):
            selectedRuntimeIds = Set(
                offer.defaultRuntimeIds.filter { runtimeId in
                    offer.runtimeOffers.contains { $0.runtimeId == runtimeId && $0.available }
                }
            )
            selectedScopes = Set(offer.defaultScopes).union(offer.requiredScopes)
            selectedScopes.formIntersection(offer.maximumScopes)
            state = .offer(offer)
        case .legacyRePairRequired(let hostId, let hostDisplayName):
            selectedRuntimeIds = []
            selectedScopes = []
            state = .legacyRePair(hostId: hostId, hostDisplayName: hostDisplayName)
        }
    }

    private func apply(_ outcome: AppRemoraLinkPairingOutcome, generation: Int) async {
        switch outcome {
        case let .awaitingHostApproval(hostId, sas, expiresAtUnixMs, runtimeIds, scopes):
            state = .awaiting(
                RemoraLinkPendingPairing(
                    hostId: hostId,
                    sas: sas,
                    expiresAtUnixMs: expiresAtUnixMs,
                    selectedRuntimeIds: runtimeIds,
                    requestedScopes: scopes
                )
            )
            do {
                let finalOutcome = try await operations.awaitPairing(hostId, nil)
                guard isCurrent(generation) else { return }
                await apply(finalOutcome, generation: generation)
            } catch {
                guard isCurrent(generation) else { return }
                apply(error)
            }
        case let .paired(hostId, sas, runtimeIds, scopes, _):
            state = .success(
                RemoraLinkPairingSuccess(
                    hostId: hostId,
                    sas: sas,
                    selectedRuntimeIds: runtimeIds,
                    grantedScopes: scopes,
                    wasAlreadyPaired: false
                )
            )
        case let .alreadyPaired(hostId, runtimeIds, scopes):
            state = .success(
                RemoraLinkPairingSuccess(
                    hostId: hostId,
                    sas: nil,
                    selectedRuntimeIds: runtimeIds,
                    grantedScopes: scopes,
                    wasAlreadyPaired: true
                )
            )
        }
    }

    private func beginAwait(hostId: String) {
        beginOperation { [weak self] generation in
            guard let self else { return }
            do {
                let outcome = try await self.operations.awaitPairing(hostId, nil)
                guard self.isCurrent(generation) else { return }
                await self.apply(outcome, generation: generation)
            } catch {
                guard self.isCurrent(generation) else { return }
                self.apply(error)
            }
        }
    }

    private func beginOperation(
        _ body: @escaping @MainActor (Int) async -> Void
    ) {
        operationGeneration &+= 1
        let generation = operationGeneration
        operationTask?.cancel()
        operationTask = Task { await body(generation) }
    }

    private func isCurrent(_ generation: Int) -> Bool {
        generation == operationGeneration && !Task.isCancelled
    }

    private func apply(_ error: Error) {
        if let error = error as? RemoraLinkError {
            switch error {
            case .OutcomeUnknown:
                state = .outcomeUnknown(
                    "The host may have completed this operation. Check Remora Link Hosts before retrying."
                )
            case .Cancelled:
                state = .ingress
            case .NotConfigured:
                state = .availability(.unavailable)
            default:
                state = .failure(Self.message(for: error))
            }
        } else if !(error is CancellationError) {
            state = .failure(error.localizedDescription)
        }
    }

    private static func message(for error: RemoraLinkError) -> String {
        switch error {
        case .InvalidPairingCode: return "That Remora Link pairing code is invalid or expired."
        case .UnknownOffer: return "This offer is no longer available. Scan the pairing code again."
        case .PairingCodeRequired: return "Scan the original pairing code to continue this approval."
        case .JournalUnavailable, .JournalConflict, .JournalCorrupt:
            return "Remora Link secure state is unavailable. Try again after reopening the app."
        case .CredentialUnavailable, .MissingCredential, .InvalidSignature:
            return "This device's secure Remora Link credential is unavailable."
        case .HostUnavailable: return "The host is unavailable. Confirm the pairing command is still running."
        case .V2Unavailable: return "This host does not support Remora Link v2."
        case .IdentityDrift, .PolicyDrift, .ConfirmationMismatch, .InvitationMismatch:
            return "The host's pairing identity or policy changed. Generate a new pairing code."
        case .ProtocolViolation: return "The host returned an invalid pairing response."
        case .PairingUnavailable: return "Pairing is not available on this host right now."
        case .AuthorizationRequired: return "Approve this device on the host to continue."
        case .RuntimeUnavailable: return "One of the selected runtimes is no longer available."
        case .InvalidSelection: return "Review the selected runtimes and permissions, then try again."
        case .NotPaired: return "This device is not paired with that host."
        case .OperationInProgress: return "Another Remora Link operation is already in progress."
        case .NeedsRepair: return "This host pairing needs repair. Revoke or forget it in Settings, then pair again."
        case .NotConfigured: return "Remora Link is unavailable on this device."
        case .Cancelled: return "Pairing was cancelled."
        case .OutcomeUnknown: return "The operation result is unknown."
        }
    }

    private static func wipe(_ bytes: inout [UInt8]) {
        bytes.withUnsafeMutableBytes { rawBuffer in
            for index in rawBuffer.indices {
                rawBuffer[index] = 0
            }
        }
        bytes.removeAll(keepingCapacity: false)
    }
}
