import CryptoKit
import Darwin
import Dispatch
import Foundation

/// One whole, Rust-owned Remora Link v2 journal snapshot.
///
/// Native code persists the payload atomically but deliberately does not parse
/// it. The outer revision exists only to provide compare-and-swap semantics
/// across foreground, background, and future extension processes.
struct RemoraLinkJournalSnapshot: Equatable, Sendable, CustomStringConvertible,
    CustomDebugStringConvertible
{
    let revision: UInt64
    let payload: Data

    var description: String {
        "RemoraLinkJournalSnapshot(revision: \(revision), payload: <redacted \(payload.count) bytes>)"
    }

    var debugDescription: String { description }
}

enum RemoraLinkJournalLoadStatus: Equatable, Sendable, CustomStringConvertible,
    CustomDebugStringConvertible
{
    case missing
    case loaded(RemoraLinkJournalSnapshot)
    case corrupt
    case unavailable

    var description: String {
        switch self {
        case .missing:
            return "RemoraLinkJournalLoadStatus.missing"
        case .loaded(let snapshot):
            return "RemoraLinkJournalLoadStatus.loaded(\(snapshot))"
        case .corrupt:
            return "RemoraLinkJournalLoadStatus.corrupt"
        case .unavailable:
            return "RemoraLinkJournalLoadStatus.unavailable"
        }
    }

    var debugDescription: String { description }
}

enum RemoraLinkJournalWriteOutcome: Equatable, Sendable {
    case stored
    case conflict
    case unavailable
    case invalidReplacement
}

/// Durable, whole-blob Remora Link v2 journal storage.
///
/// A process-local lock and a filesystem lock make the compare-and-swap one
/// atomic operation even when multiple store instances or app processes race.
/// The envelope protects only the native revision and opaque bytes; all Rust
/// payload validation remains in Rust.
final class RemoraLinkJournalStore: @unchecked Sendable {
    static let shared = RemoraLinkJournalStore()

    static let maximumPayloadBytes = 2 * 1_024 * 1_024

    private static let appGroup = "group.com.remora.app"
    private static let directoryName = "RemoraLink/v2"
    private static let journalFileName = "pairing-journal.bin"
    private static let lockFileName = "pairing-journal.lock"
    private static let envelopeMagic = Data([0x52, 0x4d, 0x4c, 0x4a, 0x76, 0x32, 0x00, 0x00])
    private static let envelopeVersion: UInt32 = 1
    private static let digestByteCount = SHA256.Digest.byteCount
    private static let fixedEnvelopeByteCount = envelopeMagic.count
        + MemoryLayout<UInt32>.size
        + MemoryLayout<UInt64>.size
        + MemoryLayout<UInt32>.size
        + digestByteCount
    private static let lockWaitInterval: TimeInterval = 1
    private static let lockWaitNanoseconds: UInt64 = 1_000_000_000
    private static let lockRetryMicroseconds: useconds_t = 10_000
    private static let processLock = NSLock()

    let journalFileURL: URL?
    private let lockFileURL: URL?
    private let durabilityRootURL: URL?
    private let fileManager: FileManager
    private let excludeFromBackup: Bool

    convenience init() {
        let fileManager = FileManager.default
        let containerURL = fileManager.containerURL(
            forSecurityApplicationGroupIdentifier: Self.appGroup
        )
        self.init(
            directoryURL: containerURL?.appendingPathComponent(Self.directoryName, isDirectory: true),
            durabilityRootURL: containerURL,
            fileManager: fileManager
        )
    }

    init(
        directoryURL: URL?,
        durabilityRootURL: URL? = nil,
        excludeFromBackup: Bool = false,
        fileManager: FileManager = .default
    ) {
        self.fileManager = fileManager
        self.excludeFromBackup = excludeFromBackup
        journalFileURL = directoryURL?.appendingPathComponent(Self.journalFileName)
        lockFileURL = directoryURL?.appendingPathComponent(Self.lockFileName)
        self.durabilityRootURL = durabilityRootURL ?? directoryURL
    }

