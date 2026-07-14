package com.remora.android.ui.conversation

import android.annotation.SuppressLint
import android.content.Intent
import com.remora.android.R
import androidx.compose.foundation.ExperimentalFoundationApi
import android.graphics.BitmapFactory
import android.net.Uri
import android.util.Base64
import coil.compose.AsyncImage
import coil.request.ImageRequest
import android.webkit.WebResourceRequest
import android.webkit.WebView
import android.webkit.WebViewClient
import androidx.compose.animation.animateContentSize
import androidx.compose.animation.core.RepeatMode
import androidx.compose.animation.core.animateFloat
import androidx.compose.animation.core.infiniteRepeatable
import androidx.compose.animation.core.rememberInfiniteTransition
import androidx.compose.animation.core.tween
import androidx.compose.foundation.Image
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.combinedClickable
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.ColumnScope
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Chat
import androidx.compose.material.icons.filled.CheckCircle
import androidx.compose.material.icons.filled.Dns
import androidx.compose.material.icons.filled.Error
import androidx.compose.material.icons.filled.GridView
import androidx.compose.material.icons.filled.HourglassEmpty
import androidx.compose.material.icons.filled.PhoneAndroid
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.produceState
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.alpha
import androidx.compose.ui.draw.clip
import androidx.compose.ui.draw.scale
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.viewinterop.AndroidView
import androidx.compose.ui.graphics.Brush
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.text.AnnotatedString
import androidx.compose.ui.text.SpanStyle
import androidx.compose.ui.text.buildAnnotatedString
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontStyle
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.text.withStyle
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import com.remora.android.state.SavedAppsStore
import com.remora.android.ui.BerkeleyMono
import com.remora.android.ui.LocalAppModel
import com.remora.android.ui.RemoraTextStyle
import com.remora.android.ui.RemoraTheme
import com.remora.android.ui.LocalTextScale
import com.remora.android.ui.scaled
import com.remora.android.state.AppModel
import androidx.compose.runtime.rememberCoroutineScope
import kotlinx.coroutines.launch
import org.json.JSONArray
import org.json.JSONObject
import uniffi.codex_mobile_client.AppMessageRenderBlock
import uniffi.codex_mobile_client.AppOperationStatus
import uniffi.codex_mobile_client.HydratedConversationItem
import uniffi.codex_mobile_client.HydratedConversationItemContent
import uniffi.codex_mobile_client.HydratedPlanStepStatus
import kotlin.math.roundToInt
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.delay
import kotlinx.coroutines.withContext

private const val UserMessageTextPreviewLimit = 1_000

/**
 * Renders a single [HydratedConversationItem] by matching on its content type.
 * Uses Rust-provided types directly — no intermediate model conversion.
 */
