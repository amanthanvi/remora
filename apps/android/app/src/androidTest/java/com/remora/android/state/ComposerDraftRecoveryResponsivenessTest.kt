package com.remora.android.state

import android.content.Context
import android.content.ContextWrapper
import android.content.SharedPreferences
import android.os.Looper
import android.util.Log
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import java.io.File
import java.security.KeyStore
import java.util.UUID
import java.util.concurrent.atomic.AtomicInteger
import java.util.concurrent.atomic.AtomicLong
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.cancelAndJoin
import kotlinx.coroutines.delay
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.withContext
import org.junit.Assert.*
import org.junit.Test
import org.junit.runner.RunWith
import uniffi.codex_mobile_client.ThreadKey

@RunWith(AndroidJUnit4::class)
class ComposerDraftRecoveryResponsivenessTest {
    /** Invoke with recoveryPhase=seed, then recoveryPhase=recover and the same recoveryId. */
    @Test
    fun recoversAcrossInstrumentationProcesses() = runBlocking<Unit> {
        val arguments = InstrumentationRegistry.getArguments()
        val phase = arguments.getString("recoveryPhase")
        org.junit.Assume.assumeTrue(phase != null)
        require(phase == "seed" || phase == "recover")
        val suffix = UUID.fromString(arguments.getString("recoveryId")).toString()
        val target = InstrumentationRegistry.getInstrumentation().targetContext
        val name = "recovery-process-$suffix"
        val directory = File(target.noBackupFilesDir, name)
        val alias = "com.remora.android.$name"
        val preferences = target.getSharedPreferences(name, Context.MODE_PRIVATE)
        val isolated = object : ContextWrapper(target) {
            override fun getNoBackupFilesDir(): File = directory
            override fun getSharedPreferences(name: String, mode: Int): SharedPreferences = preferences
        }
        val image = ComposerImageAttachment(byteArrayOf(1, 2, 3, 4), "image/png")
        val draft = AppModel.ComposerDraft("Pending @skill", image,
            listOf(ComposerFileAttachment("notes", "/remote/notes")))
        val destination = ComposerDraftDestination.Home("server", "/project")
        val payload = AppComposerPayload(draft.text, listOf(image.toUserInput()), draft.fileAttachments)
        val store = ComposerDraftRecoveryStore(persistenceFactory = {
            androidComposerDraftRecoveryPersistence(isolated, alias)
        })
        if (phase == "seed") {
            check(directory.mkdirs())
            assertTrue(preferences.edit().putBoolean(CurrentSecurityCutover.MARKER_KEY, true)
                .putInt("seedPid", android.os.Process.myPid()).commit())
            store.begin(destination, draft, payload)
            Log.i("ComposerRecoveryPerf", "processSeedPid=${android.os.Process.myPid()} recoveryId=$suffix")
        } else {
            try {
                assertNotEquals(preferences.getInt("seedPid", 0), android.os.Process.myPid())
                withContext(Dispatchers.Main) { store.load() }
                val recovered = store.entries.value.single()
                assertEquals(ComposerDraftRecoveryStatus.UNCONFIRMED, recovered.status)
                assertEquals(destination, recovered.destination)
                assertEquals(payload, recovered.payload)
                assertArrayEquals(image.data, recovered.draft.attachment!!.data)
                assertEquals(draft.fileAttachments, recovered.draft.fileAttachments)
                var sends = 0
                assertTrue(runCatching { store.submit(recovered.id) { sends++ } }.isFailure)
                assertEquals(0, sends)
                Log.i("ComposerRecoveryPerf", "processRecoveryPid=${android.os.Process.myPid()} recoveryId=$suffix sends=$sends")
            } finally {
                directory.deleteRecursively()
                target.deleteSharedPreferences(name)
                KeyStore.getInstance("AndroidKeyStore").apply { load(null); deleteEntry(alias) }
            }
        }
    }

