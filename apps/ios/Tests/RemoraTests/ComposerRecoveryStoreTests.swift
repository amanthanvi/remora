import XCTest
import UIKit
@testable import Remora

final class ComposerRecoveryStoreTests: XCTestCase {
    @MainActor
    private func loadedStore(directory: URL) async -> ComposerRecoveryStore {
        let store = ComposerRecoveryStore(directory: directory)
        try? await store.load()
        return store
    }

    @MainActor
    private func assertThrowsError<T>(
        _ expression: @autoclosure () async throws -> T,
        file: StaticString = #filePath, line: UInt = #line
    ) async {
        do {
            _ = try await expression()
            XCTFail("Expected persistence to fail", file: file, line: line)
        } catch {}
    }

    private func temporaryDirectory() throws -> URL {
        let directory = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        addTeardownBlock { try? FileManager.default.removeItem(at: directory) }
        return directory
    }

    @MainActor
    private func image() -> UIImage {
        UIGraphicsImageRenderer(size: CGSize(width: 3, height: 4)).image { context in
            UIColor.red.setFill()
            context.fill(CGRect(x: 0, y: 0, width: 3, height: 4))
        }
    }

    @MainActor
    func testFailedSubmissionPreservesNewerDraftAndAllAttachments() async throws {
        let directory = try temporaryDirectory()
        let store = await loadedStore(directory: directory)
        let context = ComposerDraftContext.thread(ThreadKey(serverId: "server", threadId: "thread"))
        let image = image()
        let original = RecoverableComposerDraft(
            text: "original", image: image,
            files: [ComposerFileAttachment(label: "notes.txt", path: "/project/notes.txt")],
            skills: [SkillMentionSelection(name: "skill", path: "/skill")],
            plugins: [PluginMentionSelection(name: "plugin", marketplace: "local", displayName: nil)]
        )
        let submission = try await store.begin(original, in: context)
        XCTAssertTrue(store.saved(in: context).isEmpty)
        try await store.finish(submission, error: "Unconfirmed")
        let current = RecoverableComposerDraft(text: "new edit", image: nil, files: [])
        let restored = try await store.recover(submission, in: context, preserving: current)
        XCTAssertEqual(restored?.text, original.text)
        XCTAssertTrue(restored?.image === image)
        XCTAssertEqual(restored?.files, original.files)
        XCTAssertEqual(restored?.skills, original.skills)
        XCTAssertEqual(restored?.plugins, original.plugins)
        XCTAssertEqual(store.saved(in: context).map { $0.draft.text }, ["original", "new edit"])
        let reloaded = await loadedStore(directory: directory)
        XCTAssertEqual(reloaded.saved(in: context).map { $0.draft.text }, ["original", "new edit"])
        let durable = try XCTUnwrap(reloaded.saved(in: context).first?.draft)
        XCTAssertNotNil(durable.image)
        XCTAssertEqual(durable.image?.cgImage?.width, image.cgImage?.width)
        XCTAssertEqual(durable.image?.cgImage?.height, image.cgImage?.height)
        XCTAssertEqual(durable.image?.scale, image.scale)
        XCTAssertEqual(durable.image?.imageOrientation, image.imageOrientation)
        XCTAssertEqual(durable.files, original.files)
        XCTAssertEqual(durable.skills, original.skills)
        XCTAssertEqual(durable.plugins, original.plugins)
    }

    @MainActor
    func testOutOfOrderCompletionAndNewThreadFailureRemainIsolated() async throws {
        let directory = try temporaryDirectory()
        let store = await loadedStore(directory: directory)
        let home = ComposerDraftContext.project(serverId: "server", cwd: "/project")
        let thread = ComposerDraftContext.thread(ThreadKey(serverId: "server", threadId: "new"))
        let draft = RecoverableComposerDraft(text: "send", image: nil, files: [])
        let first = try await store.begin(draft, in: home)
        let second = try await store.begin(draft, in: home)
        try await store.move(first, to: thread)
        try await store.finish(second)
        try await store.finish(first, error: "Timeout")
        XCTAssertTrue(store.saved(in: home).isEmpty)
        XCTAssertEqual(store.saved(in: thread).map(\.id), [first])
        let wrongContextRecovery = try await store.recover(first, in: home, preserving: draft)
        XCTAssertNil(wrongContextRecovery)
        XCTAssertEqual(store.entries.count, 1)
        try await store.finish(second, error: "Late duplicate")
        XCTAssertEqual(store.entries.count, 1)
        let reloaded = await loadedStore(directory: directory)
        XCTAssertTrue(reloaded.saved(in: home).isEmpty)
        XCTAssertEqual(reloaded.saved(in: thread).map(\.id), [first])
    }

