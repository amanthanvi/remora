package com.remora.android.state

import com.remora.android.util.LLog
import java.util.concurrent.ConcurrentHashMap
import uniffi.codex_mobile_client.SshBridge

internal class SshSessionMappings {
    private val sessions = ConcurrentHashMap<String, String>()

    fun record(serverId: String, sessionId: String): String? = sessions.put(serverId, sessionId)

    fun rollbackRecord(serverId: String, failedSessionId: String, previousSessionId: String?) {
        if (previousSessionId == null) {
            sessions.remove(serverId, failedSessionId)
        } else {
            sessions.replace(serverId, failedSessionId, previousSessionId)
        }
    }

    fun remove(serverId: String): String? = sessions.remove(serverId)

    fun activeSessionId(serverId: String): String? = sessions[serverId]
}

/**
 * Thread-safe tracking of SSH session IDs per server.
 * Allows cleanup of SSH sessions on server disconnect.
 */
class SshSessionStore(private val ssh: SshBridge) {
    private val sessions = SshSessionMappings()

    fun record(serverId: String, sessionId: String): String? {
        LLog.t("SshSessionStore", "record SSH session", fields = mapOf("serverId" to serverId, "sessionId" to sessionId))
        return sessions.record(serverId, sessionId)
    }

    fun rollbackRecord(serverId: String, failedSessionId: String, previousSessionId: String?) {
        LLog.t(
            "SshSessionStore",
            "roll back SSH session record",
            fields = mapOf(
                "serverId" to serverId,
                "failedSessionId" to failedSessionId,
                "previousSessionId" to previousSessionId,
            ),
        )
        sessions.rollbackRecord(serverId, failedSessionId, previousSessionId)
    }

    fun clear(serverId: String) {
        LLog.t("SshSessionStore", "clear SSH session", fields = mapOf("serverId" to serverId))
        sessions.remove(serverId)
    }

    suspend fun close(serverId: String) {
        val sessionId = sessions.remove(serverId) ?: return
        LLog.t("SshSessionStore", "close SSH session", fields = mapOf("serverId" to serverId, "sessionId" to sessionId))
        try {
            ssh.sshClose(sessionId)
        } catch (e: Exception) {
            // Best-effort cleanup
            LLog.e("SshSessionStore", "failed to close SSH session", e, fields = mapOf("serverId" to serverId, "sessionId" to sessionId))
        }
    }

    fun activeSessionId(serverId: String): String? = sessions.activeSessionId(serverId)
}
