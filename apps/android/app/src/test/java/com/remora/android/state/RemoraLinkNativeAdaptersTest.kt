package com.remora.android.state

import java.security.KeyPairGenerator
import java.security.Signature
import java.security.spec.ECGenParameterSpec
import java.util.concurrent.CountDownLatch
import java.util.concurrent.Executors
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicReference
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.asCoroutineDispatcher
import kotlinx.coroutines.awaitCancellation
import kotlinx.coroutines.cancel
import kotlinx.coroutines.cancelAndJoin
import kotlinx.coroutines.launch
import kotlinx.coroutines.runBlocking
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotEquals
import org.junit.Assert.assertSame
import org.junit.Assert.assertThrows
import org.junit.Assert.assertTrue
import org.junit.Test
import uniffi.codex_mobile_client.AppRemoraLinkDeviceKeyException
import uniffi.codex_mobile_client.AppRemoraLinkHardwareKeyLoad
import uniffi.codex_mobile_client.AppRemoraLinkJournalLoad
import uniffi.codex_mobile_client.AppRemoraLinkJournalSnapshot
import uniffi.codex_mobile_client.AppRemoraLinkJournalWriteOutcome
import uniffi.codex_mobile_client.AppRemoraLinkKeyAssurance
import uniffi.codex_mobile_client.AppRemoraLinkKeyDeletionStatus
import uniffi.codex_mobile_client.AppRemoraLinkTransportIdentityException

class RemoraLinkNativeAdaptersTest {
    @Test
    fun journalLoadAndCasStatusesMapFailClosed() = runBlocking {
        val backend = AdapterJournalBackend()
        val adapter = AndroidRemoraLinkJournalBackend(
            RemoraLinkJournalStore(backend),
            Dispatchers.Unconfined,
        )

        assertSame(AppRemoraLinkJournalLoad.Missing, adapter.load())
        assertEquals(
            AppRemoraLinkJournalWriteOutcome.STORED,
            adapter.compareAndSwap(null, AppRemoraLinkJournalSnapshot(1uL, byteArrayOf(7))),
        )
        val loaded = adapter.load() as AppRemoraLinkJournalLoad.Loaded
        assertEquals(1uL, loaded.snapshot.revision)
        assertArrayEquals(byteArrayOf(7), loaded.snapshot.payload)
        assertEquals(
            AppRemoraLinkJournalWriteOutcome.CONFLICT,
            adapter.compareAndSwap(null, AppRemoraLinkJournalSnapshot(1uL, byteArrayOf(8))),
        )
        assertEquals(
            AppRemoraLinkJournalWriteOutcome.UNAVAILABLE,
            adapter.compareAndSwap(1uL, AppRemoraLinkJournalSnapshot(9uL, byteArrayOf(9))),
        )

        backend.readOverride = RemoraLinkJournalBackendRead.Corrupt
        assertSame(AppRemoraLinkJournalLoad.Unavailable, adapter.load())
        assertEquals(
            AppRemoraLinkJournalWriteOutcome.UNAVAILABLE,
            adapter.compareAndSwap(1uL, AppRemoraLinkJournalSnapshot(2uL, byteArrayOf(9))),
        )
        backend.readOverride = RemoraLinkJournalBackendRead.Failed
        assertSame(AppRemoraLinkJournalLoad.Unavailable, adapter.load())
    }

    @Test
    fun journalAdapterDoesNotAliasPayloadArrays() = runBlocking {
        val backend = AdapterJournalBackend()
        val adapter = AndroidRemoraLinkJournalBackend(
            RemoraLinkJournalStore(backend),
            Dispatchers.Unconfined,
        )
        val payload = byteArrayOf(1, 2, 3)
        val replacement = AppRemoraLinkJournalSnapshot(1uL, payload)
        assertEquals(AppRemoraLinkJournalWriteOutcome.STORED, adapter.compareAndSwap(null, replacement))
        payload.fill(9)
        replacement.payload.fill(8)

        val first = adapter.load() as AppRemoraLinkJournalLoad.Loaded
        assertArrayEquals(byteArrayOf(1, 2, 3), first.snapshot.payload)
        first.snapshot.payload.fill(7)
        val second = adapter.load() as AppRemoraLinkJournalLoad.Loaded
        assertArrayEquals(byteArrayOf(1, 2, 3), second.snapshot.payload)
    }