@Composable
fun ConversationTimelineItem(
    item: HydratedConversationItem,
    serverId: String,
    threadId: String,
    agentDirectoryVersion: ULong,
    latestCommandExecutionItemId: String? = null,
    isLiveTurn: Boolean = false,
    isStreamingMessage: Boolean = false,
    onStreamingSnapshotRendered: (() -> Unit)? = null,
    onEditMessage: ((String) -> Unit)? = null,
    onForkFromMessage: ((String) -> Unit)? = null,
    onOpenSavedApp: ((String) -> Unit)? = null,
    onWidgetPrompt: ((String) -> Unit)? = null,
) {
    val shouldNotifyLiveContentRendered = remember(item.content, isLiveTurn) {
        isLiveTurn && item.content.shouldAutoFollowRenderedContent()
    }

    LaunchedEffect(item.id, item.hashCode(), shouldNotifyLiveContentRendered) {
        if (!shouldNotifyLiveContentRendered) return@LaunchedEffect
        delay(32)
        onStreamingSnapshotRendered?.invoke()
    }

    when (val content = item.content) {
        is HydratedConversationItemContent.User -> UserMessageRow(
            data = content.v1,
            itemId = item.id,
            onEdit = onEditMessage,
            onFork = onForkFromMessage,
        )

        is HydratedConversationItemContent.Assistant -> AssistantMessageRow(
            itemId = item.id,
            data = content.v1,
            serverId = serverId,
            agentDirectoryVersion = agentDirectoryVersion,
            isStreamingMessage = isStreamingMessage,
            onStreamingSnapshotRendered = onStreamingSnapshotRendered,
        )

        is HydratedConversationItemContent.CodeReview -> CodeReviewRow(
            data = content.v1,
        )

        is HydratedConversationItemContent.Reasoning -> ReasoningRow(
            data = content.v1,
        )

        is HydratedConversationItemContent.CommandExecution -> CommandExecutionRow(
            data = content.v1,
            keepExpanded = item.id == latestCommandExecutionItemId ||
                content.v1.status == AppOperationStatus.PENDING ||
                content.v1.status == AppOperationStatus.IN_PROGRESS,
        )

        is HydratedConversationItemContent.FileChange -> FileChangeRow(
            data = content.v1,
        )

        is HydratedConversationItemContent.TurnDiff -> TurnDiffRow(
            data = content.v1,
        )

        is HydratedConversationItemContent.TodoList -> TodoListRow(
            data = content.v1,
        )

        is HydratedConversationItemContent.ProposedPlan -> ProposedPlanRow(
            data = content.v1,
        )

        is HydratedConversationItemContent.McpToolCall -> {
            val cu = content.v1.computerUse
            if (cu != null) {
                ComputerUseToolCallRow(data = content.v1, view = cu)
            } else {
                McpToolCallRow(data = content.v1)
            }
        }

        is HydratedConversationItemContent.DynamicToolCall -> DynamicToolCallRow(
            data = content.v1,
        )

        is HydratedConversationItemContent.MultiAgentAction -> {
            SubagentCard(data = content.v1, serverId = serverId)
        }

        is HydratedConversationItemContent.WebSearch -> WebSearchRow(
            data = content.v1,
        )

        is HydratedConversationItemContent.ImageView -> ImageViewRow(
            data = content.v1,
            serverId = serverId,
        )

        is HydratedConversationItemContent.ImageGeneration -> ImageGenerationRow(
            data = content.v1,
        )

        is HydratedConversationItemContent.Widget -> WidgetRow(
            data = content.v1,
            originThreadId = threadId,
            onOpenSavedApp = onOpenSavedApp,
            onWidgetPrompt = onWidgetPrompt,
        )

        is HydratedConversationItemContent.UserInputResponse -> UserInputResponseRow(
            data = content.v1,
        )

        is HydratedConversationItemContent.Divider -> DividerRow(
            data = content.v1,
            isLiveTurn = isLiveTurn,
        )

        is HydratedConversationItemContent.Error -> ErrorRow(
            data = content.v1,
        )

        is HydratedConversationItemContent.Note -> NoteRow(
            data = content.v1,
        )
    }
}

private fun HydratedConversationItemContent.shouldAutoFollowRenderedContent(): Boolean {
    return when (this) {
        is HydratedConversationItemContent.Reasoning,
        is HydratedConversationItemContent.CommandExecution,
        is HydratedConversationItemContent.FileChange,
        is HydratedConversationItemContent.TurnDiff,
        is HydratedConversationItemContent.McpToolCall,
        is HydratedConversationItemContent.DynamicToolCall,
        is HydratedConversationItemContent.MultiAgentAction,
        is HydratedConversationItemContent.WebSearch,
        is HydratedConversationItemContent.ImageView,
        is HydratedConversationItemContent.ImageGeneration,
        is HydratedConversationItemContent.Widget -> true
        else -> false
    }
}

// ── User Message ─────────────────────────────────────────────────────────────

