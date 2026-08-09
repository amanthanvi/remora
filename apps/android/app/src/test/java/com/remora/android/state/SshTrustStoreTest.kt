package com.remora.android.state

import android.content.SharedPreferences
import java.lang.reflect.Proxy
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
}
