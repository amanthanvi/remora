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
import androidx.compose.material.icons.automirrored.filled.Chat
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

private const val ToolCallTextPreviewLimit = 2_000

internal fun timelineWorkspaceTitle(path: String): String =
    path.trimEnd('/').substringAfterLast('/').ifBlank { path }

// ── Command Execution ────────────────────────────────────────────────────────

@Composable
internal fun CommandExecutionRow(
    data: uniffi.codex_mobile_client.HydratedCommandExecutionData,
    keepExpanded: Boolean,
) {
    var expanded by remember(data.command) { mutableStateOf(keepExpanded) }
    val outputScrollState = rememberScrollState()
    val outputText =
        data.output
            ?.trim('\n')
            ?.takeIf { it.isNotBlank() }
            ?: if (data.status == AppOperationStatus.PENDING || data.status == AppOperationStatus.IN_PROGRESS) {
                "Waiting for output…"
            } else {
                "No output"
            }
    val isRunning = data.status == AppOperationStatus.PENDING || data.status == AppOperationStatus.IN_PROGRESS
    val displayedCommand = remember(data.command) { displayCommandText(data.command) }
    val collapsedCommand = remember(data.command) { collapseCommandText(data.command) }

    LaunchedEffect(keepExpanded) {
        expanded = keepExpanded
    }

    LaunchedEffect(outputText, outputScrollState.maxValue, expanded) {
        if (!expanded) return@LaunchedEffect
        if (outputScrollState.maxValue <= 0) return@LaunchedEffect
        outputScrollState.animateScrollTo(outputScrollState.maxValue)
    }

    Column(
        modifier = Modifier
            .fillMaxWidth()
            .background(RemoraTheme.surface, RoundedCornerShape(12.dp))
            .border(0.5.dp, RemoraTheme.border, RoundedCornerShape(12.dp))
            .clickable { expanded = !expanded }
            .padding(horizontal = 12.dp, vertical = 9.dp),
    ) {
        Row(
            modifier = Modifier.fillMaxWidth(),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Text(
                text = "$",
                color = RemoraTheme.warning,
                fontFamily = RemoraTheme.monoFont,
                fontSize = RemoraTextStyle.caption.scaled,
                fontWeight = FontWeight.SemiBold,
            )
            Spacer(Modifier.width(6.dp))
            Text(
                text = if (expanded) displayedCommand else collapsedCommand,
                color = RemoraTheme.textSystem,
                fontFamily = RemoraTheme.monoFont,
                fontSize = RemoraTextStyle.body.scaled,
                maxLines = if (expanded) Int.MAX_VALUE else 1,
                overflow = TextOverflow.Ellipsis,
                modifier = Modifier.weight(1f),
            )
            data.durationMs?.takeIf { it > 0 }?.let { ms ->
                Spacer(Modifier.width(6.dp))
                DurationChip(formatDuration(ms), statusTint(data.status))
            }
            Spacer(Modifier.width(8.dp))
            Text(
                text = if (expanded) "▲" else "▼",
                color = RemoraTheme.warning,
                fontSize = RemoraTextStyle.caption2.scaled,
                fontWeight = FontWeight.Bold,
            )
        }

        if (expanded) {
            Spacer(Modifier.height(6.dp))
            LimitedToolTextBlock(outputText, previewFromTail = isRunning) { display ->
                Box(
                    modifier = Modifier
                        .fillMaxWidth()
                        .heightIn(max = 116.dp)
                        .background(RemoraTheme.codeBackground, RoundedCornerShape(10.dp))
                        .padding(horizontal = 10.dp, vertical = 8.dp),
                ) {
                    SelectableConversationText {
                        Text(
                            text = display,
                            color = RemoraTheme.textSecondary,
                            fontFamily = RemoraTheme.monoFont,
                            fontSize = RemoraTextStyle.body.scaled,
                            modifier = Modifier
                                .fillMaxWidth()
                                .verticalScroll(outputScrollState),
                        )
                    }
                }
            }
        }
    }
}

// ── File Change ──────────────────────────────────────────────────────────────

@Composable
internal fun FileChangeRow(
    data: uniffi.codex_mobile_client.HydratedFileChangeData,
) {
    val summary = remember(data.changes) {
        buildFileChangeSummary(data)
    }
    val diffChanges = remember(data.changes) {
        data.changes.filter { it.diff.isNotBlank() }
    }

    ToolCardShell(
        summary = summary.plainText,
        summaryAnnotated = summary.annotatedText,
        accent = RemoraTheme.toolCallFileChange,
        status = data.status,
    ) {
        if (diffChanges.isEmpty() && data.changes.isNotEmpty()) {
            ListSection("Files", data.changes.map { timelineWorkspaceTitle(it.path) })
        }
        diffChanges.forEach { change ->
            DiffSection(
                label = if (diffChanges.size > 1) timelineWorkspaceTitle(change.path) else "",
                content = change.diff,
            )
        }
    }
}

private data class FileChangeSummary(
    val plainText: String,
    val annotatedText: AnnotatedString,
)

