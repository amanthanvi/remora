package com.remora.android.state

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

class SshSessionStoreTest {
    @Test
    fun failedReplacementRestoresPreviousMappingWithoutOverwritingSuccessor() {
        val store = SshSessionMappings()

        assertNull(store.record("server", "previous"))
        assertEquals("previous", store.record("server", "failed"))
        store.rollbackRecord("server", "failed", "previous")
        assertEquals("previous", store.activeSessionId("server"))

        assertEquals("previous", store.record("server", "failed"))
        assertEquals("failed", store.record("server", "successor"))
        store.rollbackRecord("server", "failed", "previous")
        assertEquals("successor", store.activeSessionId("server"))
    }
}
