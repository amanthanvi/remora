package com.remora.android.state

import android.content.Context
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import java.io.File
import java.io.IOException
import java.security.KeyStore
import java.util.concurrent.CyclicBarrier
import java.util.concurrent.Executors
import java.util.concurrent.TimeUnit
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith

@RunWith(AndroidJUnit4::class)
class RemoraLinkNativeStoresInstrumentationTest {
    @Test
    fun missingDeviceKeyLoadNeverCreatesAKey() {
        val slot = "instrumentation-missing-${System.nanoTime()}"
        val provider = RemoraLinkDeviceKeyProvider(
            allowDebugEmulatorSoftwareAssurance = true,
        )
        try {
            assertEquals(
                RemoraLinkDeviceKeyDeletionStatus.ALREADY_MISSING,
                provider.deleteKey(slot),
            )
            val first = provider.loadKey(slot) as RemoraLinkDeviceKeyStatus.Unavailable
            val second = provider.loadKey(slot) as RemoraLinkDeviceKeyStatus.Unavailable
            assertEquals(RemoraLinkDeviceKeyFailure.KEY_NOT_FOUND, first.failure)
            assertEquals(RemoraLinkDeviceKeyFailure.KEY_NOT_FOUND, second.failure)
            assertEquals(
                RemoraLinkDeviceKeyDeletionStatus.ALREADY_MISSING,
                provider.deleteKey(slot),
            )
        } finally {
            provider.deleteKey(slot)
        }
    }

    @Test
    fun defaultStoresUseAndroidNoBackupStorage() {
        val context = targetContext()
        val noBackupDirectory = context.noBackupFilesDir.canonicalFile
        val journal = AtomicFileRemoraLinkJournalBackend(context)
        val identity = AtomicEncryptedRemoraLinkTransportIdentityBackend(context)

        assertEquals(
            noBackupDirectory,
            File(journal.storageIdentity).canonicalFile.parentFile?.parentFile,
        )
        assertEquals(
            noBackupDirectory,
            File(identity.storageIdentity).canonicalFile.parentFile?.parentFile,
        )
    }

    @Test
    fun journalPersistsWholeSnapshotAcrossBackendReconstruction() {
        val context = targetContext()
        val directory = testDirectory(context, "journal-round-trip")
        val payload = byteArrayOf(0, 1, -1, 4, 9, 16)
        try {
            val store = RemoraLinkJournalStore(
                AtomicFileRemoraLinkJournalBackend(context, directory),
            )
            assertEquals(
                RemoraLinkJournalCasStatus.STORED,
                store.compareAndSwap(null, RemoraLinkJournalSnapshot(1uL, payload)),
            )

            val reconstructed = RemoraLinkJournalStore(
                AtomicFileRemoraLinkJournalBackend(context, directory),
            )
            val loaded = reconstructed.load() as RemoraLinkJournalLoadStatus.Loaded
            assertEquals(1uL, loaded.snapshot.revision)
            assertArrayEquals(payload, loaded.snapshot.opaquePayload)
        } finally {
            directory.deleteRecursively()
        }
    }

    @Test
    fun journalRejectsCorruptFileWithoutOverwritingIt() {
        val context = targetContext()
        val directory = testDirectory(context, "journal-corrupt")
        val corrupt = "broken-journal".toByteArray()
        try {
            assertTrue(directory.mkdirs())
            val file = File(directory, AtomicFileRemoraLinkJournalBackend.JOURNAL_FILE_NAME)
            file.writeBytes(corrupt)

            val store = RemoraLinkJournalStore(
                AtomicFileRemoraLinkJournalBackend(context, directory),
            )
            assertEquals(RemoraLinkJournalLoadStatus.Corrupt, store.load())
            assertEquals(
                RemoraLinkJournalCasStatus.CORRUPT,
                store.compareAndSwap(null, RemoraLinkJournalSnapshot(1uL, byteArrayOf(1))),
            )
            assertArrayEquals(corrupt, file.readBytes())
        } finally {
            directory.deleteRecursively()
        }
    }

