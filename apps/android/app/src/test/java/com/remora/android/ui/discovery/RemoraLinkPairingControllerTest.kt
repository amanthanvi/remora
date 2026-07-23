package com.remora.android.ui.discovery

import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.awaitCancellation
import kotlinx.coroutines.launch
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.withTimeout
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test
import uniffi.codex_mobile_client.AppRemoraLinkAcceptance
import uniffi.codex_mobile_client.AppRemoraLinkConfirmationMode
import uniffi.codex_mobile_client.AppRemoraLinkInspection
import uniffi.codex_mobile_client.AppRemoraLinkOffer
import uniffi.codex_mobile_client.AppRemoraLinkPairingCancellationOutcome
import uniffi.codex_mobile_client.AppRemoraLinkPairingCode
import uniffi.codex_mobile_client.AppRemoraLinkPairingOutcome
import uniffi.codex_mobile_client.AppRemoraLinkPendingApproval
import uniffi.codex_mobile_client.AppRemoraLinkRuntimeOffer
import uniffi.codex_mobile_client.AppRemoraLinkScope

class RemoraLinkPairingControllerTest {
    @Test
    fun `valid code reaches offer with Rust defaults selected`() = runBlocking {
        val api = FakePairingApi(inspection = AppRemoraLinkInspection.Ready(offer()))
        val controller = RemoraLinkPairingController(api, "Pixel")

        controller.checkAvailability()
        controller.submitCode("remora-link-code")

        val state = controller.state.value as RemoraLinkPairingState.Offer
        assertEquals(setOf("codex"), state.selectedRuntimeIds)
        assertEquals(
            setOf(AppRemoraLinkScope.INSPECT_RUNTIMES, AppRemoraLinkScope.CONNECT_RUNTIME),
            state.selectedScopes,
        )
        assertEquals(1, api.inspectionCount)
    }

    @Test
    fun `oversized UTF-8 code fails before FFI`() = runBlocking {
        val api = FakePairingApi(inspection = AppRemoraLinkInspection.Ready(offer()))
        val controller = RemoraLinkPairingController(api, "Pixel")

        controller.submitCode("é".repeat(2_065))

        assertTrue(controller.state.value is RemoraLinkPairingState.Failure)
        assertEquals(0, api.inspectionCount)
    }

    @Test
    fun `interactive acceptance exposes SAS then completes after approval`() = runBlocking {
        val api = FakePairingApi(
            inspection = AppRemoraLinkInspection.Ready(offer()),
            acceptance = AppRemoraLinkPairingOutcome.AwaitingHostApproval(
                hostId = "host-1",
                sas = "123 456",
                expiresAtUnixMs = 9_999u,
                selectedRuntimeIds = listOf("codex"),
                requestedScopes = listOf(AppRemoraLinkScope.CONNECT_RUNTIME),
            ),
            awaitResult = {
                AppRemoraLinkPairingOutcome.Paired(
                    hostId = "host-1",
                    sas = "123 456",
                    selectedRuntimeIds = listOf("codex"),
                    grantedScopes = listOf(AppRemoraLinkScope.CONNECT_RUNTIME),
                    createdAtUnixMs = 1u,
                )
            },
        )
        val controller = RemoraLinkPairingController(api, "Pixel")
        controller.submitCode("code")

        controller.acceptOffer()
        assertEquals("123 456", (controller.state.value as RemoraLinkPairingState.Awaiting).sas)

        controller.awaitApproval()
        assertTrue(controller.state.value is RemoraLinkPairingState.Success)
    }

    @Test
    fun `device display name enforces eighty UTF-8 bytes`() = runBlocking {
        val controller = RemoraLinkPairingController(
            FakePairingApi(inspection = AppRemoraLinkInspection.Ready(offer())),
            "Pixel",
        )
        controller.submitCode("code")

        controller.updateDeviceDisplayName("é".repeat(41))
        controller.acceptOffer()

        val state = controller.state.value as RemoraLinkPairingState.Offer
        assertEquals("Device name must be 80 UTF-8 bytes or fewer.", state.validationMessage)
    }

    @Test
    fun `unknown cancellation outcome is explicit`() = runBlocking {
        val api = FakePairingApi(
            inspection = AppRemoraLinkInspection.Ready(offer()),
            acceptance = AppRemoraLinkPairingOutcome.AwaitingHostApproval(
                "host-1", "123 456", 9_999u, listOf("codex"), emptyList(),
            ),
            cancellation = AppRemoraLinkPairingCancellationOutcome.OUTCOME_UNKNOWN,
        )
        val controller = RemoraLinkPairingController(api, "Pixel")
        controller.submitCode("code")
        controller.acceptOffer()

        controller.cancelPairing()

        assertTrue(controller.state.value is RemoraLinkPairingState.OutcomeUnknown)
    }