@OptIn(androidx.compose.foundation.ExperimentalFoundationApi::class)
@Composable
private fun UserMessageRow(
    data: uniffi.codex_mobile_client.HydratedUserMessageData,
    itemId: String,
    onEdit: ((String) -> Unit)?,
    onFork: ((String) -> Unit)?,
) {
    var showMenu by remember { mutableStateOf(false) }
    val context = LocalContext.current

    // Right-aligned user bubble matching iOS `UserBubble`: accent-tinted
    // rounded rect that hugs content width, with a 60dp minimum gutter on
    // the left so long messages wrap before reaching that edge.
    //
    // Long-press opens an action menu (Edit / Fork / Copy). Text selection is
    // disabled on user bubbles because Compose's SelectionContainer would
    // consume the long-press gesture before our handler sees it; copy is
    // exposed via the menu instead.
    Row(
        modifier = Modifier
            .fillMaxWidth()
            .padding(top = 10.dp, bottom = 14.dp),
        horizontalArrangement = Arrangement.End,
    ) {
        Box {
            Column(
                horizontalAlignment = Alignment.End,
                modifier = Modifier
                    .padding(start = 60.dp)
                    .background(
                        RemoraTheme.accent.copy(alpha = 0.3f),
                        RoundedCornerShape(18.dp),
                    )
                    .combinedClickable(
                        onClick = {},
                        onLongClick = { showMenu = true },
                    )
                    .padding(horizontal = 18.dp, vertical = 14.dp),
            ) {
                LimitedUserMessageText(data.text)
                // Inline images from data URIs
                for (uri in data.imageDataUris) {
                    val bytes = remember(uri) {
                        try {
                            val base64Part = uri.substringAfter("base64,", "")
                            if (base64Part.isNotEmpty()) Base64.decode(base64Part, Base64.DEFAULT) else null
                        } catch (_: Exception) { null }
                    }
                    bytes?.let {
                        AsyncImage(
                            model = ImageRequest.Builder(context)
                                .data(it)
                                .crossfade(false)
                                .build(),
                            contentDescription = "Attached image",
                            modifier = Modifier
                                .padding(top = 4.dp)
                                .heightIn(max = 200.dp)
                                .clip(RoundedCornerShape(8.dp)),
                        )
                    }
                }
            }
            DropdownMenu(
                expanded = showMenu,
                onDismissRequest = { showMenu = false },
            ) {
                if (onEdit != null) {
                    DropdownMenuItem(
                        text = { Text("Edit Message") },
                        onClick = { showMenu = false; onEdit(itemId) },
                    )
                }
                if (onFork != null) {
                    DropdownMenuItem(
                        text = { Text("Fork From Here") },
                        onClick = { showMenu = false; onFork(itemId) },
                    )
                }
                DropdownMenuItem(
                    text = { Text("Copy") },
                    onClick = {
                        showMenu = false
                        val cm = context.getSystemService(android.content.Context.CLIPBOARD_SERVICE)
                            as android.content.ClipboardManager
                        cm.setPrimaryClip(android.content.ClipData.newPlainText("message", data.text))
                    },
                )
            }
        }
    }
}

@Composable
private fun LimitedUserMessageText(text: String) {
    val isLong = text.length > UserMessageTextPreviewLimit
    var expanded by remember(text) { mutableStateOf(false) }
    val display = remember(text, expanded) {
        if (isLong && !expanded) text.take(UserMessageTextPreviewLimit) else text
    }

    com.remora.android.ui.common.FormattedText(
        text = display,
        color = RemoraTheme.textPrimary,
        fontSize = RemoraTextStyle.callout.scaled,
    )

    if (isLong) {
        Text(
            text = if (expanded) "Show less" else "Show more",
            color = RemoraTheme.accent,
            fontSize = RemoraTextStyle.caption2.scaled,
            fontWeight = FontWeight.SemiBold,
            modifier = Modifier
                .padding(top = 4.dp)
                .clickable { expanded = !expanded },
        )
    }
}

// ── Assistant Message ────────────────────────────────────────────────────────