    @MainActor
    func testHomeHandoffPreservesNewerDraftForBothSendOutcomes() async throws {
        for error in [nil, "Unconfirmed"] as [String?] {
            let directory = try temporaryDirectory()
            let store = await loadedStore(directory: directory)
            let home = ComposerDraftContext.project(serverId: "server", cwd: "/project")
            let thread = ComposerDraftContext.thread(ThreadKey(serverId: "server", threadId: "new"))
            let id = try await store.begin(RecoverableComposerDraft(text: "submitted", image: nil, files: []), in: home)
            try await store.move(id, to: thread)
            try await store.finish(id, error: error)
            let newer = RecoverableComposerDraft(text: "newer", image: image(), files: [
                ComposerFileAttachment(label: "new.txt", path: "/project/new.txt")
            ])
            try await store.preserve(newer, in: thread)
            XCTAssertTrue(store.saved(in: home).isEmpty)
            XCTAssertEqual(store.saved(in: thread).map { $0.draft.text },
                           error == nil ? ["newer"] : ["submitted", "newer"])
            XCTAssertTrue(store.saved(in: thread).last?.draft.image === newer.image)
            XCTAssertEqual(store.saved(in: thread).last?.draft.files, newer.files)
            let reloaded = await loadedStore(directory: directory)
            XCTAssertEqual(reloaded.saved(in: thread).map { $0.draft.text },
                           error == nil ? ["newer"] : ["submitted", "newer"])
        }
    }

    @MainActor
    func testInterruptedSubmissionLoadsUnconfirmedAndEquivalentResendReplacesSavedSource() async throws {
        let directory = try temporaryDirectory()
        let context = ComposerDraftContext.project(serverId: "server", cwd: "/project")
        let draft = RecoverableComposerDraft(text: "interrupted", image: nil, files: [])
        let original = try await (await loadedStore(directory: directory)).begin(draft, in: context)
        let store = await loadedStore(directory: directory)
        XCTAssertEqual(store.saved(in: context).map(\.id), [original])
        let recovery = try await store.recover(original, in: context,
                                               preserving: RecoverableComposerDraft(text: "", image: nil, files: []))
        let restored = try XCTUnwrap(recovery)
        let beforeReplacement = await loadedStore(directory: directory)
        XCTAssertEqual(beforeReplacement.saved(in: context).map(\.id), [original])
        let replacement = try await store.begin(restored, in: context)
        XCTAssertNotEqual(replacement, original)
        let afterReplacement = await loadedStore(directory: directory)
        XCTAssertEqual(afterReplacement.saved(in: context).map(\.id), [replacement])
        try await store.finish(replacement)
        let afterFinish = await loadedStore(directory: directory)
        XCTAssertTrue(afterFinish.entries.isEmpty)
    }

