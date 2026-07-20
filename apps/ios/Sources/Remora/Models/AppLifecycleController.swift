import Foundation
import os

private let appLifecycleSignpostLog = OSLog(
    subsystem: Bundle.main.bundleIdentifier ?? "com.remora.app",
    category: "lifecycle"
)

@MainActor
final class AppLifecycleController {
    private static let maximumBackgroundThreadRefreshes = 4
    private var backgroundedTurnKeys: Set<ThreadKey> = []
    private var hasRecoveredCurrentForegroundSession = false
    private var hasEnteredBackgroundSinceLaunch = false
    private var foregroundRecoveryTask: Task<Void, Never>?
    private var foregroundRecoveryID: UUID?
    private var lastBackgroundedAt: Date?

    private static let longResumeThreshold: TimeInterval = 15

    func reconnectSavedServers(appModel: AppModel) async {
        let servers = SavedServerStore.reconnectRecords(rememberedOnly: true)
        appModel.reconnectController.syncSavedServers(servers: servers)
        await appModel.reconnectController.notifyNetworkChange()
        _ = await appModel.reconnectController.reconnectSavedServers()
        await appModel.refreshSnapshot()
    }

    func reconnectServer(serverId: String, appModel: AppModel) async {
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
        hasActiveVoiceSession _: Bool
    ) {
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

    /// Bounded by `BackgroundAwarenessController` to 27 seconds. The cursor is
    /// only a wake high-water hint; success is determined by authenticated
    /// reconnect/snapshot work, never by APNs delivery.
    func reconcileBackgroundAwareness(
        appModel: AppModel,
        expectedCursor _: UInt64
    ) async -> AuthenticatedBackgroundStateResult {
        let previousSnapshot = appModel.snapshot

        appModel.reconnectController.syncSavedServers(
            servers: SavedServerStore.reconnectRecords(rememberedOnly: true)
        )
        await appModel.reconnectController.notifyNetworkChange()
        let reconnectResults = await appModel.reconnectController.reconnectSavedServers()
        guard reconnectResults.allSatisfy(\.success) else { return .failed }
        guard await appModel.refreshSnapshotAuthoritative() else { return .failed }
        guard !Task.isCancelled else { return .failed }

        let activeKey = appModel.snapshot?.activeThread
        let trackedKeys = appModel.snapshot?.threadsWithTrackedTurns.map(\.key) ?? []
        var refreshKeys: [ThreadKey] = []
        if let activeKey {
            refreshKeys.append(activeKey)
        }
        for key in trackedKeys where !refreshKeys.contains(key) {
            refreshKeys.append(key)
        }

        for key in refreshKeys.prefix(Self.maximumBackgroundThreadRefreshes) {
            guard !Task.isCancelled else { return .failed }
            do {
                try await appModel.forceRefreshThreadAuthoritative(key: key)
            } catch {
                LLog.error(
                    "background-awareness",
                    "authoritative wake reconciliation failed"
                )
                return .failed
            }
        }

        guard await appModel.refreshSnapshotAuthoritative() else { return .failed }
        return previousSnapshot != appModel.snapshot ? .changed : .unchanged
    }

    private func performForegroundRecovery(
        appModel: AppModel,
        needsInitialReconnect: Bool,
        keysToRefresh: Set<ThreadKey>
    ) async {
        appModel.reconnectController.syncSavedServers(
            servers: SavedServerStore.reconnectRecords(rememberedOnly: true)
        )

        let backgroundDuration = lastBackgroundedAt.map { Date().timeIntervalSince($0) }
        if let duration = backgroundDuration, duration > Self.longResumeThreshold {
            do {
                _ = try await appModel.client.remoraLinkLongResume()
            } catch {
                LLog.error("remora-link", "long-resume reconciliation failed", error: error)
            }
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

    }
}