    func load() -> RemoraLinkJournalLoadStatus {
        withExclusiveStorageLock {
            loadWhileLocked()
        } ?? .unavailable
    }

    func compareAndSwap(
        expectedRevision: UInt64?,
        replacement: RemoraLinkJournalSnapshot
    ) -> RemoraLinkJournalWriteOutcome {
        guard Self.validReplacement(
            expectedRevision: expectedRevision,
            replacement: replacement
        ) else {
            return .invalidReplacement
        }

        return withExclusiveStorageLock {
            let current = loadWhileLocked()
            switch current {
            case .missing where expectedRevision == nil:
                break
            case .loaded(let snapshot) where snapshot.revision == expectedRevision:
                break
            case .missing, .loaded:
                return .conflict
            case .corrupt, .unavailable:
                return .unavailable
            }

            guard let envelope = Self.encode(replacement), writeAtomically(envelope) else {
                return .unavailable
            }
            return .stored
        } ?? .unavailable
    }

    /// Removes all durable Remora Link lifecycle state before the 1.6 runtime
    /// can observe it. This is called only during process-start cutover.
    func discardForSecurityCutover() -> Bool {
        guard Self.processLock.lock(
            before: Date(timeIntervalSinceNow: Self.lockWaitInterval)
        ) else {
            return false
        }
        defer { Self.processLock.unlock() }
        guard let directoryURL = journalFileURL?.deletingLastPathComponent() else {
            return false
        }
        guard fileManager.fileExists(atPath: directoryURL.path) else {
            return true
        }
        do {
            try fileManager.removeItem(at: directoryURL)
            return true
        } catch {
            return false
        }
    }

    private static func validReplacement(
        expectedRevision: UInt64?,
        replacement: RemoraLinkJournalSnapshot
    ) -> Bool {
        guard replacement.payload.count <= maximumPayloadBytes else { return false }
        let requiredRevision: UInt64
        if let expectedRevision {
            let (next, overflow) = expectedRevision.addingReportingOverflow(1)
            guard !overflow else { return false }
            requiredRevision = next
        } else {
            requiredRevision = 1
        }
        return replacement.revision == requiredRevision
    }

    private func withExclusiveStorageLock<T>(_ body: () -> T) -> T? {
        guard Self.processLock.lock(
            before: Date(timeIntervalSinceNow: Self.lockWaitInterval)
        ) else {
            return nil
        }
        defer { Self.processLock.unlock() }

        guard let journalFileURL, let lockFileURL else { return nil }
        let directoryURL = journalFileURL.deletingLastPathComponent()
        guard prepareStorageDirectory(directoryURL) else { return nil }

        let descriptor = lockFileURL.withUnsafeFileSystemRepresentation { path -> Int32 in
            guard let path else { return -1 }
            return Darwin.open(path, O_CREAT | O_RDWR | O_NOFOLLOW, S_IRUSR | S_IWUSR)
        }
        guard descriptor >= 0 else { return nil }
        defer { Darwin.close(descriptor) }

        guard setFileLock(
            descriptor: descriptor,
            type: F_WRLCK,
            waitNanoseconds: Self.lockWaitNanoseconds,
            retryMicroseconds: Self.lockRetryMicroseconds
        ) else {
            return nil
        }
        defer {
            _ = setFileLock(
                descriptor: descriptor,
                type: F_UNLCK,
                waitNanoseconds: nil,
                retryMicroseconds: 0
            )
        }
        return body()
    }

