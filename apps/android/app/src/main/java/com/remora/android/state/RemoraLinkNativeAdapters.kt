package com.remora.android.state

import java.security.MessageDigest
import java.util.Base64
import java.util.concurrent.atomic.AtomicReference
import kotlinx.coroutines.CoroutineDispatcher
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.CoroutineStart
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.currentCoroutineContext
import kotlinx.coroutines.ensureActive
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.coroutines.withContext
import uniffi.codex_mobile_client.AppRelaySecretValue
import uniffi.codex_mobile_client.AppRemoraLinkDeviceKeyBackend
import uniffi.codex_mobile_client.AppRemoraLinkDeviceKeyException
import uniffi.codex_mobile_client.AppRemoraLinkHardwareKey
import uniffi.codex_mobile_client.AppRemoraLinkHardwareKeyLoad
import uniffi.codex_mobile_client.AppRemoraLinkJournalBackend
import uniffi.codex_mobile_client.AppRemoraLinkJournalLoad
import uniffi.codex_mobile_client.AppRemoraLinkJournalSnapshot
import uniffi.codex_mobile_client.AppRemoraLinkJournalWriteOutcome
import uniffi.codex_mobile_client.AppRemoraLinkKeyAssurance
import uniffi.codex_mobile_client.AppRemoraLinkKeyDeletionStatus
import uniffi.codex_mobile_client.AppRemoraLinkTransportIdentityBackend
import uniffi.codex_mobile_client.AppRemoraLinkTransportIdentityException

/** Thin IO-dispatched projection from the Android crash-safe store to UniFFI. */
class AndroidRemoraLinkJournalBackend internal constructor(
    private val store: RemoraLinkJournalStore,
    private val ioDispatcher: CoroutineDispatcher = Dispatchers.IO,
) : AppRemoraLinkJournalBackend {
    constructor(context: android.content.Context) : this(RemoraLinkJournalStore(context))

    override suspend fun load(): AppRemoraLinkJournalLoad = onIo {
        when (val status = store.load()) {
            RemoraLinkJournalLoadStatus.Missing -> AppRemoraLinkJournalLoad.Missing
            is RemoraLinkJournalLoadStatus.Loaded -> AppRemoraLinkJournalLoad.Loaded(
                AppRemoraLinkJournalSnapshot(
                    revision = status.snapshot.revision,
                    payload = status.snapshot.opaquePayload,
                ),
            )
            RemoraLinkJournalLoadStatus.Corrupt,
            RemoraLinkJournalLoadStatus.StorageFailure,
            -> AppRemoraLinkJournalLoad.Unavailable
        }
    }

    override suspend fun compareAndSwap(
        expectedRevision: ULong?,
        replacement: AppRemoraLinkJournalSnapshot,
    ): AppRemoraLinkJournalWriteOutcome = onIo {
        val ownedReplacement = RemoraLinkJournalSnapshot(
            revision = replacement.revision,
            opaquePayload = replacement.payload,
        )
        when (store.compareAndSwap(expectedRevision, ownedReplacement)) {
            RemoraLinkJournalCasStatus.STORED -> AppRemoraLinkJournalWriteOutcome.STORED
            RemoraLinkJournalCasStatus.CONFLICT -> AppRemoraLinkJournalWriteOutcome.CONFLICT
            RemoraLinkJournalCasStatus.CORRUPT,
            RemoraLinkJournalCasStatus.STORAGE_FAILURE,
            RemoraLinkJournalCasStatus.INVALID_REPLACEMENT,
            -> AppRemoraLinkJournalWriteOutcome.UNAVAILABLE
        }
    }

    private suspend fun <T> onIo(operation: () -> T): T = withContext(ioDispatcher) {
        currentCoroutineContext().ensureActive()
        operation().also { currentCoroutineContext().ensureActive() }
    }
}

