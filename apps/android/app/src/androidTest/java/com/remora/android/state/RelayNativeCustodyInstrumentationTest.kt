package com.remora.android.state

import android.content.Context
import android.content.ContextWrapper
import android.content.SharedPreferences
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import java.io.File
import java.security.KeyStore
import java.util.UUID
import org.junit.Assert.*
import org.junit.Test
import org.junit.runner.RunWith
import uniffi.codex_mobile_client.AppRelaySecretCasOutcome
import uniffi.codex_mobile_client.AppRelaySecretCreateOutcome
import uniffi.codex_mobile_client.AppRelaySecretReadException
import uniffi.codex_mobile_client.AppRelaySecretRevision
import uniffi.codex_mobile_client.AppRelaySecretWriteOutcome

@RunWith(AndroidJUnit4::class)
class RelayNativeCustodyInstrumentationTest {
    @Test
    fun encryptedNoBackupCustodyAndKeystoreAnchorRejectAppFileRollback() {
        val target = InstrumentationRegistry.getInstrumentation().targetContext
        val namespace = "com.remora.android.relay.test.${UUID.randomUUID()}"
        val directory = File(target.noBackupFilesDir, namespace).apply { mkdirs() }
        val preferences = "$namespace.preflight"
        val isolated = object : ContextWrapper(target) {
            override fun getNoBackupFilesDir(): File = directory
            override fun getSharedPreferences(name: String, mode: Int): SharedPreferences =
                target.getSharedPreferences(preferences, mode)
        }
        val keys = KeyStore.getInstance("AndroidKeyStore").apply { load(null) }
        try {
            assertThrows(IllegalStateException::class.java) {
                androidRelaySecretStore(isolated, false, namespace)
            }
            assertTrue(isolated.getSharedPreferences("marker", Context.MODE_PRIVATE).edit()
                .putBoolean(CurrentSecurityCutover.MARKER_KEY, true).commit())
            val normal = androidRelaySecretStore(isolated, false, namespace)
            val secret = "fixture-relay-secret-not-user-data".toByteArray()
            assertEquals(AppRelaySecretCreateOutcome.CREATED, normal.createIfAbsent("alias", secret))
            assertArrayEquals(secret, androidRelaySecretStore(isolated, false, namespace).read("alias"))
            val normalFile = File(File(directory, "${namespace}_secrets"), AtomicFileRemoraLinkJournalBackend.JOURNAL_FILE_NAME)
            assertTrue(normalFile.canonicalPath.startsWith(target.noBackupFilesDir.canonicalPath + File.separator))
            assertFalse(normalFile.readBytes().toString(Charsets.ISO_8859_1).contains(secret.toString(Charsets.UTF_8)))
            assertEquals(AppRelaySecretCasOutcome.STORED, normal.compareAndTombstone("alias", 1uL, 2uL))
            assertEquals(AppRelaySecretRevision.Found(2uL), androidRelaySecretStore(isolated, false, namespace).revision("alias"))
            assertThrows(AppRelaySecretReadException.Missing::class.java) { normal.read("alias") }

            val anchor = androidRelaySecretStore(isolated, true, namespace)
            assertEquals(AppRelaySecretCreateOutcome.CREATED, anchor.createIfAbsent(RELAY_ROLLBACK_ANCHOR_ALIAS, ByteArray(64) { 1 }))
            val anchorFile = File(File(directory, "${namespace}_anchor"), AtomicFileRemoraLinkJournalBackend.JOURNAL_FILE_NAME)
            val old = anchorFile.readBytes()
            assertEquals(AppRelaySecretCasOutcome.STORED, anchor.compareAndSwap(RELAY_ROLLBACK_ANCHOR_ALIAS, 1uL, 2uL, ByteArray(64) { 2 }))
            val latest = anchorFile.readBytes()
            anchorFile.writeBytes(old)
            assertThrows(AppRelaySecretReadException.Unavailable::class.java) {
                androidRelaySecretStore(isolated, true, namespace).read(RELAY_ROLLBACK_ANCHOR_ALIAS)
            }
            anchorFile.writeBytes(latest)
            assertArrayEquals(ByteArray(64) { 2 }, androidRelaySecretStore(isolated, true, namespace).read(RELAY_ROLLBACK_ANCHOR_ALIAS))
            assertEquals(AppRelaySecretWriteOutcome.UNAVAILABLE, anchor.delete(RELAY_ROLLBACK_ANCHOR_ALIAS))

            keys.deleteEntry("$namespace.secrets.aes")
            val before = normalFile.readBytes()
            assertEquals(AppRelaySecretWriteOutcome.UNAVAILABLE, normal.write("replacement", byteArrayOf(1)))
            assertArrayEquals(before, normalFile.readBytes())
            secret.fill(0)
        } finally {
            keys.aliases().toList().filter { it.startsWith(namespace) }.forEach(keys::deleteEntry)
            directory.deleteRecursively()
            target.deleteSharedPreferences(preferences)
        }
    }
}
