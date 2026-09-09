package com.remora.android.state

import android.content.Context
import com.remora.android.BuildConfig
import com.remora.android.background.BackgroundAwarenessWork
import com.remora.android.background.FcmRegistrationLifecycle
import com.remora.android.background.PushTokenInputStore
import com.remora.android.core.bridge.UniffiInit
import com.remora.android.util.LLog
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.launch
import uniffi.codex_mobile_client.AppClient
import uniffi.codex_mobile_client.ReconnectController
import uniffi.codex_mobile_client.TerminalSshTrustStore
import uniffi.codex_mobile_client.registerSshHostTrustStore

/** Process-owned bridges and custody, available without an Activity or AppModel. */
internal class AndroidRelayRuntime private constructor(context: Context) {
    val client: AppClient
    val reconnectController: ReconnectController
    val journal = AndroidRemoraLinkJournalBackend(context)
    val transportIdentity = AndroidRemoraLinkTransportIdentityBackend(context)
    val deviceKeys = AndroidRemoraLinkDeviceKeyBackend(RemoraLinkDeviceKeyProvider(
        allowDebugEmulatorSoftwareAssurance = BuildConfig.DEBUG,
    ))
    private val relayJournal = AndroidRelayJournalBackend(context)
    private val relaySecrets = AndroidRelaySecretBackend(context)
    val tokenInputs = PushTokenInputStore(androidRelaySecretStore(
        context, reservedAnchor = false, namespace = "com.remora.android.fcm.input.v1",
    ))
    private val linkGate: RemoraLinkConfigurationGate
    private val relayGate: RemoraLinkConfigurationGate
    val available: StateFlow<Boolean> get() = linkGate.available

    init {
        LLog.bootstrap(context)
        registerSshHostTrustStore(TerminalSshTrustStore(SshTrustStore(context)))
        registerBundledCliTools()
        client = AppClient()
        client.setSavedAppsDirectory(SavedAppsDirectory.path(context))
        client.setSlingshotCredentialsDirectory(MobilePreferencesDirectory.path(context))
        reconnectController = ReconnectController().also {
            it.setCredentialProvider(KotlinSshCredentialProvider(SshCredentialStore(context)))
            it.setSlingshotCredentialProvider(KotlinSlingshotCredentialProvider(ChatGPTOAuthTokenStore(context)))
        }
        linkGate = RemoraLinkConfigurationGate(scope) {
            client.configureRemoraLink(journal, transportIdentity, deviceKeys)
        }
        relayGate = RemoraLinkConfigurationGate(scope) {
            linkGate.configureInSeparateJob().join()
            linkGate.runWhileAvailable { }
            client.configureBackgroundRelay(relayJournal, relaySecrets, allowLoopbackHttp = false)
        }
    }

    fun retryLinkConfiguration() = linkGate.configureInSeparateJob()

    suspend fun <T> withLink(operation: suspend (AppClient) -> T): T =
        linkGate.runWhileAvailable { operation(client) }

    suspend fun <T> withRelay(operation: suspend (AppClient) -> T): T {
        relayGate.configureInSeparateJob().join()
        return relayGate.runWhileAvailable { operation(client) }
    }

    companion object {
        private val scope = CoroutineScope(SupervisorJob() + Dispatchers.IO)
        @Volatile private var instance: AndroidRelayRuntime? = null

        fun get(context: Context): AndroidRelayRuntime = instance ?: synchronized(this) {
            instance ?: run {
                val app = context.applicationContext
                check(CurrentSecurityCutover.apply(app)) { "Remora 1.6 security cutover did not complete" }
                UniffiInit.ensure(app)
                AndroidRelayRuntime(app).also { instance = it }
            }
        }

        fun start(context: Context) {
            scope.launch {
                runCatching {
                    FcmRegistrationLifecycle.ensureRegistered(context)
                    get(context).withRelay { }
                    BackgroundAwarenessWork.enqueueRegistrationSync(context)
                }.onFailure { LLog.w("BackgroundAwareness", "Background relay configuration unavailable") }
            }
        }
    }
}