/** Thin IO-dispatched projection from the dedicated v2 transport-identity store to UniFFI. */
class AndroidRemoraLinkTransportIdentityBackend internal constructor(
    private val store: RemoraLinkTransportIdentityStore,
    private val ioDispatcher: CoroutineDispatcher = Dispatchers.IO,
    private val beforeTrailingCancellationCheck: ((ByteArray) -> Unit)? = null,
) : AppRemoraLinkTransportIdentityBackend {
    constructor(context: android.content.Context) : this(RemoraLinkTransportIdentityStore(context))

    override suspend fun loadOrCreate(candidate: AppRelaySecretValue): AppRelaySecretValue {
        // This guard deliberately lives outside withContext. Coroutine prompt cancellation can
        // discard a successful IO-block result while dispatching it back to the caller; retaining
        // the exact array here lets that cancellation path wipe it before propagating.
        val pendingIdentity = AtomicReference<ByteArray?>(null)
        return try {
            val deliveredIdentity = withContext(ioDispatcher) {
                currentCoroutineContext().ensureActive()
                val ownedIdentity = when (val status = store.loadOrCreate(candidate)) {
                    is RemoraLinkTransportIdentityStatus.Ready -> status.identityBytes
                    RemoraLinkTransportIdentityStatus.InvalidCandidate,
                    RemoraLinkTransportIdentityStatus.Corrupt,
                    RemoraLinkTransportIdentityStatus.StorageFailure,
                    -> throw AppRemoraLinkTransportIdentityException.Unavailable()
                }
                pendingIdentity.set(ownedIdentity)
                beforeTrailingCancellationCheck?.invoke(ownedIdentity)
                currentCoroutineContext().ensureActive()
                ownedIdentity
            }
            if (!pendingIdentity.compareAndSet(deliveredIdentity, null)) {
                deliveredIdentity.fill(0)
                error("transport identity ownership guard lost")
            }
            deliveredIdentity
        } catch (failure: Throwable) {
            pendingIdentity.getAndSet(null)?.fill(0)
            throw failure
        }
    }
}

/** Non-exportable device-key custody projected into the shared Remora Link v2 lifecycle. */
class AndroidRemoraLinkDeviceKeyBackend internal constructor(
    private val custody: RemoraLinkDeviceKeyCustody,
    private val ioDispatcher: CoroutineDispatcher = Dispatchers.IO,
) : AppRemoraLinkDeviceKeyBackend {
    constructor(provider: RemoraLinkDeviceKeyProvider) : this(provider as RemoraLinkDeviceKeyCustody)

    override suspend fun ensureHardwareKey(hostId: String): AppRemoraLinkHardwareKey = onIo {
        val slot = remoraLinkDeviceKeySlot(hostId)
            ?: throw AppRemoraLinkDeviceKeyException.Unavailable()
        custody.ensureKey(slot).toAppHardwareKey(slot)
    }

    override suspend fun loadHardwareKey(slot: String): AppRemoraLinkHardwareKeyLoad = onIo {
        when (val status = custody.loadKey(slot)) {
            is RemoraLinkDeviceKeyStatus.Ready ->
                AppRemoraLinkHardwareKeyLoad.Loaded(status.toAppHardwareKey(slot))
            is RemoraLinkDeviceKeyStatus.Unavailable -> {
                if (status.failure == RemoraLinkDeviceKeyFailure.KEY_NOT_FOUND) {
                    AppRemoraLinkHardwareKeyLoad.Missing
                } else {
                    throw status.failure.toAppException()
                }
            }
        }
    }

    override suspend fun signMessage(
        slot: String,
        message: AppRelaySecretValue,
    ): AppRelaySecretValue = onIo {
        val ownedMessage = message.copyOf()
        try {
            when (val status = custody.sign(slot, ownedMessage)) {
                is RemoraLinkSignatureStatus.Signed -> status.signatureDer.copyOf()
                is RemoraLinkSignatureStatus.Unavailable -> throw status.failure.toAppException()
            }
        } finally {
            ownedMessage.fill(0)
        }
    }

    override suspend fun deleteHardwareKey(slot: String): AppRemoraLinkKeyDeletionStatus = onIo {
        when (custody.deleteKey(slot)) {
            RemoraLinkDeviceKeyDeletionStatus.DELETED -> AppRemoraLinkKeyDeletionStatus.DELETED
            RemoraLinkDeviceKeyDeletionStatus.ALREADY_MISSING ->
                AppRemoraLinkKeyDeletionStatus.ALREADY_MISSING
            RemoraLinkDeviceKeyDeletionStatus.INVALID_OPAQUE_SLOT ->
                throw AppRemoraLinkDeviceKeyException.Unavailable()
            RemoraLinkDeviceKeyDeletionStatus.KEYSTORE_FAILURE ->
                throw AppRemoraLinkDeviceKeyException.Unavailable()
        }
    }

    private suspend fun <T> onIo(operation: () -> T): T = withContext(ioDispatcher) {
        currentCoroutineContext().ensureActive()
        operation().also { currentCoroutineContext().ensureActive() }
    }
}

internal fun remoraLinkDeviceKeySlot(authenticatedHostId: String): String? {
    val hostBytes = authenticatedHostId.toByteArray(Charsets.UTF_8)
    if (hostBytes.isEmpty() || hostBytes.size > MAX_AUTHENTICATED_HOST_ID_BYTES) return null
    if (authenticatedHostId.any(Char::isISOControl)) return null
    val digest = MessageDigest.getInstance("SHA-256").apply {
        update(REMORA_LINK_DEVICE_KEY_SLOT_DOMAIN)
        update(0)
        update(hostBytes)
    }.digest()
    return REMORA_LINK_DEVICE_KEY_SLOT_PREFIX +
        Base64.getUrlEncoder().withoutPadding().encodeToString(digest)
}

