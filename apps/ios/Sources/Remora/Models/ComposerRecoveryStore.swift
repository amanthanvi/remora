import Foundation
import ImageIO
import Observation
import UIKit

enum ComposerDraftContext: Hashable, Sendable {
    case thread(ThreadKey)
    case project(serverId: String, cwd: String)
}

struct RecoverableComposerDraft: Sendable {
    var text: String
    var image: UIImage?
    var files: [ComposerFileAttachment]
    var skills: [SkillMentionSelection] = []
    var plugins: [PluginMentionSelection] = []

    var isEmpty: Bool { text.isEmpty && image == nil && files.isEmpty }

    // Editor identity is cheap; durable content equality is checked by storage.
    func matchesEditor(_ other: Self) -> Bool {
        text == other.text && image === other.image && files == other.files
            && skills == other.skills && plugins == other.plugins
    }
}

@MainActor
@Observable
final class ComposerRecoveryStore {
    nonisolated static let maximumArchiveBytes = 32 * 1024 * 1024
    nonisolated static let maximumImageDimension = 8_192
    nonisolated static let maximumDecodedImagePixels = 32_000_000

    struct Entry: Identifiable, Sendable {
        let id: UUID
        var context: ComposerDraftContext
        let draft: RecoverableComposerDraft
        var error: String?
    }

    private(set) var entries: [Entry] = []
    private(set) var persistenceError: String?
    @ObservationIgnored private let storage: ComposerRecoveryStorage
    @ObservationIgnored private var revision: UInt64 = 0

    init(directory: URL? = nil, beforeWrite: (@Sendable () throws -> Void)? = nil) {
        storage = ComposerRecoveryStorage(directory: directory, beforeWrite: beforeWrite)
    }

    func load() async throws {
        try await perform { try $0.loadIfNeeded() }
    }

    func begin(_ draft: RecoverableComposerDraft, in context: ComposerDraftContext) async throws -> UUID {
        try await perform { try $0.begin(draft, in: context) }
    }

    func finish(_ id: UUID, error: String? = nil) async throws {
        try await perform { try $0.finish(id, error: error) }
    }

    func saved(in context: ComposerDraftContext) -> [Entry] {
        entries.filter { $0.context == context && $0.error != nil }
    }

    func discard(_ id: UUID, in context: ComposerDraftContext) async throws {
        try await perform { try $0.discard(id, in: context) }
    }

    func move(_ id: UUID, to context: ComposerDraftContext) async throws {
        try await perform { try $0.move(id, to: context) }
    }

    func preserve(_ draft: RecoverableComposerDraft, in context: ComposerDraftContext) async throws {
        try await perform { try $0.preserve(draft, in: context) }
    }

    func recover(_ id: UUID, in context: ComposerDraftContext,
                 preserving current: RecoverableComposerDraft) async throws -> RecoverableComposerDraft? {
        try await perform { try $0.recover(id, in: context, preserving: current) }
    }

    private func perform<T: Sendable>(
        _ operation: @Sendable (isolated ComposerRecoveryStorage) throws -> T
    ) async throws -> T {
        let update = await storage.perform(operation)
        // Awaiting callers can resume out of order; never publish an older snapshot.
        if update.revision > revision {
            revision = update.revision
            entries = update.entries
            persistenceError = update.error
        }
        return try update.result.get()
    }
}

