package com.remora.android.ui

import androidx.compose.runtime.saveable.Saver
import java.nio.charset.StandardCharsets
import java.util.Base64
import uniffi.codex_mobile_client.ThreadKey

/**
 * Type-safe navigation routes for the app.
 */
sealed class Route {
    data object Home : Route()
    data class Sessions(val serverId: String, val title: String) : Route()
    data class Conversation(val key: ThreadKey) : Route()
    data class RealtimeVoice(val key: ThreadKey) : Route()
    data class ConversationInfo(val key: ThreadKey) : Route()
    data class WallpaperSelection(val key: ThreadKey) : Route()
    data class WallpaperAdjust(val key: ThreadKey) : Route()
    data class ServerInfo(val serverId: String) : Route()
    data class ServerWallpaperSelection(val serverId: String) : Route()
    data class ServerWallpaperAdjust(val serverId: String) : Route()
    data object Apps : Route()
    data class SavedApp(val appId: String) : Route()
    data class Terminal(val preferredRemoraLinkHostId: String? = null) : Route()
}

internal val Route.threadKeyOrNull: ThreadKey?
    get() = when (this) {
        is Route.Conversation -> key
        is Route.ConversationInfo -> key
        is Route.RealtimeVoice -> key
        is Route.WallpaperSelection -> key
        is Route.WallpaperAdjust -> key
        else -> null
    }

/**
 * Replaces the peer detail instead of growing the stack. The retained Home
 * anchor gives compact mode a deterministic back path after a window resize.
 */
internal fun conversationStack(key: ThreadKey): List<Route> =
    listOf(Route.Home, Route.Conversation(key))

internal fun shouldPreserveRestoredRoute(
    navigationWasRestored: Boolean,
    isInitialActiveThreadObservation: Boolean,
    currentRoute: Route,
): Boolean = navigationWasRestored &&
    isInitialActiveThreadObservation &&
    currentRoute != Route.Home

private fun encodeRouteValue(value: String): String =
    Base64.getUrlEncoder()
        .withoutPadding()
        .encodeToString(value.toByteArray(StandardCharsets.UTF_8))

private fun decodeRouteValue(value: String): String? = runCatching {
    String(Base64.getUrlDecoder().decode(value), StandardCharsets.UTF_8)
}.getOrNull()

internal fun Route.restorationToken(): String = when (this) {
    Route.Home -> "home"
    is Route.Sessions -> "sessions:${encodeRouteValue(serverId)}:${encodeRouteValue(title)}"
    is Route.Conversation -> "conversation:${encodeRouteValue(key.serverId)}:${encodeRouteValue(key.threadId)}"
    is Route.RealtimeVoice -> "voice:${encodeRouteValue(key.serverId)}:${encodeRouteValue(key.threadId)}"
    is Route.ConversationInfo -> "conversation-info:${encodeRouteValue(key.serverId)}:${encodeRouteValue(key.threadId)}"
    is Route.WallpaperSelection -> "wallpaper-select:${encodeRouteValue(key.serverId)}:${encodeRouteValue(key.threadId)}"
    is Route.WallpaperAdjust -> "wallpaper-adjust:${encodeRouteValue(key.serverId)}:${encodeRouteValue(key.threadId)}"
    is Route.ServerInfo -> "server-info:${encodeRouteValue(serverId)}"
    is Route.ServerWallpaperSelection -> "server-wallpaper-select:${encodeRouteValue(serverId)}"
    is Route.ServerWallpaperAdjust -> "server-wallpaper-adjust:${encodeRouteValue(serverId)}"
    Route.Apps -> "apps"
    is Route.SavedApp -> "saved-app:${encodeRouteValue(appId)}"
    is Route.Terminal -> "terminal-v2:${preferredRemoraLinkHostId?.let(::encodeRouteValue).orEmpty()}"
}

internal fun routeFromRestorationToken(token: String): Route? {
    val parts = token.split(':')
    fun value(index: Int): String? = parts.getOrNull(index)?.let(::decodeRouteValue)
    fun threadKey(): ThreadKey? {
        val serverId = value(1)?.takeIf { it.isNotBlank() } ?: return null
        val threadId = value(2)?.takeIf { it.isNotBlank() } ?: return null
        return ThreadKey(serverId = serverId, threadId = threadId)
    }

    return when (parts.firstOrNull()) {
        "home" -> Route.Home
        "sessions" -> {
            val serverId = value(1) ?: return null
            val title = value(2) ?: return null
            Route.Sessions(serverId, title)
        }
        "conversation" -> threadKey()?.let(Route::Conversation)
        "voice" -> threadKey()?.let(Route::RealtimeVoice)
        "conversation-info" -> threadKey()?.let(Route::ConversationInfo)
        "wallpaper-select" -> threadKey()?.let(Route::WallpaperSelection)
        "wallpaper-adjust" -> threadKey()?.let(Route::WallpaperAdjust)
        "server-info" -> value(1)?.let(Route::ServerInfo)
        "server-wallpaper-select" -> value(1)?.let(Route::ServerWallpaperSelection)
        "server-wallpaper-adjust" -> value(1)?.let(Route::ServerWallpaperAdjust)
        "apps" -> Route.Apps
        "saved-app" -> value(1)?.let(Route::SavedApp)
        "terminal-v2" -> Route.Terminal(parts.getOrNull(1)?.takeIf { it.isNotEmpty() }?.let(::decodeRouteValue))
        else -> null
    }
}

internal fun restoreRouteStack(tokens: List<String>): List<Route> {
    val restored = tokens.mapNotNull(::routeFromRestorationToken)
    if (restored.isEmpty()) return listOf(Route.Home)
    return listOf(Route.Home) + restored.filterNot { it == Route.Home }
}

internal val RouteStackSaver = Saver<List<Route>, ArrayList<String>>(
    save = { stack -> ArrayList(stack.map(Route::restorationToken)) },
    restore = { tokens -> restoreRouteStack(tokens) },
)

/** False for a fresh composition and true only after Android restores saved state. */
internal val NavigationRestorationMarkerSaver = Saver<Boolean, Boolean>(
    save = { true },
    restore = { true },
)
