package com.remora.android

import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import com.remora.android.core.bridge.UniffiInit
import org.junit.Assert.assertEquals
import org.junit.Test
import org.junit.runner.RunWith

@RunWith(AndroidJUnit4::class)
class NativeContextInitTest {
    @Test
    fun irohSeesInitializedAndroidContext() {
        val context = InstrumentationRegistry.getInstrumentation().targetContext
        val result = UniffiInit.debugNativeContextProbe(context)
        assertEquals("ok", result)
    }
}
