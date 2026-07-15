import Foundation
import Observation

@MainActor
@Observable
final class AppRuntimeController {
    static let shared = AppRuntimeController()

    @ObservationIgnored private weak var appModel: AppModel?
    @ObservationIgnored private weak var voiceRuntime: VoiceRuntimeController?
    @ObservationIgnored private let lifecycle = AppLifecycleController()
    @ObservationIgnored private let reachability = NetworkReachabilityObserver()

    func bind(appModel: AppModel, voiceRuntime: VoiceRuntimeController) {
        self.appModel = appModel
        self.voiceRuntime = voiceRuntime
        BackgroundAwarenessController.shared.bind(reconciler: self)
        reachability.bind(appModel: appModel)
        reachability.start()
        loadAndPushAlleycatSecretKey(client: appModel.client)
    }

    /// Load the persisted iroh device secret key from the keychain (if
    /// any) and push it to the Rust client BEFORE any host-pairing
    /// operation triggers the endpoint bind. After the first bind, the
    /// Rust side may have generated a fresh key — observe via
    /// `persistAlleycatSecretKeyIfNeeded`. Together these maintain a
    /// stable `EndpointId` across cold launches.
    private func loadAndPushAlleycatSecretKey(client: AppClient) {
        do {
            if let bytes = try AlleycatCredentialStore.shared.loadDeviceSecretKey() {
                client.setAlleycatSecretKey(secretKeyBytes: bytes)
                LLog.info("pairing", "loaded persisted device secret key from keychain")
            }
        } catch {
            LLog.error("pairing", "failed to load device secret key", error: error)
        }
    }

    /// After a host-pairing operation has triggered the Rust endpoint
    /// bind, read back the actually-used bytes and persist them so the
    /// next cold launch reuses the same `EndpointId`. Idempotent — safe
    /// to call any time; if the bind hasn't happened yet, returns
    /// silently.
    func persistAlleycatSecretKeyIfNeeded() {
        guard let appModel else { return }
        guard let data = appModel.client.alleycatSecretKey() else { return }
        do {
            let existing = try AlleycatCredentialStore.shared.loadDeviceSecretKey()
            if existing == data { return }
            try AlleycatCredentialStore.shared.saveDeviceSecretKey(data)
            LLog.info("pairing", "persisted device secret key to keychain")
        } catch {
            LLog.error("pairing", "failed to persist device secret key", error: error)
        }
    }

    /// Best-effort graceful shutdown of the iroh endpoint. Wired from
    /// `applicationWillTerminate` (UIKit) — see comment on that hook
    /// in RemoraApp.swift for reliability caveats. iroh sends a clean
    /// CONNECTION_CLOSE to peers instead of logging "Aborting
    /// ungracefully" when the static MobileClient slot is finally
    /// dropped at process exit.
    func shutdownAlleycatEndpoint() async {
        guard let appModel else { return }
        await appModel.client.shutdownAlleycatEndpoint()
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
        // Foreground activation is authoritative repair even when voice kept a
        // transport alive while the app was backgrounded.
        appModel.reconnectController.noteAppBecameActive()
        lifecycle.appDidBecomeActive(
            appModel: appModel,
            hasActiveVoiceSession: voiceRuntime?.activeVoiceSession != nil
        )
        // If a background wake timed out or arrived before the Rust runtime was
        // bound, retry it now. AppLifecycleController above remains the
        // unconditional foreground repair path when APNs delivered nothing.
        BackgroundAwarenessController.shared.applicationDidBecomeActive()
    }
}

extension AppRuntimeController: BackgroundStateReconciling {
    func reconcileBackgroundState(expectedCursor: UInt64) async -> AuthenticatedBackgroundStateResult {
        guard let appModel else { return .failed }
        return await lifecycle.reconcileBackgroundAwareness(
            appModel: appModel,
            expectedCursor: expectedCursor
        )
    }
}
