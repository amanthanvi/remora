package com.remora.android.background

import android.annotation.SuppressLint
import android.content.Context
import android.content.SharedPreferences
import com.remora.android.state.openEncryptedPrefsOrReset
import java.security.SecureRandom

internal data class SeenWake(
    val cursor: Long,
    val eventId: String,
)

internal data class WakeLedgerState(
    val installationId: String?,
    val highestSeenCursor: Long = 0L,
    val completedCursor: Long = 0L,
    val repairGeneration: Long = 0L,
    val completedRepairGeneration: Long = 0L,
    val recentlySeen: List<SeenWake> = emptyList(),
)

internal data class WakeReconciliationRequest(
    val targetCursor: Long,
    val repairGeneration: Long,
    val requiresFullRepair: Boolean,
) {
    val hasCursorWork: Boolean
        get() = targetCursor > 0L
}

internal enum class WakeIngestOutcome {
    ACCEPTED,
    WRONG_INSTALLATION,
    DUPLICATE,
    STALE_OR_REORDERED,
    CONFLICT,
}

internal data class WakeLedgerTransition(
    val state: WakeLedgerState,
    val outcome: WakeIngestOutcome,
)

/** Pure cursor/event reducer; Android storage is an adapter around this policy. */
internal object WakeLedgerReducer {
    private const val RECENT_EVENT_LIMIT = 32

    fun ingest(state: WakeLedgerState, hint: OpaqueWakeHint): WakeLedgerTransition {
        if (state.installationId == null || hint.installationId != state.installationId) {
            return WakeLedgerTransition(state, WakeIngestOutcome.WRONG_INSTALLATION)
        }

        val eventMatch = state.recentlySeen.firstOrNull { it.eventId == hint.eventId }
        if (eventMatch != null) {
            return WakeLedgerTransition(
                state,
                if (eventMatch.cursor == hint.cursor) {
                    WakeIngestOutcome.DUPLICATE
                } else {
                    WakeIngestOutcome.CONFLICT
                },
            )
        }
        val cursorMatch = state.recentlySeen.firstOrNull { it.cursor == hint.cursor }
        if (cursorMatch != null) {
            return WakeLedgerTransition(state, WakeIngestOutcome.CONFLICT)
        }
        if (hint.cursor <= state.completedCursor || hint.cursor <= state.highestSeenCursor) {
            return WakeLedgerTransition(state, WakeIngestOutcome.STALE_OR_REORDERED)
        }

        val recent = (state.recentlySeen + SeenWake(hint.cursor, hint.eventId))
            .sortedByDescending { it.cursor }
            .take(RECENT_EVENT_LIMIT)
        return WakeLedgerTransition(
            state.copy(
                highestSeenCursor = hint.cursor,
                recentlySeen = recent,
            ),
            WakeIngestOutcome.ACCEPTED,
        )
    }

    fun requestRepair(state: WakeLedgerState): WakeLedgerState =
        state.copy(repairGeneration = Math.addExact(state.repairGeneration, 1L))

    fun pendingRequest(state: WakeLedgerState): WakeReconciliationRequest? {
        val hasCursorWork = state.highestSeenCursor > state.completedCursor
        val hasRepairWork = state.repairGeneration > state.completedRepairGeneration
        if (!hasCursorWork && !hasRepairWork) return null
        return WakeReconciliationRequest(
            targetCursor = state.highestSeenCursor,
            repairGeneration = state.repairGeneration,
            requiresFullRepair = hasRepairWork,
        )
    }

    fun complete(
        state: WakeLedgerState,
        request: WakeReconciliationRequest,
        appliedThroughCursor: Long,
        fullRepairCompleted: Boolean,
    ): WakeLedgerState = state.copy(
        completedCursor = maxOf(
            state.completedCursor,
            minOf(request.targetCursor, appliedThroughCursor, state.highestSeenCursor),
        ),
        completedRepairGeneration = if (request.requiresFullRepair && fullRepairCompleted) {
            maxOf(
                state.completedRepairGeneration,
                minOf(request.repairGeneration, state.repairGeneration),
            )
        } else {
            state.completedRepairGeneration
        },
    )
}

internal object PushStorageLock

internal object PushClientIdentity {
    private val opaqueIdPattern = Regex("^[a-f0-9]{32}$")

    @SuppressLint("UseKtx") // Synchronous commit is part of crash-safe identity creation.
    fun getOrCreate(preferences: SharedPreferences): String = synchronized(PushStorageLock) {
        val existing = preferences.getString(KEY_CLIENT_INSTANCE_ID, null)
        if (existing != null) {
            check(opaqueIdPattern.matches(existing)) { "Invalid push installation identity" }
            return@synchronized existing
        }

        val bytes = ByteArray(16).also(SecureRandom()::nextBytes)
        val generated = buildString(capacity = bytes.size * 2) {
            bytes.forEach { byte ->
                val value = byte.toInt() and 0xff
                append(HEX[value ushr 4])
                append(HEX[value and 0x0f])
            }
        }
        check(preferences.edit().putString(KEY_CLIENT_INSTANCE_ID, generated).commit()) {
            "Unable to persist push client identity"
        }
        generated
    }