private fun buildFileChangeSummary(
    data: uniffi.codex_mobile_client.HydratedFileChangeData,
): FileChangeSummary {
    if (data.changes.isEmpty()) {
        return FileChangeSummary(
            plainText = "File changes",
            annotatedText = AnnotatedString("File changes"),
        )
    }

    val additions = data.changes.sumOf { it.additions.toInt() }
    val deletions = data.changes.sumOf { it.deletions.toInt() }
    val hasCountSummary = additions > 0 || deletions > 0

    if (data.changes.size == 1) {
        val change = data.changes.first()
        val verb = fileChangeVerb(change.kind)
        val filename = timelineWorkspaceTitle(change.path)
        if (!hasCountSummary) {
            return FileChangeSummary(
                plainText = "$verb $filename",
                annotatedText = AnnotatedString("$verb $filename"),
            )
        }
        val plainText = "$verb $filename +$additions -$deletions"
        val annotatedText = buildAnnotatedString {
            withStyle(SpanStyle(color = RemoraTheme.textSecondary)) {
                append("$verb ")
            }
            withStyle(SpanStyle(color = RemoraTheme.accent)) {
                append(filename)
            }
            withStyle(SpanStyle(color = RemoraTheme.success)) {
                append(" +$additions")
            }
            withStyle(SpanStyle(color = RemoraTheme.danger)) {
                append(" -$deletions")
            }
        }
        return FileChangeSummary(plainText = plainText, annotatedText = annotatedText)
    }

    if (!hasCountSummary) {
        return FileChangeSummary(
            plainText = "Changed ${data.changes.size} files",
            annotatedText = AnnotatedString("Changed ${data.changes.size} files"),
        )
    }

    val plainText = "Changed ${data.changes.size} files +$additions -$deletions"
    val annotatedText = buildAnnotatedString {
        append("Changed ${data.changes.size} files")
        withStyle(SpanStyle(color = RemoraTheme.success)) {
            append(" +$additions")
        }
        withStyle(SpanStyle(color = RemoraTheme.danger)) {
            append(" -$deletions")
        }
    }
    return FileChangeSummary(plainText = plainText, annotatedText = annotatedText)
}

private fun fileChangeVerb(kind: String): String = when (kind.lowercase()) {
    "add" -> "Added"
    "delete" -> "Deleted"
    "update" -> "Edited"
    else -> "Changed"
}

// ── Todo List ────────────────────────────────────────────────────────────────

@Composable
internal fun TodoListRow(
    data: uniffi.codex_mobile_client.HydratedTodoListData,
) {
    Column(
        modifier = Modifier
            .fillMaxWidth()
            .padding(vertical = 2.dp),
    ) {
        for (step in data.steps) {
            Row(
                verticalAlignment = Alignment.CenterVertically,
                modifier = Modifier.padding(vertical = 1.dp),
            ) {
                val icon = when (step.status) {
                    HydratedPlanStepStatus.COMPLETED -> "✓"
                    HydratedPlanStepStatus.IN_PROGRESS -> "●"
                    HydratedPlanStepStatus.PENDING -> "○"
                }
                val color = when (step.status) {
                    HydratedPlanStepStatus.COMPLETED -> RemoraTheme.success
                    HydratedPlanStepStatus.IN_PROGRESS -> RemoraTheme.accent
                    HydratedPlanStepStatus.PENDING -> RemoraTheme.textMuted
                }
                Text(text = icon, color = color, fontSize = RemoraTextStyle.footnote.scaled)
                Spacer(Modifier.width(6.dp))
                Text(
                    text = step.step,
                    color = RemoraTheme.textBody,
                    fontSize = RemoraTextStyle.body.scaled,
                )
            }
        }
    }
}

// ── Proposed Plan ────────────────────────────────────────────────────────────

@Composable
internal fun ProposedPlanRow(
    data: uniffi.codex_mobile_client.HydratedProposedPlanData,
) {
    Column(
        modifier = Modifier
            .fillMaxWidth()
            .padding(vertical = 4.dp),
    ) {
        Text(
            text = "Plan",
            color = RemoraTheme.accent,
            fontSize = RemoraTextStyle.caption.scaled,
            fontWeight = FontWeight.SemiBold,
        )
        Spacer(Modifier.height(4.dp))
        MarkdownText(text = data.content)
    }
}

// ── MCP Tool Call ────────────────────────────────────────────────────────────

@Composable
internal fun McpToolCallRow(
    data: uniffi.codex_mobile_client.HydratedMcpToolCallData,
) {
    val summary = if (data.server.isBlank()) data.tool else "${data.server}.${data.tool}"
    ToolCardShell(
        summary = summary,
        accent = RemoraTheme.toolCallMcpCall,
        status = data.status,
        durationMs = data.durationMs,
    ) {
        data.argumentsJson?.takeIf { it.isNotBlank() }?.let { CodeSection("Arguments", it) }
        data.contentSummary?.takeIf { it.isNotBlank() }?.let { InlineTextSection("Result", it) }
        data.structuredContentJson?.takeIf { it.isNotBlank() }?.let { CodeSection("Structured", it) }
        data.rawOutputJson?.takeIf { it.isNotBlank() }?.let { CodeSection("Raw Output", it) }
        if (data.progressMessages.isNotEmpty()) {
            ProgressSection("Progress", data.progressMessages)
        }
        data.errorMessage?.takeIf { it.isNotBlank() }?.let { InlineTextSection("Error", it, tone = RemoraTheme.danger) }
    }
}

