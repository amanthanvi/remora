package com.remora.android.ui.workflow

import androidx.compose.ui.input.key.Key
import androidx.compose.ui.test.ExperimentalTestApi
import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.hasSetTextAction
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performKeyInput
import androidx.compose.ui.test.pressKey
import androidx.test.ext.junit.runners.AndroidJUnit4
import com.remora.android.ui.Route
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import uniffi.codex_mobile_client.ThreadKey

@RunWith(AndroidJUnit4::class)
@OptIn(ExperimentalTestApi::class)
class CommandPaletteSheetTest {
    @get:Rule
    val composeRule = createComposeRule()

    private fun actions(): List<WorkflowAction> {
        val key = ThreadKey(serverId = "host", threadId = "thread")
        return WorkflowActionCatalog.actions(
            WorkflowActionContext(
                route = Route.Conversation(key),
                canNavigateBack = true,
                connectedServerCount = 1,
                terminalEnabled = true,
                orderedThreadKeys = listOf(key, ThreadKey("host", "other")),
            ),
        )
    }

    @Test
    fun arrowAndEnterExecuteTheSelectedAction() {
        val catalogActions = actions()
        var executed: WorkflowActionId? = null
        var dismissed = false

        composeRule.setContent {
            CommandPaletteSheet(
                actions = catalogActions,
                onExecute = { actionId ->
                    executed = actionId
                    null
                },
                onDismiss = { dismissed = true },
            )
        }
        composeRule.waitForIdle()

        composeRule.onNode(hasSetTextAction()).performKeyInput {
            pressKey(Key.DirectionDown)
            pressKey(Key.Enter)
        }

        composeRule.runOnIdle {
            assertEquals(WorkflowActionId.BACK, executed)
            assertTrue(dismissed)
        }
    }

    @Test
    fun upFromFirstWrapsToVisibleLastAction() {
        val catalogActions = actions()
        var executed: WorkflowActionId? = null

        composeRule.setContent {
            CommandPaletteSheet(
                actions = catalogActions,
                onExecute = { actionId ->
                    executed = actionId
                    null
                },
                onDismiss = {},
            )
        }
        composeRule.waitForIdle()

        composeRule.onNode(hasSetTextAction()).performKeyInput {
            pressKey(Key.DirectionUp)
        }
        composeRule.waitForIdle()
        composeRule.onNodeWithText("Settings").assertIsDisplayed()
        composeRule.onNode(hasSetTextAction()).performKeyInput {
            pressKey(Key.Enter)
        }

        composeRule.runOnIdle {
            assertEquals(WorkflowActionId.OPEN_SETTINGS, executed)
        }
    }
}
