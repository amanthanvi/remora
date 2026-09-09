package com.remora.android.state

import java.util.UUID
import java.util.concurrent.CyclicBarrier
import java.util.concurrent.Executors
import java.util.concurrent.TimeUnit
import javax.crypto.Cipher
import javax.crypto.KeyGenerator
import javax.crypto.spec.GCMParameterSpec
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.runBlocking
import org.junit.Assert.*
import org.junit.Test
import uniffi.codex_mobile_client.*

class RelaySecretStoreTest {
    @Test
    fun revisionedValuesAndTombstonesSurviveReconstructionWithoutAliasing() {
        val fixture = CustodyFixture()
        val store = fixture.store()
        val value = ByteArray(32) { 7 }
        assertEquals(AppRelaySecretCreateOutcome.CREATED, store.createIfAbsent("alias", value))
        value.fill(0)
        assertEquals(AppRelaySecretCreateOutcome.ALREADY_EXISTS, store.createIfAbsent("alias", byteArrayOf(9)))
        assertEquals(AppRelaySecretRevision.Found(1uL), store.revision("alias"))
        val returned = store.read("alias")
        assertTrue(returned.all { it == 7.toByte() })
        returned.fill(0)
        assertEquals(AppRelaySecretCasOutcome.STORED, store.compareAndSwap("alias", 1uL, 7uL, byteArrayOf(8)))
        assertEquals(AppRelaySecretCasOutcome.STORED, store.compareAndTombstone("alias", 7uL, 9uL))
        val restarted = fixture.store()
        assertThrows(AppRelaySecretReadException.Missing::class.java) { restarted.read("alias") }
        assertEquals(AppRelaySecretRevision.Found(9uL), restarted.revision("alias"))
        assertEquals(AppRelaySecretCasOutcome.CONFLICT, restarted.compareAndSwap("alias", null, 1uL, byteArrayOf(9)))
        assertEquals(AppRelaySecretCreateOutcome.ALREADY_EXISTS, restarted.createIfAbsent("alias", byteArrayOf(9)))
        assertEquals(AppRelaySecretCasOutcome.STORED, restarted.compareAndSwap("alias", 9uL, 10uL, byteArrayOf(10)))
        assertArrayEquals(byteArrayOf(10), restarted.read("alias"))
    }

    @Test
    fun writeAndDeleteAreIdempotentAndInvalidCasCannotOverwrite() {
        val fixture = CustodyFixture()
        val store = fixture.store()
        repeat(2) { assertEquals(AppRelaySecretWriteOutcome.APPLIED, store.write("alias", byteArrayOf(3))) }
        assertEquals(AppRelaySecretRevision.Found(1uL), store.revision("alias"))
        assertEquals(AppRelaySecretCasOutcome.UNAVAILABLE, store.compareAndSwap("alias", 1uL, 1uL, byteArrayOf(4)))
        assertEquals(AppRelaySecretCasOutcome.CONFLICT, store.compareAndSwap("alias", 2uL, 3uL, byteArrayOf(4)))
        assertArrayEquals(byteArrayOf(3), store.read("alias"))
        repeat(2) { assertEquals(AppRelaySecretWriteOutcome.APPLIED, store.delete("alias")) }
        assertSame(AppRelaySecretRevision.Missing, fixture.store().revision("alias"))
    }

    @Test
    fun concurrentStoreInstancesHaveOneCasWinner() {
        val fixture = CustodyFixture()
        val count = 12
        val barrier = CyclicBarrier(count)
        val executor = Executors.newFixedThreadPool(count)
        try {
            val outcomes = (0 until count).map { index -> executor.submit<AppRelaySecretCasOutcome> {
                barrier.await(10, TimeUnit.SECONDS)
                fixture.store().compareAndSwap("alias", null, 1uL, byteArrayOf(index.toByte()))
            } }.map { it.get(15, TimeUnit.SECONDS) }
            assertEquals(1, outcomes.count { it == AppRelaySecretCasOutcome.STORED })
            assertEquals(count - 1, outcomes.count { it == AppRelaySecretCasOutcome.CONFLICT })
        } finally {
            executor.shutdownNow()
        }
    }

