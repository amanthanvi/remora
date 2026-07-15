package com.remora.android.background

import android.annotation.SuppressLint
import android.content.Context

internal enum class PushRegistrationDisposition {
    ACTIVE,
    TOMBSTONE,
}

internal data class PushRegistrationState(
    val relayInstallationId: String? = null,
    val providerRegistrationId: String? = null,
    val relayRegistrationId: String? = null,
    val relayRegistrationGeneration: Long? = null,
    val generation: Long = 0L,
    val replacesGeneration: Long? = null,
    val replacesRelayRegistrationId: String? = null,
    val disposition: PushRegistrationDisposition? = null,
    val pendingSync: Boolean = false,
    val updatedAtMs: Long = 0L,
) {
    override fun toString(): String =
        "PushRegistrationState(generation=$generation, disposition=$disposition, " +
            "pendingSync=$pendingSync, providerRegistrationId=[redacted], " +
            "relayRegistrationId=[opaque])"
}

internal object PushRegistrationReducer {
    fun registered(
        state: PushRegistrationState,
        providerRegistrationId: String,
        nowMs: Long,
    ): PushRegistrationState {
        require(isValidProviderRegistrationId(providerRegistrationId))
        val isSameActiveRegistration =
            state.disposition == PushRegistrationDisposition.ACTIVE &&
                state.providerRegistrationId == providerRegistrationId
        return if (isSameActiveRegistration) {
            state.copy(pendingSync = true, updatedAtMs = nowMs)
        } else {
            state.copy(
                providerRegistrationId = providerRegistrationId,
                generation = Math.addExact(state.generation, 1L),
                replacesGeneration = if (
                    state.disposition == PushRegistrationDisposition.ACTIVE &&
                    state.generation > 0L
                ) {
                    state.generation
                } else {
                    null
                },
                replacesRelayRegistrationId = if (
                    state.disposition == PushRegistrationDisposition.ACTIVE
                ) {
                    state.relayRegistrationId ?: state.replacesRelayRegistrationId
                } else {
                    null
                },
                disposition = PushRegistrationDisposition.ACTIVE,
                pendingSync = true,
                updatedAtMs = nowMs,
            )
        }
    }

    fun unregistered(
        state: PushRegistrationState,
        providerRegistrationId: String,
        nowMs: Long,
    ): PushRegistrationState {
        require(isValidProviderRegistrationId(providerRegistrationId))
        if (
            state.disposition != PushRegistrationDisposition.ACTIVE ||
            state.providerRegistrationId != providerRegistrationId
        ) {
            // FCM may deliver an obsolete unregistration after a newer FID was
            // observed. Never let it tombstone the current registration.
            return state
        }
        return state.copy(
            generation = Math.addExact(state.generation, 1L),
            replacesGeneration = state.generation,
            replacesRelayRegistrationId =
                state.relayRegistrationId ?: state.replacesRelayRegistrationId,
            relayRegistrationId = null,
            disposition = PushRegistrationDisposition.TOMBSTONE,
            pendingSync = true,
            updatedAtMs = nowMs,
        )
    }

    fun acknowledgeUpsert(
        state: PushRegistrationState,
        update: PushRegistrationUpdate.Upsert,
        receipt: PushRegistrationReceipt,
    ): PushRegistrationState {
        if (
            update.generation != receipt.clientMutationGeneration ||
            update.clientInstanceId != receipt.clientInstanceId ||
            receipt.registrationId == null ||
            receipt.registrationGeneration == null ||
            receipt.registrationGeneration <= 0L ||
            receipt.replaced == null ||
            !isValidOpaqueRelayId(receipt.installationId) ||
            !isValidOpaqueRelayId(receipt.registrationId) ||
            (state.relayInstallationId != null &&
                state.relayInstallationId != receipt.installationId)
        ) {
            return state
        }
        val knownGeneration = update.knownRegistrationGeneration
        val expectedReplacement = update.replacesGeneration != null
        if (
            knownGeneration != null &&
            (
                (expectedReplacement &&
                    (!receipt.replaced || receipt.registrationGeneration <= knownGeneration)) ||
                    (!expectedReplacement &&
                        (receipt.replaced || receipt.registrationGeneration != knownGeneration))
                )
        ) {
            return state
        }
        if (state.generation > update.generation && state.pendingSync) {
            // A new FCM callback arrived while the prior network request was
            // in flight. Preserve the newer mutation, but retain the relay
            // binding/generation so its rotation or tombstone can supersede it.
            return state.copy(
                relayInstallationId = receipt.installationId,
                relayRegistrationId = receipt.registrationId,
                relayRegistrationGeneration = receipt.registrationGeneration,
                replacesRelayRegistrationId = receipt.registrationId,
            )
        }
        if (
            !state.pendingSync ||
            state.disposition != PushRegistrationDisposition.ACTIVE ||
            state.generation != update.generation
        ) {
            return state
        }
        return state.copy(
            relayInstallationId = receipt.installationId,
            relayRegistrationId = receipt.registrationId,
            relayRegistrationGeneration = receipt.registrationGeneration,
            replacesGeneration = null,
            replacesRelayRegistrationId = null,
            pendingSync = false,
        )
    }

