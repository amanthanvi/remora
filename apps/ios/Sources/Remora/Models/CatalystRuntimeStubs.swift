#if targetEnvironment(macCatalyst)
import Foundation
import Observation
import SwiftUI

@MainActor
@Observable
final class AppRuntimeController {
    static let shared = AppRuntimeController()

    @ObservationIgnored private weak var appModel: AppModel?
    @ObservationIgnored private let reachability: any RemoraLinkReachabilityObserving
    @ObservationIgnored private let remoraLinkAdapters: RemoraLinkNativeAdapters
    @ObservationIgnored private let remoraLinkConfigurator: RemoraLinkConfigurator
    @ObservationIgnored private weak var remoraLinkClient: AppClient?
    @ObservationIgnored private weak var remoraLinkConfigurationClient: AppClient?
    @ObservationIgnored private weak var remoraLinkConfiguredClient: AppClient?
    @ObservationIgnored private var remoraLinkConfigurationTask: Task<Void, Never>?
    @ObservationIgnored private var remoraLinkRetryRequested = false
    @ObservationIgnored private var hasStartedReachability = false

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
        reachability.bind(appModel: appModel)
        startReachabilityIfNeeded()
        configureRemoraLinkIfNeeded(client: appModel.client)
    }

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
        let servers = SavedServerStore.reconnectRecords(rememberedOnly: true)
        appModel.reconnectController.syncSavedServers(servers: servers)
        await appModel.reconnectController.notifyNetworkChange()
        _ = await appModel.reconnectController.reconnectSavedServers()
        await appModel.refreshSnapshot()
    }

    func reconnectServer(serverId: String) async {
        guard let appModel else { return }
        let servers = SavedServerStore.reconnectRecords()
        appModel.reconnectController.syncSavedServers(servers: servers)
        _ = await appModel.reconnectController.reconnectServer(serverId: serverId)
        await appModel.refreshSnapshot()
    }
    func appDidEnterBackground() {
        lastBackgroundedAt = Date()
    }
    func appDidBecomeInactive() {}

    func appDidBecomeActive() {
        retryRemoraLinkOnForegroundIfNeeded()
        guard !hasRecoveredOnForeground else { return }
        hasRecoveredOnForeground = true
        let backgroundDuration = lastBackgroundedAt.map { Date().timeIntervalSince($0) }
        lastBackgroundedAt = nil
        Task { [weak self, backgroundDuration] in
            guard let self else { return }
            // Same long-resume short-circuit as iOS: if we were
            // suspended longer than iroh's per-path idle, kill the
            // existing paired-host connection so the worker rebuilds
            // before any user request lands.
            if let appModel = self.appModel,
               let duration = backgroundDuration,
               duration > Self.longResumeThreshold
            {
                do {
                    _ = try await appModel.client.remoraLinkLongResume()
                } catch {
                    LLog.error("remora-link", "long-resume reconciliation failed", error: error)
                }
            }
            await self.reconnectSavedServers()
        }
    }

    @ObservationIgnored private var hasRecoveredOnForeground = false
    @ObservationIgnored private var lastBackgroundedAt: Date?
    private static let longResumeThreshold: TimeInterval = 15
}

@MainActor
@Observable
final class VoiceRuntimeController {
    static let shared = VoiceRuntimeController()
    static let persistedVoiceServerIDKey = "remora.voice.pinned.server_id"
    static let persistedVoiceThreadIDKey = "remora.voice.pinned.thread_id"

    private(set) var activeVoiceSession: VoiceSessionState?
    var handoffModel: String?
    var handoffEffort: String?
    var handoffFastMode = false

    func bind(appModel: AppModel) {}
    @discardableResult
    func startPinnedVoiceCall(
        serverId: String,
        cwd: String,
        model: String?,
        approvalPolicy: AppAskForApproval?,
        sandboxMode: AppSandboxMode?
    ) async throws -> ThreadKey {
        throw NSError(
            domain: "Remora",
            code: 9999,
            userInfo: [NSLocalizedDescriptionKey: "Voice not available on Catalyst"]
        )
    }
    func stopActiveVoiceSession() async {}
    func toggleActiveVoiceSessionSpeaker() async throws {}
}

struct VoiceSessionState: Identifiable, Equatable {
    let id: String
    let threadKey: ThreadKey
}

@MainActor
@Observable
final class StableSafeAreaInsets {
    var bottomInset: CGFloat = 0
    func start(fallback: CGFloat) {
        bottomInset = fallback
    }
}

#endif
