package com.remora.android.ui.workflow

import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.outlined.ArrowBack
import androidx.compose.material.icons.filled.Add
import androidx.compose.material.icons.outlined.Code
import androidx.compose.material.icons.outlined.FolderOpen
import androidx.compose.material.icons.outlined.Home
import androidx.compose.material.icons.outlined.KeyboardCommandKey
import androidx.compose.material.icons.outlined.Search
import androidx.compose.material.icons.outlined.Settings
import androidx.compose.material.icons.outlined.SwapVert
import androidx.compose.material.icons.outlined.Terminal
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.OutlinedTextFieldDefaults
import androidx.compose.material3.Text
import androidx.compose.material3.rememberModalBottomSheetState
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.focus.focusRequester
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.input.key.Key
import androidx.compose.ui.input.key.KeyEventType
import androidx.compose.ui.input.key.key
import androidx.compose.ui.input.key.onPreviewKeyEvent
import androidx.compose.ui.input.key.type
import androidx.compose.ui.semantics.selected
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import com.remora.android.ui.RemoraTextStyle
import com.remora.android.ui.RemoraTheme
import com.remora.android.ui.scaled

internal fun filterWorkflowActions(
    actions: List<WorkflowAction>,
    query: String,
): List<WorkflowAction> {
    val normalized = query.trim().lowercase()
    return actions.filter { action ->
        action.definition.showsInPalette && (
            normalized.isEmpty() ||
                action.definition.title.lowercase().contains(normalized) ||
                action.definition.category.title.lowercase().contains(normalized) ||
                action.definition.searchTerms.any { it.lowercase().contains(normalized) }
            )
    }
}

internal fun paletteSelectionAfterMove(
    actions: List<WorkflowAction>,
    current: WorkflowActionId?,
    direction: Int,
): WorkflowActionId? {
    val selectable = actions.filter { it.definition.showsInPalette && it.enabled }
    if (selectable.isEmpty() || direction == 0) return current?.takeIf { selected ->
        selectable.any { it.definition.id == selected }
    }
    val currentIndex = selectable.indexOfFirst { it.definition.id == current }
    if (currentIndex < 0) {
        return if (direction > 0) selectable.first().definition.id else selectable.last().definition.id
    }
    val offset = if (direction > 0) 1 else -1
    val nextIndex = (currentIndex + offset + selectable.size) % selectable.size
    return selectable[nextIndex].definition.id
}

internal fun selectedPaletteAction(
    actions: List<WorkflowAction>,
    selected: WorkflowActionId?,
): WorkflowAction? = actions.firstOrNull {
    it.enabled && it.definition.showsInPalette && it.definition.id == selected
} ?: actions.firstOrNull { it.enabled && it.definition.showsInPalette }

