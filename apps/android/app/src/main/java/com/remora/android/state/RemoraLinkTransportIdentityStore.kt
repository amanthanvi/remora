package com.remora.android.state

import android.content.Context
import android.os.Build
import android.security.keystore.KeyGenParameterSpec
import android.security.keystore.KeyProperties
import java.io.File
import java.io.FileOutputStream
import java.nio.file.StandardCopyOption
import java.security.InvalidAlgorithmParameterException
import java.security.KeyStore
import java.security.ProviderException
import javax.crypto.AEADBadTagException
import javax.crypto.Cipher
import javax.crypto.KeyGenerator
import javax.crypto.SecretKey
import javax.crypto.spec.GCMParameterSpec

/** Result of atomically loading or creating the app-wide Remora Link v2 transport identity. */
sealed interface RemoraLinkTransportIdentityStatus {
    class Ready(
        identityBytes: ByteArray,
        val created: Boolean,
    ) : RemoraLinkTransportIdentityStatus {
        private val storedBytes = identityBytes.copyOf()

        val identityBytes: ByteArray
            get() = storedBytes.copyOf()

        override fun equals(other: Any?): Boolean =
            other is Ready && created == other.created && storedBytes.contentEquals(other.storedBytes)

        override fun hashCode(): Int = 31 * storedBytes.contentHashCode() + created.hashCode()

        override fun toString(): String =
            "Ready(identityBytes=<redacted:${storedBytes.size} bytes>, created=$created)"
    }

    data object InvalidCandidate : RemoraLinkTransportIdentityStatus
    data object Corrupt : RemoraLinkTransportIdentityStatus
    data object StorageFailure : RemoraLinkTransportIdentityStatus
}

/**
 * Atomic custody for the single 32-byte Remora Link v2 Iroh transport secret.
 *
 * This namespace is intentionally brand-new. It never reads, imports, or rewrites the former v1
 * endpoint key. The file is authenticated and encrypted by a dedicated, non-exportable Android
 * Keystore AES key and lives under no-backup storage. The store's app-wide lock serializes the
 * load/create transaction, while the backend publishes the fsynced ciphertext by atomic rename.
 */
class RemoraLinkTransportIdentityStore internal constructor(
    private val backend: RemoraLinkTransportIdentityBackend,
) {
    constructor(context: Context) : this(
        AtomicEncryptedRemoraLinkTransportIdentityBackend(context.applicationContext),
    )

    fun loadOrCreate(candidate: ByteArray): RemoraLinkTransportIdentityStatus {
        if (candidate.size != REMORA_LINK_TRANSPORT_IDENTITY_BYTES) {
            return RemoraLinkTransportIdentityStatus.InvalidCandidate
        }
        // One owned snapshot feeds both persistence and the returned value. A caller mutating its
        // array after this boundary cannot make durable and returned identities diverge.
        val ownedCandidate = candidate.copyOf()
        return try {
            synchronized(transportIdentityStorageLock) {
                if (backend.storageIdentity in indeterminateTransportIdentityStorage) {
                    return@synchronized RemoraLinkTransportIdentityStatus.StorageFailure
                }
                when (val existing = readExisting()) {
                    RemoraLinkTransportIdentityRead.Missing ->
                        createOrAdopt(ownedCandidate)

                    is RemoraLinkTransportIdentityRead.Available ->
                        existing.toReady(created = false)

                    RemoraLinkTransportIdentityRead.Corrupt ->
                        RemoraLinkTransportIdentityStatus.Corrupt

                    RemoraLinkTransportIdentityRead.StorageFailure ->
                        RemoraLinkTransportIdentityStatus.StorageFailure
                }
            }
        } finally {
            ownedCandidate.fill(0)
        }
    }

    private fun createOrAdopt(
        ownedCandidate: ByteArray,
    ): RemoraLinkTransportIdentityStatus = when (createIdentity(ownedCandidate)) {
        RemoraLinkTransportIdentityBackendWrite.COMMITTED ->
            RemoraLinkTransportIdentityStatus.Ready(ownedCandidate, created = true)

        RemoraLinkTransportIdentityBackendWrite.ALREADY_EXISTS ->
            when (val winner = readExisting()) {
                is RemoraLinkTransportIdentityRead.Available -> winner.toReady(created = false)
                RemoraLinkTransportIdentityRead.Corrupt -> RemoraLinkTransportIdentityStatus.Corrupt
                RemoraLinkTransportIdentityRead.Missing,
                RemoraLinkTransportIdentityRead.StorageFailure,
                -> RemoraLinkTransportIdentityStatus.StorageFailure
            }

        RemoraLinkTransportIdentityBackendWrite.FAILED ->
            RemoraLinkTransportIdentityStatus.StorageFailure

        RemoraLinkTransportIdentityBackendWrite.INDETERMINATE -> {
            indeterminateTransportIdentityStorage.add(backend.storageIdentity)
            RemoraLinkTransportIdentityStatus.StorageFailure
        }
    }

    private fun readExisting(): RemoraLinkTransportIdentityRead = try {
        validatedIdentityRead(backend.read())
    } catch (_: Exception) {
        RemoraLinkTransportIdentityRead.StorageFailure
    }

    private fun createIdentity(
        ownedCandidate: ByteArray,
    ): RemoraLinkTransportIdentityBackendWrite = try {
        backend.create(ownedCandidate)
    } catch (_: Exception) {
        RemoraLinkTransportIdentityBackendWrite.FAILED
    }
}

