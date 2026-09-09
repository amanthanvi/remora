package com.remora.android.ui.conversation

import android.content.Context
import android.content.ContextWrapper
import android.content.SharedPreferences
import androidx.compose.foundation.layout.Column
import androidx.compose.material3.Text
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onNodeWithContentDescription
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import com.remora.android.state.*
import java.io.File
import java.security.KeyStore
import java.util.UUID
import kotlinx.coroutines.launch
import kotlinx.coroutines.runBlocking
import org.junit.Assert.*
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import uniffi.codex_mobile_client.ThreadKey

@RunWith(AndroidJUnit4::class)
class ComposerDraftRecoveryJourneyTest {
    @get:Rule val composeRule = createComposeRule()

    @Test
    fun encryptedRestartRecoveryRequiresExplicitRestoreAndKeepsNewerEditor() = runBlocking {
        val target = InstrumentationRegistry.getInstrumentation().targetContext
        val suffix = UUID.randomUUID().toString()
        val testDirectory = File(target.noBackupFilesDir, "recovery-test-$suffix").apply { mkdirs() }
        val preferencesName = "recovery-test-$suffix"
        val keyAlias = "com.remora.android.recovery-test.$suffix"
        val isolated = object : ContextWrapper(target) {
            override fun getNoBackupFilesDir(): File = testDirectory
            override fun getSharedPreferences(name: String, mode: Int): SharedPreferences =
                target.getSharedPreferences(preferencesName, mode)
        }
        try {
            // Factory cannot even open draft custody before the security cutover marker.
            assertThrows(IllegalStateException::class.java) {
                androidComposerDraftRecoveryPersistence(isolated, keyAlias)
            }
            assertTrue(isolated.getSharedPreferences("marker", Context.MODE_PRIVATE).edit()
                .putBoolean(CurrentSecurityCutover.MARKER_KEY, true).commit())
            val first = ComposerDraftRecoveryStore(androidComposerDraftRecoveryPersistence(isolated, keyAlias))
            first.load()
            val original = AppModel.ComposerDraft("Original @skill", ComposerImageAttachment(byteArrayOf(1, 2, 3), "image/png"),
                listOf(ComposerFileAttachment("file", "/remote/file")))
            val destination = ComposerDraftDestination.Conversation(ThreadKey("server", "thread"))
            first.begin(destination, original, AppComposerPayload(original.text))
            val restarted = ComposerDraftRecoveryStore(androidComposerDraftRecoveryPersistence(isolated, keyAlias))
            restarted.load()
            val editor = mutableStateOf(AppModel.ComposerDraft("Newer editor"))
            composeRule.setContent {
                val drafts by restarted.entries.collectAsState()
                val scope = rememberCoroutineScope()
                Column {
                    Text(editor.value.text)
                    RecoverableDraftsRow(drafts) { id ->
                        scope.launch { restarted.restore(id, editor.value)?.let { editor.value = it } }
                    }
                }
            }
            composeRule.onNodeWithText("Submission not confirmed").assertExists()
            composeRule.onNodeWithText("Newer editor").assertExists()
            composeRule.onNodeWithContentDescription("Saved drafts").performClick()
            composeRule.onNodeWithText("Original @skill").performClick()
            composeRule.waitUntil { editor.value.text == original.text }
            composeRule.onNodeWithText("Original @skill").assertExists()
            composeRule.runOnIdle {
                assertArrayEquals(original.attachment!!.data, editor.value.attachment!!.data)
                assertEquals(original.fileAttachments, editor.value.fileAttachments)
            }
            val recoveredAgain = ComposerDraftRecoveryStore(androidComposerDraftRecoveryPersistence(isolated, keyAlias))
            recoveredAgain.load()
            assertEquals(listOf("Original @skill", "Newer editor"), recoveredAgain.entries.value.map { it.draft.text })
            assertTrue(recoveredAgain.entries.value.all { it.status == ComposerDraftRecoveryStatus.SAVED })
            val journal = File(testDirectory, "${AtomicComposerDraftRecoveryPersistence.DIRECTORY_NAME}/${AtomicComposerDraftRecoveryPersistence.FILE_NAME}")
            assertTrue(journal.canonicalPath.startsWith(target.noBackupFilesDir.canonicalPath + File.separator))
            assertFalse(journal.readText().contains(original.text))
        } finally {
            testDirectory.deleteRecursively()
            target.deleteSharedPreferences(preferencesName)
            KeyStore.getInstance("AndroidKeyStore").apply { load(null); deleteEntry(keyAlias) }
        }
    }
}
