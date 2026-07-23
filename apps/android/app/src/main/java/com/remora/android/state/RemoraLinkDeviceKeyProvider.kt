package com.remora.android.state

import android.os.Build
import android.security.keystore.KeyGenParameterSpec
import android.security.keystore.KeyInfo
import android.security.keystore.KeyProperties
import android.security.keystore.KeyPermanentlyInvalidatedException
import com.remora.android.BuildConfig
import java.math.BigInteger
import java.security.InvalidAlgorithmParameterException
import java.security.KeyFactory
import java.security.KeyPair
import java.security.KeyPairGenerator
import java.security.KeyStore
import java.security.MessageDigest
import java.security.PrivateKey
import java.security.ProviderException
import java.security.Signature
import java.security.interfaces.ECPublicKey
import java.security.spec.ECGenParameterSpec
import java.util.Base64

enum class RemoraLinkKeyAssurance {
    STRONGBOX,
    TRUSTED_ENVIRONMENT,
    UNKNOWN_SECURE,
    DEBUG_EMULATOR_SOFTWARE,
}

enum class RemoraLinkDeviceKeyFailure {
    INVALID_OPAQUE_SLOT,
    KEY_NOT_FOUND,
    KEY_INVALIDATED,
    SOFTWARE_BACKED_KEY_REJECTED,
    UNSUPPORTED_KEY,
    KEYSTORE_FAILURE,
    OPERATION_UNAVAILABLE,
    INVALID_SIGNATURE,
}

enum class RemoraLinkDeviceKeyDeletionStatus {
    DELETED,
    ALREADY_MISSING,
    INVALID_OPAQUE_SLOT,
    KEYSTORE_FAILURE,
}

sealed interface RemoraLinkDeviceKeyStatus {
    data class Ready(
        /** Uncompressed SEC1 P-256 point: 0x04 || X (32 bytes) || Y (32 bytes). */
        val publicKeySec1: ByteArray,
        val assurance: RemoraLinkKeyAssurance,
    ) : RemoraLinkDeviceKeyStatus

    data class Unavailable(
        val failure: RemoraLinkDeviceKeyFailure,
    ) : RemoraLinkDeviceKeyStatus
}

sealed interface RemoraLinkSignatureStatus {
    /** ASN.1 DER ECDSA signature produced by SHA256withECDSA over the supplied message. */
    data class Signed(val signatureDer: ByteArray) : RemoraLinkSignatureStatus

    data class Unavailable(
        val failure: RemoraLinkDeviceKeyFailure,
    ) : RemoraLinkSignatureStatus
}

/**
 * Owns the device-bound signing identity used by Remora Link v2.
 *
 * The private key never leaves AndroidKeyStore. Callers retain only their opaque slot and the
 * uncompressed public key. Production accepts only secure-hardware-backed keys. Debug emulator
 * software assurance is available only when explicitly requested by a debug build.
 */
