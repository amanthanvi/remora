import XCTest
@testable import Remora

@MainActor
final class AdaptiveNavigationAndActionsTests: XCTestCase {
    func testLayoutRequiresUsefulWidthAndHeightForSplitMode() {
        XCTAssertEqual(
            RemoraNavigationLayoutPolicy.mode(for: CGSize(width: 1024, height: 768)),
            .split
        )
        XCTAssertEqual(
            RemoraNavigationLayoutPolicy.mode(for: CGSize(width: 852, height: 393)),
            .compact,
            "A wide phone landscape must not become an unusably short split view."
        )
        XCTAssertEqual(
            RemoraNavigationLayoutPolicy.mode(for: CGSize(width: 700, height: 900)),
            .compact
        )
    }

    func testCompactConversationSelectionKeepsBackPath() {
        let first = ThreadKey(serverId: "server-a", threadId: "thread-a")
        let second = ThreadKey(serverId: "server-a", threadId: "thread-b")
        let path = HomeNavigationPathPolicy.selectingConversation(
            second,
            in: [.conversation(first)],
            mode: .compact
        )

        XCTAssertEqual(path, [.conversation(first), .conversation(second)])
    }

    func testSplitConversationSelectionReplacesPeerDetail() {
        let first = ThreadKey(serverId: "server-a", threadId: "thread-a")
        let second = ThreadKey(serverId: "server-b", threadId: "thread-b")
        let path = HomeNavigationPathPolicy.selectingConversation(
            second,
            in: [.conversation(first), .conversationInfo(first)],
            mode: .split
        )

        XCTAssertEqual(path, [.conversation(second)])
        XCTAssertEqual(path.last?.conversationKey, second)
    }

    func testCatalogUsesLiveContextForPaletteAndShortcutAvailability() {
        var context = RemoraActionNavigationContext.empty
        context.canStartThread = true
        context.canSearchThreads = true
        context.isConversationVisible = true

        let unfocused = RemoraActionCatalog.items(context: context, composerIsFocused: false)
        let focused = RemoraActionCatalog.items(context: context, composerIsFocused: true)

        XCTAssertFalse(unfocused.first(where: { $0.id == .sendMessage })!.availability.isEnabled)
        XCTAssertTrue(focused.first(where: { $0.id == .sendMessage })!.availability.isEnabled)
        XCTAssertTrue(focused.first(where: { $0.id == .newThread })!.availability.isEnabled)
    }

    func testTerminalContextDisablesGlobalNavigationAndCyclingKeys() {
        var context = RemoraActionNavigationContext.empty
        context.canSearchThreads = true
        context.canCycleThreads = true
        context.canNavigateForward = true
        context.terminalOwnsKeyboard = true

        let items = RemoraActionCatalog.items(context: context, composerIsFocused: false)

        XCTAssertFalse(items.first(where: { $0.id == .searchThreads })!.availability.isEnabled)
        XCTAssertFalse(items.first(where: { $0.id == .nextThread })!.availability.isEnabled)
        XCTAssertFalse(items.first(where: { $0.id == .navigateForward })!.availability.isEnabled)
        XCTAssertTrue(items.first(where: { $0.id == .showSettings })!.availability.isEnabled)
    }

    func testPaletteFilteringIsBoundedToCatalogAndSearchTerms() {
        let items = RemoraActionCatalog.items(context: .empty, composerIsFocused: false)
        let results = RemoraActionCatalog.filteredItems(items, query: "shell")

        XCTAssertEqual(results.map(\.id), [.openTerminal])
        XCTAssertFalse(results.contains { $0.id == .showCommandPalette })
    }

    func testOnlyTheLatestFocusedComposerOwnsSendAction() {
        let center = RemoraActionCenter()
        let first = UUID()
        let second = UUID()

        center.setComposerFocused(true, owner: first)
        XCTAssertTrue(center.composerOwnsFocus(first))

        center.setComposerFocused(true, owner: second)
        center.setComposerFocused(false, owner: first)

        XCTAssertFalse(center.composerOwnsFocus(first))
        XCTAssertTrue(center.composerOwnsFocus(second))

        center.setComposerFocused(false, owner: second)
        XCTAssertFalse(center.composerOwnsFocus(second))
    }
}
