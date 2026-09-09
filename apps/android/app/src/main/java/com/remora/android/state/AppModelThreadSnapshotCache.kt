package com.remora.android.state

import uniffi.codex_mobile_client.AppThreadSnapshot
import uniffi.codex_mobile_client.ThreadKey

/** Accessed only under AppModel's projection guard. */
internal class AppModelThreadSnapshotCache {
    private val snapshots = mutableMapOf<ThreadKey, AppThreadSnapshot>()

    operator fun get(key: ThreadKey): AppThreadSnapshot? = snapshots[key]

    operator fun set(key: ThreadKey, thread: AppThreadSnapshot) {
        snapshots[key] = thread
    }

    fun remove(key: ThreadKey) { snapshots.remove(key) }

    fun prune(liveKeys: Set<ThreadKey>): Set<ThreadKey> {
        val removed = snapshots.keys - liveKeys
        snapshots.keys.retainAll(liveKeys)
        return removed
    }

    fun values(): Collection<AppThreadSnapshot> = snapshots.values

    fun preserveHydration(thread: AppThreadSnapshot): AppThreadSnapshot {
        if (thread.initialTurnsLoaded || thread.hydratedConversationItems.isNotEmpty()) return thread
        val cached = snapshots[thread.key] ?: return thread
        if (cached.hydratedConversationItems.isEmpty()) return thread
        return thread.copy(hydratedConversationItems = cached.hydratedConversationItems)
    }
}