// ── Computer Use Tool Call (computer-use MCP) ───────────────────────────────

@Composable
internal fun ComputerUseToolCallRow(
    data: uniffi.codex_mobile_client.HydratedMcpToolCallData,
    view: uniffi.codex_mobile_client.ComputerUseView,
) {
    ToolCardShell(
        summary = view.summary,
        accent = RemoraTheme.toolCallMcpCall,
        status = data.status,
        durationMs = data.durationMs,
    ) {
        view.screenshotPng?.let { bytes ->
            ScreenshotPreview(bytes)
        }
        data.errorMessage?.takeIf { it.isNotBlank() }?.let {
            InlineTextSection("Error", it, tone = RemoraTheme.danger)
        }
        view.accessibilityText?.takeIf { it.isNotBlank() }?.let {
            AccessibilityTreeSection(it)
        }
    }
}

@Composable
private fun ScreenshotPreview(bytes: ByteArray) {
    val context = LocalContext.current
    Column {
        Text(
            text = "SCREENSHOT",
            color = RemoraTheme.textSecondary,
            fontSize = 10f.scaled,
            fontWeight = FontWeight.Bold,
        )
        Spacer(Modifier.height(4.dp))
        AsyncImage(
            model = ImageRequest.Builder(context)
                .data(bytes)
                .crossfade(false)
                .build(),
            contentDescription = "Computer Use screenshot",
            contentScale = ContentScale.Fit,
            modifier = Modifier
                .fillMaxWidth()
                .clip(RoundedCornerShape(10.dp))
                .background(RemoraTheme.codeBackground),
        )
    }
}

@Composable
private fun AccessibilityTreeSection(text: String) {
    var expanded by remember(text) { mutableStateOf(false) }
    val lines = remember(text) { text.split('\n') }
    val previewLineCount = 6
    val display = if (expanded || lines.size <= previewLineCount) {
        text
    } else {
        lines.take(previewLineCount).joinToString("\n") + "\n… (${lines.size - previewLineCount} more lines)"
    }

    Column {
        Row(verticalAlignment = Alignment.CenterVertically) {
            Text(
                text = "ACCESSIBILITY TREE",
                color = RemoraTheme.textSecondary,
                fontSize = 10f.scaled,
                fontWeight = FontWeight.Bold,
                modifier = Modifier.weight(1f),
            )
            if (lines.size > previewLineCount) {
                Text(
                    text = if (expanded) "Show less" else "Show more",
                    color = RemoraTheme.accent,
                    fontSize = 10f.scaled,
                    fontWeight = FontWeight.Medium,
                    modifier = Modifier.clickable { expanded = !expanded },
                )
            }
        }
        Spacer(Modifier.height(4.dp))
        Text(
            text = display,
            color = RemoraTheme.textSecondary,
            fontSize = RemoraTextStyle.caption2.scaled,
            fontFamily = BerkeleyMono,
            modifier = Modifier
                .fillMaxWidth()
                .clip(RoundedCornerShape(8.dp))
                .background(RemoraTheme.codeBackground)
                .padding(10.dp),
        )
    }
}

// ── Dynamic Tool Call ────────────────────────────────────────────────────────

@Composable
internal fun DynamicToolCallRow(
    data: uniffi.codex_mobile_client.HydratedDynamicToolCallData,
) {
    val richPayload = remember(data.tool, data.contentSummary) {
        decodeRichDynamicToolPayload(data.tool, data.contentSummary)
    }
    if (richPayload != null) {
        RichDynamicToolResult(payload = richPayload)
        return
    }

    val display = data.display
    val summary = display?.summary?.takeIf { it.isNotBlank() }
        ?: data.namespace?.takeIf { it.isNotBlank() }?.let { "$it.${data.tool}" }
        ?: data.tool
    val metadata = buildList {
        display?.metadata?.forEach { entry ->
            add(entry.key to entry.value)
        }
        data.namespace?.takeIf { it.isNotBlank() }?.let { add("Namespace" to it) }
        data.success?.let { add("Success" to it.toString()) }
    }
    ToolCardShell(
        summary = summary,
        accent = RemoraTheme.toolCallMcpCall,
        status = data.status,
        durationMs = data.durationMs,
    ) {
        if (metadata.isNotEmpty()) {
            KeyValueSection(label = "Metadata", entries = metadata)
        }
        data.argumentsJson?.takeIf { it.isNotBlank() }?.let { CodeSection("Arguments", it) }
        data.contentSummary?.takeIf { it.isNotBlank() }?.let { InlineTextSection("Result", it) }
    }
}

// ── Web Search ───────────────────────────────────────────────────────────────

