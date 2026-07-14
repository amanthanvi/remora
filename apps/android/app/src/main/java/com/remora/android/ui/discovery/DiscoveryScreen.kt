package com.remora.android.ui.discovery

import android.content.Context
import android.net.Uri
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.CheckCircle
import androidx.compose.material.icons.filled.QrCodeScanner
import androidx.compose.material.icons.outlined.DesktopWindows
import androidx.compose.material.icons.outlined.Terminal
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.Icon
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.rememberModalBottomSheetState
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.draw.alpha
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import com.remora.android.state.SavedServer
import com.remora.android.state.SavedServerStore
import com.remora.android.state.SavedSshCredential
import com.remora.android.state.ChatGPTOAuth
import com.remora.android.state.SshAuthMethod
import com.remora.android.state.SshCredentialStore
import com.remora.android.state.isConnected
import com.remora.android.auth.ChatGPTOAuthActivity
import com.remora.android.ui.RemoraTheme
import com.remora.android.ui.LocalAppModel
import com.remora.android.ui.connection.SSHLoginDialog
import com.remora.android.ui.common.AgentIconView
import com.remora.android.ui.common.BetaBadge
import com.remora.android.ui.common.isBeta
import com.remora.android.util.LLog
import java.io.File
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.launch
import uniffi.codex_mobile_client.AgentAvailabilityStatus
import com.remora.android.ui.common.AgentRuntimeKind
import com.remora.android.ui.common.metadata
import com.remora.android.ui.common.runtimeLabel
import com.remora.android.ui.common.runtimeSortIndex
import uniffi.codex_mobile_client.AppSshSessionResult
import uniffi.codex_mobile_client.AppServerHealth
import uniffi.codex_mobile_client.AppServerSnapshot
import uniffi.codex_mobile_client.AppDiscoveredServer
import uniffi.codex_mobile_client.RemoteAgentAvailability
import uniffi.codex_mobile_client.AppSlingshotEnvironment
import uniffi.codex_mobile_client.SshBridgeTransport

private data class SshBridgeAgentContext(
    val server: SavedServer,
    val sessionId: String,
    val host: String,
    val availability: List<RemoteAgentAvailability>,
    val credential: SavedSshCredential,
)

private const val SLINGSHOT_BASE_URL = "https://chatgpt.com/backend-api"
private const val REMOTE_BRIDGE_STATE_DIRECTORY = "remora-bridges"
private const val LEGACY_REMOTE_BRIDGE_STATE_DIRECTORY = "alleycat-bridges"

/**
 * Server discovery and connection screen.
 * Presents the supported connection paths and owns their orchestration.
 */