    private func prepareStorageDirectory(_ directoryURL: URL) -> Bool {
        guard let durabilityRootURL,
              let durabilityHierarchy = Self.directoryHierarchy(
                  from: durabilityRootURL,
                  through: directoryURL
              ) else {
            return false
        }

        var rootIsDirectory: ObjCBool = false
        guard fileManager.fileExists(
            atPath: durabilityRootURL.path,
            isDirectory: &rootIsDirectory
        ), rootIsDirectory.boolValue else {
            return false
        }

        var missingDirectories: [URL] = []
        var cursor = directoryURL
        while cursor.standardizedFileURL != durabilityRootURL.standardizedFileURL,
              !fileManager.fileExists(atPath: cursor.path) {
            missingDirectories.append(cursor)
            let parent = cursor.deletingLastPathComponent()
            guard parent.path != cursor.path else { return false }
            cursor = parent
        }

        do {
            try fileManager.createDirectory(
                at: directoryURL,
                withIntermediateDirectories: true,
                attributes: [.posixPermissions: 0o700]
            )
            for createdDirectory in missingDirectories {
                try fileManager.setAttributes(
                    [.posixPermissions: 0o700],
                    ofItemAtPath: createdDirectory.path
                )
            }
            if excludeFromBackup {
                var directory = directoryURL
                var resources = URLResourceValues()
                resources.isExcludedFromBackup = true
                try directory.setResourceValues(resources)
                guard try directory.resourceValues(forKeys: [.isExcludedFromBackupKey])
                    .isExcludedFromBackup == true else { return false }
            }
        } catch {
            return false
        }

        #if REMORA_LINK_JOURNAL_PROCESS_HELPER
        pauseAfterDirectoryCreationForProcessTestIfRequested()
        #endif

        // Synchronize the complete chain unconditionally. If another process
        // won the create race but was suspended before syncing a parent, this
        // process finishes that durability work before it can acquire the CAS
        // lock or report a stored journal revision.
        for durableDirectory in durabilityHierarchy {
            guard syncDirectory(durableDirectory) else { return false }
        }
        return true
    }

    private static func directoryHierarchy(from rootURL: URL, through leafURL: URL) -> [URL]? {
        let rootURL = rootURL.standardizedFileURL
        let leafURL = leafURL.standardizedFileURL
        let rootComponents = rootURL.pathComponents
        let leafComponents = leafURL.pathComponents
        guard leafComponents.count >= rootComponents.count,
              Array(leafComponents.prefix(rootComponents.count)) == rootComponents else {
            return nil
        }

        var hierarchy = [rootURL]
        var cursor = rootURL
        for component in leafComponents.dropFirst(rootComponents.count) {
            cursor.appendPathComponent(component, isDirectory: true)
            hierarchy.append(cursor)
        }
        return hierarchy
    }

    private func loadWhileLocked() -> RemoraLinkJournalLoadStatus {
        guard let journalFileURL else { return .unavailable }

        let attributes: [FileAttributeKey: Any]
        do {
            attributes = try fileManager.attributesOfItem(atPath: journalFileURL.path)
        } catch let error as CocoaError where
            error.code == .fileNoSuchFile || error.code == .fileReadNoSuchFile
        {
            return .missing
        } catch {
            return .unavailable
        }
        guard attributes[.type] as? FileAttributeType == .typeRegular,
              let fileSize = attributes[.size] as? NSNumber,
              fileSize.uint64Value <= UInt64(Self.fixedEnvelopeByteCount + Self.maximumPayloadBytes) else {
            return .corrupt
        }

        let data: Data
        do {
            data = try Data(contentsOf: journalFileURL, options: .uncached)
        } catch {
            return .unavailable
        }
        return Self.decode(data).map(RemoraLinkJournalLoadStatus.loaded) ?? .corrupt
    }

    private static func encode(_ snapshot: RemoraLinkJournalSnapshot) -> Data? {
        guard snapshot.revision > 0,
              snapshot.payload.count <= maximumPayloadBytes,
              let payloadLength = UInt32(exactly: snapshot.payload.count) else {
            return nil
        }

        var authenticated = Data(capacity: fixedEnvelopeByteCount - digestByteCount + snapshot.payload.count)
        authenticated.append(envelopeMagic)
        authenticated.appendBigEndian(envelopeVersion)
        authenticated.appendBigEndian(snapshot.revision)
        authenticated.appendBigEndian(payloadLength)
        authenticated.append(snapshot.payload)

        var envelope = authenticated
        envelope.append(contentsOf: SHA256.hash(data: authenticated))
        return envelope
    }