@Composable
internal fun WebSearchRow(
    data: uniffi.codex_mobile_client.HydratedWebSearchData,
) {
    ToolCardShell(
        summary = if (data.query.isBlank()) "Web search" else "Web search for ${data.query}",
        accent = RemoraTheme.toolCallWebSearch,
        status = if (data.isInProgress) AppOperationStatus.IN_PROGRESS else AppOperationStatus.COMPLETED,
    ) {
        if (data.query.isNotBlank()) {
            InlineTextSection("Query", data.query)
        }
        data.actionJson?.takeIf { it.isNotBlank() }?.let { CodeSection("Action", it) }
    }
}

@Composable
internal fun ImageViewRow(
    data: uniffi.codex_mobile_client.HydratedImageViewData,
    serverId: String,
) {
    ToolCardShell(
        summary = timelineWorkspaceTitle(data.path),
        accent = RemoraTheme.warning,
        status = AppOperationStatus.COMPLETED,
        defaultExpanded = true,
    ) {
        ImageResultSection(path = data.path, serverId = serverId)
        KeyValueSection("Metadata", listOf("Path" to data.path))
    }
}

@Composable
internal fun ImageGenerationRow(
    data: uniffi.codex_mobile_client.HydratedImageGenerationData,
) {
    val summary = when (data.status) {
        AppOperationStatus.COMPLETED -> "Generated image"
        AppOperationStatus.FAILED -> "Image generation failed"
        else -> "Generating image…"
    }
    ToolCardShell(
        summary = summary,
        accent = RemoraTheme.accent,
        status = data.status,
        defaultExpanded = true,
    ) {
        GeneratedImageSection(data = data)
        data.revisedPrompt?.takeIf { it.isNotBlank() }?.let { prompt ->
            RevisedPromptSection(prompt)
        }
        data.savedPath?.takeIf { it.isNotBlank() }?.let { path ->
            KeyValueSection("Metadata", listOf("Saved to" to path))
        }
    }
}

@Composable
private fun GeneratedImageSection(
    data: uniffi.codex_mobile_client.HydratedImageGenerationData,
) {
    val context = LocalContext.current
    val pngBytes = data.imagePng

    Column(verticalArrangement = Arrangement.spacedBy(6.dp)) {
        SectionLabel("Image")
        Box(
            modifier = Modifier
                .fillMaxWidth()
                .background(RemoraTheme.codeBackground, RoundedCornerShape(10.dp))
                .padding(horizontal = 10.dp, vertical = 8.dp),
            contentAlignment = Alignment.Center,
        ) {
            when {
                pngBytes != null -> {
                    AsyncImage(
                        model = ImageRequest.Builder(context)
                            .data(pngBytes)
                            .crossfade(false)
                            .build(),
                        contentDescription = "Generated image",
                        contentScale = ContentScale.Fit,
                        modifier = Modifier
                            .fillMaxWidth()
                            .heightIn(max = 360.dp)
                            .clip(RoundedCornerShape(8.dp)),
                    )
                }
                data.status == AppOperationStatus.IN_PROGRESS ||
                    data.status == AppOperationStatus.PENDING -> {
                    GeneratedImageLoadingTile()
                }
                else -> {
                    Text(
                        text = "Image unavailable",
                        color = RemoraTheme.textMuted,
                        fontSize = RemoraTextStyle.caption.scaled,
                        modifier = Modifier.padding(vertical = 20.dp),
                    )
                }
            }
        }
    }
}

@Composable
private fun GeneratedImageLoadingTile() {
    val transition = rememberInfiniteTransition(label = "image-generation-loading")
    val pulse by transition.animateFloat(
        initialValue = 0.35f,
        targetValue = 1f,
        animationSpec = infiniteRepeatable(
            animation = tween(durationMillis = 900),
            repeatMode = RepeatMode.Reverse,
        ),
        label = "image-generation-pulse",
    )

    Column(
        horizontalAlignment = Alignment.CenterHorizontally,
        verticalArrangement = Arrangement.spacedBy(12.dp),
        modifier = Modifier
            .fillMaxWidth()
            .padding(vertical = 20.dp),
    ) {
        Box(
            contentAlignment = Alignment.Center,
            modifier = Modifier
                .size(48.dp)
                .scale(0.98f + pulse * 0.05f)
                .background(
                    Brush.linearGradient(
                        colors = listOf(
                            RemoraTheme.accent.copy(alpha = 0.16f + pulse * 0.08f),
                            RemoraTheme.warning.copy(alpha = 0.10f),
                        ),
                    ),
                    RoundedCornerShape(12.dp),
                )
                .border(
                    0.5.dp,
                    RemoraTheme.accent.copy(alpha = 0.28f + pulse * 0.12f),
                    RoundedCornerShape(12.dp),
                ),
        ) {
            Icon(
                imageVector = Icons.Filled.HourglassEmpty,
                contentDescription = null,
                tint = RemoraTheme.accent,
                modifier = Modifier.size(22.dp),
            )
        }

        Text(
            text = "Generating image",
            color = RemoraTheme.textPrimary,
            fontSize = RemoraTextStyle.caption.scaled,
            fontWeight = FontWeight.SemiBold,
        )

        Row(
            horizontalArrangement = Arrangement.spacedBy(5.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            listOf(28.dp, 42.dp, 28.dp).forEachIndexed { index, width ->
                Box(
                    modifier = Modifier
                        .width(width)
                        .height(4.dp)
                        .alpha((0.38f + pulse * 0.62f - index * 0.14f).coerceIn(0.24f, 1f))
                        .background(
                            RemoraTheme.accent.copy(alpha = 0.42f),
                            RoundedCornerShape(999.dp),
                        ),
                )
            }
        }
    }
}

@Composable
private fun RevisedPromptSection(prompt: String) {
    var expanded by remember(prompt) { mutableStateOf(false) }
    val isLong = prompt.length > 220 || prompt.count { it == '\n' } >= 4
    val display = if (expanded || !isLong) prompt else prompt.take(220).trimEnd() + "…"

    Column(verticalArrangement = Arrangement.spacedBy(6.dp)) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            SectionLabel("Revised Prompt")
            Spacer(Modifier.weight(1f))
            if (isLong) {
                Text(
                    text = if (expanded) "Show less" else "Show more",
                    color = RemoraTheme.accent,
                    fontSize = RemoraTextStyle.caption2.scaled,
                    fontWeight = FontWeight.Medium,
                    modifier = Modifier.clickable { expanded = !expanded },
                )
            }
        }
        Text(
            text = display,
            color = RemoraTheme.textSecondary,
            fontSize = RemoraTextStyle.body.scaled,
            modifier = Modifier
                .fillMaxWidth()
                .background(RemoraTheme.codeBackground, RoundedCornerShape(8.dp))
                .padding(10.dp),
        )
    }
}