@Composable
private fun AssistantMessageRow(
    itemId: String,
    data: uniffi.codex_mobile_client.HydratedAssistantMessageData,
    serverId: String,
    agentDirectoryVersion: ULong,
    isStreamingMessage: Boolean,
    onStreamingSnapshotRendered: (() -> Unit)?,
) {
    val appModel = LocalAppModel.current
    val renderBlocks = remember(itemId, data.text, serverId, agentDirectoryVersion, isStreamingMessage) {
        if (isStreamingMessage) {
            emptyList()
        } else {
            MessageRenderCache.getRenderBlocks(
                key = MessageRenderCache.CacheKey(
                    itemId = itemId,
                    revisionToken = data.text.hashCode(),
                    serverId = serverId,
                    agentDirectoryVersion = agentDirectoryVersion,
                ),
                parser = appModel.parser,
                text = data.text,
            )
        }
    }
    var renderedText by remember(itemId) { mutableStateOf(data.text) }
    var pendingText by remember(itemId) { mutableStateOf<String?>(null) }

    LaunchedEffect(itemId) {
        renderedText = data.text
        pendingText = null
        if (isStreamingMessage) {
            onStreamingSnapshotRendered?.invoke()
        }
    }

    LaunchedEffect(data.text, isStreamingMessage) {
        if (!isStreamingMessage) {
            renderedText = data.text
            pendingText = null
            StreamingTextCoordinator.evict(itemId)
            return@LaunchedEffect
        }
        if (data.text == renderedText) return@LaunchedEffect
        if (renderedText.isEmpty()) {
            renderedText = data.text
            onStreamingSnapshotRendered?.invoke()
        } else {
            pendingText = data.text
        }
    }

    LaunchedEffect(pendingText, isStreamingMessage) {
        val nextText = pendingText ?: return@LaunchedEffect
        if (!isStreamingMessage) return@LaunchedEffect
        delay(60)
        renderedText = nextText
        pendingText = null
        onStreamingSnapshotRendered?.invoke()
    }

    Column(
        modifier = Modifier
            .fillMaxWidth()
            .padding(vertical = 4.dp),
    ) {
        // Agent badge
        if (data.agentNickname != null || data.agentRole != null) {
            val label = buildString {
                data.agentNickname?.let { append(it) }
                data.agentRole?.let {
                    if (isNotEmpty()) append(" ")
                    append("[$it]")
                }
            }
            Text(
                text = label,
                color = RemoraTheme.accent,
                fontSize = RemoraTextStyle.caption2.scaled,
                fontWeight = FontWeight.Medium,
            )
            Spacer(Modifier.height(2.dp))
        }

        if (isStreamingMessage) {
            StreamingMarkdownView(
                text = renderedText,
                itemId = itemId,
                onRendered = onStreamingSnapshotRendered,
            )
        } else {
            AssistantRenderBlocks(
                blocks = renderBlocks,
                fallbackText = renderedText,
            )
        }
    }
}

@Composable
private fun AssistantRenderBlocks(
    blocks: List<AppMessageRenderBlock>,
    fallbackText: String,
) {
    if (blocks.isEmpty()) {
        MarkdownText(text = fallbackText)
        return
    }

    val context = LocalContext.current
    Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
        blocks.forEachIndexed { index, block ->
            when (block) {
                is AppMessageRenderBlock.Markdown -> MarkdownText(text = block.markdown)
                is AppMessageRenderBlock.CodeBlock -> {
                    if (isMathLanguage(block.language)) {
                        MarkdownText(text = mathMarkdownBlock(block.code))
                    } else {
                        ConversationCodeBlock(
                            language = block.language,
                            code = block.code,
                        )
                    }
                }
                is AppMessageRenderBlock.InlineImage -> {
                    AsyncImage(
                        model = ImageRequest.Builder(context)
                            .data(block.data)
                            .crossfade(false)
                            .build(),
                        contentDescription = "Assistant image ${index + 1}",
                        modifier = Modifier
                            .fillMaxWidth()
                            .heightIn(max = 300.dp)
                            .clip(RoundedCornerShape(10.dp)),
                    )
                }
            }
        }
    }
}

@Composable
private fun CodeReviewRow(
    data: uniffi.codex_mobile_client.HydratedCodeReviewData,
) {
    var dismissedIndices by remember(data.findings) { mutableStateOf(setOf<Int>()) }
    val visibleFindings = remember(data.findings, dismissedIndices) {
        data.findings.mapIndexedNotNull { index, finding ->
            if (dismissedIndices.contains(index)) null else index to finding
        }
    }

    Column(
        modifier = Modifier
            .fillMaxWidth()
            .padding(vertical = 4.dp),
        verticalArrangement = Arrangement.spacedBy(12.dp),
    ) {
        visibleFindings.forEach { (index, finding) ->
            CodeReviewFindingCard(
                finding = finding,
                onDismiss = { dismissedIndices = dismissedIndices + index },
            )
        }
    }
}

