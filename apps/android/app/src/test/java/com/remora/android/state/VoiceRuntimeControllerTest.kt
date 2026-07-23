package com.remora.android.state

import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.cancelAndJoin
import kotlinx.coroutines.channels.Channel
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.withTimeout
import org.junit.Assert.assertSame
import org.junit.Test
import uniffi.codex_mobile_client.AppRealtimeStartedNotification
import uniffi.codex_mobile_client.AppStoreSubscriptionInterface
import uniffi.codex_mobile_client.AppStoreUpdateRecord
import uniffi.codex_mobile_client.ThreadKey

class VoiceRuntimeControllerTest {
    @Test
    fun realtimeStartedEmittedImmediatelyAfterReceiverStartIsHandled() = runBlocking {
        val updates = Channel<AppStoreUpdateRecord>()
        val handled = CompletableDeferred<AppStoreUpdateRecord>()
        val subscription = object : AppStoreSubscriptionInterface {
            override suspend fun nextUpdate(): AppStoreUpdateRecord = updates.receive()
        }
        val started = AppStoreUpdateRecord.RealtimeStarted(
            key = ThreadKey(serverId = "local", threadId = "thread-1"),
            notification = AppRealtimeStartedNotification(
                threadId = "thread-1",
                sessionId = "voice-session-1",
                version = "1",
            ),
        )

        val receiverJob = launchRealtimeUpdateLoop(
            subscription = subscription,
            onUpdate = { handled.complete(it) },
            onFailure = { handled.completeExceptionally(it) },
        )
        updates.trySend(started).getOrThrow()

        assertSame(started, withTimeout(2_000) { handled.await() })
        receiverJob.cancelAndJoin()
    }
}
