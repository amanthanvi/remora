package com.remora.android.state

import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

class DeviceDatabaseMasterKeyStoreTest {
    @Test
    fun createsOnceAndReloadsThePersistedKey() {
        val backend = FakeSecretBackend()
        val candidate = ByteArray(32) { it.toByte() }

        val created = DeviceDatabaseMasterKeyStore(
            RemoraLinkSecretStore(backend),
            entropy = { candidate.copyOf() },
        ).loadOrCreate()
        val loaded = DeviceDatabaseMasterKeyStore(
            RemoraLinkSecretStore(backend),
            entropy = { error("must not generate again") },
        ).loadOrCreate()

        assertArrayEquals(candidate, created)
        assertArrayEquals(candidate, loaded)
        assertEquals(1, backend.replaceCount)
    }

    @Test
    fun corruptStoredKeyFailsWithoutRotation() {
        val backend = FakeSecretBackend().apply { value = "not-base64!" }
        val result = runCatching {
            DeviceDatabaseMasterKeyStore(
                RemoraLinkSecretStore(backend),
                entropy = { ByteArray(32) { 7 } },
            ).loadOrCreate()
        }

        assertTrue(result.exceptionOrNull() is IllegalStateException)
        assertEquals(0, backend.replaceCount)
    }

    @Test
    fun failedPersistenceWipesCandidateAndFailsClosed() {
        val candidate = ByteArray(32) { 9 }
        val backend = FakeSecretBackend().apply { failWrites = true }
        val result = runCatching {
            DeviceDatabaseMasterKeyStore(
                RemoraLinkSecretStore(backend),
                entropy = { candidate },
            ).loadOrCreate()
        }

        assertTrue(result.exceptionOrNull() is IllegalStateException)
        assertTrue(candidate.all { it == 0.toByte() })
    }

    private class FakeSecretBackend : RemoraLinkSecretBackend {
        var value: String? = null
        var replaceCount = 0
        var failWrites = false

        override fun read(storageKey: String): RemoraLinkSecretReadStatus =
            value?.let(RemoraLinkSecretReadStatus::Available)
                ?: RemoraLinkSecretReadStatus.Missing

        override fun replace(storageKey: String, opaqueSecret: String): SecretWriteResult {
            replaceCount += 1
            if (failWrites) return SecretWriteResult.FAILED
            value = opaqueSecret
            return SecretWriteResult.COMMITTED
        }

        override fun delete(storageKey: String): SecretWriteResult {
            value = null
            return SecretWriteResult.COMMITTED
        }
    }
}