    @Test
    fun transportIdentityMappingsAreFailClosedAndArraysAreOwned() = runBlocking {
        val backend = AdapterTransportBackend()
        val adapter = AndroidRemoraLinkTransportIdentityBackend(
            RemoraLinkTransportIdentityStore(backend),
            Dispatchers.Unconfined,
        )
        val candidate = ByteArray(REMORA_LINK_TRANSPORT_IDENTITY_BYTES) { it.toByte() }
        val returned = adapter.loadOrCreate(candidate)
        candidate.fill(0x55)
        returned.fill(0x66)
        assertArrayEquals(
            ByteArray(REMORA_LINK_TRANSPORT_IDENTITY_BYTES) { it.toByte() },
            backend.identityBytes,
        )

        val invalid = runCatching { adapter.loadOrCreate(byteArrayOf(1)) }.exceptionOrNull()
        assertTrue(invalid is AppRemoraLinkTransportIdentityException.Unavailable)

        backend.identityBytes = null
        backend.readOverride = RemoraLinkTransportIdentityBackendRead.Corrupt
        val corrupt = runCatching { adapter.loadOrCreate(ByteArray(32)) }.exceptionOrNull()
        assertTrue(corrupt is AppRemoraLinkTransportIdentityException.Unavailable)
        backend.readOverride = RemoraLinkTransportIdentityBackendRead.Failed
        val failed = runCatching { adapter.loadOrCreate(ByteArray(32)) }.exceptionOrNull()
        assertTrue(failed is AppRemoraLinkTransportIdentityException.Unavailable)
    }

    @Test
    fun deviceKeyAssuranceAndFailureMappingsAreExhaustive() = runBlocking {
        val custody = AdapterDeviceKeyCustody()
        val adapter = AndroidRemoraLinkDeviceKeyBackend(custody, Dispatchers.Unconfined)
        val publicKey = ByteArray(65).apply { this[0] = 4 }

        val assurances = listOf(
            RemoraLinkKeyAssurance.STRONGBOX to AppRemoraLinkKeyAssurance.STRONG_BOX,
            RemoraLinkKeyAssurance.TRUSTED_ENVIRONMENT to
                AppRemoraLinkKeyAssurance.TRUSTED_EXECUTION_ENVIRONMENT,
            RemoraLinkKeyAssurance.UNKNOWN_SECURE to AppRemoraLinkKeyAssurance.UNKNOWN_SECURE,
            RemoraLinkKeyAssurance.DEBUG_EMULATOR_SOFTWARE to
                AppRemoraLinkKeyAssurance.SOFTWARE_DEBUG_ONLY,
        )
        assurances.forEach { (native, expected) ->
            custody.ensureStatus = RemoraLinkDeviceKeyStatus.Ready(publicKey, native)
            assertEquals(expected, adapter.ensureHardwareKey("authenticated-host").assurance)
        }

        val failures = listOf(
            RemoraLinkDeviceKeyFailure.INVALID_OPAQUE_SLOT to
                AppRemoraLinkDeviceKeyException.Unavailable::class.java,
            RemoraLinkDeviceKeyFailure.KEY_NOT_FOUND to
                AppRemoraLinkDeviceKeyException.Missing::class.java,
            RemoraLinkDeviceKeyFailure.KEY_INVALIDATED to
                AppRemoraLinkDeviceKeyException.Invalidated::class.java,
            RemoraLinkDeviceKeyFailure.SOFTWARE_BACKED_KEY_REJECTED to
                AppRemoraLinkDeviceKeyException.HardwareUnavailable::class.java,
            RemoraLinkDeviceKeyFailure.UNSUPPORTED_KEY to
                AppRemoraLinkDeviceKeyException.HardwareUnavailable::class.java,
            RemoraLinkDeviceKeyFailure.KEYSTORE_FAILURE to
                AppRemoraLinkDeviceKeyException.Unavailable::class.java,
            RemoraLinkDeviceKeyFailure.OPERATION_UNAVAILABLE to
                AppRemoraLinkDeviceKeyException.Unavailable::class.java,
            RemoraLinkDeviceKeyFailure.INVALID_SIGNATURE to
                AppRemoraLinkDeviceKeyException.InvalidSignature::class.java,
        )
        failures.forEach { (failure, expectedClass) ->
            custody.ensureStatus = RemoraLinkDeviceKeyStatus.Unavailable(failure)
            val thrown = runCatching { adapter.ensureHardwareKey("authenticated-host") }
                .exceptionOrNull()
            assertEquals(expectedClass, thrown?.javaClass)
        }
    }

