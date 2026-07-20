package com.remora.android.ui.discovery

import java.nio.charset.StandardCharsets
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.Job
import kotlinx.coroutines.cancelAndJoin
import kotlinx.coroutines.currentCoroutineContext
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import uniffi.codex_mobile_client.AppRemoraLinkAcceptance
import uniffi.codex_mobile_client.AppRemoraLinkInspection
import uniffi.codex_mobile_client.AppRemoraLinkOffer
import uniffi.codex_mobile_client.AppRemoraLinkPairingCancellationOutcome
import uniffi.codex_mobile_client.AppRemoraLinkPairingCode
import uniffi.codex_mobile_client.AppRemoraLinkPairingOutcome
import uniffi.codex_mobile_client.AppRemoraLinkPendingApproval
import uniffi.codex_mobile_client.AppRemoraLinkScope
import uniffi.codex_mobile_client.RemoraLinkException

internal const val REMORA_LINK_PAIR_COMMAND = "npx --yes remora-link@latest pair"
internal const val REMORA_LINK_LEGACY_PAIR_COMMAND = "npx kittylitter"
internal const val REMORA_LINK_DEVICE_NAME_MAX_BYTES = 80

internal sealed interface RemoraLinkPairingState {
    data class Availability(
        val checking: Boolean = true,
        val message: String? = null,
    ) : RemoraLinkPairingState

    data object Ingress : RemoraLinkPairingState
    data object Inspecting : RemoraLinkPairingState

    data class LegacyRePair(
        val hostId: String,
        val hostDisplayName: String,
    ) : RemoraLinkPairingState

    data class Offer(
        val offer: AppRemoraLinkOffer,
        val selectedRuntimeIds: Set<String>,
        val selectedScopes: Set<AppRemoraLinkScope>,
        val deviceDisplayName: String,
        val validationMessage: String? = null,
    ) : RemoraLinkPairingState

    data class Accepting(val hostDisplayName: String) : RemoraLinkPairingState

    data class Awaiting(
        val hostId: String,
        val sas: String,
        val expiresAtUnixMs: ULong,
        val selectedRuntimeIds: List<String>,
        val requestedScopes: List<AppRemoraLinkScope>,
        val waiting: Boolean = false,
    ) : RemoraLinkPairingState

    data class Cancelling(val hostId: String) : RemoraLinkPairingState

    data class OutcomeUnknown(
        val hostId: String?,
        val message: String,
    ) : RemoraLinkPairingState

    data class Success(
        val hostId: String,
        val selectedRuntimeIds: List<String>,
        val grantedScopes: List<AppRemoraLinkScope>,
        val sas: String? = null,
        val alreadyPaired: Boolean = false,
    ) : RemoraLinkPairingState

    data class Failure(val message: String) : RemoraLinkPairingState
}

/** Narrow system boundary around the Rust-owned Remora Link lifecycle. */
internal interface RemoraLinkPairingApi {
    suspend fun checkAvailability()
    suspend fun inspect(code: AppRemoraLinkPairingCode): AppRemoraLinkInspection
    suspend fun accept(acceptance: AppRemoraLinkAcceptance): AppRemoraLinkPairingOutcome
    suspend fun await(hostId: String): AppRemoraLinkPairingOutcome
    suspend fun cancel(hostId: String): AppRemoraLinkPairingCancellationOutcome
}

