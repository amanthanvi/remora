import Foundation
import Observation

enum TerminalRenderUpdate: Sendable {
    case replace(Data)
    case append(Data)
}

@MainActor
@Observable
final class TerminalSessionController {
    enum Phase: Equatable {
        case idle
        case connecting
        case running
        case exited(Int32)
        case failed(String)
    }

    struct SshHostTrustChallenge {
        let host: String
        let port: UInt16
        let fingerprint: String
        let backend: TerminalBackendKind
    }

    private(set) var phase: Phase = .idle
    private(set) var output = ""
    private(set) var sessionId: String?
    private(set) var sshTrustChallenge: SshHostTrustChallenge?

    @ObservationIgnored private let appStore: AppStore
    @ObservationIgnored private var outputListener: TerminalOutputRelay?
    @ObservationIgnored private var outputSubscription: TerminalOutputSubscription?
    @ObservationIgnored private var outputSink: ((TerminalRenderUpdate) -> Void)?
    @ObservationIgnored private var outputBytes = Data()
    @ObservationIgnored private var expectedSequence: UInt64?
    @ObservationIgnored private var eventGeneration = 0
    @ObservationIgnored private var terminalSize = TerminalSize(cols: 80, rows: 24)

    init() {
        self.appStore = AppModel.shared.store
    }

    init(appStore: AppStore) {
        self.appStore = appStore
    }

    var canSendInput: Bool {
        if case .running = phase { return true }
        return false
    }

    func open(backend: TerminalBackendKind) async {
        guard sessionId == nil else { return }
        eventGeneration &+= 1
        let generation = eventGeneration
        phase = .connecting
        sshTrustChallenge = nil
        do {
            let id: String
            if isSshBackend(backend) {
                let trustStore = TerminalSshTrustStore(backend: SwiftSshTrustBackend.shared)
                id = try await appStore.openTerminalSessionWithTrustStore(
                    kind: backend,
                    size: terminalSize,
                    trustStore: trustStore
                )
            } else {
                id = try await appStore.openTerminalSession(
                    kind: backend,
                    size: terminalSize
                )
            }
            guard generation == eventGeneration else {
                try? await appStore.closeTerminalSession(id: id)
                return
            }
            guard let session = appStore.terminalSessionHandle(id: id) else {
                try? await appStore.closeTerminalSession(id: id)
                guard generation == eventGeneration else { return }
                phase = .failed("Session disappeared after open")
                return
            }
            sessionId = id
            appStore.setActiveTerminalId(id: id)
            let listener = TerminalOutputRelay(owner: self, generation: generation)
            outputSubscription = session.subscribeOutputEvents(listener: listener)
            outputListener = listener
            phase = .running
        } catch {
            guard generation == eventGeneration else { return }
            sessionId = nil
            if let challenge = Self.sshHostTrustChallenge(from: error, backend: backend) {
                sshTrustChallenge = challenge
                phase = .failed("Unknown SSH host key \(challenge.fingerprint)")
            } else {
                phase = .failed(error.localizedDescription)
            }
        }
    }

    private func isSshBackend(_ backend: TerminalBackendKind) -> Bool {
        if case .remoteSsh = backend { return true }
        return false
    }

    func trustUnknownSshHostAndRetry() async {
        guard let challenge = sshTrustChallenge else { return }
        // Record through the Rust store, not the backend directly: `pin`
        // applies the same host normalization (case, brackets, IPv6 zone id)
        // that the connect-time lookup uses. Writing the raw challenge host
        // straight to the backend would file the approval under a
        // noncanonical key, leaving the canonical spelling unpinned and still
        // eligible for trust-on-first-use.
        do {
            try TerminalSshTrustStore(backend: SwiftSshTrustBackend.shared).pin(
                host: challenge.host,
                port: challenge.port,
                fingerprint: challenge.fingerprint
            )
        } catch {
            phase = .failed(error.localizedDescription)
            return
        }
        sshTrustChallenge = nil
        phase = .idle
        await open(backend: challenge.backend)
    }

    func switchBackend(_ backend: TerminalBackendKind) async {
        close()
        replaceOutput(Data())
        await open(backend: backend)
    }

    func send(_ string: String) async {
        await send(Data(string.utf8))
    }

    func send(_ data: Data) async {
        guard let id = sessionId, canSendInput else { return }
        guard let session = appStore.terminalSessionHandle(id: id) else { return }
        do {
            try await session.writeInput(data: data)
        } catch {
            phase = .failed(error.localizedDescription)
        }
    }

    func sendLine(_ string: String) async {
        await send(string + "\n")
    }

    func clearOutput() {
        replaceOutput(Data())
    }

    func setOutputSink(_ sink: ((TerminalRenderUpdate) -> Void)?) {
        outputSink = sink
        if sink == nil {
            output = String(decoding: outputBytes, as: UTF8.self)
        }
        sink?(.replace(outputBytes))
    }

