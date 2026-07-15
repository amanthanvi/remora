package com.remora.android.state

import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import java.io.File
import java.security.KeyStore
import java.security.MessageDigest
import java.security.Signature
import java.security.interfaces.ECPublicKey
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertThrows
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith

@RunWith(AndroidJUnit4::class)
class RemoraLinkDeviceKeyProviderTest {
    @Test
    fun keyIsNonExportableAndSignsCanonicalMessageExactlyOnce() {
        val slot = "instrumentation-${System.nanoTime()}"
        val provider = RemoraLinkDeviceKeyProvider(
            allowDebugEmulatorSoftwareAssurance = true,
        )
        try {
            val ready = provider.ensureKey(slot) as RemoraLinkDeviceKeyStatus.Ready
            assertEquals(65, ready.publicKeySec1.size)
            assertEquals(0x04, ready.publicKeySec1[0].toInt())

            val keyStore = KeyStore.getInstance("AndroidKeyStore").apply { load(null) }
            val alias = "com.remora.android.remora_link.v2.signing.${opaqueAliasToken(slot)}"
            val privateKey = keyStore.getKey(alias, null)
            val publicKey = keyStore.getCertificate(alias).publicKey as ECPublicKey
            assertNull(privateKey.encoded)
            assertArrayEquals(ready.publicKeySec1, encodeP256PublicKeySec1(publicKey))

            val canonicalMessage = "remora-link-v2 canonical proof".toByteArray()
            val signed = provider.sign(slot, canonicalMessage) as RemoraLinkSignatureStatus.Signed
            assertTrue(Signature.getInstance("SHA256withECDSA").run {
                initVerify(publicKey)
                update(canonicalMessage)
                verify(signed.signatureDer)
            })
            assertFalse(Signature.getInstance("SHA256withECDSA").run {
                initVerify(publicKey)
                update(MessageDigest.getInstance("SHA-256").digest(canonicalMessage))
                verify(signed.signatureDer)
            })
        } finally {
            assertEquals(RemoraLinkDeviceKeyDeletionStatus.DELETED, provider.deleteKey(slot))
        }
    }

    @Test
    fun deletionIsIdempotentAndMissingKeyDoesNotRegenerateDuringSign() {
        val slot = "instrumentation-delete-${System.nanoTime()}"
        val provider = RemoraLinkDeviceKeyProvider(
            allowDebugEmulatorSoftwareAssurance = true,
        )
        provider.ensureKey(slot) as RemoraLinkDeviceKeyStatus.Ready

        assertEquals(RemoraLinkDeviceKeyDeletionStatus.DELETED, provider.deleteKey(slot))
        assertEquals(RemoraLinkDeviceKeyDeletionStatus.DELETED, provider.deleteKey(slot))
        assertEquals(
            RemoraLinkSignatureStatus.Unavailable(RemoraLinkDeviceKeyFailure.KEY_NOT_FOUND),
            provider.sign(slot, byteArrayOf(1, 2, 3)),
        )
    }

    @Test
    fun encryptedSecretStoreRoundTripsOpaqueValue() {
        val context = InstrumentationRegistry.getInstrumentation().targetContext
        val alias = "instrumentation-secret-${System.nanoTime()}"
        val store = RemoraLinkSecretStore(
            context,
            allowDebugEmulatorSoftwareAssurance = true,
        )
        val opaqueSecret = "not-decoded::+/=::${System.nanoTime()}"
        try {
            assertEquals(
                RemoraLinkSecretMutationStatus.STORED,
                store.replace(alias, opaqueSecret),
            )
            assertEquals(
                RemoraLinkSecretReadStatus.Available(opaqueSecret),
                store.load(alias),
            )
        } finally {
            assertEquals(RemoraLinkSecretMutationStatus.DELETED, store.delete(alias))
        }
    }

    @Test
    fun atomicSecretReplacementFailureSurvivesBackendReconstruction() {
        val context = InstrumentationRegistry.getInstrumentation().targetContext
        val directory = File(context.filesDir, "remora-link-atomic-test-${System.nanoTime()}")
        val alias = "atomic-replace"
        val storageKey = opaqueAliasToken(alias)
        val normalBackend = AndroidAtomicRemoraLinkSecretBackend(
            context = context,
            allowDebugEmulatorSoftwareAssurance = true,
            directory = directory,
        )
        val normalStore = RemoraLinkSecretStore(normalBackend)
        try {
            assertEquals(RemoraLinkSecretMutationStatus.STORED, normalStore.replace(alias, "old"))

            val failingStore = RemoraLinkSecretStore(
                AndroidAtomicRemoraLinkSecretBackend(
                    context = context,
                    allowDebugEmulatorSoftwareAssurance = true,
                    directory = directory,
                    faultInjector = RemoraLinkSecretWriteFaultInjector {
                        throw java.io.IOException("injected before atomic move")
                    },
                ),
            )
            assertEquals(
                RemoraLinkSecretMutationStatus.STORAGE_FAILURE,
                failingStore.replace(alias, "new"),
            )

            // A new backend instance simulates process reconstruction: the old file remains.
            val reconstructed = RemoraLinkSecretStore(
                AndroidAtomicRemoraLinkSecretBackend(
                    context = context,
                    allowDebugEmulatorSoftwareAssurance = true,
                    directory = directory,
                ),
            )
            assertEquals(
                RemoraLinkSecretReadStatus.Available("old"),
                reconstructed.load(alias),
            )
            assertTrue(File(directory, "$storageKey.rls2").isFile)
        } finally {
            normalStore.delete(alias)
            directory.deleteRecursively()
        }
    }

