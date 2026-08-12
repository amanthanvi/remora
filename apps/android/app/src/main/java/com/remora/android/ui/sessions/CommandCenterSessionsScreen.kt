package com.remora.android.ui.sessions

import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.LazyRow
import androidx.compose.foundation.lazy.itemsIndexed
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.filled.Archive
import androidx.compose.material.icons.filled.CheckCircle
import androidx.compose.material.icons.filled.Close
import androidx.compose.material.icons.filled.MoreVert
import androidx.compose.material.icons.filled.Search
import androidx.compose.material.icons.filled.Snooze
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.FilterChip
import androidx.compose.material3.FilterChipDefaults
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import com.remora.android.ui.LocalAppModel
import com.remora.android.ui.RemoraTheme
import com.remora.android.ui.home.HomeDashboardSupport
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import uniffi.codex_mobile_client.AppArchiveThreadRequest
import uniffi.codex_mobile_client.AvailabilityState
import uniffi.codex_mobile_client.SessionAttentionV1
import uniffi.codex_mobile_client.SessionFilterV1
import uniffi.codex_mobile_client.SessionListRowV1
import uniffi.codex_mobile_client.SessionStatusV1
import uniffi.codex_mobile_client.ThreadKey

private enum class CommandCenterSessionFilter(
    val title: String,
    val status: SessionStatusV1? = null,
    val attention: SessionAttentionV1? = null,
) {
    ALL("All"),
    NEEDS_YOU("Needs You", attention = SessionAttentionV1.NEEDS_YOU),
    ACTIVE("Active", status = SessionStatusV1.RUNNING),
    FAILED("Failed", status = SessionStatusV1.FAILED),
}