    @Test
    fun deviceLoadIsNonCreatingAndMissingStaysMissing() = runBlocking {
        val custody = AdapterDeviceKeyCustody().apply {
            loadStatus = RemoraLinkDeviceKeyStatus.Unavailable(
                RemoraLinkDeviceKeyFailure.KEY_NOT_FOUND,
            )
        }
        val adapter = AndroidRemoraLinkDeviceKeyBackend(custody, Dispatchers.Unconfined)

        assertSame(AppRemoraLinkHardwareKeyLoad.Missing, adapter.loadHardwareKey("durable-slot"))
        assertEquals(1, custody.loadCalls)
        assertEquals(0, custody.ensureCalls)
    }

    @Test
    fun deviceBoundaryOwnsPublicKeyMessageAndSignatureArrays() = runBlocking {
        val publicKey = ByteArray(65).apply { this[0] = 4 }
        val signature = byteArrayOf(0x30, 1, 2, 3)
        val custody = AdapterDeviceKeyCustody().apply {
            ensureStatus = RemoraLinkDeviceKeyStatus.Ready(
                publicKey,
                RemoraLinkKeyAssurance.UNKNOWN_SECURE,
            )
            signatureStatus = RemoraLinkSignatureStatus.Signed(signature)
        }
        val adapter = AndroidRemoraLinkDeviceKeyBackend(custody, Dispatchers.Unconfined)

        val key = adapter.ensureHardwareKey("host-a")
        key.publicKeySec1.fill(9)
        assertEquals(4, publicKey[0].toInt())

        val message = byteArrayOf(7, 8, 9)
        val returnedSignature = adapter.signMessage("slot", message)
        assertArrayEquals(byteArrayOf(7, 8, 9), custody.lastSignedMessage)
        assertArrayEquals(byteArrayOf(7, 8, 9), message)
        returnedSignature.fill(0)
        assertArrayEquals(byteArrayOf(0x30, 1, 2, 3), signature)
    }

    @Test
    fun deletionMappingsAreFailClosed() = runBlocking {
        val custody = AdapterDeviceKeyCustody()
        val adapter = AndroidRemoraLinkDeviceKeyBackend(custody, Dispatchers.Unconfined)

        custody.deletionStatus = RemoraLinkDeviceKeyDeletionStatus.DELETED
        assertEquals(AppRemoraLinkKeyDeletionStatus.DELETED, adapter.deleteHardwareKey("slot"))
        custody.deletionStatus = RemoraLinkDeviceKeyDeletionStatus.ALREADY_MISSING
        assertEquals(
            AppRemoraLinkKeyDeletionStatus.ALREADY_MISSING,
            adapter.deleteHardwareKey("slot"),
        )
        listOf(
            RemoraLinkDeviceKeyDeletionStatus.INVALID_OPAQUE_SLOT,
            RemoraLinkDeviceKeyDeletionStatus.KEYSTORE_FAILURE,
        ).forEach { status ->
            custody.deletionStatus = status
            val thrown = runCatching { adapter.deleteHardwareKey("slot") }.exceptionOrNull()
            assertTrue(thrown is AppRemoraLinkDeviceKeyException.Unavailable)
        }
    }

