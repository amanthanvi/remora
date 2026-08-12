import Foundation

struct DeviceDatabaseOpenResult: @unchecked Sendable {
    let database: DeviceDatabaseBridge
    let didRebuild: Bool
}

enum DeviceDatabaseControllerError: LocalizedError {
    case applicationSupportUnavailable

    var errorDescription: String? {
        switch self {
        case .applicationSupportUnavailable:
            return "Encrypted local workspace storage is unavailable."
        }
    }
}

/// Opens the Rust-owned encrypted device cache with a device-only Keychain key.
/// The cache is disposable; the key is not. A corrupt cache is rebuilt once
/// with the same key so credentials and Host trust remain untouched.
// Immutable after construction and invoked only during serialized startup.
// FileManager is thread-safe for independent operations but lacks Sendable
// annotation in the Foundation SDK.
final class DeviceDatabaseController: @unchecked Sendable {
    static let shared = DeviceDatabaseController()

    private let fileManager: FileManager
    private let keyStore: RemoraLinkTransportIdentityStore
    private let applicationSupportDirectory: URL?

    init(
        fileManager: FileManager = .default,
        keyStore: RemoraLinkTransportIdentityStore = RemoraLinkTransportIdentityStore(
            key: .deviceDatabaseV1
        ),
        applicationSupportDirectory: URL? = nil
    ) {
        self.fileManager = fileManager
        self.keyStore = keyStore
        self.applicationSupportDirectory = applicationSupportDirectory
    }

    func open() throws -> DeviceDatabaseOpenResult {
        let directory = try databaseDirectory()
        try fileManager.createDirectory(
            at: directory,
            withIntermediateDirectories: true
        )
        var resourceValues = URLResourceValues()
        resourceValues.isExcludedFromBackup = true
        var excludedDirectory = directory
        try excludedDirectory.setResourceValues(resourceValues)

        let databaseURL = directory.appendingPathComponent("workspace.sqlite3")
        let hadExistingCache = databaseFiles(for: databaseURL).contains {
            fileManager.fileExists(atPath: $0.path)
        }
        var key = try keyStore.loadOrCreate().copyBytes()
        defer { key.remoraZeroize() }

        do {
            return DeviceDatabaseOpenResult(
                database: try openBridge(path: databaseURL.path, key: key),
                didRebuild: false
            )
        } catch {
            guard hadExistingCache else { throw error }
            try removeDatabaseFiles(for: databaseURL)
            return DeviceDatabaseOpenResult(
                database: try openBridge(path: databaseURL.path, key: key),
                didRebuild: true
            )
        }
    }

    private func databaseDirectory() throws -> URL {
        if let applicationSupportDirectory {
            return applicationSupportDirectory.appendingPathComponent(
                "RemoraDeviceDatabase/v1",
                isDirectory: true
            )
        }
        guard let root = fileManager.urls(
            for: .applicationSupportDirectory,
            in: .userDomainMask
        ).first else {
            throw DeviceDatabaseControllerError.applicationSupportUnavailable
        }
        return root.appendingPathComponent("RemoraDeviceDatabase/v1", isDirectory: true)
    }

    private func openBridge(path: String, key: Data) throws -> DeviceDatabaseBridge {
        let secret = AppRelaySecretValue(copying: key)
        defer { secret.zeroize() }
        return try DeviceDatabaseBridge.open(path: path, masterKey: secret)
    }

    private func removeDatabaseFiles(for databaseURL: URL) throws {
        for url in databaseFiles(for: databaseURL)
            where fileManager.fileExists(atPath: url.path) {
            try fileManager.removeItem(at: url)
        }
    }

    private func databaseFiles(for databaseURL: URL) -> [URL] {
        [
            databaseURL,
            URL(fileURLWithPath: databaseURL.path + "-wal"),
            URL(fileURLWithPath: databaseURL.path + "-shm"),
        ]
    }
}

private extension Data {
    mutating func remoraZeroize() {
        withUnsafeMutableBytes { bytes in
            guard let baseAddress = bytes.baseAddress, !bytes.isEmpty else { return }
            remoraLinkZeroizeMemory(baseAddress, byteCount: bytes.count)
        }
    }
}
