import Foundation

enum OpaqueWakeEventClass: String, CaseIterable, Sendable {
    case stateChanged = "state_changed"
    case activityChanged = "activity_changed"
    case connectionChanged = "connection_changed"
    case securityChanged = "security_changed"
}

enum OpaqueWakePayloadError: Error, Equatable {
    case payloadTooLarge
    case invalidRootKeys
    case invalidBackgroundEnvelope
    case unsupportedSchemaVersion
    case invalidField(String)
    case expired
    case expirationTooDistant
}

/// A content-free APNs invalidation hint. This record is deliberately not an
/// application event: it may only trigger an authenticated fetch and
/// authoritative reconciliation through the shared Rust-backed runtime.
struct OpaqueWakePayload: Equatable, Sendable {
    static let currentSchemaVersion: UInt64 = 1
    static let maximumEncodedBytes = 4_096
    static let maximumExpirationHorizon: TimeInterval = 24 * 60 * 60

    private static let maximumJSONInteger: UInt64 = 9_007_199_254_740_991
    private static let allowedRootKeys: Set<String> = [
        "aps",
        "schema_version",
        "installation_id",
        "event_id",
        "cursor",
        "event_class",
        "expires_at_ms"
    ]

    let installationID: String
    let eventID: String
    let cursor: UInt64
    let eventClass: OpaqueWakeEventClass
    let expiresAt: Date

    init(userInfo: [AnyHashable: Any], now: Date = Date()) throws {
        guard JSONSerialization.isValidJSONObject(userInfo),
              let encoded = try? JSONSerialization.data(withJSONObject: userInfo),
              encoded.count <= Self.maximumEncodedBytes else {
            throw OpaqueWakePayloadError.payloadTooLarge
        }

        var fields: [String: Any] = [:]
        fields.reserveCapacity(userInfo.count)
        for (key, value) in userInfo {
            guard let key = key as? String else {
                throw OpaqueWakePayloadError.invalidRootKeys
            }
            fields[key] = value
        }

        guard Set(fields.keys) == Self.allowedRootKeys else {
            throw OpaqueWakePayloadError.invalidRootKeys
        }
        try Self.validateBackgroundEnvelope(fields["aps"])

        guard let schemaVersion = Self.exactUInt64(fields["schema_version"]),
              schemaVersion == Self.currentSchemaVersion else {
            throw OpaqueWakePayloadError.unsupportedSchemaVersion
        }

        installationID = try Self.opaqueIdentifier(
            fields["installation_id"],
            field: "installation_id"
        )
        eventID = try Self.opaqueIdentifier(fields["event_id"], field: "event_id")

        guard let cursor = Self.exactUInt64(fields["cursor"]), cursor > 0 else {
            throw OpaqueWakePayloadError.invalidField("cursor")
        }
        self.cursor = cursor

        guard let rawEventClass = fields["event_class"] as? String,
              let eventClass = OpaqueWakeEventClass(rawValue: rawEventClass) else {
            throw OpaqueWakePayloadError.invalidField("event_class")
        }
        self.eventClass = eventClass

        guard let expiresAtMilliseconds = Self.exactUInt64(fields["expires_at_ms"]) else {
            throw OpaqueWakePayloadError.invalidField("expires_at_ms")
        }
        let expiresAt = Date(timeIntervalSince1970: TimeInterval(expiresAtMilliseconds) / 1_000)
        guard expiresAt > now else {
            throw OpaqueWakePayloadError.expired
        }
        guard expiresAt.timeIntervalSince(now) <= Self.maximumExpirationHorizon else {
            throw OpaqueWakePayloadError.expirationTooDistant
        }
        self.expiresAt = expiresAt
    }

    private static func validateBackgroundEnvelope(_ rawValue: Any?) throws {
        guard let aps = rawValue as? [String: Any],
              Set(aps.keys) == ["content-available"],
              exactUInt64(aps["content-available"]) == 1 else {
            throw OpaqueWakePayloadError.invalidBackgroundEnvelope
        }
    }

    private static func opaqueIdentifier(_ value: Any?, field: String) throws -> String {
        guard let value = value as? String,
              (16...128).contains(value.utf8.count),
              value.unicodeScalars.allSatisfy({ scalar in
                  switch scalar.value {
                  case 45, 48...57, 65...90, 95, 97...122:
                      return true
                  default:
                      return false
                  }
              }) else {
            throw OpaqueWakePayloadError.invalidField(field)
        }
        return value
    }

