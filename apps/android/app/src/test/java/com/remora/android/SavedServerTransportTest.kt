package com.remora.android

import com.remora.android.state.SavedServer
import com.remora.android.state.SavedServerStore
import com.remora.android.state.hasSupportedConnectionPath
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertThrows
import org.junit.Assert.assertTrue
import org.junit.Test
import org.json.JSONArray
import org.json.JSONObject

class SavedServerTransportTest {
    @Test
    fun currentPersistenceUsesVersionedRemoraNamespace() {
        assertEquals("remora_saved_servers_v2", SavedServerStore.PREFERENCES_NAME)
        assertEquals("saved_servers", SavedServerStore.VALUE_KEY)
    }

    @Test
    fun sshTrustCleanupJournalsBeforeUnpinAndFinalizesAfterward() {
        val steps = mutableListOf<String>()

        SavedServerStore.runTrustCleanupTransaction(
            begin = { steps += "begin" },
            unpin = { steps += "unpin" },
            finish = { steps += "finish" },
        )

        assertEquals(listOf("begin", "unpin", "finish"), steps)
    }

    @Test
    fun failedSshTrustCleanupKeepsServerAbsentAndJournalDurable() {
        val steps = mutableListOf<String>()

        SavedServerStore.runTrustCleanupTransaction(
            begin = { steps += "begin" },
            unpin = {
                steps += "unpin"
                throw IllegalStateException("unpin failed")
            },
            finish = { steps += "finish" },
        )

        assertEquals(listOf("begin", "unpin"), steps)
    }

    @Test
    fun failedSshTrustCleanupBeginNeverDeletesThePin() {
        val steps = mutableListOf<String>()

        assertThrows(IllegalStateException::class.java) {
            SavedServerStore.runTrustCleanupTransaction(
                begin = {
                    steps += "begin"
                    throw IllegalStateException("journal failed")
                },
                unpin = { steps += "unpin" },
                finish = { steps += "finish" },
            )
        }

        assertEquals(listOf("begin"), steps)
    }

    @Test
    fun sshTrustCleanupJournalRejectsMalformedTargetsWithoutThrowing() {
        assertNull(SavedServerStore.decodeSshTrustCleanupTarget("not-json"))
        assertNull(SavedServerStore.decodeSshTrustCleanupTarget("{}"))
        assertNull(
            SavedServerStore.decodeSshTrustCleanupTarget(
                JSONObject().put("host", " ").put("port", 22).toString(),
            ),
        )
        assertNull(
            SavedServerStore.decodeSshTrustCleanupTarget(
                JSONObject().put("host", "host.example").put("port", 0).toString(),
            ),
        )
        assertEquals(
            "host.example" to 2222,
            SavedServerStore.decodeSshTrustCleanupTarget(
                JSONObject().put("host", "host.example").put("port", 2222).toString(),
            ),
        )
    }

    @Test
    fun explicitBridgeSelectionAndNonBridgeNullRemainDistinct() {
        val bridge = SavedServer.fromJson(
            baseJson("bridge").apply {
                put("source", "ssh")
                put("port", 22)
                put("sshPort", 22)
                put("preferredConnectionMode", "ssh")
                put("sshBridgeRuntimeKinds", JSONArray(listOf("claude", "codex")))
            },
        )
        val direct = SavedServer.fromJson(baseJson("direct"))

        assertEquals(listOf("claude", "codex"), bridge.sshBridgeRuntimeKinds)
        assertTrue(bridge.hasSupportedConnectionPath)
        assertEquals(
            listOf("claude", "codex"),
            SavedServer.fromJson(bridge.toJson()).sshBridgeRuntimeKinds,
        )
        assertNull(direct.sshBridgeRuntimeKinds)
        assertFalse(direct.toJson().has("sshBridgeRuntimeKinds"))
        assertFalse(SavedServerStore.runtimeKindsNeedRewrite(bridge.toJson(), bridge))
    }

    @Test
    fun currentPersistenceKeepsDirectAndSshPathsAndRejectsUnusableRecords() {
        val direct = SavedServer.fromJson(baseJson("direct").apply {
            put("hasCodexServer", true)
            put("port", 8390)
            put("codexPorts", JSONArray(listOf(8390)))
        })
        val ssh = SavedServer.fromJson(baseJson("ssh").apply {
            put("source", "ssh")
            put("port", 22)
            put("sshPort", 22)
            put("preferredConnectionMode", "ssh")
        })
        val unusable = SavedServer.fromJson(baseJson("unusable"))

        assertTrue(direct.hasSupportedConnectionPath)
        assertEquals(8390, direct.directCodexPort)
        assertTrue(ssh.hasSupportedConnectionPath)
        assertEquals(22, ssh.resolvedSshPort)
        assertFalse(unusable.hasSupportedConnectionPath)
    }