internal sealed interface RemoraLinkTransportIdentityBackendRead {
    data object Missing : RemoraLinkTransportIdentityBackendRead

    class Present(val identityBytes: ByteArray) : RemoraLinkTransportIdentityBackendRead {
        override fun toString(): String =
            "Present(identityBytes=<redacted:${identityBytes.size} bytes>)"
    }

    data object Corrupt : RemoraLinkTransportIdentityBackendRead
    data object Failed : RemoraLinkTransportIdentityBackendRead
}

internal enum class RemoraLinkTransportIdentityBackendWrite {
    COMMITTED,
    ALREADY_EXISTS,
    FAILED,
    INDETERMINATE,
}

internal interface RemoraLinkTransportIdentityBackend {
    /** Stable local identifier used to fence post-commit indeterminate state. */
    val storageIdentity: String

    fun read(): RemoraLinkTransportIdentityBackendRead
    fun create(identityBytes: ByteArray): RemoraLinkTransportIdentityBackendWrite
}

internal class AtomicEncryptedRemoraLinkTransportIdentityBackend(
    context: Context,
    private val directory: File = File(context.noBackupFilesDir, DIRECTORY_NAME),
    private val keyAlias: String = KEY_ALIAS,
    private val faultInjector: RemoraLinkAtomicWriteFaultInjector? = null,
    private val directorySyncOverride: (() -> Unit)? = null,
) : RemoraLinkTransportIdentityBackend {
    private val identityFile = File(directory, IDENTITY_FILE_NAME)

    override val storageIdentity: String = identityFile.absolutePath

    override fun read(): RemoraLinkTransportIdentityBackendRead = try {
        if (!identityFile.exists()) {
            RemoraLinkTransportIdentityBackendRead.Missing
        } else if (!identityFile.isFile || identityFile.length() !in MIN_ENVELOPE_BYTES.toLong()..MAX_ENVELOPE_BYTES.toLong()) {
            RemoraLinkTransportIdentityBackendRead.Corrupt
        } else {
            val envelope = identityFile.inputStream().use { it.readBytes() }
            try {
                val key = loadExistingKey()
                if (key == null) {
                    RemoraLinkTransportIdentityBackendRead.Corrupt
                } else {
                    decryptTransportIdentityEnvelope(key, envelope)
                        ?.let(RemoraLinkTransportIdentityBackendRead::Present)
                        ?: RemoraLinkTransportIdentityBackendRead.Corrupt
                }
            } finally {
                envelope.fill(0)
            }
        }
    } catch (_: AEADBadTagException) {
        RemoraLinkTransportIdentityBackendRead.Corrupt
    } catch (_: Exception) {
        RemoraLinkTransportIdentityBackendRead.Failed
    }

    override fun create(identityBytes: ByteArray): RemoraLinkTransportIdentityBackendWrite {
        require(identityBytes.size == REMORA_LINK_TRANSPORT_IDENTITY_BYTES)

        var moved = false
        var pending: File? = null
        var encrypted: ByteArray? = null
        try {
            if (identityFile.exists()) {
                return RemoraLinkTransportIdentityBackendWrite.ALREADY_EXISTS
            }
            ensureDirectoryDurable(directory)
            if (identityFile.exists()) return RemoraLinkTransportIdentityBackendWrite.ALREADY_EXISTS
            val key = loadOrCreateKeyForNewIdentity()
            encrypted = encryptTransportIdentityEnvelope(key, identityBytes)
            pending = File.createTempFile(".transport-identity-", ".pending", directory)
            FileOutputStream(pending, false).use { output ->
                output.write(encrypted)
                output.fd.sync()
            }
            faultInjector?.afterWriteBeforeCommit()
            // The public store holds transportIdentityStorageLock from its authoritative read
            // through this write. Remora has no secondary app process, so this second check plus
            // the atomic move is the create-if-absent boundary for every supported caller. Android
            // app SELinux denies hard-link publication even within no-backup storage.
            if (identityFile.exists()) {
                return RemoraLinkTransportIdentityBackendWrite.ALREADY_EXISTS
            }
            java.nio.file.Files.move(
                pending.toPath(),
                identityFile.toPath(),
                StandardCopyOption.ATOMIC_MOVE,
                StandardCopyOption.REPLACE_EXISTING,
            )
            moved = true
        } catch (_: Exception) {
            return RemoraLinkTransportIdentityBackendWrite.FAILED
        } finally {
            encrypted?.fill(0)
            pending?.delete()
        }

        return try {
            directorySyncOverride?.invoke() ?: syncRemoraLinkDirectory(directory)
            RemoraLinkTransportIdentityBackendWrite.COMMITTED
        } catch (_: Exception) {
            check(moved)
            RemoraLinkTransportIdentityBackendWrite.INDETERMINATE
        }
    }

    private fun loadExistingKey(): SecretKey? =
        KeyStore.getInstance(ANDROID_KEYSTORE).apply { load(null) }
            .getKey(keyAlias, null) as? SecretKey

    private fun loadOrCreateKeyForNewIdentity(): SecretKey {
        loadExistingKey()?.let { return it }
        check(!identityFile.exists()) { "transport identity key missing for existing custody" }
        return generatePreferredKey()
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
        val specification = KeyGenParameterSpec.Builder(
            keyAlias,
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
            init(specification)
            generateKey().also { key -> check(key.encoded == null) }
        }
    }

    private fun deletePartialKey() {
        runCatching {
            KeyStore.getInstance(ANDROID_KEYSTORE).apply {
                load(null)
                if (containsAlias(keyAlias)) deleteEntry(keyAlias)
            }
        }
    }

    internal companion object {
        const val DIRECTORY_NAME = "remora_link_transport_identity_v2"
        const val IDENTITY_FILE_NAME = "transport-identity.rlt2"
        const val KEY_ALIAS = "com.remora.android.remora_link.v2.transport_identity.aes"
        private const val ANDROID_KEYSTORE = "AndroidKeyStore"
    }
}

