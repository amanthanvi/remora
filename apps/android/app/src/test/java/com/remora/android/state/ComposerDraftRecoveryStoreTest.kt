package com.remora.android.state

import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.launch
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.asCoroutineDispatcher
import kotlinx.coroutines.async
import kotlinx.coroutines.withContext
import kotlinx.coroutines.withTimeout
import java.util.concurrent.CountDownLatch
import java.util.concurrent.Executors
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicInteger
import org.junit.Assert.assertEquals
import org.junit.Assert.assertSame
import org.junit.Assert.assertTrue
import org.junit.Test
import uniffi.codex_mobile_client.AppAskForApproval
import uniffi.codex_mobile_client.ReasoningEffort
import uniffi.codex_mobile_client.ServiceTier
import uniffi.codex_mobile_client.ThreadKey

class ComposerDraftRecoveryStoreTest {
    private val destination = ComposerDraftDestination.Conversation(ThreadKey("server", "thread"))
    private suspend fun openStore() = ComposerDraftRecoveryStore().also { it.load() }

    @Test
    fun blockedDiskWriteLeavesUiDispatcherResponsiveAndSerializesTransactions() = runBlocking {
        Executors.newSingleThreadExecutor { Thread(it, "composer-test-ui") }.asCoroutineDispatcher().use { ui ->
            val writeStarted = CompletableDeferred<Unit>()
            val releaseWrite = CountDownLatch(1)
            val writes = AtomicInteger()
            val persistedIds = mutableListOf<List<Long>>()
            val store = ComposerDraftRecoveryStore(persistenceFactory = {
                assertTrue(Thread.currentThread().name != "composer-test-ui")
                object : ComposerDraftRecoveryPersistence {
                    override fun read(): ByteArray? {
                        assertTrue(Thread.currentThread().name != "composer-test-ui")
                        return null
                    }
                    override fun replace(plaintext: ByteArray) {
                        assertTrue(Thread.currentThread().name != "composer-test-ui")
                        if (writes.incrementAndGet() == 1) {
                            writeStarted.complete(Unit)
                            check(releaseWrite.await(5, TimeUnit.SECONDS))
                        }
                        persistedIds.add(ComposerDraftRecoveryCodec.decode(plaintext).map { it.id })
                    }
                }
            })
            val first = async(ui) { store.begin(destination, AppModel.ComposerDraft("first"), AppComposerPayload("first")) }
            try {
                withTimeout(5_000) { writeStarted.await() }
                val second = async(ui) { store.begin(destination, AppModel.ComposerDraft("second"), AppComposerPayload("second")) }
                withTimeout(2_000) { withContext(ui) { assertEquals(1, writes.get()) } }
                releaseWrite.countDown()
                val ids = listOf(first.await(), second.await())
                assertEquals(ids, store.entries.value.map { it.id })
                assertEquals(listOf(listOf(ids.first()), ids), persistedIds)
            } finally {
                releaseWrite.countDown()
            }
        }
    }

    @Test
    fun lateFailureRetainsAttachmentsAndNeverOverwritesNewerDraft() = runBlocking {
        val store = openStore()
        val image = ComposerImageAttachment(byteArrayOf(1, 2, 3), "image/png")
        val files = listOf(ComposerFileAttachment("notes.txt", "/tmp/notes.txt"))
        val sent = AppModel.ComposerDraft("@skill @plugin original", image, files)
        val payload = AppComposerPayload(
            text = sent.text,
            additionalInputs = listOf(image.toUserInput()),
            fileAttachments = files,
            approvalPolicy = AppAskForApproval.Never,
            model = "chosen-model",
            reasoningEffort = ReasoningEffort.HIGH,
            serviceTier = ServiceTier.FAST,
        )
        val id = store.begin(destination, sent, payload)
        val response = CompletableDeferred<Unit>()
        val appScope = CoroutineScope(SupervisorJob() + Dispatchers.Unconfined)
        try {
            val submission = appScope.launch { runCatching { store.submit(id) { response.await() } } }
            val newer = AppModel.ComposerDraft("newer edit")
            response.completeExceptionally(IllegalStateException("transport interrupted"))
            submission.join()
            val recovered = store.entries.value.single()
            assertEquals(ComposerDraftRecoveryStatus.UNCONFIRMED, recovered.status)
            assertEquals(payload, recovered.payload)
            assertSame(image, recovered.draft.attachment)
            assertEquals(files, recovered.draft.fileAttachments)
            assertEquals("newer edit", newer.text)
            assertEquals(sent, store.restore(id, newer))
            assertEquals(listOf(sent, newer), store.entries.value.map { it.draft })
            assertTrue(store.entries.value.all { it.status == ComposerDraftRecoveryStatus.SAVED })
        } finally {
            appScope.cancel()
        }
    }

