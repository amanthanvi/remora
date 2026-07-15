package com.remora.android.ui.workflow

import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.BoxWithConstraints
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.statusBarsPadding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Add
import androidx.compose.material.icons.outlined.Home
import androidx.compose.material.icons.outlined.KeyboardCommandKey
import androidx.compose.material.icons.outlined.Search
import androidx.compose.material.icons.outlined.Settings
import androidx.compose.material.icons.outlined.Terminal
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.NavigationRail
import androidx.compose.material3.NavigationRailItem
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.OutlinedTextFieldDefaults
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.focus.focusRequester
import androidx.compose.ui.focus.onFocusChanged
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.role
import androidx.compose.ui.semantics.selected
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.stateDescription
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import com.remora.android.state.displayTitle
import com.remora.android.ui.RemoraTextStyle
import com.remora.android.ui.RemoraTheme
import com.remora.android.ui.Route
import com.remora.android.ui.scaled
import com.remora.android.ui.threadKeyOrNull
import uniffi.codex_mobile_client.AppSessionSummary
import uniffi.codex_mobile_client.ThreadKey

internal enum class AdaptiveNavigationMode {
    COMPACT,
    MEDIUM,
    EXPANDED,
}

internal data class AdaptiveNavigationMetrics(
    val mode: AdaptiveNavigationMode,
    val leadingPaneWidthDp: Int = 0,
)

internal fun adaptiveNavigationMetrics(
    availableWidthDp: Int,
    availableHeightDp: Int,
    fontScale: Float = 1f,
): AdaptiveNavigationMetrics = when {
    availableWidthDp < 600 || availableHeightDp < 480 ->
        AdaptiveNavigationMetrics(AdaptiveNavigationMode.COMPACT)
    availableWidthDp < 840 ||
        availableHeightDp < 600 ||
        (fontScale >= 1.5f && availableWidthDp < 841) ->
        AdaptiveNavigationMetrics(AdaptiveNavigationMode.MEDIUM)
    else -> AdaptiveNavigationMetrics(
        mode = AdaptiveNavigationMode.EXPANDED,
        leadingPaneWidthDp = if (availableWidthDp >= 1_100 || fontScale >= 1.5f) 360 else 320,
    )
}

@Composable
internal fun AdaptiveWorkflowScaffold(
    route: Route,
    sessions: List<AppSessionSummary>,
    actions: List<WorkflowAction>,
    terminalEnabled: Boolean,
    focusSearchRequest: Int,
    onHome: () -> Unit,
    onNewThread: () -> Unit,
    onSearch: () -> Unit,
    onShowPalette: () -> Unit,
    onOpenTerminal: () -> Unit,
    onShowSettings: () -> Unit,
    onOpenThread: (ThreadKey) -> Unit,
    onNavigationModeChanged: (AdaptiveNavigationMode) -> Unit,
    onInputFocusChanged: (Boolean) -> Unit,
    content: @Composable () -> Unit,
) {
    val enabledActionIds = remember(actions) {
        actions.asSequence().filter { it.enabled }.map { it.definition.id }.toSet()
    }
    BoxWithConstraints(modifier = Modifier.fillMaxSize()) {
        val fontScale = LocalDensity.current.fontScale
        val metrics = adaptiveNavigationMetrics(
            availableWidthDp = maxWidth.value.toInt(),
            availableHeightDp = maxHeight.value.toInt(),
            fontScale = fontScale,
        )
        LaunchedEffect(metrics.mode) {
            onNavigationModeChanged(metrics.mode)
        }
        val terminalOwnsInput = route is Route.Terminal
        when {
            terminalOwnsInput || metrics.mode == AdaptiveNavigationMode.COMPACT -> content()
            metrics.mode == AdaptiveNavigationMode.MEDIUM -> {
                Row(modifier = Modifier.fillMaxSize()) {
                    WorkflowNavigationRail(
                        route = route,
                        terminalEnabled = terminalEnabled,
                        enabledActionIds = enabledActionIds,
                        onHome = onHome,
                        onNewThread = onNewThread,
                        onSearch = onSearch,
                        onShowPalette = onShowPalette,
                        onOpenTerminal = onOpenTerminal,
                        onShowSettings = onShowSettings,
                    )
                    Box(
                        modifier = Modifier
                            .width(1.dp)
                            .fillMaxHeight()
                            .background(RemoraTheme.divider),
                    )
                    Box(modifier = Modifier.weight(1f)) { content() }
                }
            }
            route == Route.Home -> content()
            else -> {
                Row(modifier = Modifier.fillMaxSize()) {
                    ThreadNavigationPane(
                        sessions = sessions,
                        selectedThreadKey = route.threadKeyOrNull,
                        terminalEnabled = terminalEnabled,
                        enabledActionIds = enabledActionIds,
                        focusSearchRequest = focusSearchRequest,
                        onHome = onHome,
                        onNewThread = onNewThread,
                        onShowPalette = onShowPalette,
                        onOpenTerminal = onOpenTerminal,
                        onShowSettings = onShowSettings,
                        onOpenThread = onOpenThread,
                        onInputFocusChanged = onInputFocusChanged,
                        modifier = Modifier.width(metrics.leadingPaneWidthDp.dp),
                    )
                    Box(
                        modifier = Modifier
                            .width(1.dp)
                            .fillMaxHeight()
                            .background(RemoraTheme.divider),
                    )
                    Box(modifier = Modifier.weight(1f)) { content() }
                }
            }
        }
    }
}