    @Test
    fun `pending host can resume directly into approval`() {
        val controller = RemoraLinkPairingController(
            FakePairingApi(inspection = AppRemoraLinkInspection.Ready(offer())),
            "Pixel",
        )
        val pending = AppRemoraLinkPendingApproval(
            sas = "654 321",
            expiresAtUnixMs = 9_999u,
            requestedScopes = listOf(AppRemoraLinkScope.CONNECT_RUNTIME),
            deviceDisplayName = "Pixel",
        )

        controller.resume("host-2", pending)

        val state = controller.state.value as RemoraLinkPairingState.Awaiting
        assertEquals("host-2", state.hostId)
        assertEquals("654 321", state.sas)
        assertEquals(pending.requestedScopes, state.requestedScopes)
    }

    @Test
    fun `cancel interrupts a never returning approval await then calls cancel independently`() = runBlocking {
        val awaitStarted = CompletableDeferred<Unit>()
        val api = FakePairingApi(
            inspection = AppRemoraLinkInspection.Ready(offer()),
            awaitResult = {
                awaitStarted.complete(Unit)
                awaitCancellation()
            },
        )
        val controller = RemoraLinkPairingController(api, "Pixel")
        controller.resume(
            "host-1",
            AppRemoraLinkPendingApproval(
                sas = "123 456",
                expiresAtUnixMs = 9_999u,
                requestedScopes = listOf(AppRemoraLinkScope.CONNECT_RUNTIME),
                deviceDisplayName = "Pixel",
            ),
        )
        val awaitJob = launch(Dispatchers.Default) { controller.awaitApproval() }
        withTimeout(2_000) { awaitStarted.await() }

        controller.cancelPairing()
        withTimeout(2_000) { awaitJob.join() }

        assertTrue(awaitJob.isCancelled)
        assertEquals(1, api.cancellationCount)
        assertEquals(RemoraLinkPairingState.Ingress, controller.state.value)
    }

    private fun offer() = AppRemoraLinkOffer(
        offerId = "offer-1",
        hostId = "host-1",
        hostDisplayName = "Office Mac",
        expiresAtUnixMs = 9_999u,
        confirmationMode = AppRemoraLinkConfirmationMode.INTERACTIVE,
        runtimeOffers = listOf(
            AppRemoraLinkRuntimeOffer("codex", "Codex", available = true, recommended = true),
            AppRemoraLinkRuntimeOffer("claude", "Claude", available = false, recommended = false),
        ),
        maximumScopes = listOf(
            AppRemoraLinkScope.INSPECT_RUNTIMES,
            AppRemoraLinkScope.CONNECT_RUNTIME,
            AppRemoraLinkScope.RESTART_RUNTIME,
        ),
        requiredScopes = listOf(AppRemoraLinkScope.INSPECT_RUNTIMES),
        defaultScopes = listOf(
            AppRemoraLinkScope.INSPECT_RUNTIMES,
            AppRemoraLinkScope.CONNECT_RUNTIME,
        ),
        defaultRuntimeIds = listOf("codex"),
    )
}

private class FakePairingApi(
    private val inspection: AppRemoraLinkInspection,
    private val acceptance: AppRemoraLinkPairingOutcome = AppRemoraLinkPairingOutcome.AlreadyPaired(
        "host-1", listOf("codex"), emptyList(),
    ),
    private val awaitResult: suspend () -> AppRemoraLinkPairingOutcome = { acceptance },
    private val cancellation: AppRemoraLinkPairingCancellationOutcome =
        AppRemoraLinkPairingCancellationOutcome.CANCELLED,
) : RemoraLinkPairingApi {
    var inspectionCount = 0
    var cancellationCount = 0

    override suspend fun checkAvailability() = Unit

    override suspend fun inspect(code: AppRemoraLinkPairingCode): AppRemoraLinkInspection {
        inspectionCount += 1
        return inspection
    }

    override suspend fun accept(acceptance: AppRemoraLinkAcceptance) = this.acceptance

    override suspend fun await(hostId: String) = awaitResult()

    override suspend fun cancel(hostId: String): AppRemoraLinkPairingCancellationOutcome {
        cancellationCount += 1
        return cancellation
    }
}