    @Test
    fun failedOrIndeterminateCommitNeverReportsSuccessAndCorruptionCannotBeReplaced() {
        val fixture = CustodyFixture()
        val store = fixture.store()
        store.write("alias", byteArrayOf(1))
        fixture.backend.failure = RemoraLinkJournalBackendWrite.FAILED
        assertEquals(AppRelaySecretWriteOutcome.UNAVAILABLE, store.write("alias", byteArrayOf(2)))
        assertArrayEquals(byteArrayOf(1), store.read("alias"))
        fixture.backend.failure = RemoraLinkJournalBackendWrite.INDETERMINATE
        assertEquals(AppRelaySecretWriteOutcome.UNAVAILABLE, store.write("alias", byteArrayOf(3)))
        assertSame(AppRelaySecretRevision.Unavailable, fixture.store().revision("alias"))

        val corrupt = CustodyFixture()
        corrupt.store().write("alias", byteArrayOf(1))
        corrupt.backend.envelope!![0] = 0
        val before = corrupt.backend.envelope!!.copyOf()
        assertEquals(AppRelaySecretCreateOutcome.UNAVAILABLE, corrupt.store().createIfAbsent("other", byteArrayOf(2)))
        assertArrayEquals(before, corrupt.backend.envelope)
    }

    @Test
    fun anchorRejectsFileRollbackMissingDataAndEqualRevisionCiphertextMismatch() {
        val fixture = CustodyFixture(anchor = true)
        val store = fixture.store()
        assertEquals(AppRelaySecretCreateOutcome.CREATED, store.createIfAbsent(ANCHOR, byteArrayOf(1)))
        val old = fixture.backend.envelope!!.copyOf()
        assertEquals(AppRelaySecretCasOutcome.STORED, store.compareAndSwap(ANCHOR, 1uL, 2uL, byteArrayOf(2)))
        val latest = fixture.backend.envelope!!.copyOf()
        fixture.backend.envelope = old
        assertThrows(AppRelaySecretReadException.Unavailable::class.java) { fixture.store().read(ANCHOR) }
        val olderSnapshot = checkNotNull(decodeRemoraLinkJournalEnvelope(old))
        fixture.backend.envelope = encodeRemoraLinkJournalEnvelope(
            RemoraLinkJournalSnapshot(99uL, olderSnapshot.opaquePayload),
        )
        assertSame(AppRelaySecretRevision.Unavailable, fixture.store().revision(ANCHOR))
        assertEquals(2uL, fixture.markers.mark!!.revision)
        fixture.backend.envelope = null
        assertSame(AppRelaySecretRevision.Unavailable, fixture.store().revision(ANCHOR))
        fixture.backend.envelope = latest
        val snapshot = checkNotNull(decodeRemoraLinkJournalEnvelope(latest))
        val plaintext = fixture.decrypt(snapshot.opaquePayload)
        try {
            fixture.backend.envelope = encodeRemoraLinkJournalEnvelope(
                RemoraLinkJournalSnapshot(snapshot.revision, fixture.encrypt(plaintext)),
            )
        } finally { plaintext.fill(0) }
        assertSame(AppRelaySecretRevision.Unavailable, fixture.store().revision(ANCHOR))
    }

    @Test
    fun anchorHealsAuthenticatedFileAheadAndAmbiguousMarkerCommit() {
        for (failAfterCommit in listOf(false, true)) {
            val fixture = CustodyFixture(anchor = true)
            val store = fixture.store()
            store.createIfAbsent(ANCHOR, byteArrayOf(1))
            fixture.markers.failure = if (failAfterCommit) 2 else 1
            assertEquals(AppRelaySecretCasOutcome.UNAVAILABLE, store.compareAndSwap(ANCHOR, 1uL, 3uL, byteArrayOf(3)))
            fixture.markers.failure = 0
            assertArrayEquals(byteArrayOf(3), fixture.store().read(ANCHOR))
            assertEquals(AppRelaySecretRevision.Found(3uL), fixture.store().revision(ANCHOR))
            assertEquals(2uL, fixture.markers.mark!!.revision)
            assertEquals(AppRelaySecretCasOutcome.CONFLICT, fixture.store().compareAndSwap(ANCHOR, 1uL, 2uL, byteArrayOf(2)))
        }
    }