    private static func sshHostTrustChallenge(
        from error: Error,
        backend: TerminalBackendKind
    ) -> SshHostTrustChallenge? {
        guard case .remoteSsh(
            host: _,
            port: _,
            username: _,
            auth: _,
            shell: _,
            acceptUnknownHost: _,
            cwd: _
        ) = backend else {
            return nil
        }
        guard let terminalError = error as? TerminalError else {
            return nil
        }
        guard case let .SshHostKeyVerification(host, port, fingerprint, pinned) = terminalError,
              pinned == nil else { return nil }
        return SshHostTrustChallenge(
            host: host,
            port: port,
            fingerprint: fingerprint,
            backend: backend
        )
    }

    func resize(cols: UInt16, rows: UInt16, notifyBackend: Bool = true) async {
        guard cols > 0, rows > 0 else { return }
        let size = TerminalSize(cols: cols, rows: rows)
        terminalSize = size
        guard notifyBackend, let id = sessionId, canSendInput else { return }
        guard let session = appStore.terminalSessionHandle(id: id) else { return }
        do {
            try await session.resize(size: size)
        } catch {
            phase = .failed(error.localizedDescription)
        }
    }

    func close() {
        eventGeneration &+= 1
        outputSubscription?.cancel()
        outputSubscription = nil
        outputListener?.deactivate()
        outputListener = nil
        let id = sessionId
        sessionId = nil
        phase = .idle
        guard let id else { return }
        if appStore.activeTerminalId() == id {
            appStore.setActiveTerminalId(id: nil)
        }
        Task.detached(priority: .utility) { [appStore] in
            try? await appStore.closeTerminalSession(id: id)
        }
    }

    fileprivate func applyOutputEvents(
        _ events: [TerminalOutputStreamEvent],
        generation: Int
    ) {
        guard generation == eventGeneration else { return }
        for event in events {
            switch event {
            case let .snapshot(snapshot), let .reset(snapshot):
                applySnapshot(snapshot)
            case let .output(sequence, data):
                applyOutput(sequence: sequence, data: data)
            case let .exited(sequence, code):
                if let expectedSequence, sequence < expectedSequence { continue }
                expectedSequence = sequence &+ 1
                phase = .exited(code)
            }
        }
    }

    private func applySnapshot(_ snapshot: TerminalOutputSnapshot) {
        expectedSequence = snapshot.latestSequence.map { $0 &+ 1 } ?? snapshot.baseSequence
        replaceOutput(snapshot.bytes)
        if let exitCode = snapshot.exitCode {
            phase = .exited(exitCode)
        }
    }

    private func applyOutput(sequence: UInt64, data: Data) {
        if let expectedSequence {
            if sequence < expectedSequence { return }
            if sequence > expectedSequence,
               let id = sessionId,
               let session = appStore.terminalSessionHandle(id: id) {
                applySnapshot(session.outputSnapshot())
                return
            }
        }
        expectedSequence = sequence &+ 1
        appendOutput(data)
    }

    private func replaceOutput(_ data: Data) {
        outputBytes = boundedOutput(data)
        output = String(decoding: outputBytes, as: UTF8.self)
        outputSink?(.replace(outputBytes))
    }

    private func appendOutput(_ data: Data) {
        guard !data.isEmpty else { return }
        outputBytes.append(data)
        outputBytes = boundedOutput(outputBytes)
        if outputSink == nil {
            output = String(decoding: outputBytes, as: UTF8.self)
        }
        outputSink?(.append(data))
    }

    private func boundedOutput(_ data: Data) -> Data {
        let maxCount = 64 * 1024
        guard data.count > maxCount else { return data }
        return Data(data.suffix(maxCount))
    }
}

private final class TerminalOutputRelay: TerminalOutputEventListener, @unchecked Sendable {
    private weak var owner: TerminalSessionController?
    private let generation: Int
    private let lock = NSLock()
    private var pendingEvents: [TerminalOutputStreamEvent] = []
    private var drainScheduled = false
    private var active = true

    init(owner: TerminalSessionController, generation: Int) {
        self.owner = owner
        self.generation = generation
    }

    func deactivate() {
        lock.lock()
        active = false
        pendingEvents.removeAll()
        lock.unlock()
    }

    func onEvent(event: TerminalOutputStreamEvent) {
        var shouldSchedule = false
        lock.lock()
        if active {
            pendingEvents.append(event)
            if !drainScheduled {
                drainScheduled = true
                shouldSchedule = true
            }
        }
        lock.unlock()

        if shouldSchedule {
            Task { @MainActor [weak self] in
                self?.drain()
            }
        }
    }

    @MainActor
    private func drain() {
        lock.lock()
        let events = active ? pendingEvents : []
        pendingEvents.removeAll(keepingCapacity: true)
        drainScheduled = false
        lock.unlock()
        guard !events.isEmpty else { return }
        owner?.applyOutputEvents(events, generation: generation)
    }
}
