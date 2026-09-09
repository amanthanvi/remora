package com.remora.android.background

import com.remora.android.state.RelaySecretStore
import java.nio.ByteBuffer
import uniffi.codex_mobile_client.AppRelaySecretReadException
import uniffi.codex_mobile_client.AppRelaySecretRevision
import uniffi.codex_mobile_client.AppRelaySecretWriteOutcome

/** OS input only. Rust owns every host binding, registration receipt and retry. */
internal class PushTokenInputStore(private val secrets: RelaySecretStore) {
    class Input(val generation: ULong, val token: ByteArray?) : AutoCloseable {
        override fun close() { token?.fill(0) }
        override fun toString() = "PushTokenInput(generation=$generation, token=[redacted])"
    }

    fun registered(token: ByteArray) = synchronized(lock) {
        require(token.size in 16..4095 && token.all { it.toInt() in 33..126 })
        val encoded = byteArrayOf(ACTIVE) + token
        try {
            check(secrets.write(ALIAS, encoded) == AppRelaySecretWriteOutcome.APPLIED)
        } finally { encoded.fill(0) }
    }

    fun unregistered(token: ByteArray) = synchronized(lock) {
        current()?.use { current ->
            if (current.token?.contentEquals(token) == true) {
                val encoded = ByteBuffer.allocate(9).put(TOMBSTONE)
                    .putLong(current.generation.toLong()).array()
                try {
                    check(secrets.write(ALIAS, encoded) == AppRelaySecretWriteOutcome.APPLIED)
                } finally { encoded.fill(0) }
            }
        }
    }

    fun current(): Input? = synchronized(lock) {
        val encoded = try { secrets.read(ALIAS) } catch (_: AppRelaySecretReadException.Missing) {
            return@synchronized null
        }
        try {
            val revision = (secrets.revision(ALIAS) as? AppRelaySecretRevision.Found)?.revision
                ?: error("Provider input revision unavailable")
            when (encoded.firstOrNull()) {
                ACTIVE -> {
                    check(encoded.size in 17..4096)
                    Input(revision, encoded.copyOfRange(1, encoded.size))
                }
                TOMBSTONE -> {
                    check(encoded.size == 9)
                    val through = ByteBuffer.wrap(encoded, 1, 8).long.toULong()
                    check(through > 0uL && through < revision)
                    Input(through, null)
                }
                else -> error("Provider input unavailable")
            }
        } finally { encoded.fill(0) }
    }

    private companion object {
        val lock = Any()
        const val ALIAS = "fcm_provider_input_v1"
        const val ACTIVE: Byte = 1
        const val TOMBSTONE: Byte = 0
    }
}
