package com.remora.android.state

import java.io.File
import java.nio.channels.FileChannel
import java.nio.file.StandardOpenOption
import javax.crypto.Cipher
import javax.crypto.KeyGenerator
import javax.crypto.spec.GCMParameterSpec
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.CoroutineStart
import kotlinx.coroutines.launch
import kotlinx.coroutines.withTimeout
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicBoolean
import org.junit.Assert.*
import org.junit.Rule
import org.junit.Test
import org.junit.rules.TemporaryFolder
import uniffi.codex_mobile_client.*

class ComposerDraftRecoveryPersistenceTest {
    private suspend fun openStore(persistence: ComposerDraftRecoveryPersistence) =
        ComposerDraftRecoveryStore(persistence).also { it.load() }

    private suspend fun <T : Throwable> assertThrows(type: Class<T>, operation: suspend () -> Unit) {
        assertTrue(type.isInstance(runCatching { operation() }.exceptionOrNull()))
    }
    @get:Rule val temporary = TemporaryFolder()
    private val key = KeyGenerator.getInstance("AES").apply { init(256) }.generateKey()
    private val home = ComposerDraftDestination.Home("server", "/project")
    private val destination = ComposerDraftDestination.Conversation(ThreadKey("server", "thread"))
    private fun backend(directory: File, beforeCommit: () -> Unit = {}) = AtomicComposerDraftRecoveryPersistence(
        directory,
        encrypt = { plaintext ->
            val cipher = Cipher.getInstance("AES/GCM/NoPadding")
            cipher.init(Cipher.ENCRYPT_MODE, key)
            cipher.iv + cipher.doFinal(plaintext)
        },
        decrypt = { encrypted ->
            val cipher = Cipher.getInstance("AES/GCM/NoPadding")
            cipher.init(Cipher.DECRYPT_MODE, key, GCMParameterSpec(128, encrypted.copyOfRange(0, 12)))
            cipher.doFinal(encrypted, 12, encrypted.size - 12)
        },
        syncDirectory = { FileChannel.open(it.toPath(), StandardOpenOption.READ).use { channel -> channel.force(true) } },
        beforeCommit = beforeCommit,
    )

    @Test
    fun processRestartRecoversInflightTextImagesFilesMentionsAndLaunchSettingsWithoutSending() = runBlocking {
        val directory = temporary.newFolder()
        val first = openStore(backend(directory))
        val image = ComposerImageAttachment(byteArrayOf(4, 5, 6), "image/png")
        val files = listOf(ComposerFileAttachment("notes", "/remote/notes"))
        val draft = AppModel.ComposerDraft("@skill @plugin original", image, files)
        val approval = AppAskForApproval.Granular(true, false, true, false, true)
        val access = AppReadOnlyAccess.Restricted(true, listOf(AbsolutePath("/remote")))
        val payload = AppComposerPayload(draft.text, listOf(image.toUserInput(),
            AppUserInput.Skill("skill", AbsolutePath("/remote/skill")), AppUserInput.Mention("plugin", "plugin/path"),
            AppUserInput.Text("token", listOf(AppTextElement(AppByteRange(0u, 5u))))), files,
            approval, AppSandboxPolicy.WorkspaceWrite(listOf(AbsolutePath("/project")), access, false, true, false),
            "model", ReasoningEffort.HIGH, ServiceTier.FAST)
        val start = AppStartThreadRequest("codex", "model", "/project", approval, AppSandboxMode.WORKSPACE_WRITE,
            "instructions", true, listOf(AppDynamicToolSpec("tool", "description", "{}", true)), false)
        val id = first.begin(home, draft, payload, start)
        first.threadCreated(id, ThreadKey("server", "created"))
        val second = openStore(backend(directory))
        val recovered = second.entries.value.single()
        assertEquals(ComposerDraftRecoveryStatus.UNCONFIRMED, recovered.status)
        assertEquals(home, recovered.destination)
        assertEquals(draft.text, recovered.draft.text)
        assertArrayEquals(image.data, recovered.draft.attachment!!.data)
        assertEquals(files, recovered.draft.fileAttachments)
        assertEquals(payload, recovered.payload)
        assertEquals(start, recovered.threadStartRequest)
        assertEquals(ThreadKey("server", "created"), recovered.createdThreadKey)
        assertFalse(File(directory, AtomicComposerDraftRecoveryPersistence.FILE_NAME).readText().contains(draft.text))
        val newer = AppModel.ComposerDraft("newer edit")
        assertEquals(draft.text, second.restore(id, newer)!!.text)
        val third = openStore(backend(directory))
        assertEquals(listOf(draft.text, newer.text), third.entries.value.map { it.draft.text })
        assertTrue(third.entries.value.all { it.status == ComposerDraftRecoveryStatus.SAVED })
    }