    fun acknowledgeTombstone(
        state: PushRegistrationState,
        update: PushRegistrationUpdate.Tombstone,
        receipt: PushRegistrationReceipt,
    ): PushRegistrationState {
        if (
            update.generation != receipt.clientMutationGeneration ||
            update.clientInstanceId != receipt.clientInstanceId ||
            !state.pendingSync ||
            state.disposition != PushRegistrationDisposition.TOMBSTONE ||
            state.generation != update.generation ||
            receipt.registrationId != null ||
            receipt.replaced != null ||
            (receipt.registrationGeneration != null &&
                receipt.registrationGeneration != update.throughGeneration) ||
            (receipt.installationId != null && !isValidOpaqueRelayId(receipt.installationId)) ||
            (state.relayInstallationId != null &&
                receipt.installationId != null &&
                state.relayInstallationId != receipt.installationId)
        ) {
            return state
        }
        return state.copy(
            relayInstallationId = state.relayInstallationId ?: receipt.installationId,
            providerRegistrationId = null,
            relayRegistrationId = null,
            relayRegistrationGeneration = null,
            replacesGeneration = null,
            replacesRelayRegistrationId = null,
            pendingSync = false,
        )
    }

    fun isValidProviderRegistrationId(value: String): Boolean =
        value.length in 16..512 && value.all { character ->
            character.isLetterOrDigit() || character == '_' || character == '-'
        }

    fun isValidOpaqueRelayId(value: String?): Boolean =
        value != null && value.length in 16..128 && value.all { character ->
            character.isLetterOrDigit() || character == '_' || character == '-'
        }
}

/**
 * Update passed to the future hosted/self-hosted push registration adapter.
 * Its string representation never exposes the provider registration ID.
 */
sealed class PushRegistrationUpdate {
    abstract val provider: PushProvider
    abstract val clientInstanceId: String
    abstract val installationId: String?
    abstract val generation: Long
    abstract val environment: PushEnvironment
    abstract val updatedAtMs: Long

    class Upsert(
        override val provider: PushProvider = PushProvider.FCM,
        override val clientInstanceId: String,
        override val installationId: String?,
        val providerRegistrationId: String,
        override val generation: Long,
        override val environment: PushEnvironment = PushEnvironment.PRODUCTION,
        val knownRegistrationGeneration: Long?,
        val replacesGeneration: Long?,
        val replacesRegistrationId: String?,
        override val updatedAtMs: Long,
    ) : PushRegistrationUpdate() {
        override fun toString(): String =
            "PushRegistrationUpdate.Upsert(installationId=[opaque], " +
                "providerRegistrationId=[redacted], generation=$generation)"
    }

    class Tombstone(
        override val provider: PushProvider = PushProvider.FCM,
        override val clientInstanceId: String,
        override val installationId: String?,
        override val generation: Long,
        override val environment: PushEnvironment = PushEnvironment.PRODUCTION,
        val throughGeneration: Long,
        val registrationId: String?,
        override val updatedAtMs: Long,
    ) : PushRegistrationUpdate() {
        override fun toString(): String =
            "PushRegistrationUpdate.Tombstone(installationId=[opaque], " +
                "registrationId=[opaque], generation=$generation)"
    }
}

enum class PushProvider {
    FCM,
}

enum class PushEnvironment {
    PRODUCTION,
}

/**
 * Generation-guarded receipt returned by the hosted/self-hosted adapter.
 * Upsert acknowledgements must include both relay IDs. Tombstone
 * acknowledgements must omit [registrationId].
 */
data class PushRegistrationReceipt(
    val clientInstanceId: String,
    val clientMutationGeneration: Long,
    val installationId: String?,
    val registrationId: String?,
    val registrationGeneration: Long?,
    val replaced: Boolean?,
) {
    override fun toString(): String =
        "PushRegistrationReceipt(clientInstanceId=[opaque], " +
            "clientMutationGeneration=$clientMutationGeneration, " +
            "installationId=[opaque], registrationId=[opaque], " +
            "registrationGeneration=$registrationGeneration, replaced=$replaced)"
}

sealed class PushRegistrationSyncResult {
    data class Acknowledged(val receipt: PushRegistrationReceipt) :
        PushRegistrationSyncResult()

