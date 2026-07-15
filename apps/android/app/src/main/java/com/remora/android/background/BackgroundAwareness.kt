package com.remora.android.background

import android.content.Context
import androidx.work.BackoffPolicy
import androidx.work.Constraints
import androidx.work.CoroutineWorker
import androidx.work.ExistingWorkPolicy
import androidx.work.NetworkType
import androidx.work.OneTimeWorkRequestBuilder
import androidx.work.WorkManager
import androidx.work.WorkerParameters
import com.remora.android.BuildConfig
import com.remora.android.util.LLog
import java.util.concurrent.TimeUnit
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.coroutines.withContext

internal enum class BackgroundReconciliationOutcome {
    COMPLETED,
    RETRY,
}

/**
 * An opaque wake cursor is only a high-water hint. The installed Rust-owned
 * adapter must authenticate to the relay/runtime, consume authoritative events
 * or a snapshot through [throughCursor], and perform a canonical repair when
 * [requireFullRepair] is true. Push data itself must never be applied as state.
 */
data class BackgroundStateReconciliationRequest(
    val throughCursor: Long,
    val requireFullRepair: Boolean,
)

data class BackgroundStateReconciliationReceipt(
    val appliedThroughCursor: Long,
    val fullRepairCompleted: Boolean,
)

fun interface BackgroundStateReconciler {
    suspend fun reconcile(
        request: BackgroundStateReconciliationRequest,
    ): BackgroundStateReconciliationReceipt
}

object BackgroundStateReconcilers {
    @Volatile
    private var installed: BackgroundStateReconciler? = null

    fun install(context: Context, reconciler: BackgroundStateReconciler) {
        installed = reconciler
        BackgroundAwarenessWork.enqueueReconciliation(context.applicationContext)
    }

    internal fun current(): BackgroundStateReconciler? = installed
}

private object BackgroundAwarenessProcessGate {
    val reconciliation = Mutex()
    val registration = Mutex()
}

object BackgroundAwareness {
    data class Status(
        val firebaseTransportConfigured: Boolean,
        val registrationSinkInstalled: Boolean,
        val stateReconcilerInstalled: Boolean,
    )

    fun status(): Status = Status(
        firebaseTransportConfigured = BuildConfig.BACKGROUND_AWARENESS_CONFIGURED,
        registrationSinkInstalled = PushRegistrationSinks.current() != null,
        stateReconcilerInstalled = BackgroundStateReconcilers.current() != null,
    )

    /**
     * Foreground is the correctness path even if every push was lost. This is
     * invoked after the normal lifecycle reconnect, performs an authoritative
     * list refresh, and only then advances the local hint cursor.
     */
    suspend fun onForeground(
        context: Context,
        resumeRuntime: suspend () -> Unit,
    ) = BackgroundAwarenessProcessGate.reconciliation.withLock {
        // Normal reconnect and opaque-wake repair are supervised independently:
        // neither may prevent the other from running on every foreground.
        runCatching { resumeRuntime() }.onFailure {
            LLog.w("BackgroundAwareness", "Foreground runtime resume failed")
        }
        runCatching { FcmRegistrationLifecycle.ensureRegistered(context) }.onFailure {
            LLog.w("BackgroundAwareness", "FCM registration scheduling failed")
        }
        runCatching { BackgroundAwarenessWork.enqueueRegistrationSync(context) }.onFailure {
            LLog.w("BackgroundAwareness", "Registration sync scheduling failed")
        }

        val request = runCatching {
            withContext(Dispatchers.IO) { WakeLedger(context).requestFullRepair() }
        }.getOrElse {
            LLog.w("BackgroundAwareness", "Foreground repair persistence failed")
            return@withLock
        }
        if (reconcile(context, request) == BackgroundReconciliationOutcome.RETRY) {
            runCatching { BackgroundAwarenessWork.enqueueReconciliation(context) }
                .onFailure {
                    LLog.w("BackgroundAwareness", "Foreground repair scheduling failed")
                }
        }
    }

    internal suspend fun reconcile(
        context: Context,
        request: WakeReconciliationRequest,
    ): BackgroundReconciliationOutcome {
        val reconciler = BackgroundStateReconcilers.current()
            ?: return BackgroundReconciliationOutcome.RETRY
        val receipt = runCatching {
            reconciler.reconcile(
                BackgroundStateReconciliationRequest(
                    throughCursor = request.targetCursor,
                    requireFullRepair = request.requiresFullRepair,
                )
            )
        }.getOrElse {
            return BackgroundReconciliationOutcome.RETRY
        }
        if (receipt.appliedThroughCursor < 0L) {
            return BackgroundReconciliationOutcome.RETRY
        }

        return runCatching {
            withContext(Dispatchers.IO) {
                WakeLedger(context).complete(
                    request = request,
                    appliedThroughCursor = receipt.appliedThroughCursor,
                    fullRepairCompleted = receipt.fullRepairCompleted,
                )
            }
            val cursorSatisfied =
                !request.hasCursorWork || receipt.appliedThroughCursor >= request.targetCursor
            val repairSatisfied =
                !request.requiresFullRepair || receipt.fullRepairCompleted
            if (cursorSatisfied && repairSatisfied) {
                BackgroundReconciliationOutcome.COMPLETED
            } else {
                BackgroundReconciliationOutcome.RETRY
            }
        }.getOrElse {
            BackgroundReconciliationOutcome.RETRY
        }
    }
}

