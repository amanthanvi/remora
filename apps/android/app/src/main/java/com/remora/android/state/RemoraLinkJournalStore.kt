package com.remora.android.state

import android.content.Context
import android.system.Os
import android.system.OsConstants
import java.io.File
import java.io.FileOutputStream
import java.nio.ByteBuffer
import java.nio.file.StandardCopyOption
import java.security.MessageDigest

/**
 * One app-wide, non-secret Remora Link v2 journal snapshot.
 *
 * [opaquePayload] belongs entirely to shared Rust. Android persists and returns the bytes without
 * parsing or rewriting their contents. The outer revision exists only to implement whole-blob CAS.
 */
class RemoraLinkJournalSnapshot(
    val revision: ULong,
    opaquePayload: ByteArray,
) {
    private val storedPayload = opaquePayload.copyOf()

    val opaquePayload: ByteArray
        get() = storedPayload.copyOf()

    internal val payloadSize: Int
        get() = storedPayload.size

    internal fun copyPayload(): ByteArray = storedPayload.copyOf()

    override fun equals(other: Any?): Boolean =
        other is RemoraLinkJournalSnapshot &&
            revision == other.revision &&
            storedPayload.contentEquals(other.storedPayload)

    override fun hashCode(): Int = 31 * revision.hashCode() + storedPayload.contentHashCode()

    override fun toString(): String =
        "RemoraLinkJournalSnapshot(revision=$revision, opaquePayload=<redacted:${storedPayload.size} bytes>)"
}

sealed interface RemoraLinkJournalLoadStatus {
    data object Missing : RemoraLinkJournalLoadStatus

    class Loaded(val snapshot: RemoraLinkJournalSnapshot) : RemoraLinkJournalLoadStatus {
        override fun equals(other: Any?): Boolean =
            other is Loaded && snapshot == other.snapshot

        override fun hashCode(): Int = snapshot.hashCode()

        override fun toString(): String = "Loaded(snapshot=$snapshot)"
    }

    /** The stored outer envelope is malformed, truncated, oversized, or checksum-invalid. */
    data object Corrupt : RemoraLinkJournalLoadStatus

    /** The platform storage backend could not complete a durable read. */
    data object StorageFailure : RemoraLinkJournalLoadStatus
}

enum class RemoraLinkJournalCasStatus {
    STORED,
    CONFLICT,
    CORRUPT,
    STORAGE_FAILURE,
    INVALID_REPLACEMENT,
}

/**
 * Crash-safe, process-wide CAS for the single opaque Remora Link v2 journal blob.
 *
 * The file backend fsyncs the replacement before an atomic rename and fsyncs the containing
 * directory before reporting success. A post-rename durability failure is fail-closed for the rest
 * of the process, because observing that uncertain revision could fork the journal after restart.
 */
class RemoraLinkJournalStore internal constructor(
    private val backend: RemoraLinkJournalBackend,
) {
    constructor(context: Context) : this(
        AtomicFileRemoraLinkJournalBackend(context.applicationContext),
    )

    fun load(): RemoraLinkJournalLoadStatus = synchronized(journalStorageLock) {
        if (backend.storageIdentity in indeterminateJournalStorage) {
            RemoraLinkJournalLoadStatus.StorageFailure
        } else {
            readCurrent()
        }
    }

    fun compareAndSwap(
        expectedRevision: ULong?,
        replacement: RemoraLinkJournalSnapshot,
    ): RemoraLinkJournalCasStatus = synchronized(journalStorageLock) {
        if (backend.storageIdentity in indeterminateJournalStorage) {
            return@synchronized RemoraLinkJournalCasStatus.STORAGE_FAILURE
        }
        if (!isValidReplacement(expectedRevision, replacement)) {
            return@synchronized RemoraLinkJournalCasStatus.INVALID_REPLACEMENT
        }

        when (val current = readCurrent()) {
            RemoraLinkJournalLoadStatus.Missing -> {
                if (expectedRevision != null) {
                    return@synchronized RemoraLinkJournalCasStatus.CONFLICT
                }
            }

            is RemoraLinkJournalLoadStatus.Loaded -> {
                if (current.snapshot.revision != expectedRevision) {
                    return@synchronized RemoraLinkJournalCasStatus.CONFLICT
                }
            }

            RemoraLinkJournalLoadStatus.Corrupt ->
                return@synchronized RemoraLinkJournalCasStatus.CORRUPT

            RemoraLinkJournalLoadStatus.StorageFailure ->
                return@synchronized RemoraLinkJournalCasStatus.STORAGE_FAILURE
        }

        val envelope = runCatching { encodeRemoraLinkJournalEnvelope(replacement) }
            .getOrElse { return@synchronized RemoraLinkJournalCasStatus.INVALID_REPLACEMENT }
        try {
            when (replaceEnvelope(envelope)) {
                RemoraLinkJournalBackendWrite.COMMITTED -> RemoraLinkJournalCasStatus.STORED
                RemoraLinkJournalBackendWrite.FAILED ->
                    RemoraLinkJournalCasStatus.STORAGE_FAILURE

                RemoraLinkJournalBackendWrite.INDETERMINATE -> {
                    indeterminateJournalStorage.add(backend.storageIdentity)
                    RemoraLinkJournalCasStatus.STORAGE_FAILURE
                }
            }
        } finally {
            envelope.fill(0)
        }
    }

    private fun readCurrent(): RemoraLinkJournalLoadStatus = try {
        decodeBackendRead(backend.read())
    } catch (_: Exception) {
        RemoraLinkJournalLoadStatus.StorageFailure
    }

    private fun replaceEnvelope(envelope: ByteArray): RemoraLinkJournalBackendWrite = try {
        backend.replace(envelope)
    } catch (_: Exception) {
        RemoraLinkJournalBackendWrite.FAILED
    }

    private fun isValidReplacement(
        expectedRevision: ULong?,
        replacement: RemoraLinkJournalSnapshot,
    ): Boolean {
        if (replacement.payloadSize > MAX_REMORA_LINK_JOURNAL_PAYLOAD_BYTES) return false
        val requiredRevision = if (expectedRevision == null) {
            1uL
        } else {
            expectedRevision.takeUnless { it == ULong.MAX_VALUE }?.plus(1uL) ?: return false
        }
        return replacement.revision == requiredRevision
    }
}

