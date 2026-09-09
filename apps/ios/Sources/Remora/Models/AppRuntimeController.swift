import Foundation
import Observation

@MainActor
@Observable
final class AppRuntimeController {
    static let shared = AppRuntimeController()

    @ObservationIgnored private weak var appModel: AppModel?
    @ObservationIgnored private weak var voiceRuntime: VoiceRuntimeController?
    @ObservationIgnored private let lifecycle = AppLifecycleController()
    @ObservationIgnored private let reachability: any RemoraLinkReachabilityObserving
    @ObservationIgnored private let remoraLinkAdapters: RemoraLinkNativeAdapters
    @ObservationIgnored private let remoraLinkConfigurator: RemoraLinkConfigurator
    @ObservationIgnored private weak var remoraLinkClient: AppClient?
    @ObservationIgnored private weak var remoraLinkConfigurationClient: AppClient?
    @ObservationIgnored private weak var remoraLinkConfiguredClient: AppClient?
    @ObservationIgnored private var remoraLinkConfigurationTask: Task<Void, Never>?
    @ObservationIgnored private var remoraLinkRetryRequested = false
    @ObservationIgnored private var hasStartedReachability = false
    @ObservationIgnored private weak var relayClient: AppClient?

    private(set) var remoraLinkStatus: RemoraLinkNativeConfigurationStatus = .notConfigured

    init(
        reachability: (any RemoraLinkReachabilityObserving)? = nil,
        remoraLinkAdapters: RemoraLinkNativeAdapters = .shared,
        remoraLinkConfigurator: @escaping RemoraLinkConfigurator = { client, adapters in
            try await client.configureRemoraLink(
                journal: adapters.journal,
                transportIdentity: adapters.transportIdentity,
                deviceKeys: adapters.deviceKeys
            )
        }
    ) {
        self.reachability = reachability ?? NetworkReachabilityObserver()
        self.remoraLinkAdapters = remoraLinkAdapters
        self.remoraLinkConfigurator = remoraLinkConfigurator
    }

    func bind(appModel: AppModel, voiceRuntime: VoiceRuntimeController) {
        self.appModel = appModel
        self.voiceRuntime = voiceRuntime
        bindBackgroundRelay(client: appModel.client)
        reachability.bind(appModel: appModel)
        startReachabilityIfNeeded()
        configureRemoraLinkIfNeeded(client: appModel.client)
    }

    /// Background APNs launch must not depend on a SwiftUI scene appearing.
    func prepareBackgroundRuntimeIfSecurityReady() async {
        guard CurrentKeychainNamespaceCleanup.shared.isComplete else { return }
        await Task.detached(priority: .userInitiated) {
            AppModel.prewarmRustBridges()
        }.value
        guard !Task.isCancelled else { return }
        startBackgroundRuntimeIfSecurityReady()
    }

    func startBackgroundRuntimeIfSecurityReady() {
        guard CurrentKeychainNamespaceCleanup.shared.isComplete else { return }
        let model = appModel ?? AppModel.shared
        appModel = model
        model.start()
        reachability.bind(appModel: model)
        startReachabilityIfNeeded()
        bindBackgroundRelay(client: model.client)
        configureRemoraLinkIfNeeded(client: model.client)
    }

    private func bindBackgroundRelay(client: AppClient) {
        guard relayClient !== client else { return }
        relayClient = client
        BackgroundAwarenessController.shared.bind(runtime: RustBackgroundRelayRuntime(
            client: client,
            preparePairedHosts: { [weak self, client] in
                guard let self, CurrentKeychainNamespaceCleanup.shared.isComplete else {
                    throw BackgroundRelayError.SecureStorageUnavailable
                }
                self.configureRemoraLinkIfNeeded(client: client)
                await self.remoraLinkConfigurationTask?.value
                guard self.remoraLinkStatus == .available,
                      self.remoraLinkConfiguredClient === client else {
                    throw BackgroundRelayError.SecureStorageUnavailable
                }
            }
        ))
    }