internal class RemoraLinkPairingController(
    private val api: RemoraLinkPairingApi,
    suggestedDeviceName: String,
) {
    private val operationMutex = Mutex()
    private var approvalAwaitJob: Job? = null
    private val defaultDeviceName = suggestedDeviceName
        .trim()
        .ifEmpty { "Android device" }
        .truncateUtf8(REMORA_LINK_DEVICE_NAME_MAX_BYTES)

    private val _state = MutableStateFlow<RemoraLinkPairingState>(
        RemoraLinkPairingState.Availability(),
    )
    val state: StateFlow<RemoraLinkPairingState> = _state.asStateFlow()

    fun resume(hostId: String, pendingApproval: AppRemoraLinkPendingApproval) {
        _state.value = RemoraLinkPairingState.Awaiting(
            hostId = hostId,
            sas = pendingApproval.sas,
            expiresAtUnixMs = pendingApproval.expiresAtUnixMs,
            selectedRuntimeIds = emptyList(),
            requestedScopes = pendingApproval.requestedScopes,
        )
    }

    suspend fun checkAvailability() = operationMutex.withLock {
        _state.value = RemoraLinkPairingState.Availability(checking = true)
        try {
            api.checkAvailability()
            _state.value = RemoraLinkPairingState.Ingress
        } catch (error: Exception) {
            _state.value = RemoraLinkPairingState.Availability(
                checking = false,
                message = remoraLinkErrorMessage(error),
            )
        }
    }

    suspend fun submitCode(rawCode: String) = operationMutex.withLock {
        val bytes = rawCode.trim().toByteArray(StandardCharsets.UTF_8)
        if (bytes.isEmpty()) {
            _state.value = RemoraLinkPairingState.Failure("Paste or scan a pairing code first.")
            return@withLock
        }
        if (bytes.size > AppRemoraLinkPairingCode.MAXIMUM_BYTE_COUNT) {
            bytes.fill(0)
            _state.value = RemoraLinkPairingState.Failure(
                "Pairing codes must be ${AppRemoraLinkPairingCode.MAXIMUM_BYTE_COUNT} UTF-8 bytes or fewer.",
            )
            return@withLock
        }

        _state.value = RemoraLinkPairingState.Inspecting
        var carrier: AppRemoraLinkPairingCode? = null
        try {
            carrier = AppRemoraLinkPairingCode.copying(bytes)
            when (val inspection = api.inspect(carrier)) {
                is AppRemoraLinkInspection.Ready -> {
                    val offer = inspection.offer
                    val availableRuntimeIds = offer.runtimeOffers
                        .filter { it.available }
                        .mapTo(mutableSetOf()) { it.runtimeId }
                    _state.value = RemoraLinkPairingState.Offer(
                        offer = offer,
                        selectedRuntimeIds = offer.defaultRuntimeIds
                            .filterTo(linkedSetOf()) { it in availableRuntimeIds },
                        selectedScopes = (offer.defaultScopes + offer.requiredScopes)
                            .filterTo(linkedSetOf()) { it in offer.maximumScopes },
                        deviceDisplayName = defaultDeviceName,
                    )
                }

                is AppRemoraLinkInspection.LegacyRePairRequired -> {
                    _state.value = RemoraLinkPairingState.LegacyRePair(
                        hostId = inspection.hostId,
                        hostDisplayName = inspection.hostDisplayName,
                    )
                }
            }
        } catch (error: Exception) {
            _state.value = error.toPairingFailure()
        } finally {
            bytes.fill(0)
            carrier?.zeroize()
        }
    }

    fun updateDeviceDisplayName(value: String) {
        updateOffer { it.copy(deviceDisplayName = value, validationMessage = null) }
    }

    fun toggleRuntime(runtimeId: String) {
        updateOffer { state ->
            val runtime = state.offer.runtimeOffers.firstOrNull { it.runtimeId == runtimeId }
                ?: return@updateOffer state
            if (!runtime.available) return@updateOffer state
            state.copy(
                selectedRuntimeIds = state.selectedRuntimeIds.toggle(runtimeId),
                validationMessage = null,
            )
        }
    }

    fun toggleScope(scope: AppRemoraLinkScope) {
        updateOffer { state ->
            if (scope in state.offer.requiredScopes || scope !in state.offer.maximumScopes) {
                return@updateOffer state
            }
            state.copy(
                selectedScopes = state.selectedScopes.toggle(scope),
                validationMessage = null,
            )
        }
    }

    suspend fun acceptOffer() = operationMutex.withLock {
        val offerState = _state.value as? RemoraLinkPairingState.Offer ?: return@withLock
        val name = offerState.deviceDisplayName.trim()
        val validationMessage = when {
            name.isEmpty() -> "Device name cannot be empty."
            name.utf8ByteCount() > REMORA_LINK_DEVICE_NAME_MAX_BYTES ->
                "Device name must be $REMORA_LINK_DEVICE_NAME_MAX_BYTES UTF-8 bytes or fewer."
            offerState.selectedRuntimeIds.isEmpty() -> "Select at least one available runtime."
            !offerState.selectedScopes.containsAll(offerState.offer.requiredScopes) ->
                "Required permissions must remain selected."
            else -> null
        }
        if (validationMessage != null) {
            _state.value = offerState.copy(validationMessage = validationMessage)
            return@withLock
        }

        _state.value = RemoraLinkPairingState.Accepting(offerState.offer.hostDisplayName)
        try {
            val outcome = api.accept(
                AppRemoraLinkAcceptance(
                    offerId = offerState.offer.offerId,
                    deviceDisplayName = name,
                    selectedRuntimeIds = offerState.selectedRuntimeIds.toList(),
                    requestedScopes = offerState.selectedScopes.toList(),
                ),
            )
            _state.value = outcome.toPairingState()
        } catch (error: Exception) {
            _state.value = error.toPairingFailure(offerState.offer.hostId)
        }
    }

    suspend fun awaitApproval() {
        val job = currentCoroutineContext()[Job] ?: return
        val awaiting = operationMutex.withLock {
            val current = _state.value as? RemoraLinkPairingState.Awaiting
                ?: return@withLock null
            if (current.waiting) return@withLock null
            approvalAwaitJob = job
            _state.value = current.copy(waiting = true)
            current
        } ?: return

        try {
            val outcome = api.await(awaiting.hostId).toPairingState()
            operationMutex.withLock {
                if (approvalAwaitJob === job) {
                    _state.value = outcome
                }
            }
        } catch (error: CancellationException) {
            throw error
        } catch (error: Exception) {
            operationMutex.withLock {
                if (approvalAwaitJob === job) {
                    _state.value = error.toPairingFailure(awaiting.hostId)
                }
            }
        } finally {
            operationMutex.withLock {
                if (approvalAwaitJob === job) {
                    approvalAwaitJob = null
                }
            }
        }
    }

    suspend fun cancelPairing() {
        val cancellation = operationMutex.withLock {
            val current = _state.value
            val hostId = when (current) {
                is RemoraLinkPairingState.Awaiting -> current.hostId
                is RemoraLinkPairingState.OutcomeUnknown -> current.hostId
                else -> null
            } ?: return@withLock null
            val awaitJob = approvalAwaitJob
            approvalAwaitJob = null
            _state.value = RemoraLinkPairingState.Cancelling(hostId)
            hostId to awaitJob
        } ?: return

        val (hostId, awaitJob) = cancellation
        awaitJob?.cancelAndJoin()
        try {
            val outcome = when (api.cancel(hostId)) {
                AppRemoraLinkPairingCancellationOutcome.CANCELLED -> RemoraLinkPairingState.Ingress
                AppRemoraLinkPairingCancellationOutcome.OUTCOME_UNKNOWN ->
                    RemoraLinkPairingState.OutcomeUnknown(
                        hostId,
                        "The cancellation result is unknown. Check the host before trying again.",
                    )
            }
            operationMutex.withLock {
                if ((_state.value as? RemoraLinkPairingState.Cancelling)?.hostId == hostId) {
                    _state.value = outcome
                }
            }
        } catch (error: CancellationException) {
            throw error
        } catch (error: Exception) {
            operationMutex.withLock {
                if ((_state.value as? RemoraLinkPairingState.Cancelling)?.hostId == hostId) {
                    _state.value = error.toPairingFailure(hostId)
                }
            }
        }
    }

    fun returnToIngress() {
        _state.value = RemoraLinkPairingState.Ingress
    }

    private fun updateOffer(transform: (RemoraLinkPairingState.Offer) -> RemoraLinkPairingState.Offer) {
        val offer = _state.value as? RemoraLinkPairingState.Offer ?: return
        _state.value = transform(offer)
    }
}

