package com.remora.android.state

import android.content.Context
import android.content.SharedPreferences
import com.remora.android.ui.common.AgentRuntimeKind
import org.json.JSONArray
import org.json.JSONObject
import uniffi.codex_mobile_client.AppDiscoveredServer
import uniffi.codex_mobile_client.AppDiscoverySource
import uniffi.codex_mobile_client.SavedServerRecord

/**
 * Persistent server list stored in SharedPreferences.
 * Platform-specific — cannot live in Rust.
 */
data class SavedServer(
    val id: String,
    val name: String,
    val hostname: String,
    val port: Int,
    val codexPorts: List<Int> = emptyList(),
    val sshPort: Int? = null,
    val source: String = "manual", // local, bonjour, tailscale, lanProbe, arpScan, ssh, manual
    val hasCodexServer: Boolean = false,
    val wakeMAC: String? = null,
    val preferredConnectionMode: String? = null, // directCodex or ssh
    val preferredCodexPort: Int? = null,
    val sshPortForwardingEnabled: Boolean? = null, // legacy migration only
    val websocketURL: String? = null,
    val os: String? = null,
    val sshBanner: String? = null,
    val rememberedByUser: Boolean = false,
    /**
     * Null means this is not an SSH bridge, an empty list probes every supported
     * runtime, and a non-empty list reconnects only the selected runtime kinds.
     */
    val sshBridgeRuntimeKinds: List<AgentRuntimeKind>? = null,
) {
    /** Stable key for deduplication across discovery cycles. */
    val deduplicationKey: String
        get() = websocketURL ?: normalizedHostKey(hostname)

    private fun normalizedHostKey(host: String): String {
        val trimmed = host.trim().trimStart('[').trimEnd(']')
        val withoutScope = if (!trimmed.contains(":")) {
            trimmed.substringBefore('%')
        } else {
            trimmed
        }
        return withoutScope.lowercase()
    }

    fun toJson(): JSONObject = JSONObject().apply {
        put("id", id)
        put("name", name)
        put("hostname", hostname)
        put("port", port)
        put("codexPorts", JSONArray(availableDirectCodexPorts))
        sshPort?.let { put("sshPort", it) }
        put("source", source)
        put("hasCodexServer", hasCodexServer)
        wakeMAC?.let { put("wakeMAC", it) }
        preferredConnectionMode?.let { put("preferredConnectionMode", it) }
        preferredCodexPort?.let { put("preferredCodexPort", it) }
        sshPortForwardingEnabled?.let { put("sshPortForwardingEnabled", it) }
        websocketURL?.let { put("websocketURL", it) }
        os?.let { put("os", it) }
        sshBanner?.let { put("sshBanner", it) }
        put("rememberedByUser", rememberedByUser)
        sshBridgeRuntimeKinds?.let { put("sshBridgeRuntimeKinds", JSONArray(it)) }
    }

    val availableDirectCodexPorts: List<Int>
        get() {
            val ordered = buildList {
                if (hasCodexServer && port > 0) add(port)
                addAll(codexPorts.filter { it > 0 })
            }
            return ordered.distinct()
        }

    val resolvedPreferredConnectionMode: String?
        get() = when (preferredConnectionMode) {
            "directCodex" -> if (availableDirectCodexPorts.isNotEmpty() || websocketURL != null) "directCodex" else null
            "ssh" -> if (canConnectViaSsh) "ssh" else null
            else -> if (sshPortForwardingEnabled == true) "ssh" else null
        }

    val prefersSshConnection: Boolean
        get() = resolvedPreferredConnectionMode == "ssh"

    val canConnectViaSsh: Boolean
        get() = websocketURL == null && (
            sshPort != null ||
                source == "ssh" ||
                (!hasCodexServer && resolvedSshPort > 0) ||
                preferredConnectionMode == "ssh" ||
                sshPortForwardingEnabled == true
        )

    val resolvedSshPort: Int
        get() = sshPort ?: port.takeIf { !hasCodexServer && it > 0 } ?: 22

    val resolvedPreferredCodexPort: Int?
        get() = when {
            resolvedPreferredConnectionMode != "directCodex" -> null
            preferredCodexPort != null && availableDirectCodexPorts.contains(preferredCodexPort) -> preferredCodexPort
            else -> null
        }

    val requiresConnectionChoice: Boolean
        get() = websocketURL == null &&
            resolvedPreferredConnectionMode == null &&
            (
                availableDirectCodexPorts.size > 1 ||
                    (availableDirectCodexPorts.isNotEmpty() && canConnectViaSsh)
            )

    val directCodexPort: Int?
        get() = when {
            websocketURL != null -> null
            prefersSshConnection -> null
            resolvedPreferredCodexPort != null -> resolvedPreferredCodexPort
            requiresConnectionChoice -> null
            availableDirectCodexPorts.isNotEmpty() -> availableDirectCodexPorts.first()
            else -> null
        }

    fun withPreferredConnection(mode: String?, codexPort: Int? = null): SavedServer =
        copy(
            port = when (mode) {
                "directCodex" -> codexPort ?: directCodexPort ?: availableDirectCodexPorts.firstOrNull() ?: port
                "ssh" -> resolvedSshPort
                else -> port
            },
            codexPorts = availableDirectCodexPorts,
            sshPort = sshPort ?: if (canConnectViaSsh) resolvedSshPort else null,
            preferredConnectionMode = mode,
            preferredCodexPort = if (mode == "directCodex") {
                codexPort ?: directCodexPort ?: availableDirectCodexPorts.firstOrNull()
            } else {
                null
            },
            sshPortForwardingEnabled = null,
        )

    fun normalizedForPersistence(): SavedServer =
        withPreferredConnection(
            mode = resolvedPreferredConnectionMode,
            codexPort = resolvedPreferredCodexPort ?: availableDirectCodexPorts.firstOrNull(),
        ).copy(
            sshBridgeRuntimeKinds = sshBridgeRuntimeKinds?.normalizeRuntimeKinds(),
        )

    fun toDiscoveredServer(): AppDiscoveredServer {
        val codexPort = if (hasCodexServer) (preferredCodexPort ?: port) else null
        val resolvedSshPort = sshPort ?: if (hasCodexServer) null else port
        return AppDiscoveredServer(
            id = id,
            displayName = name,
            host = hostname,
            port = codexPort?.toUShort() ?: 0u,
            codexPort = codexPort?.toUShort(),
            codexPorts = availableDirectCodexPorts.map { it.toUShort() },
            sshPort = resolvedSshPort?.toUShort(),
            source = toAppDiscoverySource(source),
            reachable = true,
            os = os,
            sshBanner = sshBanner,
        )
    }

    private fun toAppDiscoverySource(source: String): AppDiscoverySource = when (source.lowercase()) {
        "bonjour" -> AppDiscoverySource.BONJOUR
        "tailscale" -> AppDiscoverySource.TAILSCALE
        "lanprobe", "lan_probe" -> AppDiscoverySource.LAN_PROBE
        "arpscan", "arp_scan" -> AppDiscoverySource.ARP_SCAN
        "manual" -> AppDiscoverySource.MANUAL
        "local" -> AppDiscoverySource.LOCAL
        else -> AppDiscoverySource.MANUAL
    }

    companion object {
        fun normalizeWakeMac(raw: String?): String? {
            val compact = raw
                ?.trim()
                ?.replace(":", "")
                ?.replace("-", "")
                ?.lowercase()
                ?: return null
            if (compact.length != 12 || compact.any { !it.isDigit() && it !in 'a'..'f' }) {
                return null
            }
            return buildString {
                compact.chunked(2).forEachIndexed { index, chunk ->
                    if (index > 0) append(':')
                    append(chunk)
                }
            }
        }

        fun fromJson(obj: JSONObject): SavedServer {
            val id = obj.getString("id")
            val legacyAgentWire = obj.optString("alleycatAgentWire").trim()
            val isHistoricalSshBridge = id.startsWith("ssh-bridge:") || legacyAgentWire == "ssh-bridge"
            val sshBridgeRuntimeKinds = when {
                obj.has("sshBridgeRuntimeKinds") && !obj.isNull("sshBridgeRuntimeKinds") ->
                    obj.optJSONArray("sshBridgeRuntimeKinds")
                        ?.toNormalizedRuntimeKinds()
                        ?: emptyList()
                obj.has("sshBridgeRuntimeKinds") -> null
                isHistoricalSshBridge -> obj.optString("alleycatAgentName")
                    .split(',')
                    .toNormalizedRuntimeKinds()
                else -> null
            }

            return SavedServer(
                id = id,
                name = obj.optString("name", ""),
                hostname = obj.optString("hostname", ""),
                port = obj.optInt("port", 0),
                codexPorts = buildList {
                    val ports = obj.optJSONArray("codexPorts")
                    if (ports != null) {
                        for (index in 0 until ports.length()) {
                            add(ports.optInt(index))
                        }
                    }
                },
                sshPort = if (obj.has("sshPort")) obj.getInt("sshPort") else null,
                source = obj.optString("source", "manual"),
                hasCodexServer = obj.optBoolean("hasCodexServer", false),
                wakeMAC = if (obj.has("wakeMAC")) obj.getString("wakeMAC") else null,
                preferredConnectionMode = obj.optString("preferredConnectionMode").ifBlank { null },
                preferredCodexPort = if (obj.has("preferredCodexPort")) obj.getInt("preferredCodexPort") else null,
                sshPortForwardingEnabled = if (obj.has("sshPortForwardingEnabled")) {
                    obj.optBoolean("sshPortForwardingEnabled")
                } else {
                    null
                },
                websocketURL = if (obj.has("websocketURL")) obj.getString("websocketURL") else null,
                os = if (obj.has("os")) obj.getString("os") else null,
                sshBanner = if (obj.has("sshBanner")) obj.getString("sshBanner") else null,
                rememberedByUser = if (obj.has("rememberedByUser")) {
                    obj.optBoolean("rememberedByUser")
                } else {
                    true
                },
                sshBridgeRuntimeKinds = sshBridgeRuntimeKinds,
            )
        }

        private fun JSONArray.toNormalizedRuntimeKinds(): List<AgentRuntimeKind> =
            (0 until length()).map { optString(it) }.normalizeRuntimeKinds()

        private fun List<String>.toNormalizedRuntimeKinds(): List<AgentRuntimeKind> =
            normalizeRuntimeKinds()

        fun from(server: AppDiscoveredServer): SavedServer = SavedServer(
            id = server.id,
            name = server.displayName,
            hostname = server.host,
            port = server.codexPort?.toInt() ?: server.port.toInt(),
            codexPorts = server.codexPorts.map { it.toInt() },
            sshPort = server.sshPort?.toInt(),
            source = when (server.source) {
                AppDiscoverySource.BONJOUR -> "bonjour"
                AppDiscoverySource.TAILSCALE -> "tailscale"
                AppDiscoverySource.LAN_PROBE -> "lanProbe"
                AppDiscoverySource.ARP_SCAN -> "arpScan"
                AppDiscoverySource.MANUAL -> "manual"
                AppDiscoverySource.LOCAL -> "local"
            },
            hasCodexServer = server.codexPort != null || server.codexPorts.isNotEmpty(),
            os = if (server.sshBanner != null) server.os else server.os,
            sshBanner = server.sshBanner,
        )
    }
}