    @Test
    fun journalPreCommitFailurePreservesPriorDurableRevision() {
        val context = targetContext()
        val directory = testDirectory(context, "journal-failure")
        val normal = RemoraLinkJournalStore(
            AtomicFileRemoraLinkJournalBackend(context, directory),
        )
        try {
            assertEquals(
                RemoraLinkJournalCasStatus.STORED,
                normal.compareAndSwap(null, RemoraLinkJournalSnapshot(1uL, byteArrayOf(1))),
            )
            val failing = RemoraLinkJournalStore(
                AtomicFileRemoraLinkJournalBackend(
                    context = context,
                    directory = directory,
                    faultInjector = RemoraLinkAtomicWriteFaultInjector {
                        throw IOException("injected before atomic rename")
                    },
                ),
            )
            assertEquals(
                RemoraLinkJournalCasStatus.STORAGE_FAILURE,
                failing.compareAndSwap(1uL, RemoraLinkJournalSnapshot(2uL, byteArrayOf(2))),
            )

            val reconstructed = RemoraLinkJournalStore(
                AtomicFileRemoraLinkJournalBackend(context, directory),
            )
            val loaded = reconstructed.load() as RemoraLinkJournalLoadStatus.Loaded
            assertEquals(1uL, loaded.snapshot.revision)
            assertArrayEquals(byteArrayOf(1), loaded.snapshot.opaquePayload)

            assertEquals(
                RemoraLinkJournalCasStatus.STORED,
                reconstructed.compareAndSwap(
                    1uL,
                    RemoraLinkJournalSnapshot(2uL, byteArrayOf(2)),
                ),
            )
            val recovered = reconstructed.load() as RemoraLinkJournalLoadStatus.Loaded
            assertEquals(2uL, recovered.snapshot.revision)
            assertArrayEquals(byteArrayOf(2), recovered.snapshot.opaquePayload)
        } finally {
            directory.deleteRecursively()
        }
    }

    @Test
    fun journalPostRenameSyncFailureFencesSamePathAcrossStoreReconstruction() {
        val context = targetContext()
        val directory = testDirectory(context, "journal-indeterminate")
        try {
            val normal = RemoraLinkJournalStore(
                AtomicFileRemoraLinkJournalBackend(context, directory),
            )
            assertEquals(
                RemoraLinkJournalCasStatus.STORED,
                normal.compareAndSwap(null, RemoraLinkJournalSnapshot(1uL, byteArrayOf(1))),
            )

            val indeterminate = RemoraLinkJournalStore(
                AtomicFileRemoraLinkJournalBackend(
                    context = context,
                    directory = directory,
                    directorySyncOverride = { throw IOException("injected directory fsync failure") },
                ),
            )
            assertEquals(
                RemoraLinkJournalCasStatus.STORAGE_FAILURE,
                indeterminate.compareAndSwap(
                    1uL,
                    RemoraLinkJournalSnapshot(2uL, byteArrayOf(2)),
                ),
            )
            assertEquals(RemoraLinkJournalLoadStatus.StorageFailure, indeterminate.load())
            assertEquals(
                RemoraLinkJournalLoadStatus.StorageFailure,
                RemoraLinkJournalStore(
                    AtomicFileRemoraLinkJournalBackend(context, directory),
                ).load(),
            )

            val raw = AtomicFileRemoraLinkJournalBackend(context, directory).read()
                as RemoraLinkJournalBackendRead.Present
            val movedSnapshot = decodeRemoraLinkJournalEnvelope(raw.envelope)
            assertEquals(2uL, movedSnapshot?.revision)
            raw.envelope.fill(0)
        } finally {
            directory.deleteRecursively()
        }
    }