    /// v2 custody is installed after the app's Rust client has been bound, not
    /// during bridge prewarm. Failure is isolated to Remora Link and remains
    /// retryable on a later bind; the rest of the app continues normally.
    func startReachabilityIfNeeded() {
        guard !hasStartedReachability else { return }
        hasStartedReachability = true
        reachability.start()
    }

    func configureRemoraLinkIfNeeded(client: AppClient) {
        remoraLinkClient = client
        guard CurrentKeychainNamespaceCleanup.shared.isComplete else {
            remoraLinkStatus = .unavailable
            return
        }
        if remoraLinkStatus == .configuring {
            remoraLinkRetryRequested = true
            return
        }
        if remoraLinkStatus == .available, remoraLinkConfiguredClient === client {
            return
        }

        remoraLinkStatus = .configuring
        remoraLinkConfigurationClient = client
        let adapters = remoraLinkAdapters
        let configurator = remoraLinkConfigurator
        remoraLinkConfigurationTask = Task { [weak self, client, adapters] in
            do {
                try await configurator(client, adapters)
                guard !Task.isCancelled else {
                    self?.finishRemoraLinkConfiguration(succeeded: false, client: client)
                    return
                }
                self?.finishRemoraLinkConfiguration(succeeded: true, client: client)
                LLog.info("remora-link", "v2 native custody configured")
            } catch {
                self?.finishRemoraLinkConfiguration(succeeded: false, client: client)
                LLog.error("remora-link", "v2 native custody unavailable", error: error)
            }
        }
    }

    private func finishRemoraLinkConfiguration(succeeded: Bool, client: AppClient) {
        guard remoraLinkConfigurationClient === client else { return }
        remoraLinkConfigurationTask = nil
        remoraLinkConfigurationClient = nil
        let latestClient = remoraLinkClient
        let clientChanged = latestClient.map { $0 !== client } ?? false
        let shouldRetryAfterFailure = remoraLinkRetryRequested
        remoraLinkRetryRequested = false

        if succeeded {
            remoraLinkConfiguredClient = client
            if clientChanged, let latestClient {
                remoraLinkStatus = .unavailable
                configureRemoraLinkIfNeeded(client: latestClient)
            } else {
                remoraLinkStatus = .available
            }
            return
        }

        remoraLinkStatus = .unavailable
        if (clientChanged || shouldRetryAfterFailure), let latestClient {
            configureRemoraLinkIfNeeded(client: latestClient)
        }
    }

    func retryRemoraLinkOnForegroundIfNeeded() {
        switch remoraLinkStatus {
        case .configuring:
            remoraLinkRetryRequested = true
        case .unavailable:
            if let remoraLinkClient {
                configureRemoraLinkIfNeeded(client: remoraLinkClient)
            }
        case .notConfigured, .available:
            break
        }
    }

    func reconnectSavedServers() async {
        guard let appModel else { return }
        await lifecycle.reconnectSavedServers(appModel: appModel)
    }

    func reconnectServer(serverId: String) async {
        guard let appModel else { return }
        await lifecycle.reconnectServer(serverId: serverId, appModel: appModel)
    }

    func appDidEnterBackground() {
        guard let appModel else { return }
        appModel.reconnectController.onAppEnteredBackground()
        lifecycle.appDidEnterBackground(
            snapshot: appModel.snapshot,
            hasActiveVoiceSession: voiceRuntime?.activeVoiceSession != nil
        )
    }

    func appDidBecomeInactive() {
        guard let appModel else { return }
        appModel.reconnectController.onAppBecameInactive()
    }

    func appDidBecomeActive() {
        guard let appModel else { return }
        retryRemoraLinkOnForegroundIfNeeded()
        // Foreground activation is authoritative repair even when voice kept a
        // transport alive while the app was backgrounded.
        appModel.reconnectController.noteAppBecameActive()
        lifecycle.appDidBecomeActive(
            appModel: appModel,
            hasActiveVoiceSession: voiceRuntime?.activeVoiceSession != nil
        )
        // Rust repairs paired relay bindings even if APNs delivered no hint.
        // Direct-server foreground repair above remains independent.
        BackgroundAwarenessController.shared.applicationDidBecomeActive()
    }
}