fun SavedServer.toRecord() = SavedServerRecord(
    id = id,
    name = name,
    hostname = hostname,
    port = port.toUShort(),
    codexPorts = codexPorts.map { it.toUShort() },
    sshPort = sshPort?.toUShort(),
    source = source,
    hasCodexServer = hasCodexServer,
    wakeMac = wakeMAC,
    preferredConnectionMode = preferredConnectionMode,
    preferredCodexPort = preferredCodexPort?.toUShort(),
    sshPortForwardingEnabled = sshPortForwardingEnabled,
    websocketUrl = websocketURL,
    rememberedByUser = rememberedByUser,
    sshBridgeRuntimeKinds = sshBridgeRuntimeKinds,
)

object SavedServerStore {
    private const val PREFS_NAME = "codex_saved_servers_prefs"
    private const val KEY = "codex_saved_servers"

    private fun prefs(context: Context): SharedPreferences =
        context.getSharedPreferences(PREFS_NAME, Context.MODE_PRIVATE)

    fun load(context: Context): List<SavedServer> {
        val json = prefs(context).getString(KEY, null) ?: return emptyList()
        return try {
            val array = JSONArray(json)
            val decoded = (0 until array.length()).map { index ->
                val objectValue = array.getJSONObject(index)
                objectValue to SavedServer.fromJson(objectValue)
            }
            val retained = decoded.filterNot { (objectValue, _) -> isLegacyV1Only(objectValue) }
            val migrated = retained.map { (_, server) -> server.normalizedForPersistence() }
            val needsRewrite = retained.size != decoded.size ||
                retained.any { (objectValue, _) -> LEGACY_V1_JSON_KEYS.any(objectValue::has) } ||
                retained.any { (objectValue, server) -> runtimeKindsNeedRewrite(objectValue, server) } ||
                retained.map { it.second } != migrated
            if (needsRewrite) {
                save(context, migrated)
            }
            migrated
        } catch (_: Exception) {
            emptyList()
        }
    }

