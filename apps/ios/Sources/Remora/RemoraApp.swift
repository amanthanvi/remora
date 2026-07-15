import SwiftUI

@main
struct RemoraApp: App {
    @UIApplicationDelegateAdaptor(AppDelegate.self) private var appDelegate
    @State private var appModel = AppModel.shared
    @State private var voiceRuntime = VoiceRuntimeController.shared
    @State private var appRuntime = AppRuntimeController.shared
    @State private var actionCenter = RemoraActionCenter.shared
    @State private var themeManager = ThemeManager.shared
    @State private var wallpaperManager = WallpaperManager.shared
    @Environment(\.scenePhase) private var scenePhase

    @SceneBuilder
    var body: some Scene {
        #if targetEnvironment(macCatalyst)
        mainWindowGroup
            .defaultSize(width: 1120, height: 760)
            // NOTE: `.windowResizability` is a no-op on Catalyst.
            // Actual resize bounds are set from
            // `MacWindowTitleBarStyler` via
            // `UIWindowScene.sizeRestrictions`.
            .commands {
                RemoraCommands(actionCenter: actionCenter, appModel: appModel)
            }
        #else
        mainWindowGroup
            .commands {
                RemoraCommands(actionCenter: actionCenter, appModel: appModel)
            }
        #endif
    }

    private var mainWindowGroup: some Scene {
        WindowGroup {
            ContentView()
                .environment(appModel)
                .environment(appRuntime)
                .environment(voiceRuntime)
                .environment(themeManager)
                .environment(wallpaperManager)
                .task {
                    appModel.start()
                    voiceRuntime.bind(appModel: appModel)
                    appRuntime.bind(appModel: appModel, voiceRuntime: voiceRuntime)
                    appDelegate.appRuntime = appRuntime
                    appRuntime.appDidBecomeActive()
                }
        }
        .onChange(of: scenePhase) { _, newPhase in
            LLog.info("lifecycle", "scenePhase changed", fields: ["phase": newPhase.debugName])
            switch newPhase {
            case .background:
                appRuntime.appDidEnterBackground()
            case .inactive:
                appRuntime.appDidBecomeInactive()
            case .active:
                appRuntime.appDidBecomeActive()
            default:
                break
            }
        }
    }
}

private extension ScenePhase {
    var debugName: String {
        switch self {
        case .active:
            return "active"
        case .inactive:
            return "inactive"
        case .background:
            return "background"
        @unknown default:
            return "unknown"
        }
    }
}
