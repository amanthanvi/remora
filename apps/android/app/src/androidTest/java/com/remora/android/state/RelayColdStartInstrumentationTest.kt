package com.remora.android.state

import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.withTimeout
import org.junit.Assert.*
import org.junit.Test
import org.junit.runner.RunWith

/** Run in its own instrumentation invocation, before any Activity-based journey. */
@RunWith(AndroidJUnit4::class)
class RelayColdStartInstrumentationTest {
    @Test fun applicationConfiguresRustAndCustodyWithoutCreatingUiState(): Unit = runBlocking {
        assertThrows(IllegalStateException::class.java) { AppModel.shared }
        val context = InstrumentationRegistry.getInstrumentation().targetContext
        val runtime = AndroidRelayRuntime.get(context)
        withTimeout(30_000) {
            runtime.withRelay { client -> assertTrue(client.backgroundRelayStatus().configured) }
        }
        assertTrue(runtime.available.value)
        assertSame(runtime, AndroidRelayRuntime.get(context))
        assertThrows(IllegalStateException::class.java) { AppModel.shared }
    }
}