    @Test
    fun largeAttachmentUsesKeystoreOffMainAndRecoversAfterReopen() = runBlocking<Unit> {
        val target = InstrumentationRegistry.getInstrumentation().targetContext
        val suffix = UUID.randomUUID().toString()
        val directory = File(target.noBackupFilesDir, "recovery-perf-$suffix").apply { mkdirs() }
        val keyAlias = "com.remora.android.recovery-perf.$suffix"
        val preferencesName = "recovery-perf-$suffix"
        val isolated = object : ContextWrapper(target) {
            override fun getNoBackupFilesDir(): File = directory
            override fun getSharedPreferences(name: String, mode: Int): SharedPreferences =
                target.getSharedPreferences(preferencesName, mode)
        }
        try {
            assertTrue(isolated.getSharedPreferences("marker", Context.MODE_PRIVATE).edit()
                .putBoolean(CurrentSecurityCutover.MARKER_KEY, true).commit())
            // Encoded image size representative of a large 12 MP JPEG, without bitmap decoding.
            val image = ComposerImageAttachment(ByteArray(6 * 1024 * 1024) { (it % 251).toByte() }, "image/jpeg")
            val draft = AppModel.ComposerDraft("Large attachment", image)
            val payload = AppComposerPayload(draft.text, listOf(image.toUserInput()))
            val destination = ComposerDraftDestination.Conversation(ThreadKey("server", "thread"))
            val baseline = withContext(Dispatchers.Main) {
                val started = System.nanoTime()
                val bytes = ComposerDraftRecoveryCodec.encode(listOf(RecoverableComposerDraft(
                    1, destination, draft, ComposerDraftRecoveryStatus.SAVED, payload)))
                try {
                    androidComposerDraftRecoveryPersistence(isolated, keyAlias).replace(bytes)
                } finally {
                    bytes.fill(0)
                }
                System.nanoTime() - started
            }
            val calls = AtomicInteger()
            fun backend(): ComposerDraftRecoveryPersistence {
                assertNotEquals(Looper.getMainLooper(), Looper.myLooper())
                val actual = androidComposerDraftRecoveryPersistence(isolated, keyAlias)
                return object : ComposerDraftRecoveryPersistence {
                    override fun read(): ByteArray? {
                        assertNotEquals(Looper.getMainLooper(), Looper.myLooper())
                        calls.incrementAndGet()
                        return actual.read()
                    }
                    override fun replace(plaintext: ByteArray) {
                        assertNotEquals(Looper.getMainLooper(), Looper.myLooper())
                        calls.incrementAndGet()
                        actual.replace(plaintext)
                    }
                }
            }
            val store = ComposerDraftRecoveryStore(persistenceFactory = ::backend)
            val ticks = AtomicInteger()
            val longestGap = AtomicLong()
            val heartbeat = launch(Dispatchers.Main) {
                var previous = System.nanoTime()
                while (isActive) {
                    delay(16)
                    val now = System.nanoTime()
                    longestGap.updateAndGet { maxOf(it, now - previous) }
                    previous = now
                    ticks.incrementAndGet()
                }
            }
            val started = System.nanoTime()
            val id: Long
            try {
                id = withContext(Dispatchers.Main) { store.begin(destination, draft, payload) }
            } finally {
                heartbeat.cancelAndJoin()
            }
            val elapsed = System.nanoTime() - started
            assertEquals(2, calls.get())
            val reopened = ComposerDraftRecoveryStore(persistenceFactory = ::backend)
            withContext(Dispatchers.Main) { reopened.load() }
            val recovered = reopened.entries.value.single()
            assertEquals(id, recovered.id)
            assertEquals(ComposerDraftRecoveryStatus.UNCONFIRMED, recovered.status)
            assertArrayEquals(image.data, recovered.draft.attachment!!.data)
            assertEquals(payload, recovered.payload)
            val journal = File(directory, "${AtomicComposerDraftRecoveryPersistence.DIRECTORY_NAME}/${AtomicComposerDraftRecoveryPersistence.FILE_NAME}")
            Log.i("ComposerRecoveryPerf", "imageBytes=${image.data.size} journalBytes=${journal.length()} " +
                "baselineMainMs=${baseline / 1_000_000} asyncElapsedMs=${elapsed / 1_000_000} " +
                "mainTicks=${ticks.get()} maxMainGapMs=${longestGap.get() / 1_000_000}")
        } finally {
            directory.deleteRecursively()
            target.deleteSharedPreferences(preferencesName)
            KeyStore.getInstance("AndroidKeyStore").apply { load(null); deleteEntry(keyAlias) }
        }
    }
}