    data object Retry : PushRegistrationSyncResult()
    data object Rejected : PushRegistrationSyncResult()
}

/**
 * Narrow provider-registration port. [PushRegistrationUpdate.clientInstanceId]
 * is a stable local idempotency key, never the relay installation ID. The
 * adapter must provision/restore one relay-issued installation and persist its
 * scoped capabilities securely when [PushRegistrationUpdate.installationId]
 * is absent. FCM is production-only. Upsert maps to the relay's atomic single
 * active row per `(installation, provider, environment)`; tombstone deletes
 * only through the supplied relay registration generation. The adapter echoes
 * the local idempotency key/mutation generation in its receipt. It never
 * receives a host, thread, user, prompt, or notification payload.
 */
fun interface PushRegistrationSink {
    suspend fun sync(update: PushRegistrationUpdate): PushRegistrationSyncResult
}

object PushRegistrationSinks {
    @Volatile
    private var installed: PushRegistrationSink? = null

    fun install(context: Context, sink: PushRegistrationSink) {
        installed = sink
        BackgroundAwarenessWork.enqueueRegistrationSync(context.applicationContext)
    }

    internal fun current(): PushRegistrationSink? = installed
}

internal class PushRegistrationStore(context: Context) {
    private val preferences = openPushAwarenessPreferences(context.applicationContext)

    fun recordRegistered(providerRegistrationId: String, nowMs: Long) =
        update { state ->
            PushRegistrationReducer.registered(state, providerRegistrationId, nowMs)
        }

    fun recordUnregistered(providerRegistrationId: String, nowMs: Long) =
        update { state ->
            PushRegistrationReducer.unregistered(state, providerRegistrationId, nowMs)
        }

    fun pendingUpdate(): PushRegistrationUpdate? = synchronized(PushStorageLock) {
        val state = readStateLocked()
        if (!state.pendingSync) return@synchronized null
        val clientInstanceId = PushClientIdentity.getOrCreate(preferences)
        when (state.disposition) {
            PushRegistrationDisposition.ACTIVE -> PushRegistrationUpdate.Upsert(
                clientInstanceId = clientInstanceId,
                installationId = state.relayInstallationId,
                providerRegistrationId = state.providerRegistrationId
                    ?: error("Pending registration has no provider registration ID"),
                generation = state.generation,
                knownRegistrationGeneration = state.relayRegistrationGeneration,
                replacesGeneration = state.replacesGeneration,
                replacesRegistrationId = state.replacesRelayRegistrationId,
                updatedAtMs = state.updatedAtMs,
            )

            PushRegistrationDisposition.TOMBSTONE -> PushRegistrationUpdate.Tombstone(
                clientInstanceId = clientInstanceId,
                installationId = state.relayInstallationId,
                generation = state.generation,
                throughGeneration = state.relayRegistrationGeneration ?: 0L,
                registrationId = state.replacesRelayRegistrationId,
                updatedAtMs = state.updatedAtMs,
            )

            null -> error("Pending registration has no disposition")
        }
    }

    fun acknowledge(update: PushRegistrationUpdate, receipt: PushRegistrationReceipt) =
        update { state ->
            when (update) {
                is PushRegistrationUpdate.Upsert ->
                    PushRegistrationReducer.acknowledgeUpsert(state, update, receipt)

                is PushRegistrationUpdate.Tombstone ->
                    PushRegistrationReducer.acknowledgeTombstone(state, update, receipt)
            }
        }

    fun relayInstallationId(): String? = synchronized(PushStorageLock) {
        readStateLocked().relayInstallationId
    }

    private fun update(transform: (PushRegistrationState) -> PushRegistrationState) =
        synchronized(PushStorageLock) {
            val next = transform(readStateLocked())
            writeStateLocked(next)
        }