    @Test
    fun journalRealFileCasHasExactlyOneWinnerAcrossStoreInstances() {
        val context = targetContext()
        val directory = testDirectory(context, "journal-race")
        try {
            val stores = List(16) {
                RemoraLinkJournalStore(AtomicFileRemoraLinkJournalBackend(context, directory))
            }
            val outcomes = race(stores.size) { index ->
                stores[index].compareAndSwap(
                    null,
                    RemoraLinkJournalSnapshot(1uL, byteArrayOf(index.toByte())),
                )
            }

            assertEquals(1, outcomes.count { it == RemoraLinkJournalCasStatus.STORED })
            assertEquals(15, outcomes.count { it == RemoraLinkJournalCasStatus.CONFLICT })
            val loaded = stores.first().load() as RemoraLinkJournalLoadStatus.Loaded
            assertEquals(1uL, loaded.snapshot.revision)
            assertEquals(1, loaded.snapshot.opaquePayload.size)
        } finally {
            directory.deleteRecursively()
        }
    }

    @Test
    fun transportIdentityIsDeviceBoundEncryptedAndDurable() {
        val context = targetContext()
        val directory = testDirectory(context, "identity-round-trip")
        val keyAlias = testKeyAlias("identity-round-trip")
        val candidate = ByteArray(REMORA_LINK_TRANSPORT_IDENTITY_BYTES) { (it + 1).toByte() }
        val losingCandidate = ByteArray(REMORA_LINK_TRANSPORT_IDENTITY_BYTES) { 0x55 }
        try {
            assertEquals(
                "remora_link_transport_identity_v2",
                AtomicEncryptedRemoraLinkTransportIdentityBackend.DIRECTORY_NAME,
            )
            val created = RemoraLinkTransportIdentityStore(
                AtomicEncryptedRemoraLinkTransportIdentityBackend(
                    context = context,
                    directory = directory,
                    keyAlias = keyAlias,
                ),
            ).loadOrCreate(candidate) as RemoraLinkTransportIdentityStatus.Ready
            assertTrue(created.created)
            assertArrayEquals(candidate, created.identityBytes)

            val reconstructed = RemoraLinkTransportIdentityStore(
                AtomicEncryptedRemoraLinkTransportIdentityBackend(
                    context = context,
                    directory = directory,
                    keyAlias = keyAlias,
                ),
            ).loadOrCreate(losingCandidate) as RemoraLinkTransportIdentityStatus.Ready
            assertFalse(reconstructed.created)
            assertArrayEquals(candidate, reconstructed.identityBytes)

            val ciphertext = File(
                directory,
                AtomicEncryptedRemoraLinkTransportIdentityBackend.IDENTITY_FILE_NAME,
            ).readBytes()
            assertFalse(ciphertext.containsSubsequence(candidate))
            val key = KeyStore.getInstance("AndroidKeyStore").apply { load(null) }
                .getKey(keyAlias, null)
            assertNull(key.encoded)
        } finally {
            directory.deleteRecursively()
            deleteTestKey(keyAlias)
        }
    }

    @Test
    fun transportIdentityPreCommitFailureNeverPublishesCandidate() {
        val context = targetContext()
        val directory = testDirectory(context, "identity-failure")
        val keyAlias = testKeyAlias("identity-failure")
        val candidate = ByteArray(REMORA_LINK_TRANSPORT_IDENTITY_BYTES) { 0x11 }
        val retryCandidate = ByteArray(REMORA_LINK_TRANSPORT_IDENTITY_BYTES) { 0x22 }
        try {
            val failing = RemoraLinkTransportIdentityStore(
                AtomicEncryptedRemoraLinkTransportIdentityBackend(
                    context = context,
                    directory = directory,
                    keyAlias = keyAlias,
                    faultInjector = RemoraLinkAtomicWriteFaultInjector {
                        throw IOException("injected before atomic rename")
                    },
                ),
            )
            assertEquals(
                RemoraLinkTransportIdentityStatus.StorageFailure,
                failing.loadOrCreate(candidate),
            )
            assertFalse(
                File(
                    directory,
                    AtomicEncryptedRemoraLinkTransportIdentityBackend.IDENTITY_FILE_NAME,
                ).exists(),
            )

            val recovered = RemoraLinkTransportIdentityStore(
                AtomicEncryptedRemoraLinkTransportIdentityBackend(
                    context = context,
                    directory = directory,
                    keyAlias = keyAlias,
                ),
            ).loadOrCreate(retryCandidate) as RemoraLinkTransportIdentityStatus.Ready
            assertTrue(recovered.created)
            assertArrayEquals(retryCandidate, recovered.identityBytes)
        } finally {
            directory.deleteRecursively()
            deleteTestKey(keyAlias)
        }
    }

