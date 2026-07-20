import Foundation
import Observation
import SwiftUI

enum RemoraActionGroup: Int, CaseIterable, Identifiable {
    case navigate
    case thread
    case review
    case terminal
    case settings

    var id: Int { rawValue }

    var title: String {
        switch self {
        case .navigate: return "Navigate"
        case .thread: return "Thread"
        case .review: return "Review"
        case .terminal: return "Terminal"
        case .settings: return "Settings"
        }
    }
}

enum RemoraActionID: String, CaseIterable, Identifiable {
    case showCommandPalette
    case newThread
    case searchThreads
    case navigateBack
    case navigateForward
    case previousThread
    case nextThread
    case sendMessage
    case openTerminal
    case showSettings

    var id: String { rawValue }
}

struct RemoraKeyboardShortcut {
    let key: KeyEquivalent
    let modifiers: EventModifiers
    let display: String
}

struct RemoraActionDefinition: Identifiable {
    let id: RemoraActionID
    let group: RemoraActionGroup
    let title: String
    let detail: String
    let systemImage: String
    let searchTerms: [String]
    let shortcut: RemoraKeyboardShortcut?
    var appearsInPalette = true
}

enum RemoraActionAvailability: Equatable {
    case enabled
    case disabled(reason: String)

    var isEnabled: Bool {
        if case .enabled = self { return true }
        return false
    }

    var disabledReason: String? {
        guard case let .disabled(reason) = self else { return nil }
        return reason
    }
}

struct RemoraActionItem: Identifiable {
    let definition: RemoraActionDefinition
    let availability: RemoraActionAvailability

    var id: RemoraActionID { definition.id }
}

struct RemoraActionNavigationContext: Equatable {
    var canStartThread = false
    var canSearchThreads = false
    var canNavigateBack = false
    var canNavigateForward = false
    var canCycleThreads = false
    var canOpenTerminal = false
    var isConversationVisible = false
    var terminalOwnsKeyboard = false

    static let empty = RemoraActionNavigationContext()
}

enum RemoraActionCatalog {
    static let definitions: [RemoraActionDefinition] = [
        RemoraActionDefinition(
            id: .showCommandPalette,
            group: .navigate,
            title: "Show Command Palette",
            detail: "Find an available Remora action",
            systemImage: "command",
            searchTerms: ["commands", "actions", "palette"],
            shortcut: RemoraKeyboardShortcut(
                key: "p",
                modifiers: [.command, .shift],
                display: "⇧⌘P"
            ),
            appearsInPalette: false
        ),
        RemoraActionDefinition(
            id: .newThread,
            group: .thread,
            title: "New Thread",
            detail: "Start work on a connected host",
            systemImage: "square.and.pencil",
            searchTerms: ["new", "session", "conversation"],
            shortcut: RemoraKeyboardShortcut(key: "n", modifiers: [.command], display: "⌘N")
        ),
        RemoraActionDefinition(
            id: .searchThreads,
            group: .navigate,
            title: "Search Threads",
            detail: "Find work across connected hosts",
            systemImage: "magnifyingglass",
            searchTerms: ["find", "filter", "sessions"],
            shortcut: RemoraKeyboardShortcut(key: "f", modifiers: [.command], display: "⌘F")
        ),
        RemoraActionDefinition(
            id: .navigateBack,
            group: .navigate,
            title: "Back",
            detail: "Return to the previous destination",
            systemImage: "chevron.backward",
            searchTerms: ["previous", "home"],
            shortcut: RemoraKeyboardShortcut(key: "[", modifiers: [.command], display: "⌘[")
        ),
        RemoraActionDefinition(
            id: .navigateForward,
            group: .navigate,
            title: "Active Thread",
            detail: "Return to the currently active thread",
            systemImage: "chevron.forward",
            searchTerms: ["forward", "current", "conversation"],
            shortcut: RemoraKeyboardShortcut(key: "]", modifiers: [.command], display: "⌘]")
        ),
        RemoraActionDefinition(
            id: .previousThread,
            group: .thread,
            title: "Previous Thread",
            detail: "Move through the current thread list",
            systemImage: "arrow.up",
            searchTerms: ["cycle", "session", "earlier"],
            shortcut: RemoraKeyboardShortcut(
                key: .upArrow,
                modifiers: [.command, .option],
                display: "⌥⌘↑"
            )
        ),
        RemoraActionDefinition(
            id: .nextThread,
            group: .thread,
            title: "Next Thread",
            detail: "Move through the current thread list",
            systemImage: "arrow.down",
            searchTerms: ["cycle", "session", "later"],
            shortcut: RemoraKeyboardShortcut(
                key: .downArrow,
                modifiers: [.command, .option],
                display: "⌥⌘↓"
            )
        ),
        RemoraActionDefinition(
            id: .sendMessage,
            group: .thread,
            title: "Send Message",
            detail: "Send from the focused composer",
            systemImage: "paperplane",
            searchTerms: ["submit", "composer", "prompt"],
            shortcut: RemoraKeyboardShortcut(key: .return, modifiers: [.command], display: "⌘↩")
        ),
        RemoraActionDefinition(
            id: .openTerminal,
            group: .terminal,
            title: "Open Terminal",
            detail: "Open the remote terminal for this host",
            systemImage: "terminal",
            searchTerms: ["shell", "ghostty", "remote"],
            shortcut: RemoraKeyboardShortcut(key: "t", modifiers: [.command], display: "⌘T")
        ),
        RemoraActionDefinition(
            id: .showSettings,
            group: .settings,
            title: "Settings",
            detail: "Configure Remora",
            systemImage: "gearshape",
            searchTerms: ["preferences", "appearance", "configuration"],
            shortcut: RemoraKeyboardShortcut(key: ",", modifiers: [.command], display: "⌘,")
        ),
    ]

