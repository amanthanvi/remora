package com.remora.android.ui.settings

import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.imePadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.filled.Check
import androidx.compose.material.icons.filled.Pets
import androidx.compose.material.icons.filled.Refresh
import androidx.compose.material.icons.filled.Widgets
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.Switch
import androidx.compose.material3.SwitchDefaults
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import com.remora.android.state.PetOverlayController
import com.remora.android.state.connectionModeLabel
import com.remora.android.state.isConnected
import com.remora.android.ui.LocalAppModel
import com.remora.android.ui.RemoraTheme
import kotlinx.coroutines.launch
import uniffi.codex_mobile_client.AppPetSummary

// Pets Sub-Screen
// ═══════════════════════════════════════════════════════════════════════════════

@Composable
internal fun PetsScreen(onBack: () -> Unit) {
    val context = LocalContext.current
    val appModel = LocalAppModel.current
    val snapshot by appModel.snapshot.collectAsState()
    val scope = rememberCoroutineScope()
    val connectedServers = remember(snapshot) {
        snapshot?.servers.orEmpty().filter { it.isConnected }
    }
    var selectedServerId by remember(connectedServers) {
        mutableStateOf(
            PetOverlayController.selectedPet?.serverId?.takeIf { id ->
                connectedServers.any { it.serverId == id }
            }
                ?: snapshot?.activeThread?.serverId?.takeIf { id ->
                    connectedServers.any { it.serverId == id }
                }
                ?: connectedServers.firstOrNull()?.serverId
                ?: "",
        )
    }
    var pets by remember(selectedServerId) { mutableStateOf<List<AppPetSummary>>(emptyList()) }
    var loading by remember(selectedServerId) { mutableStateOf(false) }
    var error by remember(selectedServerId) { mutableStateOf<String?>(null) }
    val overlayPermissionGranted = PetOverlayController.canDrawOverlays(context)

    fun refresh() {
        if (selectedServerId.isBlank()) return
        scope.launch {
            loading = true
            error = null
            runCatching { appModel.client.listPets(selectedServerId) }
                .onSuccess { pets = it }
                .onFailure {
                    pets = emptyList()
                    error = it.message ?: "Unable to load pets."
                }
            loading = false
        }
    }

    LaunchedEffect(selectedServerId) {
        refresh()
    }

    LazyColumn(
        modifier = Modifier
            .fillMaxSize()
            .imePadding()
            .padding(16.dp),
        verticalArrangement = Arrangement.spacedBy(8.dp),
    ) {
        item {
            Row(Modifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically) {
                IconButton(onClick = onBack) {
                    Icon(Icons.AutoMirrored.Filled.ArrowBack, "Back", tint = RemoraTheme.accent)
                }
                Spacer(Modifier.weight(1f))
                Text("Pet", color = RemoraTheme.textPrimary, fontSize = 17.sp, fontWeight = FontWeight.SemiBold)
                Spacer(Modifier.weight(1f))
                IconButton(onClick = { refresh() }, enabled = selectedServerId.isNotBlank() && !loading) {
                    Icon(Icons.Default.Refresh, "Refresh", tint = RemoraTheme.accent)
                }
            }
        }

        item { SectionHeader("Wake") }
        item {
            SettingsRow(
                label = "Show Pet",
                subtitle = PetOverlayController.selectedPet?.displayName ?: "No pet selected",
                icon = { Icon(Icons.Default.Pets, null, tint = RemoraTheme.accent, modifier = Modifier.size(18.dp)) },
                trailing = {
                    Switch(
                        checked = PetOverlayController.visible,
                        onCheckedChange = { PetOverlayController.setVisible(context, it) },
                        colors = SwitchDefaults.colors(checkedTrackColor = RemoraTheme.accent),
                    )
                },
            )
        }
        item {
            SettingsRow(
                label = "Show Over Other Apps",
                subtitle = if (overlayPermissionGranted) {
                    "Overlay permission granted"
                } else {
                    "Needs Display over other apps permission"
                },
                icon = { Icon(Icons.Default.Widgets, null, tint = RemoraTheme.accent, modifier = Modifier.size(18.dp)) },
                trailing = {
                    Switch(
                        checked = PetOverlayController.overlayEnabled,
                        onCheckedChange = { enabled ->
                            PetOverlayController.setOverlayEnabled(context, enabled)
                            if (enabled && !overlayPermissionGranted) {
                                PetOverlayController.requestOverlayPermission(context)
                            }
                        },
                        colors = SwitchDefaults.colors(checkedTrackColor = RemoraTheme.accent),
                    )
                },
                onClick = if (!overlayPermissionGranted) {
                    { PetOverlayController.requestOverlayPermission(context) }
                } else {
                    null
                },
            )
        }

        item { SectionHeader("Server") }
        if (connectedServers.isEmpty()) {
            item { SettingsRow(label = "Connect to a server first") }
        } else {
            items(connectedServers, key = { it.serverId }) { server ->
                SettingsRow(
                    label = server.displayName,
                    subtitle = server.connectionModeLabel,
                    trailing = {
                        if (server.serverId == selectedServerId) {
                            Icon(Icons.Default.Check, null, tint = RemoraTheme.accentStrong, modifier = Modifier.size(18.dp))
                        }
                    },
                    onClick = { selectedServerId = server.serverId },
                )
            }
        }

        item { SectionHeader("Pets") }
        when {
            selectedServerId.isBlank() -> {
                item { SettingsRow(label = "No server selected") }
            }
            loading -> {
                item {
                    Row(
                        Modifier
                            .fillMaxWidth()
                            .background(RemoraTheme.surface.copy(alpha = 0.6f), RoundedCornerShape(10.dp))
                            .padding(12.dp),
                        verticalAlignment = Alignment.CenterVertically,
                    ) {
                        CircularProgressIndicator(modifier = Modifier.size(18.dp), color = RemoraTheme.accent, strokeWidth = 2.dp)
                        Spacer(Modifier.width(10.dp))
                        Text("Loading pets", color = RemoraTheme.textSecondary, fontSize = 13.sp)
                    }
                }
            }
            error != null -> {
                item { SettingsRow(label = "Unable to load pets", subtitle = error) }
            }
            pets.isEmpty() -> {
                item { SettingsRow(label = "No pets found", subtitle = "~/.codex/pets has no hatch-pet packages") }
            }
            else -> {
                items(pets, key = { it.id }) { pet ->
                    val selected = PetOverlayController.selectedPet?.serverId == selectedServerId &&
                        PetOverlayController.selectedPet?.id == pet.id
                    SettingsRow(
                        label = pet.displayName,
                        subtitle = pet.validationError ?: pet.description ?: pet.sourcePath,
                        trailing = {
                            if (PetOverlayController.isLoading && selected) {
                                CircularProgressIndicator(modifier = Modifier.size(18.dp), color = RemoraTheme.accent, strokeWidth = 2.dp)
                            } else if (selected) {
                                Icon(Icons.Default.Check, null, tint = RemoraTheme.accentStrong, modifier = Modifier.size(18.dp))
                            }
                        },
                        onClick = if (pet.hasValidSpritesheet) {
                            {
                                scope.launch {
                                    PetOverlayController.selectPet(context, appModel, selectedServerId, pet)
                                }
                            }
                        } else {
                            null
                        },
                    )
                }
            }
        }

        PetOverlayController.errorMessage?.let { message ->
            item { SettingsRow(label = "Pet load failed", subtitle = message) }
        }

        item { Spacer(Modifier.height(32.dp)) }
    }
}