    @Test
    fun hostDerivedSlotIsStableOpaqueAndBounded() {
        val slot = checkNotNull(remoraLinkDeviceKeySlot("authenticated-host-id"))
        assertEquals(slot, remoraLinkDeviceKeySlot("authenticated-host-id"))
        assertNotEquals(slot, remoraLinkDeviceKeySlot("different-host-id"))
        assertFalse(slot.contains("authenticated-host-id"))
        assertTrue(slot.length <= 256)
        assertEquals(null, remoraLinkDeviceKeySlot(""))
        assertEquals(null, remoraLinkDeviceKeySlot("bad\nhost"))
        assertEquals(null, remoraLinkDeviceKeySlot("x".repeat(1025)))
    }

    @Test
    fun cancelledIoCallbackDoesNotReachStorage() {
        val executor = Executors.newSingleThreadExecutor()
        val dispatcher = executor.asCoroutineDispatcher()
        val blockerStarted = CountDownLatch(1)
        val releaseBlocker = CountDownLatch(1)
        val backend = AdapterJournalBackend()
        try {
            executor.execute {
                blockerStarted.countDown()
                releaseBlocker.await(10, TimeUnit.SECONDS)
            }
            assertTrue(blockerStarted.await(5, TimeUnit.SECONDS))
            runBlocking {
                val callback = launch { AndroidRemoraLinkJournalBackend(
                    RemoraLinkJournalStore(backend),
                    dispatcher,
                ).load() }
                callback.cancel()
                releaseBlocker.countDown()
                callback.join()
                assertTrue(callback.isCancelled)
            }
            assertEquals(0, backend.readCalls)
        } finally {
            releaseBlocker.countDown()
            dispatcher.close()
            executor.shutdownNow()
        }
    }

    @Test
    fun transportIdentityCopyIsZeroizedWhenCancellationWinsAtTrailingCheck() = runBlocking {
        val allocatedIdentity = AtomicReference<ByteArray>()
        val copyAllocated = CountDownLatch(1)
        val releaseTrailingCheck = CountDownLatch(1)
        val adapter = AndroidRemoraLinkTransportIdentityBackend(
            store = RemoraLinkTransportIdentityStore(AdapterTransportBackend()),
            ioDispatcher = Dispatchers.Default,
            beforeTrailingCancellationCheck = { identity ->
                allocatedIdentity.set(identity)
                copyAllocated.countDown()
                releaseTrailingCheck.await(10, TimeUnit.SECONDS)
            },
        )
        val callback = launch(
            context = Dispatchers.Default,
            start = kotlinx.coroutines.CoroutineStart.LAZY,
        ) {
            adapter.loadOrCreate(ByteArray(REMORA_LINK_TRANSPORT_IDENTITY_BYTES) { 0x5a })
        }
        callback.start()
        assertTrue(copyAllocated.await(5, TimeUnit.SECONDS))
        callback.cancel()
        releaseTrailingCheck.countDown()
        callback.join()

        assertTrue(callback.isCancelled)
        assertTrue(checkNotNull(allocatedIdentity.get()).all { it == 0.toByte() })
    }

