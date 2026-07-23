package com.remora.android.util

import com.remora.android.BuildConfig
import org.junit.After
import org.junit.Assert.assertSame
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test

class LLogDebugDetailTest {
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
    fun debugBuildRetainsDiagnosticFieldsPayloadAndThrowable() {
        assertTrue(BuildConfig.DEBUG)
        val failure = IllegalStateException("debug-exception-detail")

        LLog.i(
            "DebugInfo",
            "request completed",
            fields = mapOf(
                "state" to "debug-oauth-state",
                "sessionId" to "debug-session",
                "threadId" to "debug-thread",
                "cursor" to "debug-cursor",
            ),
            payloadJson = """{"detail":"debug-payload"}""",
        )
        LLog.e("DebugError", "request failed", failure)
        LLog.debug("DebugLazy", failure) { "debug-lazy-detail" }

        val info = entries[0]
        assertTrue(info.message.contains("debug-oauth-state"))
        assertTrue(info.message.contains("debug-session"))
        assertTrue(info.message.contains("debug-thread"))
        assertTrue(info.message.contains("debug-cursor"))
        assertTrue(info.message.contains("debug-payload"))

        val error = entries[1]
        assertTrue(error.message.contains("debug-exception-detail"))
        assertSame(failure, error.throwable)

        val lazy = entries[2]
        assertTrue(lazy.message.contains("debug-lazy-detail"))
        assertSame(failure, lazy.throwable)
    }

    private data class CapturedLog(
        val priority: Int,
        val tag: String,
        val message: String,
        val throwable: Throwable?,
    )
}