class RemoraLinkDeviceKeyProvider(
    allowDebugEmulatorSoftwareAssurance: Boolean = false,
) : RemoraLinkDeviceKeyCustody {
    private val debugEmulatorSoftwareAssuranceAllowed =
        allowDebugEmulatorSoftwareAssurance && BuildConfig.DEBUG && isProbablyEmulator()
    private val lock = Any()

    override fun ensureKey(opaqueSlot: String): RemoraLinkDeviceKeyStatus = synchronized(lock) {
        val alias = keyAlias(opaqueSlot)
            ?: return@synchronized RemoraLinkDeviceKeyStatus.Unavailable(
                RemoraLinkDeviceKeyFailure.INVALID_OPAQUE_SLOT,
            )

        try {
            val keyStore = loadKeyStore()
            val keyPair = if (keyStore.containsAlias(alias)) {
                loadKeyPair(keyStore, alias)
                    ?: return@synchronized RemoraLinkDeviceKeyStatus.Unavailable(
                        RemoraLinkDeviceKeyFailure.KEY_INVALIDATED,
                    )
            } else {
                generatePreferredKey(alias)
            }
            readyStatus(keyPair)
        } catch (_: java.security.UnrecoverableKeyException) {
            RemoraLinkDeviceKeyStatus.Unavailable(RemoraLinkDeviceKeyFailure.KEY_INVALIDATED)
        } catch (_: InvalidAlgorithmParameterException) {
            RemoraLinkDeviceKeyStatus.Unavailable(RemoraLinkDeviceKeyFailure.UNSUPPORTED_KEY)
        } catch (_: ProviderException) {
            RemoraLinkDeviceKeyStatus.Unavailable(RemoraLinkDeviceKeyFailure.KEYSTORE_FAILURE)
        } catch (_: Exception) {
            RemoraLinkDeviceKeyStatus.Unavailable(RemoraLinkDeviceKeyFailure.KEYSTORE_FAILURE)
        }
    }

    /** Loads an existing key without creating or replacing any Keystore entry. */
    override fun loadKey(opaqueSlot: String): RemoraLinkDeviceKeyStatus = synchronized(lock) {
        val alias = keyAlias(opaqueSlot)
            ?: return@synchronized RemoraLinkDeviceKeyStatus.Unavailable(
                RemoraLinkDeviceKeyFailure.INVALID_OPAQUE_SLOT,
            )

        try {
            val keyStore = loadKeyStore()
            if (!keyStore.containsAlias(alias)) {
                return@synchronized RemoraLinkDeviceKeyStatus.Unavailable(
                    RemoraLinkDeviceKeyFailure.KEY_NOT_FOUND,
                )
            }
            val keyPair = loadKeyPair(keyStore, alias)
                ?: return@synchronized RemoraLinkDeviceKeyStatus.Unavailable(
                    RemoraLinkDeviceKeyFailure.KEY_INVALIDATED,
                )
            readyStatus(keyPair)
        } catch (_: java.security.UnrecoverableKeyException) {
            RemoraLinkDeviceKeyStatus.Unavailable(RemoraLinkDeviceKeyFailure.KEY_INVALIDATED)
        } catch (_: KeyPermanentlyInvalidatedException) {
            RemoraLinkDeviceKeyStatus.Unavailable(RemoraLinkDeviceKeyFailure.KEY_INVALIDATED)
        } catch (_: InvalidAlgorithmParameterException) {
            RemoraLinkDeviceKeyStatus.Unavailable(RemoraLinkDeviceKeyFailure.UNSUPPORTED_KEY)
        } catch (_: ProviderException) {
            RemoraLinkDeviceKeyStatus.Unavailable(RemoraLinkDeviceKeyFailure.KEYSTORE_FAILURE)
        } catch (_: Exception) {
            RemoraLinkDeviceKeyStatus.Unavailable(RemoraLinkDeviceKeyFailure.KEYSTORE_FAILURE)
        }
    }

    override fun sign(opaqueSlot: String, canonicalMessage: ByteArray): RemoraLinkSignatureStatus =
        synchronized(lock) {
            val alias = keyAlias(opaqueSlot)
                ?: return@synchronized RemoraLinkSignatureStatus.Unavailable(
                    RemoraLinkDeviceKeyFailure.INVALID_OPAQUE_SLOT,
                )

            try {
                val keyStore = loadKeyStore()
                val keyPair = loadKeyPair(keyStore, alias)
                    ?: return@synchronized RemoraLinkSignatureStatus.Unavailable(
                        RemoraLinkDeviceKeyFailure.KEY_NOT_FOUND,
                    )
                when (val status = readyStatus(keyPair)) {
                    is RemoraLinkDeviceKeyStatus.Unavailable ->
                        RemoraLinkSignatureStatus.Unavailable(status.failure)

                    is RemoraLinkDeviceKeyStatus.Ready -> {
                        val signer = Signature.getInstance(SIGNATURE_ALGORITHM)
                        signer.initSign(keyPair.private)
                        // SHA256withECDSA hashes exactly once. The canonical message must not be
                        // pre-hashed before crossing this boundary.
                        signer.update(canonicalMessage)
                        val signatureDer = signer.sign()
                        if (isCanonicalP256EcdsaDerSignature(signatureDer)) {
                            RemoraLinkSignatureStatus.Signed(signatureDer)
                        } else {
                            signatureDer.fill(0)
                            RemoraLinkSignatureStatus.Unavailable(
                                RemoraLinkDeviceKeyFailure.INVALID_SIGNATURE,
                            )
                        }
                    }
                }
            } catch (_: java.security.UnrecoverableKeyException) {
                RemoraLinkSignatureStatus.Unavailable(RemoraLinkDeviceKeyFailure.KEY_INVALIDATED)
            } catch (_: KeyPermanentlyInvalidatedException) {
                RemoraLinkSignatureStatus.Unavailable(RemoraLinkDeviceKeyFailure.KEY_INVALIDATED)
            } catch (_: ProviderException) {
                RemoraLinkSignatureStatus.Unavailable(
                    RemoraLinkDeviceKeyFailure.OPERATION_UNAVAILABLE,
                )
            } catch (_: Exception) {
                // KeyStore access and Signature engine failures are operational availability
                // failures. Only concrete malformed DER evidence above is InvalidSignature.
                RemoraLinkSignatureStatus.Unavailable(
                    RemoraLinkDeviceKeyFailure.OPERATION_UNAVAILABLE,
                )
            }
        }

    /** Idempotently removes the key. Absence is already the desired state. */
    override fun deleteKey(opaqueSlot: String): RemoraLinkDeviceKeyDeletionStatus = synchronized(lock) {
        val alias = keyAlias(opaqueSlot)
            ?: return@synchronized RemoraLinkDeviceKeyDeletionStatus.INVALID_OPAQUE_SLOT
        try {
            val keyStore = loadKeyStore()
            if (!keyStore.containsAlias(alias)) {
                RemoraLinkDeviceKeyDeletionStatus.ALREADY_MISSING
            } else {
                keyStore.deleteEntry(alias)
                RemoraLinkDeviceKeyDeletionStatus.DELETED
            }
        } catch (_: Exception) {
            RemoraLinkDeviceKeyDeletionStatus.KEYSTORE_FAILURE
        }
    }

    private fun readyStatus(keyPair: KeyPair): RemoraLinkDeviceKeyStatus {
        val publicKey = keyPair.public as? ECPublicKey
            ?: return RemoraLinkDeviceKeyStatus.Unavailable(
                RemoraLinkDeviceKeyFailure.UNSUPPORTED_KEY,
            )
        val assurance = classifyAssurance(
            observeSecurityLevel(keyPair.private),
            debugEmulatorSoftwareAssuranceAllowed,
        ) ?: return RemoraLinkDeviceKeyStatus.Unavailable(
            RemoraLinkDeviceKeyFailure.SOFTWARE_BACKED_KEY_REJECTED,
        )
        val sec1 = runCatching { encodeP256PublicKeySec1(publicKey) }.getOrNull()
            ?: return RemoraLinkDeviceKeyStatus.Unavailable(
                RemoraLinkDeviceKeyFailure.UNSUPPORTED_KEY,
            )
        return RemoraLinkDeviceKeyStatus.Ready(sec1, assurance)
    }

    private fun generatePreferredKey(alias: String): KeyPair {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.P) {
            try {
                return generateKey(alias, requestStrongBox = true)
            } catch (_: InvalidAlgorithmParameterException) {
                deletePartialKey(alias)
            } catch (_: ProviderException) {
                // StrongBoxUnavailableException is a ProviderException. Keeping the catch at
                // this base type also covers vendor providers that report unavailable StrongBox
                // without the platform-specific subtype.
                deletePartialKey(alias)
            }
        }
        return generateKey(alias, requestStrongBox = false)
    }

    private fun generateKey(alias: String, requestStrongBox: Boolean): KeyPair {
        val spec = KeyGenParameterSpec.Builder(alias, KeyProperties.PURPOSE_SIGN)
            .setAlgorithmParameterSpec(ECGenParameterSpec(CURVE_NAME))
            .setDigests(KeyProperties.DIGEST_SHA256)
            .setUserAuthenticationRequired(false)
            .apply {
                if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.P) {
                    setIsStrongBoxBacked(requestStrongBox)
                }
            }
            .build()
        return KeyPairGenerator.getInstance(
            KeyProperties.KEY_ALGORITHM_EC,
            ANDROID_KEYSTORE,
        ).apply { initialize(spec) }.generateKeyPair()
    }

    private fun deletePartialKey(alias: String) {
        runCatching {
            loadKeyStore().apply {
                if (containsAlias(alias)) deleteEntry(alias)
            }
        }
    }

    @Suppress("DEPRECATION")
    private fun observeSecurityLevel(privateKey: PrivateKey): SecurityObservation {
        val keyInfo = KeyFactory.getInstance(privateKey.algorithm, ANDROID_KEYSTORE)
            .getKeySpec(privateKey, KeyInfo::class.java)
        return if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.S) {
            when (keyInfo.securityLevel) {
                KeyProperties.SECURITY_LEVEL_STRONGBOX -> SecurityObservation.STRONGBOX
                KeyProperties.SECURITY_LEVEL_TRUSTED_ENVIRONMENT ->
                    SecurityObservation.TRUSTED_ENVIRONMENT
                KeyProperties.SECURITY_LEVEL_UNKNOWN_SECURE -> SecurityObservation.UNKNOWN_SECURE
                KeyProperties.SECURITY_LEVEL_SOFTWARE -> SecurityObservation.SOFTWARE
                else -> SecurityObservation.UNKNOWN
            }
        } else if (keyInfo.isInsideSecureHardware) {
            SecurityObservation.TRUSTED_ENVIRONMENT
        } else {
            SecurityObservation.SOFTWARE
        }
    }

    private fun loadKeyStore(): KeyStore =
        KeyStore.getInstance(ANDROID_KEYSTORE).apply { load(null) }

    private fun loadKeyPair(keyStore: KeyStore, alias: String): KeyPair? {
        val privateKey = keyStore.getKey(alias, null) as? PrivateKey ?: return null
        val publicKey = keyStore.getCertificate(alias)?.publicKey ?: return null
        return KeyPair(publicKey, privateKey)
    }

    private fun keyAlias(opaqueSlot: String): String? =
        if (opaqueSlot.isEmpty()) null else "$KEY_ALIAS_PREFIX${opaqueAliasToken(opaqueSlot)}"

    companion object {
        private const val ANDROID_KEYSTORE = "AndroidKeyStore"
        private const val CURVE_NAME = "secp256r1"
        private const val SIGNATURE_ALGORITHM = "SHA256withECDSA"
        internal const val KEY_ALIAS_PREFIX = "com.remora.android.remora_link.v2.signing."
    }
}