    private static func decode(_ envelope: Data) -> RemoraLinkJournalSnapshot? {
        guard envelope.count >= fixedEnvelopeByteCount,
              envelope.count <= fixedEnvelopeByteCount + maximumPayloadBytes else {
            return nil
        }

        let authenticatedEnd = envelope.count - digestByteCount
        let authenticated = envelope[..<authenticatedEnd]
        let storedDigest = envelope[authenticatedEnd...]
        let calculatedDigest = Data(SHA256.hash(data: authenticated))
        guard constantTimeEqual(storedDigest, calculatedDigest) else { return nil }

        var cursor = 0
        guard envelope.readBytes(count: envelopeMagic.count, cursor: &cursor) == envelopeMagic,
              envelope.readBigEndian(UInt32.self, cursor: &cursor) == envelopeVersion,
              let revision = envelope.readBigEndian(UInt64.self, cursor: &cursor),
              revision > 0,
              let payloadLength = envelope.readBigEndian(UInt32.self, cursor: &cursor),
              payloadLength <= UInt32(maximumPayloadBytes),
              cursor + Int(payloadLength) == authenticatedEnd,
              let payload = envelope.readBytes(count: Int(payloadLength), cursor: &cursor) else {
            return nil
        }
        return RemoraLinkJournalSnapshot(revision: revision, payload: payload)
    }

    private func writeAtomically(_ envelope: Data) -> Bool {
        guard let journalFileURL else { return false }
        let directoryURL = journalFileURL.deletingLastPathComponent()
        let temporaryURL = directoryURL.appendingPathComponent(
            ".\(Self.journalFileName).\(UUID().uuidString).tmp"
        )
        let descriptor = temporaryURL.withUnsafeFileSystemRepresentation { path -> Int32 in
            guard let path else { return -1 }
            return Darwin.open(
                path,
                O_WRONLY | O_CREAT | O_EXCL | O_NOFOLLOW,
                S_IRUSR | S_IWUSR
            )
        }
        guard descriptor >= 0 else { return false }

        var succeeded = false
        defer {
            Darwin.close(descriptor)
            if !succeeded {
                try? fileManager.removeItem(at: temporaryURL)
            }
        }

        guard envelope.withUnsafeBytes({ rawBuffer in
            writeAll(descriptor: descriptor, buffer: rawBuffer)
        }), Darwin.fsync(descriptor) == 0 else {
            return false
        }

        #if os(iOS) || targetEnvironment(macCatalyst)
            do {
                try fileManager.setAttributes(
                    [.protectionKey: FileProtectionType.completeUntilFirstUserAuthentication],
                    ofItemAtPath: temporaryURL.path
                )
            } catch {
                return false
            }
        #endif
        guard Darwin.fsync(descriptor) == 0 else { return false }

        let renamed = temporaryURL.withUnsafeFileSystemRepresentation { source in
            journalFileURL.withUnsafeFileSystemRepresentation { destination in
                guard let source, let destination else { return false }
                return Darwin.rename(source, destination) == 0
            }
        }
        guard renamed else { return false }

        guard syncDirectory(directoryURL) else { return false }

        succeeded = true
        return true
    }

    #if REMORA_LINK_JOURNAL_PROCESS_HELPER
        private func pauseAfterDirectoryCreationForProcessTestIfRequested() {
            let environment = ProcessInfo.processInfo.environment
            guard let pauseMarkerPath = environment["REMORA_LINK_JOURNAL_PAUSE_MARKER"],
                  let releaseMarkerPath = environment["REMORA_LINK_JOURNAL_RELEASE_MARKER"] else {
                return
            }

            guard fileManager.createFile(atPath: pauseMarkerPath, contents: Data()) else { return }
            while !fileManager.fileExists(atPath: releaseMarkerPath) {
                Darwin.usleep(1_000)
            }
        }
    #endif
}