    @Test
    fun transportIdentityRejectsCiphertextTamperingWithoutReplacement() {
        val context = targetContext()
        val directory = testDirectory(context, "identity-tamper")
        val keyAlias = testKeyAlias("identity-tamper")
        val candidate = ByteArray(REMORA_LINK_TRANSPORT_IDENTITY_BYTES) { (it + 1).toByte() }
        val replacement = ByteArray(REMORA_LINK_TRANSPORT_IDENTITY_BYTES) { 0x66 }
        try {
            val backend = AtomicEncryptedRemoraLinkTransportIdentityBackend(
                context = context,
                directory = directory,
                keyAlias = keyAlias,
            )
            val created = RemoraLinkTransportIdentityStore(backend).loadOrCreate(candidate)
            assertTrue(created is RemoraLinkTransportIdentityStatus.Ready)

            val file = File(
                directory,
                AtomicEncryptedRemoraLinkTransportIdentityBackend.IDENTITY_FILE_NAME,
            )
            val tampered = file.readBytes().apply {
                this[lastIndex] = (this[lastIndex].toInt() xor 0x01).toByte()
            }
            file.writeBytes(tampered)
            assertEquals(
                RemoraLinkTransportIdentityStatus.Corrupt,
                RemoraLinkTransportIdentityStore(backend).loadOrCreate(replacement),
            )
            assertArrayEquals(tampered, file.readBytes())
        } finally {
            directory.deleteRecursively()
            deleteTestKey(keyAlias)
        }
    }

    @Test
    fun transportIdentityTreatsMissingKeystoreKeyAsCorruptWithoutReplacement() {
        val context = targetContext()
        val directory = testDirectory(context, "identity-missing-key")
        val keyAlias = testKeyAlias("identity-missing-key")
        val candidate = ByteArray(REMORA_LINK_TRANSPORT_IDENTITY_BYTES) { (it + 1).toByte() }
        try {
            val backend = AtomicEncryptedRemoraLinkTransportIdentityBackend(
                context = context,
                directory = directory,
                keyAlias = keyAlias,
            )
            assertTrue(
                RemoraLinkTransportIdentityStore(backend).loadOrCreate(candidate) is
                    RemoraLinkTransportIdentityStatus.Ready,
            )
            val file = File(
                directory,
                AtomicEncryptedRemoraLinkTransportIdentityBackend.IDENTITY_FILE_NAME,
            )
            val durableCiphertext = file.readBytes()
            deleteTestKey(keyAlias)

            assertEquals(
                RemoraLinkTransportIdentityStatus.Corrupt,
                RemoraLinkTransportIdentityStore(backend).loadOrCreate(ByteArray(32) { 0x55 }),
            )
            assertArrayEquals(durableCiphertext, file.readBytes())
        } finally {
            directory.deleteRecursively()
            deleteTestKey(keyAlias)
        }
    }