@Composable
private fun WorkflowNavigationRail(
    route: Route,
    terminalEnabled: Boolean,
    enabledActionIds: Set<WorkflowActionId>,
    onHome: () -> Unit,
    onNewThread: () -> Unit,
    onSearch: () -> Unit,
    onShowPalette: () -> Unit,
    onOpenTerminal: () -> Unit,
    onShowSettings: () -> Unit,
) {
    NavigationRail(
        containerColor = RemoraTheme.surface,
        contentColor = RemoraTheme.textPrimary,
        modifier = Modifier
            .fillMaxHeight()
            .statusBarsPadding()
            .navigationBarsPadding(),
        header = {
            RailAction(
                icon = Icons.Default.Add,
                label = "New thread",
                enabled = WorkflowActionId.NEW_THREAD in enabledActionIds,
                onClick = onNewThread,
            )
            Spacer(Modifier.height(8.dp))
        },
    ) {
        NavigationRailItem(
            selected = route == Route.Home,
            enabled = WorkflowActionId.HOME in enabledActionIds,
            onClick = onHome,
            icon = { Icon(Icons.Outlined.Home, contentDescription = "Home") },
            colors = workflowNavigationRailItemColors(),
        )
        RailAction(
            icon = Icons.Outlined.Search,
            label = "Search threads",
            enabled = WorkflowActionId.SEARCH_THREADS in enabledActionIds,
            onClick = onSearch,
        )
        if (terminalEnabled) {
            RailAction(
                icon = Icons.Outlined.Terminal,
                label = "Open terminal",
                enabled = WorkflowActionId.OPEN_TERMINAL in enabledActionIds,
                onClick = onOpenTerminal,
            )
        }
        Spacer(Modifier.weight(1f))
        RailAction(Icons.Outlined.KeyboardCommandKey, "Command palette", true, onShowPalette)
        RailAction(Icons.Outlined.Settings, "Settings", true, onShowSettings)
    }
}

@Composable
private fun RailAction(
    icon: ImageVector,
    label: String,
    enabled: Boolean,
    onClick: () -> Unit,
) {
    NavigationRailItem(
        selected = false,
        enabled = enabled,
        onClick = onClick,
        icon = { Icon(icon, contentDescription = label) },
        colors = workflowNavigationRailItemColors(),
    )
}

@Composable
private fun workflowNavigationRailItemColors() =
    androidx.compose.material3.NavigationRailItemDefaults.colors(
        selectedIconColor = RemoraTheme.onAccentStrong,
        selectedTextColor = RemoraTheme.textPrimary,
        indicatorColor = RemoraTheme.accentStrong,
        unselectedIconColor = RemoraTheme.textSecondary,
        unselectedTextColor = RemoraTheme.textSecondary,
    )