    @MainActor
    func testWriteFailureLeavesSavedSourceAndNewerEditorDraftUntouched() async throws {
        let parent = try temporaryDirectory()
        let directory = parent.appendingPathComponent("store")
        let preservedDirectory = parent.appendingPathComponent("preserved")
        let context = ComposerDraftContext.project(serverId: "server", cwd: "/project")
        let store = await loadedStore(directory: directory)
        let original = try await store.begin(RecoverableComposerDraft(text: "original", image: image(), files: []), in: context)
        try await store.finish(original, error: "Unconfirmed")
        try FileManager.default.moveItem(at: directory, to: preservedDirectory)
        try Data("blocked".utf8).write(to: directory)
        let newer = RecoverableComposerDraft(text: "newer", image: image(), files: [])
        await assertThrowsError(try await store.begin(newer, in: context))
        await assertThrowsError(try await store.finish(original))
        await assertThrowsError(try await store.discard(original, in: context))
        await assertThrowsError(try await store.move(original, to: .project(serverId: "other", cwd: "/other")))
        await assertThrowsError(try await store.recover(original, in: context, preserving: newer))
        XCTAssertEqual(newer.text, "newer")
        XCTAssertNotNil(newer.image)
        XCTAssertEqual(store.entries.map(\.id), [original])
        XCTAssertNotNil(store.persistenceError)
        let reloaded = await loadedStore(directory: preservedDirectory)
        XCTAssertEqual(reloaded.saved(in: context).map(\.id), [original])
    }

    @MainActor
    func testDiscardIsContextScopedAndDurableWithoutRemovingOtherDrafts() async throws {
        let directory = try temporaryDirectory()
        let context = ComposerDraftContext.project(serverId: "server", cwd: "/project")
        let other = ComposerDraftContext.project(serverId: "other", cwd: "/project")
        let store = await loadedStore(directory: directory)
        let draft = RecoverableComposerDraft(text: "saved", image: nil, files: [])
        let original = try await store.begin(draft, in: context)
        try await store.discard(original, in: context)
        XCTAssertEqual(store.entries.map(\.id), [original], "An active submission is not a saved draft")
        try await store.finish(original, error: "Unconfirmed")
        let sibling = try await store.begin(draft, in: context)
        let unrelated = try await store.begin(draft, in: other)
        try await store.discard(original, in: other)
        XCTAssertEqual(store.entries.map(\.id), [original, sibling, unrelated])
        _ = try await store.recover(original, in: context,
                              preserving: RecoverableComposerDraft(text: "", image: nil, files: []))
        try await store.discard(original, in: context)
        XCTAssertEqual(store.entries.map(\.id), [sibling, unrelated])
        let reloaded = await loadedStore(directory: directory)
        XCTAssertEqual(reloaded.entries.map(\.id), [sibling, unrelated])
        try await store.discard(original, in: context)
        let replacement = try await store.begin(draft, in: context)
        XCTAssertEqual(store.entries.map(\.id), [sibling, unrelated, replacement])
    }

    @MainActor
    func testTemporaryCleanupRequiresValidCanonicalArchiveAndRetainsUnrelatedFiles() async throws {
        let directory = try temporaryDirectory()
        let manager = FileManager.default
        let temporary = directory.appendingPathComponent(".drafts-\(UUID().uuidString).tmp")
        let data = Data("abandoned".utf8)
        try data.write(to: temporary)
        let store = await loadedStore(directory: directory)
        XCTAssertEqual(try Data(contentsOf: temporary), data, "A missing archive must not trigger cleanup")
        _ = try await store.begin(RecoverableComposerDraft(text: "saved", image: nil, files: []),
                            in: .project(serverId: "server", cwd: "/project"))
        let file = directory.appendingPathComponent("drafts-v1.json")
        let archive = try Data(contentsOf: file)
        let corrupt = Data("damaged archive".utf8)
        try corrupt.write(to: file)
        let corruptedStore = await loadedStore(directory: directory)
        XCTAssertNotNil(corruptedStore.persistenceError)
        XCTAssertEqual(try Data(contentsOf: file), corrupt)
        XCTAssertEqual(try Data(contentsOf: temporary), data)
        try archive.write(to: file)
        let unrelated = directory.appendingPathComponent(".drafts-not-a-uuid.tmp")
        try data.write(to: unrelated)
        let nested = directory.appendingPathComponent(".drafts-\(UUID().uuidString).tmp")
        try manager.createDirectory(at: nested, withIntermediateDirectories: false)
        let link = directory.appendingPathComponent(".drafts-\(UUID().uuidString).tmp")
        try manager.createSymbolicLink(at: link, withDestinationURL: unrelated)
        let reloaded = await loadedStore(directory: directory)
        XCTAssertNil(reloaded.persistenceError)
        XCTAssertEqual(reloaded.entries.count, 1)
        XCTAssertFalse(manager.fileExists(atPath: temporary.path))
        XCTAssertEqual(try Data(contentsOf: file), archive)
        XCTAssertEqual(try Data(contentsOf: unrelated), data)
        XCTAssertTrue(manager.fileExists(atPath: nested.path))
        XCTAssertEqual(try manager.destinationOfSymbolicLink(atPath: link.path), unrelated.path)
    }

