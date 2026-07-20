#if REMORA_LINK_JOURNAL_PROCESS_HELPER
import Darwin
import Foundation

/// Host-side, real-process regression harness for first-use directory durability.
///
/// Compile this file together with `RemoraLinkJournalStore.swift` using
/// `-D REMORA_LINK_JOURNAL_PROCESS_HELPER`, then run `orchestrate <empty-root>`.
/// The orchestrator suspends one process after mkdir but before ancestor fsync,
/// releases 23 other processes through one barrier, and verifies that every
/// loser still fsyncs the complete root-to-leaf chain before CAS.
@main
enum RemoraLinkJournalProcessHelper {
    private static let workerCount = 24
    private static let storedExitCode: Int32 = 0
    private static let conflictExitCode: Int32 = 10
    private static let unavailableExitCode: Int32 = 11
    private static let invalidExitCode: Int32 = 12

    static func main() {
        do {
            guard CommandLine.arguments.count >= 3 else {
                throw HelperFailure("usage: <orchestrate|worker> <root> [worker-index]")
            }
            let mode = CommandLine.arguments[1]
            let rootURL = URL(
                fileURLWithPath: CommandLine.arguments[2],
                isDirectory: true
            ).standardizedFileURL
            switch mode {
            case "orchestrate":
                try orchestrate(rootURL: rootURL)
            case "worker":
                guard CommandLine.arguments.count == 4,
                      let index = Int(CommandLine.arguments[3]) else {
                    throw HelperFailure("worker requires an integer index")
                }
                runWorker(rootURL: rootURL, index: index)
            default:
                throw HelperFailure("unknown mode \(mode)")
            }
        } catch {
            FileHandle.standardError.write(Data("FAIL: \(error)\n".utf8))
            Darwin.exit(70)
        }
    }

    private static func orchestrate(rootURL: URL) throws {
        var rootIsDirectory: ObjCBool = false
        guard FileManager.default.fileExists(
            atPath: rootURL.path,
            isDirectory: &rootIsDirectory
        ), rootIsDirectory.boolValue else {
            throw HelperFailure("root must be an existing directory")
        }

        let nestedDirectory = rootURL
            .appendingPathComponent("RemoraLink", isDirectory: true)
            .appendingPathComponent("v2", isDirectory: true)
        guard !FileManager.default.fileExists(atPath: nestedDirectory.path) else {
            throw HelperFailure("root must not contain RemoraLink/v2")
        }

        let readyDirectory = rootURL.appendingPathComponent("ready", isDirectory: true)
        let logDirectory = rootURL.appendingPathComponent("sync-logs", isDirectory: true)
        try FileManager.default.createDirectory(at: readyDirectory, withIntermediateDirectories: false)
        try FileManager.default.createDirectory(at: logDirectory, withIntermediateDirectories: false)
        let pauseMarker = rootURL.appendingPathComponent("paused-after-mkdir")
        let releaseMarker = rootURL.appendingPathComponent("release-paused-worker")
        let startMarker = rootURL.appendingPathComponent("start-contenders")

        var children: [Process] = []
        defer {
            _ = FileManager.default.createFile(atPath: releaseMarker.path, contents: Data())
            for child in children where child.isRunning {
                child.terminate()
            }
        }

        let pausedWorker = try launchWorker(
            rootURL: rootURL,
            index: 0,
            environment: [
                "REMORA_LINK_JOURNAL_PAUSE_MARKER": pauseMarker.path,
                "REMORA_LINK_JOURNAL_RELEASE_MARKER": releaseMarker.path,
                "REMORA_LINK_JOURNAL_SYNC_LOG_DIRECTORY": logDirectory.path
            ]
        )
        children.append(pausedWorker)
        try waitForFile(pauseMarker, timeout: 10)

        var contenders: [Process] = []
        for index in 1..<workerCount {
            let contender = try launchWorker(
                rootURL: rootURL,
                index: index,
                environment: [
                    "REMORA_LINK_JOURNAL_READY_DIRECTORY": readyDirectory.path,
                    "REMORA_LINK_JOURNAL_START_MARKER": startMarker.path,
                    "REMORA_LINK_JOURNAL_SYNC_LOG_DIRECTORY": logDirectory.path
                ]
            )
            contenders.append(contender)
            children.append(contender)
        }
        for index in 1..<workerCount {
            try waitForFile(
                readyDirectory.appendingPathComponent("\(index)"),
                timeout: 10
            )
        }
        guard FileManager.default.createFile(atPath: startMarker.path, contents: Data()) else {
            throw HelperFailure("could not release contender barrier")
        }
        try waitForProcesses(contenders, timeout: 15)

        let requiredSyncs = Set([
            rootURL.path,
            rootURL.appendingPathComponent("RemoraLink", isDirectory: true).path,
            nestedDirectory.path
        ])
        for contender in contenders {
            let logURL = logDirectory.appendingPathComponent(
                "\(contender.processIdentifier).log"
            )
            let lines = try String(contentsOf: logURL, encoding: .utf8)
                .split(separator: "\n")
                .map(String.init)
            guard requiredSyncs.isSubset(of: Set(lines)) else {
                throw HelperFailure(
                    "process \(contender.processIdentifier) did not fsync the complete ancestor chain"
                )
            }
        }

        guard FileManager.default.createFile(atPath: releaseMarker.path, contents: Data()) else {
            throw HelperFailure("could not release paused worker")
        }
        try waitForProcesses([pausedWorker], timeout: 15)

        let statuses = children.map(\.terminationStatus)
        guard statuses.filter({ $0 == storedExitCode }).count == 1,
              statuses.filter({ $0 == conflictExitCode }).count == workerCount - 1 else {
            throw HelperFailure("unexpected worker statuses \(statuses)")
        }

        let reader = RemoraLinkJournalStore(
            directoryURL: nestedDirectory,
            durabilityRootURL: rootURL
        )
        guard case .loaded(let snapshot) = reader.load(),
              snapshot.revision == 1,
              String(decoding: snapshot.payload, as: UTF8.self).hasPrefix("first-use-") else {
            throw HelperFailure("winning journal was not readable")
        }
        print("PASS: 24 processes, one stored, 23 conflicts, complete ancestor fsync observed")
    }

