package com.remora.android.ui.conversation

import androidx.compose.runtime.mutableStateOf
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.SemanticsProperties
import androidx.compose.ui.test.SemanticsMatcher
import androidx.compose.ui.test.assert
import androidx.compose.ui.test.assertIsNotSelected
import androidx.compose.ui.test.assertIsSelected
import androidx.compose.ui.test.assertTextContains
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.performTextInput
import androidx.test.ext.junit.runners.AndroidJUnit4
import org.junit.Assert.assertEquals
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import uniffi.codex_mobile_client.PendingUserInputAnswer
import uniffi.codex_mobile_client.PendingUserInputOption
import uniffi.codex_mobile_client.PendingUserInputQuestion
import uniffi.codex_mobile_client.PendingUserInputRequest

@RunWith(AndroidJUnit4::class)
class UserInputCardTest {
    @get:Rule
    val composeRule = createComposeRule()

    @Test
    fun selectionAndTextResetForEveryPartOfRequestIdentity() {
        val original = request()
        val current = mutableStateOf(original)
        var submitted = emptyList<PendingUserInputAnswer>()
        composeRule.setContent {
            UserInputCard(
                request = current.value,
                isSubmitting = false,
                onSubmit = { submitted = it },
            )
        }

        val replacements = listOf(
            original.copy(runtimeKind = "pi"),
            original.copy(serverId = "other-server"),
            original.copy(serverId = "other-server", threadId = "other-thread"),
            original.copy(serverId = "other-server", threadId = "other-thread", id = "other-request"),
        )
        for (replacement in replacements) {
            composeRule.onNodeWithText("First").performClick().assertIsSelected()
                .assert(SemanticsMatcher.expectValue(SemanticsProperties.Role, Role.RadioButton))
            composeRule.onNodeWithText("Second").assertIsNotSelected()
            composeRule.onNodeWithText("Details").performTextInput("draft")
            composeRule.onNodeWithText("Submit").performClick()
            composeRule.runOnIdle {
                assertEquals(
                    listOf(
                        PendingUserInputAnswer("choice", listOf("First")),
                        PendingUserInputAnswer("details", listOf("draft")),
                    ),
                    submitted,
                )
                current.value = replacement
            }
            composeRule.onNodeWithText("First").assertIsNotSelected()
            composeRule.onNodeWithText("Submit").performClick()
            composeRule.runOnIdle {
                assertEquals(
                    listOf(
                        PendingUserInputAnswer("choice", emptyList()),
                        PendingUserInputAnswer("details", emptyList()),
                    ),
                    submitted,
                )
            }
        }
    }

    @Test
    fun dismissalIsScopedToServerAndRuntime() {
        val request = request()
        val state = com.remora.android.ui.DismissedUserInputState()
        state.dismiss(request)
        org.junit.Assert.assertTrue(state.isDismissed(request))
        org.junit.Assert.assertFalse(state.isDismissed(request.copy(serverId = "other-server")))
        org.junit.Assert.assertFalse(state.isDismissed(request.copy(runtimeKind = "pi")))
    }

    @Test
    fun questionReorderingAndRemovalPreserveOnlyTheirOwnText() {
        val first = textQuestion("first", "First answer")
        val second = textQuestion("second", "Second answer")
        val current = mutableStateOf(request().copy(questions = listOf(first, second)))
        var submitted = emptyList<PendingUserInputAnswer>()
        composeRule.setContent {
            UserInputCard(
                request = current.value,
                isSubmitting = false,
                onSubmit = { submitted = it },
            )
        }
        composeRule.onNodeWithText("First answer").performTextInput("alpha")
        composeRule.onNodeWithText("Second answer").performTextInput("beta")
        composeRule.runOnIdle {
            current.value = current.value.copy(questions = listOf(second, first))
        }
        composeRule.onNodeWithText("First answer").assertTextContains("alpha")
        composeRule.onNodeWithText("Second answer").assertTextContains("beta")
        composeRule.runOnIdle {
            current.value = current.value.copy(questions = listOf(second))
        }
        composeRule.onNodeWithText("Second answer").assertTextContains("beta")
        composeRule.onNodeWithText("Submit").performClick()
        composeRule.runOnIdle {
            assertEquals(listOf(PendingUserInputAnswer("second", listOf("beta"))), submitted)
        }
    }

    private fun request() = PendingUserInputRequest(
        id = "request",
        serverId = "server",
        runtimeKind = "codex",
        threadId = "thread",
        turnId = "turn",
        itemId = "item",
        questions = listOf(
            textQuestion("choice", "Choice").copy(
                options = listOf(
                    PendingUserInputOption("First", null),
                    PendingUserInputOption("Second", null),
                ),
            ),
            textQuestion("details", "Details"),
        ),
        requesterAgentNickname = null,
        requesterAgentRole = null,
    )

    private fun textQuestion(id: String, header: String) = PendingUserInputQuestion(
        id = id,
        header = header,
        question = "Provide $id",
        isOtherAllowed = false,
        isSecret = false,
        options = emptyList(),
    )
}
