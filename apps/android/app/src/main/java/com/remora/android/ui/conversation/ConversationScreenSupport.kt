package com.remora.android.ui.conversation

import androidx.compose.animation.core.LinearEasing
import androidx.compose.animation.core.RepeatMode
import androidx.compose.animation.core.animateFloat
import androidx.compose.animation.core.infiniteRepeatable
import androidx.compose.animation.core.rememberInfiniteTransition
import androidx.compose.animation.core.tween
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.graphics.Brush
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.font.FontWeight
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
