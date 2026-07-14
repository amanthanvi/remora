import Foundation
import os

private let appLifecycleSignpostLog = OSLog(
    subsystem: Bundle.main.bundleIdentifier ?? "com.remora.app",
    category: "lifecycle"
)

@MainActor
final class AppLifecycleController {
    private var backgroundedTurnKeys: Set<ThreadKey> = []
    private var hasRecoveredCurrentForegroundSession = false
    private var hasEnteredBackgroundSinceLaunch = false
    private var foregroundRecoveryTask: Task<Void, Never>?
    private var foregroundRecoveryID: UUID?
    private var lastBackgroundedAt: Date?

    private static let longResumeThreshold: TimeInterval = 15

    func reconnectSavedServers(appModel: AppModel) async {
        let servers = SavedServerStore.reconnectRecords(rememberedOnly: true)
        appModel.reconnectController.setMultiClankerAndQuicEnabled(enabled: true)
        appModel.reconnectController.syncSavedServers(servers: servers)
        await appModel.reconnectController.notifyNetworkChange()
        _ = await appModel.reconnectController.reconnectSavedServers()
        await appModel.refreshSnapshot()
        AppRuntimeController.shared.persistAlleycatSecretKeyIfNeeded()
    }

    func reconnectServer(serverId: String, appModel: AppModel) async {
        appModel.reconnectController.setMultiClankerAndQuicEnabled(enabled: true)
        appModel.reconnectController.syncSavedServers(servers: SavedServerStore.reconnectRecords())
        _ = await appModel.reconnectController.reconnectServer(serverId: serverId)
        await appModel.refreshSnapshot()
    }

    func appDidEnterBackground(
        snapshot: AppSnapshotRecord?,
        hasActiveVoiceSession: Bool
    ) {
        let signpostID = OSSignpostID(log: appLifecycleSignpostLog)
        os_signpost(.begin, log: appLifecycleSignpostLog, name: "AppDidEnterBackground", signpostID: signpostID)
        defer { os_signpost(.end, log: appLifecycleSignpostLog, name: "AppDidEnterBackground", signpostID: signpostID) }

        hasEnteredBackgroundSinceLaunch = true
        hasRecoveredCurrentForegroundSession = false
        lastBackgroundedAt = Date()
        foregroundRecoveryTask?.cancel()
        foregroundRecoveryTask = nil
        foregroundRecoveryID = nil

        if hasActiveVoiceSession {
            backgroundedTurnKeys.removeAll()
        } else {
            backgroundedTurnKeys = Set(snapshot?.threadsWithTrackedTurns.map(\.key) ?? [])
        }
    }

    func appDidBecomeActive(
        appModel: AppModel,
        hasActiveVoiceSession: Bool
    ) {
        guard !hasActiveVoiceSession else { return }
        guard !hasRecoveredCurrentForegroundSession else { return }
        hasRecoveredCurrentForegroundSession = true

        let needsInitialReconnect = !hasEnteredBackgroundSinceLaunch
        let keysToRefresh = foregroundRecoveryKeys(
            snapshot: appModel.snapshot,
            backgroundedKeys: backgroundedTurnKeys
        )
        backgroundedTurnKeys.removeAll()

        foregroundRecoveryTask?.cancel()
        let recoveryID = UUID()
        foregroundRecoveryID = recoveryID
        foregroundRecoveryTask = Task { [weak self] in
            guard let self else { return }
            defer {
                if self.foregroundRecoveryID == recoveryID {
                    self.foregroundRecoveryTask = nil
                    self.foregroundRecoveryID = nil
                }
            }
            await self.performForegroundRecovery(
                appModel: appModel,
                needsInitialReconnect: needsInitialReconnect,
                keysToRefresh: keysToRefresh
            )
        }
    }

    func foregroundRecoveryKeys(
        snapshot: AppSnapshotRecord?,
        backgroundedKeys: Set<ThreadKey>
    ) -> Set<ThreadKey> {
        var keys = backgroundedKeys
        if let activeKey = snapshot?.activeThread {
            keys.insert(activeKey)
        }
        return keys
    }

    private func performForegroundRecovery(
        appModel: AppModel,
        needsInitialReconnect: Bool,
        keysToRefresh: Set<ThreadKey>
    ) async {
        appModel.reconnectController.setMultiClankerAndQuicEnabled(enabled: true)
        appModel.reconnectController.syncSavedServers(
            servers: SavedServerStore.reconnectRecords(rememberedOnly: true)
        )

        let backgroundDuration = lastBackgroundedAt.map { Date().timeIntervalSince($0) }
        if let duration = backgroundDuration, duration > Self.longResumeThreshold {
            await appModel.reconnectController.onLongResume()
        }
        lastBackgroundedAt = nil

        _ = await appModel.reconnectController.onAppBecameActive()
        await appModel.refreshSnapshot()
        if needsInitialReconnect {
            _ = await appModel.reconnectController.reconnectSavedServers()
            await appModel.refreshSnapshot()
        }
        guard !Task.isCancelled else { return }

        for key in keysToRefresh {
            do {
                try await appModel.forceRefreshThreadAuthoritative(key: key)
                await appModel.refreshThreadSnapshot(key: key)
            } catch {
                LLog.error(
                    "lifecycle",
                    "authoritative foreground refresh failed",
                    error: error,
                    fields: ["serverId": key.serverId, "threadId": key.threadId]
                )
            }
        }

        AppRuntimeController.shared.persistAlleycatSecretKeyIfNeeded()
    }
}
