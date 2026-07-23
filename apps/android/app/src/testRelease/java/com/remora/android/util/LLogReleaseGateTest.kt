package com.remora.android.util

import com.remora.android.BuildConfig
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test

class LLogReleaseGateTest {
    private val entries = mutableListOf<CapturedLog>()

    @Before
    fun installCapturingSink() {
        LLog.setSinkForTesting { priority, tag, message, throwable ->
            entries += CapturedLog(priority, tag, message, throwable)
        }
    }

    @After
    fun restoreAndroidSink() {
        LLog.setSinkForTesting(null)
    }

    @Test
    fun traceAndDebugLogsAreNoOpsInReleaseBuilds() {
        assertFalse(BuildConfig.DEBUG)

        LLog.t("LLogReleaseGateTest", "trace must be disabled")
        LLog.d("LLogReleaseGateTest", "debug must be disabled")
        LLog.debug("LLogReleaseGateTest") { "lazy debug must be disabled" }

        assertTrue(entries.isEmpty())
    }

    @Test
    fun releaseInfoSuppressesSensitiveFieldsAndPayloadFromEmittedOutput() {
        LLog.i(
            "ReleaseInfo",
            "pagination completed",
            fields = mapOf(
                "state" to "oauth-state-secret",
                "sessionId" to "session-secret",
                "thread_id" to "thread-secret",
                "cursor" to "cursor-secret",
                "loaded" to 12,
            ),
            payloadJson = """{"threadId":"payload-thread-secret"}""",
        )

        val entry = entries.single()
        assertEquals("ReleaseInfo", entry.tag)
        assertTrue(entry.message.contains("\"loaded\":12"))
        assertFalse(entry.message.contains("oauth-state-secret"))
        assertFalse(entry.message.contains("session-secret"))
        assertFalse(entry.message.contains("thread-secret"))
        assertFalse(entry.message.contains("cursor-secret"))
        assertFalse(entry.message.contains("payload-thread-secret"))
        assertNull(entry.throwable)
    }

    @Test
    fun releaseWarnSuppressesExceptionMessagesButKeepsUsefulMetadata() {
        LLog.w(
            "ReleaseWarn",
            "request failed: interpolated-exception-message-secret",
            fields = mapOf(
                "attempt" to 2,
                "errorType" to "IOException",
                "message" to "Bearer exception-message-secret",
                "error" to "raw-exception-secret",
            ),
        )

        val entry = entries.single()
        assertTrue(entry.message.startsWith("request failed "))
        assertFalse(entry.message.contains("interpolated-exception-message-secret"))
        assertTrue(entry.message.contains("\"attempt\":2"))
        assertTrue(entry.message.contains("\"errorType\":\"IOException\""))
        assertFalse(entry.message.contains("exception-message-secret"))
        assertFalse(entry.message.contains("raw-exception-secret"))
        assertNull(entry.throwable)
    }

    @Test
    fun releaseErrorDoesNotEmitThrowableOrItsMessage() {
        val failure = IllegalStateException("throwable-message-secret")

        LLog.e(
            "ReleaseError",
            "subscription failed",
            throwable = failure,
            fields = mapOf("threadId" to "thread-secret", "operation" to "subscribe"),
        )

        val entry = entries.single()
        assertTrue(entry.message.contains("\"operation\":\"subscribe\""))
        assertTrue(entry.message.contains("\"errorType\":\"IllegalStateException\""))
        assertFalse(entry.message.contains("throwable-message-secret"))
        assertFalse(entry.message.contains("thread-secret"))
        assertNull(entry.throwable)
    }

    @Test
    fun releaseDropsColonDelimitedContext() {
        LLog.i("ReleaseInfo", "pagination: initial page completed")

        assertEquals("pagination", entries.single().message)
    }

    private data class CapturedLog(
        val priority: Int,
        val tag: String,
        val message: String,
        val throwable: Throwable?,
    )
}
