package com.remora.android.state

import java.util.UUID
import java.util.concurrent.CyclicBarrier
import java.util.concurrent.Executors
import java.util.concurrent.TimeUnit
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class RemoraLinkNativeStoresTest {
    @Test
    fun journalRoundTripsOpaqueBinaryWithoutParsingRustPayload() {
        val backend = FakeJournalBackend()
        val store = RemoraLinkJournalStore(backend)
        val payload = byteArrayOf(0, -1, 0x7f, '{'.code.toByte(), 0, '}'.code.toByte())

        assertEquals(
            RemoraLinkJournalCasStatus.STORED,
            store.compareAndSwap(null, RemoraLinkJournalSnapshot(1uL, payload)),
        )
        val loaded = store.load() as RemoraLinkJournalLoadStatus.Loaded
        assertEquals(1uL, loaded.snapshot.revision)
        assertArrayEquals(payload, loaded.snapshot.opaquePayload)

        val exposed = loaded.snapshot.opaquePayload
        exposed.fill(42)
        assertArrayEquals(payload, loaded.snapshot.opaquePayload)
    }

    @Test
    fun journalRequiresExactNextNonzeroRevision() {
        val backend = FakeJournalBackend()
        val store = RemoraLinkJournalStore(backend)

        assertEquals(
            RemoraLinkJournalCasStatus.INVALID_REPLACEMENT,
            store.compareAndSwap(null, RemoraLinkJournalSnapshot(0uL, byteArrayOf(1))),
        )
        assertEquals(
            RemoraLinkJournalCasStatus.INVALID_REPLACEMENT,
            store.compareAndSwap(null, RemoraLinkJournalSnapshot(2uL, byteArrayOf(1))),
        )
        assertEquals(
            RemoraLinkJournalCasStatus.STORED,
            store.compareAndSwap(null, RemoraLinkJournalSnapshot(1uL, byteArrayOf(1))),
        )
        assertEquals(
            RemoraLinkJournalCasStatus.INVALID_REPLACEMENT,
            store.compareAndSwap(1uL, RemoraLinkJournalSnapshot(3uL, byteArrayOf(2))),
        )
        assertEquals(
            RemoraLinkJournalCasStatus.CONFLICT,
            store.compareAndSwap(null, RemoraLinkJournalSnapshot(1uL, byteArrayOf(3))),
        )

        backend.envelope = encodeRemoraLinkJournalEnvelope(
            RemoraLinkJournalSnapshot(ULong.MAX_VALUE, byteArrayOf(9)),
        )
        assertEquals(
            RemoraLinkJournalCasStatus.INVALID_REPLACEMENT,
            store.compareAndSwap(
                ULong.MAX_VALUE,
                RemoraLinkJournalSnapshot(0uL, byteArrayOf(10)),
            ),
        )
    }

    @Test
    fun journalCasHasExactlyOneWinnerAcrossStoreInstances() {
        val backend = FakeJournalBackend()
        val stores = List(24) { RemoraLinkJournalStore(backend) }
        val first = race(stores.size) { index ->
            stores[index].compareAndSwap(
                null,
                RemoraLinkJournalSnapshot(1uL, byteArrayOf(index.toByte())),
            )
        }
        assertEquals(1, first.count { it == RemoraLinkJournalCasStatus.STORED })
        assertEquals(23, first.count { it == RemoraLinkJournalCasStatus.CONFLICT })

        val firstWinner = (stores.first().load() as RemoraLinkJournalLoadStatus.Loaded).snapshot
        val second = race(stores.size) { index ->
            stores[index].compareAndSwap(
                firstWinner.revision,
                RemoraLinkJournalSnapshot(2uL, byteArrayOf((index + 30).toByte())),
            )
        }
        assertEquals(1, second.count { it == RemoraLinkJournalCasStatus.STORED })
        assertEquals(23, second.count { it == RemoraLinkJournalCasStatus.CONFLICT })
        assertEquals(2, backend.successfulWrites)
    }

    @Test
    fun journalRejectsTruncationTamperingAndOversizeBeforeOverwrite() {
        val backend = FakeJournalBackend()
        val store = RemoraLinkJournalStore(backend)
        val valid = encodeRemoraLinkJournalEnvelope(
            RemoraLinkJournalSnapshot(1uL, "opaque-rust-payload".toByteArray()),
        )

        backend.envelope = valid.copyOf(valid.size - 1)
        assertEquals(RemoraLinkJournalLoadStatus.Corrupt, store.load())
        assertEquals(
            RemoraLinkJournalCasStatus.CORRUPT,
            store.compareAndSwap(1uL, RemoraLinkJournalSnapshot(2uL, byteArrayOf(2))),
        )

        backend.envelope = valid.copyOf().apply { this[size / 2] = this[size / 2].inc() }
        assertEquals(RemoraLinkJournalLoadStatus.Corrupt, store.load())

        assertEquals(
            RemoraLinkJournalCasStatus.INVALID_REPLACEMENT,
            RemoraLinkJournalStore(FakeJournalBackend()).compareAndSwap(
                null,
                RemoraLinkJournalSnapshot(
                    1uL,
                    ByteArray(MAX_REMORA_LINK_JOURNAL_PAYLOAD_BYTES + 1),
                ),
            ),
        )
        valid.fill(0)
    }

    @Test
    fun journalPersistenceFailuresNeverReportStoredOrLosePriorSnapshot() {
        val backend = FakeJournalBackend()
        val store = RemoraLinkJournalStore(backend)
        assertEquals(
            RemoraLinkJournalCasStatus.STORED,
            store.compareAndSwap(null, RemoraLinkJournalSnapshot(1uL, byteArrayOf(1))),
        )
        backend.failWrites = true

        assertEquals(
            RemoraLinkJournalCasStatus.STORAGE_FAILURE,
            store.compareAndSwap(1uL, RemoraLinkJournalSnapshot(2uL, byteArrayOf(2))),
        )
        val reconstructed = RemoraLinkJournalStore(backend)
        val prior = reconstructed.load() as RemoraLinkJournalLoadStatus.Loaded
        assertEquals(1uL, prior.snapshot.revision)
        assertArrayEquals(byteArrayOf(1), prior.snapshot.opaquePayload)

        backend.failWrites = false
        assertEquals(
            RemoraLinkJournalCasStatus.STORED,
            reconstructed.compareAndSwap(1uL, RemoraLinkJournalSnapshot(2uL, byteArrayOf(2))),
        )
        val recovered = reconstructed.load() as RemoraLinkJournalLoadStatus.Loaded
        assertEquals(2uL, recovered.snapshot.revision)
        assertArrayEquals(byteArrayOf(2), recovered.snapshot.opaquePayload)

        backend.failReads = true
        assertEquals(RemoraLinkJournalLoadStatus.StorageFailure, reconstructed.load())
        assertEquals(
            RemoraLinkJournalCasStatus.STORAGE_FAILURE,
            reconstructed.compareAndSwap(2uL, RemoraLinkJournalSnapshot(3uL, byteArrayOf(3))),
        )
    }

    @Test
    fun journalBackendExceptionsAreMappedToStorageFailure() {
        val readFailure = FakeJournalBackend().apply { throwReads = true }
        val readStore = RemoraLinkJournalStore(readFailure)
        assertEquals(RemoraLinkJournalLoadStatus.StorageFailure, readStore.load())
        assertEquals(
            RemoraLinkJournalCasStatus.STORAGE_FAILURE,
            readStore.compareAndSwap(null, RemoraLinkJournalSnapshot(1uL, byteArrayOf(1))),
        )

        val writeFailure = FakeJournalBackend().apply { throwWrites = true }
        assertEquals(
            RemoraLinkJournalCasStatus.STORAGE_FAILURE,
            RemoraLinkJournalStore(writeFailure).compareAndSwap(
                null,
                RemoraLinkJournalSnapshot(1uL, byteArrayOf(1)),
            ),
        )
        assertEquals(null, writeFailure.envelope)
    }

    @Test
    fun indeterminateJournalCommitFailsClosedAcrossStoreReconstruction() {
        val backend = FakeJournalBackend()
        val store = RemoraLinkJournalStore(backend)
        assertEquals(
            RemoraLinkJournalCasStatus.STORED,
            store.compareAndSwap(null, RemoraLinkJournalSnapshot(1uL, byteArrayOf(1))),
        )
        backend.indeterminateWrites = true

        assertEquals(
            RemoraLinkJournalCasStatus.STORAGE_FAILURE,
            store.compareAndSwap(1uL, RemoraLinkJournalSnapshot(2uL, byteArrayOf(2))),
        )
        assertEquals(RemoraLinkJournalLoadStatus.StorageFailure, store.load())
        assertEquals(
            RemoraLinkJournalLoadStatus.StorageFailure,
            RemoraLinkJournalStore(backend).load(),
        )
    }

    @Test
    fun journalDebugOutputRedactsOpaquePayloadAndEnvelope() {
        val secretLookingPayload = "provider-token-do-not-log".toByteArray()
        val loaded = RemoraLinkJournalLoadStatus.Loaded(
            RemoraLinkJournalSnapshot(17uL, secretLookingPayload),
        )
        val envelope = encodeRemoraLinkJournalEnvelope(loaded.snapshot)
        val backendRead = RemoraLinkJournalBackendRead.Present(envelope)

        assertFalse(loaded.toString().contains("provider-token-do-not-log"))
        assertTrue(loaded.toString().contains("<redacted:${secretLookingPayload.size} bytes>"))
        assertFalse(backendRead.toString().contains(envelope.contentToString()))
    }

    @Test
    fun transportIdentityRoundTripsAndNeverReplacesExistingIdentity() {
        val backend = FakeTransportIdentityBackend()
        val store = RemoraLinkTransportIdentityStore(backend)
        val original = ByteArray(REMORA_LINK_TRANSPORT_IDENTITY_BYTES) { (it + 1).toByte() }
        val replacement = ByteArray(REMORA_LINK_TRANSPORT_IDENTITY_BYTES) { (it + 80).toByte() }

        val created = store.loadOrCreate(original) as RemoraLinkTransportIdentityStatus.Ready
        assertTrue(created.created)
        assertArrayEquals(original, created.identityBytes)

        val loaded = RemoraLinkTransportIdentityStore(backend)
            .loadOrCreate(replacement) as RemoraLinkTransportIdentityStatus.Ready
        assertFalse(loaded.created)
        assertArrayEquals(original, loaded.identityBytes)
        assertEquals(1, backend.successfulWrites)

        val exposed = loaded.identityBytes
        exposed.fill(0)
        assertArrayEquals(original, loaded.identityBytes)
    }

    @Test
    fun transportIdentityUsesOneOwnedCandidateForPersistenceAndReturn() {
        val backend = FakeTransportIdentityBackend()
        val callerCandidate = ByteArray(REMORA_LINK_TRANSPORT_IDENTITY_BYTES) { (it + 1).toByte() }
        val expected = callerCandidate.copyOf()
        backend.beforeCommit = { callerCandidate.fill(0x7f) }

        val ready = RemoraLinkTransportIdentityStore(backend)
            .loadOrCreate(callerCandidate) as RemoraLinkTransportIdentityStatus.Ready

        assertArrayEquals(expected, ready.identityBytes)
        assertArrayEquals(expected, backend.identityBytes)
        assertFalse(callerCandidate.contentEquals(expected))
    }

    @Test
    fun transportIdentityCreationHasOneWinnerAcrossStoreInstances() {
        val backend = FakeTransportIdentityBackend()
        val stores = List(32) { RemoraLinkTransportIdentityStore(backend) }
        val results = race(stores.size) { index ->
            stores[index].loadOrCreate(
                ByteArray(REMORA_LINK_TRANSPORT_IDENTITY_BYTES) { (index + 1).toByte() },
            ) as RemoraLinkTransportIdentityStatus.Ready
        }

        assertEquals(1, results.count(RemoraLinkTransportIdentityStatus.Ready::created))
        assertEquals(1, backend.successfulWrites)
        val winner = results.first(RemoraLinkTransportIdentityStatus.Ready::created).identityBytes
        results.forEach { assertArrayEquals(winner, it.identityBytes) }
    }

    @Test
    fun transportIdentityRejectsWrongLengthsAndCorruptStorage() {
        val backend = FakeTransportIdentityBackend()
        val store = RemoraLinkTransportIdentityStore(backend)

        assertEquals(
            RemoraLinkTransportIdentityStatus.InvalidCandidate,
            store.loadOrCreate(ByteArray(31)),
        )
        assertEquals(
            RemoraLinkTransportIdentityStatus.InvalidCandidate,
            store.loadOrCreate(ByteArray(33)),
        )

        backend.identityBytes = ByteArray(31)
        assertEquals(
            RemoraLinkTransportIdentityStatus.Corrupt,
            store.loadOrCreate(ByteArray(32)),
        )
        backend.identityBytes = ByteArray(33)
        assertEquals(
            RemoraLinkTransportIdentityStatus.Corrupt,
            store.loadOrCreate(ByteArray(32)),
        )
        backend.reportCorrupt = true
        assertEquals(
            RemoraLinkTransportIdentityStatus.Corrupt,
            store.loadOrCreate(ByteArray(32)),
        )
    }

    @Test
    fun transportIdentityPersistenceFailuresNeverReturnCandidateAsDurable() {
        val backend = FakeTransportIdentityBackend(failWrites = true)
        val candidate = ByteArray(REMORA_LINK_TRANSPORT_IDENTITY_BYTES) { 0x5a }

        assertEquals(
            RemoraLinkTransportIdentityStatus.StorageFailure,
            RemoraLinkTransportIdentityStore(backend).loadOrCreate(candidate),
        )
        assertEquals(null, backend.identityBytes)

        backend.failWrites = false
        backend.failReads = true
        assertEquals(
            RemoraLinkTransportIdentityStatus.StorageFailure,
            RemoraLinkTransportIdentityStore(backend).loadOrCreate(candidate),
        )
        assertEquals(0, backend.successfulWrites)
    }

    @Test
    fun transportIdentityBackendExceptionsAreMappedToStorageFailure() {
        val candidate = ByteArray(REMORA_LINK_TRANSPORT_IDENTITY_BYTES) { 0x5a }
        val readFailure = FakeTransportIdentityBackend().apply { throwReads = true }
        assertEquals(
            RemoraLinkTransportIdentityStatus.StorageFailure,
            RemoraLinkTransportIdentityStore(readFailure).loadOrCreate(candidate),
        )

        val writeFailure = FakeTransportIdentityBackend().apply { throwWrites = true }
        assertEquals(
            RemoraLinkTransportIdentityStatus.StorageFailure,
            RemoraLinkTransportIdentityStore(writeFailure).loadOrCreate(candidate),
        )
        assertEquals(null, writeFailure.identityBytes)
    }

    @Test
    fun transportIdentityAdoptsWinnerWhenCreateReportsAlreadyExists() {
        val winner = ByteArray(REMORA_LINK_TRANSPORT_IDENTITY_BYTES) { 0x2a }
        val backend = FakeTransportIdentityBackend().apply {
            publishWinnerInsteadOfCreating = winner
        }

        val ready = RemoraLinkTransportIdentityStore(backend).loadOrCreate(
            ByteArray(REMORA_LINK_TRANSPORT_IDENTITY_BYTES) { 0x7b },
        ) as RemoraLinkTransportIdentityStatus.Ready

        assertFalse(ready.created)
        assertArrayEquals(winner, ready.identityBytes)
        assertEquals(0, backend.successfulWrites)
    }

    @Test
    fun indeterminateIdentityCreationFailsClosedAcrossStoreReconstruction() {
        val backend = FakeTransportIdentityBackend(indeterminateWrites = true)
        val candidate = ByteArray(REMORA_LINK_TRANSPORT_IDENTITY_BYTES) { 0x33 }

        assertEquals(
            RemoraLinkTransportIdentityStatus.StorageFailure,
            RemoraLinkTransportIdentityStore(backend).loadOrCreate(candidate),
        )
        assertArrayEquals(candidate, backend.identityBytes)
        assertEquals(
            RemoraLinkTransportIdentityStatus.StorageFailure,
            RemoraLinkTransportIdentityStore(backend).loadOrCreate(ByteArray(32)),
        )
    }

    @Test
    fun transportIdentityDebugOutputRedactsKeyMaterial() {
        val bytes = ByteArray(REMORA_LINK_TRANSPORT_IDENTITY_BYTES) { (it + 1).toByte() }
        val status = RemoraLinkTransportIdentityStatus.Ready(bytes, created = true)
        val backendRead = RemoraLinkTransportIdentityBackendRead.Present(bytes.copyOf())

        assertFalse(status.toString().contains(bytes.contentToString()))
        assertFalse(status.toString().contains("0102030405"))
        assertTrue(status.toString().contains("<redacted:32 bytes>"))
        assertFalse(backendRead.toString().contains(bytes.contentToString()))
    }
}