internal const val REMORA_LINK_TRANSPORT_IDENTITY_BYTES = 32
private const val GCM_TAG_BYTES = 16
private const val MIN_GCM_IV_BYTES = 12
private const val MAX_GCM_IV_BYTES = 16
private const val MIN_ENVELOPE_BYTES = 8 + 1 + MIN_GCM_IV_BYTES + REMORA_LINK_TRANSPORT_IDENTITY_BYTES + GCM_TAG_BYTES
private const val MAX_ENVELOPE_BYTES = 8 + 1 + MAX_GCM_IV_BYTES + REMORA_LINK_TRANSPORT_IDENTITY_BYTES + GCM_TAG_BYTES
private val TRANSPORT_IDENTITY_MAGIC = byteArrayOf(
    'R'.code.toByte(),
    'M'.code.toByte(),
    'L'.code.toByte(),
    'T'.code.toByte(),
    '2'.code.toByte(),
    0,
    0,
    1,
)
private val TRANSPORT_IDENTITY_AAD = "com.remora.android.remora-link.v2.transport-identity".toByteArray()
private val transportIdentityStorageLock = Any()
private val indeterminateTransportIdentityStorage = mutableSetOf<String>()

private sealed interface RemoraLinkTransportIdentityRead {
    data object Missing : RemoraLinkTransportIdentityRead
    data class Available(val bytes: ByteArray) : RemoraLinkTransportIdentityRead
    data object Corrupt : RemoraLinkTransportIdentityRead
    data object StorageFailure : RemoraLinkTransportIdentityRead
}

