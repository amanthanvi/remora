package com.remora.android.ui.workflow

import com.remora.android.ui.Route
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import uniffi.codex_mobile_client.ThreadKey

class WorkflowActionsTest {
    private val first = ThreadKey("host", "one")
    private val second = ThreadKey("host", "two")

    private fun context(
        route: Route = Route.Conversation(first),
        canNavigateBack: Boolean = true,
        connectedServerCount: Int = 1,
        terminalEnabled: Boolean = true,
        orderedThreadKeys: List<ThreadKey> = listOf(first, second),
    ) = WorkflowActionContext(
        route = route,
        canNavigateBack = canNavigateBack,
        connectedServerCount = connectedServerCount,
        terminalEnabled = terminalEnabled,
        orderedThreadKeys = orderedThreadKeys,
    )

    @Test
    fun catalogUsesOneAvailabilityTableForPaletteAndShortcuts() {
        val disconnected = context(connectedServerCount = 0, terminalEnabled = false)

        val newThread = WorkflowActionCatalog.action(WorkflowActionId.NEW_THREAD, disconnected)
        val terminal = WorkflowActionCatalog.action(WorkflowActionId.OPEN_TERMINAL, disconnected)
        val review = WorkflowActionCatalog.action(WorkflowActionId.OPEN_REVIEW, disconnected)

        assertFalse(newThread.enabled)
        assertEquals("Connect a host before starting a thread", newThread.disabledReason)
        assertFalse(terminal.enabled)
        assertTrue(terminal.disabledReason!!.contains("Experimental"))
        assertFalse(review.enabled)
        assertTrue(review.disabledReason!!.contains("change summary"))
    }

    @Test
    fun neighboringThreadWrapsWithoutGrowingNavigationHistory() {
        assertEquals(second, neighboringThread(listOf(first, second), first, 1))
        assertEquals(first, neighboringThread(listOf(first, second), second, 1))
        assertEquals(second, neighboringThread(listOf(first, second), first, -1))
        assertNull(neighboringThread(listOf(first), first, 1))
    }

    @Test
    fun commandPaletteFilteringIncludesAliasesAndExcludesPaletteItself() {
        val actions = WorkflowActionCatalog.actions(context())

        assertEquals(
            listOf(WorkflowActionId.OPEN_TERMINAL),
            filterWorkflowActions(actions, "ghostty").map { it.definition.id },
        )
        assertFalse(
            filterWorkflowActions(actions, "")
                .any { it.definition.id == WorkflowActionId.SHOW_COMMAND_PALETTE },
        )
    }

    @Test
    fun paletteKeyboardSelectionSkipsDisabledActionsAndWraps() {
        val actions = WorkflowActionCatalog.actions(context())

        assertEquals(
            WorkflowActionId.HOME,
            selectedPaletteAction(actions, selected = null)?.definition?.id,
        )
        assertEquals(
            WorkflowActionId.OPEN_TERMINAL,
            paletteSelectionAfterMove(
                actions = actions,
                current = WorkflowActionId.NEW_THREAD,
                direction = 1,
            ),
        )
        assertEquals(
            WorkflowActionId.HOME,
            paletteSelectionAfterMove(
                actions = actions,
                current = WorkflowActionId.OPEN_SETTINGS,
                direction = 1,
            ),
        )
        assertEquals(
            WorkflowActionId.OPEN_SETTINGS,
            paletteSelectionAfterMove(
                actions = actions,
                current = WorkflowActionId.HOME,
                direction = -1,
            ),
        )
    }

    @Test
    fun globalShortcutsNeverRunWhileTerminalOrComposerOwnsInput() {
        val paletteStroke = WorkflowShortcutStroke(
            key = WorkflowHardwareKey.K,
            primary = true,
        )
        assertNull(actionForShortcut(paletteStroke, globalInputBlocked = true))
        assertEquals(
            WorkflowActionId.SHOW_COMMAND_PALETTE,
            actionForShortcut(paletteStroke, globalInputBlocked = false),
        )
        assertNull(
            actionForShortcut(
                WorkflowShortcutStroke(key = WorkflowHardwareKey.K),
                globalInputBlocked = false,
            ),
        )
    }
}