    const val KEY_CLIENT_INSTANCE_ID = "client_instance_id"
    private const val HEX = "0123456789abcdef"
}

internal class WakeLedger(context: Context) {
    private val preferences = openPushAwarenessPreferences(context.applicationContext)

    fun ingest(hint: OpaqueWakeHint): WakeIngestOutcome = synchronized(PushStorageLock) {
        val current = readStateLocked()
        val transition = WakeLedgerReducer.ingest(current, hint)
        if (transition.state != current) {
            writeStateLocked(transition.state)
        }
        transition.outcome
    }

    fun requestFullRepair(): WakeReconciliationRequest = synchronized(PushStorageLock) {
        val next = WakeLedgerReducer.requestRepair(readStateLocked())
        writeStateLocked(next)
        checkNotNull(WakeLedgerReducer.pendingRequest(next))
    }

    fun pendingRequest(): WakeReconciliationRequest? = synchronized(PushStorageLock) {
        WakeLedgerReducer.pendingRequest(readStateLocked())
    }

    fun complete(
        request: WakeReconciliationRequest,
        appliedThroughCursor: Long,
        fullRepairCompleted: Boolean,
    ) = synchronized(PushStorageLock) {
        require(appliedThroughCursor >= 0L)
        val current = readStateLocked()
        writeStateLocked(
            WakeLedgerReducer.complete(
                state = current,
                request = request,
                appliedThroughCursor = appliedThroughCursor,
                fullRepairCompleted = fullRepairCompleted,
            )
        )
    }

    private fun readStateLocked(): WakeLedgerState {
        val installationId = preferences.getString(PushBindingKeys.RELAY_INSTALLATION_ID, null)
        if (installationId != null) {
            check(PushRegistrationReducer.isValidOpaqueRelayId(installationId)) {
                "Invalid relay installation binding"
            }
        }
        val highestSeenCursor = preferences.getLong(KEY_HIGHEST_CURSOR, 0L)
        val completedCursor = preferences.getLong(KEY_COMPLETED_CURSOR, 0L)
        val repairGeneration = preferences.getLong(KEY_REPAIR_GENERATION, 0L)
        val completedRepairGeneration = preferences.getLong(KEY_COMPLETED_REPAIR_GENERATION, 0L)
        check(
            highestSeenCursor >= 0L &&
                completedCursor >= 0L &&
                completedCursor <= highestSeenCursor &&
                repairGeneration >= 0L &&
                completedRepairGeneration >= 0L &&
                completedRepairGeneration <= repairGeneration
        ) { "Invalid push wake ledger" }
        val seen = preferences.getStringSet(KEY_RECENTLY_SEEN, emptySet())
            .orEmpty()
            .map(::decodeSeenWake)
        return WakeLedgerState(
            installationId = installationId,
            highestSeenCursor = highestSeenCursor,
            completedCursor = completedCursor,
            repairGeneration = repairGeneration,
            completedRepairGeneration = completedRepairGeneration,
            recentlySeen = seen,
        )
    }

    @SuppressLint("UseKtx") // Do not hide a failed synchronous ledger commit.
    private fun writeStateLocked(state: WakeLedgerState) {
        val encodedSeen = state.recentlySeen
            .mapTo(mutableSetOf()) { wake -> "${wake.cursor}:${wake.eventId}" }
        check(
            preferences.edit()
                .putLong(KEY_HIGHEST_CURSOR, state.highestSeenCursor)
                .putLong(KEY_COMPLETED_CURSOR, state.completedCursor)
                .putLong(KEY_REPAIR_GENERATION, state.repairGeneration)
                .putLong(KEY_COMPLETED_REPAIR_GENERATION, state.completedRepairGeneration)
                .putStringSet(KEY_RECENTLY_SEEN, encodedSeen)
                .commit()
        ) { "Unable to persist push wake ledger" }
    }

    private fun decodeSeenWake(encoded: String): SeenWake {
        val separator = encoded.indexOf(':')
        check(separator > 0 && separator < encoded.lastIndex) { "Invalid push wake ledger" }
        val cursor = encoded.substring(0, separator).toLongOrNull()
        val eventId = encoded.substring(separator + 1)
        check(cursor != null && cursor > 0L) { "Invalid push wake ledger" }
        check(Regex("^[A-Za-z0-9_-]{16,128}$").matches(eventId)) {
            "Invalid push wake ledger"
        }
        return SeenWake(cursor, eventId)
    }

    private companion object {
        const val KEY_HIGHEST_CURSOR = "wake_highest_cursor"
        const val KEY_COMPLETED_CURSOR = "wake_completed_cursor"
        const val KEY_REPAIR_GENERATION = "wake_repair_generation"
        const val KEY_COMPLETED_REPAIR_GENERATION = "wake_completed_repair_generation"
        const val KEY_RECENTLY_SEEN = "wake_recently_seen"
    }
}

internal fun openPushAwarenessPreferences(context: Context): SharedPreferences =
    openEncryptedPrefsOrReset(context, "remora_push_awareness_v1")
