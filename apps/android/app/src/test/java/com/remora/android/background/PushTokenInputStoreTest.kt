package com.remora.android.background

import com.remora.android.state.CustodyFixture
import com.remora.android.state.RemoraLinkJournalBackendWrite
import java.util.concurrent.CyclicBarrier
import java.util.concurrent.Executors
import java.util.concurrent.TimeUnit
import org.junit.Assert.*
import org.junit.Test

class PushTokenInputStoreTest {
    private val first = "sdk:registration-input-1".toByteArray()
    private val second = "sdk:registration-input-2".toByteArray()

    @Test fun inputBeforePairingSurvivesRestartAndSameCallbackKeepsGeneration() {
        val fixture = CustodyFixture()
        PushTokenInputStore(fixture.store()).registered(first)
        val restarted = PushTokenInputStore(fixture.store())
        restarted.registered(first)
        val input = checkNotNull(restarted.current())
        assertEquals(1uL, input.generation)
        assertArrayEquals(first, input.token)
        assertFalse(input.toString().contains(String(first)))
        input.close()
        assertTrue(input.token!!.all { it == 0.toByte() })
        restarted.current()!!.use { assertArrayEquals(first, it.token) }
    }

    @Test fun staleUnregistrationCannotRemoveNewInputAndTombstoneRetainsThroughGeneration() {
        val fixture = CustodyFixture()
        val store = PushTokenInputStore(fixture.store())
        store.registered(first)
        store.registered(second)
        store.unregistered(first)
        store.current()!!.use {
            assertEquals(2uL, it.generation)
            assertArrayEquals(second, it.token)
        }
        repeat(2) { store.unregistered(second) }
        PushTokenInputStore(fixture.store()).current()!!.use {
            assertEquals(2uL, it.generation)
            assertNull(it.token)
        }
        store.registered(second)
        store.current()!!.use {
            assertEquals(4uL, it.generation)
            assertArrayEquals(second, it.token)
        }
    }

    @Test fun concurrentDuplicateCallbacksDoNotInventTokenRotations() {
        val fixture = CustodyFixture()
        val count = 8
        val pool = Executors.newFixedThreadPool(count)
        val barrier = CyclicBarrier(count)
        try {
            (1..count).map { pool.submit {
                barrier.await(10, TimeUnit.SECONDS)
                PushTokenInputStore(fixture.store()).registered(first)
            } }.forEach { it.get(15, TimeUnit.SECONDS) }
            PushTokenInputStore(fixture.store()).current()!!.use { assertEquals(1uL, it.generation) }
        } finally { pool.shutdownNow() }
    }

    @Test fun failedPersistenceAndInvalidInputsCannotReplaceCurrentToken() {
        val fixture = CustodyFixture()
        val store = PushTokenInputStore(fixture.store())
        assertNull(store.current())
        store.registered(first)
        fixture.backend.failure = RemoraLinkJournalBackendWrite.FAILED
        assertThrows(IllegalStateException::class.java) { store.registered(second) }
        assertThrows(IllegalArgumentException::class.java) { store.registered(byteArrayOf(0)) }
        store.current()!!.use {
            assertEquals(1uL, it.generation)
            assertArrayEquals(first, it.token)
        }
    }
}
