package com.remora.android.state

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import uniffi.codex_mobile_client.AppModeKind
import uniffi.codex_mobile_client.AppThreadSnapshot
import uniffi.codex_mobile_client.HydratedAssistantMessageData
import uniffi.codex_mobile_client.HydratedConversationItem
import uniffi.codex_mobile_client.HydratedConversationItemContent
import uniffi.codex_mobile_client.ThreadInfo
import uniffi.codex_mobile_client.ThreadKey
import uniffi.codex_mobile_client.ThreadSummaryStatus

class AppModelThreadSnapshotCacheTest {
    @Test
    fun loadedEmptyRollbackHistoryReplacesCachedItems() {
        val cache = AppModelThreadSnapshotCache()
        val cached = thread()
        cache[cached.key] = cached
        val empty = cached.copy(hydratedConversationItems = emptyList(), initialTurnsLoaded = true)
        val merged = cache.preserveHydration(empty)
        assertTrue(merged.hydratedConversationItems.isEmpty())
        cache[empty.key] = merged
        assertTrue(cache[empty.key]!!.hydratedConversationItems.isEmpty())
    }

    @Test
    fun metadataOnlySnapshotCanReuseExistingHydration() {
        val cache = AppModelThreadSnapshotCache()
        val cached = thread()
        cache[cached.key] = cached
        val metadata = cached.copy(hydratedConversationItems = emptyList(), initialTurnsLoaded = false)
        assertEquals(cached.hydratedConversationItems, cache.preserveHydration(metadata).hydratedConversationItems)
    }

    @Test
    fun authoritativeAbsenceEvictsCacheEvenWhenRemovalEventWasLost() {
        val cache = AppModelThreadSnapshotCache()
        val cached = thread()
        val retained = cached.copy(key = ThreadKey("server", "retained"))
        cache[cached.key] = cached
        cache[retained.key] = retained
        assertEquals(setOf(cached.key), cache.prune(setOf(retained.key)))
        assertNull(cache[cached.key])
        assertEquals(listOf(retained), cache.values().toList())
    }

    private fun thread(): AppThreadSnapshot {
        val key = ThreadKey("server", "thread")
        return AppThreadSnapshot(
            key = key,
            info = ThreadInfo(
                id = key.threadId, title = "Thread", model = null, status = ThreadSummaryStatus.IDLE,
                preview = null, cwd = null, path = null, modelProvider = null, agentNickname = null,
                agentRole = null, parentThreadId = null, forkedFromId = null, agentStatus = null,
                createdAt = null, updatedAt = null,
            ),
            agentRuntimeKind = "codex",
            collaborationMode = AppModeKind.DEFAULT,
            model = null,
            reasoningEffort = null,
            effectiveApprovalPolicy = null,
            effectiveSandboxPolicy = null,
            hydratedConversationItems = listOf(
                HydratedConversationItem(
                    id = "assistant",
                    content = HydratedConversationItemContent.Assistant(
                        HydratedAssistantMessageData("old text", null, null, null),
                    ),
                    sourceTurnId = null, sourceTurnIndex = null, timestamp = null,
                    isFromUserTurnBoundary = false,
                ),
            ),
            queuedFollowUps = emptyList(), activeTurnId = null, activePlanProgress = null,
            pendingPlanImplementationPrompt = null, contextTokensUsed = null, modelContextWindow = null,
            rateLimits = null, realtimeSessionId = null, goal = null, stats = null, tokenUsage = null,
            olderTurnsCursor = null, initialTurnsLoaded = true,
        )
    }
}