    private static func launchWorker(
        rootURL: URL,
        index: Int,
        environment additions: [String: String]
    ) throws -> Process {
        let process = Process()
        process.executableURL = URL(fileURLWithPath: CommandLine.arguments[0]).standardizedFileURL
        process.arguments = ["worker", rootURL.path, "\(index)"]
        process.environment = ProcessInfo.processInfo.environment.merging(additions) { _, new in new }
        try process.run()
        return process
    }

    private static func runWorker(rootURL: URL, index: Int) -> Never {
        let environment = ProcessInfo.processInfo.environment
        if let readyDirectoryPath = environment["REMORA_LINK_JOURNAL_READY_DIRECTORY"],
           let startMarkerPath = environment["REMORA_LINK_JOURNAL_START_MARKER"] {
            let readyURL = URL(fileURLWithPath: readyDirectoryPath, isDirectory: true)
                .appendingPathComponent("\(index)")
            guard FileManager.default.createFile(atPath: readyURL.path, contents: Data()) else {
                Darwin.exit(unavailableExitCode)
            }
            waitForFileWithoutThrowing(URL(fileURLWithPath: startMarkerPath))
        }

        let directoryURL = rootURL
            .appendingPathComponent("RemoraLink", isDirectory: true)
            .appendingPathComponent("v2", isDirectory: true)
        let store = RemoraLinkJournalStore(
            directoryURL: directoryURL,
            durabilityRootURL: rootURL
        )
        let outcome = store.compareAndSwap(
            expectedRevision: nil,
            replacement: .init(
                revision: 1,
                payload: Data("first-use-\(index)".utf8)
            )
        )
        switch outcome {
        case .stored:
            Darwin.exit(storedExitCode)
        case .conflict:
            Darwin.exit(conflictExitCode)
        case .unavailable:
            Darwin.exit(unavailableExitCode)
        case .invalidReplacement:
            Darwin.exit(invalidExitCode)
        }
    }

