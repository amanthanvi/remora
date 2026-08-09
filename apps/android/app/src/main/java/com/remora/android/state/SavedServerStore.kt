package com.remora.android.state

import android.annotation.SuppressLint
import android.content.Context
import android.content.SharedPreferences
import com.remora.android.ui.common.AgentRuntimeKind
import java.io.File
import org.json.JSONArray
import org.json.JSONObject
import uniffi.codex_mobile_client.AppDiscoveredServer
import uniffi.codex_mobile_client.AppDiscoverySource
import uniffi.codex_mobile_client.SavedServerRecord
import uniffi.codex_mobile_client.TerminalSshTrustStore

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
            else -> null
        }

    val prefersSshConnection: Boolean
        get() = resolvedPreferredConnectionMode == "ssh"

    val canConnectViaSsh: Boolean
        get() = websocketURL == null && (
            sshPort != null ||
                source == "ssh" ||
                (!hasCodexServer && port > 0) ||
                preferredConnectionMode == "ssh"
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
            val sshBridgeRuntimeKinds = when {
                obj.has("sshBridgeRuntimeKinds") && !obj.isNull("sshBridgeRuntimeKinds") ->
                    obj.optJSONArray("sshBridgeRuntimeKinds")
                        ?.toNormalizedRuntimeKinds()
                        ?: emptyList()
                obj.has("sshBridgeRuntimeKinds") -> null
                else -> null
            }

            return SavedServer(
                id = obj.getString("id"),
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
    sshPortForwardingEnabled = null,
    websocketUrl = websocketURL,
    rememberedByUser = rememberedByUser,
    sshBridgeRuntimeKinds = sshBridgeRuntimeKinds,
)

object SavedServerStore {
    internal const val PREFERENCES_NAME = "remora_saved_servers_v2"
    internal const val VALUE_KEY = "saved_servers"
    private const val PENDING_SSH_TRUST_CLEANUP_KEY = "pending_ssh_trust_cleanup"
    private val currentFields = setOf(
        "id",
        "name",
        "hostname",
        "port",
        "codexPorts",
        "sshPort",
        "source",
        "hasCodexServer",
        "wakeMAC",
        "preferredConnectionMode",
        "preferredCodexPort",
        "websocketURL",
        "os",
        "sshBanner",
        "rememberedByUser",
        "sshBridgeRuntimeKinds",
    )

    private fun prefs(context: Context): SharedPreferences =
        context.getSharedPreferences(PREFERENCES_NAME, Context.MODE_PRIVATE)

    @Synchronized
    fun load(context: Context): List<SavedServer> {
        recoverPendingSshTrustCleanup(context)
        val json = prefs(context).getString(VALUE_KEY, null) ?: return emptyList()
        return try {
            val array = JSONArray(json)
            val decoded = (0 until array.length()).mapNotNull { index ->
                val objectValue = array.optJSONObject(index) ?: return@mapNotNull null
                if (!hasOnlyCurrentFields(objectValue)) return@mapNotNull null
                runCatching { objectValue to SavedServer.fromJson(objectValue) }.getOrNull()
            }
            val retained = decoded.filter { (_, server) -> server.hasSupportedConnectionPath }
            val normalized = retained.map { (_, server) -> server.normalizedForPersistence() }
            val needsRewrite = decoded.size != array.length() ||
                retained.size != decoded.size ||
                retained.any { (objectValue, server) -> runtimeKindsNeedRewrite(objectValue, server) } ||
                retained.map { it.second } != normalized
            if (needsRewrite) {
                save(context, normalized)
            }
            normalized
        } catch (_: Exception) {
            emptyList()
        }
    }

    @Synchronized
    fun save(context: Context, servers: List<SavedServer>) {
        prefs(context).edit().putString(VALUE_KEY, encodeServers(servers)).apply()
    }

    @Synchronized
    fun upsert(context: Context, server: SavedServer) {
        val existing = load(context).toMutableList()
        val prior = existing.firstOrNull { it.id == server.id || it.deduplicationKey == server.deduplicationKey }
        existing.removeAll { it.id == server.id || it.deduplicationKey == server.deduplicationKey }
        existing.add(server.copy(rememberedByUser = prior?.rememberedByUser ?: server.rememberedByUser))
        save(context, existing)
    }

    @Synchronized
    fun replace(context: Context, server: SavedServer) {
        val existing = load(context)
        persistServerMutation(context, existing, planServerReplacement(existing, server))
    }

    internal fun replaceServer(
        existing: List<SavedServer>,
        server: SavedServer,
        unpin: (String, UShort) -> Unit,
    ): List<SavedServer> {
        val mutation = planServerReplacement(existing, server)
        mutation.trustTarget?.let { (host, port) -> unpin(host, port.toUShort()) }
        return mutation.servers
    }

    private fun planServerReplacement(
        existing: List<SavedServer>,
        server: SavedServer,
    ): SavedServerMutation {
        val index = existing.indexOfFirst { it.id == server.id }
        if (index == -1) return SavedServerMutation(existing + server, null)

        val previous = existing[index]
        val updated = existing.toMutableList().apply { this[index] = server }
        val target = previous.sshTrustTarget() ?: return SavedServerMutation(updated, null)
        val identity = sshTrustIdentity(target.first, target.second)
        val stillReferenced = updated
            .mapNotNull { it.sshTrustTarget() }
            .any { sshTrustIdentity(it.first, it.second) == identity }
        return SavedServerMutation(updated, target.takeUnless { stillReferenced })
    }

    @Synchronized
    fun remember(context: Context, server: SavedServer) {
        val existing = load(context).toMutableList()
        existing.removeAll { it.id == server.id || it.deduplicationKey == server.deduplicationKey }
        existing.add(server.copy(rememberedByUser = true))
        save(context, existing)
    }

    @Synchronized
    fun remembered(context: Context): List<SavedServer> =
        load(context).filter { it.rememberedByUser }

    @Synchronized
    fun remove(context: Context, serverId: String) {
        val existing = load(context)
        persistServerMutation(context, existing, planServerRemoval(existing, serverId))
    }

    internal fun removeServer(
        existing: List<SavedServer>,
        serverId: String,
        unpin: (String, UShort) -> Unit,
    ): List<SavedServer> {
        val mutation = planServerRemoval(existing, serverId)
        mutation.trustTarget?.let { (host, port) -> unpin(host, port.toUShort()) }
        return mutation.servers
    }

    private fun planServerRemoval(
        existing: List<SavedServer>,
        serverId: String,
    ): SavedServerMutation {
        val removed = existing.firstOrNull { it.id == serverId }
        val remaining = existing.filterNot { it.id == serverId }
        val target = removed?.sshTrustTarget() ?: return SavedServerMutation(remaining, null)
        val targetIdentity = sshTrustIdentity(target.first, target.second)
        val stillReferenced = remaining
            .mapNotNull { it.sshTrustTarget() }
            .any { sshTrustIdentity(it.first, it.second) == targetIdentity }
        return SavedServerMutation(remaining, target.takeUnless { stillReferenced })
    }

    private data class SavedServerMutation(
        val servers: List<SavedServer>,
        val trustTarget: Pair<String, Int>?,
    )

    @SuppressLint("ApplySharedPref", "UseKtx")
    private fun persistServerMutation(
        context: Context,
        existing: List<SavedServer>,
        mutation: SavedServerMutation,
    ) {
        val target = mutation.trustTarget
        if (target == null) {
            save(context, mutation.servers)
            return
        }

        val preferences = prefs(context)
        val rollbackJson = encodeServers(existing)
        val updatedJson = encodeServers(mutation.servers)
        val journal = JSONObject()
            .put("host", target.first)
            .put("port", target.second)
            .toString()
        runTrustCleanupTransaction(
            begin = {
                if (!preferences.edit()
                        .putString(VALUE_KEY, updatedJson)
                        .putString(PENDING_SSH_TRUST_CLEANUP_KEY, journal)
                        .commit()
                ) {
                    val restored = preferences.edit()
                        .putString(VALUE_KEY, rollbackJson)
                        .remove(PENDING_SSH_TRUST_CLEANUP_KEY)
                        .commit()
                    error(
                        if (restored) {
                            "Unable to persist the SSH trust cleanup transaction"
                        } else {
                            "Unable to persist or roll back the SSH trust cleanup transaction"
                        },
                    )
                }
            },
            unpin = {
                TerminalSshTrustStore(SshTrustStore(context))
                    .unpin(target.first, target.second.toUShort())
            },
            finish = {
                // Failure leaves the durable journal intact; the next load
                // retries the idempotent unpin before exposing the server list.
                preferences.edit().remove(PENDING_SSH_TRUST_CLEANUP_KEY).commit()
            },
        )
    }

    @SuppressLint("ApplySharedPref", "UseKtx")
    private fun recoverPendingSshTrustCleanup(context: Context) {
        val preferences = prefs(context)
        val encoded = preferences.getString(PENDING_SSH_TRUST_CLEANUP_KEY, null) ?: return
        val journal = try {
            JSONObject(encoded)
        } catch (error: Exception) {
            throw IllegalStateException("Unreadable SSH trust cleanup journal", error)
        }
        val host = journal.optString("host").takeIf { it.isNotBlank() }
            ?: error("SSH trust cleanup journal is missing its host")
        val port = journal.optInt("port").takeIf { it in 1..UShort.MAX_VALUE.toInt() }
            ?: error("SSH trust cleanup journal has an invalid port")
        try {
            TerminalSshTrustStore(SshTrustStore(context)).unpin(host, port.toUShort())
        } catch (_: Exception) {
            // The trust-store commit result is ambiguous: its in-memory map may
            // already have removed the pin. Keep both the server deletion and
            // journal so no reconnect can downgrade to first-use trust.
            return
        }
        // A failed clear remains safe: the durable journal causes another
        // idempotent cleanup attempt on the next process start.
        preferences.edit().remove(PENDING_SSH_TRUST_CLEANUP_KEY).commit()
    }

    private fun encodeServers(servers: List<SavedServer>): String =
        JSONArray().apply { servers.forEach { put(it.toJson()) } }.toString()

    internal fun runTrustCleanupTransaction(
        begin: () -> Unit,
        unpin: () -> Unit,
        finish: () -> Unit,
    ) {
        begin()
        try {
            unpin()
        } catch (_: Exception) {
            // Leave the journal durable and the server absent. A later load
            // retries cleanup without exposing an ambiguous first-use path.
            return
        }
        finish()
    }

    private fun SavedServer.sshTrustTarget(): Pair<String, Int>? =
        if (canConnectViaSsh && resolvedSshPort in 1..UShort.MAX_VALUE.toInt()) {
            hostname to resolvedSshPort
        } else {
            null
        }

    private fun sshTrustIdentity(host: String, port: Int): Pair<String, Int> =
        normalizedHostKey(host) to port

    @SuppressLint("ApplySharedPref", "UseKtx") // Callers require a synchronous durability result before marking cutover complete.
    @Synchronized
    fun removeAllForSecurityCutover(context: Context): Boolean {
        val currentRemoved = prefs(context).edit().clear().commit() &&
            prefs(context).all.isEmpty()
        val retiredName = retiredSavedServerPreferencesName()
        val sharedPreferencesDirectory = File(context.dataDir, "shared_prefs")
        val retiredFiles = listOf(
            File(sharedPreferencesDirectory, "$retiredName.xml"),
            File(sharedPreferencesDirectory, "$retiredName.xml.bak"),
        )
        val retiredRemoved = retiredFiles.none(File::exists) ||
            (
                context.deleteSharedPreferences(retiredName) &&
                    retiredFiles.none(File::exists)
            )
        return currentRemoved && retiredRemoved
    }

    @Synchronized
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

    @Synchronized
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

    internal fun runtimeKindsNeedRewrite(obj: JSONObject, server: SavedServer): Boolean {
        val normalized = server.sshBridgeRuntimeKinds
        if (!obj.has("sshBridgeRuntimeKinds")) return normalized != null
        if (obj.isNull("sshBridgeRuntimeKinds")) return true
        val raw = obj.optJSONArray("sshBridgeRuntimeKinds") ?: return true
        val values = (0 until raw.length()).map { raw.optString(it) }
        return values != normalized
    }

    internal fun hasOnlyCurrentFields(obj: JSONObject): Boolean =
        obj.keys().asSequence().all(currentFields::contains)

}

/**
 * One-release deletion tombstone for the unsupported pre-1.6 saved-host file.
 *
 * The historical identifier is reconstructed only to destroy the retired
 * namespace; SavedServerStore never reads or migrates from it. Remove this
 * tombstone when the direct-upgrade floor advances beyond 1.6.
 */
internal fun retiredSavedServerPreferencesName(): String =
    byteArrayOf(
        99, 111, 100, 101, 120, 95, 115, 97, 118, 101, 100,
        95, 115, 101, 114, 118, 101, 114, 115, 95, 112, 114,
        101, 102, 115,
    ).toString(Charsets.UTF_8)

internal val SavedServer.hasSupportedConnectionPath: Boolean
    get() = websocketURL?.isNotBlank() == true ||
        availableDirectCodexPorts.isNotEmpty() ||
        canConnectViaSsh

private fun List<String>.normalizeRuntimeKinds(): List<AgentRuntimeKind> =
    map { it.trim().lowercase() }.filter { it.isNotEmpty() }.distinct()