internal fun RemoraLinkKeyAssurance.toAppAssurance(): AppRemoraLinkKeyAssurance = when (this) {
    RemoraLinkKeyAssurance.STRONGBOX -> AppRemoraLinkKeyAssurance.STRONG_BOX
    RemoraLinkKeyAssurance.TRUSTED_ENVIRONMENT ->
        AppRemoraLinkKeyAssurance.TRUSTED_EXECUTION_ENVIRONMENT
    RemoraLinkKeyAssurance.UNKNOWN_SECURE -> AppRemoraLinkKeyAssurance.UNKNOWN_SECURE
    RemoraLinkKeyAssurance.DEBUG_EMULATOR_SOFTWARE ->
        AppRemoraLinkKeyAssurance.SOFTWARE_DEBUG_ONLY
}

private fun RemoraLinkDeviceKeyStatus.toAppHardwareKey(slot: String): AppRemoraLinkHardwareKey =
    when (this) {
        is RemoraLinkDeviceKeyStatus.Ready -> AppRemoraLinkHardwareKey(
            slot = slot,
            publicKeySec1 = publicKeySec1.copyOf(),
            assurance = assurance.toAppAssurance(),
        )
        is RemoraLinkDeviceKeyStatus.Unavailable -> throw failure.toAppException()
    }

private fun RemoraLinkDeviceKeyFailure.toAppException(): AppRemoraLinkDeviceKeyException =
    when (this) {
        RemoraLinkDeviceKeyFailure.KEY_NOT_FOUND -> AppRemoraLinkDeviceKeyException.Missing()
        RemoraLinkDeviceKeyFailure.KEY_INVALIDATED -> AppRemoraLinkDeviceKeyException.Invalidated()
        RemoraLinkDeviceKeyFailure.SOFTWARE_BACKED_KEY_REJECTED,
        RemoraLinkDeviceKeyFailure.UNSUPPORTED_KEY,
        -> AppRemoraLinkDeviceKeyException.HardwareUnavailable()
        RemoraLinkDeviceKeyFailure.INVALID_SIGNATURE ->
            AppRemoraLinkDeviceKeyException.InvalidSignature()
        RemoraLinkDeviceKeyFailure.INVALID_OPAQUE_SLOT,
        RemoraLinkDeviceKeyFailure.KEYSTORE_FAILURE,
        RemoraLinkDeviceKeyFailure.OPERATION_UNAVAILABLE,
        -> AppRemoraLinkDeviceKeyException.Unavailable()
    }

/**
 * Process-lifetime, retry-safe configuration gate. Configuration always runs in its own job;
 * cancellation or failure leaves the gate closed and permits a later retry.
 */
internal class RemoraLinkConfigurationGate(
    private val scope: CoroutineScope,
    private val configure: suspend () -> Unit,
) {
    private val configureMutex = Mutex()
    private val jobLock = Any()
    private val _available = MutableStateFlow(false)
    val available: StateFlow<Boolean> = _available.asStateFlow()

    @Volatile
    var lastFailure: Throwable? = null
        private set

    private var configurationJob: Job? = null

    fun configureInSeparateJob(): Job = synchronized(jobLock) {
        configurationJob?.takeIf(Job::isActive)?.let { return@synchronized it }
        val job = scope.launch(start = CoroutineStart.LAZY) {
            try {
                configureMutex.withLock {
                    if (!_available.value) {
                        configure()
                        lastFailure = null
                        _available.value = true
                    }
                }
            } catch (cancelled: kotlinx.coroutines.CancellationException) {
                throw cancelled
            } catch (failure: Throwable) {
                lastFailure = failure
                _available.value = false
            }
        }
        configurationJob = job
        job.invokeOnCompletion {
            synchronized(jobLock) {
                if (configurationJob === job) configurationJob = null
            }
        }
        job.start()
        job
    }

    suspend fun <T> runWhileAvailable(operation: suspend () -> T): T {
        if (!_available.value) throw RemoraLinkV2UnavailableException()
        return operation()
    }
}

class RemoraLinkV2UnavailableException : IllegalStateException(
    "Remora Link v2 native custody is unavailable",
)

private const val MAX_AUTHENTICATED_HOST_ID_BYTES = 1024
private const val REMORA_LINK_DEVICE_KEY_SLOT_PREFIX = "remora-link:v2:android:"
private val REMORA_LINK_DEVICE_KEY_SLOT_DOMAIN =
    "com.remora.android/remora-link-v2/device-key-slot".toByteArray(Charsets.UTF_8)