    fun save(context: Context, servers: List<SavedServer>) {
        val array = JSONArray()
        servers.forEach { array.put(it.toJson()) }
        prefs(context).edit().putString(KEY, array.toString()).apply()
    }

    fun upsert(context: Context, server: SavedServer) {
        val existing = load(context).toMutableList()
        val prior = existing.firstOrNull { it.id == server.id || it.deduplicationKey == server.deduplicationKey }
        existing.removeAll { it.id == server.id || it.deduplicationKey == server.deduplicationKey }
        existing.add(server.copy(rememberedByUser = prior?.rememberedByUser ?: server.rememberedByUser))
        save(context, existing)
    }

    fun remember(context: Context, server: SavedServer) {
        val existing = load(context).toMutableList()
        existing.removeAll { it.id == server.id || it.deduplicationKey == server.deduplicationKey }
        existing.add(server.copy(rememberedByUser = true))
        save(context, existing)
    }

    fun remembered(context: Context): List<SavedServer> =
        load(context).filter { it.rememberedByUser }

    fun remove(context: Context, serverId: String) {
        val existing = load(context).toMutableList()
        existing.removeAll { it.id == serverId }
        save(context, existing)
    }

    fun rename(context: Context, serverId: String, newName: String) {
        val trimmed = newName.trim()
        if (trimmed.isEmpty()) return

        val existing = load(context)
        val renamed = existing.map { server ->
            if (server.id == serverId) server.copy(name = trimmed) else server
        }
        if (renamed != existing) {
            save(context, renamed)
        }
    }