    private static func waitForFile(_ url: URL, timeout: TimeInterval) throws {
        let deadline = Date(timeIntervalSinceNow: timeout)
        while Date() < deadline {
            if FileManager.default.fileExists(atPath: url.path) { return }
            Darwin.usleep(1_000)
        }
        throw HelperFailure("timed out waiting for \(url.lastPathComponent)")
    }

    private static func waitForFileWithoutThrowing(_ url: URL) {
        while !FileManager.default.fileExists(atPath: url.path) {
            Darwin.usleep(1_000)
        }
    }

    private static func waitForProcesses(_ processes: [Process], timeout: TimeInterval) throws {
        let deadline = Date(timeIntervalSinceNow: timeout)
        while Date() < deadline {
            if processes.allSatisfy({ !$0.isRunning }) { return }
            Darwin.usleep(1_000)
        }
        let runningPIDs = processes.filter(\.isRunning).map(\.processIdentifier)
        throw HelperFailure("timed out waiting for processes \(runningPIDs)")
    }
}

private struct HelperFailure: Error, CustomStringConvertible {
    let description: String

    init(_ description: String) {
        self.description = description
    }
}

#else
import Foundation
import XCTest
@testable import Remora

final class RemoraLinkJournalStoreTests: XCTestCase {
    func testPersistsOneOpaqueWholeBlobWithoutInterpretingItsPayload() throws {
        let directory = try temporaryDirectory()
        let writer = RemoraLinkJournalStore(directoryURL: directory)
        let reader = RemoraLinkJournalStore(directoryURL: directory)
        let opaquePayload = Data([0x00, 0xff, 0x7b, 0x00, 0x5d, 0x80])
        let snapshot = RemoraLinkJournalSnapshot(revision: 1, payload: opaquePayload)

        XCTAssertEqual(writer.load(), .missing)
        XCTAssertEqual(
            writer.compareAndSwap(expectedRevision: nil, replacement: snapshot),
            .stored
        )
        XCTAssertEqual(reader.load(), .loaded(snapshot))
    }

    func testUnavailableStorageFailsClosedWithoutCreatingAFallbackJournal() {
        let store = RemoraLinkJournalStore(directoryURL: nil)

        XCTAssertNil(store.journalFileURL)
        XCTAssertEqual(store.load(), .unavailable)
        XCTAssertEqual(
            store.compareAndSwap(
                expectedRevision: nil,
                replacement: .init(revision: 1, payload: Data("opaque".utf8))
            ),
            .unavailable
        )
    }

    func testFirstWriteCreatesAndPersistsNestedStorageHierarchy() throws {
        let baseDirectory = try temporaryDirectory()
        let nestedDirectory = baseDirectory
            .appendingPathComponent("RemoraLink", isDirectory: true)
            .appendingPathComponent("v2", isDirectory: true)
        let store = RemoraLinkJournalStore(
            directoryURL: nestedDirectory,
            durabilityRootURL: baseDirectory
        )
        let snapshot = RemoraLinkJournalSnapshot(
            revision: 1,
            payload: Data("first-write".utf8)
        )

        XCTAssertFalse(FileManager.default.fileExists(atPath: nestedDirectory.path))
        XCTAssertEqual(
            store.compareAndSwap(expectedRevision: nil, replacement: snapshot),
            .stored
        )
        XCTAssertEqual(store.load(), .loaded(snapshot))
        XCTAssertTrue(FileManager.default.fileExists(atPath: nestedDirectory.path))
    }

