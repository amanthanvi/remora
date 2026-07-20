import SwiftUI
import UIKit

class AppDelegate: NSObject, UIApplicationDelegate {
    private var splashWindow: UIWindow?
    private var minTimeElapsed = false
    private var contentReady = false
    private var splashDismissed = false

    func application(_ application: UIApplication, didFinishLaunchingWithOptions launchOptions: [UIApplication.LaunchOptionsKey: Any]? = nil) -> Bool {
        LLog.bootstrap()

        LLog.info("lifecycle", "application did finish launching")
        LegacyV1SecretPurge.shared.start()
        #if !targetEnvironment(macCatalyst)
        // Register on every launch so APNs can report token rotation. This does
        // not request alert permission; visible notification permission remains
        // an explicit, in-context product decision.
        BackgroundAwarenessController.shared.start {
            application.registerForRemoteNotifications()
        }
        #endif
        // Pre-initialize Rust bridges (tokio runtime) on a background thread
        // before SwiftUI accesses AppModel.shared, avoiding a priority inversion
        // where the main thread blocks on lower-QoS tokio worker init.
        DispatchQueue.global(qos: .userInitiated).async {
            AppModel.prewarmRustBridges()
        }
        DispatchQueue.main.async {
            CloudKVSBridge.shared.start()
        }
        showSplashWindow()
        scheduleKeyboardWarmup()
        return true
    }

    func application(
        _ application: UIApplication,
        didRegisterForRemoteNotificationsWithDeviceToken deviceToken: Data
    ) {
        Task { @MainActor in
            BackgroundAwarenessController.shared.didRegisterForRemoteNotifications(
                deviceToken: deviceToken
            )
        }
    }

    func application(
        _ application: UIApplication,
        didFailToRegisterForRemoteNotificationsWithError error: Error
    ) {
        // Never log the token or installation identifier. The OS error itself
        // is useful for provisioning diagnostics and contains neither.
        LLog.error("background-awareness", "APNs registration failed", error: error)
        Task { @MainActor in
            BackgroundAwarenessController.shared.didFailToRegisterForRemoteNotifications()
        }
    }

    func application(
        _ application: UIApplication,
        didReceiveRemoteNotification userInfo: [AnyHashable: Any],
        fetchCompletionHandler completionHandler: @escaping (UIBackgroundFetchResult) -> Void
    ) {
        Task { @MainActor in
            let result = await BackgroundAwarenessController.shared.handleRemoteNotification(
                userInfo: userInfo
            )
            switch result {
            case .newData:
                completionHandler(.newData)
            case .noData, .unavailable:
                completionHandler(.noData)
            case .timedOut, .failed:
                completionHandler(.failed)
            }
        }
    }

    // MARK: - Splash window (sits above keyboard)

    private func showSplashWindow() {
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.05) {
            guard let scene = UIApplication.shared.connectedScenes.first as? UIWindowScene else {
                self.showSplashWindow()
                return
            }
            let window = UIWindow(windowScene: scene)
            // Keyboard window is typically at level ~10000. Go above it.
            window.windowLevel = UIWindow.Level(rawValue: 10000002)
            let hosting = UIHostingController(rootView:
                AnimatedSplashView(appReady: true) {}
            )
            hosting.view.backgroundColor = .clear
            window.rootViewController = hosting
            window.makeKeyAndVisible()
            self.splashWindow = window

            // Minimum display time
            DispatchQueue.main.asyncAfter(deadline: .now() + 0.8) {
                self.minTimeElapsed = true
                self.tryDismissSplash()
            }
            // Hard max
            DispatchQueue.main.asyncAfter(deadline: .now() + 3.0) {
                self.forceDismissSplash()
            }
        }
    }

    /// Called by ContentView when the main UI has appeared.
    func signalContentReady() {
        contentReady = true
        tryDismissSplash()
    }

    private func tryDismissSplash() {
        guard !splashDismissed, minTimeElapsed, contentReady else { return }
        dismissSplash()
    }

    private func forceDismissSplash() {
        guard !splashDismissed else { return }
        dismissSplash()
    }

    private func dismissSplash() {
        splashDismissed = true
        guard let window = splashWindow else { return }
        UIView.animate(withDuration: 0.35, animations: {
            window.alpha = 0
        }, completion: { _ in
            window.isHidden = true
            window.rootViewController = nil
            self.splashWindow = nil
        })
    }

    // MARK: - Keyboard warmup

    private func scheduleKeyboardWarmup() {
        // Load the real system keyboard while the splash window covers it.
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.3) {
            guard let scene = UIApplication.shared.connectedScenes.first as? UIWindowScene,
                  let window = scene.windows.first(where: { $0 !== self.splashWindow }) else {
                self.scheduleKeyboardWarmup()
                return
            }
            let field = UITextField(frame: CGRect(x: 0, y: 0, width: 200, height: 44))
            field.autocorrectionType = .no
            field.autocapitalizationType = .none
            field.spellCheckingType = .no
            window.addSubview(field)
            field.becomeFirstResponder()
            DispatchQueue.main.asyncAfter(deadline: .now() + 0.3) {
                field.resignFirstResponder()
                field.removeFromSuperview()
            }
        }
    }

}

private extension UIApplication.State {
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
