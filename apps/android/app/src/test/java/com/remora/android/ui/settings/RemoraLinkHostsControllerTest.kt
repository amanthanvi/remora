package com.remora.android.ui.settings

import kotlinx.coroutines.runBlocking
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test
import uniffi.codex_mobile_client.AppRemoraLinkForgetResult
import uniffi.codex_mobile_client.AppRemoraLinkHostState
import uniffi.codex_mobile_client.AppRemoraLinkHostSummary
import uniffi.codex_mobile_client.AppRemoraLinkPendingApproval
import uniffi.codex_mobile_client.AppRemoraLinkRevocationOutcome
import uniffi.codex_mobile_client.AppRemoraLinkScope

class RemoraLinkHostsControllerTest {
    @Test
    fun `forget surfaces required host cleanup after local record disappears`() = runBlocking {
        val host = host("host-1", "Office Mac")
        val api = FakeHostsApi(
            storedHosts = mutableListOf(host),
            forgetResult = AppRemoraLinkForgetResult(
                hostId = host.hostId,
                alreadyForgotten = false,
                hostRevocationStillRequired = true,
            ),
        )
        val controller = RemoraLinkHostsController(api)
        controller.refresh()

        controller.forget(host)

        assertTrue(controller.state.value.hosts.isEmpty())
        assertEquals(AppRemoraLinkHostState.FORGOTTEN, api.storedHosts.single().state)
        assertEquals("Office Mac", controller.state.value.cleanupRequiredHostName)
    }

    @Test
    fun `refresh hides forgotten journal tombstones`() = runBlocking {
        val forgotten = host("host-1", "Office Mac").copy(
            state = AppRemoraLinkHostState.FORGOTTEN,
        )
        val controller = RemoraLinkHostsController(
            FakeHostsApi(storedHosts = mutableListOf(forgotten)),
        )

        controller.refresh()

        assertTrue(controller.state.value.hosts.isEmpty())
    }

    @Test
    fun `unknown revoke result keeps host and tells user to verify`() = runBlocking {
        val host = host("host-1", "Office Mac")
        val controller = RemoraLinkHostsController(
            FakeHostsApi(
                storedHosts = mutableListOf(host),
                revocation = AppRemoraLinkRevocationOutcome.OUTCOME_UNKNOWN,
            ),
        )
        controller.refresh()

        controller.revoke(host)

        assertEquals(listOf(host), controller.state.value.hosts)
        assertTrue(controller.state.value.message.orEmpty().contains("unknown"))
    }

    @Test
    fun `dismissal after cancelled approval refreshes stale pending host`() = runBlocking {
        val pendingHost = host("host-1", "Office Mac").copy(
            state = AppRemoraLinkHostState.AWAITING_HOST_APPROVAL,
            pendingApproval = AppRemoraLinkPendingApproval(
                sas = "123 456",
                expiresAtUnixMs = 9_999u,
                requestedScopes = listOf(AppRemoraLinkScope.CONNECT_RUNTIME),
                deviceDisplayName = "Pixel",
            ),
        )
        val api = FakeHostsApi(storedHosts = mutableListOf(pendingHost))
        val controller = RemoraLinkHostsController(api)
        controller.refresh()
        assertTrue(controller.state.value.hosts.single().pendingApproval != null)

        api.storedHosts[0] = api.storedHosts.single().copy(
            state = AppRemoraLinkHostState.FORGOTTEN,
            pendingApproval = null,
        )
        controller.refreshAfterResumedPairingDismissal()

        assertTrue(controller.state.value.hosts.isEmpty())
    }

    private fun host(id: String, name: String) = AppRemoraLinkHostSummary(
        hostId = id,
        hostDisplayName = name,
        state = AppRemoraLinkHostState.PAIRED,
        selectedRuntimeIds = listOf("codex"),
        grantedScopes = emptyList(),
        pendingApproval = null,
        hostRevocationStillRequired = false,
    )
}

private class FakeHostsApi(
    val storedHosts: MutableList<AppRemoraLinkHostSummary>,
    private val revocation: AppRemoraLinkRevocationOutcome = AppRemoraLinkRevocationOutcome.REVOKED,
    private val forgetResult: AppRemoraLinkForgetResult = AppRemoraLinkForgetResult("", false, false),
) : RemoraLinkHostsApi {
    override suspend fun hosts(): List<AppRemoraLinkHostSummary> = storedHosts.toList()

    override suspend fun revoke(hostId: String): AppRemoraLinkRevocationOutcome {
        if (revocation == AppRemoraLinkRevocationOutcome.REVOKED) {
            storedHosts.removeAll { it.hostId == hostId }
        }
        return revocation
    }

    override suspend fun forget(hostId: String): AppRemoraLinkForgetResult {
        val index = storedHosts.indexOfFirst { it.hostId == hostId }
        if (index >= 0) {
            storedHosts[index] = storedHosts[index].copy(state = AppRemoraLinkHostState.FORGOTTEN)
        }
        return forgetResult
    }
}
