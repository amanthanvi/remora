package com.remora.android.state

import com.remora.android.ui.common.StatusDotState
import org.junit.Assert.*
import org.junit.Test
import uniffi.codex_mobile_client.*

class ServerAccountPresentationTest {
    @Test fun customProviderWithoutOpenaiAuthIsConnectedAndNeverRedirectsToLogin() {
        val server = server(requiresAuth = false)
        assertFalse(server.needsAccountLogin)
        assertEquals("Connected", server.statusLabel)
        assertEquals(AppServerTransportState.CONNECTED.accentColor, server.statusColor)
        assertEquals(StatusDotState.OK, server.statusDotState)
        assertFalse(server.copy(isLocal = true).needsAccountLogin)
    }

    @Test fun authRequiredRemoteServerRetainsSignInStatus() {
        val server = server(requiresAuth = true)
        assertTrue(server.needsAccountLogin)
        assertEquals("Sign in required", server.statusLabel)
        assertEquals(StatusDotState.PENDING, server.statusDotState)
        assertEquals("Disconnected", server.copy(transportState = AppServerTransportState.DISCONNECTED).statusLabel)
    }

    @Test fun completedConnectionProgressDoesNotLeaveCustomProviderPending() {
        val progress = AppConnectionProgressSnapshot(listOf(AppConnectionStepSnapshot(
            AppConnectionStepKind.CONNECTED, AppConnectionStepState.COMPLETED, null,
        )), false, null)
        val custom = server(requiresAuth = false).copy(connectionProgress = progress)
        assertEquals(StatusDotState.OK, custom.statusDotState)
        assertEquals(StatusDotState.PENDING, custom.copy(requiresOpenaiAuth = true).statusDotState)
        val failed = progress.copy(steps = listOf(progress.steps.single().copy(state = AppConnectionStepState.FAILED)))
        assertEquals(StatusDotState.ERROR, custom.copy(connectionProgress = failed).statusDotState)
    }

    private fun server(requiresAuth: Boolean) = AppServerSnapshot(
        serverId = "test-server", displayName = "Provider", host = "localhost", port = 1234u,
        wakeMac = null, isLocal = false, health = AppServerHealth.CONNECTED,
        transportState = AppServerTransportState.CONNECTED,
        capabilities = AppServerCapabilities(true, true, true, true, true),
        account = null, requiresOpenaiAuth = requiresAuth, rateLimits = null,
        rateLimitsByRuntime = emptyList(), availableModels = null, agentRuntimes = emptyList(),
        connectionProgress = null, usageStats = null, codexVersion = null,
    )
}