@Composable
fun CommandCenterSessionsScreen(
    serverId: String?,
    onOpenConversation: (ThreadKey) -> Unit,
    onBack: () -> Unit,
) {
    val appModel = LocalAppModel.current
    val scope = rememberCoroutineScope()
    var query by remember { mutableStateOf("") }
    var selectedFilter by remember { mutableStateOf(CommandCenterSessionFilter.ALL) }
    var rows by remember { mutableStateOf<List<SessionListRowV1>>(emptyList()) }
    var nextCursor by remember { mutableStateOf<String?>(null) }
    var totalCount by remember { mutableStateOf(0u) }
    var isLoading by remember { mutableStateOf(false) }
    var errorMessage by remember { mutableStateOf<String?>(null) }
    var mutatingKeys by remember { mutableStateOf<Set<ThreadKey>>(emptySet()) }
    var menuRow by remember { mutableStateOf<SessionListRowV1?>(null) }
    var pendingArchive by remember { mutableStateOf<SessionListRowV1?>(null) }
    var actionError by remember { mutableStateOf<String?>(null) }

    suspend fun loadPage(reset: Boolean) {
        if (isLoading && !reset) return
        isLoading = true
        errorMessage = null
        if (reset) {
            rows = emptyList()
            nextCursor = null
            totalCount = 0u
        }
        try {
            val filter = SessionFilterV1(
                query = query,
                serverId = serverId,
                projectLabel = null,
                runtimeId = null,
                status = selectedFilter.status,
                attention = selectedFilter.attention,
                updatedAfterMs = null,
            )
            val cursor = nextCursor
            val page = withContext(Dispatchers.Default) {
                appModel.store.sessionsPageFiltered(
                    filter = filter,
                    cursor = cursor,
                    limit = 50u,
                )
            }
            val existing = rows.mapTo(mutableSetOf()) { it.key }
            rows = rows + page.rows.filterNot { it.key in existing }
            nextCursor = page.nextCursor
            totalCount = page.totalCount
        } catch (error: Exception) {
            errorMessage = error.message ?: "Unable to load sessions"
        } finally {
            isLoading = false
        }
    }

    suspend fun mutateAttention(row: SessionListRowV1, action: () -> Unit) {
        if (row.key in mutatingKeys) return
        mutatingKeys = mutatingKeys + row.key
        try {
            withContext(Dispatchers.Default) { action() }
            loadPage(reset = true)
        } catch (error: Exception) {
            actionError = error.message ?: "Unable to update this session"
        } finally {
            mutatingKeys = mutatingKeys - row.key
        }
    }

    suspend fun archive(row: SessionListRowV1) {
        if (row.key in mutatingKeys) return
        mutatingKeys = mutatingKeys + row.key
        try {
            if (appModel.snapshot.value?.activeThread == row.key) {
                appModel.activateThread(null)
            }
            appModel.client.archiveThread(
                row.key.serverId,
                AppArchiveThreadRequest(threadId = row.key.threadId),
            )
            appModel.refreshSnapshot()
            loadPage(reset = true)
        } catch (error: Exception) {
            actionError = error.message ?: "Unable to archive this session"
        } finally {
            mutatingKeys = mutatingKeys - row.key
        }
    }

    LaunchedEffect(serverId, query, selectedFilter) {
        if (query.isNotBlank()) delay(250)
        loadPage(reset = true)
    }

    Column(
        modifier = Modifier
            .fillMaxSize()
            .background(RemoraTheme.background),
    ) {
        Row(
            modifier = Modifier
                .fillMaxWidth()
                .padding(horizontal = 8.dp, vertical = 6.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            IconButton(onClick = onBack, modifier = Modifier.size(RemoraTheme.minimumTouchTarget)) {
                Icon(
                    Icons.AutoMirrored.Filled.ArrowBack,
                    contentDescription = "Back",
                    tint = RemoraTheme.textPrimary,
                )
            }
            Text(
                text = "Sessions",
                color = RemoraTheme.textPrimary,
                fontSize = 16.sp,
                fontWeight = FontWeight.SemiBold,
                modifier = Modifier.weight(1f),
            )
            Text(
                text = "${rows.size}/$totalCount",
                color = RemoraTheme.textMuted,
                fontFamily = FontFamily.Monospace,
                fontSize = 11.sp,
            )
        }

        OutlinedTextField(
            value = query,
            onValueChange = { query = it },
            singleLine = true,
            placeholder = { Text("Search sessions") },
            leadingIcon = {
                Icon(Icons.Default.Search, contentDescription = null)
            },
            trailingIcon = if (query.isEmpty()) null else {
                {
                    IconButton(onClick = { query = "" }) {
                        Icon(Icons.Default.Close, contentDescription = "Clear search")
                    }
                }
            },
            modifier = Modifier
                .fillMaxWidth()
                .padding(horizontal = 14.dp),
        )

        LazyRow(
            contentPadding = androidx.compose.foundation.layout.PaddingValues(
                horizontal = 14.dp,
                vertical = 8.dp,
            ),
            horizontalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            itemsIndexed(CommandCenterSessionFilter.entries) { _, filter ->
                val selected = selectedFilter == filter
                FilterChip(
                    selected = selected,
                    onClick = { selectedFilter = filter },
                    label = { Text(filter.title, fontSize = 11.sp) },
                    modifier = Modifier.heightIn(min = RemoraTheme.minimumTouchTarget),
                    colors = FilterChipDefaults.filterChipColors(
                        selectedContainerColor = RemoraTheme.accent,
                        selectedLabelColor = RemoraTheme.onAccentStrong,
                    ),
                )
            }
        }
        HorizontalDivider(color = RemoraTheme.border.copy(alpha = 0.25f))

        when {
            rows.isEmpty() && isLoading -> LoadingSessions()
            rows.isEmpty() -> EmptySessions(errorMessage)
            else -> LazyColumn(modifier = Modifier.fillMaxSize()) {
                itemsIndexed(
                    items = rows,
                    key = { _, row -> "${row.key.serverId}/${row.key.threadId}" },
                ) { index, row ->
                    CommandCenterSessionRow(
                        row = row,
                        onOpen = { onOpenConversation(row.key) },
                        menuExpanded = menuRow?.key == row.key,
                        enabled = row.key !in mutatingKeys,
                        onShowMenu = { menuRow = row },
                        onDismissMenu = { menuRow = null },
                        onAcknowledge = if (row.canAcknowledge) {
                            {
                                menuRow = null
                                scope.launch {
                                    mutateAttention(row) {
                                        appModel.store.acknowledgeThreadAttention(row.key)
                                    }
                                }
                            }
                        } else {
                            null
                        },
                        onSnooze = if (row.attention == SessionAttentionV1.NEEDS_YOU) {
                            {
                                menuRow = null
                                scope.launch {
                                    mutateAttention(row) {
                                        appModel.store.snoozeThreadAttention(row.key)
                                    }
                                }
                            }
                        } else {
                            null
                        },
                        onArchive = if (
                            row.archiveAvailability.state == AvailabilityState.AVAILABLE
                        ) {
                            {
                                menuRow = null
                                pendingArchive = row
                            }
                        } else {
                            null
                        },
                    )
                    HorizontalDivider(color = RemoraTheme.border.copy(alpha = 0.16f))
                    if (index == rows.lastIndex && nextCursor != null) {
                        LaunchedEffect(nextCursor) { loadPage(reset = false) }
                    }
                }
                if (isLoading) {
                    item(key = "loading") { LoadingSessions(compact = true) }
                } else if (errorMessage != null) {
                    item(key = "retry") {
                        TextButton(
                            onClick = { scope.launch { loadPage(reset = false) } },
                            modifier = Modifier.fillMaxWidth(),
                        ) {
                            Text("Retry", color = RemoraTheme.warning)
                        }
                    }
                }
            }
        }
    }

    pendingArchive?.let { row ->
        AlertDialog(
            onDismissRequest = { pendingArchive = null },
            title = { Text("Archive Session?") },
            text = { Text("This moves the session into archived history on its Host.") },
            confirmButton = {
                TextButton(onClick = {
                    pendingArchive = null
                    scope.launch { archive(row) }
                }) {
                    Text("Archive", color = RemoraTheme.danger)
                }
            },
            dismissButton = {
                TextButton(onClick = { pendingArchive = null }) { Text("Cancel") }
            },
        )
    }
    actionError?.let { message ->
        AlertDialog(
            onDismissRequest = { actionError = null },
            title = { Text("Session Action Failed") },
            text = { Text(message) },
            confirmButton = {
                TextButton(onClick = { actionError = null }) { Text("OK") }
            },
        )
    }
}

@Composable
private fun LoadingSessions(compact: Boolean = false) {
    Box(
        modifier = Modifier
            .fillMaxWidth()
            .then(if (compact) Modifier.padding(16.dp) else Modifier.fillMaxSize()),
        contentAlignment = Alignment.Center,
    ) {
        CircularProgressIndicator(
            color = RemoraTheme.accent,
            strokeWidth = 2.dp,
            modifier = Modifier.size(if (compact) 18.dp else 28.dp),
        )
    }
}

@Composable
private fun EmptySessions(errorMessage: String?) {
    Box(
        modifier = Modifier.fillMaxSize().padding(24.dp),
        contentAlignment = Alignment.Center,
    ) {
        Text(
            text = errorMessage ?: "No sessions match these filters.",
            color = if (errorMessage == null) RemoraTheme.textMuted else RemoraTheme.warning,
            fontSize = 13.sp,
        )
    }
}

@Composable
private fun CommandCenterSessionRow(
    row: SessionListRowV1,
    onOpen: () -> Unit,
    menuExpanded: Boolean,
    enabled: Boolean,
    onShowMenu: () -> Unit,
    onDismissMenu: () -> Unit,
    onAcknowledge: (() -> Unit)?,
    onSnooze: (() -> Unit)?,
    onArchive: (() -> Unit)?,
) {
    val hasActions = onAcknowledge != null || onSnooze != null || onArchive != null
    Column(
        modifier = Modifier
            .fillMaxWidth()
            .clickable(enabled = enabled, onClick = onOpen)
            .padding(horizontal = 16.dp, vertical = 12.dp),
        verticalArrangement = Arrangement.spacedBy(6.dp),
    ) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            Text(
                text = row.title,
                color = RemoraTheme.textPrimary,
                fontSize = 14.sp,
                fontWeight = FontWeight.SemiBold,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
                modifier = Modifier.weight(1f),
            )
            Spacer(Modifier.width(8.dp))
            SessionStatusLabel(row)
            if (hasActions) Box {
                IconButton(
                    onClick = onShowMenu,
                    enabled = enabled,
                    modifier = Modifier.size(RemoraTheme.minimumTouchTarget),
                ) {
                    Icon(
                        Icons.Default.MoreVert,
                        contentDescription = "Session actions",
                        tint = RemoraTheme.textMuted,
                    )
                }
                DropdownMenu(
                    expanded = menuExpanded,
                    onDismissRequest = onDismissMenu,
                ) {
                    onAcknowledge?.let { action ->
                        DropdownMenuItem(
                            text = { Text("Acknowledge") },
                            leadingIcon = {
                                Icon(Icons.Default.CheckCircle, contentDescription = null)
                            },
                            onClick = action,
                        )
                    }
                    onSnooze?.let { action ->
                        DropdownMenuItem(
                            text = { Text("Snooze 1 hour") },
                            leadingIcon = {
                                Icon(Icons.Default.Snooze, contentDescription = null)
                            },
                            onClick = action,
                        )
                    }
                    onArchive?.let { action ->
                        DropdownMenuItem(
                            text = { Text("Archive", color = RemoraTheme.danger) },
                            leadingIcon = {
                                Icon(
                                    Icons.Default.Archive,
                                    contentDescription = null,
                                    tint = RemoraTheme.danger,
                                )
                            },
                            onClick = action,
                        )
                    }
                }
            }
        }
        row.preview?.takeIf { it.isNotBlank() }?.let { preview ->
            Text(
                text = preview,
                color = RemoraTheme.textSecondary,
                fontSize = 12.sp,
                maxLines = 2,
                overflow = TextOverflow.Ellipsis,
            )
        }
        Text(
            text = buildList {
                add(row.hostLabel)
                row.projectLabel?.let(::add)
                add(row.runtimeId)
                row.updatedAtMs?.let { add(HomeDashboardSupport.relativeTime(it / 1_000L)) }
            }.joinToString(" · "),
            color = RemoraTheme.textMuted,
            fontFamily = FontFamily.Monospace,
            fontSize = 10.sp,
            maxLines = 1,
            overflow = TextOverflow.Ellipsis,
        )
    }
}

