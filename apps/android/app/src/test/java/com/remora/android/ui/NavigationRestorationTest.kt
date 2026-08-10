package com.remora.android.ui

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import uniffi.codex_mobile_client.ThreadKey

class NavigationRestorationTest {
    private val key = ThreadKey(
        serverId = "host:west/one",
        threadId = "thread with spaces?and=delimiters",
    )

    @Test
    fun everyRouteRoundTripsThroughRestorationToken() {
        val routes = listOf(
            Route.Home,
            Route.Sessions("server:one", "My sessions / active"),
            Route.Conversation(key),
            Route.RealtimeVoice(key),
            Route.ConversationInfo(key),
            Route.WallpaperSelection(key),
            Route.WallpaperAdjust(key),
            Route.ServerInfo("server:one"),
            Route.ServerWallpaperSelection("server:one"),
            Route.ServerWallpaperAdjust("server:one"),
            Route.Terminal(preferredRemoraLinkHostId = "remora-link:host:one/two"),
            Route.Terminal(null),
        )

        routes.forEach { route ->
            assertEquals(route, routeFromRestorationToken(route.restorationToken()))
        }
    }

    @Test
    fun restorationDropsMalformedEntriesAndKeepsHomeAnchor() {
        val restored = restoreRouteStack(
            listOf(
                "not-a-route",
                Route.Conversation(key).restorationToken(),
                "conversation:broken",
            ),
        )

        assertEquals(listOf(Route.Home, Route.Conversation(key)), restored)
    }

    @Test
    fun legacyTerminalRestorationTokensAreRejected() {
        assertEquals(null, routeFromRestorationToken("terminal:bm9kZS1pZA"))
        assertEquals(
            Route.Terminal("remora-link:host-one"),
            routeFromRestorationToken(Route.Terminal("remora-link:host-one").restorationToken()),
        )
    }

    @Test
    fun selectingPeerConversationReplacesRatherThanGrowsStack() {
        val first = conversationStack(key)
        val replacementKey = ThreadKey("host-two", "thread-two")
        val second = conversationStack(replacementKey)

        assertEquals(2, first.size)
        assertEquals(listOf(Route.Home, Route.Conversation(replacementKey)), second)
        assertFalse(second.contains(Route.Conversation(key)))
    }

    @Test
    fun onlyRestoredNavigationSuppressesInitialActiveThreadHydration() {
        val detailRoute = Route.ServerInfo("server-one")

        assertTrue(
            shouldPreserveRestoredRoute(
                navigationWasRestored = true,
                isInitialActiveThreadObservation = true,
                currentRoute = detailRoute,
            ),
        )
        assertFalse(
            shouldPreserveRestoredRoute(
                navigationWasRestored = false,
                isInitialActiveThreadObservation = true,
                currentRoute = detailRoute,
            ),
        )
        assertFalse(
            shouldPreserveRestoredRoute(
                navigationWasRestored = true,
                isInitialActiveThreadObservation = false,
                currentRoute = detailRoute,
            ),
        )
    }
}
