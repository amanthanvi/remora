package com.remora.android.ui.settings

import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.imePadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Pets
import androidx.compose.material.icons.filled.ChevronRight
import androidx.compose.material.icons.filled.Palette
import androidx.compose.material.icons.filled.Science
import androidx.compose.material.icons.filled.Widgets
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Switch
import androidx.compose.material3.SwitchDefaults
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import com.remora.android.state.DebugSettings
import com.remora.android.state.PetOverlayController
import com.remora.android.state.SavedServer
import com.remora.android.state.SavedServerStore
import com.remora.android.state.SshAuthMethod
import com.remora.android.state.SshCredentialStore
import com.remora.android.ui.BerkeleyMono
import com.remora.android.ui.ConversationPrefs
import com.remora.android.ui.LocalAppModel
import com.remora.android.ui.RemoraTheme
import com.remora.android.ui.RemoraThemeManager
import com.remora.android.ui.connection.SSHLoginDialog
import com.remora.android.util.LLog
import kotlinx.coroutines.launch
import uniffi.codex_mobile_client.Account
import uniffi.codex_mobile_client.AppServerSnapshot

/**
 * Settings — hierarchical navigation matching iOS:
 * Top level: Appearance → | Font | Conversation | Experimental → | Account | Servers
 * Appearance pushes to sub-screen with theme pickers.
 * Experimental pushes to sub-screen with feature toggles.
 */

// ═══════════════════════════════════════════════════════════════════════════════
// Top-level Settings
// ═══════════════════════════════════════════════════════════════════════════════

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun SettingsSheet(
    onDismiss: () -> Unit,
    onOpenAccount: (serverId: String) -> Unit,
    initialSubScreen: SettingsStartDestination = SettingsStartDestination.TopLevel,
    onOpenApps: (() -> Unit)? = null,
) {
    // Sub-screen navigation
    var subScreen by remember(initialSubScreen) {
        mutableStateOf(
            when (initialSubScreen) {
                SettingsStartDestination.TopLevel -> null
                SettingsStartDestination.Pets -> SettingsSubScreen.Pets
            },
        )
    }

    when (subScreen) {
        SettingsSubScreen.Appearance -> AppearanceScreen(onBack = { subScreen = null })
        SettingsSubScreen.Experimental -> ExperimentalScreen(onBack = { subScreen = null })
        SettingsSubScreen.Pets -> PetsScreen(onBack = { subScreen = null })
        SettingsSubScreen.TipJar -> TipJarScreen(onBack = { subScreen = null })
        SettingsSubScreen.Debug -> DebugScreen(onBack = { subScreen = null })
        null -> SettingsTopLevel(
            onDismiss = onDismiss,
            onOpenAppearance = { subScreen = SettingsSubScreen.Appearance },
            onOpenExperimental = { subScreen = SettingsSubScreen.Experimental },
            onOpenPets = { subScreen = SettingsSubScreen.Pets },
            onOpenTipJar = { subScreen = SettingsSubScreen.TipJar },
            onOpenDebug = { subScreen = SettingsSubScreen.Debug },
            onOpenAccount = onOpenAccount,
            onOpenApps = onOpenApps,
        )
    }
}

enum class SettingsStartDestination { TopLevel, Pets }

private enum class SettingsSubScreen { Appearance, Experimental, Pets, TipJar, Debug }

