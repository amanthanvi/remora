import Foundation

/// Thin Swift implementation of the Rust-defined `TerminalRendererBackend`
/// callback interface. Holds a weak reference to the platform-side
/// `RemoraGhosttyTerminal` and hops Ghostty mutations onto the main thread
/// (Ghostty's surface APIs are not thread-safe). Synchronous queries only
/// read snapshots previously captured on main, so Rust callback threads never
/// block waiting for UIKit.
///
/// Selection state lives here because Ghostty's C surface doesn't expose a
/// public setter for the painted selection range — the platform paints the
/// overlay itself and uses the stored range to satisfy `readSelection` via
/// `ghostty_surface_read_text`. The UI overlay view subscribes to
/// `onSelectionRangeChanged` to redraw handles when Rust pushes a new range.
final class GhosttyRendererBackendBridge: TerminalRendererBackend, @unchecked Sendable {
    private weak var terminal: RemoraGhosttyTerminal?

    /// Most recently pushed selection range (viewport-relative). `nil` when
    /// no selection is active. Written from the Rust runtime via
    /// `setSelectionOverlay` and read from the main thread by the overlay
    /// view + edit menu. Guarded by `snapshotLock` with the other immutable
    /// surface snapshots consumed by Rust callback threads.
    private let snapshotLock = NSLock()
    private var selectionRange: TerminalCellRange?
    private var selectionText: String?
    private var selectionGeneration: UInt64 = 0
    private var surfaceSnapshot = GhosttySurfaceSnapshot.empty

    /// Callback fired on the main thread whenever the stored selection
    /// range changes. The terminal view installs this to drive handle
    /// repaints + edit-menu visibility.
    var onSelectionRangeChanged: ((TerminalCellRange?) -> Void)?

    init(terminal: RemoraGhosttyTerminal) {
        self.terminal = terminal
    }

    func setFocus(focused: Bool) {
        let terminal = self.terminal
        DispatchQueue.main.async {
            terminal?.setFocused(focused)
        }
    }

    func setOcclusion(occluded: Bool) {
        let terminal = self.terminal
        DispatchQueue.main.async {
            terminal?.setOcclusion(occluded)
        }
    }

    func requestRedraw() {
        // UIKit Ghostty surfaces render through Ghostty's own renderer thread.
        // Ghostty's wakeup callback drains the app mailbox; this Rust-side
        // renderer callback only exists for Android's app-thread EGL path.
    }

    func applyConfigFile(path: String) {
        let terminal = self.terminal
        if Thread.isMainThread {
            try? terminal?.applyConfig(atPath: path)
        } else {
            DispatchQueue.main.async {
                try? terminal?.applyConfig(atPath: path)
            }
        }
    }

    func dispatchKey(event: TerminalKeyEvent) {
        let terminal = self.terminal
        let action = Int32(GhosttyKeyTranslator.action(for: event.action))
        let remoraKey = GhosttyKeyTranslator.remoraKey(for: event.code)
        let mods = Int32(GhosttyKeyTranslator.mods(for: event.mods))
        let text = event.text.isEmpty ? nil : event.text
        DispatchQueue.main.async {
            _ = terminal?.dispatchKeyAction(
                action,
                key: remoraKey,
                mods: mods,
                text: text,
                composing: false
            )
        }
    }

    func dispatchText(text: String, composing: Bool) {
        let terminal = self.terminal
        DispatchQueue.main.async {
            if composing {
                terminal?.setPreeditText(text.isEmpty ? nil : text)
            } else {
                terminal?.sendText(text)
            }
        }
    }

    func dispatchPaste(bytes: Data) {
        // Bracketed-paste bytes must travel PTY-input direction
        // (terminal → shell), not PTY-output direction. The terminal's
        // `inputHandler` is the same closure Ghostty's
        // `external_pty_write` ultimately fires when the user types, so
        // we reuse it: the platform-side controller forwards the bytes
        // to the running process unchanged. Writing them through
        // `writeOutput` would paint the wrapper on screen instead.
        let terminal = self.terminal
        DispatchQueue.main.async {
            terminal?.inputHandler?(bytes)
        }
    }

    func readSelection() -> String? {
        snapshotLock.withLock { selectionText }
    }

    func readText(startRow: UInt32, startCol: UInt32, endRow: UInt32, endCol: UInt32) -> String? {
        snapshotLock.withLock {
            surfaceSnapshot.text(
                startRow: startRow,
                startCol: startCol,
                endRow: endRow,
                endCol: endCol
            )
        }
    }

    func cellMetrics() -> TerminalCellMetrics {
        snapshotLock.withLock { surfaceSnapshot.metrics }
    }

    func setSelectionOverlay(range: TerminalCellRange?) {
        let generation = snapshotLock.withLock {
            selectionRange = range
            selectionText = nil
            selectionGeneration &+= 1
            return selectionGeneration
        }
        Task { @MainActor [weak self] in
            guard let self, self.snapshotLock.withLock({ self.selectionGeneration == generation }) else {
                return
            }
            self.onSelectionRangeChanged?(range)
        }
    }

    /// Snapshot the current selection range. Used by `readSelection` and
    /// by the overlay view via `currentRange` to repaint.
    func currentSelectionRange() -> TerminalCellRange? {
        snapshotLock.withLock { selectionRange }
    }

    func currentSelectionText() -> String? {
        snapshotLock.withLock { selectionText }
    }

