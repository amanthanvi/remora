package com.remora.android.state

import android.content.Context
import android.content.SharedPreferences
import uniffi.codex_mobile_client.SshTrustStoreException
import uniffi.codex_mobile_client.TerminalSshTrustBackend

/// Persistent host-key fingerprint pinning backed by EncryptedSharedPreferences.
/// Implements the Rust [`TerminalSshTrustBackend`] callback interface so the
/// shared terminal SSH backend can consult and update pins on every connect.
class SshTrustStore internal constructor(
    private val openPrefs: () -> SharedPreferences,
) : TerminalSshTrustBackend {
    @Volatile
    private var cachedPrefs: SharedPreferences? = null

    constructor(context: Context) : this({
        openEncryptedPrefs(context.applicationContext, PREFS_NAME)
    })

    /**
     * Look up a pinned fingerprint.
     *
     * A missing key means "this host is new" and returns null. A failure to
     * read the encrypted store (keystore unavailable, decryption failure) is
     * thrown instead, because reporting it as null would tell Rust the host is
     * unknown and quietly downgrade an already-pinned host back to
     * trust-on-first-use.
     */
    override fun read(host: String, port: UShort): String? {
        val prefs = preferences("open", host, port)
        return try {
            prefs.getString(key(host, port), null)
        } catch (error: Exception) {
            throw unavailable("read", host, port, error)
        }
    }

    override fun write(host: String, port: UShort, fingerprint: String) {
        val prefs = preferences("write", host, port)
        try {
            val committed = prefs.edit()
                .putString(key(host, port), fingerprint)
                .commit()
            if (!committed) {
                throw IllegalStateException("encrypted preferences commit returned false")
            }
        } catch (error: Exception) {
            throw unavailable("write", host, port, error)
        }
    }

    override fun remove(host: String, port: UShort) {
        val prefs = preferences("remove", host, port)
        try {
            val committed = prefs.edit().remove(key(host, port)).commit()
            if (!committed) {
                throw IllegalStateException("encrypted preferences commit returned false")
            }
        } catch (error: Exception) {
            throw unavailable("remove", host, port, error)
        }
    }

    private fun unavailable(
        operation: String,
        host: String,
        port: UShort,
        error: Throwable,
    ) = SshTrustStoreException.Unavailable(
        "encrypted trust store $operation failed for $host:$port: ${error.message ?: error}",
    )

    private fun key(host: String, port: UShort): String = "${host.lowercase()}:$port"

    private fun preferences(operation: String, host: String, port: UShort): SharedPreferences {
        cachedPrefs?.let { return it }
        return synchronized(this) {
            cachedPrefs ?: try {
                openPrefs().also { cachedPrefs = it }
            } catch (error: Exception) {
                throw unavailable(operation, host, port, error)
            }
        }
    }

    companion object {
        private const val PREFS_NAME = "remora_ssh_trust"
    }
}