    @Test
    fun restoreRemainsDurableUntilAnExplicitSubmissionReplacesIt() = runBlocking {
        val directory = temporary.newFolder()
        val original = AppModel.ComposerDraft("original")
        val first = openStore(backend(directory))
        val id = first.begin(destination, original, AppComposerPayload(original.text))
        val second = openStore(backend(directory))
        val restored = second.restore(id, AppModel.ComposerDraft.EMPTY)!!
        val third = openStore(backend(directory))
        val next = third.begin(destination, restored, AppComposerPayload(restored.text))
        assertNotEquals(id, next)
        assertEquals(1, third.entries.value.size)
        var sends = 0
        third.submit(next) { sends++ }
        assertEquals(1, sends)
        assertTrue(openStore(backend(directory)).entries.value.isEmpty())
    }

    @Test
    fun failedPersistencePreventsClearAndDispatchAndPreservesThePreviousJournal() = runBlocking {
        val directory = temporary.newFolder()
        val healthy = openStore(backend(directory))
        healthy.begin(destination, AppModel.ComposerDraft("previous"), AppComposerPayload("previous"))
        val file = File(directory, AtomicComposerDraftRecoveryPersistence.FILE_NAME)
        val previousBytes = file.readBytes()
        val failing = openStore(backend(directory) { error("disk unavailable") })
        var editor = AppModel.ComposerDraft("keep this")
        var dispatched = false
        assertThrows(ComposerDraftPersistenceException::class.java) {
            failing.begin(destination, editor, AppComposerPayload(editor.text))
            editor = AppModel.ComposerDraft.EMPTY
            dispatched = true
        }
        assertEquals("keep this", editor.text)
        assertFalse(dispatched)
        assertNotNull(failing.storageError.value)
        assertArrayEquals(previousBytes, file.readBytes())
        assertEquals("previous", openStore(backend(directory)).entries.value.single().draft.text)
    }

    @Test
    fun corruptCiphertextIsNotResetOrOverwrittenByANewSubmission() = runBlocking {
        val directory = temporary.newFolder()
        val file = File(directory, AtomicComposerDraftRecoveryPersistence.FILE_NAME)
        val originalBytes = byteArrayOf(1, 2, 3, 4)
        file.writeBytes(originalBytes)
        val store = openStore(backend(directory))
        assertTrue(store.entries.value.isEmpty())
        assertNotNull(store.storageError.value)
        assertThrows(ComposerDraftPersistenceException::class.java) {
            store.begin(destination, AppModel.ComposerDraft("new"), AppComposerPayload("new"))
        }
        assertArrayEquals(originalBytes, file.readBytes())
    }

    @Test
    fun unknownVersionAndUnknownStatusBlockWritesWithoutDeletingRecovery() = runBlocking {
        for (json in listOf("{\"version\":2,\"entries\":[]}",
            ComposerDraftRecoveryCodec.encode(listOf(RecoverableComposerDraft(1, destination,
                AppModel.ComposerDraft("original"), ComposerDraftRecoveryStatus.SAVED)))
                .toString(Charsets.UTF_8).replace("SAVED", "FUTURE_STATUS"))) {
            val directory = temporary.newFolder()
            backend(directory).replace(json.toByteArray())
            val file = File(directory, AtomicComposerDraftRecoveryPersistence.FILE_NAME)
            val bytes = file.readBytes()
            val store = openStore(backend(directory))
            assertNotNull(store.storageError.value)
            assertThrows(ComposerDraftPersistenceException::class.java) {
                store.begin(home, AppModel.ComposerDraft("new"), AppComposerPayload("new"))
            }
            assertArrayEquals(bytes, file.readBytes())
        }
    }

    @Test
    fun failedCompletionKeepsAnUnconfirmedEntryAcrossRestart() = runBlocking {
        val directory = temporary.newFolder()
        var fail = false
        val store = openStore(backend(directory) { if (fail) error("disk full") })
        val id = store.begin(destination, AppModel.ComposerDraft("original"), AppComposerPayload("original"))
        fail = true
        val result = runCatching { store.submit(id) {} }
        assertTrue(result.isFailure)
        assertEquals(ComposerDraftRecoveryStatus.UNCONFIRMED, store.entries.value.single().status)
        assertEquals(ComposerDraftRecoveryStatus.UNCONFIRMED,
            openStore(backend(directory)).entries.value.single().status)
    }

