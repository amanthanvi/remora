package com.remora.android.ui.terminal

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import uniffi.codex_mobile_client.AppRemoraLinkHostState
import uniffi.codex_mobile_client.AppRemoraLinkHostSummary
import uniffi.codex_mobile_client.AppRemoraLinkScope
import uniffi.codex_mobile_client.TerminalBackendKind

class RemoraLinkTerminalHostTest {
    @Test
    fun pairedAuthorizedShellProjectsToCredentialFreeTerminalBackend() {
        val host = host()

        val projected = host.toRemoraLinkTerminalHost()

        requireNotNull(projected)
        assertEquals("remora-link:host-1", projected.hostId)
        assertEquals("Studio", projected.displayName)
        assertEquals(
            TerminalBackendKind.RemoteRemoraLink(
                hostId = "remora-link:host-1",
                shell = null,
            ),
            projected.backend,
        )
    }

    @Test
    fun eligibilityRequiresPairedStateExactShellRuntimeAndConnectScope() {
        assertTrue(host().isEligibleForRemoraLinkTerminal())
        assertFalse(host(state = AppRemoraLinkHostState.NEEDS_REPAIR).isEligibleForRemoraLinkTerminal())
        assertFalse(host(selectedRuntimeIds = listOf("Shell")).isEligibleForRemoraLinkTerminal())
        assertFalse(host(selectedRuntimeIds = listOf("codex")).isEligibleForRemoraLinkTerminal())
        assertFalse(host(grantedScopes = listOf(AppRemoraLinkScope.INSPECT_RUNTIMES)).isEligibleForRemoraLinkTerminal())
    }

    @Test
    fun eligibleHostLookupMatchesTheExactCanonicalHostId() {
        val hosts = listOf(
            host(hostId = "remora-link:host-1", displayName = "Studio"),
            host(hostId = "remora-link:host-2", displayName = "Laptop"),
        )

        assertEquals(
            "remora-link:host-2",
            hosts.remoraLinkTerminalHost("remora-link:host-2")?.hostId,
        )
        assertNull(hosts.remoraLinkTerminalHost("host-2"))
        assertNull(
            listOf(host(state = AppRemoraLinkHostState.REVOKED))
                .remoraLinkTerminalHost("remora-link:host-1"),
        )
    }

    @Test
    fun exactPreferredHostFailsClosedInsteadOfSelectingAnotherBackend() {
        val options = listOf(
            "remora-link:one" to "remora-link:one",
            "ssh:workstation" to null,
        )

        assertEquals(
            "remora-link:one",
            resolveInitialTerminalBackendId(options, "remora-link:one"),
        )
        assertNull(resolveInitialTerminalBackendId(options, "remora-link:missing"))
        assertNull(resolveInitialTerminalBackendId(options, " "))
    }

    @Test
    fun genericTerminalRouteMaySelectTheFirstAvailableBackend() {
        val options = listOf(
            "remora-link:one" to "remora-link:one",
            "ssh:workstation" to null,
        )

        assertEquals(
            "remora-link:one",
            resolveInitialTerminalBackendId(options, preferredRemoraLinkHostId = null),
        )
        assertNull(resolveInitialTerminalBackendId(emptyList(), preferredRemoraLinkHostId = null))
    }

    private fun host(
        hostId: String = "remora-link:host-1",
        displayName: String = "Studio",
        state: AppRemoraLinkHostState = AppRemoraLinkHostState.PAIRED,
        selectedRuntimeIds: List<String> = listOf("codex", "shell"),
        grantedScopes: List<AppRemoraLinkScope> = listOf(AppRemoraLinkScope.CONNECT_RUNTIME),
    ) = AppRemoraLinkHostSummary(
        hostId = hostId,
        hostDisplayName = displayName,
        state = state,
        selectedRuntimeIds = selectedRuntimeIds,
        grantedScopes = grantedScopes,
        pendingApproval = null,
        pendingRestart = null,
        hostRevocationStillRequired = false,
    )
}
