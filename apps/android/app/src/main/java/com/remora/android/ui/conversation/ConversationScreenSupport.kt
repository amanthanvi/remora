package com.remora.android.ui.conversation

import androidx.compose.animation.core.LinearEasing
import androidx.compose.animation.core.RepeatMode
import androidx.compose.animation.core.animateFloat
import androidx.compose.animation.core.infiniteRepeatable
import androidx.compose.animation.core.rememberInfiniteTransition
import androidx.compose.animation.core.tween
import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.SportsEsports
import androidx.compose.material3.Icon
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.graphics.Brush
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import com.remora.android.state.contextPercent
import com.remora.android.ui.RemoraTextStyle
import com.remora.android.ui.RemoraTheme
import com.remora.android.ui.scaled
import uniffi.codex_mobile_client.AppThreadSnapshot
import uniffi.codex_mobile_client.HydratedConversationItem
import uniffi.codex_mobile_client.HydratedConversationItemContent
import uniffi.codex_mobile_client.PendingUserInputRequest
import uniffi.codex_mobile_client.ThreadKey

internal fun PendingUserInputRequest.isRelevantToThread(threadKey: ThreadKey): Boolean {
    if (serverId != threadKey.serverId) return false

    val requestThreadId = threadId.trim()
    return requestThreadId.isEmpty() || requestThreadId == threadKey.threadId
}

internal fun AppThreadSnapshot.composerContextPercent(): Int? {
    if (contextTokensUsed == null && modelContextWindow == null) return null
    val contextWindow = modelContextWindow?.toLong()
    val baseline = 12_000L
    if (contextWindow == null || contextWindow <= baseline) {
        return contextPercent.coerceIn(0, 100)
    }
    val totalTokens = contextTokensUsed?.toLong() ?: baseline
    val effectiveWindow = contextWindow - baseline
    val usedTokens = (totalTokens - baseline).coerceAtLeast(0)
    val remainingTokens = (effectiveWindow - usedTokens).coerceAtLeast(0)
    return ((remainingTokens.toDouble() / effectiveWindow.toDouble()) * 100.0)
        .toInt()
        .coerceIn(0, 100)
}

internal fun conversationBottomAnchorIndex(turnCount: Int): Int = turnCount + 1

/**
 * Resolve the user-message position in the currently-loaded transcript.
 * `forkThreadFromMessage` / `editMessage` on the Rust side expect an index
 * into the thread's items filtered to user messages — see
 * `rollback_depth_for_turn` in `mobile_client/thread_projection.rs`.
 * Recomputing from the live `items` keeps the index correct under
 * pagination (older turns can shift positions; a cached `sourceTurnIndex`
 * from a prior hydrate would be stale).
 */
internal fun loadedUserItemIndex(
    items: List<HydratedConversationItem>,
    messageId: String,
): UInt? {
    var idx = 0u
    for (candidate in items) {
        if (candidate.content !is HydratedConversationItemContent.User) continue
        if (candidate.id == messageId) return idx
        idx++
    }
    return null
}

internal fun lastUserAndAssistantText(
    items: List<HydratedConversationItem>,
): Pair<String?, String?> {
    var lastUser: String? = null
    var lastAssistant: String? = null
    for (item in items.reversed()) {
        when (val content = item.content) {
            is HydratedConversationItemContent.User -> if (lastUser == null) lastUser = content.v1.text
            is HydratedConversationItemContent.Assistant -> if (lastAssistant == null) lastAssistant = content.v1.text
            else -> {}
        }
        if (lastUser != null && lastAssistant != null) break
    }
    return lastUser to lastAssistant
}

/** Shimmering "Thinking..." text shown while the assistant is working. */
@Composable
internal fun StreamingCursor() {
    val transition = rememberInfiniteTransition(label = "shimmer")
    val shimmerOffset by transition.animateFloat(
        initialValue = -1f,
        targetValue = 2f,
        animationSpec = infiniteRepeatable(
            animation = tween(durationMillis = 1500, easing = LinearEasing),
            repeatMode = RepeatMode.Restart,
        ),
        label = "shimmerOffset",
    )
    val shimmerBrush = Brush.linearGradient(
        colors = listOf(
            RemoraTheme.textSecondary.copy(alpha = 0.4f),
            RemoraTheme.accent,
            RemoraTheme.textSecondary.copy(alpha = 0.4f),
        ),
        start = Offset(shimmerOffset * 200f, 0f),
        end = Offset((shimmerOffset + 0.6f) * 200f, 0f),
    )
    Text(
        text = "Thinking...",
        fontSize = RemoraTextStyle.body.scaled,
        fontWeight = FontWeight.Medium,
        style = TextStyle(brush = shimmerBrush),
    )
}

@Composable
internal fun MinigameLaunchButton(onClick: () -> Unit) {
    Surface(
        onClick = onClick,
        shape = CircleShape,
        color = RemoraTheme.surface.copy(alpha = 0.9f),
        border = BorderStroke(0.5.dp, RemoraTheme.accent.copy(alpha = 0.3f)),
        shadowElevation = 2.dp,
        modifier = Modifier.size(36.dp),
    ) {
        Box(Modifier.fillMaxSize(), contentAlignment = Alignment.Center) {
            Icon(
                imageVector = Icons.Filled.SportsEsports,
                contentDescription = "Play a minigame while waiting",
                tint = RemoraTheme.accent,
                modifier = Modifier.size(18.dp),
            )
        }
    }
}
