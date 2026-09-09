package com.remora.android.background

import android.content.Context
import androidx.work.BackoffPolicy
import androidx.work.Constraints
import androidx.work.CoroutineWorker
import androidx.work.Data
import androidx.work.ExistingWorkPolicy
import androidx.work.NetworkType
import androidx.work.OneTimeWorkRequestBuilder
import androidx.work.Operation
import androidx.work.WorkManager
import androidx.work.WorkerParameters
import com.remora.android.state.AndroidRelayRuntime
import com.remora.android.util.LLog
import java.util.concurrent.TimeUnit
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.coroutines.withContext

object BackgroundAwareness {
    private val processGate = Mutex()

    fun onPairingChanged(context: Context) {
        runCatching { BackgroundAwarenessWork.enqueueRegistrationSync(context) }.onFailure {
            // Optional wake scheduling cannot turn committed pairing into a UI failure.
            LLog.w("BackgroundAwareness", "Post-pairing relay synchronization remains pending")
        }
    }

    suspend fun onForeground(context: Context, resumeRuntime: suspend () -> Unit) {
        try { resumeRuntime() } catch (cancelled: CancellationException) {
            throw cancelled
        } catch (_: Exception) {
            LLog.w("BackgroundAwareness", "Foreground runtime resume failed")
        }
        FcmRegistrationLifecycle.ensureRegistered(context)
        BackgroundAwarenessWork.enqueueRegistrationSync(context)
        if (!reconcile(context)) BackgroundAwarenessWork.enqueueReconciliation(context)
    }

    internal suspend fun reconcile(context: Context, wake: OpaqueWakeHint? = null): Boolean =
        processGate.withLock {
            try {
                withContext(Dispatchers.IO) {
                    val runtime = AndroidRelayRuntime.get(context)
                    runtime.withRelay { client ->
                        reconcileRelayInputs(client, runtime.tokenInputs, wake)
                    }
                }
            } catch (cancelled: CancellationException) {
                throw cancelled
            } catch (_: Exception) {
                LLog.w("BackgroundAwareness", "Background relay reconciliation pending")
                false
            }
        }
}

internal object BackgroundAwarenessWork {
    private const val WORK_NAME = "remora.background.relay"
    private val networkConstraints = Constraints.Builder()
        .setRequiredNetworkType(NetworkType.CONNECTED).build()

    fun enqueueReconciliation(context: Context, wake: Map<String, String> = emptyMap()): Operation {
        val data = Data.Builder().apply { wake.forEach { (key, value) -> putString(key, value) } }.build()
        val request = OneTimeWorkRequestBuilder<BackgroundReconciliationWorker>()
            .setInputData(data)
            .setConstraints(networkConstraints)
            .setBackoffCriteria(BackoffPolicy.EXPONENTIAL, 30L, TimeUnit.SECONDS)
            .addTag("remora.background.awareness")
            .build()
        // Do not cancel a repair/ACK in flight or drop a callback arriving just
        // before that repair completes. WorkManager persists the opaque ingress.
        return WorkManager.getInstance(context.applicationContext).enqueueUniqueWork(
            WORK_NAME, ExistingWorkPolicy.APPEND_OR_REPLACE, request,
        )
    }

    fun enqueueRegistrationSync(context: Context) = enqueueReconciliation(context)
}

class BackgroundReconciliationWorker(context: Context, parameters: WorkerParameters) :
    CoroutineWorker(context, parameters) {
    override suspend fun doWork(): Result {
        val data = inputData.keyValueMap.mapNotNull { (key, value) ->
            (value as? String)?.let { key to it }
        }.toMap()
        val wake = (OpaqueWakeHintDecoder.decode(data) as? OpaqueWakeDecodeResult.Accepted)?.hint
        return if (BackgroundAwareness.reconcile(applicationContext, wake)) Result.success()
        else if (runAttemptCount < 4) Result.retry() else {
            // Rust retains pending durable work. Finish this attempt chain so a
            // later ingress item is not discarded as a failed prerequisite.
            LLog.w("BackgroundAwareness", "Relay retry budget exhausted; durable work remains pending")
            Result.success()
        }
    }
}