internal object BackgroundAwarenessWork {
    private const val RECONCILIATION_WORK_NAME = "remora.background.reconcile"
    private const val REGISTRATION_WORK_NAME = "remora.background.registration"
    private const val WORK_TAG = "remora.background.awareness"

    private val networkConstraints = Constraints.Builder()
        .setRequiredNetworkType(NetworkType.CONNECTED)
        .build()

    fun enqueueReconciliation(context: Context) {
        val request = OneTimeWorkRequestBuilder<BackgroundReconciliationWorker>()
            .setConstraints(networkConstraints)
            .setBackoffCriteria(BackoffPolicy.EXPONENTIAL, 30L, TimeUnit.SECONDS)
            .addTag(WORK_TAG)
            .build()
        WorkManager.getInstance(context.applicationContext).enqueueUniqueWork(
            RECONCILIATION_WORK_NAME,
            ExistingWorkPolicy.REPLACE,
            request,
        )
    }

    fun enqueueRegistrationSync(context: Context) {
        val request = OneTimeWorkRequestBuilder<PushRegistrationSyncWorker>()
            .setConstraints(networkConstraints)
            .setBackoffCriteria(BackoffPolicy.EXPONENTIAL, 30L, TimeUnit.SECONDS)
            .addTag(WORK_TAG)
            .build()
        WorkManager.getInstance(context.applicationContext).enqueueUniqueWork(
            REGISTRATION_WORK_NAME,
            ExistingWorkPolicy.REPLACE,
            request,
        )
    }
}

class BackgroundReconciliationWorker(
    appContext: Context,
    workerParameters: WorkerParameters,
) : CoroutineWorker(appContext, workerParameters) {
    override suspend fun doWork(): Result {
        return BackgroundAwarenessProcessGate.reconciliation.withLock {
            val request = runCatching {
                withContext(Dispatchers.IO) { WakeLedger(applicationContext).pendingRequest() }
            }.getOrElse { return@withLock Result.failure() }
                ?: return@withLock Result.success()
            when (BackgroundAwareness.reconcile(applicationContext, request)) {
                BackgroundReconciliationOutcome.COMPLETED -> Result.success()
                BackgroundReconciliationOutcome.RETRY -> boundedRetry()
            }
        }
    }

    private fun boundedRetry(): Result =
        if (runAttemptCount + 1 >= MAX_ATTEMPTS) Result.success() else Result.retry()

    private companion object {
        const val MAX_ATTEMPTS = 5
    }
}

class PushRegistrationSyncWorker(
    appContext: Context,
    workerParameters: WorkerParameters,
) : CoroutineWorker(appContext, workerParameters) {
    override suspend fun doWork(): Result {
        return BackgroundAwarenessProcessGate.registration.withLock {
            val store = runCatching {
                withContext(Dispatchers.IO) { PushRegistrationStore(applicationContext) }
            }.getOrElse { return@withLock Result.failure() }
            val update = runCatching {
                withContext(Dispatchers.IO) { store.pendingUpdate() }
            }.getOrElse { return@withLock Result.failure() }
                ?: return@withLock Result.success()

            // Provider deployment is intentionally outside this repository.
            // Keep the update pending until an authenticated adapter is installed.
            val sink = PushRegistrationSinks.current() ?: return@withLock Result.success()
            when (val result = runCatching { sink.sync(update) }
                .getOrDefault(PushRegistrationSyncResult.Retry)) {
                is PushRegistrationSyncResult.Acknowledged -> runCatching {
                    withContext(Dispatchers.IO) {
                        store.acknowledge(update, result.receipt)
                    }
                    Result.success()
                }.getOrElse {
                    boundedRetry()
                }

                PushRegistrationSyncResult.Retry -> boundedRetry()
                PushRegistrationSyncResult.Rejected -> Result.success()
            }
        }
    }

    private fun boundedRetry(): Result =
        if (runAttemptCount + 1 >= MAX_ATTEMPTS) Result.success() else Result.retry()

    private companion object {
        const val MAX_ATTEMPTS = 5
    }
}
