import Security
import XCTest
@testable import Remora

@MainActor
final class CurrentKeychainNamespaceCleanupTests: XCTestCase {
    func testMissingMarkerClearsCredentialsKeysAndPersistentPairingStateBeforeCompleting() throws {
        let (defaults, suiteName) = try makeDefaults()
        defer { defaults.removePersistentDomain(forName: suiteName) }
        var deletedQueries: [[String: Any]] = []
        var resetCount = 0
        var completed = false
        let remoraLinkKeyTag =
            RemoraLinkKeyStore.securityCutoverApplicationTagPrefix + Data("owned".utf8)
        let unrelatedKeyTag = Data("com.example.sdk.signing.owned".utf8)
        let cleanup = CurrentKeychainNamespaceCleanup(
            defaults: defaults,
            copyItems: { query, result in
                if CFEqual(query[kSecClass as String] as CFTypeRef, kSecClassGenericPassword) {
                    result?.pointee = [
                        [
                            kSecAttrService as String: "com.remora.app.ssh.credentials",
                            kSecAttrAccount as String: "host:22",
                        ],
                        [
                            kSecAttrService as String: RemoraLinkTransportIdentityKey.applicationV2.service,
                            kSecAttrAccount as String: "application-transport-secret",
                        ],
                        [
                            kSecAttrService as String: "retired.service",
                            kSecAttrAccount as String: "token",
                        ],
                    ] as CFArray
                } else {
                    result?.pointee = [
                        [kSecAttrApplicationTag as String: remoraLinkKeyTag],
                        [kSecAttrApplicationTag as String: unrelatedKeyTag],
                    ] as CFArray
                }
                return errSecSuccess
            },
            deleteItems: { query in
                deletedQueries.append(query)
                return errSecSuccess
            },
            resetPersistentState: {
                resetCount += 1
                return true
            }
        )

        cleanup.start { completed = true }

        XCTAssertEqual(deletedQueries.count, 3)
        XCTAssertEqual(
            deletedQueries.compactMap { $0[kSecAttrService as String] as? String },
            [
                RemoraLinkTransportIdentityKey.applicationV2.service,
                "retired.service",
            ]
        )
        XCTAssertEqual(
            deletedQueries.compactMap { $0[kSecAttrApplicationTag as String] as? Data },
            [remoraLinkKeyTag]
        )
        XCTAssertEqual(resetCount, 1)
        XCTAssertTrue(cleanup.isComplete)
        XCTAssertTrue(completed)
    }

    func testCompletedMarkerMakesCutoverIdempotent() throws {
        let (defaults, suiteName) = try makeDefaults()
        defer { defaults.removePersistentDomain(forName: suiteName) }
        defaults.set(true, forKey: CurrentKeychainNamespaceCleanup.completionMarkerKey)
        var deleteCount = 0
        var resetCount = 0
        var completed = false
        let cleanup = CurrentKeychainNamespaceCleanup(
            defaults: defaults,
            copyItems: { _, _ in
                XCTFail("completed cutover must not enumerate Keychain")
                return errSecInternalError
            },
            deleteItems: { _ in
                deleteCount += 1
                return errSecSuccess
            },
            resetPersistentState: {
                resetCount += 1
                return true
            }
        )

        cleanup.start { completed = true }

        XCTAssertEqual(deleteCount, 0)
        XCTAssertEqual(resetCount, 0)
        XCTAssertTrue(completed)
    }

    func testProtectedDataFailureRequestsRetryWithoutWritingMarker() throws {
        let (defaults, suiteName) = try makeDefaults()
        defer { defaults.removePersistentDomain(forName: suiteName) }
        let cleanup = CurrentKeychainNamespaceCleanup(
            defaults: defaults,
            copyItems: { _, _ in errSecInteractionNotAllowed },
            deleteItems: { _ in errSecSuccess },
            resetPersistentState: { true }
        )

        XCTAssertTrue(cleanup.cleanNow())
        XCTAssertFalse(cleanup.isComplete)
    }

    func testPersistentResetFailureDoesNotWriteMarker() throws {
        let (defaults, suiteName) = try makeDefaults()
        defer { defaults.removePersistentDomain(forName: suiteName) }
        let cleanup = CurrentKeychainNamespaceCleanup(
            defaults: defaults,
            copyItems: { _, _ in errSecItemNotFound },
            deleteItems: { _ in errSecItemNotFound },
            resetPersistentState: { false }
        )

        XCTAssertFalse(cleanup.cleanNow())
        XCTAssertFalse(cleanup.isComplete)
    }

    private func makeDefaults() throws -> (UserDefaults, String) {
        let suiteName = "CurrentKeychainNamespaceCleanupTests.\(UUID().uuidString)"
        return (try XCTUnwrap(UserDefaults(suiteName: suiteName)), suiteName)
    }
}
