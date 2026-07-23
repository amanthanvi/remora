package com.remora.android.ui.settings

import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.width
import androidx.compose.material3.Button
import androidx.compose.material3.Text
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onNodeWithContentDescription
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import androidx.compose.ui.unit.Density
import androidx.compose.ui.unit.dp
import com.remora.android.ui.RemoraAppTheme
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import uniffi.codex_mobile_client.AppRemoraLinkForgetResult
import uniffi.codex_mobile_client.AppRemoraLinkHostState
import uniffi.codex_mobile_client.AppRemoraLinkHostSummary
import uniffi.codex_mobile_client.AppRemoraLinkPendingApproval
import uniffi.codex_mobile_client.AppRemoraLinkPendingRestart
import uniffi.codex_mobile_client.AppRemoraLinkRestartOutcome
import uniffi.codex_mobile_client.AppRemoraLinkRevocationOutcome
import uniffi.codex_mobile_client.AppRemoraLinkScope

class RemoraLinkHostSettingsAccessibilityTest {
    @get:Rule
    val composeRule = createComposeRule()

    @Test
    fun titleKeepsNavigationSpaceAtTwoHundredPercentFontScale() {
        composeRule.setContent {
            val displayDensity = LocalDensity.current.density
            CompositionLocalProvider(LocalDensity provides Density(displayDensity, fontScale = 2f)) {
                RemoraAppTheme {
                    Box(Modifier.width(320.dp)) {
                        RemoraLinkHostsTitle(onBack = {})
                    }
                }
            }
        }

        composeRule.onNodeWithText("Remora Link Hosts").assertIsDisplayed()
        val backBounds = composeRule
            .onNodeWithContentDescription("Back to Settings")
            .fetchSemanticsNode()
            .boundsInRoot
        val titleBounds = composeRule
            .onNodeWithText("Remora Link Hosts")
            .fetchSemanticsNode()
            .boundsInRoot

        assertTrue("title must begin after the back target", titleBounds.left >= backBounds.right)
        assertTrue("large text must be allowed to wrap", titleBounds.height > 34f)
    }

    @Test
    fun pendingHostOffersContinueApprovalAction() {
        var continued = false
        composeRule.setContent {
            RemoraAppTheme {
                RemoraLinkHostRow(
                    host = AppRemoraLinkHostSummary(
                        hostId = "host-1",
                        hostDisplayName = "Office Mac",
                        state = AppRemoraLinkHostState.AWAITING_HOST_APPROVAL,
                        selectedRuntimeIds = listOf("codex"),
                        grantedScopes = listOf(AppRemoraLinkScope.CONNECT_RUNTIME),
                        pendingApproval = AppRemoraLinkPendingApproval(
                            sas = "123 456",
                            expiresAtUnixMs = 9_999u,
                            requestedScopes = listOf(AppRemoraLinkScope.CONNECT_RUNTIME),
                            deviceDisplayName = "Pixel",
                        ),
                        pendingRestart = null,
                        hostRevocationStillRequired = false,
                    ),
                    busy = false,
                    onRevoke = {},
                    onForget = {},
                    onContinueApproval = { continued = true },
                    onRestart = {},
                    onAcknowledgeRestart = {},
                )
            }
        }

        composeRule.onNodeWithText("Continue approval").assertIsDisplayed().performClick()

        composeRule.runOnIdle { assertTrue(continued) }
    }

    @Test
    fun restartRequiresConfirmationAndUnknownOutcomeRequiresCheckedHostConfirmation() {
        val host = pendingHost().copy(
            state = AppRemoraLinkHostState.PAIRED,
            grantedScopes = listOf(AppRemoraLinkScope.RESTART_RUNTIME),
            pendingApproval = null,
        )
        val api = FakeRemoraLinkHostsApi(
            initialHost = host,
            restartOutcome = AppRemoraLinkRestartOutcome.OutcomeUnknown(41uL),
        )
        composeRule.setContent {
            RemoraAppTheme {
                RemoraLinkHostsContent(onBack = {}, api = api)
            }
        }

        composeRule.waitUntil(5_000) { api.hostReadCount >= 1 }
        composeRule.onNodeWithText("Restart codex").assertIsDisplayed().performClick()
        composeRule.onNodeWithText("Restart runtime").assertIsDisplayed()
        composeRule.runOnIdle { assertTrue(api.restartRequestCount == 0) }
        composeRule.onNodeWithText("Restart runtime").performClick()
        composeRule.waitUntil(5_000) { api.restartRequestCount == 1 && api.hostReadCount >= 2 }

        composeRule.onNodeWithText("Restart outcome unknown").assertIsDisplayed()
        composeRule.onNodeWithText("Runtime: codex").assertIsDisplayed()
        composeRule.onNodeWithText("Sequence: 41").assertIsDisplayed()
        composeRule.onNodeWithText("Acknowledge after checking host").performClick()
        composeRule.onNodeWithText("I checked the host").assertIsDisplayed()
        composeRule.runOnIdle { assertTrue(api.acknowledgementCount == 0) }
        composeRule.onNodeWithText("I checked the host").performClick()
        composeRule.waitUntil(5_000) { api.acknowledgementCount == 1 && api.hostReadCount >= 3 }

        composeRule.onNodeWithText("Restart outcome unknown").assertDoesNotExist()
    }