@Composable
private fun CodeReviewFindingCard(
    finding: uniffi.codex_mobile_client.HydratedCodeReviewFindingData,
    onDismiss: () -> Unit,
) {
    val priorityTint = when (finding.priority?.toInt()) {
        0, 1 -> RemoraTheme.danger
        2 -> RemoraTheme.warning
        3 -> RemoraTheme.textSecondary
        else -> RemoraTheme.textSecondary
    }
    val locationText = remember(finding.codeLocation) {
        val location = finding.codeLocation ?: return@remember null
        val range = location.lineRange
        when {
            range == null -> location.absoluteFilePath
            range.start == range.end -> "${location.absoluteFilePath}:${range.start}"
            else -> "${location.absoluteFilePath}:${range.start}-${range.end}"
        }
    }

    Column(
        modifier = Modifier
            .fillMaxWidth()
            .background(RemoraTheme.surface.copy(alpha = 0.72f), RoundedCornerShape(22.dp))
            .padding(20.dp),
        verticalArrangement = Arrangement.spacedBy(14.dp),
    ) {
        Row(
            modifier = Modifier.fillMaxWidth(),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            finding.priority?.let { priority ->
                Text(
                    text = "P${priority.toInt()}",
                    color = priorityTint,
                    fontSize = RemoraTextStyle.caption2.scaled,
                    fontWeight = FontWeight.Bold,
                    modifier = Modifier
                        .background(priorityTint.copy(alpha = 0.12f), RoundedCornerShape(999.dp))
                        .padding(horizontal = 10.dp, vertical = 6.dp),
                )
                Spacer(Modifier.width(10.dp))
            }

            Text(
                text = finding.title,
                color = RemoraTheme.textPrimary,
                fontSize = RemoraTextStyle.callout.scaled,
                fontWeight = FontWeight.SemiBold,
                modifier = Modifier.weight(1f),
            )

            Text(
                text = "Dismiss",
                color = RemoraTheme.textSecondary,
                fontSize = RemoraTextStyle.callout.scaled,
                fontWeight = FontWeight.Medium,
                modifier = Modifier.clickable(onClick = onDismiss),
            )
        }

        MarkdownText(text = finding.body)

        locationText?.takeIf { it.isNotBlank() }?.let { location ->
            Text(
                text = location,
                color = RemoraTheme.textSecondary,
                fontSize = RemoraTextStyle.footnote.scaled,
                fontFamily = RemoraTheme.monoFont,
            )
        }
    }
}

// ── Reasoning ────────────────────────────────────────────────────────────────

@Composable
private fun ReasoningRow(
    data: uniffi.codex_mobile_client.HydratedReasoningData,
) {
    val reasoningText = remember(data.summary, data.content) {
        (data.summary + data.content)
            .filter { it.isNotBlank() }
            .joinToString(separator = "\n\n")
    }

    if (reasoningText.isBlank()) return

    SelectableConversationText(
        modifier = Modifier
            .fillMaxWidth()
            .padding(vertical = 4.dp),
    ) {
        Text(
            text = reasoningText,
            color = RemoraTheme.textSecondary,
            fontSize = RemoraTextStyle.body.scaled,
            fontFamily = RemoraTheme.monoFont,
            fontStyle = FontStyle.Italic,
        )
    }
}

@Composable
private fun UserInputResponseRow(
    data: uniffi.codex_mobile_client.HydratedUserInputResponseData,
) {
    Column(
        modifier = Modifier
            .fillMaxWidth()
            .background(RemoraTheme.surface, RoundedCornerShape(12.dp))
            .padding(horizontal = 10.dp, vertical = 6.dp),
        verticalArrangement = Arrangement.spacedBy(6.dp),
    ) {
        Text(
            text = "Requested Input",
            color = RemoraTheme.textPrimary,
            fontSize = RemoraTextStyle.body.scaled,
            fontWeight = FontWeight.SemiBold,
        )

        data.questions.forEach { question ->
            Column(verticalArrangement = Arrangement.spacedBy(2.dp)) {
                question.header?.takeIf { it.isNotBlank() }?.let { header ->
                    Text(
                        text = header.uppercase(),
                        color = RemoraTheme.textMuted,
                        fontSize = RemoraTextStyle.caption2.scaled,
                        fontWeight = FontWeight.Bold,
                    )
                }
                Text(
                    text = question.question,
                    color = RemoraTheme.textPrimary,
                    fontSize = RemoraTextStyle.body.scaled,
                    fontWeight = FontWeight.Medium,
                )
                Text(
                    text = question.answer.ifBlank { "No answer provided" },
                    color = RemoraTheme.textSecondary,
                    fontSize = RemoraTextStyle.body.scaled,
                )
            }
        }
    }
}

// ── Divider ──────────────────────────────────────────────────────────────────