    @MainActor
    func testInvalidImageOrArchiveCannotOverwriteSavedDrafts() async throws {
        let directory = try temporaryDirectory()
        let context = ComposerDraftContext.project(serverId: "server", cwd: "/project")
        let store = await loadedStore(directory: directory)
        let original = try await store.begin(RecoverableComposerDraft(text: "original", image: nil, files: []), in: context)
        let file = directory.appendingPathComponent("drafts-v1.json")
        let before = try Data(contentsOf: file)
        await assertThrowsError(try await store.begin(RecoverableComposerDraft(text: "invalid image", image: UIImage(), files: []), in: context))
        XCTAssertEqual(store.entries.map(\.id), [original])
        XCTAssertEqual(try Data(contentsOf: file), before)
        let invalid = Data("invalid archive".utf8)
        try invalid.write(to: file)
        let unavailable = await loadedStore(directory: directory)
        XCTAssertNotNil(unavailable.persistenceError)
        await assertThrowsError(try await unavailable.begin(RecoverableComposerDraft(text: "new", image: nil, files: []), in: context))
        XCTAssertEqual(try Data(contentsOf: file), invalid)
        var unknownVersion = try XCTUnwrap(JSONSerialization.jsonObject(with: before) as? [String: Any])
        unknownVersion["version"] = 2
        let unknownData = try JSONSerialization.data(withJSONObject: unknownVersion)
        try unknownData.write(to: file)
        let newerVersion = await loadedStore(directory: directory)
        await assertThrowsError(try await newerVersion.preserve(RecoverableComposerDraft(text: "new", image: nil, files: []), in: context))
        XCTAssertEqual(try Data(contentsOf: file), unknownData)
    }

    @MainActor
    func testRecoveryFileIsVersionedProtectedAndExcludedFromBackup() async throws {
        let directory = try temporaryDirectory()
        try FileManager.default.setAttributes([.posixPermissions: 0o755], ofItemAtPath: directory.path)
        let store = await loadedStore(directory: directory)
        _ = try await store.begin(RecoverableComposerDraft(text: "private draft", image: nil, files: []),
                            in: .project(serverId: "server", cwd: "/project"))
        let file = directory.appendingPathComponent("drafts-v1.json")
        let archive = try XCTUnwrap(JSONSerialization.jsonObject(with: Data(contentsOf: file)) as? [String: Any])
        XCTAssertEqual(archive["version"] as? Int, 1)
        XCTAssertEqual(try file.resourceValues(forKeys: [.isExcludedFromBackupKey]).isExcludedFromBackup, true)
        XCTAssertEqual(try directory.resourceValues(forKeys: [.isExcludedFromBackupKey]).isExcludedFromBackup, true)
        let attributes = try FileManager.default.attributesOfItem(atPath: file.path)
        XCTAssertEqual((attributes[.posixPermissions] as? NSNumber)?.intValue, 0o600)
        let directoryAttributes = try FileManager.default.attributesOfItem(atPath: directory.path)
        XCTAssertEqual((directoryAttributes[.posixPermissions] as? NSNumber)?.intValue, 0o700)
        #if !targetEnvironment(simulator)
        // Simulator files have no data-protection class; device tests enforce it.
        XCTAssertEqual(attributes[.protectionKey] as? FileProtectionType, .complete)
        XCTAssertEqual(directoryAttributes[.protectionKey] as? FileProtectionType, .complete)
        #endif
    }