@Composable
private fun SettingsTopLevel(
    onDismiss: () -> Unit,
    onOpenAppearance: () -> Unit,
    onOpenExperimental: () -> Unit,
    onOpenPets: () -> Unit,
    onOpenTipJar: () -> Unit,
    onOpenDebug: () -> Unit,
    onOpenAccount: (serverId: String) -> Unit,
    onOpenApps: (() -> Unit)?,
) {
    val appModel = LocalAppModel.current
    val context = LocalContext.current
    val snapshot by appModel.snapshot.collectAsState()
    val scope = rememberCoroutineScope()
    val collapseTurns = ConversationPrefs.areTurnsCollapsed
    var renameTarget by remember { mutableStateOf<AppServerSnapshot?>(null) }
    var renameText by remember { mutableStateOf("") }

    val currentServer = remember(snapshot) {
        val activeServerId = snapshot?.activeThread?.serverId
        snapshot?.servers?.firstOrNull { it.serverId == activeServerId }
            ?: snapshot?.servers?.firstOrNull { it.isLocal }
            ?: snapshot?.servers?.firstOrNull()
    }

    var editTarget by remember { mutableStateOf<AppServerSnapshot?>(null) }
    var sshReconnectTarget by remember { mutableStateOf<SavedServer?>(null) }

    LazyColumn(
        modifier = Modifier
            .fillMaxWidth()
            .imePadding()
            .padding(16.dp),
        verticalArrangement = Arrangement.spacedBy(4.dp),
    ) {
        // Title
        item {
            Box(Modifier.fillMaxWidth(), contentAlignment = Alignment.Center) {
                Text("Settings", color = RemoraTheme.textPrimary, fontSize = 17.sp, fontWeight = FontWeight.SemiBold)
                TextButton(onClick = onDismiss, modifier = Modifier.align(Alignment.CenterEnd)) {
                    Text("Done", color = RemoraTheme.accent)
                }
            }
            Spacer(Modifier.height(8.dp))
        }

        // ── Support ──
        item { SectionHeader("Support") }
        item {
            NavRow(icon = Icons.Default.Pets, label = "Tip the Remora", onClick = onOpenTipJar)
        }

        // ── Theme ──
        item { SectionHeader("Theme") }
        item {
            NavRow(icon = Icons.Default.Palette, label = "Appearance", onClick = onOpenAppearance)
        }

        // ── Font ──
        item { SectionHeader("Font") }
        item {
            Column(
                Modifier.fillMaxWidth().background(RemoraTheme.surface.copy(alpha = 0.6f), RoundedCornerShape(10.dp)),
            ) {
                FontRow("Berkeley Mono", BerkeleyMono, RemoraThemeManager.monoFontEnabled) { RemoraThemeManager.applyFont(true) }
                HorizontalDivider(color = RemoraTheme.divider)
                FontRow("System Default", FontFamily.Default, !RemoraThemeManager.monoFontEnabled) { RemoraThemeManager.applyFont(false) }
            }
        }

        // ── Conversation ──
        item { SectionHeader("Conversation") }
        item {
            SettingsRow(
                icon = { Text("⊟", color = RemoraTheme.accent, fontSize = 16.sp) },
                label = "Collapse Turns", subtitle = "Collapse previous turns into cards",
                trailing = {
                    Switch(
                        checked = collapseTurns,
                        onCheckedChange = { ConversationPrefs.setCollapseTurns(context, it) },
                        colors = SwitchDefaults.colors(checkedTrackColor = RemoraTheme.accent),
                    )
                },
            )
        }

        // ── Pets ──
        item { SectionHeader("Pet") }
        item {
            SettingsRow(
                icon = { Icon(Icons.Default.Pets, null, tint = RemoraTheme.accent, modifier = Modifier.size(18.dp)) },
                label = "Wake Pet",
                subtitle = PetOverlayController.selectedPet?.displayName ?: "Choose a Codex pet",
                trailing = {
                    Switch(
                        checked = PetOverlayController.visible,
                        onCheckedChange = { PetOverlayController.setVisible(context, it) },
                        colors = SwitchDefaults.colors(checkedTrackColor = RemoraTheme.accent),
                    )
                },
                onClick = onOpenPets,
            )
        }

        // ── Apps ──
        if (onOpenApps != null) {
            item { SectionHeader("Apps") }
            item {
                NavRow(
                    icon = Icons.Default.Widgets,
                    label = "Saved Apps",
                    onClick = {
                        onDismiss()
                        onOpenApps()
                    },
                )
            }
        }

        // ── Experimental ──
        item { SectionHeader("Experimental") }
        item {
            NavRow(icon = Icons.Default.Science, label = "Experimental Features", onClick = onOpenExperimental)
        }

        // ── Debug ──
        if (DebugSettings.enabled) {
            item { SectionHeader("Debug") }
            item {
                NavRow(icon = Icons.Default.Science, label = "Debug Settings", onClick = onOpenDebug)
            }
        }

        // ── Account ──
        item { SectionHeader("Account") }
        item {
            if (currentServer != null) {
                val accountStatus = when (val account = currentServer!!.account) {
                    is Account.Chatgpt -> account.email.ifEmpty { "ChatGPT account" }
                    is Account.ApiKey -> "OpenAI API key"
                    null -> "Not logged in"
                }
                SettingsRow(
                    icon = { Text("@", color = RemoraTheme.accent, fontSize = 16.sp, fontWeight = FontWeight.SemiBold) },
                    label = currentServer!!.displayName,
                    subtitle = accountStatus,
                    trailing = {
                        Icon(
                            Icons.Default.ChevronRight,
                            null,
                            tint = RemoraTheme.textMuted,
                            modifier = Modifier.size(16.dp),
                        )
                    },
                    onClick = { onOpenAccount(currentServer!!.serverId) },
                )
            } else {
                SettingsRow(label = "Connect to a server first")
            }
        }

        // ── Servers ──
        item { SectionHeader("Servers") }
        val servers = snapshot?.servers ?: emptyList()
        if (servers.isEmpty()) {
            item { SettingsRow(label = "No servers connected") }
        } else {
            items(servers, key = { it.serverId }) { server ->
                ServerSettingsRow(
                    server = server,
                    onRename = {
                        renameText = server.displayName
                        renameTarget = server
                    },
                    onEdit = {
                        editTarget = server
                    },
                    onRemove = {
                        scope.launch {
                            SavedServerStore.remove(context, server.serverId)
                            appModel.sshSessionStore.close(server.serverId)
                            appModel.serverBridge.disconnectServer(server.serverId)
                            appModel.refreshSnapshot()
                        }
                    },
                )
            }
        }

        item { Spacer(Modifier.height(32.dp)) }
    }

    renameTarget?.let { server ->
        AlertDialog(
            onDismissRequest = { renameTarget = null },
            title = { Text("Rename Server") },
            text = {
                OutlinedTextField(
                    value = renameText,
                    onValueChange = { renameText = it },
                    label = { Text("Name") },
                    singleLine = true,
                )
            },
            confirmButton = {
                TextButton(onClick = {
                    val trimmed = renameText.trim()
                    if (trimmed.isEmpty()) return@TextButton
                    scope.launch {
                        SavedServerStore.rename(context, server.serverId, trimmed)
                        appModel.refreshSnapshot()
                    }
                    renameTarget = null
                }) {
                    Text("Save")
                }
            },
            dismissButton = {
                TextButton(onClick = { renameTarget = null }) {
                    Text("Cancel")
                }
            },
        )
    }

    editTarget?.let { server ->
        ServerEditSheet(
            server = server,
            onDismiss = { editTarget = null },
            onSave = { editTarget = null },
            onTriggerSshReconnect = { saved ->
                editTarget = null
                sshReconnectTarget = saved
            },
        )
    }

    sshReconnectTarget?.let { saved ->
        val sshCredentialStore = remember(context) { SshCredentialStore(context.applicationContext) }
        val sshPort = saved.resolvedSshPort
        SSHLoginDialog(
            server = saved,
            initialCredential = sshCredentialStore.load(saved.hostname, sshPort),
            onDismiss = { sshReconnectTarget = null },
            onConnect = { credential, rememberCredentials ->
                try {
                    if (rememberCredentials) {
                        sshCredentialStore.save(saved.hostname, sshPort, credential)
                    } else {
                        sshCredentialStore.delete(saved.hostname, sshPort)
                    }

                    appModel.serverBridge.disconnectServer(saved.id)

                    when (credential.method) {
                        SshAuthMethod.PASSWORD -> appModel.serverBridge.startRemoteOverSshConnect(
                            serverId = saved.id,
                            displayName = saved.name,
                            host = saved.hostname,
                            port = sshPort.toUShort(),
                            username = credential.username,
                            password = credential.password,
                            privateKeyPem = null,
                            passphrase = null,
                            unlockMacosKeychain = credential.unlockMacosKeychain,
                            acceptUnknownHost = true,
                            workingDir = null,
                        )
                        SshAuthMethod.KEY -> appModel.serverBridge.startRemoteOverSshConnect(
                            serverId = saved.id,
                            displayName = saved.name,
                            host = saved.hostname,
                            port = sshPort.toUShort(),
                            username = credential.username,
                            password = null,
                            privateKeyPem = credential.privateKey,
                            passphrase = credential.passphrase,
                            unlockMacosKeychain = false,
                            acceptUnknownHost = true,
                            workingDir = null,
                        )
                    }
                    appModel.refreshSnapshot()
                    sshReconnectTarget = null
                    null
                } catch (e: Exception) {
                    LLog.e("SettingsSheet", "SSH reconnect failed: ${e.message}", e)
                    e.message ?: "SSH reconnect failed"
                }
            },
        )
    }

}
