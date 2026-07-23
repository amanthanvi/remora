package com.remora.android.state

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class CurrentSecurityCutoverTest {
    @Test
    fun missingMarkerClearsEveryAuthorityClassBeforeCommittingMarker() {
        val backend = FakeBackend()

        assertTrue(CurrentSecurityCutover.apply(backend))
        assertEquals(
            listOf("persistent", "retiredCredentials", "keystore", "servers", "marker"),
            backend.operations,
        )
        assertTrue(backend.complete)
    }

    @Test
    fun completedMarkerMakesCutoverIdempotent() {
        val backend = FakeBackend(complete = true)

        assertTrue(CurrentSecurityCutover.apply(backend))
        assertTrue(backend.operations.isEmpty())
    }

    @Test
    fun failureNeverCommitsMarkerOrRunsLaterSteps() {
        val backend = FakeBackend(persistentResult = false)

        assertFalse(CurrentSecurityCutover.apply(backend))
        assertEquals(listOf("persistent"), backend.operations)
        assertFalse(backend.complete)
    }

    @Test
    fun markerCommitFailureLeavesCutoverIncomplete() {
        val backend = FakeBackend(markerResult = false)

        assertFalse(CurrentSecurityCutover.apply(backend))
        assertEquals(
            listOf("persistent", "retiredCredentials", "keystore", "servers", "marker"),
            backend.operations,
        )
        assertFalse(backend.complete)
    }

    @Test
    fun savedServerDeletionFailureLeavesCutoverIncomplete() {
        val backend = FakeBackend(serversResult = false)

        assertFalse(CurrentSecurityCutover.apply(backend))
        assertEquals(
            listOf("persistent", "retiredCredentials", "keystore", "servers"),
            backend.operations,
        )
        assertFalse(backend.complete)
    }

    @Test
    fun retiredCredentialDeletionFailureLeavesCutoverIncomplete() {
        val backend = FakeBackend(retiredCredentialsResult = false)

        assertFalse(CurrentSecurityCutover.apply(backend))
        assertEquals(
            listOf("persistent", "retiredCredentials"),
            backend.operations,
        )
        assertFalse(backend.complete)
    }

    @Test
    fun retiredCredentialTombstoneTargetsHistoricalFileWithoutShippingItsIdentifier() {
        assertTrue(
            retiredCredentialPreferencesName().toByteArray().contentEquals(
                byteArrayOf(
                    97, 108, 108, 101, 121, 99, 97, 116, 95, 99,
                    114, 101, 100, 101, 110, 116, 105, 97, 108, 115,
                ),
            ),
        )
    }

    private class FakeBackend(
        var complete: Boolean = false,
        private val persistentResult: Boolean = true,
        private val retiredCredentialsResult: Boolean = true,
        private val keyStoreResult: Boolean = true,
        private val serversResult: Boolean = true,
        private val markerResult: Boolean = true,
    ) : SecurityCutoverBackend {
        val operations = mutableListOf<String>()

        override fun isComplete(): Boolean = complete

        override fun clearPersistentPairingState(): Boolean {
            operations += "persistent"
            return persistentResult
        }

        override fun clearRetiredCredentialState(): Boolean {
            operations += "retiredCredentials"
            return retiredCredentialsResult
        }

        override fun clearKeyStoreAuthority(): Boolean {
            operations += "keystore"
            return keyStoreResult
        }

        override fun clearSavedServers(): Boolean {
            operations += "servers"
            return serversResult
        }

        override fun markComplete(): Boolean {
            operations += "marker"
            if (markerResult) complete = true
            return markerResult
        }
    }
}
