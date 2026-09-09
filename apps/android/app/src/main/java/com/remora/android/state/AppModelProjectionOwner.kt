package com.remora.android.state

import java.util.concurrent.locks.ReentrantLock
import kotlin.concurrent.withLock
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.CoroutineStart
import kotlinx.coroutines.Job
import kotlinx.coroutines.launch
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock

/** Serializes native projection commits only; never hold this guard across native calls. */
internal class AppModelProjectionOwner {
    private val lock = ReentrantLock()
    private var revision = 0L

    fun <T> read(block: () -> T): T = lock.withLock(block)

    fun <T> write(block: () -> T): T = lock.withLock {
        revision += 1
        block()
    }

    fun readTicket(): Long = read { revision }

    fun commitIfCurrent(ticket: Long, block: () -> Unit): Boolean = lock.withLock {
        if (revision != ticket) return false
        revision += 1
        block()
        true
    }
}

/** Reference-counted subscription lifetime, serialized with the projection it feeds. */
internal class AppModelSubscriptionOwner(
    private val projection: AppModelProjectionOwner,
    private val scope: CoroutineScope,
    private val onStop: () -> Unit,
    private val collect: suspend () -> Unit,
) {
    private var job: Job? = null
    private var clients = 0
    private val collectionMutex = Mutex()

    fun start() {
        val next = projection.read {
            clients += 1
            if (job?.let { !it.isCompleted && !it.isCancelled } == true) return@read null
            scope.launch(start = CoroutineStart.LAZY) {
                collectionMutex.withLock { collect() }
            }.also { job = it }
        }
        next?.start()
    }

    fun stop() {
        projection.read {
            clients = (clients - 1).coerceAtLeast(0)
            if (clients != 0) return@read
            job?.cancel()
            onStop()
        }
    }
}