private fun AppRemoraLinkPairingOutcome.toPairingState(): RemoraLinkPairingState = when (this) {
    is AppRemoraLinkPairingOutcome.AwaitingHostApproval -> RemoraLinkPairingState.Awaiting(
        hostId = hostId,
        sas = sas,
        expiresAtUnixMs = expiresAtUnixMs,
        selectedRuntimeIds = selectedRuntimeIds,
        requestedScopes = requestedScopes,
    )
    is AppRemoraLinkPairingOutcome.Paired -> RemoraLinkPairingState.Success(
        hostId = hostId,
        selectedRuntimeIds = selectedRuntimeIds,
        grantedScopes = grantedScopes,
        sas = sas,
    )
    is AppRemoraLinkPairingOutcome.AlreadyPaired -> RemoraLinkPairingState.Success(
        hostId = hostId,
        selectedRuntimeIds = selectedRuntimeIds,
        grantedScopes = grantedScopes,
        alreadyPaired = true,
    )
}

private fun Exception.toPairingFailure(hostId: String? = null): RemoraLinkPairingState =
    if (this is RemoraLinkException.OutcomeUnknown) {
        RemoraLinkPairingState.OutcomeUnknown(
            hostId,
            "The host may have completed this operation. Check Remora Link Hosts before retrying.",
        )
    } else {
        RemoraLinkPairingState.Failure(remoraLinkErrorMessage(this))
    }