    func testConcurrentFirstUseOfMissingHierarchyHasExactlyOneWinner() throws {
        let baseDirectory = try temporaryDirectory()
        let nestedDirectory = baseDirectory
            .appendingPathComponent("RemoraLink", isDirectory: true)
            .appendingPathComponent("v2", isDirectory: true)
        let outcomes = LockedJournalOutcomes()
        let start = DispatchSemaphore(value: 0)
        let group = DispatchGroup()

        XCTAssertFalse(FileManager.default.fileExists(atPath: nestedDirectory.path))
        for index in 0..<24 {
            group.enter()
            DispatchQueue.global(qos: .userInitiated).async {
                start.wait()
                let store = RemoraLinkJournalStore(
                    directoryURL: nestedDirectory,
                    durabilityRootURL: baseDirectory
                )
                outcomes.append(
                    store.compareAndSwap(
                        expectedRevision: nil,
                        replacement: .init(
                            revision: 1,
                            payload: Data("first-use-\(index)".utf8)
                        )
                    )
                )
                group.leave()
            }
        }
        for _ in 0..<24 { start.signal() }
        XCTAssertEqual(group.wait(timeout: .now() + 10), .success)

        let values = outcomes.values
        XCTAssertEqual(values.filter { $0 == .stored }.count, 1)
        XCTAssertEqual(values.filter { $0 == .conflict }.count, 23)
        let reader = RemoraLinkJournalStore(
            directoryURL: nestedDirectory,
            durabilityRootURL: baseDirectory
        )
        guard case .loaded(let persisted) = reader.load() else {
            return XCTFail("expected the winning first-use revision")
        }
        XCTAssertEqual(persisted.revision, 1)
        XCTAssertTrue(String(decoding: persisted.payload, as: UTF8.self).hasPrefix("first-use-"))
    }

    func testConcurrentCompareAndSwapHasExactlyOneWinnerAcrossStoreInstances() throws {
        let directory = try temporaryDirectory()
        let initialStore = RemoraLinkJournalStore(directoryURL: directory)
        XCTAssertEqual(
            initialStore.compareAndSwap(
                expectedRevision: nil,
                replacement: .init(revision: 1, payload: Data("initial".utf8))
            ),
            .stored
        )

        let outcomes = LockedJournalOutcomes()
        let start = DispatchSemaphore(value: 0)
        let group = DispatchGroup()
        for index in 0..<24 {
            group.enter()
            DispatchQueue.global(qos: .userInitiated).async {
                start.wait()
                let store = RemoraLinkJournalStore(directoryURL: directory)
                let outcome = store.compareAndSwap(
                    expectedRevision: 1,
                    replacement: .init(
                        revision: 2,
                        payload: Data("candidate-\(index)".utf8)
                    )
                )
                outcomes.append(outcome)
                group.leave()
            }
        }
        for _ in 0..<24 { start.signal() }
        XCTAssertEqual(group.wait(timeout: .now() + 10), .success)

        let values = outcomes.values
        XCTAssertEqual(values.filter { $0 == .stored }.count, 1)
        XCTAssertEqual(values.filter { $0 == .conflict }.count, 23)
        guard case .loaded(let persisted) = initialStore.load() else {
            return XCTFail("expected the winning revision")
        }
        XCTAssertEqual(persisted.revision, 2)
        XCTAssertTrue(String(decoding: persisted.payload, as: UTF8.self).hasPrefix("candidate-"))
    }

    func testRejectsStaleSkippedZeroOverflowAndOversizedReplacements() throws {
        let directory = try temporaryDirectory()
        let store = RemoraLinkJournalStore(directoryURL: directory)

        XCTAssertEqual(
            store.compareAndSwap(
                expectedRevision: nil,
                replacement: .init(revision: 0, payload: Data())
            ),
            .invalidReplacement
        )
        XCTAssertEqual(
            store.compareAndSwap(
                expectedRevision: nil,
                replacement: .init(revision: 2, payload: Data())
            ),
            .invalidReplacement
        )
        XCTAssertEqual(
            store.compareAndSwap(
                expectedRevision: .max,
                replacement: .init(revision: 1, payload: Data())
            ),
            .invalidReplacement
        )
        XCTAssertEqual(
            store.compareAndSwap(
                expectedRevision: nil,
                replacement: .init(
                    revision: 1,
                    payload: Data(repeating: 0xaa, count: RemoraLinkJournalStore.maximumPayloadBytes + 1)
                )
            ),
            .invalidReplacement
        )

        XCTAssertEqual(
            store.compareAndSwap(
                expectedRevision: nil,
                replacement: .init(revision: 1, payload: Data("one".utf8))
            ),
            .stored
        )
        XCTAssertEqual(
            store.compareAndSwap(
                expectedRevision: nil,
                replacement: .init(revision: 1, payload: Data("stale".utf8))
            ),
            .conflict
        )
        XCTAssertEqual(
            store.compareAndSwap(
                expectedRevision: 1,
                replacement: .init(revision: 3, payload: Data("skip".utf8))
            ),
            .invalidReplacement
        )
    }