internal interface RemoraLinkDeviceKeyCustody {
    fun ensureKey(opaqueSlot: String): RemoraLinkDeviceKeyStatus
    fun loadKey(opaqueSlot: String): RemoraLinkDeviceKeyStatus
    fun sign(opaqueSlot: String, canonicalMessage: ByteArray): RemoraLinkSignatureStatus
    fun deleteKey(opaqueSlot: String): RemoraLinkDeviceKeyDeletionStatus
}

internal enum class SecurityObservation {
    STRONGBOX,
    TRUSTED_ENVIRONMENT,
    UNKNOWN_SECURE,
    SOFTWARE,
    UNKNOWN,
}

internal fun classifyAssurance(
    observation: SecurityObservation,
    debugEmulatorSoftwareAssuranceAllowed: Boolean,
): RemoraLinkKeyAssurance? = when (observation) {
    SecurityObservation.STRONGBOX -> RemoraLinkKeyAssurance.STRONGBOX
    SecurityObservation.TRUSTED_ENVIRONMENT -> RemoraLinkKeyAssurance.TRUSTED_ENVIRONMENT
    SecurityObservation.UNKNOWN_SECURE -> RemoraLinkKeyAssurance.UNKNOWN_SECURE
    SecurityObservation.SOFTWARE -> if (debugEmulatorSoftwareAssuranceAllowed) {
        RemoraLinkKeyAssurance.DEBUG_EMULATOR_SOFTWARE
    } else {
        null
    }
    SecurityObservation.UNKNOWN -> null
}