    static func items(
        context: RemoraActionNavigationContext,
        composerIsFocused: Bool
    ) -> [RemoraActionItem] {
        definitions.map { definition in
            RemoraActionItem(
                definition: definition,
                availability: availability(
                    for: definition.id,
                    context: context,
                    composerIsFocused: composerIsFocused
                )
            )
        }
    }

    static func filteredItems(_ items: [RemoraActionItem], query: String) -> [RemoraActionItem] {
        let normalized = query.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
        let paletteItems = items.filter(\.definition.appearsInPalette)
        guard !normalized.isEmpty else { return paletteItems }
        return paletteItems.filter { item in
            let searchable = [
                item.definition.title,
                item.definition.detail,
                item.definition.group.title,
            ] + item.definition.searchTerms
            return searchable.contains { $0.lowercased().contains(normalized) }
        }
    }

    private static func availability(
        for id: RemoraActionID,
        context: RemoraActionNavigationContext,
        composerIsFocused: Bool
    ) -> RemoraActionAvailability {
        switch id {
        case .showCommandPalette, .showSettings:
            return .enabled
        case .newThread:
            return context.canStartThread
                ? .enabled
                : .disabled(reason: "Connect a host before starting a thread.")
        case .searchThreads:
            if context.terminalOwnsKeyboard {
                return .disabled(reason: "Leave the terminal before searching threads.")
            }
            return context.canSearchThreads
                ? .enabled
                : .disabled(reason: "Return Home to search threads on this compact layout.")
        case .navigateBack:
            return context.canNavigateBack
                ? .enabled
                : .disabled(reason: "There is no previous destination.")
        case .navigateForward:
            if context.terminalOwnsKeyboard {
                return .disabled(reason: "Terminal input currently owns navigation keys.")
            }
            return context.canNavigateForward
                ? .enabled
                : .disabled(reason: "The active thread is already visible.")
        case .previousThread, .nextThread:
            if context.terminalOwnsKeyboard {
                return .disabled(reason: "Terminal input currently owns navigation keys.")
            }
            return context.canCycleThreads
                ? .enabled
                : .disabled(reason: "Open at least two threads to cycle between them.")
        case .sendMessage:
            return context.isConversationVisible && composerIsFocused
                ? .enabled
                : .disabled(reason: "Focus a conversation composer before sending.")
        case .openTerminal:
            if context.terminalOwnsKeyboard {
                return .disabled(reason: "The terminal is already open.")
            }
            return context.canOpenTerminal
                ? .enabled
                : .disabled(reason: "Pair a compatible host and enable Terminal first.")
        }
    }
}

enum RemoraActionSource: Equatable {
    case keyboard
    case palette
    case toolbar
}

@MainActor
final class RemoraActionRequest {
    let id: RemoraActionID
    let source: RemoraActionSource
    let contextRevision: UInt64

    init(id: RemoraActionID, source: RemoraActionSource, contextRevision: UInt64) {
        self.id = id
        self.source = source
        self.contextRevision = contextRevision
    }

    func finish(errorMessage: String? = nil) {
        RemoraActionCenter.shared.finish(self, errorMessage: errorMessage)
    }
}

extension Notification.Name {
    static let remoraActionRequested = Notification.Name("com.remora.action.requested")
}

@MainActor
@Observable
final class RemoraActionCenter {
    static let shared = RemoraActionCenter()

    private(set) var navigationContext: RemoraActionNavigationContext = .empty
    private(set) var contextRevision: UInt64 = 0
    private(set) var executingActionID: RemoraActionID?
    var isPalettePresented = false
    var executionErrorMessage: String?

    private var focusedComposerOwner: UUID?

    var items: [RemoraActionItem] {
        RemoraActionCatalog.items(
            context: navigationContext,
            composerIsFocused: focusedComposerOwner != nil
        )
    }

    func item(for id: RemoraActionID) -> RemoraActionItem {
        items.first(where: { $0.id == id }) ?? RemoraActionItem(
            definition: RemoraActionDefinition(
                id: id,
                group: .navigate,
                title: id.rawValue,
                detail: "",
                systemImage: "questionmark",
                searchTerms: [],
                shortcut: nil
            ),
            availability: .disabled(reason: "This action is unavailable.")
        )
    }

    func updateNavigationContext(_ nextContext: RemoraActionNavigationContext) {
        guard navigationContext != nextContext else { return }
        navigationContext = nextContext
        contextRevision &+= 1
    }

    func setComposerFocused(_ focused: Bool, owner: UUID) {
        if focused {
            focusedComposerOwner = owner
        } else if focusedComposerOwner == owner {
            focusedComposerOwner = nil
        }
    }

    func composerOwnsFocus(_ owner: UUID) -> Bool {
        focusedComposerOwner == owner
    }

    func presentPalette() {
        executionErrorMessage = nil
        isPalettePresented = true
    }

    @discardableResult
    func perform(_ id: RemoraActionID, source: RemoraActionSource) -> Bool {
        if id == .showCommandPalette {
            presentPalette()
            return true
        }

        let action = item(for: id)
        guard action.availability.isEnabled else {
            executionErrorMessage = action.availability.disabledReason
            return false
        }

        executionErrorMessage = nil
        if source == .palette {
            executingActionID = id
        }
        NotificationCenter.default.post(
            name: .remoraActionRequested,
            object: RemoraActionRequest(
                id: id,
                source: source,
                contextRevision: contextRevision
            )
        )
        return true
    }

    func finish(_ request: RemoraActionRequest, errorMessage: String?) {
        if request.source == .palette {
            executingActionID = nil
            executionErrorMessage = errorMessage
            if errorMessage == nil {
                isPalettePresented = false
            }
        }
    }
}
