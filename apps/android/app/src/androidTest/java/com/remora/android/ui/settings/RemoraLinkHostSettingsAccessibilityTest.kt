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
                        hostRevocationStillRequired = false,
                    ),
                    busy = false,
                    onRevoke = {},
                    onForget = {},
                    onContinueApproval = { continued = true },
                )
            }
        }

        composeRule.onNodeWithText("Continue approval").assertIsDisplayed().performClick()

        composeRule.runOnIdle { assertTrue(continued) }
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
        hostRevocationStillRequired = false,
    )
}

private class FakeRemoraLinkHostsApi(initialHost: AppRemoraLinkHostSummary) : RemoraLinkHostsApi {
    private val lock = Any()
    private var storedHosts = listOf(initialHost)

    @Volatile
    var hostReadCount: Int = 0
        private set

    override suspend fun hosts(): List<AppRemoraLinkHostSummary> = synchronized(lock) {
        hostReadCount += 1
        storedHosts
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
