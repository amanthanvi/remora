import Foundation

/// One-time greenfield product-state cutover. This intentionally touches only
/// obsolete local UI/cache state; credentials, pairing material, Host trust,
/// and Remora Link identity live outside these paths and remain intact.
enum CurrentWorkspaceRebuild {
    static let completionMarkerKey = "remora.workspaceRebuild.2.completed"
    private static let noticeMarkerKey = "remora.workspaceRebuild.2.noticePending"
    private static let retiredMinigameFeatureKey = "thinking_minigame"

    @discardableResult
    static func apply(
        defaults: UserDefaults = .standard,
        fileManager: FileManager = .default,
        applicationSupportDirectory: URL? = nil,
        documentsDirectory: URL? = nil
    ) -> Bool {
        guard !defaults.bool(forKey: completionMarkerKey) else { return false }

        let applicationSupport = applicationSupportDirectory ?? fileManager.urls(
            for: .applicationSupportDirectory,
            in: .userDomainMask
        ).first
        let documents = documentsDirectory
            ?? fileManager.urls(for: .documentDirectory, in: .userDomainMask).first
        let obsoletePaths = [
            applicationSupport?.appendingPathComponent("RemoraPreferences/mobile_prefs.json"),
            applicationSupport?.appendingPathComponent("RemoraPreferences/mobile_prefs.json.tmp"),
            documents?.appendingPathComponent("Apps", isDirectory: true),
        ].compactMap { $0 }

        do {
            for path in obsoletePaths where fileManager.fileExists(atPath: path.path) {
                try fileManager.removeItem(at: path)
            }
        } catch {
            LLog.error("workspace-rebuild", "obsolete product-state cleanup failed", error: error)
            return false
        }

        if var features = defaults.dictionary(forKey: "remora.experimentalFeatures") as? [String: Bool] {
            features.removeValue(forKey: retiredMinigameFeatureKey)
            defaults.set(features, forKey: "remora.experimentalFeatures")
        }
        defaults.set(true, forKey: completionMarkerKey)
        defaults.set(true, forKey: noticeMarkerKey)
        return true
    }

    static func consumeNotice(defaults: UserDefaults = .standard) -> Bool {
        guard defaults.bool(forKey: noticeMarkerKey) else { return false }
        defaults.removeObject(forKey: noticeMarkerKey)
        return true
    }

    static func markNotice(defaults: UserDefaults = .standard) {
        defaults.set(true, forKey: noticeMarkerKey)
    }
}
