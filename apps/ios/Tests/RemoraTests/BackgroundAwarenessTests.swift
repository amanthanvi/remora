import Foundation
import XCTest
@testable import Remora

final class OpaqueWakePayloadTests: XCTestCase {
    private let now = Date(timeIntervalSince1970: 2_000_000_000)

    func testDecodesOnlyTheContentFreeVersionedWakeSchema() throws {
        let payload = try OpaqueWakePayload(
            userInfo: makeWakePayload(cursor: 42),
            now: now
        )

        XCTAssertEqual(payload.installationID, "installation_0123456789abcdef")
        XCTAssertEqual(payload.eventID, "event_0123456789abcdef")
        XCTAssertEqual(payload.cursor, 42)
        XCTAssertEqual(payload.eventClass, .stateChanged)
        XCTAssertEqual(payload.expiresAt, now.addingTimeInterval(300))
    }

    func testRejectsHostThreadUserAndContentFields() {
        for forbiddenKey in ["host_id", "thread_id", "user_id", "content", "command", "approval"] {
            var userInfo = makeWakePayload(cursor: 1)
            userInfo[forbiddenKey] = "forbidden_0123456789"

            XCTAssertThrowsError(try OpaqueWakePayload(userInfo: userInfo, now: now)) { error in
                XCTAssertEqual(error as? OpaqueWakePayloadError, .invalidRootKeys)
            }
        }
    }

    func testRejectsVisibleAlertAndActionEnvelope() {
        var alert = makeWakePayload(cursor: 1)
        alert["aps"] = [
            "content-available": 1,
            "alert": "Approval required",
            "category": "APPROVE"
        ]

        XCTAssertThrowsError(try OpaqueWakePayload(userInfo: alert, now: now)) { error in
            XCTAssertEqual(error as? OpaqueWakePayloadError, .invalidBackgroundEnvelope)
        }
    }

    func testRejectsExpiredAndLongLivedWakeHints() {
        var expired = makeWakePayload(cursor: 1)
        expired["expires_at_ms"] = milliseconds(now.addingTimeInterval(-1))
        XCTAssertThrowsError(try OpaqueWakePayload(userInfo: expired, now: now)) { error in
            XCTAssertEqual(error as? OpaqueWakePayloadError, .expired)
        }

        var distant = makeWakePayload(cursor: 1)
        distant["expires_at_ms"] = milliseconds(
            now.addingTimeInterval(OpaqueWakePayload.maximumExpirationHorizon + 1)
        )
        XCTAssertThrowsError(try OpaqueWakePayload(userInfo: distant, now: now)) { error in
            XCTAssertEqual(error as? OpaqueWakePayloadError, .expirationTooDistant)
        }
    }

    func testRejectsBooleanAndFractionalIntegerFields() {
        var booleanCursor = makeWakePayload(cursor: 1)
        booleanCursor["cursor"] = true
        XCTAssertThrowsError(try OpaqueWakePayload(userInfo: booleanCursor, now: now)) { error in
            XCTAssertEqual(error as? OpaqueWakePayloadError, .invalidField("cursor"))
        }

        var fractionalExpiry = makeWakePayload(cursor: 1)
        fractionalExpiry["expires_at_ms"] = 2_000_000_300_000.5
        XCTAssertThrowsError(try OpaqueWakePayload(userInfo: fractionalExpiry, now: now)) { error in
            XCTAssertEqual(error as? OpaqueWakePayloadError, .invalidField("expires_at_ms"))
        }
    }

    func testRejectsUnknownEventClassAndOversizedPayload() {
        var unknownClass = makeWakePayload(cursor: 1)
        unknownClass["event_class"] = "approval_required"
        XCTAssertThrowsError(try OpaqueWakePayload(userInfo: unknownClass, now: now)) { error in
            XCTAssertEqual(error as? OpaqueWakePayloadError, .invalidField("event_class"))
        }

        var oversized = makeWakePayload(cursor: 1)
        oversized["event_id"] = String(repeating: "a", count: OpaqueWakePayload.maximumEncodedBytes)
        XCTAssertThrowsError(try OpaqueWakePayload(userInfo: oversized, now: now)) { error in
            XCTAssertEqual(error as? OpaqueWakePayloadError, .payloadTooLarge)
        }
    }

    private func makeWakePayload(cursor: UInt64) -> [AnyHashable: Any] {
        [
            "aps": ["content-available": 1],
            "schema_version": 1,
            "installation_id": "installation_0123456789abcdef",
            "event_id": "event_0123456789abcdef",
            "cursor": NSNumber(value: cursor),
            "event_class": "state_changed",
            "expires_at_ms": milliseconds(now.addingTimeInterval(300))
        ]
    }

    private func milliseconds(_ date: Date) -> NSNumber {
        NSNumber(value: UInt64(date.timeIntervalSince1970 * 1_000))
    }
}
