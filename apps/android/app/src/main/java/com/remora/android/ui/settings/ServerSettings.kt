package com.remora.android.ui.settings

import android.content.Context
import android.net.Uri
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.imePadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.MoreVert
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.rememberModalBottomSheetState
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import com.remora.android.auth.ChatGPTOAuthActivity
import com.remora.android.state.ChatGPTOAuth
import com.remora.android.state.SavedServer
import com.remora.android.state.SavedServerStore
import com.remora.android.state.connectionModeLabel
import com.remora.android.state.statusColor
import com.remora.android.state.statusLabel
import com.remora.android.state.toRecord
import com.remora.android.ui.LocalAppModel
import com.remora.android.ui.RemoraTheme
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import uniffi.codex_mobile_client.AppServerSnapshot

@Composable
internal fun ServerSettingsRow(
    server: AppServerSnapshot,
    onRename: (() -> Unit)?,
    onEdit: (() -> Unit)?,
    onRemove: () -> Unit,
) {
    var showMenu by remember { mutableStateOf(false) }

    Box(
        modifier = Modifier
            .fillMaxWidth()
            .background(RemoraTheme.surface.copy(alpha = 0.6f), RoundedCornerShape(10.dp)),
    ) {
        Row(
            verticalAlignment = Alignment.CenterVertically,
            modifier = Modifier
                .fillMaxWidth()
                .padding(12.dp),
        ) {
            Text(if (server.isLocal) "📱" else "🖥", fontSize = 16.sp)
            Spacer(Modifier.width(10.dp))
            Column(Modifier.weight(1f)) {
                Text(server.displayName, color = RemoraTheme.textPrimary, fontSize = 13.sp)
                Text(
                    "${server.statusLabel} · ${server.connectionModeLabel}",
                    color = server.statusColor,
                    fontSize = 11.sp,
                )
            }
            IconButton(
                onClick = { showMenu = true },
                modifier = Modifier.size(RemoraTheme.minimumTouchTarget),
            ) {
                Icon(
                    Icons.Default.MoreVert,
                    contentDescription = "Server actions",
                    tint = RemoraTheme.textSecondary,
                )
            }
        }

        DropdownMenu(expanded = showMenu, onDismissRequest = { showMenu = false }) {
            if (onEdit != null) {
                DropdownMenuItem(
                    text = { Text("Edit") },
                    onClick = {
                        showMenu = false
                        onEdit()
                    },
                )
            }
            if (onRename != null) {
                DropdownMenuItem(
                    text = { Text("Rename") },
                    onClick = {
                        showMenu = false
                        onRename()
                    },
                )
            }
            DropdownMenuItem(
                text = { Text("Remove") },
                onClick = {
                    showMenu = false
                    onRemove()
                },
            )
        }
    }
}

private enum class ServerConnectionMode(val label: String, val formHeader: String) {
    LOCAL("Local", "Local Runtime"),
    SSH("SSH", "SSH Host"),
    DIRECT_CODEX("Codex", "Codex Server"),
    WEBSOCKET("WebSocket", "Codex URL"),
    SLINGSHOT("Slingshot", "Slingshot"),
}

private fun isSettingsSlingshotUrl(rawUrl: String): Boolean =
    runCatching { Uri.parse(rawUrl).scheme?.equals("slingshot", ignoreCase = true) == true }
        .getOrDefault(false)

private suspend fun loadSettingsSlingshotTokens(context: Context) =
    ChatGPTOAuth.requireStoredOrRefreshedTokens(
        context,
        "Sign in with ChatGPT before connecting with Slingshot.",
    )