@OptIn(ExperimentalMaterial3Api::class)
@Composable
internal fun CommandPaletteSheet(
    actions: List<WorkflowAction>,
    onExecute: (WorkflowActionId) -> String?,
    onDismiss: () -> Unit,
) {
    var query by remember { mutableStateOf("") }
    var executionError by remember { mutableStateOf<String?>(null) }
    var selectedActionId by remember { mutableStateOf<WorkflowActionId?>(null) }
    val focusRequester = remember { FocusRequester() }
    val filteredActions = remember(actions, query) { filterWorkflowActions(actions, query) }
    val groupedActions = remember(filteredActions) {
        WorkflowActionCategory.entries.mapNotNull { category ->
            filteredActions.filter { it.definition.category == category }
                .takeIf { it.isNotEmpty() }
                ?.let { category to it }
        }
    }
    val actionItemIndices = remember(groupedActions) {
        buildMap {
            var itemIndex = 0
            groupedActions.forEach { (_, categoryActions) ->
                itemIndex += 1 // Category header.
                categoryActions.forEach { action ->
                    put(action.definition.id, itemIndex)
                    itemIndex += 1
                }
            }
        }
    }
    val listState = rememberLazyListState()

    LaunchedEffect(Unit) {
        focusRequester.requestFocus()
    }
    LaunchedEffect(filteredActions.map { it.definition.id to it.enabled }) {
        selectedActionId = selectedPaletteAction(filteredActions, selectedActionId)?.definition?.id
    }
    LaunchedEffect(selectedActionId, actionItemIndices) {
        selectedActionId?.let(actionItemIndices::get)?.let { itemIndex ->
            listState.animateScrollToItem(itemIndex)
        }
    }

    fun execute(action: WorkflowAction) {
        executionError = onExecute(action.definition.id)
        if (executionError == null) onDismiss()
    }

    ModalBottomSheet(
        onDismissRequest = onDismiss,
        sheetState = rememberModalBottomSheetState(skipPartiallyExpanded = true),
        containerColor = RemoraTheme.surface,
        contentColor = RemoraTheme.textPrimary,
    ) {
        Column(
            modifier = Modifier
                .fillMaxWidth()
                .navigationBarsPadding()
                .padding(bottom = 12.dp),
        ) {
            Text(
                text = "Commands",
                color = RemoraTheme.textPrimary,
                fontFamily = RemoraTheme.monoFont,
                fontSize = 18.sp,
                fontWeight = FontWeight.SemiBold,
                modifier = Modifier.padding(horizontal = 20.dp, vertical = 8.dp),
            )
            OutlinedTextField(
                value = query,
                onValueChange = {
                    query = it
                    executionError = null
                    selectedActionId = null
                },
                leadingIcon = {
                    Icon(
                        imageVector = Icons.Outlined.Search,
                        contentDescription = null,
                        tint = RemoraTheme.textSecondary,
                        modifier = Modifier.size(18.dp),
                    )
                },
                placeholder = {
                    Text(
                        text = "Search actions",
                        color = RemoraTheme.textSecondary,
                        fontSize = RemoraTextStyle.footnote.scaled,
                    )
                },
                singleLine = true,
                shape = RoundedCornerShape(10.dp),
                colors = OutlinedTextFieldDefaults.colors(
                    focusedTextColor = RemoraTheme.textPrimary,
                    unfocusedTextColor = RemoraTheme.textPrimary,
                    focusedBorderColor = RemoraTheme.accent,
                    unfocusedBorderColor = RemoraTheme.border,
                    focusedContainerColor = RemoraTheme.surfaceLight,
                    unfocusedContainerColor = RemoraTheme.surfaceLight,
                    cursorColor = RemoraTheme.accent,
                ),
                modifier = Modifier
                    .fillMaxWidth()
                    .padding(horizontal = 16.dp)
                    .focusRequester(focusRequester)
                    .onPreviewKeyEvent { event ->
                        if (event.type != KeyEventType.KeyDown) return@onPreviewKeyEvent false
                        when (event.key) {
                            Key.DirectionDown -> {
                                selectedActionId = paletteSelectionAfterMove(
                                    actions = filteredActions,
                                    current = selectedActionId,
                                    direction = 1,
                                )
                                true
                            }
                            Key.DirectionUp -> {
                                selectedActionId = paletteSelectionAfterMove(
                                    actions = filteredActions,
                                    current = selectedActionId,
                                    direction = -1,
                                )
                                true
                            }
                            Key.Enter,
                            Key.NumPadEnter,
                            -> {
                                selectedPaletteAction(filteredActions, selectedActionId)?.let(::execute) != null
                            }
                            Key.Escape -> {
                                onDismiss()
                                true
                            }
                            else -> false
                        }
                    },
            )

            executionError?.let { message ->
                Text(
                    text = message,
                    color = RemoraTheme.danger,
                    fontSize = RemoraTextStyle.caption.scaled,
                    modifier = Modifier.padding(horizontal = 20.dp, vertical = 8.dp),
                )
            }

            if (filteredActions.isEmpty()) {
                Text(
                    text = "No matching actions",
                    color = RemoraTheme.textSecondary,
                    fontSize = RemoraTextStyle.footnote.scaled,
                    modifier = Modifier.padding(horizontal = 20.dp, vertical = 24.dp),
                )
            } else {
                LazyColumn(
                    state = listState,
                    modifier = Modifier
                        .fillMaxWidth()
                        .heightIn(max = 420.dp),
                ) {
                    groupedActions.forEach { (category, categoryActions) ->
                        item(key = "category-${category.name}") {
                            Text(
                                text = category.title,
                                color = RemoraTheme.textSecondary,
                                fontSize = RemoraTextStyle.caption.scaled,
                                fontWeight = FontWeight.Medium,
                                modifier = Modifier.padding(
                                    start = 20.dp,
                                    end = 20.dp,
                                    top = 16.dp,
                                    bottom = 6.dp,
                                ),
                            )
                            HorizontalDivider(
                                color = RemoraTheme.divider,
                                modifier = Modifier.padding(horizontal = 16.dp),
                            )
                        }
                        items(
                            items = categoryActions,
                            key = { action -> action.definition.id.name },
                        ) { action ->
                            CommandPaletteRow(
                                action = action,
                                selected = action.definition.id == selectedActionId,
                                onClick = { execute(action) },
                            )
                        }
                    }
                }
            }
        }
    }
}