    @Test
    fun anchorCannotBeResetThroughUnfencedApisOrWrongNamespace() {
        val fixture = CustodyFixture(anchor = true)
        val store = fixture.store()
        store.createIfAbsent(ANCHOR, byteArrayOf(1))
        assertEquals(AppRelaySecretWriteOutcome.UNAVAILABLE, store.write(ANCHOR, byteArrayOf(2)))
        assertEquals(AppRelaySecretWriteOutcome.UNAVAILABLE, store.delete(ANCHOR))
        assertEquals(AppRelaySecretCasOutcome.UNAVAILABLE, store.compareAndTombstone(ANCHOR, 1uL, 2uL))
        assertEquals(AppRelaySecretCreateOutcome.UNAVAILABLE, store.createIfAbsent("other", byteArrayOf(2)))
        assertEquals(AppRelaySecretCreateOutcome.UNAVAILABLE, CustodyFixture().store().createIfAbsent(ANCHOR, byteArrayOf(2)))
        assertArrayEquals(byteArrayOf(1), fixture.store().read(ANCHOR))
    }

    @Test
    fun nativeJournalAndSecretCallbacksPreserveOpaquePayloadAndTypedFailures(): Unit = runBlocking {
        val backend = CustodyMemoryBackend()
        val journal = AndroidRelayJournalBackend(RemoraLinkJournalStore(backend), Dispatchers.Unconfined)
        assertSame(AppRelayJournalLoad.Missing, journal.load())
        assertEquals(AppRelayJournalWriteOutcome.STORED, journal.compareAndSwap(null,
            AppRelayJournalSnapshot(1uL, byteArrayOf(0, -1, 9))))
        assertArrayEquals(byteArrayOf(0, -1, 9), (journal.load() as AppRelayJournalLoad.Loaded).snapshot.payload)
        val normal = CustodyFixture()
        val anchor = CustodyFixture(anchor = true)
        val adapter = AndroidRelaySecretBackend(normal.store(), anchor.store(), Dispatchers.Unconfined)
        assertEquals(AppRelaySecretCreateOutcome.CREATED, adapter.createIfAbsent("alias", byteArrayOf(4)))
        assertArrayEquals(byteArrayOf(4), adapter.read("alias"))
        assertEquals(AppRelaySecretCreateOutcome.CREATED, adapter.createIfAbsent(ANCHOR, byteArrayOf(5)))
        assertArrayEquals(byteArrayOf(5), adapter.read(ANCHOR))
        assertEquals(AppRelaySecretCasOutcome.STORED, adapter.compareAndTombstone("alias", 1uL, 2uL))
        assertThrows(AppRelaySecretReadException.Missing::class.java) { runBlocking { adapter.read("alias") } }
    }

    private companion object { const val ANCHOR = RELAY_ROLLBACK_ANCHOR_ALIAS }
}

internal class CustodyFixture(private val anchor: Boolean = false) {
    val backend = CustodyMemoryBackend()
    val markers = CustodyMemoryMarkers()
    private val key = KeyGenerator.getInstance("AES").apply { init(256) }.generateKey()
    fun encrypt(value: ByteArray): ByteArray = Cipher.getInstance("AES/GCM/NoPadding").run {
        init(Cipher.ENCRYPT_MODE, key)
        iv + doFinal(value)
    }
    fun decrypt(value: ByteArray): ByteArray = Cipher.getInstance("AES/GCM/NoPadding").run {
        init(Cipher.DECRYPT_MODE, key, GCMParameterSpec(128, value.copyOfRange(0, 12)))
        doFinal(value, 12, value.size - 12)
    }
    fun store(): RelaySecretStore = RelaySecretStore(RemoraLinkJournalStore(backend), ::encrypt, ::decrypt,
        reservedAnchor = anchor, verifyFence = { if (anchor) RelayRollbackFence(markers).verify(it) })
}

internal class CustodyMemoryBackend : RemoraLinkJournalBackend {
    override val storageIdentity = "relay-test-${UUID.randomUUID()}"
    var envelope: ByteArray? = null
    var failure: RemoraLinkJournalBackendWrite? = null
    override fun read(): RemoraLinkJournalBackendRead = envelope?.copyOf()?.let(RemoraLinkJournalBackendRead::Present)
        ?: RemoraLinkJournalBackendRead.Missing
    override fun replace(envelope: ByteArray): RemoraLinkJournalBackendWrite {
        if (failure == RemoraLinkJournalBackendWrite.FAILED) return checkNotNull(failure)
        this.envelope = envelope.copyOf()
        return failure ?: RemoraLinkJournalBackendWrite.COMMITTED
    }
}

internal class CustodyMemoryMarkers : RelayAnchorMarkers {
    var mark: RelayAnchorMark? = null
    var failure = 0
    override fun latest(): RelayAnchorMark? = mark
    override fun commit(mark: RelayAnchorMark) {
        check(failure != 1)
        this.mark = mark
        check(failure != 2)
    }
}
