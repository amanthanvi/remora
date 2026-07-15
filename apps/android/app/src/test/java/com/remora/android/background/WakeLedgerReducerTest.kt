package com.remora.android.background

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class WakeLedgerReducerTest {
    private val installationId = "0123456789abcdef0123456789abcdef"

    @Test
    fun `deduplicates and rejects cursor event conflicts without applying state`() {
        val initial = WakeLedgerState(installationId = installationId)
        val accepted = WakeLedgerReducer.ingest(initial, hint(cursor = 10L))
        assertEquals(WakeIngestOutcome.ACCEPTED, accepted.outcome)
        assertEquals(10L, accepted.state.highestSeenCursor)

        val duplicate = WakeLedgerReducer.ingest(accepted.state, hint(cursor = 10L))
        assertEquals(WakeIngestOutcome.DUPLICATE, duplicate.outcome)
        assertEquals(accepted.state, duplicate.state)

        val cursorConflict = WakeLedgerReducer.ingest(
            accepted.state,
            hint(cursor = 10L, eventId = "event_aaaaaaaaaaaaaaaa"),
        )
        assertEquals(WakeIngestOutcome.CONFLICT, cursorConflict.outcome)
        assertEquals(accepted.state, cursorConflict.state)

        val eventConflict = WakeLedgerReducer.ingest(
            accepted.state,
            hint(cursor = 11L, eventId = "event_0000000000000010"),
        )
        assertEquals(WakeIngestOutcome.CONFLICT, eventConflict.outcome)
    }

    @Test
    fun `coalesces gaps to the highest cursor and preserves newer work during completion`() {
        val initial = WakeLedgerState(installationId = installationId)
        val atTen = WakeLedgerReducer.ingest(initial, hint(cursor = 10L)).state
        val firstRequest = checkNotNull(WakeLedgerReducer.pendingRequest(atTen))

        val atTwelve = WakeLedgerReducer.ingest(atTen, hint(cursor = 12L)).state
        assertEquals(12L, atTwelve.highestSeenCursor)
        val afterOldCompletion = WakeLedgerReducer.complete(
            atTwelve,
            firstRequest,
            appliedThroughCursor = 10L,
            fullRepairCompleted = false,
        )
        assertEquals(10L, afterOldCompletion.completedCursor)
        assertEquals(
            12L,
            checkNotNull(WakeLedgerReducer.pendingRequest(afterOldCompletion)).targetCursor,
        )

        val complete = WakeLedgerReducer.complete(
            afterOldCompletion,
            checkNotNull(WakeLedgerReducer.pendingRequest(afterOldCompletion)),
            appliedThroughCursor = 12L,
            fullRepairCompleted = false,
        )
        assertNull(WakeLedgerReducer.pendingRequest(complete))
    }

    @Test
    fun `foreground repair generation is independent and generation guarded`() {
        val initial = WakeLedgerState(installationId = installationId)
        val requested = WakeLedgerReducer.requestRepair(initial)
        val request = checkNotNull(WakeLedgerReducer.pendingRequest(requested))
        assertEquals(1L, request.repairGeneration)
        assertTrue(!request.hasCursorWork)

        val secondRepair = WakeLedgerReducer.requestRepair(requested)
        val afterOldCompletion = WakeLedgerReducer.complete(
            secondRepair,
            request,
            appliedThroughCursor = 0L,
            fullRepairCompleted = true,
        )
        assertEquals(1L, afterOldCompletion.completedRepairGeneration)
        assertEquals(
            2L,
            checkNotNull(WakeLedgerReducer.pendingRequest(afterOldCompletion)).repairGeneration,
        )
    }

    @Test
    fun `partial or unauthenticated receipts never advance beyond applied state`() {
        val pending = WakeLedgerReducer.requestRepair(
            WakeLedgerReducer.ingest(
                WakeLedgerState(installationId = installationId),
                hint(cursor = 20L),
            ).state,
        )
        val request = checkNotNull(WakeLedgerReducer.pendingRequest(pending))
        val partial = WakeLedgerReducer.complete(
            pending,
            request,
            appliedThroughCursor = 12L,
            fullRepairCompleted = false,
        )

        assertEquals(12L, partial.completedCursor)
        assertEquals(0L, partial.completedRepairGeneration)
        assertEquals(request, WakeLedgerReducer.pendingRequest(partial))
    }

    @Test
    fun `wrong installation and stale reorder cannot schedule new work`() {
        val completed = WakeLedgerState(
            installationId = installationId,
            highestSeenCursor = 20L,
            completedCursor = 20L,
        )
        assertEquals(
            WakeIngestOutcome.WRONG_INSTALLATION,
            WakeLedgerReducer.ingest(
                completed,
                hint(cursor = 21L, installation = "fedcba9876543210fedcba9876543210"),
            ).outcome,
        )
        assertEquals(
            WakeIngestOutcome.STALE_OR_REORDERED,
            WakeLedgerReducer.ingest(completed, hint(cursor = 19L)).outcome,
        )
    }

    private fun hint(
        cursor: Long,
        eventId: String = "event_${cursor.toString().padStart(16, '0')}",
        installation: String = installationId,
    ) = OpaqueWakeHint(
        schemaVersion = 1,
        installationId = installation,
        eventId = eventId,
        cursor = cursor,
        eventClass = OpaqueWakeEventClass.STATE_CHANGED,
        expiresAtMs = Long.MAX_VALUE,
    )
}
