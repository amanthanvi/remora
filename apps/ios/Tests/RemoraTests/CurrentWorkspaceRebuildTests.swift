import Foundation
import XCTest
@testable import Remora

final class CurrentWorkspaceRebuildTests: XCTestCase {
    func testRebuildDeletesOnlyObsoleteProductStateAndEmitsNoticeOnce() throws {
        let suite = "CurrentWorkspaceRebuildTests.\(UUID().uuidString)"
        let defaults = try XCTUnwrap(UserDefaults(suiteName: suite))
        defer { defaults.removePersistentDomain(forName: suite) }

        let root = FileManager.default.temporaryDirectory
            .appendingPathComponent(UUID().uuidString, isDirectory: true)
        defer { try? FileManager.default.removeItem(at: root) }
        let support = root.appendingPathComponent("Support", isDirectory: true)
        let documents = root.appendingPathComponent("Documents", isDirectory: true)
        let preferences = support.appendingPathComponent("RemoraPreferences", isDirectory: true)
        let apps = documents.appendingPathComponent("Apps", isDirectory: true)
        try FileManager.default.createDirectory(at: preferences, withIntermediateDirectories: true)
        try FileManager.default.createDirectory(at: apps, withIntermediateDirectories: true)
        try Data("pins".utf8).write(to: preferences.appendingPathComponent("mobile_prefs.json"))
        let preservedCredential = preferences.appendingPathComponent("slingshot_credentials.json")
        try Data("credential".utf8).write(to: preservedCredential)
        try Data("app".utf8).write(to: apps.appendingPathComponent("saved_apps.json"))
        defaults.set(
            ["thinking_minigame": true, "terminal": true],
            forKey: "remora.experimentalFeatures"
        )

        XCTAssertTrue(CurrentWorkspaceRebuild.apply(
            defaults: defaults,
            applicationSupportDirectory: support,
            documentsDirectory: documents
        ))
        XCTAssertFalse(FileManager.default.fileExists(
            atPath: preferences.appendingPathComponent("mobile_prefs.json").path
        ))
        XCTAssertFalse(FileManager.default.fileExists(atPath: apps.path))
        XCTAssertTrue(FileManager.default.fileExists(atPath: preservedCredential.path))
        let features = defaults.dictionary(forKey: "remora.experimentalFeatures") as? [String: Bool]
        XCTAssertNil(features?["thinking_minigame"])
        XCTAssertEqual(features?["terminal"], true)
        XCTAssertTrue(CurrentWorkspaceRebuild.consumeNotice(defaults: defaults))
        XCTAssertFalse(CurrentWorkspaceRebuild.consumeNotice(defaults: defaults))
        XCTAssertFalse(CurrentWorkspaceRebuild.apply(
            defaults: defaults,
            applicationSupportDirectory: support,
            documentsDirectory: documents
        ))
    }
}
