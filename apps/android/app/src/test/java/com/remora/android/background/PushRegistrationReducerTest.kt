package com.remora.android.background

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class PushRegistrationReducerTest {
    private val firstRegistration = "fcm_installation_0000000000000001"
    private val rotatedRegistration = "fcm_installation_0000000000000002"

    @Test
    fun `same registration re-upserts without inventing a rotation generation`() {
        val first = PushRegistrationReducer.registered(
            PushRegistrationState(),
            firstRegistration,
            nowMs = 1L,
        )
        assertEquals(1L, first.generation)
        assertEquals(PushRegistrationDisposition.ACTIVE, first.disposition)
        assertTrue(first.pendingSync)

        val acknowledged = PushRegistrationReducer.acknowledgeUpsert(
            first,
            update(first),
            receipt(first, "reg_0123456789abcdef", serverGeneration = 1L),
        )
        val repeated = PushRegistrationReducer.registered(
            acknowledged,
            firstRegistration,
            nowMs = 2L,
        )
        assertEquals(1L, repeated.generation)
        assertTrue(repeated.pendingSync)
    }

    @Test
    fun `rotation and unregistration create ordered generations and tombstones`() {
        val first = PushRegistrationReducer.registered(
            PushRegistrationState(),
            firstRegistration,
            nowMs = 1L,
        )
        val rotated = PushRegistrationReducer.registered(
            PushRegistrationReducer.acknowledgeUpsert(
                first,
                update(first),
                receipt(first, "reg_0123456789abcdef", serverGeneration = 1L),
            ),
            rotatedRegistration,
            nowMs = 2L,
        )
        assertEquals(2L, rotated.generation)
        assertEquals(rotatedRegistration, rotated.providerRegistrationId)
        assertEquals(1L, rotated.replacesGeneration)
        assertEquals("reg_0123456789abcdef", rotated.replacesRelayRegistrationId)

        val tombstone = PushRegistrationReducer.unregistered(
            rotated,
            rotatedRegistration,
            nowMs = 3L,
        )
        assertEquals(3L, tombstone.generation)
        assertEquals(PushRegistrationDisposition.TOMBSTONE, tombstone.disposition)
        assertTrue(tombstone.pendingSync)

        val repeated = PushRegistrationReducer.unregistered(
            tombstone,
            rotatedRegistration,
            nowMs = 4L,
        )
        assertEquals(3L, repeated.generation)
    }

    @Test
    fun `acknowledgement is generation guarded and clears acknowledged tombstone secret`() {
        val active = PushRegistrationReducer.registered(
            PushRegistrationState(),
            firstRegistration,
            nowMs = 1L,
        )
        val tombstone = PushRegistrationReducer.unregistered(
            active,
            firstRegistration,
            nowMs = 2L,
        )

        assertEquals(
            tombstone,
            PushRegistrationReducer.acknowledgeTombstone(
                tombstone,
                tombstoneUpdate(tombstone, throughGeneration = 1L),
                PushRegistrationReceipt(
                    clientInstanceId = clientInstanceId,
                    clientMutationGeneration = 1L,
                    installationId = relayInstallationId,
                    registrationId = null,
                    registrationGeneration = 1L,
                    replaced = null,
                ),
            ),
        )
        val acknowledged = PushRegistrationReducer.acknowledgeTombstone(
            tombstone,
            tombstoneUpdate(tombstone, throughGeneration = 1L),
            PushRegistrationReceipt(
                clientInstanceId = clientInstanceId,
                clientMutationGeneration = tombstone.generation,
                installationId = relayInstallationId,
                registrationId = null,
                registrationGeneration = 1L,
                replaced = null,
            ),
        )
        assertFalse(acknowledged.pendingSync)
        assertNull(acknowledged.providerRegistrationId)
    }

    @Test
    fun `diagnostics redact provider registration identifiers`() {
        val state = PushRegistrationReducer.registered(
            PushRegistrationState(),
            firstRegistration,
            nowMs = 1L,
        )
        val update = PushRegistrationUpdate.Upsert(
            clientInstanceId = clientInstanceId,
            installationId = relayInstallationId,
            providerRegistrationId = firstRegistration,
            generation = 1L,
            knownRegistrationGeneration = null,
            replacesGeneration = null,
            replacesRegistrationId = null,
            updatedAtMs = 1L,
        )

        assertFalse(state.toString().contains(firstRegistration))
        assertFalse(update.toString().contains(firstRegistration))
        assertTrue(state.toString().contains("[redacted]"))
        assertEquals(PushProvider.FCM, update.provider)
    }

    @Test
    fun `relay receipt binds server installation and stale receipt cannot overwrite it`() {
        val pending = PushRegistrationReducer.registered(
            PushRegistrationState(),
            firstRegistration,
            nowMs = 1L,
        )
        val bound = PushRegistrationReducer.acknowledgeUpsert(
            pending,
            update(pending),
            receipt(pending, "reg_0123456789abcdef", serverGeneration = 1L),
        )
        assertEquals(relayInstallationId, bound.relayInstallationId)
        assertEquals("reg_0123456789abcdef", bound.relayRegistrationId)

        val rotated = PushRegistrationReducer.registered(bound, rotatedRegistration, nowMs = 2L)
        val stale = PushRegistrationReducer.acknowledgeUpsert(
            rotated,
            update(pending),
            receipt(pending, "reg_stale_0123456789", serverGeneration = 1L),
        )
        assertEquals(rotated.generation, stale.generation)
        assertTrue(stale.pendingSync)
        assertEquals(1L, stale.relayRegistrationGeneration)
    }

    @Test
    fun `stale unregistration cannot tombstone a rotated FCM identity`() {
        val current = PushRegistrationReducer.registered(
            PushRegistrationReducer.registered(
                PushRegistrationState(),
                firstRegistration,
                nowMs = 1L,
            ),
            rotatedRegistration,
            nowMs = 2L,
        )

        assertEquals(
            current,
            PushRegistrationReducer.unregistered(current, firstRegistration, nowMs = 3L),
        )
    }

    private fun receipt(
        state: PushRegistrationState,
        registrationId: String,
        serverGeneration: Long,
    ) = PushRegistrationReceipt(
        clientInstanceId = clientInstanceId,
        clientMutationGeneration = state.generation,
        installationId = relayInstallationId,
        registrationId = registrationId,
        registrationGeneration = serverGeneration,
        replaced = state.replacesGeneration != null,
    )

    private fun update(state: PushRegistrationState) = PushRegistrationUpdate.Upsert(
        clientInstanceId = clientInstanceId,
        installationId = state.relayInstallationId,
        providerRegistrationId = checkNotNull(state.providerRegistrationId),
        generation = state.generation,
        knownRegistrationGeneration = state.relayRegistrationGeneration,
        replacesGeneration = state.replacesGeneration,
        replacesRegistrationId = state.replacesRelayRegistrationId,
        updatedAtMs = state.updatedAtMs,
    )

    private fun tombstoneUpdate(
        state: PushRegistrationState,
        throughGeneration: Long,
    ) = PushRegistrationUpdate.Tombstone(
        clientInstanceId = clientInstanceId,
        installationId = state.relayInstallationId,
        generation = state.generation,
        throughGeneration = throughGeneration,
        registrationId = state.replacesRelayRegistrationId,
        updatedAtMs = state.updatedAtMs,
    )

    private companion object {
        const val relayInstallationId = "inst_0123456789abcdef"
        const val clientInstanceId = "0123456789abcdef0123456789abcdef"
    }
}