    private fun readStateLocked(): PushRegistrationState {
        val relayInstallationId = preferences.getString(PushBindingKeys.RELAY_INSTALLATION_ID, null)
        if (relayInstallationId != null) {
            check(PushRegistrationReducer.isValidOpaqueRelayId(relayInstallationId)) {
                "Invalid relay installation state"
            }
        }
        val providerRegistrationId = preferences.getString(KEY_PROVIDER_REGISTRATION_ID, null)
        if (providerRegistrationId != null) {
            check(PushRegistrationReducer.isValidProviderRegistrationId(providerRegistrationId)) {
                "Invalid provider registration state"
            }
        }
        val relayRegistrationId = preferences.getString(KEY_RELAY_REGISTRATION_ID, null)
        if (relayRegistrationId != null) {
            check(PushRegistrationReducer.isValidOpaqueRelayId(relayRegistrationId)) {
                "Invalid relay registration state"
            }
        }
        val relayRegistrationGeneration = if (
            preferences.contains(KEY_RELAY_REGISTRATION_GENERATION)
        ) {
            preferences.getLong(KEY_RELAY_REGISTRATION_GENERATION, 0L)
        } else {
            null
        }
        check(relayRegistrationGeneration == null || relayRegistrationGeneration > 0L) {
            "Invalid relay registration generation"
        }
        val generation = preferences.getLong(KEY_GENERATION, 0L)
        val replacesGeneration = if (preferences.contains(KEY_REPLACES_GENERATION)) {
            preferences.getLong(KEY_REPLACES_GENERATION, 0L)
        } else {
            null
        }
        check(replacesGeneration == null || replacesGeneration > 0L) {
            "Invalid provider registration state"
        }
        val replacesRegistrationId = preferences.getString(KEY_REPLACES_REGISTRATION_ID, null)
        if (replacesRegistrationId != null) {
            check(PushRegistrationReducer.isValidOpaqueRelayId(replacesRegistrationId)) {
                "Invalid relay replacement state"
            }
        }
        val updatedAtMs = preferences.getLong(KEY_UPDATED_AT_MS, 0L)
        check(generation >= 0L && updatedAtMs >= 0L) { "Invalid provider registration state" }
        val disposition = preferences.getString(KEY_DISPOSITION, null)?.let { encoded ->
            runCatching { PushRegistrationDisposition.valueOf(encoded) }
                .getOrElse { error("Invalid provider registration state") }
        }
        return PushRegistrationState(
            relayInstallationId = relayInstallationId,
            providerRegistrationId = providerRegistrationId,
            relayRegistrationId = relayRegistrationId,
            relayRegistrationGeneration = relayRegistrationGeneration,
            generation = generation,
            replacesGeneration = replacesGeneration,
            replacesRelayRegistrationId = replacesRegistrationId,
            disposition = disposition,
            pendingSync = preferences.getBoolean(KEY_PENDING_SYNC, false),
            updatedAtMs = updatedAtMs,
        )
    }

    @SuppressLint("UseKtx") // A failed synchronous security-state commit must surface.
    private fun writeStateLocked(state: PushRegistrationState) {
        val editor = preferences.edit()
            .putLong(KEY_GENERATION, state.generation)
            .putBoolean(KEY_PENDING_SYNC, state.pendingSync)
            .putLong(KEY_UPDATED_AT_MS, state.updatedAtMs)
        state.relayInstallationId?.let {
            editor.putString(PushBindingKeys.RELAY_INSTALLATION_ID, it)
        } ?: editor.remove(PushBindingKeys.RELAY_INSTALLATION_ID)
        state.providerRegistrationId?.let { editor.putString(KEY_PROVIDER_REGISTRATION_ID, it) }
            ?: editor.remove(KEY_PROVIDER_REGISTRATION_ID)
        state.relayRegistrationId?.let { editor.putString(KEY_RELAY_REGISTRATION_ID, it) }
            ?: editor.remove(KEY_RELAY_REGISTRATION_ID)
        state.relayRegistrationGeneration?.let {
            editor.putLong(KEY_RELAY_REGISTRATION_GENERATION, it)
        } ?: editor.remove(KEY_RELAY_REGISTRATION_GENERATION)
        state.replacesGeneration?.let { editor.putLong(KEY_REPLACES_GENERATION, it) }
            ?: editor.remove(KEY_REPLACES_GENERATION)
        state.replacesRelayRegistrationId?.let {
            editor.putString(KEY_REPLACES_REGISTRATION_ID, it)
        } ?: editor.remove(KEY_REPLACES_REGISTRATION_ID)
        state.disposition?.let { editor.putString(KEY_DISPOSITION, it.name) }
            ?: editor.remove(KEY_DISPOSITION)
        check(editor.commit()) { "Unable to persist provider registration state" }
    }

    private companion object {
        const val KEY_PROVIDER_REGISTRATION_ID = "provider_registration_id"
        const val KEY_RELAY_REGISTRATION_ID = "relay_registration_id"
        const val KEY_RELAY_REGISTRATION_GENERATION = "relay_registration_generation"
        const val KEY_GENERATION = "provider_registration_generation"
        const val KEY_REPLACES_GENERATION = "provider_registration_replaces_generation"
        const val KEY_REPLACES_REGISTRATION_ID = "relay_replaces_registration_id"
        const val KEY_DISPOSITION = "provider_registration_disposition"
        const val KEY_PENDING_SYNC = "provider_registration_pending_sync"
        const val KEY_UPDATED_AT_MS = "provider_registration_updated_at_ms"
    }
}

internal object PushBindingKeys {
    const val RELAY_INSTALLATION_ID = "relay_installation_id"
}
