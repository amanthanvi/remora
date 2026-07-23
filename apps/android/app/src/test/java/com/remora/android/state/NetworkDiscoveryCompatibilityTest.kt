package com.remora.android.state

import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class NetworkDiscoveryCompatibilityTest {
    @Test
    fun modernNsdApisStartAtAndroid14() {
        assertFalse(NetworkDiscovery.usesModernNsdApis(33))
        assertTrue(NetworkDiscovery.usesModernNsdApis(34))
        assertTrue(NetworkDiscovery.usesModernNsdApis(35))
    }
}
