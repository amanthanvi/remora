package com.remora.android.state

import java.util.concurrent.CountDownLatch
import java.util.concurrent.Executors
import java.util.concurrent.TimeUnit
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.NonCancellable
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.awaitCancellation
import kotlinx.coroutines.cancel
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.withContext
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class AppModelProjectionOwnerTest {
    @Test
    fun activationAndStreamingCommitTogetherWithTheirCache() {
        val owner = AppModelProjectionOwner()
        data class Projection(val active: String = "old", val text: String = "")
        var snapshot = Projection()
        val cache = mutableMapOf("thread" to "")
        val started = CountDownLatch(1)
        val release = CountDownLatch(1)
        val pool = Executors.newFixedThreadPool(2)
        try {
            val stream = pool.submit {
                owner.write {
                    val before = snapshot
                    started.countDown()
                    assertTrue(release.await(5, TimeUnit.SECONDS))
                    snapshot = before.copy(text = "delta")
                    cache["thread"] = "delta"
                }
            }
            assertTrue(started.await(5, TimeUnit.SECONDS))
            val activate = pool.submit {
                owner.write { snapshot = snapshot.copy(active = "new") }
                owner.read { assertEquals(snapshot.text, cache["thread"]) }
            }
            release.countDown()
            stream.get(5, TimeUnit.SECONDS)
            activate.get(5, TimeUnit.SECONDS)
            assertEquals(Projection(active = "new", text = "delta"), snapshot)
        } finally {
            release.countDown()
            pool.shutdownNow()
        }
    }

    @Test
    fun hydrationStartedBeforeRemovalCannotResurrectTheCache() {
        val owner = AppModelProjectionOwner()
        val cache = mutableMapOf("thread" to "old")
        val ticket = owner.readTicket()
        owner.write { cache.remove("thread") }
        assertFalse(owner.commitIfCurrent(ticket) { cache["thread"] = "late hydration" })
        assertTrue(owner.read { cache.isEmpty() })
    }

    @Test
    fun fullSnapshotReadCannotOverwriteNewerActivation() {
        val owner = AppModelProjectionOwner()
        var active = "old"
        val ticket = owner.readTicket()
        owner.write { active = "new" }
        assertFalse(owner.commitIfCurrent(ticket) { active = "old" })
        assertEquals("new", active)
    }

    @Test
    fun startStopChurnWaitsForPreviousCollectorAndKeepsReferenceCounts() = runBlocking {
        val scope = CoroutineScope(SupervisorJob() + Dispatchers.Unconfined)
        val cleanup = CompletableDeferred<Unit>()
        var starts = 0
        var running = 0
        var maxRunning = 0
        val owner = AppModelSubscriptionOwner(AppModelProjectionOwner(), scope, {}) {
            starts += 1
            running += 1
            maxRunning = maxOf(maxRunning, running)
            try {
                awaitCancellation()
            } finally {
                withContext(NonCancellable) { cleanup.await() }
                running -= 1
            }
        }
        try {
            owner.start()
            owner.start()
            assertEquals(1, starts)
            owner.stop()
            assertEquals(1, running)
            owner.stop()
            owner.start()
            assertEquals(1, starts)
            owner.stop()
            owner.start()
            assertEquals(1, starts)
            cleanup.complete(Unit)
            assertEquals(2, starts)
            assertEquals(1, maxRunning)
            owner.stop()
            assertEquals(0, running)
        } finally {
            cleanup.complete(Unit)
            scope.cancel()
        }
    }
}