    @MainActor
    func testUnrelatedPersistenceErrorDoesNotMakeInflightSubmissionRecoverableOrDiscardable() async throws {
        let directory = try temporaryDirectory()
        let store = await loadedStore(directory: directory)
        let context = ComposerDraftContext.project(serverId: "server", cwd: "/project")
        let original = try await store.begin(RecoverableComposerDraft(text: "in flight", image: nil, files: []), in: context)
        await assertThrowsError(try await store.begin(
            RecoverableComposerDraft(text: "invalid image", image: UIImage(), files: []), in: context
        ))
        XCTAssertNotNil(store.persistenceError)
        XCTAssertTrue(store.saved(in: context).isEmpty)
        let recovery = try await store.recover(original, in: context,
                                               preserving: RecoverableComposerDraft(text: "newer", image: nil, files: []))
        XCTAssertNil(recovery)
        try await store.discard(original, in: context)
        XCTAssertEqual(store.entries.map(\.id), [original])
        XCTAssertNil(store.entries.first?.error)
        try await store.finish(original, error: "Submission not confirmed")
        XCTAssertEqual(store.saved(in: context).map(\.id), [original])
    }

    @MainActor
    func testFailedTerminalWriteRemainsRecoverableAfterAnUnrelatedSuccessfulCommit() async throws {
        for terminalError in [nil, "Remote result unknown"] as [String?] {
            let parent = try temporaryDirectory()
            let directory = parent.appendingPathComponent("store")
            let preservedDirectory = parent.appendingPathComponent("preserved")
            let store = await loadedStore(directory: directory)
            let context = ComposerDraftContext.project(serverId: "server", cwd: "/project")
            let original = try await store.begin(RecoverableComposerDraft(text: "original", image: nil, files: []), in: context)
            try FileManager.default.moveItem(at: directory, to: preservedDirectory)
            try Data("blocked".utf8).write(to: directory)
            await assertThrowsError(try await store.finish(original, error: terminalError))
            XCTAssertEqual(store.saved(in: context).map(\.id), [original])
            try FileManager.default.removeItem(at: directory)
            try FileManager.default.moveItem(at: preservedDirectory, to: directory)
            let other = try await store.begin(RecoverableComposerDraft(text: "other", image: nil, files: []), in: context)
            XCTAssertNil(store.persistenceError)
            XCTAssertEqual(store.saved(in: context).map(\.id), [original])
            XCTAssertEqual(store.entries.map(\.id), [original, other])
            XCTAssertNotNil(store.entries.first?.error)
            let reloaded = await loadedStore(directory: directory)
            XCTAssertEqual(reloaded.entries.map(\.id), [original, other])
        }
    }

    @MainActor
    func testRepeatedRecoveryOfTheSameContentDoesNotGrowTheArchive() async throws {
        let directory = try temporaryDirectory()
        let context = ComposerDraftContext.project(serverId: "server", cwd: "/project")
        let store = await loadedStore(directory: directory)
        let draft = RecoverableComposerDraft(text: "original", image: image(), files: [
            ComposerFileAttachment(label: "file", path: "/file")
        ], skills: [SkillMentionSelection(name: "skill", path: "/skill")],
        plugins: [PluginMentionSelection(name: "plugin", marketplace: "local", displayName: nil)])
        let id = try await store.begin(draft, in: context)
        try await store.finish(id, error: "Unconfirmed")
        let file = directory.appendingPathComponent("drafts-v1.json")
        let before = try Data(contentsOf: file)
        for _ in 0..<3 {
            let recovered = try await store.recover(id, in: context, preserving: draft)
            XCTAssertNotNil(recovered)
        }
        XCTAssertEqual(store.entries.map(\.id), [id])
        XCTAssertEqual(try Data(contentsOf: file), before)
        let replacement = try await store.begin(draft, in: context)
        XCTAssertEqual(store.entries.map(\.id), [replacement])
    }