    @Test
    fun removingLastSshServerUnpinsItsExactTrustTarget() {
        val ssh = SavedServer(
            id = "ssh",
            name = "SSH",
            hostname = "HOST.EXAMPLE",
            port = 22,
            sshPort = 2222,
            source = "ssh",
            preferredConnectionMode = "ssh",
        )
        val unpinned = mutableListOf<Pair<String, UShort>>()

        val remaining = SavedServerStore.removeServer(listOf(ssh), ssh.id) { host, port ->
            unpinned += host to port
        }

        assertTrue(remaining.isEmpty())
        assertEquals(listOf("HOST.EXAMPLE" to 2222u.toUShort()), unpinned)
    }

    @Test
    fun removingSharedSshTargetKeepsPinUntilLastReferenceIsGone() {
        val first = SavedServer(
            id = "ssh-1",
            name = "SSH 1",
            hostname = "HOST.EXAMPLE",
            port = 22,
            sshPort = 22,
            source = "ssh",
            preferredConnectionMode = "ssh",
        )
        val second = first.copy(id = "ssh-2", hostname = "host.example")
        val unpinned = mutableListOf<Pair<String, UShort>>()

        val remaining = SavedServerStore.removeServer(listOf(first, second), first.id) { host, port ->
            unpinned += host to port
        }

        assertEquals(listOf(second), remaining)
        assertTrue(unpinned.isEmpty())

        val empty = SavedServerStore.removeServer(remaining, second.id) { host, port ->
            unpinned += host to port
        }

        assertTrue(empty.isEmpty())
        assertEquals(listOf("host.example" to 22.toUShort()), unpinned)
    }

    @Test
    fun failedPinRemovalRetainsSavedServerForRetry() {
        val ssh = SavedServer(
            id = "ssh",
            name = "SSH",
            hostname = "host.example",
            port = 22,
            sshPort = 22,
            source = "ssh",
            preferredConnectionMode = "ssh",
        )
        val existing = listOf(ssh)

        val error = assertThrows(IllegalStateException::class.java) {
            SavedServerStore.removeServer(existing, ssh.id) { _, _ ->
                throw IllegalStateException("pin removal failed")
            }
        }

        assertEquals("pin removal failed", error.message)
    }

    @Test
    fun replacingSshEndpointUnpinsPreviousTrustTarget() {
        val previous = SavedServer(
            id = "ssh",
            name = "SSH",
            hostname = "old.example",
            port = 22,
            sshPort = 22,
            source = "ssh",
            preferredConnectionMode = "ssh",
        )
        val replacement = previous.copy(hostname = "new.example", sshPort = 2222)
        val unpinned = mutableListOf<Pair<String, UShort>>()

        val updated = SavedServerStore.replaceServer(listOf(previous), replacement) { host, port ->
            unpinned += host to port
        }

        assertEquals(listOf(replacement), updated)
        assertEquals(listOf("old.example" to 22.toUShort()), unpinned)
    }

    @Test
    fun replacingSharedSshEndpointKeepsReferencedPin() {
        val first = SavedServer(
            id = "ssh-1",
            name = "SSH 1",
            hostname = "HOST.EXAMPLE",
            port = 22,
            sshPort = 22,
            source = "ssh",
            preferredConnectionMode = "ssh",
        )
        val second = first.copy(id = "ssh-2", hostname = "host.example")
        val replacement = first.copy(hostname = "new.example")
        val unpinned = mutableListOf<Pair<String, UShort>>()

        val updated = SavedServerStore.replaceServer(
            listOf(first, second),
            replacement,
        ) { host, port ->
            unpinned += host to port
        }

        assertEquals(listOf(replacement, second), updated)
        assertTrue(unpinned.isEmpty())
    }

    @Test
    fun failedEndpointReplacementUnpinRetainsPreviousServer() {
        val previous = SavedServer(
            id = "ssh",
            name = "SSH",
            hostname = "old.example",
            port = 22,
            sshPort = 22,
            source = "ssh",
            preferredConnectionMode = "ssh",
        )
        val replacement = previous.copy(hostname = "new.example")
        val existing = listOf(previous)

        val error = assertThrows(IllegalStateException::class.java) {
            SavedServerStore.replaceServer(existing, replacement) { _, _ ->
                throw IllegalStateException("pin replacement failed")
            }
        }

        assertEquals("pin replacement failed", error.message)
    }

    @Test
    fun currentPersistenceRejectsRetiredAndUnknownFields() {
        val current = baseJson("current").apply {
            put("sshPort", 22)
            put("preferredConnectionMode", "ssh")
        }
        val retired = JSONObject(current.toString()).apply {
            put("sshPortForwardingEnabled", true)
        }
        val unknown = JSONObject(current.toString()).apply {
            put("unsupportedField", "discard")
        }

        assertTrue(SavedServerStore.hasOnlyCurrentFields(current))
        assertFalse(SavedServerStore.hasOnlyCurrentFields(retired))
        assertFalse(SavedServerStore.hasOnlyCurrentFields(unknown))
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
