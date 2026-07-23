package com.remora.android.voice

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class RealtimeWebRtcSessionAudioRoutingTest {
    @Test
    fun restoresPreviouslySelectedCommunicationDevice() {
        var selectedDevice: String? = null
        var didClear = false

        restoreSelectedCommunicationDevice(
            previousDevice = "earpiece",
            selectDevice = {
                selectedDevice = it
                true
            },
            clearDevice = { didClear = true },
        )

        assertEquals("earpiece", selectedDevice)
        assertFalse(didClear)
    }

    @Test
    fun clearsCommunicationDeviceWhenNoDeviceWasPreviouslySelected() {
        var didSelect = false
        var didClear = false

        restoreSelectedCommunicationDevice<String>(
            previousDevice = null,
            selectDevice = {
                didSelect = true
                true
            },
            clearDevice = { didClear = true },
        )

        assertFalse(didSelect)
        assertTrue(didClear)
    }

    @Test
    fun clearsCommunicationDeviceWhenPreviousDeviceCannotBeRestored() {
        var didClear = false

        restoreSelectedCommunicationDevice(
            previousDevice = "disconnected-headset",
            selectDevice = { false },
            clearDevice = { didClear = true },
        )

        assertTrue(didClear)
    }
}