private sealed interface ToolImageLoadState {
    data object Loading : ToolImageLoadState
    data class Loaded(val bitmap: android.graphics.Bitmap) : ToolImageLoadState
    data class Failed(val message: String) : ToolImageLoadState
}

@Composable
private fun ImageResultSection(
    path: String,
    serverId: String,
) {
    val appModel = LocalAppModel.current
    val loadState by produceState<ToolImageLoadState>(
        initialValue = ToolImageLoadState.Loading,
        path,
        serverId,
    ) {
        value = ToolImageLoadState.Loading
        value = loadToolImage(appModel, path, serverId)
    }

    Column(verticalArrangement = Arrangement.spacedBy(6.dp)) {
        SectionLabel("Image")
        Box(
            modifier = Modifier
                .fillMaxWidth()
                .background(RemoraTheme.codeBackground, RoundedCornerShape(10.dp))
                .padding(horizontal = 10.dp, vertical = 8.dp),
            contentAlignment = Alignment.Center,
        ) {
            when (val state = loadState) {
                ToolImageLoadState.Loading -> {
                    CircularProgressIndicator(
                        color = RemoraTheme.accent,
                        strokeWidth = 2.dp,
                        modifier = Modifier.padding(vertical = 24.dp),
                    )
                }

                is ToolImageLoadState.Loaded -> {
                    Image(
                        bitmap = state.bitmap.asImageBitmap(),
                        contentDescription = timelineWorkspaceTitle(path),
                        contentScale = ContentScale.Fit,
                        modifier = Modifier
                            .fillMaxWidth()
                            .heightIn(max = 320.dp)
                            .clip(RoundedCornerShape(8.dp)),
                    )
                }

                is ToolImageLoadState.Failed -> {
                    Text(
                        text = state.message,
                        color = RemoraTheme.danger,
                        fontSize = RemoraTextStyle.caption.scaled,
                        modifier = Modifier.padding(vertical = 20.dp),
                    )
                }
            }
        }
    }
}

private suspend fun loadToolImage(
    appModel: AppModel,
    path: String,
    serverId: String,
): ToolImageLoadState {
    return try {
        val resolved = withContext(Dispatchers.IO) {
            appModel.client.resolveImageView(serverId, path)
        }
        val bitmap = BitmapFactory.decodeByteArray(resolved.bytes, 0, resolved.bytes.size)
        if (bitmap != null) {
            ToolImageLoadState.Loaded(bitmap)
        } else {
            ToolImageLoadState.Failed("Could not decode the image.")
        }
    } catch (error: Exception) {
        val message = error.message?.trim().orEmpty()
        ToolImageLoadState.Failed(
            if (message.isNotEmpty()) message else "Image unavailable",
        )
    }
}

