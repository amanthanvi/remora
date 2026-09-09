import Foundation

@MainActor
protocol NativeBackgroundRelayRuntime: AnyObject {
    func configure() async throws
    func observe(observation: AppRelayPushTokenObservation, token: AppRelaySecretValue) async throws -> AppRelayFanoutReceipt
    func tombstone(tombstone: AppRelayPushTokenTombstone) async throws -> AppRelayFanoutReceipt
    func ingest(hint: AppRelayWakeHint) async throws -> AppRelayReconcileReceipt
    func reconcile() async throws -> [AppRelayReconcileOutcome]
    func status() async throws -> AppRelayStatusSnapshot
}

@MainActor
final class RustBackgroundRelayRuntime: NativeBackgroundRelayRuntime {
    private let client: AppClient
    private let preparePairedHosts: @MainActor () async throws -> Void

    init(client: AppClient, preparePairedHosts: @escaping @MainActor () async throws -> Void) {
        self.client = client
        self.preparePairedHosts = preparePairedHosts
    }

    func configure() async throws {
        try await preparePairedHosts()
        guard !Task.isCancelled else { throw BackgroundRelayError.Cancelled }
        try await client.configureBackgroundRelay(
            journal: NativeRelayJournalBackend.shared,
            secrets: NativeRelaySecretBackend.shared,
            allowLoopbackHttp: false
        )
    }

    func observe(observation: AppRelayPushTokenObservation, token: AppRelaySecretValue) async throws -> AppRelayFanoutReceipt {
        try await client.backgroundRelayObservePushToken(observation: observation, token: token)
    }

    func tombstone(tombstone: AppRelayPushTokenTombstone) async throws -> AppRelayFanoutReceipt {
        try await client.backgroundRelayTombstonePushToken(tombstone: tombstone)
    }

    func ingest(hint: AppRelayWakeHint) async throws -> AppRelayReconcileReceipt {
        try await client.backgroundRelayIngestWake(hint: hint)
    }

    func reconcile() async throws -> [AppRelayReconcileOutcome] {
        try await client.backgroundRelayReconcile()
    }

    func status() async throws -> AppRelayStatusSnapshot {
        try await client.backgroundRelayStatus()
    }
}

/// Custody for the latest OS input, including inputs received before pairing.
/// The secure CAS revision sequences native callbacks, not relay registrations.
/// No installation, cursor, relay receipt, or token digest is stored here.
@MainActor
final class NativeRelayProviderTokenCustody {
    private let secrets: any AppRelaySecretBackend
    private let environment: AppRelayPushEnvironment
    private let alias: String

    init(
        secrets: any AppRelaySecretBackend = NativeRelaySecretBackend.shared,
        environment: AppRelayPushEnvironment = APNsEnvironment.current.relayEnvironment
    ) {
        self.secrets = secrets
        self.environment = environment
        alias = environment == .sandbox ? "remora_apns_input_sandbox_v1" : "remora_apns_input_production_v1"
    }

    func observe(token: Data) async throws {
        guard (1...1_024).contains(token.count) else { throw BackgroundRelayError.InvalidProviderRegistration }
        try await replace(token: token)
    }

    func tombstone() async throws {
        try await replace(token: nil)
    }

    func synchronize(runtime: any NativeBackgroundRelayRuntime) async throws -> AppRelayFanoutReceipt? {
        for _ in 0..<8 {
            guard let revision = try await currentRevision() else { return nil }
            let token: AppRelaySecretValue?
            do { token = try await secrets.read(alias: alias) }
            catch AppRelaySecretReadError.Missing { token = nil }
            catch { throw BackgroundRelayError.SecureStorageUnavailable }
            defer { token?.zeroize() }
            guard try await currentRevision() == revision else { continue }
            if let token {
                return try await runtime.observe(
                    observation: AppRelayPushTokenObservation(provider: .apns, environment: environment, localGeneration: revision),
                    token: token
                )
            }
            return try await runtime.tombstone(tombstone: AppRelayPushTokenTombstone(
                provider: .apns, environment: environment, throughLocalGeneration: revision
            ))
        }
        throw BackgroundRelayError.SecureStorageUnavailable
    }

    private func replace(token: Data?) async throws {
        for _ in 0..<8 {
            let previous = try await currentRevision()
            if let token, previous != nil {
                do {
                    let existing = try await secrets.read(alias: alias)
                    defer { existing.zeroize() }
                    let matches = existing.withUnsafeBytes { $0.elementsEqual(token) }
                    if matches, try await currentRevision() == previous { return }
                } catch AppRelaySecretReadError.Missing {
                    // A newer observation may replace an explicit tombstone.
                } catch { throw BackgroundRelayError.SecureStorageUnavailable }
            }
            let (revision, overflow) = (previous ?? 0).addingReportingOverflow(1)
            guard !overflow else { throw BackgroundRelayError.SecureStorageUnavailable }
            let outcome: AppRelaySecretCasOutcome
            if let token {
                let secret = AppRelaySecretValue(copying: token)
                defer { secret.zeroize() }
                outcome = await secrets.compareAndSwap(
                    alias: alias, expectedRevision: previous, replacementRevision: revision, value: secret
                )
            } else {
                outcome = await secrets.compareAndTombstone(
                    alias: alias, expectedRevision: previous, replacementRevision: revision
                )
            }
            switch outcome {
            case .stored: return
            case .conflict: continue
            case .unavailable: throw BackgroundRelayError.SecureStorageUnavailable
            }
        }
        throw BackgroundRelayError.SecureStorageUnavailable
    }

    private func currentRevision() async throws -> UInt64? {
        switch await secrets.revision(alias: alias) {
        case .missing: return nil
        case .found(let revision): return revision
        case .unavailable: throw BackgroundRelayError.SecureStorageUnavailable
        }
    }
}
