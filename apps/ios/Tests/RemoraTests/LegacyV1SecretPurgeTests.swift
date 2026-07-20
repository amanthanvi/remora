import Security
import XCTest
@testable import Remora

@MainActor
final class LegacyV1SecretPurgeTests: XCTestCase {
    func testPurgeDeletesAllTokenAccountsAndOnlyTheLegacyDeviceKeyAccount() {
        var queries: [[String: Any]] = []
        let purge = LegacyV1SecretPurge { query in
            queries.append(query)
            return errSecSuccess
        }

        XCTAssertFalse(purge.purgeNow())
        XCTAssertEqual(queries.count, 2)
        XCTAssertEqual(queries[0][kSecClass as String] as? String, kSecClassGenericPassword as String)
        XCTAssertEqual(queries[0][kSecAttrService as String] as? String, "com.alleycat.token")
        XCTAssertNil(queries[0][kSecAttrAccount as String])
        XCTAssertEqual(queries[1][kSecClass as String] as? String, kSecClassGenericPassword as String)
        XCTAssertEqual(queries[1][kSecAttrService as String] as? String, "com.alleycat.device_key")
        XCTAssertEqual(queries[1][kSecAttrAccount as String] as? String, "__device_secret_key__")
    }

    func testProtectedDataFailureRetriesBothDeletesWhenDataBecomesAvailable() {
        let center = NotificationCenter()
        let notification = Notification.Name("LegacyV1SecretPurgeTests.protectedData")
        var statuses: [OSStatus] = [
            errSecInteractionNotAllowed,
            errSecSuccess,
            errSecSuccess,
            errSecSuccess,
        ]
        var queryCount = 0
        let purge = LegacyV1SecretPurge(
            notificationCenter: center,
            protectedDataNotification: notification
        ) { _ in
            defer { queryCount += 1 }
            return statuses.removeFirst()
        }

        purge.start()
        XCTAssertEqual(queryCount, 2)

        center.post(name: notification, object: nil)

        XCTAssertEqual(queryCount, 4)
        XCTAssertTrue(statuses.isEmpty)
    }

    func testSuccessfulPurgeRunsAgainWithoutCompletionMarker() {
        var queryCount = 0
        let purge = LegacyV1SecretPurge { _ in
            queryCount += 1
            return errSecItemNotFound
        }

        purge.start()
        purge.start()

        XCTAssertEqual(queryCount, 4)
    }
}