@Composable
internal fun ToolCardShell(
    summary: String,
    summaryAnnotated: AnnotatedString? = null,
    accent: Color,
    status: AppOperationStatus,
    durationMs: Long? = null,
    defaultExpanded: Boolean = false,
    content: @Composable ColumnScope.() -> Unit,
) {
    var expanded by remember(summary, status) {
        mutableStateOf(defaultExpanded || status == AppOperationStatus.FAILED)
    }

    Column(
        modifier = Modifier
            .fillMaxWidth()
            .background(RemoraTheme.surface, RoundedCornerShape(12.dp))
            .border(0.5.dp, RemoraTheme.border, RoundedCornerShape(12.dp))
            .clickable { expanded = !expanded }
            .padding(horizontal = 12.dp, vertical = 9.dp),
    ) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            StatusIcon(status)
            Spacer(Modifier.width(8.dp))
            Text(
                text = summaryAnnotated ?: AnnotatedString(summary),
                color = RemoraTheme.textSystem,
                fontSize = RemoraTextStyle.body.scaled,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
                modifier = Modifier.weight(1f),
            )
            durationMs?.takeIf { it > 0 }?.let { ms ->
                Spacer(Modifier.width(8.dp))
                DurationChip(formatDuration(ms), statusTint(status))
            }
            Spacer(Modifier.width(8.dp))
            Text(
                text = if (expanded) "▲" else "▼",
                color = accent,
                fontSize = RemoraTextStyle.caption2.scaled,
                fontWeight = FontWeight.Bold,
            )
        }

        if (expanded) {
            Column(
                modifier = Modifier
                    .fillMaxWidth()
                    .padding(top = 6.dp),
                verticalArrangement = Arrangement.spacedBy(8.dp),
                content = content,
            )
        }
    }
}

private fun displayCommandText(command: String): String {
    val trimmed = command.trim()
    return if (trimmed.isEmpty()) "command" else trimmed
}

private fun collapseCommandText(command: String): String {
    val collapsed = displayCommandText(command)
        .replace(Regex("\\s+"), " ")
        .trim()
    return if (collapsed.isEmpty()) "command" else collapsed
}

@Composable
private fun SectionLabel(text: String) {
    Text(
        text = text.uppercase(),
        color = RemoraTheme.textSecondary,
        fontSize = RemoraTextStyle.caption2.scaled,
        fontWeight = FontWeight.Bold,
    )
}

@Composable
private fun CodeSection(
    label: String,
    content: String,
) {
    Column(verticalArrangement = Arrangement.spacedBy(6.dp)) {
        SectionLabel(label)
        LimitedToolTextBlock(content) { display ->
            Box(
                modifier = Modifier
                    .fillMaxWidth()
                    .background(RemoraTheme.codeBackground, RoundedCornerShape(8.dp))
                    .padding(10.dp),
            ) {
                Text(
                    text = display,
                    color = RemoraTheme.textBody,
                    fontFamily = RemoraTheme.monoFont,
                    fontSize = RemoraTextStyle.body.scaled,
                    modifier = Modifier.horizontalScroll(rememberScrollState()),
                )
            }
        }
    }
}

@Composable
private fun InlineTextSection(
    label: String,
    content: String,
    tone: Color = RemoraTheme.textBody,
) {
    Column(verticalArrangement = Arrangement.spacedBy(6.dp)) {
        SectionLabel(label)
        LimitedToolTextBlock(content) { display ->
            Text(
                text = display,
                color = tone,
                fontFamily = RemoraTheme.monoFont,
                fontSize = RemoraTextStyle.body.scaled,
                modifier = Modifier
                    .fillMaxWidth()
                    .background(RemoraTheme.codeBackground, RoundedCornerShape(8.dp))
                    .padding(horizontal = 10.dp, vertical = 8.dp),
            )
        }
    }
}

@Composable
private fun KeyValueSection(
    label: String,
    entries: List<Pair<String, String>>,
) {
    if (entries.isEmpty()) return
    Column(verticalArrangement = Arrangement.spacedBy(6.dp)) {
        SectionLabel(label)
        Column(
            modifier = Modifier
                .fillMaxWidth()
                .background(RemoraTheme.surface.copy(alpha = 0.6f), RoundedCornerShape(8.dp))
                .padding(8.dp),
            verticalArrangement = Arrangement.spacedBy(4.dp),
        ) {
            entries.forEach { (key, value) ->
                Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    Text(
                        text = "$key:",
                        color = RemoraTheme.textSecondary,
                        fontSize = RemoraTextStyle.body.scaled,
                        fontWeight = FontWeight.SemiBold,
                    )
                    Column(modifier = Modifier.weight(1f)) {
                        LimitedToolTextBlock(value) { display ->
                            Text(
                                text = display,
                                color = RemoraTheme.textSystem,
                                fontSize = RemoraTextStyle.body.scaled,
                            )
                        }
                    }
                }
            }
        }
    }
}

@Composable
private fun ListSection(
    label: String,
    items: List<String>,
) {
    if (items.isEmpty()) return
    Column(verticalArrangement = Arrangement.spacedBy(6.dp)) {
        SectionLabel(label)
        Column(
            modifier = Modifier
                .fillMaxWidth()
                .background(RemoraTheme.surface.copy(alpha = 0.6f), RoundedCornerShape(8.dp))
                .padding(8.dp),
            verticalArrangement = Arrangement.spacedBy(4.dp),
        ) {
            items.forEach { item ->
                Row(horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                    Text("•", color = RemoraTheme.textSecondary, fontSize = RemoraTextStyle.body.scaled)
                    Column(modifier = Modifier.weight(1f)) {
                        LimitedToolTextBlock(item) { display ->
                            Text(
                                text = display,
                                color = RemoraTheme.textSystem,
                                fontSize = RemoraTextStyle.body.scaled,
                            )
                        }
                    }
                }
            }
        }
    }
}

