package com.remora.android.ui.terminal

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test
import uniffi.codex_mobile_client.TerminalCellMetrics

class GhosttySurfaceSnapshotTest {
    private val snapshot = GhosttySurfaceSnapshot(
        metrics = TerminalCellMetrics(
            cellWidthPx = 10f,
            cellHeightPx = 20f,
            cols = 80u,
            rows = 3u,
            viewportTop = 0u,
        ),
        rows = listOf("first row", "cached selection", "third row"),
    )

    @Test
    fun readsSingleCachedRowWithoutPlatformQuery() {
        assertEquals("cached", snapshot.text(1, 0, 1, 5))
    }

    @Test
    fun readsAndClampsMultilineCachedRange() {
        assertEquals(
            "row\ncached selection\nthird",
            snapshot.text(0, 6, 2, 4),
        )
    }

    @Test
    fun rejectsRowsOutsideSnapshot() {
        assertNull(snapshot.text(4, 0, 4, 2))
    }

    @Test
    fun physicalRowCapturePreservesSoftWrapAndEmptyRows() {
        val requestedRows = mutableListOf<Int>()
        val physicalRows = listOf("soft-wra", "pped", "", "after\n")
        val metrics = TerminalCellMetrics(
            cellWidthPx = 10f,
            cellHeightPx = 20f,
            cols = 8u,
            rows = 4u,
            viewportTop = 0u,
        )

        val captured = GhosttySurfaceSnapshot.capture(metrics) { row ->
            requestedRows += row
            physicalRows[row]
        }

        assertEquals(listOf(0, 1, 2, 3), requestedRows)
        assertEquals(listOf("soft-wra", "pped", "", "after"), captured.rows)
        assertEquals("soft-wra\npped\n\nafter", captured.text(0, 0, 3, 4))
    }

    @Test
    fun refreshGateCoalescesDuplicateQueriesUntilSurfaceIsDirty() {
        val gate = GhosttySnapshotRefreshGate()

        assertEquals(true, gate.consumeCapture())
        assertEquals(false, gate.consumeCapture())
        gate.markDirty()
        gate.markDirty()
        assertEquals(true, gate.consumeCapture())
        assertEquals(false, gate.consumeCapture())
    }
}