internal fun opaqueAliasToken(opaqueAlias: String): String =
    Base64.getUrlEncoder().withoutPadding().encodeToString(
        MessageDigest.getInstance("SHA-256").digest(opaqueAlias.toByteArray(Charsets.UTF_8)),
    )

internal fun isProbablyEmulator(): Boolean =
    Build.FINGERPRINT.startsWith("generic") ||
        Build.FINGERPRINT.lowercase().contains("emulator") ||
        Build.MODEL.contains("Emulator") ||
        Build.MODEL.contains("Android SDK built for") ||
        Build.MANUFACTURER.contains("Genymotion") ||
        Build.PRODUCT.contains("sdk") ||
        Build.HARDWARE.contains("ranchu") ||
        Build.HARDWARE.contains("goldfish")

internal fun encodeP256PublicKeySec1(publicKey: ECPublicKey): ByteArray {
    require(publicKey.params.curve.field.fieldSize == 256) { "public key must use P-256" }
    val x = publicKey.w.affineX.toUnsignedFixed(32)
    val y = publicKey.w.affineY.toUnsignedFixed(32)
    return byteArrayOf(0x04) + x + y
}

/** Strict canonical DER validation matching the P-256 signature boundary Rust enforces. */
internal fun isCanonicalP256EcdsaDerSignature(signature: ByteArray): Boolean {
    if (signature.size !in MIN_P256_DER_SIGNATURE_BYTES..MAX_P256_DER_SIGNATURE_BYTES) return false
    if (signature[0] != DER_SEQUENCE_TAG || signature[1].toInt() != signature.size - 2) return false
    var cursor = 2

    fun readInteger(): BigInteger? {
        if (cursor + 2 > signature.size || signature[cursor] != DER_INTEGER_TAG) return null
        val length = signature[cursor + 1].toInt() and 0xff
        cursor += 2
        if (length !in 1..MAX_P256_DER_INTEGER_BYTES || cursor + length > signature.size) {
            return null
        }
        val first = signature[cursor].toInt() and 0xff
        if ((first and 0x80) != 0) return null
        if (length > 1 && first == 0 && (signature[cursor + 1].toInt() and 0x80) == 0) {
            return null
        }
        val value = BigInteger(1, signature.copyOfRange(cursor, cursor + length))
        cursor += length
        return value.takeIf { it.signum() > 0 && it < P256_CURVE_ORDER }
    }

    return readInteger() != null && readInteger() != null && cursor == signature.size
}

private fun BigInteger.toUnsignedFixed(size: Int): ByteArray {
    require(signum() >= 0) { "coordinate must be unsigned" }
    val raw = toByteArray()
    val unsigned = if (raw.size > 1 && raw[0] == 0.toByte()) raw.copyOfRange(1, raw.size) else raw
    require(unsigned.size <= size) { "coordinate exceeds P-256 width" }
    return ByteArray(size - unsigned.size) + unsigned
}

private const val MIN_P256_DER_SIGNATURE_BYTES = 8
private const val MAX_P256_DER_SIGNATURE_BYTES = 72
private const val MAX_P256_DER_INTEGER_BYTES = 33
private const val DER_SEQUENCE_TAG: Byte = 0x30
private const val DER_INTEGER_TAG: Byte = 0x02
private val P256_CURVE_ORDER = BigInteger(
    "FFFFFFFF00000000FFFFFFFFFFFFFFFFBCE6FAADA7179E84F3B9CAC2FC632551",
    16,
)
