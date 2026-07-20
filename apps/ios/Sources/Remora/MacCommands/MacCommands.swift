import SwiftUI
import UIKit

#if targetEnvironment(macCatalyst)
extension Notification.Name {
    /// Posted with userInfo `["index": Int]` where `index` is 0-based.
    static let remoraCommandSelectSession = Notification.Name("com.remora.command.selectSession")
}
#endif

struct RemoraCommands: Commands {
    let actionCenter: RemoraActionCenter
    let appModel: AppModel

    var body: some Commands {
        CommandGroup(replacing: .newItem) {
            RemoraActionCommandButton(id: .newThread, actionCenter: actionCenter)

            #if targetEnvironment(macCatalyst)
            Button("New Window") {
                openNewWindow()
            }
            .keyboardShortcut("n", modifiers: [.command, .shift])
            #endif
        }

        CommandMenu("Navigate") {
            RemoraActionCommandButton(id: .showCommandPalette, actionCenter: actionCenter)
            RemoraActionCommandButton(id: .searchThreads, actionCenter: actionCenter)
            Divider()
            RemoraActionCommandButton(id: .navigateBack, actionCenter: actionCenter)
            RemoraActionCommandButton(id: .navigateForward, actionCenter: actionCenter)
        }

        CommandMenu("Thread") {
            RemoraActionCommandButton(id: .sendMessage, actionCenter: actionCenter)
            Divider()
            RemoraActionCommandButton(id: .previousThread, actionCenter: actionCenter)
            RemoraActionCommandButton(id: .nextThread, actionCenter: actionCenter)
        }

        CommandMenu("Terminal") {
            RemoraActionCommandButton(id: .openTerminal, actionCenter: actionCenter)
        }

        // Replace UIKit's built-in Cmd-, entry so the standard shortcut opens
        // Remora's in-app settings sheet instead of competing with a second
        // command that has undefined menu-builder precedence.
        CommandGroup(replacing: .appSettings) {
            RemoraActionCommandButton(id: .showSettings, actionCenter: actionCenter)
        }

        #if targetEnvironment(macCatalyst)
        SidebarCommands()

        CommandMenu("Session Slots") {
            SessionShortcutsMenu(appModel: appModel)
        }
        #endif
    }
}

private struct RemoraActionCommandButton: View {
    let id: RemoraActionID
    let actionCenter: RemoraActionCenter

    var body: some View {
        let item = actionCenter.item(for: id)
        Group {
            if let shortcut = item.definition.shortcut {
                commandButton(item)
                    .keyboardShortcut(shortcut.key, modifiers: shortcut.modifiers)
            } else {
                commandButton(item)
            }
        }
    }

    private func commandButton(_ item: RemoraActionItem) -> some View {
        Button(item.definition.title) {
            _ = actionCenter.perform(id, source: .keyboard)
        }
        .disabled(!item.availability.isEnabled)
        .help(item.availability.disabledReason ?? item.definition.detail)
    }
}

#if targetEnvironment(macCatalyst)
private struct SessionShortcutsMenu: View {
    let appModel: AppModel

    var body: some View {
        let summaries = appModel.snapshot?.sessionSummaries ?? []
        ForEach(0..<9, id: \.self) { index in
            let shortcutKey = KeyEquivalent(Character("\(index + 1)"))
            let summary: AppSessionSummary? = summaries.indices.contains(index) ? summaries[index] : nil
            Button(label(for: summary, index: index)) {
                guard summary != nil else { return }
                NotificationCenter.default.post(
                    name: .remoraCommandSelectSession,
                    object: nil,
                    userInfo: ["index": index]
                )
            }
            .keyboardShortcut(shortcutKey, modifiers: [.command])
            .disabled(summary == nil)
        }
    }

    private func label(for summary: AppSessionSummary?, index: Int) -> String {
        guard let summary else { return "Session \(index + 1)" }
        return "Session \(index + 1): \(summary.displayTitle)"
    }
}

@MainActor
private func openNewWindow() {
    UIApplication.shared.requestSceneSessionActivation(
        nil,
        userActivity: nil,
        options: nil,
        errorHandler: { error in
            LLog.error("multiwindow", "open failed", error: error)
        }
    )
}

/// Catalyst window setup: keeps the underlying NSWindow opaque (so the
/// desktop can't bleed through sidebar Liquid Glass), installs a
/// compact unified titlebar with an NSToolbar carrying the settings
/// button next to the traffic lights, and declares the window's
/// resize bounds.
struct MacWindowTitleBarStyler: UIViewRepresentable {
    func makeUIView(context: Context) -> UIView {
        let view = SceneConfigView()
        view.isHidden = true
        return view
    }

    func updateUIView(_ uiView: UIView, context: Context) {}

    private final class SceneConfigView: UIView {
        override func didMoveToWindow() {
            super.didMoveToWindow()
            // Force the Catalyst UIWindow opaque so the desktop can't
            // bleed through NavigationSplitView's sidebar Liquid Glass
            // material. Paint black behind SwiftUI so any remaining
            // translucency still resolves to a dark surface, not the
            // Mac desktop.
            if let window {
                window.isOpaque = true
                window.backgroundColor = .black
            }

            DispatchQueue.main.async { [weak self] in
                guard let windowScene = self?.window?.windowScene else { return }
                if let titlebar = windowScene.titlebar {
                    titlebar.titleVisibility = .hidden
                    titlebar.toolbar = nil
                }
                let restrictions = windowScene.sizeRestrictions
                restrictions?.minimumSize = CGSize(width: 760, height: 560)
                restrictions?.maximumSize = CGSize(
                    width: CGFloat.greatestFiniteMagnitude,
                    height: CGFloat.greatestFiniteMagnitude
                )
            }
        }
    }
}
#endif
