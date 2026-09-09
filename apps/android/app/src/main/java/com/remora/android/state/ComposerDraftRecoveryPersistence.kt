package com.remora.android.state

import java.io.File
import java.io.FileOutputStream
import java.nio.file.Files
import java.nio.file.StandardCopyOption

internal interface ComposerDraftRecoveryPersistence {
    fun read(): ByteArray?
    fun replace(plaintext: ByteArray)
}

/** The encryptor and directory fsync are platform adapters; the commit path is JVM-testable. */
internal class AtomicComposerDraftRecoveryPersistence(
    private val directory: File,
    private val encrypt: (ByteArray) -> ByteArray,
    private val decrypt: (ByteArray) -> ByteArray,
    private val syncDirectory: (File) -> Unit,
    private val beforeCommit: () -> Unit = {},
) : ComposerDraftRecoveryPersistence {
    private val file = File(directory, FILE_NAME)

    override fun read(): ByteArray? {
        if (!file.exists()) return null
        check(file.isFile && file.length() in 1..MAX_ARCHIVE_BYTES.toLong())
        return file.readBytes().let { encrypted ->
            try {
                decrypt(encrypted).also { check(it.size <= MAX_ARCHIVE_BYTES) }
            } finally {
                encrypted.fill(0)
            }
        }
    }

    override fun replace(plaintext: ByteArray) {
        require(plaintext.size <= MAX_ARCHIVE_BYTES)
        if (!directory.isDirectory) {
            check(!directory.exists() && directory.mkdirs())
            syncDirectory(checkNotNull(directory.parentFile))
        }
        val encrypted = encrypt(plaintext)
        require(encrypted.size <= MAX_ARCHIVE_BYTES)
        var pending: File? = null
        try {
            pending = File.createTempFile(".drafts-", ".pending", directory)
            FileOutputStream(pending).use { output ->
                output.write(encrypted)
                output.fd.sync()
            }
            beforeCommit()
            Files.move(pending.toPath(), file.toPath(), StandardCopyOption.ATOMIC_MOVE,
                StandardCopyOption.REPLACE_EXISTING)
            syncDirectory(directory)
        } finally {
            encrypted.fill(0)
            // A failed write never removes or resets the previous recovery file.
            pending?.delete()
        }
    }

    companion object {
        const val DIRECTORY_NAME = "composer_recovery_v1"
        const val FILE_NAME = "drafts-v1.enc"
        const val MAX_ARCHIVE_BYTES = 32 * 1024 * 1024
    }
}
