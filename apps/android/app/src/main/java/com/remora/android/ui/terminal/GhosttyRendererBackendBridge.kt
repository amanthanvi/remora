package com.remora.android.ui.terminal

import android.os.Handler
import android.os.Looper
import com.remora.android.core.bridge.GhosttyRendererBridge
import uniffi.codex_mobile_client.TerminalCellMetrics
import uniffi.codex_mobile_client.TerminalCellRange
import uniffi.codex_mobile_client.TerminalKeyAction
import uniffi.codex_mobile_client.TerminalKeyCode
import uniffi.codex_mobile_client.TerminalKeyEvent
import uniffi.codex_mobile_client.TerminalKeyMods
import uniffi.codex_mobile_client.TerminalRendererBackend
import java.util.concurrent.atomic.AtomicReference
import java.util.concurrent.atomic.AtomicLong

/**
 * Kotlin implementation of the Rust-defined `TerminalRendererBackend` callback
 * interface. The Rust [`uniffi.codex_mobile_client.TerminalRenderer`] tick task
 * invokes these methods from a tokio worker; mutations hop to the main thread.
 * Synchronous queries only read snapshots captured on the view thread, so no
 * callback thread blocks waiting for Android's main looper.
 *
 * Selection state lives here because Ghostty's C surface doesn't expose a
 * public setter for the painted overlay — the platform paints handles
 * itself and uses the stored range to satisfy `readSelection` via
 * `nativeReadText`. The Compose overlay subscribes to
 * [`onSelectionRangeChanged`] to redraw when Rust pushes a new range.
 */
internal class GhosttyRendererBackendBridge(
    private val surface: GhosttyRendererBridge.GhosttyRendererSurface,
    private val onRequestRedraw: () -> Unit,
    /// Closure invoked when the renderer needs to push raw bytes to the
    /// PTY input direction (terminal → shell). Bracketed-paste payloads
    /// flow through here so they reach the running process unmodified.
    private val onPasteBytes: (ByteArray) -> Unit,
    private val onSurfaceMutation: () -> Unit,
) : TerminalRendererBackend {

    private val mainHandler = Handler(Looper.getMainLooper())
    private val selectionRange = AtomicReference<TerminalCellRange?>(null)
    private val selectionText = AtomicReference<String?>(null)
    private val selectionGeneration = AtomicLong(0)
    private val surfaceSnapshot = AtomicReference(GhosttySurfaceSnapshot.EMPTY)

    /// Main-thread callback fired whenever the stored selection range
    /// changes. The terminal surface installs this to drive handle
    /// repaints + ActionMode visibility.
    @Volatile
    var onSelectionRangeChanged: ((TerminalCellRange?) -> Unit)? = null

    override fun setFocus(focused: Boolean) {
        runOnMain { surface.setFocus(focused) }
    }

    override fun setOcclusion(occluded: Boolean) {
        runOnMain { surface.setOcclusion(occluded) }
    }

    override fun requestRedraw() {
        runOnMain { onRequestRedraw() }
    }

    override fun applyConfigFile(path: String) {
        runOnMain {
            surface.applyConfig(path)
            onRequestRedraw()
            onSurfaceMutation()
        }
    }

    override fun dispatchKey(event: TerminalKeyEvent) {
        val action = when (event.action) {
            TerminalKeyAction.RELEASE -> 0
            TerminalKeyAction.PRESS -> 1
            TerminalKeyAction.REPEAT -> 2
        }
        val key = bridgeKey(event.code)
        val mods = packMods(event.mods)
        val text = event.text.ifEmpty { null }
        runOnMain { surface.sendKey(action, key, mods, text, composing = false) }
    }

    override fun dispatchText(text: String, composing: Boolean) {
        runOnMain {
            if (composing) {
                surface.sendPreedit(text.ifEmpty { null })
            } else if (text.isNotEmpty()) {
                surface.sendText(text)
            }
        }
    }

    override fun dispatchPaste(bytes: ByteArray) {
        // Bracketed-paste bytes must travel PTY-input direction (terminal
        // → shell), so we feed them through the same callback Ghostty's
        // `external_pty_write` triggers on key input. `surface.write`
        // would route them PTY-output direction (paint), which is wrong.
        runOnMain { onPasteBytes(bytes) }
    }

    override fun readSelection(): String? {
        return selectionText.get()
    }

    override fun readText(
        startRow: UInt,
        startCol: UInt,
        endRow: UInt,
        endCol: UInt,
    ): String? = surfaceSnapshot.get().text(
        startRow = startRow.toInt(),
        startCol = startCol.toInt(),
        endRow = endRow.toInt(),
        endCol = endCol.toInt(),
    )

    override fun cellMetrics(): TerminalCellMetrics {
        return surfaceSnapshot.get().metrics
    }

    override fun setSelectionOverlay(range: TerminalCellRange?) {
        selectionRange.set(range)
        selectionText.set(null)
        val generation = selectionGeneration.incrementAndGet()
        runOnMain {
            if (selectionGeneration.get() == generation) {
                onSelectionRangeChanged?.invoke(range)
            }
        }
    }

    /// Snapshot the current selection range (for the overlay view / edit
    /// menu without going through Rust).
    fun currentSelectionRange(): TerminalCellRange? = selectionRange.get()

    fun currentSelectionText(): String? = selectionText.get()

    /** Refresh only live metrics; performs no physical-row text reads. */
    fun refreshMetricsSnapshot(): TerminalCellMetrics {
        val size = surface.surfaceSize()
        if (size == null) {
            surfaceSnapshot.set(
                GhosttySurfaceSnapshot(
                    metrics = GhosttySurfaceSnapshot.EMPTY.metrics,
                    rows = surfaceSnapshot.get().rows,
                ),
            )
            return GhosttySurfaceSnapshot.EMPTY.metrics
        }
        val metrics = TerminalCellMetrics(
            cellWidthPx = size.cellWidthPx.toFloat(),
            cellHeightPx = size.cellHeightPx.toFloat(),
            cols = size.columns.toUInt(),
            rows = size.rows.toUInt(),
            viewportTop = 0u,
        )
        surfaceSnapshot.updateAndGet { snapshot -> snapshot.copy(metrics = metrics) }
        return metrics
    }

    /** Capture live physical rows on the view thread for lock-free queries. */
    fun captureSurfaceSnapshot(): GhosttySurfaceSnapshot {
        val metrics = refreshMetricsSnapshot()
        return GhosttySurfaceSnapshot.capture(metrics) { row ->
            if (metrics.cols > 0u) {
                surface.readText(row, 0, row, metrics.cols.toInt() - 1)
            } else {
                ""
            }
        }.also(surfaceSnapshot::set)
    }

    fun invalidateSelectionSnapshot() {
        selectionText.set(null)
    }

    fun refreshSelectionSnapshot() {
        val generation = selectionGeneration.get()
        val range = selectionRange.get()
        val text = if (range == null) {
            null
        } else {
            surface.readText(
                range.start.row.toInt(),
                range.start.col.toInt(),
                range.end.row.toInt(),
                range.end.col.toInt(),
            )
        }
        if (selectionGeneration.get() == generation) {
            selectionText.set(text)
        }
    }

    private fun packMods(mods: TerminalKeyMods): Int {
        var bits = 0
        if (mods.shift) bits = bits or (1 shl 0)
        if (mods.ctrl) bits = bits or (1 shl 1)
        if (mods.alt) bits = bits or (1 shl 2)
        if (mods.meta) bits = bits or (1 shl 3)
        return bits
    }

    // Mirrors `RemoraBridgeKey` in ghostty_jni.cpp; the JNI bridge does the
    // final translation to ghostty_input_key_e.
    private fun bridgeKey(code: TerminalKeyCode): Int = when (code) {
        is TerminalKeyCode.Enter -> 1
        is TerminalKeyCode.Tab -> 2
        is TerminalKeyCode.Backspace -> 3
        is TerminalKeyCode.Escape -> 4
        is TerminalKeyCode.Space -> 5
        is TerminalKeyCode.ArrowUp -> 6
        is TerminalKeyCode.ArrowDown -> 7
        is TerminalKeyCode.ArrowLeft -> 8
        is TerminalKeyCode.ArrowRight -> 9
        is TerminalKeyCode.PageUp -> 10
        is TerminalKeyCode.PageDown -> 11
        is TerminalKeyCode.Home -> 12
        is TerminalKeyCode.End -> 13
        is TerminalKeyCode.Delete -> 14
        is TerminalKeyCode.Insert -> 15
        else -> 0
    }

    private fun runOnMain(block: () -> Unit) {
        if (Looper.myLooper() == Looper.getMainLooper()) {
            block()
        } else {
            mainHandler.post(block)
        }
    }
}