@Composable
private fun ProgressSection(
    label: String,
    items: List<String>,
) {
    if (items.isEmpty()) return
    Column(verticalArrangement = Arrangement.spacedBy(6.dp)) {
        SectionLabel(label)
        Column(
            modifier = Modifier
                .fillMaxWidth()
                .background(RemoraTheme.surface.copy(alpha = 0.6f), RoundedCornerShape(8.dp))
                .padding(8.dp),
            verticalArrangement = Arrangement.spacedBy(6.dp),
        ) {
            items.forEachIndexed { index, item ->
                Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    Text(
                        text = "•",
                        color = if (index == items.lastIndex) RemoraTheme.accentStrong else RemoraTheme.textMuted,
                        fontSize = RemoraTextStyle.body.scaled,
                    )
                    Column(modifier = Modifier.weight(1f)) {
                        LimitedToolTextBlock(item) { display ->
                            Text(
                                text = display,
                                color = RemoraTheme.textSystem,
                                fontSize = RemoraTextStyle.body.scaled,
                            )
                        }
                    }
                }
            }
        }
    }
}

@Composable
internal fun DiffSection(
    label: String,
    content: String,
) {
    Column(verticalArrangement = Arrangement.spacedBy(6.dp)) {
        if (label.isNotEmpty()) {
            SectionLabel(label)
        }
        LimitedToolTextBlock(content) { display ->
            SyntaxHighlightedDiffBlock(
                diff = display,
                titleHint = label.ifEmpty { null },
                fontSize = RemoraTextStyle.caption.sp,
                modifier = Modifier
                    .fillMaxWidth()
                    .background(RemoraTheme.codeBackground, RoundedCornerShape(8.dp))
                    .padding(horizontal = 10.dp, vertical = 6.dp),
            )
        }
    }
}

@Composable
private fun LimitedToolTextBlock(
    content: String,
    previewFromTail: Boolean = false,
    body: @Composable (String) -> Unit,
) {
    val isLong = content.length > ToolCallTextPreviewLimit
    var expanded by remember(content, previewFromTail) { mutableStateOf(false) }
    val display = remember(content, expanded, previewFromTail) {
        if (isLong && !expanded) {
            if (previewFromTail) content.takeLast(ToolCallTextPreviewLimit) else content.take(ToolCallTextPreviewLimit)
        } else {
            content
        }
    }

    body(display)

    if (isLong) {
        TextButton(onClick = { expanded = !expanded }) {
            Text(
                text = if (expanded) "Show less" else "Show more",
                color = RemoraTheme.accent,
                fontSize = RemoraTextStyle.caption2.scaled,
                fontWeight = FontWeight.SemiBold,
            )
        }
    }
}

@Composable
private fun RichDynamicToolResult(
    payload: RichDynamicToolPayload,
) {
    when (payload) {
        is RichDynamicToolPayload.Servers -> {
            Column(verticalArrangement = Arrangement.spacedBy(6.dp)) {
                payload.items.forEach { item ->
                    SessionServerCard(
                        icon = {
                            Icon(
                                if (item.isLocal) Icons.Default.PhoneAndroid else Icons.Default.Dns,
                                contentDescription = null,
                                tint = RemoraTheme.accent,
                                modifier = Modifier.size(18.dp),
                            )
                        },
                        title = item.name,
                        subtitle = item.hostname,
                        trailing = if (item.isConnected) "Connected" else "Offline",
                        statusDotColor = if (item.isConnected) RemoraTheme.success else RemoraTheme.textMuted,
                    )
                }
            }
        }
        is RichDynamicToolPayload.Sessions -> {
            Column(verticalArrangement = Arrangement.spacedBy(6.dp)) {
                payload.items.forEach { item ->
                    val subtitle = listOfNotNull(
                        item.serverName?.takeIf { it.isNotBlank() },
                        item.model?.takeIf { it.isNotBlank() },
                    ).joinToString(" \u00b7 ")
                    SessionServerCard(
                        icon = {
                            Icon(
                                Icons.AutoMirrored.Filled.Chat,
                                contentDescription = null,
                                tint = RemoraTheme.accent,
                                modifier = Modifier.size(18.dp),
                            )
                        },
                        title = item.title.ifBlank { "Untitled session" },
                        subtitle = subtitle,
                        trailing = null,
                        statusDotColor = null,
                    )
                }
            }
        }
    }
}