    @Test
    fun transportIdentityPostRenameSyncFailureFencesSamePath() {
        val context = targetContext()
        val directory = testDirectory(context, "identity-indeterminate")
        val keyAlias = testKeyAlias("identity-indeterminate")
        val candidate = ByteArray(REMORA_LINK_TRANSPORT_IDENTITY_BYTES) { 0x31 }
        try {
            val indeterminate = RemoraLinkTransportIdentityStore(
                AtomicEncryptedRemoraLinkTransportIdentityBackend(
                    context = context,
                    directory = directory,
                    keyAlias = keyAlias,
                    directorySyncOverride = { throw IOException("injected directory fsync failure") },
                ),
            )
            assertEquals(
                RemoraLinkTransportIdentityStatus.StorageFailure,
                indeterminate.loadOrCreate(candidate),
            )
            val raw = AtomicEncryptedRemoraLinkTransportIdentityBackend(
                context = context,
                directory = directory,
                keyAlias = keyAlias,
            ).read()
            assertTrue(raw is RemoraLinkTransportIdentityBackendRead.Present)
            (raw as RemoraLinkTransportIdentityBackendRead.Present).identityBytes.fill(0)
            assertEquals(
                RemoraLinkTransportIdentityStatus.StorageFailure,
                RemoraLinkTransportIdentityStore(
                    AtomicEncryptedRemoraLinkTransportIdentityBackend(
                        context = context,
                        directory = directory,
                        keyAlias = keyAlias,
                    ),
                ).loadOrCreate(ByteArray(32) { 0x72 }),
            )
        } finally {
            directory.deleteRecursively()
            deleteTestKey(keyAlias)
        }
    }

    @Test
    fun transportIdentityRealFileCreationHasOneWinnerAcrossStoreInstances() {
        val context = targetContext()
        val directory = testDirectory(context, "identity-race")
        val keyAlias = testKeyAlias("identity-race")
        try {
            val stores = List(12) {
                RemoraLinkTransportIdentityStore(
                    AtomicEncryptedRemoraLinkTransportIdentityBackend(
                        context = context,
                        directory = directory,
                        keyAlias = keyAlias,
                    ),
                )
            }
            val outcomes = race(stores.size) { index ->
                stores[index].loadOrCreate(
                    ByteArray(REMORA_LINK_TRANSPORT_IDENTITY_BYTES) { (index + 1).toByte() },
                ) as RemoraLinkTransportIdentityStatus.Ready
            }

            assertEquals(1, outcomes.count(RemoraLinkTransportIdentityStatus.Ready::created))
            val winner = outcomes.first(RemoraLinkTransportIdentityStatus.Ready::created)
                .identityBytes
            outcomes.forEach { assertArrayEquals(winner, it.identityBytes) }
        } finally {
            directory.deleteRecursively()
            deleteTestKey(keyAlias)
        }
    }

    private fun targetContext(): Context =
        InstrumentationRegistry.getInstrumentation().targetContext.applicationContext

    private fun testDirectory(context: Context, purpose: String): File =
        File(context.noBackupFilesDir, "remora-link-$purpose-${System.nanoTime()}")

    private fun testKeyAlias(purpose: String): String =
        "com.remora.android.tests.remora_link.v2.$purpose.${System.nanoTime()}"

    private fun deleteTestKey(alias: String) {
        runCatching {
            KeyStore.getInstance("AndroidKeyStore").apply {
                load(null)
                if (containsAlias(alias)) deleteEntry(alias)
            }
        }
    }
}

private fun ByteArray.containsSubsequence(candidate: ByteArray): Boolean {
    if (candidate.isEmpty()) return true
    if (candidate.size > size) return false
    return (0..size - candidate.size).any { offset ->
        candidate.indices.all { index -> this[offset + index] == candidate[index] }
    }
}

private fun <T> race(count: Int, operation: (Int) -> T): List<T> {
    val executor = Executors.newFixedThreadPool(count)
    val barrier = CyclicBarrier(count)
    return try {
        val futures = (0 until count).map { index ->
            executor.submit<T> {
                barrier.await(10, TimeUnit.SECONDS)
                operation(index)
            }
        }
        futures.map { it.get(30, TimeUnit.SECONDS) }
    } finally {
        executor.shutdownNow()
        executor.awaitTermination(5, TimeUnit.SECONDS)
    }
}
