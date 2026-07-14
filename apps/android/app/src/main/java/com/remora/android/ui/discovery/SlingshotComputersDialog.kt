package com.remora.android.ui.discovery

import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.DesktopWindows
import androidx.compose.material.icons.outlined.Dns
import androidx.compose.material.icons.outlined.Laptop
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.Icon
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import com.remora.android.state.SavedServer
import com.remora.android.ui.RemoraTheme
import uniffi.codex_mobile_client.AppSlingshotEnvironment

@Composable
internal fun ConnectedComputersDialog(
    environments: List<AppSlingshotEnvironment>,
    loading: Boolean,
    error: String?,
    onDismiss: () -> Unit,
    onRefresh: () -> Unit,
    onSelect: (AppSlingshotEnvironment) -> Unit,
) {
    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text("Connected Computers") },
        text = {
            Column(verticalArrangement = Arrangement.spacedBy(12.dp)) {
                Text(
                    text = "These computers come from ChatGPT using your signed-in account. Start Codex on the computer first so it appears here.",
                    color = RemoraTheme.textSecondary,
                    fontSize = 12.sp,
                )
                when {
                    loading && environments.isEmpty() -> {
                        Row(
                            verticalAlignment = Alignment.CenterVertically,
                            horizontalArrangement = Arrangement.spacedBy(10.dp),
                        ) {
                            CircularProgressIndicator(
                                modifier = Modifier.size(18.dp),
                                strokeWidth = 2.dp,
                                color = RemoraTheme.accent,
                            )
                            Text(
                                text = "Loading connected computers...",
                                color = RemoraTheme.textSecondary,
                                fontSize = 12.sp,
                            )
                        }
                    }

                    error != null -> {
                        Text(
                            text = error,
                            color = RemoraTheme.danger,
                            fontSize = 12.sp,
                        )
                    }

                    environments.isEmpty() -> {
                        Text(
                            text = "No connected computers were found for this account.",
                            color = RemoraTheme.textSecondary,
                            fontSize = 12.sp,
                        )
                    }

                    else -> {
                        if (loading) {
                            LinearProgressIndicator(
                                modifier = Modifier.fillMaxWidth(),
                                color = RemoraTheme.accent,
                                trackColor = RemoraTheme.border,
                            )
                        }
                        LazyColumn(
                            verticalArrangement = Arrangement.spacedBy(8.dp),
                            modifier = Modifier.height(340.dp),
                        ) {
                            items(environments, key = { it.id }) { environment ->
                                ConnectedComputerRow(
                                    environment = environment,
                                    onClick = { onSelect(environment) },
                                )
                            }
                        }
                    }
                }
            }
        },
        confirmButton = {
            TextButton(
                onClick = onRefresh,
                enabled = !loading,
            ) {
                Text("Refresh")
            }
        },
        dismissButton = {
            TextButton(onClick = onDismiss) {
                Text("Cancel")
            }
        },
    )
}

@Composable
private fun ConnectedComputerRow(
    environment: AppSlingshotEnvironment,
    onClick: () -> Unit,
) {
    Row(
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(12.dp),
        modifier = Modifier
            .fillMaxWidth()
            .background(RemoraTheme.surface, RoundedCornerShape(10.dp))
            .clickable(enabled = environment.online, onClick = onClick)
            .padding(horizontal = 12.dp, vertical = 10.dp),
    ) {
        Icon(
            imageVector = slingshotEnvironmentIcon(environment),
            contentDescription = null,
            tint = if (environment.online) RemoraTheme.accent else RemoraTheme.textMuted,
            modifier = Modifier.size(22.dp),
        )
        Column(
            modifier = Modifier.weight(1f),
            verticalArrangement = Arrangement.spacedBy(2.dp),
        ) {
            Text(
                text = environment.displayName,
                color = if (environment.online) RemoraTheme.textPrimary else RemoraTheme.textSecondary,
                fontSize = 14.sp,
                fontWeight = FontWeight.SemiBold,
            )
            Text(
                text = slingshotEnvironmentSubtitle(environment),
                color = RemoraTheme.textSecondary,
                fontSize = 11.sp,
            )
        }
        Text(
            text = slingshotEnvironmentStatus(environment),
            color = if (environment.online && !environment.busy) RemoraTheme.accent else RemoraTheme.textMuted,
            fontSize = 11.sp,
            fontWeight = FontWeight.SemiBold,
        )
    }
}

internal fun slingshotSavedServer(environment: AppSlingshotEnvironment): SavedServer =
    SavedServer(
        id = "slingshot-${environment.id}",
        name = environment.displayName,
        hostname = environment.id,
        port = 0,
        codexPorts = emptyList(),
        source = "manual",
        hasCodexServer = true,
        preferredConnectionMode = "directCodex",
        websocketURL = environment.connectionUrl,
        os = environment.operatingSystem,
        rememberedByUser = true,
    )

private fun slingshotEnvironmentSubtitle(environment: AppSlingshotEnvironment): String {
    val parts = buildList {
        environment.hostName?.trim()?.takeIf { it.isNotEmpty() }?.let(::add)
        listOfNotNull(
            environment.operatingSystem.trim().takeIf { it.isNotEmpty() },
            environment.architecture?.trim()?.takeIf { it.isNotEmpty() },
        ).joinToString(" ").takeIf { it.isNotEmpty() }?.let(::add)
        environment.appServerVersion?.trim()?.takeIf { it.isNotEmpty() }?.let { add("Codex $it") }
    }
    return parts.ifEmpty { listOf(environment.id) }.joinToString(" - ")
}

private fun slingshotEnvironmentStatus(environment: AppSlingshotEnvironment): String =
    when {
        !environment.online -> "offline"
        environment.busy -> "busy"
        else -> "online"
    }

private fun slingshotEnvironmentIcon(
    environment: AppSlingshotEnvironment,
): ImageVector =
    when (environment.operatingSystem.lowercase()) {
        "linux" -> Icons.Outlined.Dns
        "windows" -> Icons.Outlined.DesktopWindows
        "macos", "darwin" -> Icons.Outlined.DesktopWindows
        else -> Icons.Outlined.Laptop
    }
