package com.remora.android.state

import java.security.KeyPairGenerator
import java.security.interfaces.ECPublicKey
import java.security.spec.ECGenParameterSpec
import javax.crypto.KeyGenerator
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertThrows
import org.junit.Assert.assertTrue
import org.junit.Test

class RemoraLinkCustodyTest {
    @Test
    fun opaqueAliasesAreStableNamespacedHashes() {
        val token = opaqueAliasToken("pairing-slot-17")

        assertEquals(token, opaqueAliasToken("pairing-slot-17"))
        assertFalse(token.contains("pairing-slot-17"))
        assertFalse(token.contains('='))
        assertEquals(43, token.length)
    }

    @Test
    fun p256PublicKeyIsEncodedAsFixedWidthUncompressedSec1() {
        val keyPair = KeyPairGenerator.getInstance("EC").apply {
            initialize(ECGenParameterSpec("secp256r1"))
        }.generateKeyPair()

        val encoded = encodeP256PublicKeySec1(keyPair.public as ECPublicKey)

        assertEquals(65, encoded.size)
        assertEquals(0x04, encoded[0].toInt())
        assertArrayEquals(
            (keyPair.public as ECPublicKey).w.affineX.toFixedCoordinate(),
            encoded.copyOfRange(1, 33),
        )
        assertArrayEquals(
            (keyPair.public as ECPublicKey).w.affineY.toFixedCoordinate(),
            encoded.copyOfRange(33, 65),
        )
    }

    @Test
    fun productionPolicyRejectsSoftwareAndUnknownAssurance() {
        assertNull(classifyAssurance(SecurityObservation.SOFTWARE, false))
        assertNull(classifyAssurance(SecurityObservation.UNKNOWN, false))
        assertEquals(
            RemoraLinkKeyAssurance.DEBUG_EMULATOR_SOFTWARE,
            classifyAssurance(SecurityObservation.SOFTWARE, true),
        )
    }

    @Test
    fun secureHardwareAssuranceIsAccepted() {
        assertEquals(
            RemoraLinkKeyAssurance.STRONGBOX,
            classifyAssurance(SecurityObservation.STRONGBOX, false),
        )
        assertEquals(
            RemoraLinkKeyAssurance.TRUSTED_ENVIRONMENT,
            classifyAssurance(SecurityObservation.TRUSTED_ENVIRONMENT, false),
        )
        assertEquals(
            RemoraLinkKeyAssurance.UNKNOWN_SECURE,
            classifyAssurance(SecurityObservation.UNKNOWN_SECURE, false),
        )
    }

    @Test
    fun secretStorePreservesOpaqueValueWithoutDecoding() {
        val backend = FakeSecretBackend()
        val store = RemoraLinkSecretStore(backend)
        val opaqueSecret = "  eyJub3QiOiJkZWNvZGVkIn0=.v2/+/  "

        assertEquals(
            RemoraLinkSecretMutationStatus.STORED,
            store.replace("host-slot", opaqueSecret),
        )
        assertEquals(
            RemoraLinkSecretReadStatus.Available(opaqueSecret),
            store.load("host-slot"),
        )
        assertTrue(backend.values.keys.none { it.contains("host-slot") })
    }

    @Test
    fun atomicReplacementFailureKeepsPriorDurableValueAcrossStoreReconstruction() {
        val backend = FakeSecretBackend()
        val store = RemoraLinkSecretStore(backend)
        val alias = "replace-failure"
        assertEquals(RemoraLinkSecretMutationStatus.STORED, store.replace(alias, "old"))
        backend.failNextWrite = true

        assertEquals(
            RemoraLinkSecretMutationStatus.STORAGE_FAILURE,
            store.replace(alias, "new"),
        )
        assertEquals(
            RemoraLinkSecretReadStatus.Available("old"),
            RemoraLinkSecretStore(backend).load(alias),
        )
    }

