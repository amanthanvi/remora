import CoreGraphics
import Foundation

enum RemoraNavigationMode: String, Equatable {
    case compact
    case split
}

enum RemoraNavigationLayoutPolicy {
    /// A split surface must leave enough horizontal room for a useful
    /// sidebar and conversation, and enough vertical room that the sidebar
    /// search and thread list remain usable. This intentionally keeps wide,
    /// short phone-landscape windows compact.
    static let minimumSplitWidth: CGFloat = 760
    static let minimumSplitHeight: CGFloat = 540

    static func mode(for size: CGSize) -> RemoraNavigationMode {
        guard size.width >= minimumSplitWidth,
              size.height >= minimumSplitHeight else {
            return .compact
        }
        return .split
    }
}

enum HomeNavigationRoute: Hashable {
    case sessions(serverId: String, title: String)
    case conversation(ThreadKey)
    case realtimeVoice(ThreadKey)
    case conversationInfo(ThreadKey)
    case wallpaperSelection(ThreadKey)
    case wallpaperAdjust(ThreadKey)
    case serverInfo(serverId: String)
    case serverWallpaperSelection(serverId: String)
    case serverWallpaperAdjust(serverId: String)
    case replayRecording(URL)
    case newThread
    case appsList
    case savedApp(appId: String)
    case terminal(preferredAlleycatNodeId: String?)

    var conversationKey: ThreadKey? {
        guard case let .conversation(key) = self else { return nil }
        return key
    }

    var ownsTerminalKeyboard: Bool {
        if case .terminal = self { return true }
        return false
    }
}

enum HomeNavigationPathPolicy {
    static func selectingConversation(
        _ key: ThreadKey,
        in path: [HomeNavigationRoute],
        mode: RemoraNavigationMode
    ) -> [HomeNavigationRoute] {
        let destination = HomeNavigationRoute.conversation(key)
        guard path.last != destination else { return path }

        switch mode {
        case .compact:
            return path + [destination]
        case .split:
            // Sidebar selections are peers. Replacing the detail path avoids
            // growing a hidden back stack on every thread tap, while a later
            // collapse still leaves one route and therefore one usable path
            // back to Home.
            return [destination]
        }
    }

    static func replacingTopConversation(
        with key: ThreadKey,
        in path: [HomeNavigationRoute],
        mode: RemoraNavigationMode
    ) -> [HomeNavigationRoute] {
        var nextPath = path
        if nextPath.last?.conversationKey != nil {
            nextPath.removeLast()
        }
        return selectingConversation(key, in: nextPath, mode: mode)
    }
}
