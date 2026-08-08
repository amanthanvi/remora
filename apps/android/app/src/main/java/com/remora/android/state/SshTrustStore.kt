package com.remora.android.state

import android.content.Context
import uniffi.codex_mobile_client.SshTrustStoreException
import uniffi.codex_mobile_client.TerminalSshTrustBackend

/// Persistent host-key fingerprint pinning backed by EncryptedSharedPreferences.
/// Implements the Rust [`TerminalSshTrustBackend`] callback interface so the
/// shared terminal SSH backend can consult and update pins on every connect.
class SshTrustStore(context: Context) : TerminalSshTrustBackend {
    private val prefs = openEncryptedPrefsOrReset(context, PREFS_NAME)

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
        return try {
            prefs.getString(key(host, port), null)
        } catch (error: Exception) {
            throw SshTrustStoreException.Unavailable(
                "encrypted trust store read failed for $host:$port: ${error.message ?: error}",
            )
        }
    }

    override fun write(host: String, port: UShort, fingerprint: String) {
        prefs.edit().putString(key(host, port), fingerprint).apply()
    }

    override fun remove(host: String, port: UShort) {
        prefs.edit().remove(key(host, port)).apply()
    }

    private fun key(host: String, port: UShort): String = "${host.lowercase()}:$port"

    companion object {
        private const val PREFS_NAME = "remora_ssh_trust"
    }
}