private class FakeJournalBackend : RemoraLinkJournalBackend {
    override val storageIdentity: String = "fake-journal-${UUID.randomUUID()}"
    var envelope: ByteArray? = null
    var failReads = false
    var failWrites = false
    var throwReads = false
    var throwWrites = false
    var indeterminateWrites = false
    var reportCorrupt = false
    var successfulWrites = 0

    override fun read(): RemoraLinkJournalBackendRead = when {
        throwReads -> error("injected journal read exception")
        failReads -> RemoraLinkJournalBackendRead.Failed
        reportCorrupt -> RemoraLinkJournalBackendRead.Corrupt
        envelope == null -> RemoraLinkJournalBackendRead.Missing
        else -> RemoraLinkJournalBackendRead.Present(checkNotNull(envelope).copyOf())
    }

    override fun replace(envelope: ByteArray): RemoraLinkJournalBackendWrite {
        if (throwWrites) error("injected journal write exception")
        if (failWrites) return RemoraLinkJournalBackendWrite.FAILED
        this.envelope = envelope.copyOf()
        if (indeterminateWrites) return RemoraLinkJournalBackendWrite.INDETERMINATE
        successfulWrites += 1
        return RemoraLinkJournalBackendWrite.COMMITTED
    }
}

private class FakeTransportIdentityBackend(
    var failWrites: Boolean = false,
    var indeterminateWrites: Boolean = false,
) : RemoraLinkTransportIdentityBackend {
    override val storageIdentity: String = "fake-identity-${UUID.randomUUID()}"
    var identityBytes: ByteArray? = null
    var failReads = false
    var throwReads = false
    var throwWrites = false
    var reportCorrupt = false
    var successfulWrites = 0
    var beforeCommit: (() -> Unit)? = null
    var publishWinnerInsteadOfCreating: ByteArray? = null

    override fun read(): RemoraLinkTransportIdentityBackendRead = when {
        throwReads -> error("injected identity read exception")
        failReads -> RemoraLinkTransportIdentityBackendRead.Failed
        reportCorrupt -> RemoraLinkTransportIdentityBackendRead.Corrupt
        identityBytes == null -> RemoraLinkTransportIdentityBackendRead.Missing
        else -> RemoraLinkTransportIdentityBackendRead.Present(checkNotNull(identityBytes).copyOf())
    }

    override fun create(identityBytes: ByteArray): RemoraLinkTransportIdentityBackendWrite {
        if (throwWrites) error("injected identity write exception")
        if (failWrites) return RemoraLinkTransportIdentityBackendWrite.FAILED
        publishWinnerInsteadOfCreating?.let { winner ->
            this.identityBytes = winner.copyOf()
            return RemoraLinkTransportIdentityBackendWrite.ALREADY_EXISTS
        }
        beforeCommit?.invoke()
        this.identityBytes = identityBytes.copyOf()
        if (indeterminateWrites) return RemoraLinkTransportIdentityBackendWrite.INDETERMINATE
        successfulWrites += 1
        return RemoraLinkTransportIdentityBackendWrite.COMMITTED
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
        futures.map { it.get(20, TimeUnit.SECONDS) }
    } finally {
        executor.shutdownNow()
        executor.awaitTermination(5, TimeUnit.SECONDS)
    }
}