    /// Refresh only live grid metrics. This is safe for layout/resize paths:
    /// it performs no per-row Ghostty text reads.
    @MainActor
    func refreshMetricsSnapshot() -> TerminalCellMetrics {
        guard let terminal else { return snapshotLock.withLock { surfaceSnapshot.metrics } }
        let native = terminal.surfaceMetrics()
        let metrics = TerminalCellMetrics(
            cellWidthPx: Float(native.cellWidthPx),
            cellHeightPx: Float(native.cellHeightPx),
            cols: UInt32(native.columns),
            rows: UInt32(native.rows),
            viewportTop: 0
        )
        snapshotLock.withLock {
            surfaceSnapshot = GhosttySurfaceSnapshot(metrics: metrics, rows: surfaceSnapshot.rows)
        }
        return metrics
    }

    /// Capture the live physical Ghostty rows while already on main. Rust and
    /// its callback threads only consume this immutable copy.
    @MainActor
    func captureSurfaceSnapshot() -> GhosttySurfaceSnapshot {
        guard let terminal else { return snapshotLock.withLock { surfaceSnapshot } }
        let metrics = refreshMetricsSnapshot()
        let snapshot = GhosttySurfaceSnapshot.capture(metrics: metrics) { row in
            guard metrics.cols > 0 else { return "" }
            return terminal.readText(
                fromRow: UInt32(row),
                column: 0,
                toRow: UInt32(row),
                column: metrics.cols - 1
            )
        }
        snapshotLock.withLock { surfaceSnapshot = snapshot }
        return snapshot
    }

    @MainActor
    func refreshSelectionSnapshotOnMain() {
        let (range, generation) = snapshotLock.withLock { (selectionRange, selectionGeneration) }
        let text: String?
        if let range, let terminal {
            text = terminal.readText(
                fromRow: range.start.row,
                column: range.start.col,
                toRow: range.end.row,
                column: range.end.col
            )
        } else {
            text = nil
        }
        snapshotLock.withLock {
            guard selectionGeneration == generation else { return }
            selectionText = text
        }
    }

    func invalidateSelectionSnapshot() {
        snapshotLock.withLock { selectionText = nil }
    }
}

struct GhosttySurfaceSnapshot {
    let metrics: TerminalCellMetrics
    let rows: [String]

    static let empty = GhosttySurfaceSnapshot(
        metrics: TerminalCellMetrics(
            cellWidthPx: 0,
            cellHeightPx: 0,
            cols: 0,
            rows: 0,
            viewportTop: 0
        ),
        rows: []
    )

    /// Capture one string for every physical Ghostty grid row. Reading the
    /// whole viewport at once is incorrect because Ghostty unwraps soft-wrapped
    /// lines in selection text, which shifts all following row coordinates.
    static func capture(
        metrics: TerminalCellMetrics,
        readRow: (Int) -> String?
    ) -> GhosttySurfaceSnapshot {
        let rows = (0..<Int(metrics.rows)).map { row in
            var text = readRow(row) ?? ""
            while text.last == "\n" || text.last == "\r" {
                text.removeLast()
            }
            return text
        }
        return GhosttySurfaceSnapshot(metrics: metrics, rows: rows)
    }

    func text(startRow: UInt32, startCol: UInt32, endRow: UInt32, endCol: UInt32) -> String? {
        guard startRow <= endRow, Int(startRow) < rows.count else { return nil }
        let lastRow = min(Int(endRow), rows.count - 1)
        return (Int(startRow)...lastRow).map { rowIndex in
            let characters = Array(rows[rowIndex])
            guard !characters.isEmpty else { return "" }
            let lower = rowIndex == Int(startRow) ? Int(startCol) : 0
            let upper = rowIndex == lastRow ? Int(endCol) : characters.count - 1
            guard lower < characters.count, lower <= upper else { return "" }
            return String(characters[lower...min(upper, characters.count - 1)])
        }.joined(separator: "\n")
    }
}

/// Coalesces repeated layout/gesture/debounce requests into one physical-row
/// capture for each dirty surface generation. Main-actor owned by the view.
struct GhosttySnapshotRefreshGate {
    private(set) var isDirty = true

    mutating func markDirty() {
        isDirty = true
    }

    mutating func consumeCapture() -> Bool {
        guard isDirty else { return false }
        isDirty = false
        return true
    }
}

private extension NSLock {
    func withLock<T>(_ body: () throws -> T) rethrows -> T {
        lock()
        defer { unlock() }
        return try body()
    }
}

/// Translation: Rust `TerminalKey*` → bridge-level `RemoraGhosttyKey`.
/// Bridge does the final Ghostty-enum mapping in Obj-C.
enum GhosttyKeyTranslator {
    static func action(for value: TerminalKeyAction) -> Int {
        switch value {
        case .release: return 0
        case .press: return 1
        case .repeat: return 2
        }
    }

    static func mods(for value: TerminalKeyMods) -> Int {
        var bits = 0
        if value.shift { bits |= 1 << 0 }
        if value.ctrl { bits |= 1 << 1 }
        if value.alt { bits |= 1 << 2 }
        if value.meta { bits |= 1 << 3 }
        return bits
    }

    static func remoraKey(for value: TerminalKeyCode) -> RemoraGhosttyKey {
        switch value {
        case .enter: return .enter
        case .tab: return .tab
        case .backspace: return .backspace
        case .escape: return .escape
        case .space: return .space
        case .arrowUp: return .arrowUp
        case .arrowDown: return .arrowDown
        case .arrowLeft: return .arrowLeft
        case .arrowRight: return .arrowRight
        case .pageUp: return .pageUp
        case .pageDown: return .pageDown
        case .home: return .home
        case .end: return .end
        case .delete: return .delete
        case .insert: return .insert
        default: return .unidentified
        }
    }
}
