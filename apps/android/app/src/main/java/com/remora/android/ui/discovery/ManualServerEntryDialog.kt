package com.remora.android.ui.discovery

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.FilterChip
import androidx.compose.material3.FilterChipDefaults
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import com.remora.android.state.SavedServer
import com.remora.android.ui.RemoraTheme
import java.net.URI

@Composable
internal fun ManualEntryDialog(
    onDismiss: () -> Unit,
    onSubmit: (ManualEntryAction) -> Unit,
) {
    var mode by remember { mutableStateOf(ManualConnectionMode.SSH) }
    var codexUrl by remember { mutableStateOf("") }
    var host by remember { mutableStateOf("") }
    var sshPort by remember { mutableStateOf("22") }
    var wakeMac by remember { mutableStateOf("") }
    var errorMessage by remember { mutableStateOf<String?>(null) }

    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text("Add Server") },
        text = {
            Column(
                verticalArrangement = Arrangement.spacedBy(12.dp),
                modifier = Modifier.verticalScroll(rememberScrollState()),
            ) {
                Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    FilterChip(
                        selected = mode == ManualConnectionMode.CODEX,
                        onClick = { mode = ManualConnectionMode.CODEX },
                        label = { Text(ManualConnectionMode.CODEX.label) },
                        colors = FilterChipDefaults.filterChipColors(
                            selectedContainerColor = RemoraTheme.accent.copy(alpha = 0.18f),
                            selectedLabelColor = RemoraTheme.textPrimary,
                        ),
                    )
                    FilterChip(
                        selected = mode == ManualConnectionMode.SSH,
                        onClick = { mode = ManualConnectionMode.SSH },
                        label = { Text(ManualConnectionMode.SSH.label) },
                        colors = FilterChipDefaults.filterChipColors(
                            selectedContainerColor = RemoraTheme.accent.copy(alpha = 0.18f),
                            selectedLabelColor = RemoraTheme.textPrimary,
                        ),
                    )
                }

                when (mode) {
                    ManualConnectionMode.CODEX -> {
                        OutlinedTextField(
                            value = codexUrl,
                            onValueChange = {
                                codexUrl = it
                                errorMessage = null
                            },
                            label = { Text("Codex URL") },
                            placeholder = { Text("ws://host:8390 or host:8390") },
                            singleLine = true,
                        )
                        Text(
                            text = "Prefer the SSH flow — it binds 127.0.0.1 on the remote and forwards the port. " +
                                "If you run manually, bind loopback and tunnel yourself: " +
                                "codex app-server --listen ws://127.0.0.1:8390",
                            color = RemoraTheme.textMuted,
                            fontSize = 11.sp,
                        )
                    }

                    ManualConnectionMode.SSH -> {
                        OutlinedTextField(
                            value = host,
                            onValueChange = {
                                host = it
                                errorMessage = null
                            },
                            label = { Text("SSH host") },
                            placeholder = { Text("hostname or IP") },
                            singleLine = true,
                        )
                        OutlinedTextField(
                            value = sshPort,
                            onValueChange = {
                                sshPort = it
                                errorMessage = null
                            },
                            label = { Text("SSH port") },
                            singleLine = true,
                        )
                        OutlinedTextField(
                            value = wakeMac,
                            onValueChange = {
                                wakeMac = it
                                errorMessage = null
                            },
                            label = { Text("Wake MAC (optional)") },
                            placeholder = { Text("aa:bb:cc:dd:ee:ff") },
                            singleLine = true,
                        )
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
                onClick = {
                    errorMessage = when (val action = buildManualEntryAction(
                        mode,
                        codexUrl,
                        host,
                        sshPort,
                        wakeMac,
                    )) {
                        is ManualEntryBuild.Action -> {
                            onSubmit(action.action)
                            null
                        }

                        is ManualEntryBuild.Error -> action.message
                    }
                },
            ) {
                Text(mode.primaryButtonTitle)
            }
        },
        dismissButton = {
            TextButton(onClick = onDismiss) {
                Text("Cancel")
            }
        },
    )
}

internal sealed interface ManualEntryAction {
    data class Connect(val server: SavedServer) : ManualEntryAction
    data class ContinueWithSsh(val server: SavedServer) : ManualEntryAction
}

private sealed interface ManualEntryBuild {
    data class Action(val action: ManualEntryAction) : ManualEntryBuild
    data class Error(val message: String) : ManualEntryBuild
}