@Composable
fun DiscoveryScreen(
    discoveredServers: List<AppDiscoveredServer>,
    isScanning: Boolean,
    scanProgress: Float = 0f,
    scanProgressLabel: String? = null,
    onRefresh: () -> Unit,
    onDismiss: () -> Unit,
) {
    val logTag = "DiscoveryScreen"
    val appModel = LocalAppModel.current
    val snapshot by appModel.snapshot.collectAsState()
    val context = LocalContext.current
    val scope = rememberCoroutineScope()
    val sshCredentialStore = remember(context) { SshCredentialStore(context.applicationContext) }

    var showManualEntry by remember { mutableStateOf(false) }
    var showRemotePairingSheet by remember { mutableStateOf(false) }
    var showSlingshotComputers by remember { mutableStateOf(false) }
    var slingshotEnvironments by remember { mutableStateOf<List<AppSlingshotEnvironment>>(emptyList()) }
    var slingshotIsLoading by remember { mutableStateOf(false) }
    var slingshotError by remember { mutableStateOf<String?>(null) }
    var pendingManualSshServer by remember { mutableStateOf<SavedServer?>(null) }
    var sshServer by remember { mutableStateOf<SavedServer?>(null) }
    var sshAgentContext by remember { mutableStateOf<SshBridgeAgentContext?>(null) }
    var connectionChoiceServer by remember { mutableStateOf<SavedServer?>(null) }
    var pendingAutoNavigateServerId by remember { mutableStateOf<String?>(null) }
    var pendingSlingshotEnvironment by remember { mutableStateOf<AppSlingshotEnvironment?>(null) }
    var authorizedSlingshotConnect by remember { mutableStateOf<Pair<AppSlingshotEnvironment, String>?>(null) }
    var wakingServerId by remember { mutableStateOf<String?>(null) }
    var connectError by remember { mutableStateOf<String?>(null) }
    val slingshotStepUpLauncher = rememberLauncherForActivityResult(
        contract = ActivityResultContracts.StartActivityForResult(),
    ) { result ->
        val environment = pendingSlingshotEnvironment
        pendingSlingshotEnvironment = null
        if (environment == null) {
            return@rememberLauncherForActivityResult
        }
        if (result.resultCode != android.app.Activity.RESULT_OK) {
            connectError = result.data?.getStringExtra(ChatGPTOAuthActivity.EXTRA_ERROR)
                ?: "Remote-control authorization was cancelled."
            return@rememberLauncherForActivityResult
        }
        val stepUpToken = ChatGPTOAuthActivity.parseRemoteControlStepUpToken(result.data)
        if (stepUpToken == null) {
            connectError = "Remote-control authorization returned incomplete credentials."
            return@rememberLauncherForActivityResult
        }
        authorizedSlingshotConnect = environment to stepUpToken
    }

    LaunchedEffect(showManualEntry, pendingManualSshServer) {
        if (!showManualEntry && pendingManualSshServer != null) {
            sshServer = pendingManualSshServer
            pendingManualSshServer = null
        }
    }

    LaunchedEffect(snapshot, pendingAutoNavigateServerId) {
        val pendingServerId = pendingAutoNavigateServerId ?: return@LaunchedEffect
        val serverSnapshot = snapshot?.servers?.firstOrNull { it.serverId == pendingServerId } ?: return@LaunchedEffect
        if (serverSnapshot.isConnected) {
            pendingAutoNavigateServerId = null
            onDismiss()
        } else if (serverSnapshot.health == AppServerHealth.DISCONNECTED) {
            serverSnapshot.connectionProgress?.terminalMessage?.let { message ->
                pendingAutoNavigateServerId = null
                connectError = message
            }
        }
    }

    suspend fun loadSlingshotEnvironments() {
        if (slingshotIsLoading) {
            return
        }
        slingshotIsLoading = true
        slingshotError = null
        try {
            val tokens = loadSlingshotTokens(context)
            slingshotEnvironments = appModel.serverBridge
                .listSlingshotEnvironments(
                    baseUrl = SLINGSHOT_BASE_URL,
                    accessToken = tokens.accessToken,
                    accountId = tokens.accountId,
                )
                .sortedWith(
                    compareByDescending<AppSlingshotEnvironment> { it.online }
                        .thenBy { it.busy }
                        .thenBy { it.displayName.lowercase() },
                )
        } catch (e: Exception) {
            LLog.e(logTag, "slingshot environment load failed", e)
            slingshotError = e.message ?: "Unable to load connected computers."
        } finally {
            slingshotIsLoading = false
        }
    }

    suspend fun connectSlingshotEnvironmentOrThrow(environment: AppSlingshotEnvironment, stepUpToken: String) {
        if (!environment.online) {
            throw IllegalStateException("${environment.displayName} is offline.")
        }
        val server = slingshotSavedServer(environment)
        val tokens = loadSlingshotTokens(context)
        appModel.serverBridge.connectRemoteSlingshotUrlServer(
            server.id,
            server.name,
            environment.connectionUrl,
            tokens.accessToken,
            tokens.accountId,
            stepUpToken,
        )
        SavedServerStore.remember(context, server.normalizedForPersistence())
        appModel.refreshSnapshot()
    }

    fun finishSuccessfulSlingshotConnect() {
        showSlingshotComputers = false
        onDismiss()
    }

    fun startSlingshotConnect(environment: AppSlingshotEnvironment) {
        if (!environment.online) {
            connectError = "${environment.displayName} is offline."
            return
        }
        scope.launch {
            var needsAuthorization = false
            try {
                connectSlingshotEnvironmentOrThrow(environment, "")
            } catch (e: CancellationException) {
                throw e
            } catch (e: Exception) {
                if (!ChatGPTOAuth.isRemoteControlAuthorizationRequired(e)) {
                    LLog.e(
                        logTag,
                        "slingshot cached connect failed",
                        e,
                        fields = mapOf("environmentId" to environment.id),
                    )
                    connectError = e.message ?: "Unable to connect to this computer."
                    return@launch
                }
                needsAuthorization = true
            }

            if (!needsAuthorization) {
                finishSuccessfulSlingshotConnect()
                return@launch
            }

            try {
                pendingSlingshotEnvironment = environment
                slingshotStepUpLauncher.launch(
                    ChatGPTOAuthActivity.createIntent(
                        context,
                        ChatGPTOAuth.createRemoteControlEnrollmentAttempt(),
                    ),
                )
            } catch (e: Exception) {
                pendingSlingshotEnvironment = null
                connectError = e.localizedMessage ?: e.message ?: "Unable to authorize remote control."
            }
        }
    }

    suspend fun connectSlingshotEnvironment(environment: AppSlingshotEnvironment, stepUpToken: String) {
        try {
            connectSlingshotEnvironmentOrThrow(environment, stepUpToken)
        } catch (e: CancellationException) {
            throw e
        } catch (e: Exception) {
            LLog.e(
                logTag,
                "slingshot connect failed",
                e,
                fields = mapOf("environmentId" to environment.id),
            )
            connectError = e.message ?: "Unable to connect to this computer."
            return
        }
        finishSuccessfulSlingshotConnect()
    }

    LaunchedEffect(authorizedSlingshotConnect) {
        val pending = authorizedSlingshotConnect ?: return@LaunchedEffect
        authorizedSlingshotConnect = null
        connectSlingshotEnvironment(pending.first, pending.second)
    }

    suspend fun openSshSession(server: SavedServer, credential: SavedSshCredential): AppSshSessionResult =
        when (credential.method) {
            SshAuthMethod.PASSWORD -> appModel.ssh.sshOpenSession(
                host = server.hostname,
                port = server.resolvedSshPort.toUShort(),
                username = credential.username,
                password = credential.password,
                privateKeyPem = null,
                passphrase = null,
                unlockMacosKeychain = credential.unlockMacosKeychain,
                acceptUnknownHost = true,
            )

            SshAuthMethod.KEY -> appModel.ssh.sshOpenSession(
                host = server.hostname,
                port = server.resolvedSshPort.toUShort(),
                username = credential.username,
                password = null,
                privateKeyPem = credential.privateKey,
                passphrase = credential.passphrase,
                unlockMacosKeychain = false,
                acceptUnknownHost = true,
            )
        }

    suspend fun startGuidedSshConnect(server: SavedServer, credential: SavedSshCredential) {
        when (credential.method) {
            SshAuthMethod.PASSWORD -> {
                appModel.serverBridge.startRemoteOverSshConnect(
                    serverId = server.id,
                    displayName = server.name,
                    host = server.hostname,
                    port = server.resolvedSshPort.toUShort(),
                    username = credential.username,
                    password = credential.password,
                    privateKeyPem = null,
                    passphrase = null,
                    unlockMacosKeychain = credential.unlockMacosKeychain,
                    acceptUnknownHost = true,
                    workingDir = null,
                )
            }

            SshAuthMethod.KEY -> {
                appModel.serverBridge.startRemoteOverSshConnect(
                    serverId = server.id,
                    displayName = server.name,
                    host = server.hostname,
                    port = server.resolvedSshPort.toUShort(),
                    username = credential.username,
                    password = null,
                    privateKeyPem = credential.privateKey,
                    passphrase = credential.passphrase,
                    unlockMacosKeychain = false,
                    acceptUnknownHost = true,
                    workingDir = null,
                )
            }
        }
    }

    suspend fun prepareServerForSelection(entry: SavedServer): SavedServer {
        if (entry.source == "local" || entry.websocketURL != null) {
            return entry
        }

        wakingServerId = entry.id
        try {
            return when (
                val wakeResult = waitForWakeSignal(
                    host = entry.hostname,
                    preferredCodexPort = entry.directCodexPort ?: entry.availableDirectCodexPorts.firstOrNull(),
                    preferredSshPort = entry.sshPort ?: if (entry.canConnectViaSsh) entry.resolvedSshPort else null,
                    timeoutMillis = if (entry.hasCodexServer) 12_000L else 18_000L,
                    wakeMac = entry.wakeMAC,
                )
            ) {
                is WakeSignalResult.Codex -> entry.copy(
                    port = wakeResult.port,
                    codexPorts = listOf(wakeResult.port) + entry.availableDirectCodexPorts.filter { it != wakeResult.port },
                    hasCodexServer = true,
                    preferredConnectionMode = entry.preferredConnectionMode,
                    preferredCodexPort = wakeResult.port,
                ).normalizedForPersistence()

                is WakeSignalResult.Ssh -> entry.copy(
                    port = wakeResult.port,
                    sshPort = wakeResult.port,
                    hasCodexServer = false,
                    preferredConnectionMode = "ssh",
                    preferredCodexPort = null,
                ).normalizedForPersistence()

                WakeSignalResult.None -> entry
            }
        } finally {
            wakingServerId = null
        }
    }

    suspend fun connectPreparedRemoteUrl(prepared: SavedServer) {
        val websocketURL = prepared.websocketURL ?: return
        if (isSlingshotUrl(websocketURL)) {
            val tokens = loadSlingshotTokens(context)
            appModel.serverBridge.connectRemoteSlingshotUrlServer(
                prepared.id,
                prepared.name,
                websocketURL,
                tokens.accessToken,
                tokens.accountId,
                "",
            )
        } else {
            appModel.serverBridge.connectRemoteUrlServer(
                prepared.id,
                prepared.name,
                websocketURL,
            )
        }
    }

    suspend fun connectSelectedServer(entry: SavedServer) {
        if (wakingServerId != null && wakingServerId != entry.id) {
            return
        }

        try {
            val connected = connectedSnapshot(entry, snapshot?.servers ?: emptyList())
            if (connected?.isConnected == true) {
                LLog.t(logTag, "server already connected", fields = mapOf("serverId" to entry.id))
                onDismiss()
                return
            }

            val prepared = prepareServerForSelection(entry)
            when {
                prepared.source == "local" -> {
                    appModel.serverBridge.connectLocalServer(
                        prepared.id,
                        prepared.name,
                        prepared.hostname,
                        prepared.port.toUShort(),
                    )
                    appModel.restoreStoredLocalAuthState(prepared.id)
                    SavedServerStore.remember(context, prepared.normalizedForPersistence())
                    appModel.refreshSnapshot()
                    onDismiss()
                }

                prepared.websocketURL != null -> {
                    connectPreparedRemoteUrl(prepared)
                    SavedServerStore.remember(context, prepared.normalizedForPersistence())
                    appModel.refreshSnapshot()
                    onDismiss()
                }

                prepared.requiresConnectionChoice -> {
                    connectionChoiceServer = prepared
                }

                prepared.prefersSshConnection || (!prepared.hasCodexServer && prepared.canConnectViaSsh) -> {
                    sshServer = prepared.withPreferredConnection("ssh")
                }

                prepared.directCodexPort != null -> {
                    appModel.serverBridge.connectRemoteServer(
                        prepared.id,
                        prepared.name,
                        prepared.hostname,
                        prepared.directCodexPort!!.toUShort(),
                    )
                    SavedServerStore.remember(
                        context,
                        prepared.withPreferredConnection("directCodex", prepared.directCodexPort),
                    )
                    appModel.refreshSnapshot()
                    onDismiss()
                }

                else -> {
                    connectError = "Server did not respond after wake attempt. Enable Wake for network access on the Mac."
                }
            }
        } catch (e: Exception) {
            LLog.e(
                logTag,
                "server connect failed",
                e,
                fields = mapOf(
                    "serverId" to entry.id,
                    "host" to entry.hostname,
                    "preferredConnectionMode" to entry.preferredConnectionMode,
                ),
            )
            connectError = e.message ?: "Unable to connect."
        }
    }

    Column(
        modifier = Modifier
            .fillMaxWidth()
            .padding(16.dp),
    ) {
        Row(
            verticalAlignment = Alignment.CenterVertically,
            modifier = Modifier.fillMaxWidth(),
        ) {
            Text(
                text = "Add Server",
                color = RemoraTheme.textPrimary,
                fontSize = 18.sp,
                fontWeight = FontWeight.SemiBold,
                modifier = Modifier.weight(1f),
            )
        }

        Spacer(Modifier.height(8.dp))

        Text(
            text = "Pick how you want to connect.",
            color = RemoraTheme.textSecondary,
            fontSize = 12.sp,
        )

        Spacer(Modifier.height(14.dp))

        Column(verticalArrangement = Arrangement.spacedBy(12.dp)) {
            ChooserCard(
                title = "Pair with Remora",
                subtitle = "Run npx kittylitter on the host, then scan the QR code it prints.",
                badge = "RECOMMENDED",
                icon = Icons.Default.QrCodeScanner,
                supportedAgents = RemotePairingAgents,
                isRecommended = true,
                onClick = { showRemotePairingSheet = true },
            )

            ChooserCard(
                title = "Connected Computer",
                subtitle = "Connect to a computer already signed in and running Codex for this ChatGPT account.",
                badge = null,
                icon = Icons.Outlined.DesktopWindows,
                supportedAgents = CodexOnlyAgents,
                isRecommended = false,
                onClick = { showSlingshotComputers = true },
            )

            ChooserCard(
                title = "SSH or Codex URL",
                subtitle = "Connect over SSH or paste a ws:// codex URL.",
                badge = null,
                icon = Icons.Outlined.Terminal,
                supportedAgents = CodexOnlyAgents,
                isRecommended = false,
                onClick = { showManualEntry = true },
            )
        }
    }

    if (showManualEntry) {
        ManualEntryDialog(
            onDismiss = { showManualEntry = false },
            onSubmit = { action ->
                when (action) {
                    is ManualEntryAction.Connect -> {
                        showManualEntry = false
                        scope.launch { connectSelectedServer(action.server) }
                    }

                    is ManualEntryAction.ContinueWithSsh -> {
                        pendingManualSshServer = action.server
                        showManualEntry = false
                    }
                }
            },
        )
    }

    if (showSlingshotComputers) {
        ConnectedComputersDialog(
            environments = slingshotEnvironments,
            loading = slingshotIsLoading,
            error = slingshotError,
            onDismiss = { showSlingshotComputers = false },
            onRefresh = { scope.launch { loadSlingshotEnvironments() } },
            onSelect = { environment ->
                startSlingshotConnect(environment)
            },
        )
        LaunchedEffect(Unit) {
            if (slingshotEnvironments.isEmpty() && !slingshotIsLoading) {
                loadSlingshotEnvironments()
            }
        }
    }

    connectionChoiceServer?.let { server ->
        AlertDialog(
            onDismissRequest = { connectionChoiceServer = null },
            title = { Text("Connect ${server.name.ifBlank { server.hostname }}") },
            text = {
                Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
                    Text(
                        connectionChoiceMessage(server),
                        color = RemoraTheme.textSecondary,
                    )
                    server.availableDirectCodexPorts.forEach { port ->
                        TextButton(
                            onClick = {
                                connectionChoiceServer = null
                                scope.launch {
                                    try {
                                        appModel.serverBridge.connectRemoteServer(
                                            server.id,
                                            server.name,
                                            server.hostname,
                                            port.toUShort(),
                                        )
                                        SavedServerStore.remember(
                                            context,
                                            server.withPreferredConnection("directCodex", port),
                                        )
                                        appModel.refreshSnapshot()
                                        onDismiss()
                                    } catch (e: Exception) {
                                        LLog.e(
                                            logTag,
                                            "direct codex connect failed",
                                            e,
                                            fields = mapOf(
                                                "serverId" to server.id,
                                                "host" to server.hostname,
                                                "codexPort" to port,
                                                "os" to server.os,
                                            ),
                                        )
                                        connectError = e.message ?: "Unable to connect."
                                    }
                                }
                            },
                            modifier = Modifier.fillMaxWidth(),
                        ) {
                            Text("Use Codex ($port)")
                        }
                    }
                    if (server.canConnectViaSsh) {
                        TextButton(
                            onClick = {
                                sshServer = server.withPreferredConnection("ssh")
                                connectionChoiceServer = null
                            },
                            modifier = Modifier.fillMaxWidth(),
                        ) {
                            Text("Connect via SSH", color = RemoraTheme.accent)
                        }
                    }
                }
            },
            confirmButton = {
                TextButton(onClick = { connectionChoiceServer = null }) {
                    Text("Cancel")
                }
            },
            dismissButton = {},
        )
    }

    sshServer?.let { server ->
        SSHLoginDialog(
            server = server,
            initialCredential = sshCredentialStore.load(server.hostname, server.resolvedSshPort),
            onDismiss = { sshServer = null },
            onConnect = { credential, rememberCredentials ->
                try {
                    LLog.t(
                        logTag,
                        "starting SSH connect",
                        fields = mapOf(
                            "serverId" to server.id,
                            "host" to server.hostname,
                            "sshPort" to server.resolvedSshPort,
                            "authMethod" to credential.method.name,
                            "os" to server.os,
                        ),
                    )
                    if (rememberCredentials) {
                        sshCredentialStore.save(server.hostname, server.resolvedSshPort, credential)
                    } else {
                        sshCredentialStore.delete(server.hostname, server.resolvedSshPort)
                    }

                    val session = openSshSession(server, credential)
                    val availability = appModel.ssh.sshProbeRemoteAgents(session.sessionId)
                    val bridgeAgents = availableSshBridgeKinds(availability)
                    if (bridgeAgents.isNotEmpty()) {
                        sshAgentContext = SshBridgeAgentContext(
                            server = server,
                            sessionId = session.sessionId,
                            host = session.normalizedHost,
                            availability = availability,
                            credential = credential,
                        )
                        sshServer = null
                        null
                    } else {
                        appModel.ssh.sshClose(session.sessionId)
                        LLog.t(
                            logTag,
                            "no SSH bridge agents available; falling back to Codex SSH",
                            fields = mapOf(
                                "serverId" to server.id,
                                "host" to server.hostname,
                            ),
                        )
                        startGuidedSshConnect(server, credential)
                        SavedServerStore.remember(
                            context,
                            server.withPreferredConnection("ssh"),
                        )
                        appModel.refreshSnapshot()
                        pendingAutoNavigateServerId = server.id
                        LLog.t(
                            logTag,
                            "guided SSH bootstrap started",
                            fields = mapOf(
                                "serverId" to server.id,
                                "host" to server.hostname,
                                "sshPort" to server.resolvedSshPort,
                            ),
                        )
                        sshServer = null
                        null
                    }
                } catch (e: Exception) {
                    LLog.e(
                        logTag,
                        "guided SSH connect failed",
                        e,
                        fields = mapOf(
                            "serverId" to server.id,
                            "host" to server.hostname,
                            "sshPort" to server.resolvedSshPort,
                            "authMethod" to credential.method.name,
                            "os" to server.os,
                        ),
                    )
                    e.message ?: "Unable to connect over SSH."
                }
            },
        )
    }

    sshAgentContext?.let { agentContext ->
        SSHAgentPickerDialog(
            context = agentContext,
            onDismiss = {
                scope.launch {
                    runCatching { appModel.ssh.sshClose(agentContext.sessionId) }
                    sshAgentContext = null
                }
            },
            onUseCodex = {
                scope.launch {
                    runCatching { appModel.ssh.sshClose(agentContext.sessionId) }
                    startGuidedSshConnect(agentContext.server, agentContext.credential)
                    SavedServerStore.remember(
                        context,
                        agentContext.server.withPreferredConnection("ssh"),
                    )
                    appModel.refreshSnapshot()
                    pendingAutoNavigateServerId = agentContext.server.id
                    sshAgentContext = null
                }
            },
            onConnect = { selectedKinds ->
                try {
                    val result = appModel.ssh.sshConnectBridgeSession(
                        sessionId = agentContext.sessionId,
                        serverId = "ssh-bridge:${agentContext.host}",
                        displayName = agentContext.server.name,
                        host = agentContext.host,
                        stateRoot = sshBridgeStateRoot(context, agentContext.host),
                        runtimeKinds = selectedKinds,
                        transport = SshBridgeTransport.EPHEMERAL,
                    )
                    val server = agentContext.server.copy(
                        id = result.serverId,
                        hostname = agentContext.host,
                        port = 0,
                        codexPorts = emptyList(),
                        source = "ssh",
                        hasCodexServer = true,
                        preferredConnectionMode = "ssh",
                    )
                    appModel.sshSessionStore.record(result.serverId, agentContext.sessionId)
                    SavedServerStore.remember(context, server)
                    appModel.refreshSnapshot()
                    pendingAutoNavigateServerId = result.serverId
                    sshAgentContext = null
                    null
                } catch (e: Exception) {
                    LLog.e(
                        logTag,
                        "SSH bridge connect failed",
                        e,
                        fields = mapOf(
                            "serverId" to agentContext.server.id,
                            "host" to agentContext.host,
                        ),
                    )
                    e.message ?: "Unable to connect SSH bridge agents."
                }
            },
        )
    }

    connectError?.let { message ->
        AlertDialog(
            onDismissRequest = { connectError = null },
            title = { Text("Connection Failed") },
            text = { Text(message) },
            confirmButton = {
                TextButton(onClick = { connectError = null }) {
                    Text("OK")
                }
            },
        )
    }

    if (showRemotePairingSheet) {
        @OptIn(ExperimentalMaterial3Api::class)
        ModalBottomSheet(
            onDismissRequest = { showRemotePairingSheet = false },
            sheetState = rememberModalBottomSheetState(skipPartiallyExpanded = true),
            containerColor = RemoraTheme.background,
        ) {
            RemotePairingSheet(
                onDismiss = { showRemotePairingSheet = false },
                startScanningOnAppear = true,
                onConnected = { result ->
                    showRemotePairingSheet = false
                    scope.launch {
                        SavedServerStore.rememberAlleycat(
                            context = context,
                            serverId = result.serverId,
                            displayName = result.displayName,
                            nodeId = result.nodeId,
                            relay = result.params.relay,
                            agentName = result.agentName,
                            agentWire = remotePairingWireStorageValue(result.agentWire),
                        )
                        appModel.refreshSnapshot()
                        pendingAutoNavigateServerId = result.serverId
                    }
                },
            )
        }
    }
}

