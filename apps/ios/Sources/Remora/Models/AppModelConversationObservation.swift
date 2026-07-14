import Foundation
import Observation

/// A narrowly-scoped observation surface for one conversation route.
///
/// The payload is deliberately ignored by Observation. Consumers observe the
/// equality-gated `revision`, so an update to another thread in the global app
/// snapshot cannot invalidate the active conversation hierarchy.
@MainActor
@Observable
final class AppModelConversationObservation {
    private struct State: Equatable {
        var thread: AppThreadSnapshot?
        var pendingUserInputRequest: PendingUserInputRequest?
        var server: AppServerSnapshot?
        var agentDirectoryVersion: UInt64
        var composerPrefillRequest: AppModel.ComposerPrefillRequest?
    }

    let threadKey: ThreadKey
    private(set) var revision: UInt64 = 0

    @ObservationIgnored private var state: State

    var thread: AppThreadSnapshot? { state.thread }
    var pendingUserInputRequest: PendingUserInputRequest? { state.pendingUserInputRequest }
    var server: AppServerSnapshot? { state.server }
    var agentDirectoryVersion: UInt64 { state.agentDirectoryVersion }
    var composerPrefillRequest: AppModel.ComposerPrefillRequest? { state.composerPrefillRequest }

    init(threadKey: ThreadKey, initialThread: AppThreadSnapshot? = nil) {
        self.threadKey = threadKey
        self.state = State(
            thread: initialThread,
            pendingUserInputRequest: nil,
            server: nil,
            agentDirectoryVersion: 0,
            composerPrefillRequest: nil
        )
    }

    func matching(threadKey expectedThreadKey: ThreadKey?) -> AppModelConversationObservation? {
        guard threadKey == expectedThreadKey else { return nil }
        return self
    }

    func refresh(
        snapshot: AppSnapshotRecord?,
        cachedThread: AppThreadSnapshot?,
        composerPrefillRequest: AppModel.ComposerPrefillRequest?
    ) {
        let nextState = State(
            thread: snapshot?.threadSnapshot(for: threadKey) ?? cachedThread,
            pendingUserInputRequest: snapshot?.pendingUserInputs.first {
                $0.isRelevant(to: threadKey)
            },
            server: snapshot?.serverSnapshot(for: threadKey.serverId),
            agentDirectoryVersion: snapshot?.agentDirectoryVersion ?? 0,
            composerPrefillRequest: composerPrefillRequest.flatMap { request in
                request.threadKey == threadKey ? request : nil
            }
        )
        guard state != nextState else { return }
        state = nextState
        revision &+= 1
    }
}

@MainActor
final class WeakAppModelConversationObservation {
    weak var value: AppModelConversationObservation?

    init(_ value: AppModelConversationObservation) {
        self.value = value
    }
}