@Composable
private fun ThreadNavigationPane(
    sessions: List<AppSessionSummary>,
    selectedThreadKey: ThreadKey?,
    terminalEnabled: Boolean,
    enabledActionIds: Set<WorkflowActionId>,
    focusSearchRequest: Int,
    onHome: () -> Unit,
    onNewThread: () -> Unit,
    onShowPalette: () -> Unit,
    onOpenTerminal: () -> Unit,
    onShowSettings: () -> Unit,
    onOpenThread: (ThreadKey) -> Unit,
    onInputFocusChanged: (Boolean) -> Unit,
    modifier: Modifier = Modifier,
) {
    var query by remember { mutableStateOf("") }
    val focusRequester = remember { FocusRequester() }
    LaunchedEffect(focusSearchRequest) {
        if (focusSearchRequest > 0) focusRequester.requestFocus()
    }
    DisposableEffect(Unit) {
        onDispose { onInputFocusChanged(false) }
    }
    val visibleSessions = remember(sessions, query) {
        val normalizedQuery = query.trim().lowercase()
        sessions
            .asSequence()
            .distinctBy { it.key.serverId to it.key.threadId }
            .filter { session ->
                normalizedQuery.isEmpty() ||
                    session.displayTitle.lowercase().contains(normalizedQuery) ||
                    session.cwd.lowercase().contains(normalizedQuery) ||
                    session.serverDisplayName.lowercase().contains(normalizedQuery)
            }
            .sortedByDescending { it.updatedAt ?: 0L }
            .take(50)
            .toList()
    }

    Column(
        modifier = modifier
            .fillMaxHeight()
            .background(RemoraTheme.surface)
            .statusBarsPadding()
            .navigationBarsPadding()
            .padding(horizontal = 12.dp),
    ) {
        Row(
            modifier = Modifier
                .fillMaxWidth()
                .padding(top = 10.dp, bottom = 8.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Box(
                modifier = Modifier
                    .weight(1f)
                    .height(48.dp)
                    .clickable(role = Role.Button, onClick = onHome),
                contentAlignment = Alignment.CenterStart,
            ) {
                Text(
                    text = "Remora",
                    color = RemoraTheme.textPrimary,
                    fontFamily = RemoraTheme.monoFont,
                    fontSize = 18.sp,
                    fontWeight = FontWeight.SemiBold,
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                )
            }
            PaneIconButton(
                icon = Icons.Default.Add,
                label = "New thread",
                enabled = WorkflowActionId.NEW_THREAD in enabledActionIds,
                onClick = onNewThread,
            )
            if (terminalEnabled) {
                PaneIconButton(
                    icon = Icons.Outlined.Terminal,
                    label = "Open terminal",
                    enabled = WorkflowActionId.OPEN_TERMINAL in enabledActionIds,
                    onClick = onOpenTerminal,
                )
            }
            PaneIconButton(Icons.Outlined.KeyboardCommandKey, "Command palette", true, onShowPalette)
            PaneIconButton(Icons.Outlined.Settings, "Settings", true, onShowSettings)
        }

        OutlinedTextField(
            value = query,
            onValueChange = { query = it },
            leadingIcon = {
                Icon(
                    Icons.Outlined.Search,
                    contentDescription = null,
                    tint = RemoraTheme.textSecondary,
                    modifier = Modifier.size(18.dp),
                )
            },
            placeholder = {
                Text(
                    text = "Search threads",
                    color = RemoraTheme.textSecondary,
                    fontSize = RemoraTextStyle.caption.scaled,
                )
            },
            singleLine = true,
            shape = RoundedCornerShape(10.dp),
            colors = OutlinedTextFieldDefaults.colors(
                focusedTextColor = RemoraTheme.textPrimary,
                unfocusedTextColor = RemoraTheme.textPrimary,
                cursorColor = RemoraTheme.accent,
                focusedBorderColor = RemoraTheme.accent,
                unfocusedBorderColor = RemoraTheme.border,
                focusedContainerColor = RemoraTheme.surfaceLight,
                unfocusedContainerColor = RemoraTheme.surfaceLight,
            ),
            modifier = Modifier
                .fillMaxWidth()
                .focusRequester(focusRequester)
                .onFocusChanged { onInputFocusChanged(it.isFocused) },
        )

        Text(
            text = "Threads",
            color = RemoraTheme.textSecondary,
            fontSize = RemoraTextStyle.caption.scaled,
            fontWeight = FontWeight.Medium,
            modifier = Modifier.padding(top = 14.dp, bottom = 6.dp),
        )
        HorizontalDivider(color = RemoraTheme.divider)

        if (visibleSessions.isEmpty()) {
            Text(
                text = if (query.isBlank()) "No threads yet" else "No matching threads",
                color = RemoraTheme.textSecondary,
                fontSize = RemoraTextStyle.footnote.scaled,
                modifier = Modifier.padding(vertical = 18.dp, horizontal = 4.dp),
            )
        } else {
            LazyColumn(
                modifier = Modifier.fillMaxSize(),
                verticalArrangement = Arrangement.spacedBy(2.dp),
            ) {
                items(
                    items = visibleSessions,
                    key = { "${it.key.serverId}/${it.key.threadId}" },
                ) { session ->
                    ThreadNavigationRow(
                        session = session,
                        selected = session.key == selectedThreadKey,
                        onClick = { onOpenThread(session.key) },
                    )
                }
            }
        }
    }
}

@Composable
private fun PaneIconButton(
    icon: ImageVector,
    label: String,
    enabled: Boolean,
    onClick: () -> Unit,
) {
    IconButton(enabled = enabled, onClick = onClick, modifier = Modifier.size(48.dp)) {
        Icon(
            imageVector = icon,
            contentDescription = label,
            tint = if (enabled) RemoraTheme.textSecondary else RemoraTheme.textMuted,
            modifier = Modifier.size(19.dp),
        )
    }
}

@Composable
private fun ThreadNavigationRow(
    session: AppSessionSummary,
    selected: Boolean,
    onClick: () -> Unit,
) {
    val shape = RoundedCornerShape(8.dp)
    val baseModifier = Modifier
        .fillMaxWidth()
        .semantics {
            this.selected = selected
            role = Role.Tab
            stateDescription = if (session.hasActiveTurn) "Active thread" else "Idle thread"
        }
        .background(
            color = if (selected) RemoraTheme.surfaceLight else Color.Transparent,
            shape = shape,
        )
        .then(
            if (selected) {
                Modifier.border(1.dp, RemoraTheme.accent.copy(alpha = 0.7f), shape)
            } else {
                Modifier
            },
        )
        .clickable(onClick = onClick)
        .padding(horizontal = 10.dp, vertical = 9.dp)

    Row(modifier = baseModifier, verticalAlignment = Alignment.Top) {
        Box(
            modifier = Modifier
                .padding(top = 5.dp, end = 9.dp)
                .size(7.dp)
                .background(
                    color = if (session.hasActiveTurn) RemoraTheme.success else RemoraTheme.textMuted,
                    shape = RoundedCornerShape(4.dp),
                ),
        )
        Column(modifier = Modifier.weight(1f)) {
            Text(
                text = session.displayTitle,
                color = RemoraTheme.textPrimary,
                fontSize = RemoraTextStyle.footnote.scaled,
                fontWeight = if (selected) FontWeight.SemiBold else FontWeight.Normal,
                maxLines = 2,
                overflow = TextOverflow.Ellipsis,
            )
            Text(
                text = buildString {
                    append(session.serverDisplayName)
                    session.cwd.takeIf { it.isNotBlank() }?.let { append(" · ${it.substringAfterLast('/')}") }
                },
                color = RemoraTheme.textSecondary,
                fontSize = RemoraTextStyle.caption2.scaled,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
            )
        }
    }
}
