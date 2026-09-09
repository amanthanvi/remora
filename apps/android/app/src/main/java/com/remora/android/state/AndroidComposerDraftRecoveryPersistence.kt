package com.remora.android.state

import android.content.Context
import android.security.keystore.KeyGenParameterSpec
import android.security.keystore.KeyProperties
import java.io.File
import java.security.KeyStore
import javax.crypto.Cipher
import javax.crypto.KeyGenerator
import javax.crypto.SecretKey
import javax.crypto.spec.GCMParameterSpec

/** Constructed by AppModel only after the fail-closed 1.6 authority cutover. */
internal fun androidComposerDraftRecoveryPersistence(
    context: Context,
    keyAlias: String = "com.remora.android.composer_recovery.v1.aes",
): ComposerDraftRecoveryPersistence {
    check(context.getSharedPreferences(CurrentSecurityCutover.MARKER_PREFS, Context.MODE_PRIVATE)
        .getBoolean(CurrentSecurityCutover.MARKER_KEY, false)) {
        "Complete the security cutover before opening draft recovery."
    }
    val directory = File(context.noBackupFilesDir, AtomicComposerDraftRecoveryPersistence.DIRECTORY_NAME)
    val custody = ComposerDraftRecoveryKey(directory, keyAlias)
    return AtomicComposerDraftRecoveryPersistence(directory, custody::encrypt, custody::decrypt,
        ::syncRemoraLinkDirectory)
}

private class ComposerDraftRecoveryKey(private val directory: File, private val keyAlias: String) {
    private fun key(create: Boolean): SecretKey {
        val keys = KeyStore.getInstance("AndroidKeyStore").apply { load(null) }
        (keys.getKey(keyAlias, null) as? SecretKey)?.let { return it }
        // Never generate a replacement key over ciphertext whose original key is unavailable.
        check(create && !File(directory, AtomicComposerDraftRecoveryPersistence.FILE_NAME).exists())
        return KeyGenerator.getInstance(KeyProperties.KEY_ALGORITHM_AES, "AndroidKeyStore").run {
            init(KeyGenParameterSpec.Builder(keyAlias,
                KeyProperties.PURPOSE_ENCRYPT or KeyProperties.PURPOSE_DECRYPT)
                .setKeySize(256)
                .setBlockModes(KeyProperties.BLOCK_MODE_GCM)
                .setEncryptionPaddings(KeyProperties.ENCRYPTION_PADDING_NONE)
                .setRandomizedEncryptionRequired(true)
                .setUserAuthenticationRequired(false)
                .build())
            generateKey()
        }
    }

    fun encrypt(plaintext: ByteArray): ByteArray {
        val cipher = Cipher.getInstance("AES/GCM/NoPadding")
        cipher.init(Cipher.ENCRYPT_MODE, key(create = true))
        cipher.updateAAD(AAD)
        check(cipher.iv.size == 12)
        return byteArrayOf(1) + cipher.iv + cipher.doFinal(plaintext)
    }

    fun decrypt(envelope: ByteArray): ByteArray {
        check(envelope.size >= 29 && envelope[0] == 1.toByte())
        val cipher = Cipher.getInstance("AES/GCM/NoPadding")
        cipher.init(Cipher.DECRYPT_MODE, key(create = false),
            GCMParameterSpec(128, envelope.copyOfRange(1, 13)))
        cipher.updateAAD(AAD)
        return cipher.doFinal(envelope, 13, envelope.size - 13)
    }

    companion object {
        private val AAD = "com.remora.android.composer-recovery.v1".toByteArray(Charsets.UTF_8)
    }
}
