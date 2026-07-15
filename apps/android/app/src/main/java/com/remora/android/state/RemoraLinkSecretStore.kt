package com.remora.android.state

import android.content.Context
import android.os.Build
import android.security.keystore.KeyGenParameterSpec
import android.security.keystore.KeyInfo
import android.security.keystore.KeyProperties
import android.system.Os
import android.system.OsConstants
import com.remora.android.BuildConfig
import java.io.File
import java.io.FileOutputStream
import java.nio.file.StandardCopyOption
import java.security.InvalidAlgorithmParameterException
import java.security.KeyStore
import java.security.ProviderException
import javax.crypto.Cipher
import javax.crypto.KeyGenerator
import javax.crypto.SecretKey
import javax.crypto.SecretKeyFactory
import javax.crypto.spec.GCMParameterSpec

sealed interface RemoraLinkSecretReadStatus {
    /** Opaque protocol-owned value. This layer deliberately performs no protocol decoding. */
    data class Available(val opaqueSecret: String) : RemoraLinkSecretReadStatus

    data object Missing : RemoraLinkSecretReadStatus
    data object StorageFailure : RemoraLinkSecretReadStatus
    data object InvalidOpaqueAlias : RemoraLinkSecretReadStatus
}

enum class RemoraLinkSecretMutationStatus {
    STORED,
    DELETED,
    STORAGE_FAILURE,
    /** Atomic move completed but directory durability is unknown; caller must enter repair. */
    PERSISTENCE_INDETERMINATE,
    INVALID_OPAQUE_ALIAS,
}

/** Device-local, authenticated, atomic custody for opaque Remora Link v2 credentials. */
class RemoraLinkSecretStore internal constructor(
    private val backend: RemoraLinkSecretBackend,
) {
    constructor(
        context: Context,
        allowDebugEmulatorSoftwareAssurance: Boolean = false,
    ) : this(
        AndroidAtomicRemoraLinkSecretBackend(
            context = context,
            allowDebugEmulatorSoftwareAssurance = allowDebugEmulatorSoftwareAssurance,
        ),
    )

    fun load(opaqueAlias: String): RemoraLinkSecretReadStatus = synchronized(storageLock) {
        val key = storageKey(opaqueAlias)
            ?: return@synchronized RemoraLinkSecretReadStatus.InvalidOpaqueAlias
        if (key in indeterminateStorageKeys) {
            return@synchronized RemoraLinkSecretReadStatus.StorageFailure
        }
        backend.read(key)
    }

    /** Atomically replaces the durable value before reporting STORED. */
    fun replace(
        opaqueAlias: String,
        opaqueSecret: String,
    ): RemoraLinkSecretMutationStatus = synchronized(storageLock) {
        val key = storageKey(opaqueAlias)
            ?: return@synchronized RemoraLinkSecretMutationStatus.INVALID_OPAQUE_ALIAS
        when (backend.replace(key, opaqueSecret)) {
            SecretWriteResult.COMMITTED -> {
                indeterminateStorageKeys.remove(key)
                RemoraLinkSecretMutationStatus.STORED
            }
            SecretWriteResult.FAILED -> RemoraLinkSecretMutationStatus.STORAGE_FAILURE
            SecretWriteResult.INDETERMINATE -> {
                indeterminateStorageKeys.add(key)
                RemoraLinkSecretMutationStatus.PERSISTENCE_INDETERMINATE
            }
        }
    }

    /** Atomically writes an authenticated tombstone; absence is already the desired state. */
    fun delete(opaqueAlias: String): RemoraLinkSecretMutationStatus = synchronized(storageLock) {
        val key = storageKey(opaqueAlias)
            ?: return@synchronized RemoraLinkSecretMutationStatus.INVALID_OPAQUE_ALIAS
        when (backend.delete(key)) {
            SecretWriteResult.COMMITTED -> {
                indeterminateStorageKeys.remove(key)
                RemoraLinkSecretMutationStatus.DELETED
            }
            SecretWriteResult.FAILED -> RemoraLinkSecretMutationStatus.STORAGE_FAILURE
            SecretWriteResult.INDETERMINATE -> {
                indeterminateStorageKeys.add(key)
                RemoraLinkSecretMutationStatus.PERSISTENCE_INDETERMINATE
            }
        }
    }

    private fun storageKey(opaqueAlias: String): String? =
        if (opaqueAlias.isEmpty()) null else opaqueAliasToken(opaqueAlias)

    companion object {
        internal const val DIRECTORY_NAME = "remora_link_credentials_v2"
        private val storageLock = Any()
        private val indeterminateStorageKeys = mutableSetOf<String>()
    }
}

