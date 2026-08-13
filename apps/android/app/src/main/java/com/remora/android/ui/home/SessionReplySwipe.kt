package com.remora.android.ui.home

import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.Reply
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import com.remora.android.state.AppComposerPayload
import com.remora.android.state.AppModel
import com.remora.android.ui.RemoraTheme
import com.remora.android.ui.common.SwipeAction
import com.remora.android.ui.common.SwipeableRow
import uniffi.codex_mobile_client.AppServerHealth
import uniffi.codex_mobile_client.AppSessionSummary

/**
 * Wraps a session row with a right-swipe "reply" gesture. When the user
 * releases past the commit threshold, a [QuickReplySheet] opens pre-targeted
 * at the session's thread. Mirrors iOS `SessionReplySwipe.swift`.
 *
 * An optional [trailingAction] is forwarded to the inner [SwipeableRow] so
 * the same row can host both a reply swipe (leading) and a hide/archive
 * swipe (trailing) — nesting two separate swipe wrappers would have the
 * inner and outer gesture handlers fighting over the same pointer stream.
 *
 * The send path resumes connected cold-launch threads and otherwise uses the
 * same secure text-only outbox as the full conversation composer.
 */
@Composable
fun SessionReplySwipe(
    session: AppSessionSummary,
    appModel: AppModel,
    modifier: Modifier = Modifier,
    trailingAction: SwipeAction? = null,
    onError: (String) -> Unit = {},
    /**
     * Caller-provided trigger so the long-press menu can open the same
     * reply sheet path as the swipe. When null, the swipe still owns its
     * own local sheet state (legacy behavior).
     */
    onReply: (() -> Unit)? = null,
    content: @Composable () -> Unit,
) {
    var isSheetVisible by remember { mutableStateOf(false) }

    SwipeableRow(
        leadingAction = SwipeAction(
            icon = Icons.AutoMirrored.Filled.Reply,
            label = "reply",
            tint = RemoraTheme.accent,
            onTrigger = { onReply?.invoke() ?: run { isSheetVisible = true } },
        ),
        trailingAction = trailingAction,
        modifier = modifier,
    ) {
        content()
    }

    if (isSheetVisible) {
        QuickReplySheet(
            thread = session,
            onDismiss = { isSheetVisible = false },
            onSend = { threadKey, text ->
                runCatching {
                    sendQuickReplyTurn(appModel, threadKey, text)
                }.onFailure { err ->
                    onError(err.message ?: "Failed to send reply")
                }
            },
        )
    }
}

/**
 * Send-path for a quick reply from the home dashboard. Hoisted out of
 * `SessionReplySwipe` so the long-press menu can reuse it via a single
 * caller-owned reply sheet. Mirrors iOS `RemoraApp.swift:1043-1078`.
 */
suspend fun sendQuickReplyTurn(
    appModel: AppModel,
    threadKey: uniffi.codex_mobile_client.ThreadKey,
    text: String,
) {
    val connected = appModel.snapshot.value?.servers
        ?.firstOrNull { it.serverId == threadKey.serverId }
        ?.health == AppServerHealth.CONNECTED
    val activeKey = if (connected) {
        // Cold-launch snapshots are not necessarily registered with the live
        // upstream session, so the connected path still resumes.
        val resumeKey = appModel.hydrateThreadPermissions(threadKey) ?: threadKey
        try {
            appModel.externalResumeThread(resumeKey)
        } catch (_: Exception) {
            val cwdOverride = appModel.threadSnapshot(resumeKey)?.info?.cwd
            appModel.client.resumeThread(
                resumeKey.serverId,
                appModel.launchState.threadResumeRequest(
                    resumeKey.threadId,
                    cwdOverride = cwdOverride,
                    threadKey = resumeKey,
                ),
            )
        }
        resumeKey
    } else {
        threadKey
    }
    val payload = AppComposerPayload(
        text = text,
        additionalInputs = emptyList(),
        approvalPolicy = appModel.launchState.approvalPolicyValue(activeKey),
        sandboxPolicy = appModel.launchState.turnSandboxPolicy(activeKey),
        model = appModel.launchState.snapshot.value.selectedModel
            .trim().ifEmpty { null },
        reasoningEffort = null,
        serviceTier = null,
    )
    appModel.submitComposerTurn(activeKey, payload)
    if (connected) {
        appModel.refreshThreadSnapshot(activeKey)
    }
}
