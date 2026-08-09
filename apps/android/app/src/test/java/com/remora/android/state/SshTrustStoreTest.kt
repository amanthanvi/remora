package com.remora.android.state

import org.junit.Assert.assertThrows
import org.junit.Assert.assertTrue
import org.junit.Test
import uniffi.codex_mobile_client.SshTrustStoreException

class SshTrustStoreTest {
    @Test
    fun unreadablePreferencesFailClosedWithoutFailingConstruction() {
        val store = SshTrustStore {
            throw IllegalStateException("restored ciphertext cannot be decrypted")
        }

        val error = assertThrows(SshTrustStoreException.Unavailable::class.java) {
            store.read("host.example", 22u)
        }

        assertTrue(error.detail.contains("open failed for host.example:22"))
        assertTrue(error.detail.contains("restored ciphertext cannot be decrypted"))
    }
}