internal interface RemoraLinkSecretBackend {
    fun read(storageKey: String): RemoraLinkSecretReadStatus
    fun replace(storageKey: String, opaqueSecret: String): SecretWriteResult
    fun delete(storageKey: String): SecretWriteResult
}

internal enum class SecretWriteResult {
    COMMITTED,
    FAILED,
    INDETERMINATE,
}

internal fun interface RemoraLinkSecretWriteFaultInjector {
    fun afterWriteBeforeCommit()
}

internal class AndroidAtomicRemoraLinkSecretBackend(
    context: Context,
    allowDebugEmulatorSoftwareAssurance: Boolean = false,
    private val directory: File = File(context.filesDir, RemoraLinkSecretStore.DIRECTORY_NAME),
    private val faultInjector: RemoraLinkSecretWriteFaultInjector? = null,
    private val directorySyncOverride: (() -> Unit)? = null,
) : RemoraLinkSecretBackend {
    private val debugSoftwareAllowed =
        allowDebugEmulatorSoftwareAssurance && BuildConfig.DEBUG && isProbablyEmulator()

    override fun read(storageKey: String): RemoraLinkSecretReadStatus {
        val file = credentialFile(storageKey)
        if (!file.exists()) return RemoraLinkSecretReadStatus.Missing
        return try {
            require(file.length() in 1..MAX_ENVELOPE_BYTES)
            val envelope = file.inputStream().use { it.readBytes() }
            require(envelope.size.toLong() <= MAX_ENVELOPE_BYTES)
            when (val decoded = decryptEnvelope(loadOrCreateKey(), storageKey, envelope)) {
                is SecretEnvelope.Present -> RemoraLinkSecretReadStatus.Available(decoded.value)
                SecretEnvelope.Tombstone -> RemoraLinkSecretReadStatus.Missing
            }
        } catch (_: Exception) {
            RemoraLinkSecretReadStatus.StorageFailure
        }
    }

    override fun replace(storageKey: String, opaqueSecret: String): SecretWriteResult =
        writeEnvelope(storageKey, SecretEnvelope.Present(opaqueSecret))

    override fun delete(storageKey: String): SecretWriteResult {
        if (!credentialFile(storageKey).exists()) return SecretWriteResult.COMMITTED
        return writeEnvelope(storageKey, SecretEnvelope.Tombstone)
    }

    private fun writeEnvelope(
        storageKey: String,
        envelope: SecretEnvelope,
    ): SecretWriteResult {
        try {
            directory.mkdirs()
            check(directory.isDirectory) { "credential directory unavailable" }
            // Persist the credential-directory entry itself before any envelope can become
            // current. This is required on first use; syncing only the child directory would not
            // make its creation durable in filesDir.
            syncDirectory(checkNotNull(directory.parentFile))
            val encrypted = encryptEnvelope(loadOrCreateKey(), storageKey, envelope)
            require(encrypted.size.toLong() <= MAX_ENVELOPE_BYTES)
            val destination = credentialFile(storageKey)
            val pending = File(directory, "$storageKey.$PENDING_EXTENSION")
            try {
                FileOutputStream(pending, false).use { output ->
                    output.write(encrypted)
                    // Surface writeback and close failures before the atomic move can make this
                    // envelope current.
                    output.fd.sync()
                }
                faultInjector?.afterWriteBeforeCommit()
                // API 26+ atomic move either replaces the complete envelope or throws while the
                // old destination remains authoritative. Do no fallible work after this call.
                java.nio.file.Files.move(
                    pending.toPath(),
                    destination.toPath(),
                    StandardCopyOption.ATOMIC_MOVE,
                    StandardCopyOption.REPLACE_EXISTING,
                )
            } catch (_: Exception) {
                pending.delete()
                return SecretWriteResult.FAILED
            }
        } catch (_: Exception) {
            return SecretWriteResult.FAILED
        }

        return try {
            // The file data and atomic rename are not durably committed until the containing
            // directory is synced. A post-move sync failure is explicitly indeterminate; it must
            // never be reported as STORED/DELETED.
            directorySyncOverride?.invoke() ?: syncDirectory(directory)
            SecretWriteResult.COMMITTED
        } catch (_: Exception) {
            SecretWriteResult.INDETERMINATE
        }
    }

    private fun loadOrCreateKey(): SecretKey {
        val keyStore = KeyStore.getInstance(ANDROID_KEYSTORE).apply { load(null) }
        (keyStore.getKey(KEY_ALIAS, null) as? SecretKey)?.let { key ->
            requireSecureKey(key)
            return key
        }

        // A restored/copied credential file without its non-exportable Keystore key is repair,
        // never an invitation to silently create a different key over existing custody.
        if (directory.listFiles()?.any { it.extension == FILE_EXTENSION } == true) {
            throw IllegalStateException("credential key missing for existing custody")
        }

        val key = generatePreferredKey()
        requireSecureKey(key)
        return key
    }

    private fun generatePreferredKey(): SecretKey {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.P) {
            try {
                return generateKey(requestStrongBox = true)
            } catch (_: InvalidAlgorithmParameterException) {
                deletePartialKey()
            } catch (_: ProviderException) {
                deletePartialKey()
            }
        }
        return generateKey(requestStrongBox = false)
    }

    private fun generateKey(requestStrongBox: Boolean): SecretKey {
        val spec = KeyGenParameterSpec.Builder(
            KEY_ALIAS,
            KeyProperties.PURPOSE_ENCRYPT or KeyProperties.PURPOSE_DECRYPT,
        )
            .setKeySize(256)
            .setBlockModes(KeyProperties.BLOCK_MODE_GCM)
            .setEncryptionPaddings(KeyProperties.ENCRYPTION_PADDING_NONE)
            .setRandomizedEncryptionRequired(true)
            .setUserAuthenticationRequired(false)
            .apply {
                if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.P) {
                    setIsStrongBoxBacked(requestStrongBox)
                }
            }
            .build()
        return KeyGenerator.getInstance(KeyProperties.KEY_ALGORITHM_AES, ANDROID_KEYSTORE).run {
            init(spec)
            generateKey()
        }
    }

    @Suppress("DEPRECATION")
    private fun requireSecureKey(key: SecretKey) {
        val keyInfo: KeyInfo = SecretKeyFactory.getInstance(key.algorithm, ANDROID_KEYSTORE)
            .getKeySpec(key, KeyInfo::class.java) as KeyInfo
        val observation = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.S) {
            when (keyInfo.securityLevel) {
                KeyProperties.SECURITY_LEVEL_STRONGBOX -> SecurityObservation.STRONGBOX
                KeyProperties.SECURITY_LEVEL_TRUSTED_ENVIRONMENT ->
                    SecurityObservation.TRUSTED_ENVIRONMENT
                KeyProperties.SECURITY_LEVEL_UNKNOWN_SECURE -> SecurityObservation.UNKNOWN_SECURE
                KeyProperties.SECURITY_LEVEL_SOFTWARE -> SecurityObservation.SOFTWARE
                else -> SecurityObservation.UNKNOWN
            }
        } else if (keyInfo.isInsideSecureHardware) {
            SecurityObservation.TRUSTED_ENVIRONMENT
        } else {
            SecurityObservation.SOFTWARE
        }
        check(classifyAssurance(observation, debugSoftwareAllowed) != null) {
            "software-backed credential key rejected"
        }
    }

    private fun deletePartialKey() {
        runCatching {
            KeyStore.getInstance(ANDROID_KEYSTORE).apply {
                load(null)
                if (containsAlias(KEY_ALIAS)) deleteEntry(KEY_ALIAS)
            }
        }
    }

    private fun credentialFile(storageKey: String): File =
        File(directory, "$storageKey.$FILE_EXTENSION")

    private fun syncDirectory(directory: File) {
        val descriptor = Os.open(
            directory.absolutePath,
            OsConstants.O_RDONLY,
            0,
        )
        try {
            Os.fsync(descriptor)
        } finally {
            runCatching { Os.close(descriptor) }
        }
    }

    companion object {
        private const val ANDROID_KEYSTORE = "AndroidKeyStore"
        private const val KEY_ALIAS = "com.remora.android.remora_link.v2.secrets.aes"
        private const val FILE_EXTENSION = "rls2"
        private const val PENDING_EXTENSION = "rls2.new"
        private const val MAX_ENVELOPE_BYTES = 1_048_576L
    }
}