@Composable
private fun SSHAgentPickerDialog(
    context: SshBridgeAgentContext,
    onDismiss: () -> Unit,
    onUseCodex: () -> Unit,
    onConnect: suspend (List<AgentRuntimeKind>) -> String?,
) {
    val scope = rememberCoroutineScope()
    val availableKinds = remember(context.sessionId) {
        availableSshBridgeKinds(context.availability)
    }
    var selectedKinds by remember(context.sessionId) {
        mutableStateOf(availableKinds.filterNot { it.isBeta }.toSet())
    }
    var isConnecting by remember(context.sessionId) { mutableStateOf(false) }
    var errorMessage by remember(context.sessionId) { mutableStateOf<String?>(null) }

    AlertDialog(
        onDismissRequest = { if (!isConnecting) onDismiss() },
        title = { Text("Remote Agents") },
        text = {
            Column(
                verticalArrangement = Arrangement.spacedBy(10.dp),
                modifier = Modifier.verticalScroll(rememberScrollState()),
            ) {
                Text(
                    text = "${context.server.name.ifBlank { context.host }}\n${context.host}",
                    color = RemoraTheme.textPrimary,
                    fontSize = 13.sp,
                )
                context.availability.forEach { agent ->
                    val enabled = isSshBridgeKind(agent.kind) &&
                        agent.status == AgentAvailabilityStatus.AVAILABLE &&
                        !isConnecting
                    Row(
                        verticalAlignment = Alignment.CenterVertically,
                        modifier = Modifier
                            .fillMaxWidth()
                            .clickable(enabled = enabled) {
                                selectedKinds = if (agent.kind in selectedKinds) {
                                    selectedKinds - agent.kind
                                } else {
                                    selectedKinds + agent.kind
                                }
                            }
                            .padding(vertical = 4.dp),
                    ) {
                        AgentIconView(
                            kind = agent.kind,
                            sizeDp = 22,
                            modifier = Modifier.alpha(
                                if (agent.status == AgentAvailabilityStatus.AVAILABLE) 1f else 0.45f,
                            ),
                        )
                        Spacer(Modifier.width(10.dp))
                        Column(modifier = Modifier.weight(1f)) {
                            Row(verticalAlignment = Alignment.CenterVertically) {
                                Text(
                                    text = sshRuntimeLabel(agent.kind),
                                    color = if (agent.status == AgentAvailabilityStatus.AVAILABLE) {
                                        RemoraTheme.textPrimary
                                    } else {
                                        RemoraTheme.textSecondary
                                    },
                                    fontSize = 14.sp,
                                    fontWeight = FontWeight.Medium,
                                )
                                if (agent.kind.isBeta) {
                                    Spacer(Modifier.width(6.dp))
                                    BetaBadge()
                                }
                            }
                            Text(
                                text = sshAgentStatusLabel(agent),
                                color = RemoraTheme.textSecondary,
                                fontSize = 11.sp,
                            )
                        }
                        if (agent.kind in selectedKinds) {
                            Icon(
                                imageVector = Icons.Filled.CheckCircle,
                                contentDescription = null,
                                tint = RemoraTheme.accent,
                                modifier = Modifier.size(18.dp),
                            )
                        }
                    }
                }
                if (errorMessage != null) {
                    Text(
                        text = errorMessage!!,
                        color = RemoraTheme.danger,
                        fontSize = 12.sp,
                    )
                }
            }
        },
        confirmButton = {
            TextButton(
                enabled = !isConnecting && selectedKinds.isNotEmpty(),
                onClick = {
                    scope.launch {
                        isConnecting = true
                        errorMessage = onConnect(selectedKinds.sortedBy(::sshRuntimeSortRank))
                        isConnecting = false
                    }
                },
            ) {
                if (isConnecting) {
                    CircularProgressIndicator(
                        modifier = Modifier.size(14.dp),
                        strokeWidth = 2.dp,
                        color = RemoraTheme.accent,
                    )
                } else {
                    Text("Connect")
                }
            }
        },
        dismissButton = {
            Row {
                TextButton(onClick = onUseCodex, enabled = !isConnecting) {
                    Text("Use Codex SSH")
                }
                TextButton(onClick = onDismiss, enabled = !isConnecting) {
                    Text("Cancel")
                }
            }
        },
    )
}

