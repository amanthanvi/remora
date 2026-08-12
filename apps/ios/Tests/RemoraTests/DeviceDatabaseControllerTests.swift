import XCTest
@testable import Remora

final class DeviceDatabaseControllerTests: XCTestCase {
    func testCorruptCacheRebuildKeepsMasterKey() throws {
        let root = FileManager.default.temporaryDirectory
            .appendingPathComponent(UUID().uuidString, isDirectory: true)
        defer { try? FileManager.default.removeItem(at: root) }
        let databaseDirectory = root.appendingPathComponent(
            "RemoraDeviceDatabase/v1",
            isDirectory: true
        )
        try FileManager.default.createDirectory(
            at: databaseDirectory,
            withIntermediateDirectories: true
        )
        let databaseURL = databaseDirectory.appendingPathComponent("workspace.sqlite3")
        try Data("not-a-sqlite-database".utf8).write(to: databaseURL)
        let security = DeviceDatabaseTestSecurity(key: Data(repeating: 7, count: 32))
        let controller = DeviceDatabaseController(
            keyStore: RemoraLinkTransportIdentityStore(
                key: .deviceDatabaseV1,
                security: security
            ),
            applicationSupportDirectory: root
        )

        let result = try controller.open()

        XCTAssertTrue(result.didRebuild)
        XCTAssertEqual(try result.database.search(query: "anything", limit: 1), [])
        XCTAssertEqual(security.loadedKeys, [.deviceDatabaseV1])
    }

    func testDatabaseAndTransportNamespacesAreDistinct() {
        XCTAssertNotEqual(
            RemoraLinkTransportIdentityKey.applicationV2,
            RemoraLinkTransportIdentityKey.deviceDatabaseV1
        )
    }
}

private final class DeviceDatabaseTestSecurity: RemoraLinkTransportIdentitySecurity,
    @unchecked Sendable
{
    private let lock = NSLock()
    private let key: Data
    private(set) var loadedKeys: [RemoraLinkTransportIdentityKey] = []

    init(key: Data) {
        self.key = key
    }

    func load(key requestedKey: RemoraLinkTransportIdentityKey) -> RemoraLinkTransportIdentityLookup {
        lock.lock()
        loadedKeys.append(requestedKey)
        lock.unlock()
        return .found(
            RemoraLinkTransportIdentityStoredItem(
                bytes: key,
                policy: .backgroundCapableDeviceOnly
            )
        )
    }

    func createIfAbsent(
        key: RemoraLinkTransportIdentityKey,
        candidate: UnsafeRawBufferPointer,
        policy: RemoraLinkTransportIdentityStoragePolicy
    ) -> RemoraLinkTransportIdentityCreateOutcome {
        .duplicate
    }
}