    @Test
    fun outOfOrderCompletionCannotRemoveAnotherSubmission() = runBlocking {
        val store = openStore()
        val first = store.begin(destination, AppModel.ComposerDraft("first"), AppComposerPayload("first"))
        val second = store.begin(destination, AppModel.ComposerDraft("second"), AppComposerPayload("second"))
        store.complete(second)
        store.unconfirmed(first)
        assertEquals(listOf(first), store.entries.value.map { it.id })
        assertEquals("first", store.entries.value.single().draft.text)
        assertEquals(null, store.restore(second, AppModel.ComposerDraft("newer")))
    }

    @Test
    fun failedHomeThreadCreationRetainsItsProjectAndLaunchSettings() = runBlocking {
        val store = openStore()
        val home = ComposerDraftDestination.Home("server", "/project")
        val params = AppThreadLaunchConfig(model = "model").toAppStartThreadRequest("/project")
        val original = AppModel.ComposerDraft("initial turn")
        val id = store.begin(home, original, AppComposerPayload(original.text), params)
        runCatching { store.submit(id) { error("thread creation failed") } }
        val recovery = store.entries.value.single()
        assertEquals(home, recovery.destination)
        assertEquals(params, recovery.threadStartRequest)
        assertEquals(null, recovery.createdThreadKey)
        assertEquals(original, store.restore(id, AppModel.ComposerDraft.EMPTY))
        assertEquals(original, store.entries.value.single().draft)
        assertEquals(ComposerDraftRecoveryStatus.SAVED, store.entries.value.single().status)
    }

    @Test
    fun initialTurnFailureRetainsCreatedThreadIdentity() = runBlocking {
        val store = openStore()
        val id = store.begin(
            ComposerDraftDestination.Home("server", "/project"),
            AppModel.ComposerDraft("initial turn"),
            AppComposerPayload("initial turn"),
        )
        val created = ThreadKey("server", "created-thread")
        store.threadCreated(id, created)
        store.unconfirmed(id)
        assertEquals(created, store.entries.value.single().createdThreadKey)
    }

    @Test
    fun navigatingAwayDoesNotCancelTheAppOwnedSubmission() = runBlocking {
        val store = openStore()
        val response = CompletableDeferred<Unit>()
        val appScope = CoroutineScope(SupervisorJob() + Dispatchers.Unconfined)
        val viewScope = CoroutineScope(SupervisorJob() + Dispatchers.Unconfined)
        val id = store.begin(destination, AppModel.ComposerDraft("sent"), AppComposerPayload("sent"))
        try {
            val submission = appScope.launch { runCatching { store.submit(id) { response.await() } } }
            viewScope.cancel()
            assertTrue(submission.isActive)
            response.completeExceptionally(IllegalStateException("response lost"))
            submission.join()
            assertEquals(ComposerDraftRecoveryStatus.UNCONFIRMED, store.entries.value.single().status)
        } finally {
            appScope.cancel()
            viewScope.cancel()
        }
    }
}