    @Test
    fun transportIdentityCopyIsZeroizedWhenCancelledDuringReturnDispatch() = runBlocking {
        val callerExecutor = Executors.newSingleThreadExecutor()
        val ioExecutor = Executors.newSingleThreadExecutor()
        val callerDispatcher = callerExecutor.asCoroutineDispatcher()
        val ioDispatcher = ioExecutor.asCoroutineDispatcher()
        val allocatedIdentity = AtomicReference<ByteArray>()
        val ioResultAllocated = CountDownLatch(1)
        val releaseIoResult = CountDownLatch(1)
        val callerBlocked = CountDownLatch(1)
        val releaseCaller = CountDownLatch(1)
        val ioBlockCompleted = CountDownLatch(1)
        try {
            val adapter = AndroidRemoraLinkTransportIdentityBackend(
                store = RemoraLinkTransportIdentityStore(AdapterTransportBackend()),
                ioDispatcher = ioDispatcher,
                beforeTrailingCancellationCheck = { identity ->
                    allocatedIdentity.set(identity)
                    ioResultAllocated.countDown()
                    releaseIoResult.await(10, TimeUnit.SECONDS)
                },
            )
            val callback = launch(
                context = callerDispatcher,
                start = kotlinx.coroutines.CoroutineStart.LAZY,
            ) {
                adapter.loadOrCreate(ByteArray(REMORA_LINK_TRANSPORT_IDENTITY_BYTES) { 0x6b })
            }
            callback.start()
            assertTrue(ioResultAllocated.await(5, TimeUnit.SECONDS))

            // Occupy the distinct caller dispatcher before allowing IO to complete, forcing the
            // successful withContext result to wait in the return-dispatch queue.
            callerExecutor.execute {
                callerBlocked.countDown()
                releaseCaller.await(10, TimeUnit.SECONDS)
            }
            assertTrue(callerBlocked.await(5, TimeUnit.SECONDS))
            releaseIoResult.countDown()
            ioExecutor.execute { ioBlockCompleted.countDown() }
            assertTrue(ioBlockCompleted.await(5, TimeUnit.SECONDS))

            // IO has completed and queued the return, but the caller continuation cannot run yet.
            callback.cancel()
            releaseCaller.countDown()
            callback.join()

            assertTrue(callback.isCancelled)
            assertTrue(checkNotNull(allocatedIdentity.get()).all { it == 0.toByte() })
        } finally {
            releaseIoResult.countDown()
            releaseCaller.countDown()
            callerDispatcher.close()
            ioDispatcher.close()
            callerExecutor.shutdownNow()
            ioExecutor.shutdownNow()
        }
    }

    @Test
    fun signingMessageCopyIsZeroizedWhenCancelledDuringProviderOperation() = runBlocking {
        val copiedMessage = AtomicReference<ByteArray>()
        val signEntered = CountDownLatch(1)
        val releaseSign = CountDownLatch(1)
        val custody = AdapterDeviceKeyCustody().apply {
            onSign = { message ->
                copiedMessage.set(message)
                signEntered.countDown()
                releaseSign.await(10, TimeUnit.SECONDS)
            }
        }
        val adapter = AndroidRemoraLinkDeviceKeyBackend(custody, Dispatchers.Default)
        val callback = launch(
            context = Dispatchers.Default,
            start = kotlinx.coroutines.CoroutineStart.LAZY,
        ) {
            adapter.signMessage("slot", byteArrayOf(7, 8, 9))
        }
        callback.start()
        assertTrue(signEntered.await(5, TimeUnit.SECONDS))
        callback.cancel()
        releaseSign.countDown()
        callback.join()

        assertTrue(callback.isCancelled)
        assertTrue(checkNotNull(copiedMessage.get()).all { it == 0.toByte() })
    }

    @Test
    fun onlyMalformedSignatureEvidenceMapsToInvalidSignature() {
        val keyPair = KeyPairGenerator.getInstance("EC").apply {
            initialize(ECGenParameterSpec("secp256r1"))
        }.generateKeyPair()
        val valid = Signature.getInstance("SHA256withECDSA").run {
            initSign(keyPair.private)
            update("canonical-message".toByteArray())
            sign()
        }

        assertTrue(isCanonicalP256EcdsaDerSignature(valid))
        assertFalse(isCanonicalP256EcdsaDerSignature(byteArrayOf(0x30, 0x00)))
        assertFalse(
            isCanonicalP256EcdsaDerSignature(
                valid.copyOf().apply { this[1] = (this[1] - 1).toByte() },
            ),
        )
    }

    @Test
    fun configurationGateIsClosedUntilSuccessAndRetriesAfterCancellation() = runBlocking {
        val scope = CoroutineScope(SupervisorJob() + Dispatchers.Default)
        val firstAttemptEntered = CompletableDeferred<Unit>()
        var attempts = 0
        val gate = RemoraLinkConfigurationGate(scope) {
            attempts += 1
            if (attempts == 1) {
                firstAttemptEntered.complete(Unit)
                awaitCancellation()
            }
        }
        try {
            assertThrows(RemoraLinkV2UnavailableException::class.java) {
                runBlocking { gate.runWhileAvailable { "should-not-run" } }
            }
            val first = gate.configureInSeparateJob()
            firstAttemptEntered.await()
            first.cancelAndJoin()
            assertFalse(gate.available.value)

            gate.configureInSeparateJob().join()
            assertTrue(gate.available.value)
            assertEquals(2, attempts)
            assertEquals("ready", gate.runWhileAvailable { "ready" })
        } finally {
            scope.cancel()
        }
    }

