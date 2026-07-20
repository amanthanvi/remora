package com.remora.android

import com.remora.android.state.SavedServer
import com.remora.android.state.SavedServerStore
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import org.json.JSONArray
import org.json.JSONObject

class SavedServerTransportTest {
    @Test
    fun historicalSshBridgeCsvMigratesToNormalizedRuntimeKinds() {
        val migrated = SavedServer.fromJson(
            baseJson("legacy-bridge").apply {
                put("alleycatAgentWire", "ssh-bridge")
                put("alleycatAgentName", " Codex,claude,CODEX, ")
            },
        )

        assertEquals(listOf("codex", "claude"), migrated.sshBridgeRuntimeKinds)
        assertFalse(migrated.toJson().has("alleycatAgentWire"))
        assertFalse(migrated.toJson().has("alleycatAgentName"))
    }

    @Test
    fun historicalSshBridgeIdWithoutSelectionMigratesToProbeAll() {
        val historical = baseJson("ssh-bridge:studio.local")
        val migrated = SavedServer.fromJson(historical)

        assertEquals(emptyList<String>(), migrated.sshBridgeRuntimeKinds)
        assertEquals(0, migrated.toJson().getJSONArray("sshBridgeRuntimeKinds").length())
        assertTrue(SavedServerStore.runtimeKindsNeedRewrite(historical, migrated))
    }

    @Test
    fun explicitBridgeSelectionAndNonBridgeNullRemainDistinct() {
        val bridge = SavedServer.fromJson(
            baseJson("bridge").put("sshBridgeRuntimeKinds", JSONArray(listOf("claude", "codex"))),
        )
        val direct = SavedServer.fromJson(baseJson("direct"))

        assertEquals(listOf("claude", "codex"), bridge.sshBridgeRuntimeKinds)
        assertNull(direct.sshBridgeRuntimeKinds)
        assertFalse(direct.toJson().has("sshBridgeRuntimeKinds"))
        assertFalse(SavedServerStore.runtimeKindsNeedRewrite(bridge.toJson(), bridge))
    }

    @Test
    fun v1OnlyPairingDropsButMixedDirectAndSshRecordSurvives() {
        val v1Only = baseJson("alleycat:node-1").apply {
            put("alleycatNodeId", "node-1")
            put("hasCodexServer", true)
        }
        val mixed = baseJson("mixed").apply {
            put("alleycatNodeId", "old-node")
            put("hasCodexServer", true)
            put("port", 8390)
            put("codexPorts", JSONArray(listOf(8390)))
            put("sshPort", 22)
        }

        assertTrue(SavedServerStore.isLegacyV1Only(v1Only))
        assertFalse(SavedServerStore.isLegacyV1Only(mixed))
        val decodedMixed = SavedServer.fromJson(mixed)
        assertEquals(listOf(8390), decodedMixed.codexPorts)
        assertEquals(22, decodedMixed.sshPort)
        assertFalse(decodedMixed.toJson().has("alleycatNodeId"))
    }

    @Test
    fun legacyPairingMetadataDoesNotDropImplicitSshPortRecord() {
        val mixed = baseJson("mixed-legacy-ssh").apply {
            put("alleycatNodeId", "old-node")
            put("hasCodexServer", false)
            put("port", 2222)
        }

        assertFalse(SavedServerStore.isLegacyV1Only(mixed))
        val decoded = SavedServer.fromJson(mixed)
        assertTrue(decoded.canConnectViaSsh)
        assertEquals(2222, decoded.resolvedSshPort)
        assertFalse(decoded.toJson().has("alleycatNodeId"))
    }

    @Test
    fun codexAndSshDiscoveryRequiresChoiceUntilPreferenceIsSet() {
        val server =
            SavedServer(
                id = "server-1",
                name = "Studio",
                hostname = "192.168.1.203",
                port = 8390,
                codexPorts = listOf(8390),
                sshPort = 22,
                hasCodexServer = true,
            )

        assertFalse(server.prefersSshConnection)
        assertTrue(server.requiresConnectionChoice)
        assertNull(server.directCodexPort)
    }

    @Test
    fun sshPreferenceForcesSshTransport() {
        val server =
            SavedServer(
                id = "server-2",
                name = "SSH Tunnel",
                hostname = "10.0.0.5",
                port = 8390,
                codexPorts = listOf(8390),
                sshPort = 22,
                hasCodexServer = true,
                preferredConnectionMode = "ssh",
            )

        assertTrue(server.prefersSshConnection)
        assertNull(server.directCodexPort)
        assertEquals(22, server.resolvedSshPort)
    }

    @Test
    fun legacyForwardingFlagMigratesToSshPreference() {
        val server =
            SavedServer(
                id = "server-3",
                name = "Old Saved Host",
                hostname = "192.168.1.203",
                port = 8390,
                codexPorts = listOf(8390),
                sshPort = 22,
                hasCodexServer = true,
                sshPortForwardingEnabled = true,
            )

        assertTrue(server.prefersSshConnection)
        assertNull(server.directCodexPort)
        assertEquals(22, server.resolvedSshPort)
    }

    @Test
    fun codexOnlyHostUsesDirectTransport() {
        val server =
            SavedServer(
                id = "server-4",
                name = "Codex",
                hostname = "10.0.0.4",
                port = 9234,
                codexPorts = listOf(9234),
                hasCodexServer = true,
            )

        assertFalse(server.prefersSshConnection)
        assertEquals(9234, server.directCodexPort)
    }

    private fun baseJson(id: String): JSONObject = JSONObject().apply {
        put("id", id)
        put("name", "Studio")
        put("hostname", "studio.local")
        put("port", 0)
        put("codexPorts", JSONArray())
        put("source", "manual")
        put("hasCodexServer", false)
        put("rememberedByUser", true)
    }
}