private func syncDirectory(_ directoryURL: URL) -> Bool {
    let descriptor = directoryURL.withUnsafeFileSystemRepresentation { path -> Int32 in
        guard let path else { return -1 }
        return Darwin.open(path, O_RDONLY | O_DIRECTORY | O_NOFOLLOW)
    }
    guard descriptor >= 0 else { return false }
    defer { Darwin.close(descriptor) }
    let succeeded = Darwin.fsync(descriptor) == 0
    #if REMORA_LINK_JOURNAL_PROCESS_HELPER
        if succeeded {
            recordDirectorySyncForProcessTest(directoryURL)
        }
    #endif
    return succeeded
}

#if REMORA_LINK_JOURNAL_PROCESS_HELPER
    private func recordDirectorySyncForProcessTest(_ directoryURL: URL) {
        guard let logDirectoryPath = ProcessInfo.processInfo.environment[
            "REMORA_LINK_JOURNAL_SYNC_LOG_DIRECTORY"
        ] else {
            return
        }
        let logURL = URL(fileURLWithPath: logDirectoryPath, isDirectory: true)
            .appendingPathComponent("\(Darwin.getpid()).log")
        let descriptor = logURL.withUnsafeFileSystemRepresentation { path -> Int32 in
            guard let path else { return -1 }
            return Darwin.open(
                path,
                O_WRONLY | O_CREAT | O_APPEND | O_NOFOLLOW,
                S_IRUSR | S_IWUSR
            )
        }
        guard descriptor >= 0 else { return }
        defer { Darwin.close(descriptor) }
        let line = Data((directoryURL.standardizedFileURL.path + "\n").utf8)
        _ = line.withUnsafeBytes { writeAll(descriptor: descriptor, buffer: $0) }
    }
#endif

private extension Data {
    mutating func appendBigEndian<T: FixedWidthInteger>(_ value: T) {
        var value = value.bigEndian
        Swift.withUnsafeBytes(of: &value) { append(contentsOf: $0) }
    }

    func readBigEndian<T: FixedWidthInteger>(
        _ type: T.Type,
        cursor: inout Int
    ) -> T? {
        guard let bytes = readBytes(count: MemoryLayout<T>.size, cursor: &cursor) else {
            return nil
        }
        return bytes.reduce(into: T.zero) { value, byte in
            value = (value << 8) | T(byte)
        }
    }

    func readBytes(count: Int, cursor: inout Int) -> Data? {
        guard count >= 0, cursor >= 0, cursor <= self.count - count else { return nil }
        defer { cursor += count }
        return self[cursor..<(cursor + count)]
    }
}

private func constantTimeEqual<C1: Collection, C2: Collection>(
    _ lhs: C1,
    _ rhs: C2
) -> Bool where C1.Element == UInt8, C2.Element == UInt8 {
    guard lhs.count == rhs.count else { return false }
    var difference: UInt8 = 0
    for (left, right) in zip(lhs, rhs) {
        difference |= left ^ right
    }
    return difference == 0
}

private func writeAll(descriptor: Int32, buffer: UnsafeRawBufferPointer) -> Bool {
    var offset = 0
    while offset < buffer.count {
        guard let baseAddress = buffer.baseAddress else { return buffer.isEmpty }
        let written = Darwin.write(
            descriptor,
            baseAddress.advanced(by: offset),
            buffer.count - offset
        )
        if written < 0 {
            if errno == EINTR { continue }
            return false
        }
        guard written > 0 else { return false }
        offset += written
    }
    return true
}

private func setFileLock(
    descriptor: Int32,
    type: Int32,
    waitNanoseconds: UInt64?,
    retryMicroseconds: useconds_t
) -> Bool {
    var lock = flock()
    lock.l_type = Int16(type)
    lock.l_whence = Int16(SEEK_SET)
    let deadline = waitNanoseconds.map { timeout -> UInt64 in
        let (value, overflow) = DispatchTime.now().uptimeNanoseconds.addingReportingOverflow(timeout)
        return overflow ? UInt64.max : value
    }

    while Darwin.fcntl(descriptor, F_SETLK, &lock) != 0 {
        if errno == EINTR { continue }
        guard (errno == EACCES || errno == EAGAIN),
              let deadline,
              DispatchTime.now().uptimeNanoseconds < deadline else {
            return false
        }
        Darwin.usleep(retryMicroseconds)
    }
    return true
}
