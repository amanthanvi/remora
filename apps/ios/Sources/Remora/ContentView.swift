import Combine
import SwiftUI
import UIKit

struct ContentView: View {
    @Environment(AppModel.self) private var appModel
    @Environment(AppRuntimeController.self) private var appRuntime
    @Environment(ThemeManager.self) private var themeManager
    @State private var appState = AppState()
    @State private var stableSafeAreaInsets = StableSafeAreaInsets()
    @State private var conversationWarmup = ConversationWarmupCoordinator()
    @State private var petOverlay = PetOverlayController.shared
    @State private var composerBottomInset: CGFloat = 0
    @State private var splashDismissed = false
    @Environment(\.colorScheme) private var colorScheme
    @Environment(\.scenePhase) private var scenePhase
    @AppStorage("conversationTextSizeStep") private var textSizeStep = ConversationTextSize.large.rawValue

    private var textScale: CGFloat {
        ConversationTextSize.clamped(rawValue: textSizeStep).scale
    }

    var body: some View {
        @Bindable var bindableAppState = appState

        GeometryReader { geometry in
            ZStack {
                RemoraTheme.backgroundGradient.ignoresSafeArea()

                #if DEBUG
                if ConversationDisplayUITestHarnessView.isEnabled {
                    ConversationDisplayUITestHarnessView()
                        .frame(maxWidth: .infinity, maxHeight: .infinity)
                } else {
                    standardHomeNavigationView(
                        topInset: geometry.safeAreaInsets.top,
                        bottomInset: composerBottomInset
                    )
                }
                #else
                standardHomeNavigationView(
                    topInset: geometry.safeAreaInsets.top,
                    bottomInset: composerBottomInset
                )
                #endif

                #if DEBUG
                if !ConversationDisplayUITestHarnessView.isEnabled {
                    standardOverlays
                }
                #else
                standardOverlays
                #endif

            }
            .ignoresSafeArea(.container)
            .task {
                if composerBottomInset <= 0, geometry.safeAreaInsets.bottom > 0 {
                    composerBottomInset = geometry.safeAreaInsets.bottom
                }
                stableSafeAreaInsets.start(
                    fallback: max(composerBottomInset, geometry.safeAreaInsets.bottom)
                )
            }
            .onChange(of: stableSafeAreaInsets.bottomInset) { (_: CGFloat, nextInset: CGFloat) in
                guard nextInset > 0 else { return }
                composerBottomInset = nextInset
            }
        }
        .environment(appState)
        .environment(conversationWarmup)
        .environment(\.textScale, textScale)
        .preferredColorScheme(themeManager.appearanceMode.preferredColorScheme)
        .background {
            InterfaceStyleSynchronizer(style: themeManager.appearanceMode.userInterfaceStyle)
                .frame(width: 0, height: 0)
        }
        #if targetEnvironment(macCatalyst)
        .background {
            MacWindowTitleBarStyler()
        }
        #endif
        .onAppear {
            themeManager.syncSystemColorScheme(colorScheme)
            let forceDiscoveryForUITest =
                ProcessInfo.processInfo.environment["CODEXIOS_UI_TEST_FORCE_DISCOVERY"] == "1"
            if forceDiscoveryForUITest {
                appState.showServerPicker = true
            }
        }
        .onChange(of: colorScheme) { _, nextColorScheme in
            // iOS toggles `colorScheme` while capturing light+dark
            // app-switcher snapshots on background. Reacting to that
            // bumps `themeManager.themeVersion`, which the navigation
            // root uses as `.id(...)` and would tear down every
            // in-flight @State (composer text, focus, scroll) every
            // time the user switches apps. Only react when the scene
            // is actually active — i.e., a real user theme toggle.
            guard scenePhase == .active else { return }
            themeManager.syncSystemColorScheme(nextColorScheme)
        }
        .onChange(of: scenePhase) { _, newPhase in
            // Catch up to any colorScheme change that landed while we
            // were inactive but represents a real user-driven theme
            // toggle (e.g. system appearance changed in Settings while
            // the app was backgrounded).
            if newPhase == .active {
                themeManager.syncSystemColorScheme(colorScheme)
            }
        }
        .onChange(of: appModel.snapshot?.activeThread) { _, _ in
            appState.selectedModel = ""
            appState.selectedAgentRuntimeKind = nil
            appState.reasoningEffort = ""
            appState.showModelSelector = false
        }
        .sheet(isPresented: $bindableAppState.showServerPicker) {
            NavigationStack {
                DiscoveryView(onServerSelected: { _ in
                    appState.showServerPicker = false
                })
            }
            .environment(appModel)
            .environment(appState)
            .environment(\.textScale, textScale)
        }
        .sheet(isPresented: $bindableAppState.showSettings) {
            SettingsView()
                .environment(appModel)
                .environment(appState)
                .environment(themeManager)
                .environment(\.textScale, textScale)
                .background {
                    InterfaceStyleSynchronizer(style: themeManager.appearanceMode.userInterfaceStyle)
                        .frame(width: 0, height: 0)
                }
        }
        #if targetEnvironment(macCatalyst)
        .onReceive(NotificationCenter.default.publisher(for: .remoraCommandShowSettings)) { _ in
            appState.showSettings = true
        }
        #endif
    }

