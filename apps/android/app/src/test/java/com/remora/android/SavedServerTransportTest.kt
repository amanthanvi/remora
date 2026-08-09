package com.remora.android

import com.remora.android.state.SavedServer
import com.remora.android.state.SavedServerStore
import com.remora.android.state.SshTrustCleanupOutcome
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

        val outcome = SavedServerStore.runTrustCleanupTransaction(
            begin = { steps += "begin" },
            unpin = { steps += "unpin" },
            finish = { steps += "finish" },
        )

        assertEquals(SshTrustCleanupOutcome.Complete, outcome)
        assertEquals(listOf("begin", "unpin", "finish"), steps)
    }

    @Test
    fun failedSshTrustCleanupKeepsServerAbsentAndJournalDurable() {
        val steps = mutableListOf<String>()

        val outcome = SavedServerStore.runTrustCleanupTransaction(
            begin = { steps += "begin" },
            unpin = {
                steps += "unpin"
                throw IllegalStateException("unpin failed")
            },
            finish = { steps += "finish" },
        )

        assertEquals(SshTrustCleanupOutcome.Pending, outcome)
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
        val encoded = SavedServerStore.encodeSshTrustCleanupJournal(
            host = "host.example",
            port = 2222,
            fingerprint = "SHA256:original",
        )
        assertEquals("SHA256:original", JSONObject(encoded).getString("fingerprint"))
    }

    @Test
    fun sshTrustCleanupJournalRoundTripsEveryUniqueTarget() {
        val encoded = SavedServerStore.encodeSshTrustCleanupJournal(
            listOf(
                Triple("first.example", 22, "SHA256:first"),
                Triple("second.example", 2222, null),
            ),
        )

        assertEquals(
            listOf("first.example" to 22, "second.example" to 2222),
            SavedServerStore.decodeSshTrustCleanupTargets(encoded),
        )
    }

    @Test
    fun pendingSshTrustCleanupBlocksAnotherTrustTargetMutation() {
        SavedServerStore.ensureNoPendingSshTrustCleanup(null)

        val error = assertThrows(IllegalStateException::class.java) {
            SavedServerStore.ensureNoPendingSshTrustCleanup("pending-target")
        }

        assertTrue(error.message.orEmpty().contains("still pending"))
    }

    @Test
    fun pendingCleanupSkipsUnpinWhenSavedListReferencesNormalizedTarget() {
        val active = SavedServer(
            id = "ssh",
            name = "SSH",
            hostname = "first.example",
            port = 22,
            sshPort = 22,
            source = "ssh",
            preferredConnectionMode = "ssh",
        )
        val encodedServers = JSONArray().put(active.toJson()).toString()
        val steps = mutableListOf<String>()

        SavedServerStore.runPendingSshTrustCleanupRecovery(
            encodedServers = encodedServers,
            host = "[FIRST.EXAMPLE]",
            port = 22,
            unpin = { steps += "unpin" },
            finish = { steps += "finish" },
        )

        assertEquals(listOf("finish"), steps)
    }

    @Test
    fun pendingCleanupStillUnpinsSameHostOnDifferentPort() {
        val active = SavedServer(
            id = "ssh",
            name = "SSH",
            hostname = "first.example",
            port = 2222,
            sshPort = 2222,
            source = "ssh",
            preferredConnectionMode = "ssh",
        )
        val encodedServers = JSONArray().put(active.toJson()).toString()
        val steps = mutableListOf<String>()

        SavedServerStore.runPendingSshTrustCleanupRecovery(
            encodedServers = encodedServers,
            host = "First.Example",
            port = 22,
            unpin = { steps += "unpin" },
            finish = { steps += "finish" },
        )

        assertEquals(listOf("unpin", "finish"), steps)
    }

    @Test
    fun multiTargetCleanupKeepsJournalUntilEveryUnpinSucceeds() {
        val targets = listOf("first.example" to 22, "second.example" to 2222)
        val steps = mutableListOf<String>()

        SavedServerStore.runPendingSshTrustCleanupRecovery(
            encodedServers = null,
            targets = targets,
            unpin = { host, _ ->
                steps += host
                if (host == "second.example") throw IllegalStateException("unavailable")
            },
            finish = { steps += "finish" },
        )

        assertEquals(listOf("first.example", "second.example"), steps)
    }

    @Test
    fun upsertCancelsPendingCleanupForSameNormalizedTarget() {
        val readded = SavedServer(
            id = "ssh",
            name = "SSH",
            hostname = "first.example",
            port = 22,
            sshPort = 22,
            source = "ssh",
            preferredConnectionMode = "ssh",
        )
        val pendingCleanup = SavedServerStore.encodeSshTrustCleanupJournal(
            host = "[FIRST.EXAMPLE]",
            port = 22,
            fingerprint = "SHA256:original",
        )
        val recoverySteps = mutableListOf<String>()
        val restorationSteps = mutableListOf<String>()
        var persisted = emptyList<SavedServer>()
        var cancelledPendingCleanup = false

        SavedServerStore.upsert(
            pendingCleanup = pendingCleanup,
            server = readded,
            loadServers = { recoverPendingCleanup ->
                if (recoverPendingCleanup) recoverySteps += "unpin"
                emptyList()
            },
            restorePendingTrust = { host, port, fingerprint ->
                restorationSteps += "$host:$port=$fingerprint"
            },
        ) { servers, cancelsPendingCleanup ->
            restorationSteps += "persist"
            persisted = servers
            cancelledPendingCleanup = cancelsPendingCleanup
        }

        assertTrue(recoverySteps.isEmpty())
        assertEquals(listOf("[FIRST.EXAMPLE]:22=SHA256:original", "persist"), restorationSteps)
        assertTrue(cancelledPendingCleanup)
        assertEquals(listOf(readded), persisted)
    }

    @Test
    fun upsertIntoMultiTargetCleanupRestoresMatchAndKeepsRemainingJournal() {
        val readded = SavedServer(
            id = "ssh",
            name = "SSH",
            hostname = "first.example",
            port = 22,
            sshPort = 22,
            source = "ssh",
            preferredConnectionMode = "ssh",
        )
        val pendingCleanup = SavedServerStore.encodeSshTrustCleanupJournal(
            listOf(
                Triple("first.example", 22, "SHA256:first"),
                Triple("second.example", 2222, "SHA256:second"),
            ),
        )
        val steps = mutableListOf<String>()
        var cancelledPendingCleanup = true

        SavedServerStore.upsert(
            pendingCleanup = pendingCleanup,
            server = readded,
            loadServers = { recoverPendingCleanup ->
                steps += "load:$recoverPendingCleanup"
                emptyList()
            },
            restorePendingTrust = { host, port, fingerprint ->
                steps += "restore:$host:$port=$fingerprint"
            },
        ) { _, cancelsPendingCleanup ->
            steps += "persist"
            cancelledPendingCleanup = cancelsPendingCleanup
        }

        assertEquals(
            listOf("restore:first.example:22=SHA256:first", "load:false", "persist"),
            steps,
        )
        assertFalse(cancelledPendingCleanup)
    }

    @Test
    fun rememberUsesGuardedReaddPathAndForcesRememberedFlag() {
        val readded = SavedServer(
            id = "ssh",
            name = "SSH",
            hostname = "first.example",
            port = 22,
            sshPort = 22,
            source = "ssh",
            preferredConnectionMode = "ssh",
        )
        val pendingCleanup = SavedServerStore.encodeSshTrustCleanupJournal(
            host = "first.example",
            port = 22,
            fingerprint = "SHA256:original",
        )
        val steps = mutableListOf<String>()
        var persisted = emptyList<SavedServer>()

        SavedServerStore.upsert(
            pendingCleanup = pendingCleanup,
            server = readded,
            forceRemembered = true,
            loadServers = { recoverPendingCleanup ->
                if (recoverPendingCleanup) steps += "unpin"
                emptyList()
            },
            restorePendingTrust = { _, _, _ -> steps += "restore" },
        ) { servers, _ ->
            steps += "persist"
            persisted = servers
        }

        assertEquals(listOf("restore", "persist"), steps)
        assertTrue(persisted.single().rememberedByUser)
    }

    @Test
    fun matchingLegacyCleanupJournalBlocksReaddBeforeRecovery() {
        val readded = SavedServer(
            id = "ssh",
            name = "SSH",
            hostname = "first.example",
            port = 22,
            sshPort = 22,
            source = "ssh",
            preferredConnectionMode = "ssh",
        )
        val pendingCleanup = JSONObject()
            .put("host", "first.example")
            .put("port", 22)
            .toString()
        val steps = mutableListOf<String>()

        val error = assertThrows(IllegalStateException::class.java) {
            SavedServerStore.upsert(
                pendingCleanup = pendingCleanup,
                server = readded,
                loadServers = {
                    steps += "load"
                    emptyList()
                },
            ) { _, _ -> steps += "persist" }
        }

        assertTrue(error.message.orEmpty().contains("lacks the original fingerprint"))
        assertTrue(steps.isEmpty())
    }

    @Test
    fun ambiguousPinRestoreSucceedsOnlyWhenFingerprintCanBeVerified() {
        var stored: String? = null

        SavedServerStore.restorePendingSshTrust(
            originalFingerprint = "SHA256:original",
            pinned = { stored },
            pin = { fingerprint ->
                stored = fingerprint
                throw IllegalStateException("ambiguous write")
            },
        )

        assertEquals("SHA256:original", stored)
    }

    @Test
    fun changedFingerprintBlocksPendingCleanupCancellation() {
        var pinAttempted = false

        val error = assertThrows(IllegalStateException::class.java) {
            SavedServerStore.restorePendingSshTrust(
                originalFingerprint = "SHA256:original",
                pinned = { "SHA256:changed" },
                pin = { pinAttempted = true },
            )
        }

        assertTrue(error.message.orEmpty().contains("fingerprint changed"))
        assertFalse(pinAttempted)
    }

    @Test
    fun upsertDoesNotCancelPendingCleanupForDifferentPort() {
        val readded = SavedServer(
            id = "ssh",
            name = "SSH",
            hostname = "first.example",
            port = 2222,
            sshPort = 2222,
            source = "ssh",
            preferredConnectionMode = "ssh",
        )
        val pendingCleanup = JSONObject()
            .put("host", "First.Example")
            .put("port", 22)
            .toString()
        val recoverySteps = mutableListOf<String>()
        var cancelledPendingCleanup = true

        SavedServerStore.upsert(
            pendingCleanup = pendingCleanup,
            server = readded,
            loadServers = { recoverPendingCleanup ->
                if (recoverPendingCleanup) recoverySteps += "unpin"
                emptyList()
            },
        ) { _, cancelsPendingCleanup ->
            cancelledPendingCleanup = cancelsPendingCleanup
        }

        assertEquals(listOf("unpin"), recoverySteps)
        assertFalse(cancelledPendingCleanup)
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
    fun removingDuplicateServerIdsUnpinsEveryOrphanedTrustTarget() {
        val first = SavedServer(
            id = "duplicate",
            name = "First",
            hostname = "first.example",
            port = 22,
            sshPort = 22,
            source = "ssh",
            preferredConnectionMode = "ssh",
        )
        val second = first.copy(name = "Second", hostname = "second.example", sshPort = 2222)
        val unpinned = mutableListOf<Pair<String, UShort>>()

        val remaining = SavedServerStore.removeServer(listOf(first, second), "duplicate") { host, port ->
            unpinned += host to port
        }

        assertTrue(remaining.isEmpty())
        assertEquals(
            listOf("first.example" to 22.toUShort(), "second.example" to 2222.toUShort()),
            unpinned,
        )
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