    @Test
    fun failedRestoreKeepsBothTheEditorAndDurableRecovery() = runBlocking {
        val directory = temporary.newFolder()
        val first = openStore(backend(directory))
        val id = first.begin(destination, AppModel.ComposerDraft("original"), AppComposerPayload("original"))
        val second = openStore(backend(directory) { error("disk full") })
        var editor = AppModel.ComposerDraft("newer")
        assertThrows(ComposerDraftPersistenceException::class.java) {
            editor = second.restore(id, editor)!!
        }
        assertEquals("newer", editor.text)
        assertEquals("original", openStore(backend(directory)).entries.value.single().draft.text)
    }

    @Test
    fun reopenedUnconfirmedEntryCannotBeSentWithoutANewExplicitBegin() = runBlocking {
        val directory = temporary.newFolder()
        val first = openStore(backend(directory))
        val id = first.begin(destination, AppModel.ComposerDraft("original"), AppComposerPayload("original"))
        val restarted = openStore(backend(directory))
        var sends = 0
        assertTrue(runCatching { restarted.submit(id) { sends++ } }.isFailure)
        assertEquals(0, sends)
        assertEquals(ComposerDraftRecoveryStatus.UNCONFIRMED, restarted.entries.value.single().status)
    }

    @Test
    fun everySupportedInputAndPolicyVariantRoundTripsThroughTheStore() = runBlocking {
        val approvals = listOf(AppAskForApproval.Never, AppAskForApproval.OnFailure,
            AppAskForApproval.OnRequest, AppAskForApproval.UnlessTrusted,
            AppAskForApproval.Granular(true, false, true, false, true))
        val sandboxes = listOf(AppSandboxPolicy.DangerFullAccess,
            AppSandboxPolicy.ReadOnly(AppReadOnlyAccess.FullAccess, false),
            AppSandboxPolicy.ExternalSandbox(AppNetworkAccess.ENABLED),
            AppSandboxPolicy.WorkspaceWrite(emptyList(), AppReadOnlyAccess.FullAccess, true, false, true))
        val inputs = listOf(AppUserInput.Text("text", emptyList()), AppUserInput.Image("data:image/png;base64,AQ=="),
            AppUserInput.LocalImage(AbsolutePath("/remote/image.png")),
            AppUserInput.Skill("skill", AbsolutePath("/remote/skill")), AppUserInput.Mention("plugin", "plugin/path"))
        for (approval in approvals) for (sandbox in sandboxes) {
            val directory = temporary.newFolder()
            val first = openStore(backend(directory))
            val payload = AppComposerPayload("text", inputs, approvalPolicy = approval, sandboxPolicy = sandbox)
            first.begin(home, AppModel.ComposerDraft("text"), payload)
            assertEquals(payload, openStore(backend(directory)).entries.value.single().payload)
        }
    }

    @Test
    fun postRenameSyncFailureBlocksDispatchButRetainsThePublishedDraft() = runBlocking {
        val directory = temporary.newFolder()
        val working = backend(directory)
        val postCommitFailure = object : ComposerDraftRecoveryPersistence {
            override fun read(): ByteArray? = working.read()
            override fun replace(plaintext: ByteArray) {
                working.replace(plaintext)
                error("directory sync outcome unavailable")
            }
        }
        val store = openStore(postCommitFailure)
        assertThrows(ComposerDraftPersistenceException::class.java) {
            store.begin(destination, AppModel.ComposerDraft("original"), AppComposerPayload("original"))
        }
        assertTrue(store.entries.value.isEmpty())
        val recovered = openStore(backend(directory)).entries.value.single()
        assertEquals("original", recovered.draft.text)
        assertEquals(ComposerDraftRecoveryStatus.UNCONFIRMED, recovered.status)
    }

    @Test
    fun explicitDiscardIsDurableAndCannotDiscardAnInflightSubmission() = runBlocking {
        val directory = temporary.newFolder()
        val first = openStore(backend(directory))
        val id = first.begin(home, AppModel.ComposerDraft("original"), AppComposerPayload("original"))
        first.discard(id)
        assertEquals(1, first.entries.value.size)
        val second = openStore(backend(directory))
        second.discard(id)
        assertTrue(openStore(backend(directory)).entries.value.isEmpty())
    }

