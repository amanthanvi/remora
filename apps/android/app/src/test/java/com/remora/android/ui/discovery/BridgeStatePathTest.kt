package com.remora.android.ui.discovery

import java.io.File
import kotlinx.coroutines.runBlocking
import org.junit.Assert.assertEquals
import org.junit.Assert.assertSame
import org.junit.Test

class BridgeStatePathTest {
    @Test
    fun `persistence failure cleans the new connection before surfacing`() = runBlocking {
        val failure = IllegalStateException("save failed")
        val steps = mutableListOf<String>()

        val surfaced = runCatching {
            persistConnectionAdmissionOrCleanup(
                persistAndAdmit = { _ ->
                    steps += "persist"
                    throw failure
                },
                cleanup = { steps += "cleanup" },
            )
        }.exceptionOrNull()

        assertSame(failure, surfaced)
        assertEquals(listOf("persist", "cleanup"), steps)
    }

    @Test
    fun `cancellation after admission does not clean the durable connection`() = runBlocking {
        val cancellation = kotlinx.coroutines.CancellationException("cancelled after admission")
        val steps = mutableListOf<String>()

        val surfaced = runCatching {
            persistConnectionAdmissionOrCleanup(
                persistAndAdmit = { markAdmissionComplete ->
                    steps += "persist"
                    markAdmissionComplete()
                    steps += "admit"
                    throw cancellation
                },
                cleanup = { steps += "cleanup" },
            )
        }.exceptionOrNull()

        assertSame(cancellation, surfaced)
        assertEquals(listOf("persist", "admit"), steps)
    }

    @Test
    fun `state directory uses neutral root and Rust-compatible host encoding`() {
        val filesDir = File("/data/user/0/com.remora.android/files")

        assertEquals(
            File(filesDir, "remora-bridges/host%2Elocal%3A22"),
            sshBridgeStateDirectory(filesDir, "host.local:22"),
        )
    }

    @Test
    fun `host encoding percent encodes UTF-8 bytes and punctuation`() {
        assertEquals("host%5Fname%2D1", encodeBridgeStateHost("host_name-1"))
        assertEquals("caf%C3%A9", encodeBridgeStateHost("café"))
    }
}