    @Test
    fun configurationGateRetriesAfterOrdinaryFailure() = runBlocking {
        val scope = CoroutineScope(SupervisorJob() + Dispatchers.Default)
        var attempts = 0
        val gate = RemoraLinkConfigurationGate(scope) {
            attempts += 1
            if (attempts == 1) error("preflight unavailable")
        }
        try {
            gate.configureInSeparateJob().join()
            assertFalse(gate.available.value)
            assertEquals("preflight unavailable", gate.lastFailure?.message)

            gate.configureInSeparateJob().join()
            assertTrue(gate.available.value)
            assertEquals(null, gate.lastFailure)
            assertEquals(2, attempts)
        } finally {
            scope.cancel()
        }
    }
}

private class AdapterJournalBackend : RemoraLinkJournalBackend {
    override val storageIdentity = "adapter-journal-${System.nanoTime()}"
    var envelope: ByteArray? = null
    var readOverride: RemoraLinkJournalBackendRead? = null
    var readCalls = 0

    override fun read(): RemoraLinkJournalBackendRead {
        readCalls += 1
        return readOverride ?: envelope?.copyOf()?.let(RemoraLinkJournalBackendRead::Present)
            ?: RemoraLinkJournalBackendRead.Missing
    }

    override fun replace(envelope: ByteArray): RemoraLinkJournalBackendWrite {
        this.envelope = envelope.copyOf()
        return RemoraLinkJournalBackendWrite.COMMITTED
    }
}

private class AdapterTransportBackend : RemoraLinkTransportIdentityBackend {
    override val storageIdentity = "adapter-transport-${System.nanoTime()}"
    var identityBytes: ByteArray? = null
    var readOverride: RemoraLinkTransportIdentityBackendRead? = null

    override fun read(): RemoraLinkTransportIdentityBackendRead =
        readOverride ?: identityBytes?.copyOf()?.let(RemoraLinkTransportIdentityBackendRead::Present)
            ?: RemoraLinkTransportIdentityBackendRead.Missing

    override fun create(identityBytes: ByteArray): RemoraLinkTransportIdentityBackendWrite {
        if (this.identityBytes != null) return RemoraLinkTransportIdentityBackendWrite.ALREADY_EXISTS
        this.identityBytes = identityBytes.copyOf()
        return RemoraLinkTransportIdentityBackendWrite.COMMITTED
    }
}

private class AdapterDeviceKeyCustody : RemoraLinkDeviceKeyCustody {
    var ensureStatus: RemoraLinkDeviceKeyStatus = RemoraLinkDeviceKeyStatus.Ready(
        ByteArray(65).apply { this[0] = 4 },
        RemoraLinkKeyAssurance.STRONGBOX,
    )
    var loadStatus: RemoraLinkDeviceKeyStatus = ensureStatus
    var signatureStatus: RemoraLinkSignatureStatus = RemoraLinkSignatureStatus.Signed(byteArrayOf(1))
    var deletionStatus = RemoraLinkDeviceKeyDeletionStatus.DELETED
    var ensureCalls = 0
    var loadCalls = 0
    var lastSignedMessage: ByteArray? = null
    var onSign: ((ByteArray) -> Unit)? = null

    override fun ensureKey(opaqueSlot: String): RemoraLinkDeviceKeyStatus {
        ensureCalls += 1
        return ensureStatus
    }

    override fun loadKey(opaqueSlot: String): RemoraLinkDeviceKeyStatus {
        loadCalls += 1
        return loadStatus
    }

    override fun sign(
        opaqueSlot: String,
        canonicalMessage: ByteArray,
    ): RemoraLinkSignatureStatus {
        onSign?.invoke(canonicalMessage)
        lastSignedMessage = canonicalMessage.copyOf()
        return signatureStatus
    }

    override fun deleteKey(opaqueSlot: String): RemoraLinkDeviceKeyDeletionStatus = deletionStatus
}
