package com.remora.android.ui.widget

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class ActiveTurnWidgetProjectionTest {
    @Test
    fun zeroIsInactive() {
        val projection = activeTurnWidgetProjection(0)

        assertFalse(projection.isActive)
        assertEquals("No active turns", projection.countLabel)
        assertEquals("Idle", projection.statusLabel)
    }

    @Test
    fun oneIsActive() {
        val projection = activeTurnWidgetProjection(1)

        assertTrue(projection.isActive)
        assertEquals("1 active turn", projection.countLabel)
        assertEquals("Running", projection.statusLabel)
    }

    @Test
    fun pluralCountIsDisplayed() {
        val projection = activeTurnWidgetProjection(7)

        assertTrue(projection.isActive)
        assertEquals("7 active turns", projection.countLabel)
        assertEquals("Running", projection.statusLabel)
    }

    @Test
    fun countAboveNinetyNineIsBounded() {
        val projection = activeTurnWidgetProjection(100)

        assertTrue(projection.isActive)
        assertEquals("99+ active turns", projection.countLabel)
        assertEquals("Running", projection.statusLabel)
    }

    @Test
    fun negativeCountIsTreatedAsZero() {
        val projection = activeTurnWidgetProjection(-1)

        assertFalse(projection.isActive)
        assertEquals("No active turns", projection.countLabel)
        assertEquals("Idle", projection.statusLabel)
    }
}
