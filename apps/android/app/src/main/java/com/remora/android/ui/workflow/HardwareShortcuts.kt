package com.remora.android.ui.workflow

import android.view.KeyEvent as AndroidKeyEvent
import androidx.compose.ui.input.key.KeyEvent
import androidx.compose.ui.input.key.KeyEventType
import androidx.compose.ui.input.key.isAltPressed
import androidx.compose.ui.input.key.isCtrlPressed
import androidx.compose.ui.input.key.isMetaPressed
import androidx.compose.ui.input.key.isShiftPressed
import androidx.compose.ui.input.key.type

internal enum class WorkflowHardwareKey {
    K,
    N,
    F,
    T,
    COMMA,
    LEFT,
    UP,
    DOWN,
    UNKNOWN,
}

internal data class WorkflowShortcutStroke(
    val key: WorkflowHardwareKey,
    val primary: Boolean = false,
    val alt: Boolean = false,
    val shift: Boolean = false,
)

internal fun actionForShortcut(
    stroke: WorkflowShortcutStroke,
    globalInputBlocked: Boolean,
): WorkflowActionId? {
    if (globalInputBlocked) return null
    return when {
        stroke.primary && !stroke.alt && !stroke.shift && stroke.key == WorkflowHardwareKey.K ->
            WorkflowActionId.SHOW_COMMAND_PALETTE
        stroke.primary && !stroke.alt && !stroke.shift && stroke.key == WorkflowHardwareKey.N ->
            WorkflowActionId.NEW_THREAD
        stroke.primary && !stroke.alt && !stroke.shift && stroke.key == WorkflowHardwareKey.F ->
            WorkflowActionId.SEARCH_THREADS
        stroke.primary && !stroke.alt && !stroke.shift && stroke.key == WorkflowHardwareKey.T ->
            WorkflowActionId.OPEN_TERMINAL
        stroke.primary && !stroke.alt && !stroke.shift && stroke.key == WorkflowHardwareKey.COMMA ->
            WorkflowActionId.OPEN_SETTINGS
        stroke.alt && !stroke.primary && !stroke.shift && stroke.key == WorkflowHardwareKey.LEFT ->
            WorkflowActionId.BACK
        stroke.alt && !stroke.primary && !stroke.shift && stroke.key == WorkflowHardwareKey.DOWN ->
            WorkflowActionId.NEXT_THREAD
        stroke.alt && !stroke.primary && !stroke.shift && stroke.key == WorkflowHardwareKey.UP ->
            WorkflowActionId.PREVIOUS_THREAD
        else -> null
    }
}

internal fun actionForKeyEvent(
    event: KeyEvent,
    globalInputBlocked: Boolean,
): WorkflowActionId? {
    if (event.type != KeyEventType.KeyDown) return null
    val key = when (event.nativeKeyEvent.keyCode) {
        AndroidKeyEvent.KEYCODE_K -> WorkflowHardwareKey.K
        AndroidKeyEvent.KEYCODE_N -> WorkflowHardwareKey.N
        AndroidKeyEvent.KEYCODE_F -> WorkflowHardwareKey.F
        AndroidKeyEvent.KEYCODE_T -> WorkflowHardwareKey.T
        AndroidKeyEvent.KEYCODE_COMMA -> WorkflowHardwareKey.COMMA
        AndroidKeyEvent.KEYCODE_DPAD_LEFT -> WorkflowHardwareKey.LEFT
        AndroidKeyEvent.KEYCODE_DPAD_UP -> WorkflowHardwareKey.UP
        AndroidKeyEvent.KEYCODE_DPAD_DOWN -> WorkflowHardwareKey.DOWN
        else -> WorkflowHardwareKey.UNKNOWN
    }
    return actionForShortcut(
        stroke = WorkflowShortcutStroke(
            key = key,
            primary = event.isCtrlPressed || event.isMetaPressed,
            alt = event.isAltPressed,
            shift = event.isShiftPressed,
        ),
        globalInputBlocked = globalInputBlocked,
    )
}