@OptIn(ExperimentalMaterial3Api::class)
@Composable
internal fun ServerEditSheet(
    server: AppServerSnapshot,
    onDismiss: () -> Unit,
    onSave: () -> Unit,
    onTriggerSshReconnect: (SavedServer) -> Unit,
) {
    val context = LocalContext.current
    val appModel = LocalAppModel.current
    val scope = rememberCoroutineScope()

    val savedServers = remember { SavedServerStore.load(context) }
    val originalSaved = remember(savedServers, server.serverId) {
        savedServers.firstOrNull { it.id == server.serverId }
    }

    val resolvedMode = remember(originalSaved, server.isLocal) {
        when {
            server.isLocal -> ServerConnectionMode.LOCAL
            originalSaved?.websocketURL?.let(::isSettingsSlingshotUrl) == true -> ServerConnectionMode.SLINGSHOT
            originalSaved?.websocketURL != null -> ServerConnectionMode.WEBSOCKET
            originalSaved?.preferredConnectionMode == "ssh" || (originalSaved?.sshPort != null && originalSaved?.hasCodexServer == false) -> ServerConnectionMode.SSH
            else -> ServerConnectionMode.DIRECT_CODEX
        }
    }
    var displayName by remember { mutableStateOf(originalSaved?.name?.trim()?.takeIf { it.isNotEmpty() } ?: server.displayName) }
    var connectionMode by remember { mutableStateOf(resolvedMode) }
    var host by remember { mutableStateOf(originalSaved?.hostname?.trim()?.takeIf { it.isNotEmpty() } ?: server.host) }
    var codexPort by remember { mutableStateOf(originalSaved?.preferredCodexPort?.toString() ?: originalSaved?.port?.takeIf { it > 0 }?.toString() ?: "8390") }
    var websocketURL by remember { mutableStateOf(originalSaved?.websocketURL ?: "") }
    var sshPort by remember { mutableStateOf(originalSaved?.sshPort?.toString() ?: "22") }
    var wakeMAC by remember { mutableStateOf(originalSaved?.wakeMAC ?: "") }
    var validationError by remember { mutableStateOf<String?>(null) }
    var isReconnecting by remember { mutableStateOf(false) }
    var pendingSlingshotReconnect by remember { mutableStateOf<SavedServer?>(null) }

    val sheetState = rememberModalBottomSheetState(skipPartiallyExpanded = true)

    fun validateAndBuild(): SavedServer? {
        val name = displayName.trim()
        if (name.isEmpty()) {
            validationError = "Server name cannot be empty."
            return null
        }

        if (originalSaved?.sshBridgeRuntimeKinds != null) {
            // SSH bridge runtime selection is established during discovery.
            return originalSaved.copy(name = name)
        }

        return when (connectionMode) {
            ServerConnectionMode.LOCAL -> {
                SavedServer(
                    id = server.serverId,
                    name = name,
                    hostname = "127.0.0.1",
                    port = 0,
                    codexPorts = emptyList(),
                    sshPort = null,
                    source = "local",
                    hasCodexServer = true,
                    wakeMAC = null,
                    preferredConnectionMode = null,
                    preferredCodexPort = null,
                    websocketURL = null,
                    rememberedByUser = true,
                )
            }
            ServerConnectionMode.SSH -> {
                val resolvedHost = host.trim()
                if (resolvedHost.isEmpty()) {
                    validationError = "Host cannot be empty."
                    return null
                }
                val resolvedSSHPort = sshPort.trim().toIntOrNull()
                if (resolvedSSHPort == null || resolvedSSHPort !in 1..65535) {
                    validationError = "SSH port must be a valid number."
                    return null
                }
                val wakeInput = wakeMAC.trim()
                val resolvedWakeMAC = SavedServer.normalizeWakeMac(wakeInput)
                if (wakeInput.isNotEmpty() && resolvedWakeMAC == null) {
                    validationError = "Wake MAC must look like aa:bb:cc:dd:ee:ff."
                    return null
                }
                SavedServer(
                    id = server.serverId,
                    name = name,
                    hostname = resolvedHost,
                    port = 0,
                    codexPorts = emptyList(),
                    sshPort = resolvedSSHPort,
                    source = "manual",
                    hasCodexServer = false,
                    wakeMAC = resolvedWakeMAC,
                    preferredConnectionMode = "ssh",
                    preferredCodexPort = null,
                    websocketURL = null,
                    rememberedByUser = true,
                )
            }
            ServerConnectionMode.DIRECT_CODEX -> {
                val resolvedHost = host.trim()
                if (resolvedHost.isEmpty()) {
                    validationError = "Host cannot be empty."
                    return null
                }
                val resolvedCodexPort = codexPort.trim().toIntOrNull()
                if (resolvedCodexPort == null || resolvedCodexPort !in 1..65535) {
                    validationError = "Codex port must be a valid number."
                    return null
                }
                SavedServer(
                    id = server.serverId,
                    name = name,
                    hostname = resolvedHost,
                    port = resolvedCodexPort,
                    codexPorts = listOf(resolvedCodexPort),
                    sshPort = null,
                    source = "manual",
                    hasCodexServer = true,
                    wakeMAC = null,
                    preferredConnectionMode = "directCodex",
                    preferredCodexPort = resolvedCodexPort,
                    websocketURL = null,
                    rememberedByUser = true,
                )
            }
            ServerConnectionMode.WEBSOCKET -> {
                val rawURL = websocketURL.trim()
                if (!rawURL.startsWith("ws://", ignoreCase = true) && !rawURL.startsWith("wss://", ignoreCase = true)) {
                    validationError = "Enter a valid ws:// or wss:// URL."
                    return null
                }
                val uri = runCatching { java.net.URI(rawURL) }.getOrNull()
                if (uri == null || uri.host.isNullOrEmpty()) {
                    validationError = "Enter a valid ws:// or wss:// URL."
                    return null
                }
                val resolvedPort = if (uri.port != -1) uri.port else null
                SavedServer(
                    id = server.serverId,
                    name = name,
                    hostname = uri.host,
                    port = resolvedPort ?: 0,
                    codexPorts = if (resolvedPort != null) listOf(resolvedPort) else emptyList(),
                    sshPort = null,
                    source = "manual",
                    hasCodexServer = true,
                    wakeMAC = null,
                    preferredConnectionMode = "directCodex",
                    preferredCodexPort = resolvedPort,
                    websocketURL = rawURL,
                    rememberedByUser = true,
                )
            }
            ServerConnectionMode.SLINGSHOT -> {
                val saved = originalSaved ?: run {
                    validationError = "Remove and add this connected computer again."
                    return null
                }
                saved.copy(
                    name = name,
                    rememberedByUser = true,
                )
            }
        }
    }

    suspend fun persist(saved: SavedServer): Boolean {
        return try {
            val updated = withContext(Dispatchers.IO) {
                SavedServerStore.replace(context, saved)
                SavedServerStore.load(context)
            }
            appModel.reconnectController.syncSavedServers(
                updated.filter { it.rememberedByUser }.map { it.toRecord() }
            )
            appModel.store.renameServer(saved.id, saved.name)
            true
        } catch (cancellation: CancellationException) {
            throw cancellation
        } catch (error: Exception) {
            validationError = error.localizedMessage ?: error.message ?: "Unable to update server."
            false
        }
    }

    suspend fun reconnect(serverId: String) {
        val servers = withContext(Dispatchers.IO) {
            SavedServerStore.load(context).map { it.toRecord() }
        }
        appModel.reconnectController.syncSavedServers(servers)
        val result = appModel.reconnectController.reconnectServer(serverId)
        if (result.needsLocalAuthRestore) {
            appModel.restoreStoredLocalAuthState(result.serverId)
            runCatching { appModel.refreshSessions(listOf(result.serverId)) }
        }
        appModel.refreshSnapshot()
    }

    suspend fun connectSlingshotSaved(saved: SavedServer, stepUpToken: String) {
        val websocketURL = saved.websocketURL?.takeIf(::isSettingsSlingshotUrl)
            ?: throw IllegalStateException("Saved server is not a Slingshot connection.")

        val tokens = loadSettingsSlingshotTokens(context)
        appModel.serverBridge.connectRemoteSlingshotUrlServer(
            saved.id,
            saved.name,
            websocketURL,
            tokens.accessToken,
            tokens.accountId,
            stepUpToken,
        )
        appModel.refreshSnapshot()
    }

    suspend fun reconnectSaved(saved: SavedServer) {
        if (saved.websocketURL?.let(::isSettingsSlingshotUrl) != true) {
            reconnect(saved.id)
            return
        }

        connectSlingshotSaved(saved, "")
    }

    val slingshotStepUpLauncher = rememberLauncherForActivityResult(
        contract = ActivityResultContracts.StartActivityForResult(),
    ) { result ->
        val saved = pendingSlingshotReconnect
        pendingSlingshotReconnect = null
        if (saved == null) {
            return@rememberLauncherForActivityResult
        }
        if (result.resultCode != android.app.Activity.RESULT_OK) {
            validationError = result.data?.getStringExtra(ChatGPTOAuthActivity.EXTRA_ERROR)
                ?: "Remote-control authorization was cancelled."
            isReconnecting = false
            return@rememberLauncherForActivityResult
        }
        val stepUpToken = ChatGPTOAuthActivity.parseRemoteControlStepUpToken(result.data)
        if (stepUpToken == null) {
            validationError = "Remote-control authorization returned incomplete credentials."
            isReconnecting = false
            return@rememberLauncherForActivityResult
        }

        scope.launch {
            isReconnecting = true
            try {
                connectSlingshotSaved(saved, stepUpToken)
                onSave()
            } catch (e: Exception) {
                validationError = e.message
            } finally {
                isReconnecting = false
            }
        }
    }

    fun launchSlingshotStepUp(saved: SavedServer) {
        try {
            pendingSlingshotReconnect = saved
            slingshotStepUpLauncher.launch(
                ChatGPTOAuthActivity.createIntent(
                    context,
                    ChatGPTOAuth.createRemoteControlEnrollmentAttempt(),
                ),
            )
        } catch (e: Exception) {
            pendingSlingshotReconnect = null
            validationError = e.localizedMessage ?: e.message ?: "Unable to authorize remote control."
        }
    }

    ModalBottomSheet(
        onDismissRequest = onDismiss,
        sheetState = sheetState,
        containerColor = RemoraTheme.background,
    ) {
        Column(
            modifier = Modifier
                .fillMaxWidth()
                .imePadding()
                .padding(16.dp),
            verticalArrangement = Arrangement.spacedBy(12.dp),
        ) {
            // Header
            Row(Modifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically) {
                Spacer(Modifier.weight(1f))
                Text("Edit Server", color = RemoraTheme.textPrimary, fontSize = 17.sp, fontWeight = FontWeight.SemiBold)
                Spacer(Modifier.weight(1f))
                TextButton(onClick = onDismiss) { Text("Done", color = RemoraTheme.accent) }
            }

            LazyColumn(verticalArrangement = Arrangement.spacedBy(12.dp)) {
                item {
                    SectionHeader("Name")
                    OutlinedTextField(
                        value = displayName,
                        onValueChange = { displayName = it },
                        label = { Text("Server name") },
                        singleLine = true,
                        modifier = Modifier.fillMaxWidth(),
                        textStyle = TextStyle(color = RemoraTheme.textPrimary, fontSize = 14.sp),
                    )
                }

                item {
                    SectionHeader(connectionMode.formHeader)

                    if (originalSaved?.sshBridgeRuntimeKinds != null) {
                        Text(
                            "This SSH bridge uses saved runtime selection. Edit its display name here, or remove and add it again to change the selection.",
                            color = RemoraTheme.textSecondary,
                            fontSize = 12.sp,
                        )
                    } else if (server.isLocal) {
                        Text(
                            "This device's local runtime is managed automatically.",
                            color = RemoraTheme.textSecondary,
                            fontSize = 12.sp,
                        )
                    } else if (connectionMode == ServerConnectionMode.SLINGSHOT) {
                        Text(
                            "This connected computer comes from ChatGPT using your signed-in account. Edit its display name here, or remove and add it again to change the computer.",
                            color = RemoraTheme.textSecondary,
                            fontSize = 12.sp,
                        )
                    } else {
                        // Mode selector
                        Row(
                            modifier = Modifier
                                .fillMaxWidth()
                                .background(RemoraTheme.surface.copy(alpha = 0.6f), RoundedCornerShape(10.dp))
                                .padding(4.dp),
                            horizontalArrangement = Arrangement.spacedBy(4.dp),
                        ) {
                            val modes = listOf(
                                ServerConnectionMode.SSH,
                                ServerConnectionMode.DIRECT_CODEX,
                                ServerConnectionMode.WEBSOCKET,
                            )
                            modes.forEach { mode ->
                                val selected = mode == connectionMode
                                Box(
                                    modifier = Modifier
                                        .weight(1f)
                                        .clip(RoundedCornerShape(8.dp))
                                        .background(if (selected) RemoraTheme.accent else Color.Transparent)
                                        .clickable { connectionMode = mode }
                                        .padding(vertical = 9.dp),
                                    contentAlignment = Alignment.Center,
                                ) {
                                    Text(
                                        mode.label,
                                        color = if (selected) RemoraTheme.onAccentStrong else RemoraTheme.textSecondary,
                                        fontSize = 12.sp,
                                        fontWeight = if (selected) FontWeight.SemiBold else FontWeight.Medium,
                                    )
                                }
                            }
                        }

                        Spacer(Modifier.height(8.dp))

                        when (connectionMode) {
                            ServerConnectionMode.SSH -> {
                                OutlinedTextField(
                                    value = host,
                                    onValueChange = { host = it },
                                    label = { Text("hostname or IP") },
                                    singleLine = true,
                                    modifier = Modifier.fillMaxWidth(),
                                    textStyle = TextStyle(color = RemoraTheme.textPrimary, fontSize = 14.sp),
                                )
                                OutlinedTextField(
                                    value = sshPort,
                                    onValueChange = { sshPort = it },
                                    label = { Text("SSH port") },
                                    singleLine = true,
                                    modifier = Modifier.fillMaxWidth(),
                                    textStyle = TextStyle(color = RemoraTheme.textPrimary, fontSize = 14.sp),
                                )
                                OutlinedTextField(
                                    value = wakeMAC,
                                    onValueChange = { wakeMAC = it },
                                    label = { Text("wake MAC (optional)") },
                                    singleLine = true,
                                    modifier = Modifier.fillMaxWidth(),
                                    textStyle = TextStyle(color = RemoraTheme.textPrimary, fontSize = 14.sp),
                                )
                            }
                            ServerConnectionMode.DIRECT_CODEX -> {
                                OutlinedTextField(
                                    value = host,
                                    onValueChange = { host = it },
                                    label = { Text("hostname or IP") },
                                    singleLine = true,
                                    modifier = Modifier.fillMaxWidth(),
                                    textStyle = TextStyle(color = RemoraTheme.textPrimary, fontSize = 14.sp),
                                )
                                OutlinedTextField(
                                    value = codexPort,
                                    onValueChange = { codexPort = it },
                                    label = { Text("Codex port") },
                                    singleLine = true,
                                    modifier = Modifier.fillMaxWidth(),
                                    textStyle = TextStyle(color = RemoraTheme.textPrimary, fontSize = 14.sp),
                                )
                            }
                            ServerConnectionMode.WEBSOCKET -> {
                                OutlinedTextField(
                                    value = websocketURL,
                                    onValueChange = { websocketURL = it },
                                    label = { Text("ws://host:port or wss://...") },
                                    singleLine = true,
                                    modifier = Modifier.fillMaxWidth(),
                                    textStyle = TextStyle(color = RemoraTheme.textPrimary, fontSize = 14.sp),
                                )
                            }
                            else -> Unit
                        }

                        if (connectionMode == ServerConnectionMode.WEBSOCKET) {
                            Text(
                                "Prefer SSH when possible. If you run codex manually, bind loopback and tunnel it yourself; do not expose it directly to the internet unless you know what you are doing.",
                                color = RemoraTheme.textMuted,
                                fontSize = 11.sp,
                                modifier = Modifier.padding(top = 4.dp),
                            )
                        }
                    }
                }

                item {
                    if (isReconnecting) {
                        Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.Center) {
                            CircularProgressIndicator(color = RemoraTheme.accent, strokeWidth = 2.dp)
                        }
                    } else {
                        Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
                            Button(
                                onClick = {
                                    validationError = null
                                    val saved = validateAndBuild()
                                    if (saved != null) {
                                        scope.launch {
                                            if (persist(saved)) {
                                                onSave()
                                            }
                                        }
                                    }
                                },
                                colors = ButtonDefaults.buttonColors(containerColor = RemoraTheme.accent),
                                modifier = Modifier.fillMaxWidth(),
                            ) {
                                Text("Save", color = RemoraTheme.onAccentStrong)
                            }
                            if (server.isLocal || originalSaved?.sshBridgeRuntimeKinds == null) {
                                Button(
                                    onClick = {
                                        validationError = null
                                        val saved = validateAndBuild()
                                        if (saved != null) {
                                            scope.launch {
                                                if (!persist(saved)) {
                                                    return@launch
                                                }
                                                // SSH mode requires interactive credentials, mirroring iOS:
                                                // hand off to the parent which will open SSHLoginDialog.
                                                if (connectionMode == ServerConnectionMode.SSH && !server.isLocal) {
                                                    onTriggerSshReconnect(saved)
                                                    return@launch
                                                }
                                                isReconnecting = true
                                                try {
                                                    reconnectSaved(saved)
                                                    onSave()
                                                } catch (cancellation: CancellationException) {
                                                    throw cancellation
                                                } catch (e: Exception) {
                                                    isReconnecting = false
                                                    if (
                                                        connectionMode == ServerConnectionMode.SLINGSHOT &&
                                                        ChatGPTOAuth.isRemoteControlAuthorizationRequired(e)
                                                    ) {
                                                        launchSlingshotStepUp(saved)
                                                    } else {
                                                        validationError = e.message
                                                    }
                                                } finally {
                                                    if (pendingSlingshotReconnect == null) {
                                                        isReconnecting = false
                                                    }
                                                }
                                            }
                                        }
                                    },
                                    colors = ButtonDefaults.buttonColors(containerColor = RemoraTheme.accentStrong),
                                    modifier = Modifier.fillMaxWidth(),
                                ) {
                                    Text(
                                        if (server.isLocal) "Save & Restart" else "Save & Reconnect",
                                        color = RemoraTheme.background,
                                    )
                                }
                            }
                        }
                    }
                }

                item { Spacer(Modifier.height(32.dp)) }
            }
        }
    }

    validationError?.let { error ->
        AlertDialog(
            onDismissRequest = { validationError = null },
            title = { Text("Invalid Server") },
            text = { Text(error) },
            confirmButton = {
                TextButton(onClick = { validationError = null }) {
                    Text("OK")
                }
            },
        )
    }
}