    @MainActor
    func testOversizedSubmissionDoesNotReplaceTheExistingArchive() async throws {
        let directory = try temporaryDirectory()
        let context = ComposerDraftContext.project(serverId: "server", cwd: "/project")
        let store = await loadedStore(directory: directory)
        let id = try await store.begin(RecoverableComposerDraft(text: "original", image: nil, files: []), in: context)
        let file = directory.appendingPathComponent("drafts-v1.json")
        let before = try Data(contentsOf: file)
        // Content at the byte limit still exceeds the encoded archive limit once metadata is included.
        let oversized = RecoverableComposerDraft(
            text: String(repeating: "x", count: ComposerRecoveryStore.maximumArchiveBytes), image: nil, files: []
        )
        await assertThrowsError(try await store.begin(oversized, in: context))
        XCTAssertEqual(oversized.text.utf8.count, ComposerRecoveryStore.maximumArchiveBytes)
        XCTAssertEqual(store.entries.map(\.id), [id])
        XCTAssertEqual(try Data(contentsOf: file), before)
    }

    @MainActor
    func testOversizedArchiveIsNotLoadedOrOverwritten() async throws {
        let directory = try temporaryDirectory()
        let file = directory.appendingPathComponent("drafts-v1.json")
        try Data().write(to: file)
        let handle = try FileHandle(forWritingTo: file)
        try handle.truncate(atOffset: UInt64(ComposerRecoveryStore.maximumArchiveBytes + 1))
        try handle.close()
        let store = await loadedStore(directory: directory)
        XCTAssertTrue(store.entries.isEmpty)
        XCTAssertNotNil(store.persistenceError)
        await assertThrowsError(try await store.begin(RecoverableComposerDraft(text: "new", image: nil, files: []),
                                             in: .project(serverId: "server", cwd: "/project")))
        XCTAssertEqual(try file.resourceValues(forKeys: [.fileSizeKey]).fileSize,
                       ComposerRecoveryStore.maximumArchiveBytes + 1)
    }

    @MainActor
    func testOversizedImageDimensionsAreRejectedBeforePNGEncodingOrRasterRecovery() async throws {
        let directory = try temporaryDirectory()
        let context = ComposerDraftContext.project(serverId: "server", cwd: "/project")
        let store = await loadedStore(directory: directory)
        let original = try await store.begin(RecoverableComposerDraft(text: "original", image: nil, files: []), in: context)
        let file = directory.appendingPathComponent("drafts-v1.json")
        let before = try Data(contentsOf: file)
        let format = UIGraphicsImageRendererFormat()
        format.scale = 1
        let size = CGSize(width: CGFloat(ComposerRecoveryStore.maximumImageDimension + 1), height: 1)
        let wideImage = UIGraphicsImageRenderer(size: size, format: format).image { context in
            UIColor.red.setFill()
            context.fill(CGRect(origin: .zero, size: size))
        }
        await assertThrowsError(try await store.begin(RecoverableComposerDraft(text: "wide", image: wideImage, files: []), in: context))
        XCTAssertEqual(store.entries.map(\.id), [original])
        XCTAssertEqual(try Data(contentsOf: file), before)
        var archive = try XCTUnwrap(JSONSerialization.jsonObject(with: before) as? [String: Any])
        var entries = try XCTUnwrap(archive["entries"] as? [[String: Any]])
        var draft = try XCTUnwrap(entries[0]["draft"] as? [String: Any])
        draft["image"] = try XCTUnwrap(wideImage.pngData()).base64EncodedString()
        draft["imageScale"] = 1
        draft["imageOrientation"] = 0
        entries[0]["draft"] = draft
        archive["entries"] = entries
        let encoded = try JSONSerialization.data(withJSONObject: archive)
        try encoded.write(to: file)
        let reloaded = await loadedStore(directory: directory)
        XCTAssertTrue(reloaded.entries.isEmpty)
        XCTAssertNotNil(reloaded.persistenceError)
        XCTAssertEqual(try Data(contentsOf: file), encoded)
    }