// Transactions contain no suspension points. Encoding, reads, atomic writes,
// and fsync all run on this actor, never on the UI executor.
private actor ComposerRecoveryStorage {
    typealias Entry = ComposerRecoveryStore.Entry
    private static let maximumArchiveBytes = ComposerRecoveryStore.maximumArchiveBytes
    private static let maximumImageDimension = ComposerRecoveryStore.maximumImageDimension
    private static let maximumDecodedImagePixels = ComposerRecoveryStore.maximumDecodedImagePixels
    private var entries: [Entry] = []
    private var persistenceError: String?
    private let directory: URL?
    private let beforeWrite: (@Sendable () throws -> Void)?
    private var didLoad = false
    private var recoveredEntries: [ComposerDraftContext: UUID] = [:]
    private var revision: UInt64 = 0

    init(directory: URL?, beforeWrite: (@Sendable () throws -> Void)?) {
        self.directory = directory
        self.beforeWrite = beforeWrite
    }

    private var resolvedDirectory: URL? {
        directory ?? FileManager.default.urls(
            for: .applicationSupportDirectory, in: .userDomainMask
        ).first?.appendingPathComponent("RemoraComposerRecovery", isDirectory: true)
    }

    func perform<T: Sendable>(
        _ operation: @Sendable (isolated ComposerRecoveryStorage) throws -> T
    ) -> (result: Result<T, Error>, entries: [Entry], error: String?, revision: UInt64) {
        let result = Result { try operation(self) }
        revision += 1
        return (result, entries, persistenceError, revision)
    }

    func begin(_ draft: RecoverableComposerDraft, in context: ComposerDraftContext) throws -> UUID {
        try loadIfNeeded()
        let id = UUID()
        var next = entries
        if let recoveredId = recoveredEntries[context],
           let previous = next.first(where: { $0.id == recoveredId }),
           try StoredDraft(previous.draft) == StoredDraft(draft) {
            next.removeAll { $0.id == recoveredId }
        }
        next.append(Entry(id: id, context: context, draft: draft))
        try commit(next)
        recoveredEntries.removeValue(forKey: context)
        return id
    }

    func finish(_ id: UUID, error: String? = nil) throws {
        try loadIfNeeded()
        var next = entries
        guard let index = next.firstIndex(where: { $0.id == id }) else { return }
        if let error {
            next[index].error = error
        } else {
            next.remove(at: index)
        }
        do {
            try commit(next)
        } catch let persistenceFailure {
            // The operation has ended even when its terminal state cannot be
            // written. Never hide this entry when another write clears the error.
            entries[index].error = error ?? Self.unconfirmedMessage
            throw persistenceFailure
        }
    }

    func discard(_ id: UUID, in context: ComposerDraftContext) throws {
        try loadIfNeeded()
        var next = entries
        guard let index = next.firstIndex(where: {
            $0.id == id && $0.context == context && $0.error != nil
        }) else { return }
        next.remove(at: index)
        try commit(next)
        if recoveredEntries[context] == id {
            recoveredEntries.removeValue(forKey: context)
        }
    }

    func move(_ id: UUID, to context: ComposerDraftContext) throws {
        try loadIfNeeded()
        var next = entries
        guard let index = next.firstIndex(where: { $0.id == id }) else { return }
        next[index].context = context
        try commit(next)
    }

    func preserve(_ draft: RecoverableComposerDraft, in context: ComposerDraftContext) throws {
        try loadIfNeeded()
        guard !draft.isEmpty else { return }
        var next = entries
        next.append(Entry(id: UUID(), context: context, draft: draft,
                          error: "Draft saved before navigation or recovery."))
        try commit(next)
    }

    func recover(_ id: UUID, in context: ComposerDraftContext,
                 preserving current: RecoverableComposerDraft) throws -> RecoverableComposerDraft? {
        try loadIfNeeded()
        guard let index = entries.firstIndex(where: {
            $0.id == id && $0.context == context && $0.error != nil
        }) else { return nil }
        let recovered = entries[index].draft
        // The editor is volatile. Keep its durable source until an equivalent
        // submission replaces it, and save newer edits before switching drafts.
        if !current.isEmpty {
            if try StoredDraft(current) != StoredDraft(recovered) {
                try preserve(current, in: context)
            }
        }
        recoveredEntries[context] = id
        return recovered
    }

    private nonisolated static let storageErrorMessage = "Saved drafts could not be accessed. Your current draft has not been cleared."
    private nonisolated static let unconfirmedMessage = "Submission not confirmed. Check the conversation before sending again."

    private enum StorageError: LocalizedError {
        case unavailable, unsupportedVersion, invalidImage, archiveTooLarge
        var errorDescription: String? { ComposerRecoveryStorage.storageErrorMessage }
    }

    private struct Archive: Codable {
        let version: Int
        let entries: [StoredEntry]
    }

    private enum StoredContext: Codable {
        case thread(serverId: String, threadId: String)
        case project(serverId: String, cwd: String)

        init(_ context: ComposerDraftContext) {
            switch context {
            case let .thread(key): self = .thread(serverId: key.serverId, threadId: key.threadId)
            case let .project(serverId, cwd): self = .project(serverId: serverId, cwd: cwd)
            }
        }

        var value: ComposerDraftContext {
            switch self {
            case let .thread(serverId, threadId): return .thread(ThreadKey(serverId: serverId, threadId: threadId))
            case let .project(serverId, cwd): return .project(serverId: serverId, cwd: cwd)
            }
        }
    }

    private struct StoredEntry: Codable {
        let id: UUID
        let context: StoredContext
        let draft: StoredDraft
    }

    private struct StoredDraft: Codable, Equatable {
        struct File: Codable, Equatable { let label: String; let path: String }
        struct Skill: Codable, Equatable { let name: String; let path: String }
        struct Plugin: Codable, Equatable { let name: String; let marketplace: String; let displayName: String? }
        let text: String
        let image: Data?
        let imageScale: Double?
        let imageOrientation: Int?
        let files: [File]
        let skills: [Skill]
        let plugins: [Plugin]

        init(_ draft: RecoverableComposerDraft) throws {
            text = draft.text
            guard text.utf8.count <= ComposerRecoveryStore.maximumArchiveBytes else {
                throw StorageError.archiveTooLarge
            }
            if let image = draft.image {
                _ = try ComposerRecoveryStorage.imagePixels(
                    width: Double(image.size.width * image.scale),
                    height: Double(image.size.height * image.scale)
                )
            }
            image = draft.image?.pngData()
            imageScale = draft.image.map { Double($0.scale) }
            imageOrientation = draft.image?.imageOrientation.rawValue
            if draft.image != nil && image == nil { throw StorageError.invalidImage }
            guard (image?.count ?? 0) <= ComposerRecoveryStore.maximumArchiveBytes else {
                throw StorageError.archiveTooLarge
            }
            files = draft.files.map { File(label: $0.label, path: $0.path) }
            skills = draft.skills.map { Skill(name: $0.name, path: $0.path) }
            plugins = draft.plugins.map { Plugin(name: $0.name, marketplace: $0.marketplace, displayName: $0.displayName) }
        }

        func value() throws -> RecoverableComposerDraft {
            var decodedImage: UIImage?
            if let image {
                let (source, _) = try ComposerRecoveryStorage.imageSource(image)
                guard let imageScale, imageScale.isFinite, imageScale > 0,
                      let imageOrientation,
                      let orientation = UIImage.Orientation(rawValue: imageOrientation),
                      let raster = CGImageSourceCreateImageAtIndex(source, 0, nil) else {
                    throw StorageError.invalidImage
                }
                decodedImage = UIImage(cgImage: raster, scale: CGFloat(imageScale), orientation: orientation)
            }
            return RecoverableComposerDraft(
                text: text, image: decodedImage,
                files: files.map { ComposerFileAttachment(label: $0.label, path: $0.path) },
                skills: skills.map { SkillMentionSelection(name: $0.name, path: $0.path) },
                plugins: plugins.map { PluginMentionSelection(name: $0.name, marketplace: $0.marketplace, displayName: $0.displayName) }
            )
        }
    }

    private nonisolated static func imagePixels(width: Double, height: Double) throws -> Int {
        guard width.isFinite, height.isFinite, width > 0, height > 0,
              width <= Double(maximumImageDimension), height <= Double(maximumImageDimension),
              width * height <= Double(maximumDecodedImagePixels) else {
            throw StorageError.invalidImage
        }
        return Int((width * height).rounded(.up))
    }

    private nonisolated static func imageSource(_ data: Data) throws -> (CGImageSource, Int) {
        let options = [kCGImageSourceShouldCache: false] as CFDictionary
        guard let source = CGImageSourceCreateWithData(data as CFData, options),
              let properties = CGImageSourceCopyPropertiesAtIndex(source, 0, options) as? [CFString: Any],
              let width = properties[kCGImagePropertyPixelWidth] as? NSNumber,
              let height = properties[kCGImagePropertyPixelHeight] as? NSNumber else {
            throw StorageError.invalidImage
        }
        return (source, try imagePixels(width: width.doubleValue, height: height.doubleValue))
    }

    private static func validateImages(_ archive: Archive) throws {
        var remainingPixels = maximumDecodedImagePixels
        for entry in archive.entries {
            if let image = entry.draft.image {
                let (_, pixels) = try imageSource(image)
                guard pixels <= remainingPixels else { throw StorageError.invalidImage }
                remainingPixels -= pixels
            }
        }
    }

    func loadIfNeeded() throws {
        guard !didLoad else { return }
        do {
            guard let directory = resolvedDirectory else { throw StorageError.unavailable }
            let file = directory.appendingPathComponent("drafts-v1.json")
            let data: Data
            do {
                let values = try file.resourceValues(forKeys: [.fileSizeKey, .isRegularFileKey])
                guard values.isRegularFile == true, let size = values.fileSize,
                      size <= Self.maximumArchiveBytes else { throw StorageError.archiveTooLarge }
                let handle = try FileHandle(forReadingFrom: file)
                defer { try? handle.close() }
                data = try handle.read(upToCount: Self.maximumArchiveBytes + 1) ?? Data()
                guard data.count <= Self.maximumArchiveBytes else { throw StorageError.archiveTooLarge }
            }
            catch let error as CocoaError where error.code == .fileReadNoSuchFile {
                didLoad = true
                persistenceError = nil
                return
            }
            let archive = try JSONDecoder().decode(Archive.self, from: data)
            guard archive.version == 1 else { throw StorageError.unsupportedVersion }
            try Self.validateImages(archive)
            let restored = try archive.entries.map {
                Entry(id: $0.id, context: $0.context.value, draft: try $0.draft.value(),
                      error: Self.unconfirmedMessage)
            }
            guard Set(restored.map(\.id)).count == restored.count else { throw StorageError.unavailable }
            entries = restored
            didLoad = true
            persistenceError = nil
            removeAbandonedTemporaryFiles(in: directory)
        } catch {
            persistenceError = Self.storageErrorMessage
            throw StorageError.unavailable
        }
    }

    private func removeAbandonedTemporaryFiles(in directory: URL) {
        let manager = FileManager.default
        let keys: Set<URLResourceKey> = [.isRegularFileKey, .isSymbolicLinkKey]
        guard let files = try? manager.contentsOfDirectory(at: directory,
                                                          includingPropertiesForKeys: Array(keys)) else { return }
        for file in files {
            let name = file.lastPathComponent
            guard name.hasPrefix(".drafts-"), name.hasSuffix(".tmp"),
                  UUID(uuidString: String(name.dropFirst(8).dropLast(4))) != nil,
                  let values = try? file.resourceValues(forKeys: keys),
                  values.isRegularFile == true, values.isSymbolicLink == false else { continue }
            // A validated canonical archive is authoritative; cleanup failure
            // must not prevent access to its saved drafts.
            try? manager.removeItem(at: file)
        }
    }

    private func commit(_ next: [Entry]) throws {
        do {
            try beforeWrite?()
            guard var directory = resolvedDirectory else { throw StorageError.unavailable }
            let manager = FileManager.default
            try manager.createDirectory(at: directory, withIntermediateDirectories: true,
                                        attributes: [.protectionKey: FileProtectionType.complete, .posixPermissions: 0o700])
            try manager.setAttributes([.protectionKey: FileProtectionType.complete, .posixPermissions: 0o700],
                                      ofItemAtPath: directory.path)
            var values = URLResourceValues()
            values.isExcludedFromBackup = true
            try directory.setResourceValues(values)
            let archive = Archive(version: 1, entries: try next.map {
                StoredEntry(id: $0.id, context: StoredContext($0.context), draft: try StoredDraft($0.draft))
            })
            try Self.validateImages(archive)
            let data = try JSONEncoder().encode(archive)
            guard data.count <= Self.maximumArchiveBytes else { throw StorageError.archiveTooLarge }
            var temporary = directory.appendingPathComponent(".drafts-\(UUID().uuidString).tmp")
            defer { try? manager.removeItem(at: temporary) }
            try data.write(to: temporary, options: [.withoutOverwriting, .completeFileProtection])
            try manager.setAttributes([.posixPermissions: 0o600], ofItemAtPath: temporary.path)
            try temporary.setResourceValues(values)
            let handle = try FileHandle(forWritingTo: temporary)
            defer { try? handle.close() }
            try handle.synchronize()
            let file = directory.appendingPathComponent("drafts-v1.json")
            if manager.fileExists(atPath: file.path) {
                _ = try manager.replaceItemAt(file, withItemAt: temporary, options: .usingNewMetadataOnly)
            } else {
                try manager.moveItem(at: temporary, to: file)
            }
            entries = next
            persistenceError = nil
        } catch {
            persistenceError = Self.storageErrorMessage
            throw StorageError.unavailable
        }
    }
}