    @Test
    fun closingResumedPairingAfterCancelRefreshesPendingHost() {
        val api = FakeRemoraLinkHostsApi(pendingHost())
        composeRule.setContent {
            RemoraAppTheme {
                RemoraLinkHostsContent(
                    onBack = {},
                    api = api,
                    resumedPairingContent = { _, _, onDismiss, _ ->
                        var cancelled by remember { mutableStateOf(false) }
                        if (cancelled) {
                            Button(onClick = onDismiss) { Text("Close after cancel") }
                        } else {
                            Button(
                                onClick = {
                                    api.cancelPendingApproval()
                                    cancelled = true
                                },
                            ) { Text("Cancel pairing") }
                        }
                    },
                )
            }
        }

        composeRule.waitUntil(5_000) { api.hostReadCount >= 1 }
        composeRule.onNodeWithText("Continue approval").performClick()
        composeRule.onNodeWithText("Cancel pairing").assertIsDisplayed().performClick()
        composeRule.onNodeWithText("Close after cancel").assertIsDisplayed().performClick()
        composeRule.waitUntil(5_000) { api.hostReadCount >= 2 }

        composeRule.onNodeWithText("Approval pending").assertDoesNotExist()
        composeRule.onNodeWithText("No Remora Link hosts are paired with this device.").assertIsDisplayed()
    }

    @Test
    fun pairedCallbackRefreshesPendingHost() {
        val api = FakeRemoraLinkHostsApi(pendingHost())
        composeRule.setContent {
            RemoraAppTheme {
                RemoraLinkHostsContent(
                    onBack = {},
                    api = api,
                    resumedPairingContent = { hostId, _, _, onPaired ->
                        Button(
                            onClick = {
                                api.completePairing()
                                onPaired(hostId)
                            },
                        ) { Text("Finish pairing") }
                    },
                )
            }
        }

        composeRule.waitUntil(5_000) { api.hostReadCount >= 1 }
        composeRule.onNodeWithText("Continue approval").performClick()
        composeRule.onNodeWithText("Finish pairing").assertIsDisplayed().performClick()
        composeRule.waitUntil(5_000) { api.hostReadCount >= 2 }

        composeRule.onNodeWithText("Approval pending").assertDoesNotExist()
        composeRule.onNodeWithText("Paired").assertIsDisplayed()
    }

    private fun pendingHost() = AppRemoraLinkHostSummary(
        hostId = "host-1",
        hostDisplayName = "Office Mac",
        state = AppRemoraLinkHostState.AWAITING_HOST_APPROVAL,
        selectedRuntimeIds = listOf("codex"),
        grantedScopes = listOf(AppRemoraLinkScope.CONNECT_RUNTIME),
        pendingApproval = AppRemoraLinkPendingApproval(
            sas = "123 456",
            expiresAtUnixMs = 9_999u,
            requestedScopes = listOf(AppRemoraLinkScope.CONNECT_RUNTIME),
            deviceDisplayName = "Pixel",
        ),
        pendingRestart = null,
        hostRevocationStillRequired = false,
    )
}

private class FakeRemoraLinkHostsApi(
    initialHost: AppRemoraLinkHostSummary,
    private val restartOutcome: AppRemoraLinkRestartOutcome = AppRemoraLinkRestartOutcome.Succeeded(7uL),
) : RemoraLinkHostsApi {
    private val lock = Any()
    private var storedHosts = listOf(initialHost)

    @Volatile
    var hostReadCount: Int = 0
        private set

    @Volatile
    var restartRequestCount: Int = 0
        private set

    @Volatile
    var acknowledgementCount: Int = 0
        private set

    override suspend fun hosts(): List<AppRemoraLinkHostSummary> = synchronized(lock) {
        hostReadCount += 1
        storedHosts
    }

    override suspend fun restart(hostId: String, runtimeId: String): AppRemoraLinkRestartOutcome =
        synchronized(lock) {
            restartRequestCount += 1
            if (restartOutcome is AppRemoraLinkRestartOutcome.OutcomeUnknown) {
                storedHosts = storedHosts.map { host ->
                    if (host.hostId == hostId) {
                        host.copy(
                            pendingRestart = AppRemoraLinkPendingRestart(
                                runtimeId = runtimeId,
                                commandSequence = restartOutcome.commandSequence,
                                outcomeUnknown = true,
                            ),
                        )
                    } else {
                        host
                    }
                }
            }
            restartOutcome
        }

    override suspend fun acknowledgeUnknownRestart(hostId: String): ULong = synchronized(lock) {
        acknowledgementCount += 1
        val sequence = storedHosts.first { it.hostId == hostId }.pendingRestart?.commandSequence ?: 0uL
        storedHosts = storedHosts.map { host ->
            if (host.hostId == hostId) host.copy(pendingRestart = null) else host
        }
        sequence
    }

    override suspend fun revoke(hostId: String): AppRemoraLinkRevocationOutcome =
        AppRemoraLinkRevocationOutcome.OUTCOME_UNKNOWN

    override suspend fun forget(hostId: String): AppRemoraLinkForgetResult =
        AppRemoraLinkForgetResult(hostId, false, false)

    fun cancelPendingApproval() = synchronized(lock) {
        storedHosts = storedHosts.map {
            it.copy(
                state = AppRemoraLinkHostState.FORGOTTEN,
                pendingApproval = null,
            )
        }
    }

    fun completePairing() = synchronized(lock) {
        storedHosts = storedHosts.map {
            it.copy(
                state = AppRemoraLinkHostState.PAIRED,
                pendingApproval = null,
            )
        }
    }
}
