package com.remora.android.ui.settings

import kotlinx.coroutines.runBlocking
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test
import uniffi.codex_mobile_client.AppRemoraLinkForgetResult
import uniffi.codex_mobile_client.AppRemoraLinkHostState
import uniffi.codex_mobile_client.AppRemoraLinkHostSummary
import uniffi.codex_mobile_client.AppRemoraLinkPendingApproval
import uniffi.codex_mobile_client.AppRemoraLinkPendingRestart
import uniffi.codex_mobile_client.AppRemoraLinkRestartOutcome
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

    @Test
    fun `restart only accepts a paired selected runtime with restart scope`() = runBlocking {
        val ineligibleHosts = listOf(
            host("unpaired", "Unpaired").copy(state = AppRemoraLinkHostState.NEEDS_REPAIR),
            host("no-scope", "No scope"),
            host("wrong-runtime", "Wrong runtime").copy(
                grantedScopes = listOf(AppRemoraLinkScope.RESTART_RUNTIME),
            ),
        )
        val api = FakeHostsApi(storedHosts = ineligibleHosts.toMutableList())
        val controller = RemoraLinkHostsController(api)

        controller.restart(ineligibleHosts[0], "codex")
        controller.restart(ineligibleHosts[1], "codex")
        controller.restart(ineligibleHosts[2], "other")

        assertTrue(api.restartRequests.isEmpty())

        val eligible = host("eligible", "Office Mac").copy(
            grantedScopes = listOf(AppRemoraLinkScope.RESTART_RUNTIME),
        )
        api.storedHosts += eligible
        controller.restart(eligible, "codex")

        assertEquals(listOf(eligible.hostId to "codex"), api.restartRequests)
        assertTrue(controller.state.value.message.orEmpty().contains("sequence 7"))

        val prepared = eligible.copy(
            selectedRuntimeIds = listOf("codex", "other"),
            pendingRestart = AppRemoraLinkPendingRestart(
                runtimeId = "codex",
                commandSequence = 8uL,
                outcomeUnknown = false,
            ),
        )
        assertTrue(prepared.canRestartRuntime("codex"))
        assertTrue(!prepared.canRestartRuntime("other"))
        assertTrue(!prepared.copy(
            pendingRestart = prepared.pendingRestart?.copy(outcomeUnknown = true),
        ).canRestartRuntime("codex"))
    }

    @Test
    fun `unknown restart stays visible with runtime and sequence until acknowledged`() = runBlocking {
        val host = host("host-1", "Office Mac").copy(
            grantedScopes = listOf(AppRemoraLinkScope.RESTART_RUNTIME),
        )
        val api = FakeHostsApi(
            storedHosts = mutableListOf(host),
            restartOutcome = AppRemoraLinkRestartOutcome.OutcomeUnknown(41uL),
        )
        val controller = RemoraLinkHostsController(api)

        controller.restart(host, "codex")

        val pendingHost = controller.state.value.hosts.single()
        assertEquals("codex", pendingHost.pendingRestart?.runtimeId)
        assertEquals(41uL, pendingHost.pendingRestart?.commandSequence)
        assertTrue(pendingHost.pendingRestart?.outcomeUnknown == true)
        assertTrue(controller.state.value.message.orEmpty().contains("Check the host"))

        controller.acknowledgeUnknownRestart(pendingHost)

        assertEquals(listOf(host.hostId), api.acknowledgementRequests)
        assertTrue(controller.state.value.hosts.single().pendingRestart == null)
        assertTrue(controller.state.value.message.orEmpty().contains("sequence 41"))
    }

    @Test
    fun `acknowledge ignores hosts without a persisted unknown restart`() = runBlocking {
        val host = host("host-1", "Office Mac")
        val api = FakeHostsApi(storedHosts = mutableListOf(host))
        val controller = RemoraLinkHostsController(api)

        controller.acknowledgeUnknownRestart(host)

        assertTrue(api.acknowledgementRequests.isEmpty())
    }

    private fun host(id: String, name: String) = AppRemoraLinkHostSummary(
        hostId = id,
        hostDisplayName = name,
        state = AppRemoraLinkHostState.PAIRED,
        selectedRuntimeIds = listOf("codex"),
        grantedScopes = emptyList(),
        pendingApproval = null,
        pendingRestart = null,
        hostRevocationStillRequired = false,
    )
}

private class FakeHostsApi(
    val storedHosts: MutableList<AppRemoraLinkHostSummary>,
    private val revocation: AppRemoraLinkRevocationOutcome = AppRemoraLinkRevocationOutcome.REVOKED,
    private val forgetResult: AppRemoraLinkForgetResult = AppRemoraLinkForgetResult("", false, false),
    private val restartOutcome: AppRemoraLinkRestartOutcome = AppRemoraLinkRestartOutcome.Succeeded(7uL),
) : RemoraLinkHostsApi {
    val restartRequests = mutableListOf<Pair<String, String>>()
    val acknowledgementRequests = mutableListOf<String>()

    override suspend fun hosts(): List<AppRemoraLinkHostSummary> = storedHosts.toList()

    override suspend fun restart(hostId: String, runtimeId: String): AppRemoraLinkRestartOutcome {
        restartRequests += hostId to runtimeId
        if (restartOutcome is AppRemoraLinkRestartOutcome.OutcomeUnknown) {
            val index = storedHosts.indexOfFirst { it.hostId == hostId }
            storedHosts[index] = storedHosts[index].copy(
                pendingRestart = AppRemoraLinkPendingRestart(
                    runtimeId = runtimeId,
                    commandSequence = restartOutcome.commandSequence,
                    outcomeUnknown = true,
                ),
            )
        }
        return restartOutcome
    }

    override suspend fun acknowledgeUnknownRestart(hostId: String): ULong {
        acknowledgementRequests += hostId
        val index = storedHosts.indexOfFirst { it.hostId == hostId }
        val sequence = storedHosts[index].pendingRestart?.commandSequence ?: 0uL
        storedHosts[index] = storedHosts[index].copy(pendingRestart = null)
        return sequence
    }

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
