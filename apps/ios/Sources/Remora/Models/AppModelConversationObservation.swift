import Foundation
import Observation

/// Root-level chrome state projected away from the full Rust snapshot.
///
/// Streaming conversation payloads change frequently, but the app root only
/// needs the active route and approval overlay. Keeping that payload behind an
/// equality-gated revision prevents an
/// unrelated stream from invalidating the entire SwiftUI hierarchy.
@MainActor
@Observable
final class AppModelChromeObservation {
    private struct State: Equatable {
        var activeThread: ThreadKey?
        var pendingApproval: PendingApproval?
    }

    private(set) var revision: UInt64 = 0

    @ObservationIgnored private var state = State(
        activeThread: nil,
        pendingApproval: nil
    )

    var activeThread: ThreadKey? { state.activeThread }
    var pendingApproval: PendingApproval? { state.pendingApproval }

    func refresh(snapshot: AppSnapshotRecord?) {
        let nextState = State(
            activeThread: snapshot?.activeThread,
            pendingApproval: snapshot?.pendingApprovals.first {
                $0.kind != .mcpElicitation
            }
        )
        guard state != nextState else { return }
        state = nextState
        revision &+= 1
    }
}

/// Navigation-only state projected away from thread content and activity.
///
/// Home navigation needs identities, working directories, resume state, and
/// coarse server transport state. In particular, previews, tool logs, token
/// usage, and hydrated conversation items must not rebuild the navigation root
/// while a response streams.
@MainActor
@Observable
final class AppModelNavigationObservation {
    struct SessionTarget: Equatable {
        let key: ThreadKey
        let cwd: String
        let isResumed: Bool
    }

    struct ServerIdentity: Equatable {
        let serverId: String
        let transportState: AppServerTransportState
        let port: UInt16

        var isConnected: Bool { transportState == .connected }
    }

    private struct State: Equatable {
        var activeThread: ThreadKey?
        var sessionTargets: [SessionTarget]
        var threadKeys: [ThreadKey]
        var servers: [ServerIdentity]
    }

    private(set) var revision: UInt64 = 0

    @ObservationIgnored private var state = State(
        activeThread: nil,
        sessionTargets: [],
        threadKeys: [],
        servers: []
    )

    var activeThread: ThreadKey? { state.activeThread }
    var sessionTargets: [SessionTarget] { state.sessionTargets }
    var servers: [ServerIdentity] { state.servers }

    func threadKey(threadId: String) -> ThreadKey? {
        state.threadKeys.first { $0.threadId == threadId }
    }

    func sessionTarget(for key: ThreadKey) -> SessionTarget? {
        state.sessionTargets.first { $0.key == key }
    }

    func refresh(snapshot: AppSnapshotRecord?) {
        let nextState = State(
            activeThread: snapshot?.activeThread,
            sessionTargets: snapshot?.sessionSummaries.map {
                SessionTarget(key: $0.key, cwd: $0.cwd, isResumed: $0.isResumed)
            } ?? [],
            threadKeys: snapshot?.threads.map(\.key) ?? [],
            servers: snapshot?.servers.map {
                ServerIdentity(
                    serverId: $0.serverId,
                    transportState: $0.transportState,
                    port: $0.port
                )
            } ?? []
        )
        guard state != nextState else { return }
        state = nextState
        revision &+= 1
    }
}

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
