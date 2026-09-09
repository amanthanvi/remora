package com.remora.android.state

import android.graphics.Bitmap
import android.graphics.Rect
import android.util.Log
import android.view.accessibility.AccessibilityNodeInfo
import androidx.lifecycle.Lifecycle
import androidx.test.core.app.ActivityScenario
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import com.remora.android.MainActivity
import kotlinx.coroutines.delay
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.withTimeout
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Assume.assumeTrue
import org.junit.Test
import org.junit.runner.RunWith
import java.io.File
import java.net.Socket
import uniffi.codex_mobile_client.AppStartThreadRequest
import uniffi.codex_mobile_client.HydratedConversationItemContent

/** Opt-in real SSH/app-server journey; the remote fixture completes a turn after 10 seconds. */
@RunWith(AndroidJUnit4::class)
class RemoteTurnResumeJourneyTest {
    @Test
    fun completedRemoteTurnReconcilesAfterBackgroundTransportLoss(): Unit = runBlocking {
        val args = InstrumentationRegistry.getArguments()
        assumeTrue("Requires disposable remote fixture", args.containsKey("remoteSshPort"))
        val port = requireNotNull(args.getString("remoteSshPort")).toInt()
        val password = requireNotNull(args.getString("remoteSshPassword"))
        val controlPort = requireNotNull(args.getString("remoteControlPort")).toInt()
        val host = "10.0.2.2"
        val serverId = "remote-resume-journey-$port"
        val context = InstrumentationRegistry.getInstrumentation().targetContext
        val credentials = SshCredentialStore(context)
        val trust = SshTrustStore(context)
        check(SavedServerStore.remembered(context).none {
            it.id == serverId || (it.hostname == host && it.port == port)
        }) { "Fixture endpoint already has a saved server" }
        check(credentials.load(host, port) == null) { "Fixture endpoint already has saved credentials" }
        check(trust.read(host, port.toUShort()) == null) { "Fixture endpoint already has a pinned host" }
        File(context.cacheDir, "remote-resume-journey.png").delete()
        File(context.cacheDir, "remote-resume-accessibility.txt").delete()
        val server = SavedServer(
            id = serverId, name = "Remote Resume Journey", hostname = host, port = port,
            source = "ssh", preferredConnectionMode = "ssh", rememberedByUser = true,
        )
        ActivityScenario.launch(MainActivity::class.java).use { scenario ->
            val model = AppModel.shared
            try {
                withTimeout(120_000) {
                    model.serverBridge.connectRemoteOverSsh(
                        serverId, server.name, host, port.toUShort(), "remora-test", password,
                        null, null, false, true, null,
                    )
                    credentials.save(host, port, SavedSshCredential("remora-test", SshAuthMethod.PASSWORD, password))
                    SavedServerStore.remember(context, server)
                    model.reconnectController.syncSavedServers(SavedServerStore.remembered(context).map { it.toRecord() })
                    val key = model.startThread(serverId, AppStartThreadRequest(
                        model = null, cwd = null, approvalPolicy = null, sandbox = null,
                        developerInstructions = null, persistExtendedHistory = true, dynamicTools = null,
                    ))
                    model.activateThread(key)
                    model.startTurn(key, AppComposerPayload(text = "Remote resume verification only."))
                    while (model.threadSnapshot(key)?.activeTurnId == null) delay(100)
                    Log.i("RemoteResumeJourney", "active remote turn observed")
                    scenario.moveToState(Lifecycle.State.CREATED)
                    // Model a socket lost during OS suspension, not a server-side turn cancellation.
                    setTransportAvailable(controlPort, false)
                    delay(20_000)
                    assertFalse(model.threadSnapshot(key)?.hydratedConversationItems.orEmpty().any {
                        (it.content as? HydratedConversationItemContent.Assistant)?.v1?.text == "REMOTE_RESUME_COMPLETE"
                    })
                    setTransportAvailable(controlPort, true)
                    scenario.moveToState(Lifecycle.State.RESUMED)
                    while (true) {
                        val thread = model.threadSnapshot(key)
                        val text = thread?.hydratedConversationItems?.mapNotNull {
                            (it.content as? HydratedConversationItemContent.Assistant)?.v1?.text
                        }.orEmpty()
                        if (thread?.activeTurnId == null && text.contains("REMOTE_RESUME_COMPLETE")) break
                        delay(200)
                    }
                    val thread = model.threadSnapshot(key)!!
                    assertFalse(thread.hasActiveTurn)
                    assertEquals(1, thread.hydratedConversationItems.count {
                        (it.content as? HydratedConversationItemContent.Assistant)?.v1?.text == "REMOTE_RESUME_COMPLETE"
                    })
                    val userIndex = thread.hydratedConversationItems.indexOfFirst {
                        (it.content as? HydratedConversationItemContent.User)?.v1?.text == "Remote resume verification only."
                    }
                    val assistantIndex = thread.hydratedConversationItems.indexOfFirst {
                        (it.content as? HydratedConversationItemContent.Assistant)?.v1?.text == "REMOTE_RESUME_COMPLETE"
                    }
                    assertTrue("User prompt must precede recovered completion", userIndex >= 0 && assistantIndex > userIndex)
                    assertTrue(model.snapshot.value!!.servers.any { it.serverId == serverId })
                    val instrumentation = InstrumentationRegistry.getInstrumentation()
                    var userVisible = false
                    var assistantVisible = false
                    try {
                        withTimeout(15_000) {
                            while (true) {
                                instrumentation.waitForIdleSync()
                                val root = instrumentation.uiAutomation.rootInActiveWindow
                                userVisible = hasVisibleText(root, "Remote resume verification only.")
                                assistantVisible = hasVisibleText(root, "REMOTE_RESUME_COMPLETE")
                                if (userVisible && assistantVisible) break
                                delay(100)
                            }
                        }
                    } finally {
                        val tree = StringBuilder()
                        val observed = model.threadSnapshot(key)
                        tree.append("time=${System.currentTimeMillis()} key=$key active=${model.snapshot.value?.activeThread} lifecycle=${scenario.state}\n")
                        tree.append("userVisible=$userVisible assistantVisible=$assistantVisible activeTurn=${observed?.activeTurnId} status=${observed?.info?.status}\n")
                        tree.append("projectedItems=${observed?.hydratedConversationItems}\n")
                        fun appendNode(node: AccessibilityNodeInfo, depth: Int) {
                            val bounds = Rect()
                            node.getBoundsInScreen(bounds)
                            tree.append("$depth class=${node.className} visible=${node.isVisibleToUser} bounds=$bounds text=${node.text} label=${node.contentDescription}\n")
                            for (index in 0 until node.childCount) {
                                node.getChild(index)?.let { appendNode(it, depth + 1) }
                            }
                        }
                        instrumentation.uiAutomation.rootInActiveWindow?.let { appendNode(it, 0) }
                        File(context.cacheDir, "remote-resume-accessibility.txt").writeText(tree.toString())
                        File(context.cacheDir, "remote-resume-journey.png").outputStream().use { output ->
                            val screenshot = instrumentation.uiAutomation.takeScreenshot()
                            check(checkNotNull(screenshot).compress(Bitmap.CompressFormat.PNG, 100, output))
                            screenshot.recycle()
                        }
                    }
                    Log.i("RemoteResumeJourney", "PASS: missed remote completion recovered exactly once after activity background and SSH reconnect")
                }
            } finally {
                SavedServerStore.remove(context, serverId)
                model.reconnectController.syncSavedServers(SavedServerStore.remembered(context).map { it.toRecord() })
                credentials.delete(host, port)
                trust.remove(host, port.toUShort())
                model.serverBridge.disconnectServer(serverId)
                check(SavedServerStore.remembered(context).none { it.id == serverId })
                check(credentials.load(host, port) == null)
                check(trust.read(host, port.toUShort()) == null)
                Log.i("RemoteResumeJourney", "CLEANED: fixture saved host, credentials, and trust")
            }
        }
    }

    private fun hasVisibleText(node: AccessibilityNodeInfo?, text: String): Boolean {
        if (node == null) return false
        if (node.isVisibleToUser && node.text?.toString() == text) return true
        return (0 until node.childCount).any { hasVisibleText(node.getChild(it), text) }
    }

    private fun setTransportAvailable(port: Int, available: Boolean) {
        Socket("10.0.2.2", port).use { socket ->
            socket.soTimeout = 5_000
            val path = if (available) "reconnect" else "disconnect"
            socket.getOutputStream().write("POST /$path HTTP/1.0\r\nHost: localhost\r\nContent-Length: 0\r\n\r\n".toByteArray())
            check(socket.getInputStream().bufferedReader().readLine().contains("200 OK"))
        }
    }
}
