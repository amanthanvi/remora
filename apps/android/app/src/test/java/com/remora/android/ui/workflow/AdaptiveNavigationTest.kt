package com.remora.android.ui.workflow

import org.junit.Assert.assertEquals
import org.junit.Test

class AdaptiveNavigationTest {
    @Test
    fun compactUsesAvailableHeightForPhoneLandscape() {
        assertEquals(
            AdaptiveNavigationMode.COMPACT,
            adaptiveNavigationMetrics(availableWidthDp = 900, availableHeightDp = 420).mode,
        )
    }

    @Test
    fun widthThresholdsSelectMediumAndExpandedModes() {
        assertEquals(
            AdaptiveNavigationMode.COMPACT,
            adaptiveNavigationMetrics(599, 900).mode,
        )
        assertEquals(
            AdaptiveNavigationMode.MEDIUM,
            adaptiveNavigationMetrics(600, 700).mode,
        )
        assertEquals(
            AdaptiveNavigationMode.MEDIUM,
            adaptiveNavigationMetrics(900, 599).mode,
        )
        assertEquals(
            AdaptiveNavigationMode.EXPANDED,
            adaptiveNavigationMetrics(840, 600).mode,
        )
    }

    @Test
    fun expandedPaneLeavesAConversationReadingFloor() {
        val tablet = adaptiveNavigationMetrics(840, 700)
        val largeTablet = adaptiveNavigationMetrics(1_100, 800)

        assertEquals(320, tablet.leadingPaneWidthDp)
        assertEquals(360, largeTablet.leadingPaneWidthDp)
        assert(tablet.leadingPaneWidthDp + 480 <= 840)
        assert(largeTablet.leadingPaneWidthDp + 480 <= 1_100)
    }

    @Test
    fun expandedPaneWidensAtAccessibilityFontScale() {
        assertEquals(
            AdaptiveNavigationMode.MEDIUM,
            adaptiveNavigationMetrics(
                availableWidthDp = 840,
                availableHeightDp = 700,
                fontScale = 2f,
            ).mode,
        )
        val metrics = adaptiveNavigationMetrics(
            availableWidthDp = 841,
            availableHeightDp = 700,
            fontScale = 2f,
        )

        assertEquals(AdaptiveNavigationMode.EXPANDED, metrics.mode)
        assertEquals(360, metrics.leadingPaneWidthDp)
        assert(metrics.leadingPaneWidthDp + 1 + 480 <= 841)
    }
}