@Composable
private fun TurnDiffRow(
    data: uniffi.codex_mobile_client.HydratedTurnDiffData,
) {
    ToolCardShell(
        summary = "Turn Diff",
        accent = RemoraTheme.toolCallFileChange,
        status = AppOperationStatus.COMPLETED,
    ) {
        DiffSection(label = "Diff", content = data.diff)
    }
}

@Composable
private fun DividerRow(
    data: uniffi.codex_mobile_client.HydratedDividerData,
    isLiveTurn: Boolean,
) {
    val label = when (data) {
        is uniffi.codex_mobile_client.HydratedDividerData.ContextCompaction ->
            if (data.isComplete && !isLiveTurn) "Context compacted" else "Compacting context\u2026"
        is uniffi.codex_mobile_client.HydratedDividerData.ModelRerouted -> {
            val route = data.fromModel?.takeIf { it.isNotBlank() }?.let { "$it -> ${data.toModel}" }
                ?: "Routed to ${data.toModel}"
            val reason = data.reason?.takeIf { it.isNotBlank() }
            if (reason != null) "$route | $reason" else route
        }
        is uniffi.codex_mobile_client.HydratedDividerData.ReviewEntered -> "Review started"
        is uniffi.codex_mobile_client.HydratedDividerData.ReviewExited -> "Review ended"
    }

    Row(
        modifier = Modifier
            .fillMaxWidth()
            .padding(vertical = 8.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        HorizontalDivider(
            modifier = Modifier.weight(1f),
            color = RemoraTheme.divider,
        )
        Text(
            text = "  $label  ",
            color = RemoraTheme.textMuted,
            fontSize = RemoraTextStyle.caption2.scaled,
        )
        HorizontalDivider(
            modifier = Modifier.weight(1f),
            color = RemoraTheme.divider,
        )
    }
}

// ── Note ─────────────────────────────────────────────────────────────────────

@Composable
private fun NoteRow(
    data: uniffi.codex_mobile_client.HydratedNoteData,
) {
    Column(
        modifier = Modifier
            .fillMaxWidth()
            .background(RemoraTheme.surface, RoundedCornerShape(8.dp))
            .padding(8.dp),
    ) {
        Text(
            text = data.title,
            color = RemoraTheme.textPrimary,
            fontSize = RemoraTextStyle.body.scaled,
            fontWeight = FontWeight.Medium,
        )
        if (data.body.isNotBlank()) {
            Text(
                text = data.body,
                color = RemoraTheme.textSecondary,
                fontSize = RemoraTextStyle.body.scaled,
                modifier = Modifier.padding(top = 2.dp),
            )
        }
    }
}

@Composable
private fun ErrorRow(
    data: uniffi.codex_mobile_client.HydratedErrorData,
) {
    Column(
        modifier = Modifier
            .fillMaxWidth()
            .background(RemoraTheme.surface, RoundedCornerShape(8.dp))
            .padding(8.dp),
    ) {
        SelectableConversationText {
            Text(
                text = data.title.ifBlank { "Error" },
                color = RemoraTheme.danger,
                fontSize = RemoraTextStyle.body.scaled,
                fontWeight = FontWeight.Medium,
            )
            Text(
                text = data.message,
                color = RemoraTheme.textPrimary,
                fontSize = RemoraTextStyle.body.scaled,
                modifier = Modifier.padding(top = 2.dp),
            )
            data.details?.takeIf { it.isNotBlank() }?.let { details ->
                Text(
                    text = details,
                    color = RemoraTheme.textSecondary,
                    fontSize = RemoraTextStyle.body.scaled,
                    modifier = Modifier.padding(top = 2.dp),
                )
            }
        }
    }
}

// ── Markdown Rendering ───────────────────────────────────────────────────

@Composable
internal fun MarkdownText(
    text: String,
    modifier: Modifier = Modifier,
) {
    if (com.remora.android.state.DebugSettings.enabled && com.remora.android.state.DebugSettings.disableMarkdown) {
        SelectableConversationText(modifier = modifier.fillMaxWidth()) {
            Text(
                text = text,
                color = RemoraTheme.textBody,
                fontFamily = FontFamily.Monospace,
                fontSize = RemoraTextStyle.body.scaled,
            )
        }
        return
    }

    SelectableMarkdownText(
        text = text,
        modifier = modifier.fillMaxWidth(),
    )
}
