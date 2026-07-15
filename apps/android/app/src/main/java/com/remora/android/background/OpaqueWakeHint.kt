package com.remora.android.background

import java.nio.charset.StandardCharsets

/**
 * Platform ingress envelope for a content-free FCM data message.
 *
 * This type is deliberately not application state. A valid hint can only ask
 * the authenticated runtime to reconcile; it cannot update UI, launch a
 * harness, or decide an approval.
 */
internal data class OpaqueWakeHint(
    val schemaVersion: Int,
    val installationId: String,
    val eventId: String,
    val cursor: Long,
    val eventClass: OpaqueWakeEventClass,
    val expiresAtMs: Long,
)

internal enum class OpaqueWakeEventClass(val wireValue: String) {
    STATE_CHANGED("state_changed"),
    ACTIVITY_CHANGED("activity_changed"),
    CONNECTION_CHANGED("connection_changed"),
    SECURITY_CHANGED("security_changed"),
    ;

    companion object {
        fun fromWireValue(value: String): OpaqueWakeEventClass? =
            entries.firstOrNull { it.wireValue == value }
    }
}

internal sealed interface OpaqueWakeDecodeResult {
    data class Accepted(val hint: OpaqueWakeHint) : OpaqueWakeDecodeResult

    data class Rejected(val reason: OpaqueWakeRejection) : OpaqueWakeDecodeResult
}

internal enum class OpaqueWakeRejection {
    UNEXPECTED_SCHEMA,
    OVERSIZED,
    INVALID_SCHEMA_VERSION,
    INVALID_INSTALLATION_ID,
    INVALID_EVENT_ID,
    INVALID_CURSOR,
    INVALID_EVENT_CLASS,
    INVALID_EXPIRY,
}

internal object OpaqueWakeHintDecoder {
    private const val CURRENT_SCHEMA_VERSION = 1
    private const val MAX_PAYLOAD_BYTES = 4_096
    private const val MAX_FUTURE_LIFETIME_MS = 24L * 60L * 60L * 1_000L
    private val opaqueIdPattern = Regex("^[A-Za-z0-9_-]{16,128}$")
    private val expectedKeys = setOf(
        "schema_version",
        "installation_id",
        "event_id",
        "cursor",
        "event_class",
        "expires_at_ms",
    )

    fun decode(
        data: Map<String, String>,
        nowMs: Long = System.currentTimeMillis(),
    ): OpaqueWakeDecodeResult {
        if (data.keys != expectedKeys) {
            return OpaqueWakeDecodeResult.Rejected(OpaqueWakeRejection.UNEXPECTED_SCHEMA)
        }
        val payloadBytes = data.entries.sumOf { (key, value) ->
            key.toByteArray(StandardCharsets.UTF_8).size +
                value.toByteArray(StandardCharsets.UTF_8).size
        }
        if (payloadBytes > MAX_PAYLOAD_BYTES) {
            return OpaqueWakeDecodeResult.Rejected(OpaqueWakeRejection.OVERSIZED)
        }

        val schemaVersion = data.getValue("schema_version").toIntOrNull()
        if (schemaVersion != CURRENT_SCHEMA_VERSION) {
            return OpaqueWakeDecodeResult.Rejected(OpaqueWakeRejection.INVALID_SCHEMA_VERSION)
        }
        val installationId = data.getValue("installation_id")
        if (!opaqueIdPattern.matches(installationId)) {
            return OpaqueWakeDecodeResult.Rejected(OpaqueWakeRejection.INVALID_INSTALLATION_ID)
        }
        val eventId = data.getValue("event_id")
        if (!opaqueIdPattern.matches(eventId)) {
            return OpaqueWakeDecodeResult.Rejected(OpaqueWakeRejection.INVALID_EVENT_ID)
        }
        val cursor = data.getValue("cursor").toLongOrNull()
        if (cursor == null || cursor <= 0L) {
            return OpaqueWakeDecodeResult.Rejected(OpaqueWakeRejection.INVALID_CURSOR)
        }
        val eventClass = OpaqueWakeEventClass.fromWireValue(data.getValue("event_class"))
            ?: return OpaqueWakeDecodeResult.Rejected(OpaqueWakeRejection.INVALID_EVENT_CLASS)
        val expiresAtMs = data.getValue("expires_at_ms").toLongOrNull()
        if (
            expiresAtMs == null ||
            expiresAtMs <= nowMs ||
            expiresAtMs - nowMs > MAX_FUTURE_LIFETIME_MS
        ) {
            return OpaqueWakeDecodeResult.Rejected(OpaqueWakeRejection.INVALID_EXPIRY)
        }

        return OpaqueWakeDecodeResult.Accepted(
            OpaqueWakeHint(
                schemaVersion = schemaVersion,
                installationId = installationId,
                eventId = eventId,
                cursor = cursor,
                eventClass = eventClass,
                expiresAtMs = expiresAtMs,
            ),
        )
    }
}