    private func standardHomeNavigationView(topInset: CGFloat, bottomInset: CGFloat) -> some View {
        HomeNavigationView(
            topInset: topInset,
            bottomInset: bottomInset
        )
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .ignoresSafeArea(.container, edges: [.top, .bottom])
        .id(themeManager.themeVersion)
        .onAppear {
            if !splashDismissed {
                splashDismissed = true
                (UIApplication.shared.delegate as? AppDelegate)?.signalContentReady()
            }
        }
    }

    @ViewBuilder
    private var standardOverlays: some View {
        if petOverlay.visible, let pet = petOverlay.selectedPet {
            PetOverlayView(
                pet: pet,
                state: petOverlay.avatarState(snapshot: appModel.snapshot),
                message: petOverlay.avatarMessage(snapshot: appModel.snapshot),
                reduceMotion: UIAccessibility.isReduceMotionEnabled
            )
            .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
        }

        if let approval = appModel.snapshot?.pendingApprovals.first(where: {
            $0.kind != .mcpElicitation
        }) {
            ApprovalPromptView(approval: approval) { decision in
                Task {
                    try? await appModel.store.respondToApproval(
                        requestId: approval.id,
                        decision: decision
                    )
                }
            } onViewThread: { threadKey in
                appState.pendingThreadNavigation = threadKey
            }
        }

        if let warmupID = conversationWarmup.activeWarmupID {
            ConversationWarmupView(warmupID: warmupID) {
                conversationWarmup.finishWarmup()
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
        }
    }
}

private struct InterfaceStyleSynchronizer: UIViewRepresentable {
    let style: UIUserInterfaceStyle

    func makeUIView(context: Context) -> InterfaceStyleSyncView {
        let view = InterfaceStyleSyncView()
        view.isHidden = true
        view.isUserInteractionEnabled = false
        view.targetStyle = style
        return view
    }

    func updateUIView(_ uiView: InterfaceStyleSyncView, context: Context) {
        uiView.targetStyle = style
    }

    final class InterfaceStyleSyncView: UIView {
        var targetStyle: UIUserInterfaceStyle = .unspecified {
            didSet { applyStyleIfNeeded() }
        }

        override func didMoveToWindow() {
            super.didMoveToWindow()
            applyStyleIfNeeded()
            DispatchQueue.main.async { [weak self] in
                self?.applyStyleIfNeeded()
            }
        }

        private func applyStyleIfNeeded() {
            guard let window else { return }
            if window.overrideUserInterfaceStyle != targetStyle {
                window.overrideUserInterfaceStyle = targetStyle
            }
            guard let windowScene = window.windowScene else { return }
            for sceneWindow in windowScene.windows where sceneWindow.overrideUserInterfaceStyle != targetStyle {
                sceneWindow.overrideUserInterfaceStyle = targetStyle
            }
        }
    }
}