    private static func exactUInt64(_ value: Any?) -> UInt64? {
        guard let number = value as? NSNumber,
              CFGetTypeID(number) != CFBooleanGetTypeID() else {
            return nil
        }
        let doubleValue = number.doubleValue
        guard doubleValue.isFinite,
              doubleValue >= 0,
              doubleValue.rounded(.towardZero) == doubleValue,
              doubleValue <= Double(maximumJSONInteger) else {
            return nil
        }
        return number.uint64Value
    }
}

enum VisibleNotificationAuthorization: Equatable, Sendable {
    case unknown
    case notDetermined
    case denied
    case provisional
    case authorized
    case ephemeral
}

struct NotificationPermissionState: Equatable, Sendable {
    static let unknown = NotificationPermissionState(
        authorization: .unknown,
        alertsEnabled: false,
        soundsEnabled: false,
        badgesEnabled: false
    )

    let authorization: VisibleNotificationAuthorization
    let alertsEnabled: Bool
    let soundsEnabled: Bool
    let badgesEnabled: Bool

    var canRequestInContext: Bool {
        authorization == .notDetermined
    }
}

enum APNsEnvironment: String, Codable, Equatable, Sendable {
    case sandbox
    case production

    static var current: Self {
        #if DEBUG
        .sandbox
        #else
        .production
        #endif
    }
}

enum PushTokenProvider: String, Codable, Equatable, Sendable {
    case apns
}

struct APNsTokenRegistration: Equatable, Sendable {
    /// Stable, device-local idempotency scope. This is never a relay routing
    /// identity and must never appear in an opaque wake payload.
    let clientInstanceID: String
    /// Relay-issued identity from a prior successful registration, if one is
    /// already pinned locally. A registry adapter provisions/restores the
    /// relay installation when this is nil.
    let installationID: String?
    let token: Data
    let generation: UInt64
    let replacesGeneration: UInt64?
    let provider: PushTokenProvider
    let environment: APNsEnvironment
    let observedAt: Date
}

/// Authoritative receipt returned by the relay's atomic per-provider upsert.
/// The registry adapter must persist the installation's scoped capabilities in
/// Keychain before returning this receipt; this lifecycle pins only the opaque
/// routing identity needed to validate wake hints.
struct PushTokenRegistrationReceipt: Equatable, Sendable {
    static let currentSchemaVersion: UInt64 = 1

    let schemaVersion: UInt64
    let installationID: String
    let registrationID: String
    let provider: PushTokenProvider
    let environment: APNsEnvironment
    let generation: UInt64
    let replaced: Bool
}

struct APNsTokenTombstone: Equatable, Sendable {
    let clientInstanceID: String
    let installationID: String?
    let throughGeneration: UInt64
    let provider: PushTokenProvider
    let environment: APNsEnvironment
    let observedAt: Date
}

/// The deployed gateway adapter implements this protocol. Upsert must
/// atomically replace any older generation for an installation; tombstone must
/// make every generation through the supplied value undeliverable.
@MainActor
protocol PushTokenRegistry: AnyObject {
    /// Creates/restores a relay installation when needed, securely persists the
    /// relay-issued id plus scoped capabilities, then atomically replaces the
    /// active token for this provider/environment.
    func upsert(_ registration: APNsTokenRegistration) async throws -> PushTokenRegistrationReceipt
    /// Idempotently revokes the active registration through this generation.
    /// If `installationID` is nil after a crash boundary, the adapter must
    /// restore its Keychain binding from `clientInstanceID` before revoking.
    func tombstone(_ tombstone: APNsTokenTombstone) async throws
}

enum PushTokenSyncState: Equatable, Sendable {
    case unavailable
    case pending(generation: UInt64)
    case synced(generation: UInt64)
    case failed(generation: UInt64)
    case tombstoned(generation: UInt64)
}

enum RemoteNotificationRegistrationState: Equatable, Sendable {
    case idle
    case registering
    case registered
    case failed
}

enum BackgroundReconciliationResult: Equatable, Sendable {
    case newData
    case noData
    case timedOut
    case unavailable
    case failed
}

enum AuthenticatedBackgroundStateResult: Equatable, Sendable {
    case changed
    case unchanged
    case failed
}

@MainActor
protocol BackgroundStateReconciling: AnyObject {
    /// `expectedCursor` is a high-water hint only. Implementations must fetch
    /// authenticated state and must not mark the cursor applied merely because
    /// APNs delivered it.
    func reconcileBackgroundState(expectedCursor: UInt64) async -> AuthenticatedBackgroundStateResult
}