private enum class ManualConnectionMode(
    val label: String,
    val primaryButtonTitle: String,
) {
    CODEX("Codex", "Connect"),
    SSH("SSH", "Continue to SSH Login"),
}

private fun buildManualEntryAction(
    mode: ManualConnectionMode,
    codexUrl: String,
    host: String,
    sshPort: String,
    wakeMac: String,
): ManualEntryBuild = when (mode) {
    ManualConnectionMode.CODEX -> buildManualCodexEntry(codexUrl)
    ManualConnectionMode.SSH -> buildManualSshEntry(host, sshPort, wakeMac)
}

private fun buildManualCodexEntry(rawInput: String): ManualEntryBuild {
    val raw = rawInput.trim()
    if (raw.isEmpty()) {
        return ManualEntryBuild.Error("Enter a ws:// URL or host:port.")
    }

    runCatching { URI(raw) }
        .getOrNull()
        ?.let { uri ->
            val scheme = uri.scheme?.lowercase()
            val host = uri.host?.takeIf { it.isNotBlank() }
            if ((scheme == "ws" || scheme == "wss") && host != null) {
                val port = uri.port.takeIf { it > 0 }
                return ManualEntryBuild.Action(
                    ManualEntryAction.Connect(
                        SavedServer(
                            id = "manual-url-$raw",
                            name = host,
                            hostname = host,
                            port = port ?: 0,
                            codexPorts = port?.let(::listOf) ?: emptyList(),
                            source = "manual",
                            hasCodexServer = true,
                            preferredConnectionMode = "directCodex",
                            preferredCodexPort = port,
                            websocketURL = raw,
                        ).normalizedForPersistence(),
                    ),
                )
            }
        }

    val (host, port) = parseBareHostAndPort(raw) ?: return ManualEntryBuild.Error("Enter a ws:// URL or host:port.")
    if (host.isBlank()) {
        return ManualEntryBuild.Error("Enter a hostname or IP address.")
    }

    return ManualEntryBuild.Action(
        ManualEntryAction.Connect(
            SavedServer(
                id = "manual-$host:$port",
                name = host,
                hostname = host,
                port = port,
                codexPorts = listOf(port),
                source = "manual",
                hasCodexServer = true,
                preferredConnectionMode = "directCodex",
                preferredCodexPort = port,
            ).normalizedForPersistence(),
        ),
    )
}

private fun buildManualSshEntry(
    hostInput: String,
    sshPortInput: String,
    wakeMacInput: String,
): ManualEntryBuild {
    val host = hostInput.trim()
    if (host.isEmpty()) {
        return ManualEntryBuild.Error("Enter a hostname or IP address.")
    }

    val sshPort = sshPortInput.trim().toIntOrNull()
    if (sshPort == null || sshPort !in 1..65535) {
        return ManualEntryBuild.Error("SSH port must be a valid number.")
    }

    val wakeInput = wakeMacInput.trim()
    val normalizedWakeMac = SavedServer.normalizeWakeMac(wakeInput)
    if (wakeInput.isNotEmpty() && normalizedWakeMac == null) {
        return ManualEntryBuild.Error("Wake MAC must look like aa:bb:cc:dd:ee:ff.")
    }

    return ManualEntryBuild.Action(
        ManualEntryAction.ContinueWithSsh(
            SavedServer(
                id = "manual-ssh-$host:$sshPort",
                name = host,
                hostname = host,
                port = sshPort,
                sshPort = sshPort,
                source = "manual",
                hasCodexServer = false,
                wakeMAC = normalizedWakeMac,
                preferredConnectionMode = "ssh",
            ).normalizedForPersistence(),
        ),
    )
}

private fun parseBareHostAndPort(raw: String): Pair<String, Int>? {
    if (raw.startsWith("[")) {
        val closing = raw.indexOf(']')
        if (closing > 1) {
            val host = raw.substring(1, closing)
            val portPart = raw.substring(closing + 1)
            val port = when {
                portPart.isEmpty() -> 8390
                portPart.startsWith(":") -> portPart.drop(1).toIntOrNull() ?: return null
                else -> return null
            }
            return host to port
        }
    }

    val colonCount = raw.count { it == ':' }
    if (colonCount == 1) {
        val index = raw.lastIndexOf(':')
        val host = raw.substring(0, index)
        val port = raw.substring(index + 1).toIntOrNull() ?: return null
        return host to port
    }

    return raw to 8390
}