internal fun remoraLinkErrorMessage(error: Throwable): String = when (error) {
    is RemoraLinkException.NotConfigured -> "Remora Link is unavailable on this device."
    is RemoraLinkException.InvalidPairingCode -> "That pairing code is invalid or expired."
    is RemoraLinkException.UnknownOffer -> "This pairing offer is no longer available. Scan it again."
    is RemoraLinkException.PairingCodeRequired -> "Scan the original pairing code to continue."
    is RemoraLinkException.JournalUnavailable -> "Secure Remora Link storage is unavailable."
    is RemoraLinkException.JournalConflict -> "Remora Link changed elsewhere. Refresh and try again."
    is RemoraLinkException.JournalCorrupt -> "Remora Link storage needs recovery before pairing."
    is RemoraLinkException.CredentialUnavailable -> "The device credential is temporarily unavailable."
    is RemoraLinkException.MissingCredential -> "This host needs to be paired again."
    is RemoraLinkException.InvalidSignature -> "The host could not verify this device."
    is RemoraLinkException.HostUnavailable -> "The host is unavailable."
    is RemoraLinkException.V2Unavailable -> "This host does not support Remora Link v2."
    is RemoraLinkException.Cancelled -> "Pairing was cancelled."
    is RemoraLinkException.IdentityDrift -> "The host identity changed. Pair again after verifying the host."
    is RemoraLinkException.PolicyDrift -> "The host permissions changed. Pair again to review them."
    is RemoraLinkException.ConfirmationMismatch -> "The host confirmation did not match."
    is RemoraLinkException.ProtocolViolation -> "The host returned an invalid pairing response."
    is RemoraLinkException.PairingUnavailable -> "Pairing is not available on the host right now."
    is RemoraLinkException.AuthorizationRequired -> "Approve this device on the host to continue."
    is RemoraLinkException.RuntimeUnavailable -> "A selected runtime is no longer available."
    is RemoraLinkException.OutcomeUnknown -> "The operation may have completed. Check the host before retrying."
    is RemoraLinkException.InvalidSelection -> "Review the selected runtimes and permissions."
    is RemoraLinkException.InvitationMismatch -> "This code does not match the pending pairing request."
    is RemoraLinkException.NotPaired -> "This device is not paired with the host."
    is RemoraLinkException.OperationInProgress -> "Another Remora Link operation is already in progress."
    is RemoraLinkException.NeedsRepair -> "This host needs to be paired again."
    else -> error.localizedMessage?.takeIf { it.isNotBlank() } ?: "Remora Link could not complete the operation."
}

private fun String.utf8ByteCount(): Int = toByteArray(StandardCharsets.UTF_8).size

private fun String.truncateUtf8(maxBytes: Int): String {
    if (utf8ByteCount() <= maxBytes) return this
    val builder = StringBuilder()
    for (character in this) {
        val candidate = builder.toString() + character
        if (candidate.utf8ByteCount() > maxBytes) break
        builder.append(character)
    }
    return builder.toString()
}

private fun <T> Set<T>.toggle(value: T): Set<T> =
    if (value in this) this - value else this + value