@Composable
private fun CommandPaletteRow(
    action: WorkflowAction,
    selected: Boolean,
    onClick: () -> Unit,
) {
    Row(
        modifier = Modifier
            .fillMaxWidth()
            .semantics { this.selected = selected }
            .background(
                if (selected) RemoraTheme.accent.copy(alpha = 0.12f) else RemoraTheme.surface,
            )
            .clickable(enabled = action.enabled, onClick = onClick)
            .padding(horizontal = 20.dp, vertical = 11.dp),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(12.dp),
    ) {
        Icon(
            imageVector = actionIcon(action.definition.id),
            contentDescription = null,
            tint = when {
                !action.enabled -> RemoraTheme.textMuted
                selected -> RemoraTheme.accent
                else -> RemoraTheme.textPrimary
            },
            modifier = Modifier.size(20.dp),
        )
        Column(modifier = Modifier.weight(1f)) {
            Text(
                text = action.definition.title,
                color = if (action.enabled) RemoraTheme.textPrimary else RemoraTheme.textMuted,
                fontSize = RemoraTextStyle.footnote.scaled,
                fontWeight = FontWeight.Medium,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
            )
            action.disabledReason?.let { reason ->
                Text(
                    text = reason,
                    color = RemoraTheme.textSecondary,
                    fontSize = RemoraTextStyle.caption2.scaled,
                    maxLines = 2,
                    overflow = TextOverflow.Ellipsis,
                )
            }
        }
        action.definition.shortcut?.let { shortcut ->
            Text(
                text = shortcut.displayLabel,
                color = RemoraTheme.textSecondary,
                fontSize = RemoraTextStyle.caption2.scaled,
                fontFamily = RemoraTheme.monoFont,
                modifier = Modifier
                    .background(RemoraTheme.surfaceLight, RoundedCornerShape(6.dp))
                    .padding(horizontal = 7.dp, vertical = 4.dp),
            )
        }
    }
}

private fun actionIcon(id: WorkflowActionId): ImageVector = when (id) {
    WorkflowActionId.HOME -> Icons.Outlined.Home
    WorkflowActionId.BACK -> Icons.AutoMirrored.Outlined.ArrowBack
    WorkflowActionId.SEARCH_THREADS -> Icons.Outlined.Search
    WorkflowActionId.NEXT_THREAD,
    WorkflowActionId.PREVIOUS_THREAD,
    -> Icons.Outlined.SwapVert
    WorkflowActionId.NEW_THREAD -> Icons.Default.Add
    WorkflowActionId.OPEN_REVIEW -> Icons.Outlined.Code
    WorkflowActionId.OPEN_FILES -> Icons.Outlined.FolderOpen
    WorkflowActionId.OPEN_TERMINAL -> Icons.Outlined.Terminal
    WorkflowActionId.OPEN_SETTINGS -> Icons.Outlined.Settings
    WorkflowActionId.SHOW_COMMAND_PALETTE -> Icons.Outlined.KeyboardCommandKey
}