    @Test
    fun cancellationDuringAtomicBeginKeepsEditorAndPublishesUnconfirmedWithoutSending() = runBlocking {
        val directory = temporary.newFolder()
        val committing = CompletableDeferred<Unit>()
        val release = CountDownLatch(1)
        val store = openStore(backend(directory) {
            committing.complete(Unit)
            check(release.await(5, TimeUnit.SECONDS))
        })
        var editor = AppModel.ComposerDraft("original")
        var sent = false
        val save = launch {
            val id = store.begin(destination, editor, AppComposerPayload(editor.text))
            editor = AppModel.ComposerDraft.EMPTY
            store.submit(id) { sent = true }
        }
        try {
            withTimeout(5_000) { committing.await() }
            assertEquals("original", editor.text)
            editor = AppModel.ComposerDraft("newer edit during save")
            save.cancel()
        } finally {
            release.countDown()
        }
        save.join()
        assertEquals("newer edit during save", editor.text)
        assertFalse(sent)
        assertEquals(ComposerDraftRecoveryStatus.UNCONFIRMED, store.entries.value.single().status)
        val reopened = openStore(backend(directory)).entries.value.single()
        assertEquals("original", reopened.draft.text)
        assertEquals(ComposerDraftRecoveryStatus.UNCONFIRMED, reopened.status)
    }

    @Test
    fun cancellationDuringRestoreKeepsSourceAndDisplacedDraftDurable() = runBlocking {
        val directory = temporary.newFolder()
        val first = openStore(backend(directory))
        val id = first.begin(destination, AppModel.ComposerDraft("original"), AppComposerPayload("original"))
        val committing = CompletableDeferred<Unit>()
        val release = CountDownLatch(1)
        val store = openStore(backend(directory) {
            committing.complete(Unit)
            check(release.await(5, TimeUnit.SECONDS))
        })
        var editor = AppModel.ComposerDraft("displaced draft")
        val restoring = launch { store.restore(id, editor)?.let { editor = it } }
        try {
            withTimeout(5_000) { committing.await() }
            editor = AppModel.ComposerDraft("newer edit during restore")
            restoring.cancel()
        } finally {
            release.countDown()
        }
        restoring.join()
        assertEquals("newer edit during restore", editor.text)
        val reopened = openStore(backend(directory))
        assertEquals(listOf("original", "displaced draft"), reopened.entries.value.map { it.draft.text })
        assertTrue(reopened.entries.value.all { it.status == ComposerDraftRecoveryStatus.SAVED })
    }

    @Test
    fun canceledValidationOnlyMarksStillSubmittingEntriesUnconfirmed() = runBlocking {
        for (status in ComposerDraftRecoveryStatus.entries) {
            val directory = temporary.newFolder()
            val writeStarted = CompletableDeferred<Unit>()
            val releaseWrite = CountDownLatch(1)
            val holdWrite = AtomicBoolean(false)
            val store = openStore(backend(directory) {
                if (holdWrite.compareAndSet(true, false)) {
                    writeStarted.complete(Unit)
                    check(releaseWrite.await(5, TimeUnit.SECONDS))
                }
            })
            val id = store.begin(destination, AppModel.ComposerDraft("original"), AppComposerPayload("original"))
            if (status != ComposerDraftRecoveryStatus.SUBMITTING) store.unconfirmed(id)
            if (status == ComposerDraftRecoveryStatus.SAVED) store.restore(id, AppModel.ComposerDraft.EMPTY)
            var sends = 0
            if (status != ComposerDraftRecoveryStatus.SUBMITTING) {
                assertTrue(runCatching { store.submit(id) { sends++ } }.exceptionOrNull() is IllegalStateException)
                assertEquals(status, store.entries.value.single().status)
            }
            holdWrite.set(true)
            val writer = launch {
                store.begin(destination, AppModel.ComposerDraft("other"), AppComposerPayload("other"))
            }
            try {
                withTimeout(5_000) { writeStarted.await() }
                val submission = launch(start = CoroutineStart.UNDISPATCHED) {
                    store.submit(id) { sends++ }
                }
                submission.cancel()
                releaseWrite.countDown()
                withTimeout(5_000) { submission.join() }
                writer.join()
            } finally {
                releaseWrite.countDown()
            }
            assertEquals(0, sends)
            val expected = if (status == ComposerDraftRecoveryStatus.SUBMITTING) {
                ComposerDraftRecoveryStatus.UNCONFIRMED
            } else status
            assertEquals(expected, store.entries.value.first { it.id == id }.status)
            assertEquals(expected, openStore(backend(directory)).entries.value.first { it.id == id }.status)
            assertNull(store.storageError.value)
        }
    }
}