@Composable
private fun SessionStatusLabel(row: SessionListRowV1) {
    val needsYou = row.attention == SessionAttentionV1.NEEDS_YOU
    val snoozed = row.snoozedUntilMs != null
    val label = if (needsYou) "Needs You" else if (snoozed) "Snoozed" else when (row.status) {
        SessionStatusV1.RUNNING -> "Running"
        SessionStatusV1.WAITING -> "Waiting"
        SessionStatusV1.FAILED -> "Failed"
        SessionStatusV1.IDLE -> "Idle"
        SessionStatusV1.UNKNOWN -> "Unknown"
    }
    val color: Color = if (needsYou) {
        RemoraTheme.warning
    } else if (snoozed) {
        RemoraTheme.accent
    } else when (row.status) {
        SessionStatusV1.RUNNING -> RemoraTheme.accent
        SessionStatusV1.WAITING -> RemoraTheme.warning
        SessionStatusV1.FAILED -> RemoraTheme.danger
        SessionStatusV1.IDLE -> RemoraTheme.textSecondary
        SessionStatusV1.UNKNOWN -> RemoraTheme.textMuted
    }
    Text(
        text = label,
        color = color,
        fontFamily = FontFamily.Monospace,
        fontSize = 10.sp,
        fontWeight = FontWeight.SemiBold,
        modifier = Modifier
            .background(color.copy(alpha = 0.12f), RoundedCornerShape(999.dp))
            .padding(horizontal = 7.dp, vertical = 3.dp),
    )
}
