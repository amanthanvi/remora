package com.remora.android.state

import android.content.Context
import android.security.keystore.KeyGenParameterSpec
import android.security.keystore.KeyProperties
import java.io.File
import java.security.KeyStore
import java.security.MessageDigest
import javax.crypto.Cipher
import javax.crypto.KeyGenerator
import javax.crypto.SecretKey
import javax.crypto.spec.GCMParameterSpec

internal data class RelayAnchorMark(val revision: ULong, val ciphertextDigest: String)

internal interface RelayAnchorMarkers {
    fun latest(): RelayAnchorMark?
    fun commit(mark: RelayAnchorMark)
}

/** Independent Keystore state fences app-file restore, not a compromised OS/Keystore rollback. */
internal class RelayRollbackFence(private val markers: RelayAnchorMarkers) {
    fun verify(snapshot: RemoraLinkJournalSnapshot?) {
        val current = markers.latest()
        if (snapshot == null) {
            check(current == null) { "Relay anchor file is missing" }
            return
        }
        val payload = snapshot.opaquePayload
        val digest = try { MessageDigest.getInstance("SHA-256").digest(payload) } finally { payload.fill(0) }
        val mark = RelayAnchorMark(snapshot.revision, digest.joinToString("") {
            (it.toInt() and 0xff).toString(16).padStart(2, '0')
        })
        digest.fill(0)
        check(current == null || mark.revision >= current.revision) { "Relay anchor rollback" }
        if (current?.revision == mark.revision) {
            check(current == mark) { "Relay anchor digest mismatch" }
        } else {
            // A valid authenticated file can be ahead after interruption between fsync and marker.
            markers.commit(mark)
            check(markers.latest() == mark) { "Relay anchor marker did not commit" }
        }
    }
}

internal fun androidRelaySecretStore(
    context: Context,
    reservedAnchor: Boolean,
    namespace: String = "com.remora.android.relay.v1",
): RelaySecretStore {
    requireRelaySecurityCutover(context)
    val suffix = if (reservedAnchor) "anchor" else "secrets"
    val directory = File(context.noBackupFilesDir, "${namespace}_$suffix")
    val cipher = AndroidRelayCipher(directory, "$namespace.$suffix.aes")
    val fence = if (reservedAnchor) RelayRollbackFence(AndroidRelayAnchorMarkers("$namespace.anchor.mark.")) else null
    return RelaySecretStore(
        journal = RemoraLinkJournalStore(AtomicFileRemoraLinkJournalBackend(context, directory)),
        encrypt = cipher::encrypt,
        decrypt = cipher::decrypt,
        reservedAnchor = reservedAnchor,
        verifyFence = { fence?.verify(it) },
    )
}

internal fun requireRelaySecurityCutover(context: Context) {
    check(context.getSharedPreferences(CurrentSecurityCutover.MARKER_PREFS, Context.MODE_PRIVATE)
        .getBoolean(CurrentSecurityCutover.MARKER_KEY, false)) {
        "Complete the security cutover before opening relay custody"
    }
}

private class AndroidRelayCipher(private val directory: File, private val alias: String) {
    private fun key(create: Boolean): SecretKey {
        val keys = KeyStore.getInstance("AndroidKeyStore").apply { load(null) }
        (keys.getKey(alias, null) as? SecretKey)?.let { return it }
        check(create && !File(directory, AtomicFileRemoraLinkJournalBackend.JOURNAL_FILE_NAME).exists()) {
            "Relay custody key is unavailable"
        }
        return generateRelayKey(alias)
    }

    fun encrypt(plaintext: ByteArray): ByteArray {
        val cipher = Cipher.getInstance("AES/GCM/NoPadding")
        cipher.init(Cipher.ENCRYPT_MODE, key(create = true))
        cipher.updateAAD(alias.toByteArray(Charsets.UTF_8))
        check(cipher.iv.size == 12)
        return byteArrayOf(1) + cipher.iv + cipher.doFinal(plaintext)
    }

    fun decrypt(envelope: ByteArray): ByteArray {
        check(envelope.size >= 29 && envelope[0] == 1.toByte())
        val cipher = Cipher.getInstance("AES/GCM/NoPadding")
        cipher.init(Cipher.DECRYPT_MODE, key(create = false),
            GCMParameterSpec(128, envelope.copyOfRange(1, 13)))
        cipher.updateAAD(alias.toByteArray(Charsets.UTF_8))
        return cipher.doFinal(envelope, 13, envelope.size - 13)
    }
}

private class AndroidRelayAnchorMarkers(private val prefix: String) : RelayAnchorMarkers {
    override fun latest(): RelayAnchorMark? = synchronized(markerLock) {
        readMarkers().values.maxByOrNull { it.revision }
    }

    override fun commit(mark: RelayAnchorMark) = synchronized(markerLock) {
        val existing = readMarkers()
        val previous = existing.values.maxByOrNull { it.revision }
        if (previous == mark) return@synchronized
        check(previous == null || mark.revision > previous.revision)
        val alias = "$prefix${mark.revision}.${mark.ciphertextDigest}"
        generateRelayKey(alias)
        val keys = KeyStore.getInstance("AndroidKeyStore").apply { load(null) }
        check(keys.containsAlias(alias))
        // Never remove the newest marker. Failed old-marker cleanup only leaves redundant fences.
        existing.keys.forEach { old -> runCatching { keys.deleteEntry(old) } }
    }

    private fun readMarkers(): Map<String, RelayAnchorMark> {
        val keys = KeyStore.getInstance("AndroidKeyStore").apply { load(null) }
        val result = linkedMapOf<String, RelayAnchorMark>()
        val aliases = keys.aliases()
        while (aliases.hasMoreElements()) {
            val alias = aliases.nextElement()
            if (!alias.startsWith(prefix)) continue
            check(result.size < 1024)
            val fields = alias.removePrefix(prefix).split('.')
            check(fields.size == 2)
            val revision = fields[0].toULongOrNull()
            check(revision != null && revision > 0uL && revision.toString() == fields[0])
            check(fields[1].length == 64 && fields[1].all { it in '0'..'9' || it in 'a'..'f' })
            val mark = RelayAnchorMark(revision, fields[1])
            check(result.values.none { it.revision == revision && it != mark })
            result[alias] = mark
        }
        return result
    }

    private companion object {
        val markerLock = Any()
    }
}

private fun generateRelayKey(alias: String): SecretKey =
    KeyGenerator.getInstance(KeyProperties.KEY_ALGORITHM_AES, "AndroidKeyStore").run {
        init(KeyGenParameterSpec.Builder(alias, KeyProperties.PURPOSE_ENCRYPT or KeyProperties.PURPOSE_DECRYPT)
            .setKeySize(256)
            .setBlockModes(KeyProperties.BLOCK_MODE_GCM)
            .setEncryptionPaddings(KeyProperties.ENCRYPTION_PADDING_NONE)
            .setRandomizedEncryptionRequired(true)
            .setUserAuthenticationRequired(false)
            .build())
        generateKey().also { check(it.encoded == null) }
    }
