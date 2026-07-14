package com.remora.android.ui.discovery

import java.io.File
import org.junit.Assert.assertEquals
import org.junit.Test

class BridgeStatePathTest {
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