internal sealed interface RemoraLinkJournalBackendRead {
    data object Missing : RemoraLinkJournalBackendRead

    class Present(val envelope: ByteArray) : RemoraLinkJournalBackendRead {
        override fun toString(): String =
            "Present(envelope=<redacted:${envelope.size} bytes>)"
    }

    data object Corrupt : RemoraLinkJournalBackendRead
    data object Failed : RemoraLinkJournalBackendRead
}

internal enum class RemoraLinkJournalBackendWrite {
    COMMITTED,
    FAILED,
    INDETERMINATE,
}

internal interface RemoraLinkJournalBackend {
    /** Stable local identifier used to fence post-commit indeterminate state. */
    val storageIdentity: String

    fun read(): RemoraLinkJournalBackendRead
    fun replace(envelope: ByteArray): RemoraLinkJournalBackendWrite
}

internal fun interface RemoraLinkAtomicWriteFaultInjector {
    fun afterWriteBeforeCommit()
}

internal class AtomicFileRemoraLinkJournalBackend(
    context: Context,
    private val directory: File = File(context.noBackupFilesDir, DIRECTORY_NAME),
    private val faultInjector: RemoraLinkAtomicWriteFaultInjector? = null,
    private val directorySyncOverride: (() -> Unit)? = null,
) : RemoraLinkJournalBackend {
    private val journalFile = File(directory, JOURNAL_FILE_NAME)

    override val storageIdentity: String = journalFile.absolutePath

    override fun read(): RemoraLinkJournalBackendRead = try {
        if (!journalFile.exists()) {
            RemoraLinkJournalBackendRead.Missing
        } else if (!journalFile.isFile || journalFile.length() !in MIN_JOURNAL_ENVELOPE_BYTES.toLong()..MAX_JOURNAL_ENVELOPE_BYTES.toLong()) {
            RemoraLinkJournalBackendRead.Corrupt
        } else {
            val envelope = journalFile.inputStream().use { it.readBytes() }
            if (envelope.size !in MIN_JOURNAL_ENVELOPE_BYTES..MAX_JOURNAL_ENVELOPE_BYTES) {
                envelope.fill(0)
                RemoraLinkJournalBackendRead.Corrupt
            } else {
                RemoraLinkJournalBackendRead.Present(envelope)
            }
        }
    } catch (_: Exception) {
        RemoraLinkJournalBackendRead.Failed
    }

    override fun replace(envelope: ByteArray): RemoraLinkJournalBackendWrite {
        var moved = false
        var pending: File? = null
        try {
            ensureDirectoryDurable(directory)
            pending = File.createTempFile(".journal-", ".pending", directory)
            FileOutputStream(pending, false).use { output ->
                output.write(envelope)
                output.fd.sync()
            }
            faultInjector?.afterWriteBeforeCommit()
            java.nio.file.Files.move(
                pending.toPath(),
                journalFile.toPath(),
                StandardCopyOption.ATOMIC_MOVE,
                StandardCopyOption.REPLACE_EXISTING,
            )
            moved = true
        } catch (_: Exception) {
            pending?.delete()
            return RemoraLinkJournalBackendWrite.FAILED
        }

        return try {
            directorySyncOverride?.invoke() ?: syncRemoraLinkDirectory(directory)
            RemoraLinkJournalBackendWrite.COMMITTED
        } catch (_: Exception) {
            check(moved)
            RemoraLinkJournalBackendWrite.INDETERMINATE
        } finally {
            pending?.delete()
        }
    }

    internal companion object {
        const val DIRECTORY_NAME = "remora_link_journal_v2"
        const val JOURNAL_FILE_NAME = "pairing-journal.rlj2"
    }
}

/** Must stay aligned with shared Rust's Remora Link journal parser bound. */
internal const val MAX_REMORA_LINK_JOURNAL_PAYLOAD_BYTES = 2 * 1024 * 1024

