import XCTest
@testable import Remora

final class GhosttySurfaceSnapshotTests: XCTestCase {
    private let snapshot = GhosttySurfaceSnapshot(
        metrics: TerminalCellMetrics(
            cellWidthPx: 10,
            cellHeightPx: 20,
            cols: 80,
            rows: 3,
            viewportTop: 0
        ),
        rows: ["first row", "cached selection", "third row"]
    )

    func testReadsSingleCachedRowWithoutPlatformQuery() {
        XCTAssertEqual(
            snapshot.text(startRow: 1, startCol: 0, endRow: 1, endCol: 5),
            "cached"
        )
    }

    func testReadsAndClampsMultilineCachedRange() {
        XCTAssertEqual(
            snapshot.text(startRow: 0, startCol: 6, endRow: 2, endCol: 4),
            "row\ncached selection\nthird"
        )
    }

    func testRejectsRowsOutsideSnapshot() {
        XCTAssertNil(snapshot.text(startRow: 4, startCol: 0, endRow: 4, endCol: 2))
    }

    func testPhysicalRowCapturePreservesSoftWrapAndEmptyRows() {
        var requestedRows: [Int] = []
        let metrics = TerminalCellMetrics(
            cellWidthPx: 10,
            cellHeightPx: 20,
            cols: 8,
            rows: 4,
            viewportTop: 0
        )
        let physicalRows = ["soft-wra", "pped", "", "after\n"]

        let captured = GhosttySurfaceSnapshot.capture(metrics: metrics) { row in
            requestedRows.append(row)
            return physicalRows[row]
        }

        XCTAssertEqual(requestedRows, [0, 1, 2, 3])
        XCTAssertEqual(captured.rows, ["soft-wra", "pped", "", "after"])
        XCTAssertEqual(
            captured.text(startRow: 0, startCol: 0, endRow: 3, endCol: 4),
            "soft-wra\npped\n\nafter"
        )
    }

    func testRefreshGateCoalescesDuplicateQueriesUntilSurfaceIsDirty() {
        var gate = GhosttySnapshotRefreshGate()

        XCTAssertTrue(gate.consumeCapture())
        XCTAssertFalse(gate.consumeCapture())
        gate.markDirty()
        gate.markDirty()
        XCTAssertTrue(gate.consumeCapture())
        XCTAssertFalse(gate.consumeCapture())
    }
}
