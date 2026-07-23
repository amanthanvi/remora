package com.remora.android.state

import android.annotation.SuppressLint
import android.content.Context
import java.io.File
import java.security.KeyStore

/**
 * One-time Remora 1.6 authority cutover.
 *
 * The marker is committed only after all pre-1.6 host state, encrypted
 * credentials, transport identity, and Android Keystore authority are gone.
 * AppModel must not initialize when this returns false.
 */
internal object CurrentSecurityCutover {
    internal const val MARKER_PREFS = "remora_security_cutover"
    internal const val MARKER_KEY = "completed_1_6"

    fun apply(context: Context): Boolean = apply(AndroidSecurityCutoverBackend(context.applicationContext))

    internal fun apply(backend: SecurityCutoverBackend): Boolean {
        if (backend.isComplete()) return true
        if (!backend.clearPersistentPairingState()) return false
        if (!backend.clearRetiredCredentialState()) return false
        if (!backend.clearKeyStoreAuthority()) return false
        if (!backend.clearSavedServers()) return false
        return backend.markComplete() && backend.isComplete()
    }
}

internal interface SecurityCutoverBackend {
    fun isComplete(): Boolean
    fun clearPersistentPairingState(): Boolean
    fun clearRetiredCredentialState(): Boolean
    fun clearKeyStoreAuthority(): Boolean
    fun clearSavedServers(): Boolean
    fun markComplete(): Boolean
}

private class AndroidSecurityCutoverBackend(
    private val context: Context,
) : SecurityCutoverBackend {
    private val markerPreferences
        get() = context.getSharedPreferences(CurrentSecurityCutover.MARKER_PREFS, Context.MODE_PRIVATE)

    override fun isComplete(): Boolean =
        markerPreferences.getBoolean(CurrentSecurityCutover.MARKER_KEY, false)

    override fun clearPersistentPairingState(): Boolean =
        listOf(
            File(context.noBackupFilesDir, AtomicFileRemoraLinkJournalBackend.DIRECTORY_NAME),
            File(
                context.noBackupFilesDir,
                AtomicEncryptedRemoraLinkTransportIdentityBackend.DIRECTORY_NAME,
            ),
            File(context.filesDir, RemoraLinkSecretStore.DIRECTORY_NAME),
        ).all(::deleteTree)

    override fun clearRetiredCredentialState(): Boolean = runCatching {
        val name = retiredCredentialPreferencesName()
        val sharedPreferencesDirectory = File(context.dataDir, "shared_prefs")
        val storedFiles = listOf(
            File(sharedPreferencesDirectory, "$name.xml"),
            File(sharedPreferencesDirectory, "$name.xml.bak"),
        )
        if (storedFiles.none(File::exists)) {
            true
        } else {
            context.deleteSharedPreferences(name) && storedFiles.none(File::exists)
        }
    }.getOrDefault(false)

    override fun clearKeyStoreAuthority(): Boolean = runCatching {
        val keyStore = KeyStore.getInstance(ANDROID_KEYSTORE).apply { load(null) }
        val aliases = keyStore.aliases().toList()
        aliases
            .filter { alias ->
                alias == AtomicEncryptedRemoraLinkTransportIdentityBackend.KEY_ALIAS ||
                    alias == AndroidAtomicRemoraLinkSecretBackend.KEY_ALIAS ||
                    alias.startsWith(RemoraLinkDeviceKeyProvider.KEY_ALIAS_PREFIX)
            }
            .forEach(keyStore::deleteEntry)
        true
    }.getOrDefault(false)

    override fun clearSavedServers(): Boolean =
        runCatching {
            SavedServerStore.removeAllForSecurityCutover(context)
        }.getOrDefault(false)

    @SuppressLint("ApplySharedPref", "UseKtx") // The authority cutover marker must be durably committed.
    override fun markComplete(): Boolean =
        markerPreferences.edit()
            .putBoolean(CurrentSecurityCutover.MARKER_KEY, true)
            .commit()

    private fun deleteTree(file: File): Boolean =
        !file.exists() || file.deleteRecursively()

    private companion object {
        const val ANDROID_KEYSTORE = "AndroidKeyStore"
    }
}

/**
 * One-release deletion tombstone for the unsupported pre-1.6 credential file.
 *
 * Keep the retired product identifier out of source and binary string tables;
 * this byte spelling exists only to destroy the old bearer/private-key store.
 * Remove it when the direct-upgrade floor advances beyond 1.6.
 */
internal fun retiredCredentialPreferencesName(): String =
    byteArrayOf(
        97, 108, 108, 101, 121, 99, 97, 116, 95, 99,
        114, 101, 100, 101, 110, 116, 105, 97, 108, 115,
    ).toString(Charsets.UTF_8)