internal sealed interface SecretEnvelope {
    data class Present(val value: String) : SecretEnvelope
    data object Tombstone : SecretEnvelope
}

internal fun encryptEnvelope(
    key: SecretKey,
    storageKey: String,
    envelope: SecretEnvelope,
): ByteArray {
    val plaintext = when (envelope) {
        is SecretEnvelope.Present -> byteArrayOf(PRESENT_MARKER) +
            envelope.value.toByteArray(Charsets.UTF_8)
        SecretEnvelope.Tombstone -> byteArrayOf(TOMBSTONE_MARKER)
    }
    val cipher = Cipher.getInstance("AES/GCM/NoPadding")
    cipher.init(Cipher.ENCRYPT_MODE, key)
    cipher.updateAAD(storageKey.toByteArray(Charsets.UTF_8))
    val ciphertext = cipher.doFinal(plaintext)
    return ENVELOPE_MAGIC + byteArrayOf(cipher.iv.size.toByte()) + cipher.iv + ciphertext
}

internal fun decryptEnvelope(
    key: SecretKey,
    storageKey: String,
    envelope: ByteArray,
): SecretEnvelope {
    require(envelope.size > ENVELOPE_MAGIC.size + 1)
    require(envelope.copyOfRange(0, ENVELOPE_MAGIC.size).contentEquals(ENVELOPE_MAGIC))
    val ivSize = envelope[ENVELOPE_MAGIC.size].toInt() and 0xff
    require(ivSize in 12..16)
    val ivStart = ENVELOPE_MAGIC.size + 1
    val ciphertextStart = ivStart + ivSize
    require(ciphertextStart < envelope.size)
    val cipher = Cipher.getInstance("AES/GCM/NoPadding")
    cipher.init(
        Cipher.DECRYPT_MODE,
        key,
        GCMParameterSpec(128, envelope.copyOfRange(ivStart, ciphertextStart)),
    )
    cipher.updateAAD(storageKey.toByteArray(Charsets.UTF_8))
    val plaintext = cipher.doFinal(envelope.copyOfRange(ciphertextStart, envelope.size))
    require(plaintext.isNotEmpty())
    return when (plaintext[0]) {
        PRESENT_MARKER -> SecretEnvelope.Present(
            plaintext.copyOfRange(1, plaintext.size).toString(Charsets.UTF_8),
        )
        TOMBSTONE_MARKER -> {
            require(plaintext.size == 1)
            SecretEnvelope.Tombstone
        }
        else -> throw IllegalArgumentException("unknown credential envelope")
    }
}

private val ENVELOPE_MAGIC = byteArrayOf('R'.code.toByte(), 'L'.code.toByte(), 'S'.code.toByte(), 2)
private const val PRESENT_MARKER: Byte = 1
private const val TOMBSTONE_MARKER: Byte = 2
