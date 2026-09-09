package com.remora.android.background

import kotlinx.coroutines.CancellationException
import uniffi.codex_mobile_client.AppClientInterface
import uniffi.codex_mobile_client.AppRelayPushEnvironment
import uniffi.codex_mobile_client.AppRelayPushProvider
import uniffi.codex_mobile_client.AppRelayPushTokenObservation
import uniffi.codex_mobile_client.AppRelayPushTokenTombstone
import uniffi.codex_mobile_client.AppRelayReconcileOutcome
import uniffi.codex_mobile_client.BackgroundRelayException

/** Native input forwarding only; no cursor/registration receipt is interpreted as state. */
internal suspend fun reconcileRelayInputs(
    client: AppClientInterface,
    tokenInputs: PushTokenInputStore,
    wake: OpaqueWakeHint? = null,
    nowMs: Long = System.currentTimeMillis(),
): Boolean {
    var completed = true
    suspend fun attempt(operation: suspend () -> Boolean) {
        try {
            if (!operation()) completed = false
        } catch (cancelled: CancellationException) {
            throw cancelled
        } catch (_: Exception) {
            completed = false
        }
    }

    if (wake != null && wake.expiresAtMs > nowMs) {
        attempt {
            try {
                client.backgroundRelayIngestWake(wake.toRelayHint())
            } catch (_: BackgroundRelayException.InvalidWake) {
                // Rejected ingress cannot block authoritative foreground repair.
            } catch (_: BackgroundRelayException.UnknownInstallation) {
                // A delayed hint for an already removed host has no authority.
            }
            true
        }
    }
    attempt {
        tokenInputs.current()?.use { input ->
            val receipt = if (input.token == null) client.backgroundRelayTombstonePushToken(
                AppRelayPushTokenTombstone(AppRelayPushProvider.FCM,
                    AppRelayPushEnvironment.PRODUCTION, input.generation),
            ) else client.backgroundRelayObservePushToken(
                AppRelayPushTokenObservation(AppRelayPushProvider.FCM,
                    AppRelayPushEnvironment.PRODUCTION, input.generation), input.token,
            )
            receipt.pendingRetry == 0u
        } ?: true
    }
    attempt { client.backgroundRelayReconcile().none { it is AppRelayReconcileOutcome.Failed } }
    return completed
}
