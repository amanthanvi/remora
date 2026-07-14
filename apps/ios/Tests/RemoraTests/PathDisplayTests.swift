import XCTest
@testable import Remora

final class PathDisplayTests: XCTestCase {
    func testRemoteWindowsUserHomeAbbreviatesToTilde() {
        XCTAssertEqual(PathDisplay.display("C:\\Users\\npace", isLocal: false), "~")
    }

    func testRemoteWindowsUserPathAbbreviatesWithBackslashes() {
        XCTAssertEqual(PathDisplay.display("C:\\Users\\npace\\dev\\remora", isLocal: false), "~\\dev\\remora")
    }

    func testRemotePosixHomePathStillAbbreviates() {
        XCTAssertEqual(PathDisplay.display("/Users/npace/dev/remora", isLocal: false), "~/dev/remora")
    }

    func testRemoteWindowsDisplayPathExpandsUsingResolvedHome() {
        XCTAssertEqual(
            PathDisplay.expand("~\\dev\\remora", isLocal: false, remoteHome: "C:\\Users\\npace"),
            "C:\\Users\\npace\\dev\\remora"
        )
    }

    func testRemoteWindowsDisplayPathAcceptsForwardSlashSuffix() {
        XCTAssertEqual(
            PathDisplay.expand("~/dev/remora", isLocal: false, remoteHome: "C:\\Users\\npace"),
            "C:\\Users\\npace\\dev\\remora"
        )
    }

    func testRemotePosixDisplayPathExpandsUsingResolvedHome() {
        XCTAssertEqual(
            PathDisplay.expand("~/dev/remora", isLocal: false, remoteHome: "/home/npace"),
            "/home/npace/dev/remora"
        )
    }
}