    func testDetectsTruncationChecksumCorruptionAndOversizedFiles() throws {
        let directory = try temporaryDirectory()
        let store = RemoraLinkJournalStore(directoryURL: directory)
        XCTAssertEqual(
            store.compareAndSwap(
                expectedRevision: nil,
                replacement: .init(revision: 1, payload: Data("opaque-secret-shaped-text".utf8))
            ),
            .stored
        )

        let journalFileURL = try XCTUnwrap(store.journalFileURL)
        var envelope = try Data(contentsOf: journalFileURL)
        envelope[envelope.index(before: envelope.endIndex)] ^= 0xff
        try envelope.write(to: journalFileURL, options: .atomic)
        XCTAssertEqual(store.load(), .corrupt)
        XCTAssertEqual(
            store.compareAndSwap(
                expectedRevision: 1,
                replacement: .init(revision: 2, payload: Data())
            ),
            .unavailable
        )

        try Data([0x52, 0x4d]).write(to: journalFileURL, options: .atomic)
        XCTAssertEqual(store.load(), .corrupt)

        let oversizedEnvelope = Data(
            repeating: 0,
            count: RemoraLinkJournalStore.maximumPayloadBytes + 128
        )
        try oversizedEnvelope.write(to: journalFileURL, options: .atomic)
        XCTAssertEqual(store.load(), .corrupt)
    }

    func testUnreadableJournalIsUnavailableRatherThanCorrupt() throws {
        let directory = try temporaryDirectory()
        let store = RemoraLinkJournalStore(directoryURL: directory)
        XCTAssertEqual(
            store.compareAndSwap(
                expectedRevision: nil,
                replacement: .init(revision: 1, payload: Data("opaque".utf8))
            ),
            .stored
        )

        let journalFileURL = try XCTUnwrap(store.journalFileURL)
        try FileManager.default.setAttributes(
            [.posixPermissions: 0o000],
            ofItemAtPath: journalFileURL.path
        )

        XCTAssertEqual(store.load(), .unavailable)
    }

    func testSnapshotAndLoadStatusDebugOutputRedactOpaqueBytes() {
        let payload = Data("do-not-log-this-payload".utf8)
        let snapshot = RemoraLinkJournalSnapshot(revision: 9, payload: payload)

        let snapshotDebug = String(reflecting: snapshot)
        let loadDebug = String(reflecting: RemoraLinkJournalLoadStatus.loaded(snapshot))

        XCTAssertFalse(snapshotDebug.contains("do-not-log"))
        XCTAssertFalse(loadDebug.contains("do-not-log"))
        XCTAssertTrue(snapshotDebug.contains("<redacted \(payload.count) bytes>"))
        XCTAssertTrue(loadDebug.contains("<redacted \(payload.count) bytes>"))
    }

    private func temporaryDirectory() throws -> URL {
        let directory = FileManager.default.temporaryDirectory.appendingPathComponent(
            "RemoraLinkJournalStoreTests-\(UUID().uuidString)",
            isDirectory: true
        )
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        addTeardownBlock {
            try? FileManager.default.removeItem(at: directory)
        }
        return directory
    }
}

private final class LockedJournalOutcomes: @unchecked Sendable {
    private let lock = NSLock()
    private var storage: [RemoraLinkJournalWriteOutcome] = []

    var values: [RemoraLinkJournalWriteOutcome] {
        lock.withLock { storage }
    }

    func append(_ outcome: RemoraLinkJournalWriteOutcome) {
        lock.withLock { storage.append(outcome) }
    }
}
#endif