@Composable
private fun SessionServerCard(
    icon: @Composable () -> Unit,
    title: String,
    subtitle: String,
    trailing: String?,
    statusDotColor: Color?,
) {
    Row(
        modifier = Modifier
            .fillMaxWidth()
            .background(RemoraTheme.surface.copy(alpha = 0.6f), RoundedCornerShape(14.dp))
            .padding(horizontal = 14.dp, vertical = 12.dp),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(12.dp),
    ) {
        Box(
            modifier = Modifier
                .size(32.dp)
                .background(RemoraTheme.accent.copy(alpha = 0.12f), RoundedCornerShape(8.dp)),
            contentAlignment = Alignment.Center,
        ) {
            icon()
        }
        Column(modifier = Modifier.weight(1f)) {
            Text(
                text = title,
                color = RemoraTheme.textPrimary,
                fontSize = RemoraTextStyle.subheadline.scaled,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
            )
            if (subtitle.isNotBlank()) {
                Text(
                    text = subtitle,
                    color = RemoraTheme.textMuted,
                    fontSize = RemoraTextStyle.caption.scaled,
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                )
            }
        }
        if (statusDotColor != null || trailing != null) {
            Row(
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.spacedBy(6.dp),
            ) {
                statusDotColor?.let { dotColor ->
                    Box(
                        modifier = Modifier
                            .size(8.dp)
                            .clip(CircleShape)
                            .background(dotColor),
                    )
                }
                trailing?.let {
                    Text(
                        text = it,
                        color = RemoraTheme.textMuted,
                        fontSize = RemoraTextStyle.caption.scaled,
                    )
                }
            }
        }
    }
}

private sealed class RichDynamicToolPayload {
    data class Servers(val items: List<ServerItem>) : RichDynamicToolPayload()
    data class Sessions(val items: List<SessionItem>) : RichDynamicToolPayload()
}

private data class ServerItem(
    val name: String,
    val hostname: String,
    val isConnected: Boolean,
    val isLocal: Boolean,
)

private data class SessionItem(
    val title: String,
    val serverName: String?,
    val model: String?,
)

private fun decodeRichDynamicToolPayload(
    tool: String,
    contentSummary: String?,
): RichDynamicToolPayload? {
    if (contentSummary.isNullOrBlank()) return null
    if (tool != "list_servers" && tool != "list_sessions") return null
    return try {
        val root = JSONObject(contentSummary)
        when (root.optString("type")) {
            "servers" -> {
                val items = root.optJSONArray("items") ?: JSONArray()
                RichDynamicToolPayload.Servers(
                    List(items.length()) { index ->
                        val item = items.optJSONObject(index) ?: JSONObject()
                        ServerItem(
                            name = item.optString("name"),
                            hostname = item.optString("hostname"),
                            isConnected = item.optBoolean("isConnected"),
                            isLocal = item.optBoolean("isLocal"),
                        )
                    },
                )
            }
            "sessions" -> {
                val items = root.optJSONArray("items") ?: JSONArray()
                RichDynamicToolPayload.Sessions(
                    List(items.length()) { index ->
                        val item = items.optJSONObject(index) ?: JSONObject()
                        SessionItem(
                            title = item.optString("preview"),
                            serverName = item.optString("serverName").takeIf { it.isNotBlank() },
                            model = item.optString("modelProvider").ifBlank {
                                item.optString("model_provider")
                            }.takeIf { it.isNotBlank() },
                        )
                    },
                )
            }
            else -> null
        }
    } catch (_: Exception) {
        null
    }
}

// ── Shared Helpers ───────────────────────────────────────────────────────────

@Composable
internal fun StatusIcon(status: AppOperationStatus) {
    when (status) {
        AppOperationStatus.IN_PROGRESS -> {
            CircularProgressIndicator(
                modifier = Modifier.size(14.dp),
                strokeWidth = 2.dp,
                color = RemoraTheme.accent,
            )
        }
        AppOperationStatus.COMPLETED -> {
            Icon(
                Icons.Default.CheckCircle,
                contentDescription = "Completed",
                tint = RemoraTheme.success,
                modifier = Modifier.size(14.dp),
            )
        }
        AppOperationStatus.FAILED -> {
            Icon(
                Icons.Default.Error,
                contentDescription = "Failed",
                tint = RemoraTheme.danger,
                modifier = Modifier.size(14.dp),
            )
        }
        else -> {
            Icon(
                Icons.Default.HourglassEmpty,
                contentDescription = "Unknown",
                tint = RemoraTheme.textMuted,
                modifier = Modifier.size(14.dp),
            )
        }
    }
}

internal fun statusTint(status: AppOperationStatus): Color {
    return when (status) {
        AppOperationStatus.COMPLETED -> RemoraTheme.success
        AppOperationStatus.IN_PROGRESS -> RemoraTheme.warning
        AppOperationStatus.FAILED -> RemoraTheme.danger
        else -> RemoraTheme.textMuted
    }
}

@Composable
private fun DurationChip(text: String, tint: Color) {
    Box(
        modifier = Modifier
            .background(tint.copy(alpha = 0.10f), RoundedCornerShape(999.dp))
            .border(0.5.dp, tint.copy(alpha = 0.22f), RoundedCornerShape(999.dp))
            .padding(horizontal = 7.dp, vertical = 2.dp),
    ) {
        Text(
            text = text,
            color = tint,
            fontSize = RemoraTextStyle.caption2.scaled,
        )
    }
}

private fun formatDuration(ms: Long): String {
    return when {
        ms < 1000 -> "${ms}ms"
        ms < 60_000 -> "%.1fs".format(ms / 1000.0)
        else -> "${ms / 60_000}m ${(ms % 60_000) / 1000}s"
    }
}
