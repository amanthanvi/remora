package com.remora.android.background

import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

class OpaqueWakeHintDecoderTest {
    private val nowMs = 1_700_000_000_000L

    @Test
    fun `accepts only the settled opaque envelope and closed event classes`() {
        OpaqueWakeEventClass.entries.forEach { eventClass ->
            val result = OpaqueWakeHintDecoder.decode(
                validPayload(eventClass.wireValue),
                nowMs,
            )

            assertTrue(result is OpaqueWakeDecodeResult.Accepted)
            val hint = (result as OpaqueWakeDecodeResult.Accepted).hint
            assertEquals(1, hint.schemaVersion)
            assertEquals(42L, hint.cursor)
            assertEquals(eventClass, hint.eventClass)
        }
    }

    @Test
    fun `rejects content identity and action fields`() {
        listOf(
            "host_id",
            "thread_id",
            "user_id",
            "prompt",
            "content",
            "approval_action",
        ).forEach { forbiddenKey ->
            val result = OpaqueWakeHintDecoder.decode(
                validPayload() + (forbiddenKey to "must-not-cross-push"),
                nowMs,
            )
            assertRejected(result, OpaqueWakeRejection.UNEXPECTED_SCHEMA)
        }
    }

    @Test
    fun `rejects missing unknown expired and excessively durable envelopes`() {
        assertRejected(
            OpaqueWakeHintDecoder.decode(validPayload() - "event_id", nowMs),
            OpaqueWakeRejection.UNEXPECTED_SCHEMA,
        )
        assertRejected(
            OpaqueWakeHintDecoder.decode(
                validPayload().toMutableMap().apply { put("schema_version", "2") },
                nowMs,
            ),
            OpaqueWakeRejection.INVALID_SCHEMA_VERSION,
        )
        assertRejected(
            OpaqueWakeHintDecoder.decode(
                validPayload().toMutableMap().apply { put("expires_at_ms", nowMs.toString()) },
                nowMs,
            ),
            OpaqueWakeRejection.INVALID_EXPIRY,
        )
        assertRejected(
            OpaqueWakeHintDecoder.decode(
                validPayload().toMutableMap().apply {
                    put("expires_at_ms", (nowMs + 24L * 60L * 60L * 1_000L + 1L).toString())
                },
                nowMs,
            ),
            OpaqueWakeRejection.INVALID_EXPIRY,
        )
    }

    @Test
    fun `rejects malformed identifiers cursor class and oversized input`() {
        assertRejected(
            OpaqueWakeHintDecoder.decode(
                validPayload().toMutableMap().apply { put("installation_id", "too-short") },
                nowMs,
            ),
            OpaqueWakeRejection.INVALID_INSTALLATION_ID,
        )
        assertRejected(
            OpaqueWakeHintDecoder.decode(
                validPayload().toMutableMap().apply { put("cursor", "0") },
                nowMs,
            ),
            OpaqueWakeRejection.INVALID_CURSOR,
        )
        assertRejected(
            OpaqueWakeHintDecoder.decode(
                validPayload().toMutableMap().apply { put("event_class", "approval_pending") },
                nowMs,
            ),
            OpaqueWakeRejection.INVALID_EVENT_CLASS,
        )
        assertRejected(
            OpaqueWakeHintDecoder.decode(
                validPayload().toMutableMap().apply { put("event_id", "a".repeat(4_096)) },
                nowMs,
            ),
            OpaqueWakeRejection.OVERSIZED,
        )
    }

    private fun validPayload(
        eventClass: String = OpaqueWakeEventClass.STATE_CHANGED.wireValue,
    ): Map<String, String> = mapOf(
        "schema_version" to "1",
        "installation_id" to "0123456789abcdef0123456789abcdef",
        "event_id" to "event_0123456789abcdef",
        "cursor" to "42",
        "event_class" to eventClass,
        "expires_at_ms" to (nowMs + 60_000L).toString(),
    )

    private fun assertRejected(
        result: OpaqueWakeDecodeResult,
        expected: OpaqueWakeRejection,
    ) {
        assertTrue(result is OpaqueWakeDecodeResult.Rejected)
        assertEquals(expected, (result as OpaqueWakeDecodeResult.Rejected).reason)
    }
}
