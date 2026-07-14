import SwiftUI
import UIKit
import XCTest
@testable import Remora

final class RemoraAppearanceModeTests: XCTestCase {
    func testPreferredColorSchemeMapping() {
        XCTAssertNil(RemoraAppearanceMode.system.preferredColorScheme)
        XCTAssertEqual(RemoraAppearanceMode.light.preferredColorScheme, .light)
        XCTAssertEqual(RemoraAppearanceMode.dark.preferredColorScheme, .dark)
    }

    func testResolvedColorSchemeUsesSystemOnlyForSystemMode() {
        XCTAssertEqual(RemoraAppearanceMode.system.resolvedColorScheme(systemColorScheme: .light), .light)
        XCTAssertEqual(RemoraAppearanceMode.system.resolvedColorScheme(systemColorScheme: .dark), .dark)
        XCTAssertEqual(RemoraAppearanceMode.light.resolvedColorScheme(systemColorScheme: .dark), .light)
        XCTAssertEqual(RemoraAppearanceMode.dark.resolvedColorScheme(systemColorScheme: .light), .dark)
    }

    func testUserInterfaceStyleMapping() {
        XCTAssertEqual(RemoraAppearanceMode.system.userInterfaceStyle, .unspecified)
        XCTAssertEqual(RemoraAppearanceMode.light.userInterfaceStyle, .light)
        XCTAssertEqual(RemoraAppearanceMode.dark.userInterfaceStyle, .dark)
    }
}