    @Test
    fun atomicSecretDeletionFailureSurvivesBackendReconstruction() {
        val context = InstrumentationRegistry.getInstrumentation().targetContext
        val directory = File(context.filesDir, "remora-link-delete-test-${System.nanoTime()}")
        val alias = "atomic-delete"
        val normalStore = RemoraLinkSecretStore(
            AndroidAtomicRemoraLinkSecretBackend(
                context = context,
                allowDebugEmulatorSoftwareAssurance = true,
                directory = directory,
            ),
        )
        try {
            normalStore.replace(alias, "old")
            val failingStore = RemoraLinkSecretStore(
                AndroidAtomicRemoraLinkSecretBackend(
                    context = context,
                    allowDebugEmulatorSoftwareAssurance = true,
                    directory = directory,
                    faultInjector = RemoraLinkSecretWriteFaultInjector {
                        throw java.io.IOException("injected before tombstone commit")
                    },
                ),
            )
            assertEquals(
                RemoraLinkSecretMutationStatus.STORAGE_FAILURE,
                failingStore.delete(alias),
            )
            val reconstructed = RemoraLinkSecretStore(
                AndroidAtomicRemoraLinkSecretBackend(
                    context = context,
                    allowDebugEmulatorSoftwareAssurance = true,
                    directory = directory,
                ),
            )
            assertEquals(
                RemoraLinkSecretReadStatus.Available("old"),
                reconstructed.load(alias),
            )
        } finally {
            normalStore.delete(alias)
            directory.deleteRecursively()
        }
    }

    @Test
    fun processDeathBeforeAtomicMoveLeavesPriorCredentialForReplaceAndDelete() {
        val context = InstrumentationRegistry.getInstrumentation().targetContext
        val directory = File(context.filesDir, "remora-link-death-test-${System.nanoTime()}")
        val alias = "atomic-process-death"
        fun normalStore() = RemoraLinkSecretStore(
            AndroidAtomicRemoraLinkSecretBackend(
                context = context,
                allowDebugEmulatorSoftwareAssurance = true,
                directory = directory,
            ),
        )
        val original = normalStore()
        try {
            assertEquals(RemoraLinkSecretMutationStatus.STORED, original.replace(alias, "old"))
            fun crashingStore() = RemoraLinkSecretStore(
                AndroidAtomicRemoraLinkSecretBackend(
                    context = context,
                    allowDebugEmulatorSoftwareAssurance = true,
                    directory = directory,
                    faultInjector = RemoraLinkSecretWriteFaultInjector {
                        throw SimulatedProcessDeath()
                    },
                ),
            )

            assertThrows(SimulatedProcessDeath::class.java) {
                crashingStore().replace(alias, "new")
            }
            assertEquals(
                RemoraLinkSecretReadStatus.Available("old"),
                normalStore().load(alias),
            )

            assertThrows(SimulatedProcessDeath::class.java) {
                crashingStore().delete(alias)
            }
            assertEquals(
                RemoraLinkSecretReadStatus.Available("old"),
                normalStore().load(alias),
            )
        } finally {
            original.delete(alias)
            directory.deleteRecursively()
        }
    }

    @Test
    fun directorySyncFailureNeverReportsStoredOrDeleted() {
        val context = InstrumentationRegistry.getInstrumentation().targetContext
        val directory = File(context.filesDir, "remora-link-sync-test-${System.nanoTime()}")
        val alias = "directory-sync-failure"
        fun normalStore() = RemoraLinkSecretStore(
            AndroidAtomicRemoraLinkSecretBackend(
                context = context,
                allowDebugEmulatorSoftwareAssurance = true,
                directory = directory,
            ),
        )
        val normal = normalStore()
        try {
            assertEquals(RemoraLinkSecretMutationStatus.STORED, normal.replace(alias, "old"))
            fun syncFailingStore() = RemoraLinkSecretStore(
                AndroidAtomicRemoraLinkSecretBackend(
                    context = context,
                    allowDebugEmulatorSoftwareAssurance = true,
                    directory = directory,
                    directorySyncOverride = {
                        throw java.io.SyncFailedException("injected directory fsync failure")
                    },
                ),
            )

            assertEquals(
                RemoraLinkSecretMutationStatus.PERSISTENCE_INDETERMINATE,
                syncFailingStore().replace(alias, "new"),
            )
            assertEquals(
                RemoraLinkSecretReadStatus.StorageFailure,
                normalStore().load(alias),
            )
            assertEquals(RemoraLinkSecretMutationStatus.STORED, normal.replace(alias, "new"))
            assertEquals(
                RemoraLinkSecretMutationStatus.PERSISTENCE_INDETERMINATE,
                syncFailingStore().delete(alias),
            )
            assertEquals(RemoraLinkSecretReadStatus.StorageFailure, normalStore().load(alias))
        } finally {
            normal.delete(alias)
            directory.deleteRecursively()
        }
    }
}

private class SimulatedProcessDeath : Error()
