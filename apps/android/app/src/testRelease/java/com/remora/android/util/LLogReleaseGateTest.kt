package com.remora.android.util

import com.remora.android.BuildConfig
import org.junit.Assert.assertFalse
import org.junit.Test

class LLogReleaseGateTest {
    @Test
    fun traceAndDebugLogsAreNoOpsInReleaseBuilds() {
        assertFalse(BuildConfig.DEBUG)

        LLog.t("LLogReleaseGateTest", "trace must be disabled")
        LLog.d("LLogReleaseGateTest", "debug must be disabled")
    }
}