private fun availableSshBridgeKinds(agents: List<RemoteAgentAvailability>): List<AgentRuntimeKind> =
    agents
        .filter { isSshBridgeKind(it.kind) && it.status == AgentAvailabilityStatus.AVAILABLE }
        .map { it.kind }
        .sortedBy(::sshRuntimeSortRank)

private fun isSshBridgeKind(kind: AgentRuntimeKind): Boolean =
    kind.metadata?.capabilities?.supportsSshBridge ?: false

private fun sshRuntimeLabel(kind: AgentRuntimeKind): String = kind.runtimeLabel

private fun sshRuntimeSortRank(kind: AgentRuntimeKind): Int = kind.runtimeSortIndex

private fun sshAgentStatusLabel(agent: RemoteAgentAvailability): String = when (agent.status) {
    AgentAvailabilityStatus.AVAILABLE -> "Available"
    AgentAvailabilityStatus.AGENT_CLI_MISSING -> "CLI missing"
    AgentAvailabilityStatus.WINDOWS_NOT_YET_SUPPORTED -> "Windows not supported"
}

private fun sshBridgeStateRoot(context: Context, host: String): String {
    val safeHost = host.replace(Regex("[^A-Za-z0-9._-]"), "_")
    val stateRoot = File(context.filesDir, REMOTE_BRIDGE_STATE_DIRECTORY)
    val legacyStateRoot = File(context.filesDir, LEGACY_REMOTE_BRIDGE_STATE_DIRECTORY)

    // Preserve existing SSH bridge state while moving new installs to the
    // neutral directory name. The root rename is atomic on app storage; the
    // per-host retry handles installs where the new root already exists.
    if (!stateRoot.exists() && legacyStateRoot.isDirectory) {
        legacyStateRoot.renameTo(stateRoot)
    }
    stateRoot.mkdirs()

    val stateDirectory = File(stateRoot, safeHost)
    val legacyStateDirectory = File(legacyStateRoot, safeHost)
    if (!stateDirectory.exists() && legacyStateDirectory.isDirectory) {
        legacyStateDirectory.renameTo(stateDirectory)
    }
    if (!stateDirectory.exists() && legacyStateDirectory.isDirectory) {
        return legacyStateDirectory.absolutePath
    }

    stateDirectory.mkdirs()
    return stateDirectory.absolutePath
}

private fun connectedSnapshot(
    entry: SavedServer,
    servers: List<AppServerSnapshot>,
): AppServerSnapshot? = servers.firstOrNull { it.serverId == entry.id }
    ?: servers.firstOrNull { it.host.lowercase().trim().trimStart('[').trimEnd(']') == entry.deduplicationKey }

private fun connectionChoiceMessage(server: SavedServer): String {
    val directPorts = server.availableDirectCodexPorts.map(Int::toString)
    if (directPorts.isEmpty()) {
        return "Use SSH to bootstrap Codex on ${server.hostname}."
    }
    if (server.canConnectViaSsh) {
        return "Codex is available on ports ${directPorts.joinToString(", ")} and SSH is also available on port ${server.resolvedSshPort}."
    }
    return "Choose a Codex app-server port on ${server.hostname}."
}

private fun isSlingshotUrl(rawUrl: String): Boolean =
    runCatching { Uri.parse(rawUrl).scheme?.equals("slingshot", ignoreCase = true) == true }
        .getOrDefault(false)

private suspend fun loadSlingshotTokens(context: Context) =
    ChatGPTOAuth.requireStoredOrRefreshedTokens(
        context,
        "Sign in with ChatGPT before connecting with Slingshot.",
    )