private fun RemoraLinkTransportIdentityRead.Available.toReady(
    created: Boolean,
): RemoraLinkTransportIdentityStatus.Ready =
    RemoraLinkTransportIdentityStatus.Ready(bytes, created).also { bytes.fill(0) }

private fun validatedIdentityRead(
    read: RemoraLinkTransportIdentityBackendRead,
): RemoraLinkTransportIdentityRead = when (read) {
    RemoraLinkTransportIdentityBackendRead.Missing -> RemoraLinkTransportIdentityRead.Missing
    is RemoraLinkTransportIdentityBackendRead.Present -> {
        val bytes = read.identityBytes
        if (bytes.size != REMORA_LINK_TRANSPORT_IDENTITY_BYTES) {
            bytes.fill(0)
            RemoraLinkTransportIdentityRead.Corrupt
        } else {
            RemoraLinkTransportIdentityRead.Available(bytes)
        }
    }

    RemoraLinkTransportIdentityBackendRead.Corrupt -> RemoraLinkTransportIdentityRead.Corrupt
    RemoraLinkTransportIdentityBackendRead.Failed -> RemoraLinkTransportIdentityRead.StorageFailure
}

private fun encryptTransportIdentityEnvelope(
    key: SecretKey,
    identityBytes: ByteArray,
): ByteArray {
    val cipher = Cipher.getInstance("AES/GCM/NoPadding")
    cipher.init(Cipher.ENCRYPT_MODE, key)
    cipher.updateAAD(TRANSPORT_IDENTITY_AAD)
    val iv = cipher.iv
    val ciphertext = cipher.doFinal(identityBytes)
    return try {
        require(iv.size in MIN_GCM_IV_BYTES..MAX_GCM_IV_BYTES)
        TRANSPORT_IDENTITY_MAGIC + byteArrayOf(iv.size.toByte()) + iv + ciphertext
    } finally {
        iv.fill(0)
        ciphertext.fill(0)
    }
}

private fun decryptTransportIdentityEnvelope(
    key: SecretKey,
    envelope: ByteArray,
): ByteArray? {
    if (envelope.size !in MIN_ENVELOPE_BYTES..MAX_ENVELOPE_BYTES) return null
    if (!envelope.copyOfRange(0, TRANSPORT_IDENTITY_MAGIC.size).contentEquals(TRANSPORT_IDENTITY_MAGIC)) {
        return null
    }
    val ivSize = envelope[TRANSPORT_IDENTITY_MAGIC.size].toInt() and 0xff
    if (ivSize !in MIN_GCM_IV_BYTES..MAX_GCM_IV_BYTES) return null
    val ivStart = TRANSPORT_IDENTITY_MAGIC.size + 1
    val ciphertextStart = ivStart + ivSize
    if (ciphertextStart >= envelope.size) return null
    val iv = envelope.copyOfRange(ivStart, ciphertextStart)
    val ciphertext = envelope.copyOfRange(ciphertextStart, envelope.size)
    return try {
        val cipher = Cipher.getInstance("AES/GCM/NoPadding")
        cipher.init(Cipher.DECRYPT_MODE, key, GCMParameterSpec(128, iv))
        cipher.updateAAD(TRANSPORT_IDENTITY_AAD)
        val plaintext = cipher.doFinal(ciphertext)
        if (plaintext.size == REMORA_LINK_TRANSPORT_IDENTITY_BYTES) {
            plaintext
        } else {
            plaintext.fill(0)
            null
        }
    } finally {
        iv.fill(0)
        ciphertext.fill(0)
    }
}
