import CoreGraphics
import Foundation

struct RemoraLinkTerminalHost: Identifiable, Equatable, Hashable {
    let hostId: String
    let displayName: String

    var id: String { hostId }

    var backend: TerminalBackendKind {
        .remoteRemoraLink(hostId: hostId, shell: nil)
    }
}

enum RemoraLinkTerminalSupport {
    static func eligibleHosts(
        from summaries: [AppRemoraLinkHostSummary]
    ) -> [RemoraLinkTerminalHost] {
        summaries.compactMap { summary in
            guard summary.state == .paired,
                  summary.selectedRuntimeIds.contains("shell"),
                  summary.grantedScopes.contains(.connectRuntime) else {
                return nil
            }
            let trimmedName = summary.hostDisplayName.trimmingCharacters(in: .whitespacesAndNewlines)
            return RemoraLinkTerminalHost(
                hostId: summary.hostId,
                displayName: trimmedName.isEmpty ? "Remote shell" : trimmedName
            )
        }
    }

    static func eligibleHostIds(
        from summaries: [AppRemoraLinkHostSummary]
    ) -> Set<String> {
        Set(eligibleHosts(from: summaries).map(\.hostId))
    }

    static func initialOption<Option>(
        preferredHostId: String?,
        options: [Option],
        hostId: (Option) -> String?
    ) -> Option? {
        guard let preferredHostId else { return options.first }
        let normalized = preferredHostId.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !normalized.isEmpty else { return nil }
        return options.first { hostId($0) == normalized }
    }
}

struct RemoraLinkTerminalEligibilityState {
    private(set) var canOpenShell = false
    private var generation: UInt64 = 0

    mutating func beginRequest() -> UInt64 {
        generation &+= 1
        canOpenShell = false
        return generation
    }

    mutating func invalidate() {
        generation &+= 1
        canOpenShell = false
    }

    mutating func apply(
        hosts: [AppRemoraLinkHostSummary],
        serverId: String,
        generation: UInt64
    ) {
        guard generation == self.generation else { return }
        canOpenShell = RemoraLinkTerminalSupport.eligibleHostIds(from: hosts).contains(serverId)
    }
}

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
    case terminal(preferredRemoraLinkHostId: String?)

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
