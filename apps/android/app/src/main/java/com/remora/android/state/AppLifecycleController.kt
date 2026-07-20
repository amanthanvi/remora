package com.remora.android.state

import android.content.Context
import com.remora.android.util.LLog
import uniffi.codex_mobile_client.ThreadKey

/**
 * Handles app lifecycle events: server reconnection on resume,
 * background turn tracking on pause, and authoritative refresh on resume.
 *
 * Reconnect orchestration is delegated to the shared Rust [ReconnectController].
 */
class AppLifecycleController {

    /** Threads that were active when the app went to background. */
    private val backgroundedTurnKeys = mutableSetOf<ThreadKey>()

    /** Wall-clock timestamp (epoch ms) of the most recent [onPause]. */
    private var lastBackgroundedAt: Long? = null

    /**
     * Reconnects all saved servers on app launch or resume.
     */
    suspend fun reconnectSavedServers(context: Context, appModel: AppModel) {
        val servers = SavedServerStore.remembered(context).map { it.toRecord() }
        appModel.reconnectController.syncSavedServers(servers)
        appModel.reconnectController.notifyNetworkChange()
        val results = appModel.reconnectController.reconnectSavedServers()
        restoreLocalStateAfterReconnect(appModel, results)
        val retryResults = appModel.reconnectController.reconnectSavedServers()
        restoreLocalStateAfterReconnect(appModel, retryResults)
        appModel.refreshSnapshot()
    }

    /**
     * Reconnects a single server by ID.
     */
    suspend fun reconnectServer(context: Context, appModel: AppModel, serverId: String) {
        val servers = SavedServerStore.load(context).map { it.toRecord() }
        appModel.reconnectController.syncSavedServers(servers)
        val result = appModel.reconnectController.reconnectServer(serverId)
        restoreLocalStateAfterReconnect(appModel, listOf(result))
        appModel.refreshSnapshot()
    }

    /**
     * Called when the app enters the foreground.
     */
    suspend fun onResume(context: Context, appModel: AppModel) {
        val keysToRefresh = buildSet {
            addAll(backgroundedTurnKeys)
            appModel.snapshot.value?.activeThread?.let(::add)
        }
        val servers = SavedServerStore.remembered(context).map { it.toRecord() }
        appModel.reconnectController.syncSavedServers(servers)

        // Close stale network paths before reconnecting after a long suspension.
        val backgroundedAt = lastBackgroundedAt
        lastBackgroundedAt = null
        if (backgroundedAt != null) {
            val durationMs = System.currentTimeMillis() - backgroundedAt
            if (durationMs > LONG_RESUME_THRESHOLD_MS) {
                LLog.i(
                    "AppLifecycleController",
                    "long resume — abandoning stale connections backgroundDurationSec=${durationMs / 1000}",
                )
                try {
                    appModel.withRemoraLinkV2 { it.remoraLinkLongResume() }
                } catch (error: Exception) {
                    LLog.w(
                        "AppLifecycleController",
                        "Remora Link long-resume reconciliation failed: ${error.message}",
                    )
                }
            }
        }

        val results = appModel.reconnectController.onAppBecameActive()
        restoreLocalStateAfterReconnect(appModel, results)
        val retryResults = appModel.reconnectController.reconnectSavedServers()
        restoreLocalStateAfterReconnect(appModel, retryResults)
        backgroundedTurnKeys.clear()
        keysToRefresh.forEach { key ->
            // Force-authoritative: a turn that completed during a long
            // suspension fired `TurnCompleted` while no client connection
            // was attached, so the local snapshot still shows the turn
            // as in-progress. Pull back `excludeTurns = false` so
            // `reconcile_active_turn` can clear the stale
            // `active_turn_id` — otherwise the user sees a "thinking"
            // spinner whose `turn/interrupt` attempts get rejected with
            // "no active turn to interrupt".
            try {
                appModel.forceRefreshThreadAuthoritative(key)
            } catch (error: Exception) {
                LLog.w(
                    "AppLifecycleController",
                    "force-authoritative refresh failed; falling back to refreshThreadSnapshot: ${error.message}",
                )
                appModel.refreshThreadSnapshot(key)
            }
        }
    }

    /**
     * Called when the app goes to background.
     * Tracks active turns so they can be refreshed authoritatively on resume.
     */
    fun onPause(appModel: AppModel) {
        appModel.reconnectController.onAppEnteredBackground()
        lastBackgroundedAt = System.currentTimeMillis()
        backgroundedTurnKeys.clear()
        val snap = appModel.snapshot.value ?: return
        for (thread in snap.threads) {
            if (thread.activeTurnId != null) {
                backgroundedTurnKeys.add(thread.key)
            }
        }
    }

    private companion object {
        /**
         * Threshold for triggering proactive `Connection::close()` on
         * resume. Tied to iroh's per-path idle timeout (default 15s):
         * if we were suspended longer, the existing path is almost
         * certainly dead and waiting on iroh's connection-level idle
         * timer would make the next user request hang for the
         * remainder of that window.
         */
        const val LONG_RESUME_THRESHOLD_MS = 15_000L
    }

    private suspend fun restoreLocalStateAfterReconnect(
        appModel: AppModel,
        results: List<uniffi.codex_mobile_client.ReconnectResult>,
    ) {
        for (result in results) {
            if (!result.needsLocalAuthRestore) {
                continue
            }
            appModel.restoreStoredLocalAuthState(result.serverId)
            runCatching {
                appModel.refreshSessions(listOf(result.serverId))
            }
        }
    }
}
