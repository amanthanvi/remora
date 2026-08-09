package com.remora.android.state

import android.content.SharedPreferences
import java.lang.reflect.Proxy
import org.junit.Assert.assertEquals
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

    @Test
    fun transientOpenFailureIsRetriedOnTheNextOperation() {
        var attempts = 0
        val store = SshTrustStore {
            attempts += 1
            if (attempts == 1) {
                throw IllegalStateException("keystore temporarily unavailable")
            }
            preferencesWithValue("host.example:22", "SHA256:recovered")
        }

        assertThrows(SshTrustStoreException.Unavailable::class.java) {
            store.read("host.example", 22u)
        }

        assertEquals("SHA256:recovered", store.read("host.example", 22u))
        assertEquals(2, attempts)
    }

    @Test
    fun unwritablePreferencesSurfaceTypedTrustStoreFailure() {
        val store = SshTrustStore {
            throw IllegalStateException("encrypted preferences unavailable")
        }

        val error = assertThrows(SshTrustStoreException.Unavailable::class.java) {
            store.write("host.example", 22u, "SHA256:test")
        }

        assertTrue(error.detail.contains("write failed for host.example:22"))
        assertTrue(error.detail.contains("encrypted preferences unavailable"))
    }

    @Test
    fun unremovablePreferencesSurfaceTypedTrustStoreFailure() {
        val store = SshTrustStore {
            throw IllegalStateException("encrypted preferences unavailable")
        }

        val error = assertThrows(SshTrustStoreException.Unavailable::class.java) {
            store.remove("host.example", 22u)
        }

        assertTrue(error.detail.contains("remove failed for host.example:22"))
        assertTrue(error.detail.contains("encrypted preferences unavailable"))
    }

    @Test
    fun failedWriteCommitSurfacesTypedTrustStoreFailure() {
        val store = SshTrustStore { preferencesWithCommitResult(false) }

        val error = assertThrows(SshTrustStoreException.Unavailable::class.java) {
            store.write("host.example", 22u, "SHA256:test")
        }

        assertTrue(error.detail.contains("write failed for host.example:22"))
        assertTrue(error.detail.contains("commit returned false"))
    }

    @Test
    fun failedRemoveCommitSurfacesTypedTrustStoreFailure() {
        val store = SshTrustStore { preferencesWithCommitResult(false) }

        val error = assertThrows(SshTrustStoreException.Unavailable::class.java) {
            store.remove("host.example", 22u)
        }

        assertTrue(error.detail.contains("remove failed for host.example:22"))
        assertTrue(error.detail.contains("commit returned false"))
    }

    private fun preferencesWithCommitResult(result: Boolean): SharedPreferences {
        val editor = Proxy.newProxyInstance(
            SharedPreferences.Editor::class.java.classLoader,
            arrayOf(SharedPreferences.Editor::class.java),
        ) { proxy, method, _ ->
            when (method.name) {
                "putString", "remove" -> proxy
                "commit" -> result
                else -> throw UnsupportedOperationException(method.name)
            }
        } as SharedPreferences.Editor
        return Proxy.newProxyInstance(
            SharedPreferences::class.java.classLoader,
            arrayOf(SharedPreferences::class.java),
        ) { _, method, _ ->
            when (method.name) {
                "edit" -> editor
                else -> throw UnsupportedOperationException(method.name)
            }
        } as SharedPreferences
    }

    private fun preferencesWithValue(key: String, value: String): SharedPreferences =
        Proxy.newProxyInstance(
            SharedPreferences::class.java.classLoader,
            arrayOf(SharedPreferences::class.java),
        ) { _, method, args ->
            when (method.name) {
                "getString" -> if (args?.firstOrNull() == key) value else null
                else -> throw UnsupportedOperationException(method.name)
            }
        } as SharedPreferences
}