private val JOURNAL_ENVELOPE_MAGIC = byteArrayOf(
    'R'.code.toByte(),
    'M'.code.toByte(),
    'L'.code.toByte(),
    'J'.code.toByte(),
    '2'.code.toByte(),
    0,
    0,
    1,
)
private const val JOURNAL_HEADER_BYTES = 8 + Long.SIZE_BYTES + Int.SIZE_BYTES
private const val JOURNAL_CHECKSUM_BYTES = 32
private const val MIN_JOURNAL_ENVELOPE_BYTES = JOURNAL_HEADER_BYTES + JOURNAL_CHECKSUM_BYTES
private const val MAX_JOURNAL_ENVELOPE_BYTES =
    MIN_JOURNAL_ENVELOPE_BYTES + MAX_REMORA_LINK_JOURNAL_PAYLOAD_BYTES
private val journalStorageLock = Any()
private val indeterminateJournalStorage = mutableSetOf<String>()

internal fun encodeRemoraLinkJournalEnvelope(snapshot: RemoraLinkJournalSnapshot): ByteArray {
    require(snapshot.revision != 0uL) { "journal revision must be nonzero" }
    require(snapshot.payloadSize <= MAX_REMORA_LINK_JOURNAL_PAYLOAD_BYTES) {
        "journal payload exceeds storage limit"
    }
    val payload = snapshot.copyPayload()
    val authenticated = ByteBuffer.allocate(JOURNAL_HEADER_BYTES + payload.size)
        .put(JOURNAL_ENVELOPE_MAGIC)
        .putLong(snapshot.revision.toLong())
        .putInt(payload.size)
        .put(payload)
        .array()
    payload.fill(0)
    val checksum = MessageDigest.getInstance("SHA-256").digest(authenticated)
    val envelope = authenticated + checksum
    authenticated.fill(0)
    checksum.fill(0)
    return envelope
}

internal fun decodeRemoraLinkJournalEnvelope(envelope: ByteArray): RemoraLinkJournalSnapshot? {
    if (envelope.size !in MIN_JOURNAL_ENVELOPE_BYTES..MAX_JOURNAL_ENVELOPE_BYTES) return null
    val buffer = ByteBuffer.wrap(envelope)
    val magic = ByteArray(JOURNAL_ENVELOPE_MAGIC.size).also(buffer::get)
    if (!magic.contentEquals(JOURNAL_ENVELOPE_MAGIC)) return null
    val revision = buffer.long.toULong()
    if (revision == 0uL) return null
    val payloadSize = buffer.int
    if (payloadSize !in 0..MAX_REMORA_LINK_JOURNAL_PAYLOAD_BYTES) return null
    val expectedSize = JOURNAL_HEADER_BYTES.toLong() + payloadSize + JOURNAL_CHECKSUM_BYTES
    if (expectedSize != envelope.size.toLong()) return null

    val authenticatedSize = JOURNAL_HEADER_BYTES + payloadSize
    val expectedChecksum = envelope.copyOfRange(authenticatedSize, envelope.size)
    val authenticated = envelope.copyOfRange(0, authenticatedSize)
    val actualChecksum = MessageDigest.getInstance("SHA-256").digest(authenticated)
    authenticated.fill(0)
    val checksumMatches = MessageDigest.isEqual(expectedChecksum, actualChecksum)
    expectedChecksum.fill(0)
    actualChecksum.fill(0)
    if (!checksumMatches) return null

    val payload = envelope.copyOfRange(JOURNAL_HEADER_BYTES, authenticatedSize)
    return RemoraLinkJournalSnapshot(revision, payload).also { payload.fill(0) }
}

private fun decodeBackendRead(read: RemoraLinkJournalBackendRead): RemoraLinkJournalLoadStatus =
    when (read) {
        RemoraLinkJournalBackendRead.Missing -> RemoraLinkJournalLoadStatus.Missing
        is RemoraLinkJournalBackendRead.Present -> try {
            decodeRemoraLinkJournalEnvelope(read.envelope)
                ?.let(RemoraLinkJournalLoadStatus::Loaded)
                ?: RemoraLinkJournalLoadStatus.Corrupt
        } finally {
            read.envelope.fill(0)
        }

        RemoraLinkJournalBackendRead.Corrupt -> RemoraLinkJournalLoadStatus.Corrupt
        RemoraLinkJournalBackendRead.Failed -> RemoraLinkJournalLoadStatus.StorageFailure
    }

internal fun ensureDirectoryDurable(directory: File) {
    if (directory.isDirectory) return
    check(!directory.exists()) { "storage path is not a directory" }
    check(directory.mkdirs() && directory.isDirectory) { "unable to create storage directory" }
    syncRemoraLinkDirectory(checkNotNull(directory.parentFile))
}

internal fun syncRemoraLinkDirectory(directory: File) {
    val descriptor = Os.open(directory.absolutePath, OsConstants.O_RDONLY, 0)
    try {
        Os.fsync(descriptor)
    } finally {
        runCatching { Os.close(descriptor) }
    }
}
