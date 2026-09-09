package com.remora.android.background

import android.annotation.SuppressLint
import android.content.Context
import com.google.firebase.FirebaseApp
import com.google.firebase.messaging.FirebaseMessaging
import com.google.firebase.messaging.FirebaseMessagingService
import com.google.firebase.messaging.RemoteMessage
import com.remora.android.BuildConfig
import com.remora.android.state.AndroidRelayRuntime
import com.remora.android.util.LLog
import java.util.concurrent.TimeUnit

/**
 * Data-only FCM ingress. No notification channel, action, approval, deep link,
 * or foreground service is created here.
 */
@SuppressLint("MissingFirebaseInstanceTokenRefresh")
class RemoraFirebaseMessagingService : FirebaseMessagingService() {
    override fun onRegistered(installationId: String) {
        val bytes = installationId.toByteArray(Charsets.UTF_8)
        try {
            AndroidRelayRuntime.get(applicationContext).tokenInputs.registered(bytes)
            BackgroundAwarenessWork.enqueueRegistrationSync(applicationContext).result.get(5, TimeUnit.SECONDS)
        } catch (_: Exception) {
            LLog.w(TAG, "FCM registration persistence failed")
        } finally { bytes.fill(0) }
    }

    override fun onUnregistered(installationId: String) {
        val bytes = installationId.toByteArray(Charsets.UTF_8)
        try {
            AndroidRelayRuntime.get(applicationContext).tokenInputs.unregistered(bytes)
            BackgroundAwarenessWork.enqueueRegistrationSync(applicationContext).result.get(5, TimeUnit.SECONDS)
        } catch (_: Exception) {
            LLog.w(TAG, "FCM registration tombstone persistence failed")
        } finally { bytes.fill(0) }
    }

    override fun onMessageReceived(message: RemoteMessage) {
        // OS-rendered notification payloads bypass the content-free contract.
        // Ignore them and accept only the exact data envelope.
        if (message.notification != null) return

        val decoded = OpaqueWakeHintDecoder.decode(message.data)
        if (decoded !is OpaqueWakeDecodeResult.Accepted) return
        runCatching {
            BackgroundAwarenessWork.enqueueReconciliation(applicationContext, message.data).result.get(5, TimeUnit.SECONDS)
        }.onFailure {
            LLog.w(TAG, "Opaque wake persistence failed")
        }
    }

    override fun onDeletedMessages() {
        runCatching {
            BackgroundAwarenessWork.enqueueReconciliation(applicationContext).result.get(5, TimeUnit.SECONDS)
        }.onFailure {
            LLog.w(TAG, "FCM deletion repair scheduling failed")
        }
    }

    private companion object {
        const val TAG = "BackgroundAwareness"
    }
}

internal object FcmRegistrationLifecycle {
    fun ensureRegistered(context: Context) {
        val applicationContext = context.applicationContext
        if (!BuildConfig.BACKGROUND_AWARENESS_CONFIGURED) {
            LLog.w("BackgroundAwareness", "FCM opaque wake transport is disabled")
            return
        }
        if (FirebaseApp.getApps(applicationContext).isEmpty()) {
            LLog.w("BackgroundAwareness", "FCM configuration failed to initialize")
            return
        }
        runCatching { FirebaseMessaging.getInstance().register() }
            .onSuccess { task ->
                task.addOnFailureListener {
                    LLog.w("BackgroundAwareness", "FCM registration refresh failed")
                }
            }
            .onFailure {
                LLog.w("BackgroundAwareness", "FCM registration is not configured")
            }
    }
}