    @Test
    fun atomicTombstoneFailureKeepsPriorDurableValueAcrossStoreReconstruction() {
        val backend = FakeSecretBackend()
        val store = RemoraLinkSecretStore(backend)
        val alias = "delete-failure"
        assertEquals(RemoraLinkSecretMutationStatus.STORED, store.replace(alias, "old"))
        backend.failNextWrite = true

        assertEquals(
            RemoraLinkSecretMutationStatus.STORAGE_FAILURE,
            store.delete(alias),
        )
        assertEquals(
            RemoraLinkSecretReadStatus.Available("old"),
            RemoraLinkSecretStore(backend).load(alias),
        )
    }

    @Test
    fun authenticatedEnvelopeRejectsTamperingAndAliasSwapping() {
        val key = KeyGenerator.getInstance("AES").apply { init(256) }.generateKey()
        val encoded = encryptEnvelope(key, "slot-a", SecretEnvelope.Present("opaque-value"))

        assertEquals(
            SecretEnvelope.Present("opaque-value"),
            decryptEnvelope(key, "slot-a", encoded),
        )
        assertThrows(Exception::class.java) {
            decryptEnvelope(key, "slot-b", encoded)
        }
        val tampered = encoded.copyOf().apply { this[lastIndex] = this[lastIndex].inc() }
        assertThrows(Exception::class.java) {
            decryptEnvelope(key, "slot-a", tampered)
        }
    }

    @Test
    fun authenticatedTombstoneRoundTrips() {
        val key = KeyGenerator.getInstance("AES").apply { init(256) }.generateKey()
        val encoded = encryptEnvelope(key, "slot", SecretEnvelope.Tombstone)

        assertEquals(SecretEnvelope.Tombstone, decryptEnvelope(key, "slot", encoded))
    }

    @Test
    fun secretDeletionIsSynchronousAndIdempotent() {
        val store = RemoraLinkSecretStore(FakeSecretBackend())
        store.replace("slot", "secret")

        assertEquals(RemoraLinkSecretMutationStatus.DELETED, store.delete("slot"))
        assertEquals(RemoraLinkSecretReadStatus.Missing, store.load("slot"))
        assertEquals(RemoraLinkSecretMutationStatus.DELETED, store.delete("slot"))
    }

    @Test
    fun emptyOpaqueAliasesAreRejected() {
        val store = RemoraLinkSecretStore(FakeSecretBackend())

        assertEquals(RemoraLinkSecretReadStatus.InvalidOpaqueAlias, store.load(""))
        assertEquals(
            RemoraLinkSecretMutationStatus.INVALID_OPAQUE_ALIAS,
            store.replace("", "secret"),
        )
        assertEquals(
            RemoraLinkSecretMutationStatus.INVALID_OPAQUE_ALIAS,
            store.delete(""),
        )
    }
}

private class FakeSecretBackend : RemoraLinkSecretBackend {
    val values = mutableMapOf<String, String>()
    var failNextWrite = false

    override fun read(storageKey: String): RemoraLinkSecretReadStatus =
        values[storageKey]?.let(RemoraLinkSecretReadStatus::Available)
            ?: RemoraLinkSecretReadStatus.Missing

    override fun replace(storageKey: String, opaqueSecret: String): SecretWriteResult {
        if (failNextWrite) {
            failNextWrite = false
            return SecretWriteResult.FAILED
        }
        values[storageKey] = opaqueSecret
        return SecretWriteResult.COMMITTED
    }

    override fun delete(storageKey: String): SecretWriteResult {
        if (failNextWrite) {
            failNextWrite = false
            return SecretWriteResult.FAILED
        }
        values.remove(storageKey)
        return SecretWriteResult.COMMITTED
    }
}

private fun java.math.BigInteger.toFixedCoordinate(): ByteArray {
    val raw = toByteArray()
    val unsigned = if (raw.size == 33 && raw[0] == 0.toByte()) raw.copyOfRange(1, 33) else raw
    return ByteArray(32 - unsigned.size) + unsigned
}