    fun updateWakeMac(context: Context, serverId: String, host: String, wakeMac: String?) {
        val normalizedWakeMac = SavedServer.normalizeWakeMac(wakeMac) ?: return
        val existing = load(context)
        val updated = existing.map { server ->
            if (server.id == serverId || normalizedHostKey(server.hostname) == normalizedHostKey(host)) {
                if (server.wakeMAC != normalizedWakeMac) server.copy(wakeMAC = normalizedWakeMac) else server
            } else {
                server
            }
        }
        if (updated != existing) {
            save(context, updated)
        }
    }

    private fun normalizedHostKey(host: String): String {
        val trimmed = host.trim().trimStart('[').trimEnd(']').replace("%25", "%")
        val withoutScope = if (!trimmed.contains(":")) {
            trimmed.substringBefore('%')
        } else {
            trimmed
        }
        return withoutScope.lowercase()
    }

    internal fun isLegacyV1Only(obj: JSONObject): Boolean {
        val id = obj.optString("id")
        val agentWire = obj.optString("alleycatAgentWire")
        if (id.startsWith("ssh-bridge:") || agentWire == "ssh-bridge") return false

        val hasLegacyPairingMarker = id.startsWith("alleycat:") ||
            obj.has("alleycatHost") ||
            obj.has("alleycatNodeId") ||
            obj.has("alleycatRelay")
        if (!hasLegacyPairingMarker) return false

        val directPorts = obj.optJSONArray("codexPorts")
        val hasDirectPort = (obj.optBoolean("hasCodexServer") && obj.optInt("port") > 0) ||
            (directPorts != null && (0 until directPorts.length()).any { directPorts.optInt(it) > 0 }) ||
            obj.optString("websocketURL").isNotBlank()
        val hasSshPath = obj.optInt("sshPort") > 0 ||
            obj.optString("source").equals("ssh", ignoreCase = true) ||
            obj.optString("preferredConnectionMode") == "ssh" ||
            obj.optBoolean("sshPortForwardingEnabled") ||
            (!obj.optBoolean("hasCodexServer") && obj.optInt("port") > 0)
        return !hasDirectPort && !hasSshPath
    }

    internal fun runtimeKindsNeedRewrite(obj: JSONObject, server: SavedServer): Boolean {
        val normalized = server.sshBridgeRuntimeKinds
        if (!obj.has("sshBridgeRuntimeKinds")) return normalized != null
        if (obj.isNull("sshBridgeRuntimeKinds")) return true
        val raw = obj.optJSONArray("sshBridgeRuntimeKinds") ?: return true
        val values = (0 until raw.length()).map { raw.optString(it) }
        return values != normalized
    }

    private val LEGACY_V1_JSON_KEYS = listOf(
        "alleycatHost",
        "alleycatNodeId",
        "alleycatRelay",
        "alleycatAgentName",
        "alleycatAgentWire",
    )
}

private fun List<String>.normalizeRuntimeKinds(): List<AgentRuntimeKind> =
    map { it.trim().lowercase() }.filter { it.isNotEmpty() }.distinct()