    @MainActor
    func testBlockedPersistenceAllowsMainActorEditsAndSerializesConcurrentTransactions() async throws {
        let directory = try temporaryDirectory()
        let enteredWrite = expectation(description: "Storage entered its blocking write")
        enteredWrite.assertForOverFulfill = false
        let releaseWrite = DispatchSemaphore(value: 0)
        let store = ComposerRecoveryStore(directory: directory, beforeWrite: {
            XCTAssertFalse(Thread.isMainThread)
            enteredWrite.fulfill()
            XCTAssertEqual(releaseWrite.wait(timeout: .now() + 5), .success)
        })
        let context = ComposerDraftContext.project(serverId: "server", cwd: "/project")
        var editor = RecoverableComposerDraft(text: "submitted", image: image(), files: [])
        let captured = editor
        let first = Task { try await store.begin(captured, in: context) }
        await fulfillment(of: [enteredWrite], timeout: 2)

        // This runs on the main actor while persistence remains blocked.
        editor.text = "newer edit during await"
        editor.files = [ComposerFileAttachment(label: "new.txt", path: "/new.txt")]
        let secondDraft = editor
        let second = Task { try await store.preserve(secondDraft, in: context) }
        XCTAssertTrue(store.entries.isEmpty, "Uncommitted drafts must not be published")
        XCTAssertFalse(editor.matchesEditor(captured))
        releaseWrite.signal()
        releaseWrite.signal()
        let firstId = try await first.value
        try await second.value
        if editor.matchesEditor(captured) {
            editor = RecoverableComposerDraft(text: "", image: nil, files: [])
        }
        XCTAssertEqual(editor.text, "newer edit during await")
        XCTAssertEqual(editor.files, secondDraft.files)
        XCTAssertEqual(store.entries.count, 2)
        XCTAssertTrue(store.entries.contains { $0.id == firstId })
        XCTAssertEqual(store.saved(in: context).map { $0.draft.text }, [secondDraft.text])
        let reloaded = await loadedStore(directory: directory)
        XCTAssertEqual(Set(reloaded.entries.map { $0.draft.text }), Set([captured.text, secondDraft.text]))
    }

    @MainActor
    func testLargeImagePersistenceReportsLatencyWithoutBlockingMainActor() async throws {
        let directory = try temporaryDirectory()
        let format = UIGraphicsImageRendererFormat()
        format.scale = 1
        let size = CGSize(width: 3_072, height: 2_048)
        let largeImage = UIGraphicsImageRenderer(size: size, format: format).image { context in
            for x in stride(from: 0, to: 3_072, by: 8) {
                UIColor(hue: CGFloat(x % 360) / 360, saturation: 0.8, brightness: 0.9, alpha: 1).setFill()
                context.fill(CGRect(x: x, y: 0, width: 8, height: 2_048))
            }
        }
        let store = ComposerRecoveryStore(directory: directory, beforeWrite: {
            XCTAssertFalse(Thread.isMainThread)
        })
        let heartbeat = Task { @MainActor in
            var ticks = 0
            var maximumTickGap = 0.0
            var previous = ContinuousClock.now
            while !Task.isCancelled {
                do { try await Task.sleep(for: .milliseconds(5)) }
                catch { break }
                let now = ContinuousClock.now
                let gap = previous.duration(to: now).components
                maximumTickGap = max(maximumTickGap, Double(gap.seconds) + Double(gap.attoseconds) / 1e18)
                previous = now
                ticks += 1
            }
            return (ticks, maximumTickGap)
        }
        defer { heartbeat.cancel() }
        let start = ContinuousClock.now
        _ = try await store.begin(RecoverableComposerDraft(text: "large image", image: largeImage, files: []),
                                  in: .project(serverId: "server", cwd: "/project"))
        let elapsed = start.duration(to: .now)
        heartbeat.cancel()
        let (ticks, maximumTickGap) = await heartbeat.value
        let bytes = try directory.appendingPathComponent("drafts-v1.json").resourceValues(forKeys: [.fileSizeKey]).fileSize ?? 0
        print("RECOVERY_STORAGE image=3072x2048 archive_bytes=\(bytes) commit=\(elapsed) main_actor_ticks=\(ticks) max_tick_gap_seconds=\(maximumTickGap)")
        let reloaded = await loadedStore(directory: directory)
        XCTAssertEqual(reloaded.entries.first?.draft.image?.cgImage?.width, 3_072)
        XCTAssertEqual(reloaded.entries.first?.draft.image?.cgImage?.height, 2_048)
    }
}