internal data class GhosttySurfaceSnapshot(
    val metrics: TerminalCellMetrics,
    val rows: List<String>,
) {
    fun text(startRow: Int, startCol: Int, endRow: Int, endCol: Int): String? {
        if (startRow < 0 || startRow > endRow || startRow >= rows.size) return null
        val lastRow = endRow.coerceAtMost(rows.lastIndex)
        return (startRow..lastRow).joinToString("\n") { rowIndex ->
            val codePoints = rows[rowIndex].codePoints().toArray()
            if (codePoints.isEmpty()) return@joinToString ""
            val lower = if (rowIndex == startRow) startCol.coerceAtLeast(0) else 0
            val upper = if (rowIndex == lastRow) endCol else codePoints.lastIndex
            if (lower >= codePoints.size || lower > upper) return@joinToString ""
            String(codePoints, lower, upper.coerceAtMost(codePoints.lastIndex) - lower + 1)
        }
    }

    companion object {
        val EMPTY = GhosttySurfaceSnapshot(
            metrics = TerminalCellMetrics(
                cellWidthPx = 0f,
                cellHeightPx = 0f,
                cols = 0u,
                rows = 0u,
                viewportTop = 0u,
            ),
            rows = emptyList(),
        )

        /**
         * Capture exactly one entry per physical Ghostty grid row. A single
         * viewport read unwraps soft-wrapped lines and cannot preserve row
         * coordinates used by hit-testing and selection ranges.
         */
        fun capture(
            metrics: TerminalCellMetrics,
            readRow: (Int) -> String?,
        ): GhosttySurfaceSnapshot = GhosttySurfaceSnapshot(
            metrics = metrics,
            rows = List(metrics.rows.toInt()) { row ->
                readRow(row).orEmpty().trimEnd('\r', '\n')
            },
        )
    }
}
