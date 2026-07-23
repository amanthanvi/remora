package com.remora.android.ui.settings

import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.OutlinedButton
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
import androidx.compose.ui.Modifier
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import com.remora.android.ui.LocalAppModel
import com.remora.android.ui.RemoraTheme
import com.remora.android.ui.discovery.RemotePairingSheet
import com.remora.android.ui.discovery.remoraLinkErrorMessage
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch
import uniffi.codex_mobile_client.AppRemoraLinkForgetResult
import uniffi.codex_mobile_client.AppRemoraLinkHostState
import uniffi.codex_mobile_client.AppRemoraLinkHostSummary
import uniffi.codex_mobile_client.AppRemoraLinkPendingApproval
import uniffi.codex_mobile_client.AppRemoraLinkRestartOutcome
import uniffi.codex_mobile_client.AppRemoraLinkRevocationOutcome
import uniffi.codex_mobile_client.AppRemoraLinkScope

internal typealias RemoraLinkResumedPairingContent = @Composable (
    String,
    AppRemoraLinkPendingApproval,
    () -> Unit,
    (String) -> Unit,
) -> Unit

internal data class RemoraLinkHostsUiState(
    val hosts: List<AppRemoraLinkHostSummary> = emptyList(),
    val loading: Boolean = true,
    val busyHostId: String? = null,
    val message: String? = null,
    val cleanupRequiredHostName: String? = null,
)

internal interface RemoraLinkHostsApi {
    suspend fun hosts(): List<AppRemoraLinkHostSummary>
    suspend fun restart(hostId: String, runtimeId: String): AppRemoraLinkRestartOutcome
    suspend fun acknowledgeUnknownRestart(hostId: String): ULong
    suspend fun revoke(hostId: String): AppRemoraLinkRevocationOutcome
    suspend fun forget(hostId: String): AppRemoraLinkForgetResult
}

internal class RemoraLinkHostsController(private val api: RemoraLinkHostsApi) {
    private val _state = MutableStateFlow(RemoraLinkHostsUiState())
    val state: StateFlow<RemoraLinkHostsUiState> = _state.asStateFlow()

    suspend fun refresh() {
        _state.value = _state.value.copy(loading = true, message = null)
        try {
            _state.value = _state.value.copy(
                hosts = visibleHosts(api.hosts()),
                loading = false,
            )
        } catch (error: Exception) {
            _state.value = _state.value.copy(loading = false, message = remoraLinkErrorMessage(error))
        }
    }

    suspend fun refreshAfterResumedPairingDismissal() {
        refresh()
    }

    suspend fun restart(host: AppRemoraLinkHostSummary, runtimeId: String) {
        if (!host.canRestartRuntime(runtimeId)) return
        _state.value = _state.value.copy(busyHostId = host.hostId, message = null)
        val outcome = try {
            api.restart(host.hostId, runtimeId)
        } catch (error: Exception) {
            refreshAfterOperationFailure(error)
            return
        }
        val message = when (outcome) {
            is AppRemoraLinkRestartOutcome.Succeeded ->
                "Restart succeeded for $runtimeId on ${host.hostDisplayName} " +
                    "(sequence ${outcome.commandSequence})."
            is AppRemoraLinkRestartOutcome.OutcomeUnknown ->
                "Restart outcome is unknown for $runtimeId on ${host.hostDisplayName} " +
                    "(sequence ${outcome.commandSequence}). Check the host before acknowledging."
        }
        refreshAfterOperation(message)
    }

    suspend fun acknowledgeUnknownRestart(host: AppRemoraLinkHostSummary) {
        val pendingRestart = host.pendingRestart
        if (pendingRestart?.outcomeUnknown != true) return
        _state.value = _state.value.copy(busyHostId = host.hostId, message = null)
        val acknowledgedSequence = try {
            api.acknowledgeUnknownRestart(host.hostId)
        } catch (error: Exception) {
            refreshAfterOperationFailure(error)
            return
        }
        refreshAfterOperation(
            "Acknowledged the checked restart for ${pendingRestart.runtimeId} on " +
                "${host.hostDisplayName} (sequence $acknowledgedSequence).",
        )
    }

    suspend fun revoke(host: AppRemoraLinkHostSummary) {
        if (host.state == AppRemoraLinkHostState.FORGOTTEN) return
        _state.value = _state.value.copy(busyHostId = host.hostId, message = null)
        try {
            when (api.revoke(host.hostId)) {
                AppRemoraLinkRevocationOutcome.REVOKED -> {
                    _state.value = _state.value.copy(
                        hosts = visibleHosts(api.hosts()),
                        busyHostId = null,
                        message = "Access revoked for ${host.hostDisplayName}.",
                    )
                }
                AppRemoraLinkRevocationOutcome.OUTCOME_UNKNOWN -> {
                    _state.value = _state.value.copy(
                        busyHostId = null,
                        message = "The revoke result is unknown. Check the host before retrying.",
                    )
                }
            }
        } catch (error: Exception) {
            _state.value = _state.value.copy(busyHostId = null, message = remoraLinkErrorMessage(error))
        }
    }

    suspend fun forget(host: AppRemoraLinkHostSummary) {
        if (host.state == AppRemoraLinkHostState.FORGOTTEN) return
        _state.value = _state.value.copy(busyHostId = host.hostId, message = null)
        try {
            val result = api.forget(host.hostId)
            _state.value = _state.value.copy(
                hosts = visibleHosts(api.hosts()),
                busyHostId = null,
                cleanupRequiredHostName = if (result.hostRevocationStillRequired) {
                    host.hostDisplayName
                } else {
                    null
                },
                message = if (result.alreadyForgotten) {
                    "${host.hostDisplayName} was already forgotten on this device."
                } else {
                    "Forgot ${host.hostDisplayName} on this device."
                },
            )
        } catch (error: Exception) {
            _state.value = _state.value.copy(busyHostId = null, message = remoraLinkErrorMessage(error))
        }
    }

    private fun visibleHosts(hosts: List<AppRemoraLinkHostSummary>) = hosts
        .filterNot { it.state == AppRemoraLinkHostState.FORGOTTEN }
        .sortedBy { it.hostDisplayName.lowercase() }

    private suspend fun refreshAfterOperationFailure(error: Exception) {
        refreshAfterOperation(remoraLinkErrorMessage(error))
    }

    private suspend fun refreshAfterOperation(message: String) {
        var refreshed = true
        val hosts = try {
            visibleHosts(api.hosts())
        } catch (_: Exception) {
            refreshed = false
            _state.value.hosts
        }
        _state.value = _state.value.copy(
            hosts = hosts,
            busyHostId = null,
            message = if (refreshed) message else "$message Host status could not be refreshed.",
        )
    }
}

@Composable
internal fun RemoraLinkHostsScreen(onBack: () -> Unit) {
    val appModel = LocalAppModel.current
    val api = remember(appModel) {
        object : RemoraLinkHostsApi {
            override suspend fun hosts(): List<AppRemoraLinkHostSummary> =
                appModel.withRemoraLinkV2 { it.remoraLinkHosts() }

            override suspend fun restart(
                hostId: String,
                runtimeId: String,
            ): AppRemoraLinkRestartOutcome =
                appModel.withRemoraLinkV2 { it.restartRemoraLinkRuntime(hostId, runtimeId) }

            override suspend fun acknowledgeUnknownRestart(hostId: String): ULong =
                appModel.withRemoraLinkV2 { it.acknowledgeRemoraLinkUnknownRestart(hostId) }

            override suspend fun revoke(hostId: String): AppRemoraLinkRevocationOutcome =
                appModel.withRemoraLinkV2 { it.revokeRemoraLinkHost(hostId) }

            override suspend fun forget(hostId: String): AppRemoraLinkForgetResult =
                appModel.withRemoraLinkV2 { it.forgetRemoraLinkHost(hostId) }
        }
    }
    RemoraLinkHostsContent(onBack = onBack, api = api)
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
internal fun RemoraLinkHostsContent(
    onBack: () -> Unit,
    api: RemoraLinkHostsApi,
    resumedPairingContent: RemoraLinkResumedPairingContent =
        { hostId, pendingApproval, onDismiss, onPaired ->
            RemotePairingSheet(
                onDismiss = onDismiss,
                onPaired = onPaired,
                resumeHostId = hostId,
                pendingApproval = pendingApproval,
            )
        },
) {
    val scope = rememberCoroutineScope()
    val controller = remember(api) { RemoraLinkHostsController(api) }
    val state by controller.state.collectAsState()
    var revokeTarget by remember { mutableStateOf<AppRemoraLinkHostSummary?>(null) }
    var forgetTarget by remember { mutableStateOf<AppRemoraLinkHostSummary?>(null) }
    var resumeTarget by remember { mutableStateOf<AppRemoraLinkHostSummary?>(null) }
    var restartTarget by remember { mutableStateOf<Pair<AppRemoraLinkHostSummary, String>?>(null) }
    var acknowledgeRestartTarget by remember { mutableStateOf<AppRemoraLinkHostSummary?>(null) }

    fun dismissResumedPairingSheet() {
        resumeTarget = null
        scope.launch { controller.refreshAfterResumedPairingDismissal() }
    }

    LaunchedEffect(controller) { controller.refresh() }

    LazyColumn(
        modifier = Modifier.fillMaxWidth().padding(16.dp),
        verticalArrangement = Arrangement.spacedBy(10.dp),
    ) {
        item {
            RemoraLinkHostsTitle(onBack)
            Text(
                "Revoke removes this device’s access on the host. Forget only removes the local record.",
                color = RemoraTheme.textSecondary,
                fontSize = 12.sp,
                modifier = Modifier.padding(top = 8.dp),
            )
        }

        state.cleanupRequiredHostName?.let { hostName ->
            item {
                HostNotice(
                    "Host cleanup required",
                    "$hostName was forgotten locally, but may still authorize this device. Remove Remora from that host before pairing again.",
                )
            }
        }
        state.message?.let { message -> item { HostNotice("Remora Link", message) } }

        if (state.loading) {
            item {
                Row(
                    verticalAlignment = Alignment.CenterVertically,
                    horizontalArrangement = Arrangement.spacedBy(10.dp),
                    modifier = Modifier.fillMaxWidth().padding(vertical = 24.dp),
                ) {
                    CircularProgressIndicator(Modifier.size(20.dp), strokeWidth = 2.dp, color = RemoraTheme.accent)
                    Text("Loading hosts…", color = RemoraTheme.textSecondary, fontSize = 13.sp)
                }
            }
        } else if (state.hosts.isEmpty()) {
            item {
                Text(
                    "No Remora Link hosts are paired with this device.",
                    color = RemoraTheme.textSecondary,
                    fontSize = 13.sp,
                    textAlign = TextAlign.Center,
                    modifier = Modifier.fillMaxWidth().padding(vertical = 28.dp),
                )
            }
        } else {
            items(state.hosts, key = { it.hostId }) { host ->
                RemoraLinkHostRow(
                    host = host,
                    busy = state.busyHostId == host.hostId,
                    onRevoke = { revokeTarget = host },
                    onForget = { forgetTarget = host },
                    onContinueApproval = { resumeTarget = host },
                    onRestart = { runtimeId -> restartTarget = host to runtimeId },
                    onAcknowledgeRestart = { acknowledgeRestartTarget = host },
                )
            }
        }

        item {
            TextButton(
                onClick = { scope.launch { controller.refresh() } },
                enabled = !state.loading && state.busyHostId == null,
                modifier = Modifier.fillMaxWidth().heightIn(min = RemoraTheme.minimumTouchTarget),
            ) { Text("Refresh", color = RemoraTheme.accent) }
            Spacer(Modifier.height(20.dp))
        }
    }

    revokeTarget?.let { host ->
        AlertDialog(
            onDismissRequest = { revokeTarget = null },
            title = { Text("Revoke host access?") },
            text = { Text("This asks ${host.hostDisplayName} to revoke this device and removes its active Remora Link access.") },
            confirmButton = {
                TextButton(onClick = {
                    revokeTarget = null
                    scope.launch { controller.revoke(host) }
                }) { Text("Revoke", color = RemoraTheme.danger) }
            },
            dismissButton = { TextButton(onClick = { revokeTarget = null }) { Text("Cancel") } },
        )
    }

    restartTarget?.let { (host, runtimeId) ->
        val pendingRestart = host.pendingRestart
            ?.takeIf { it.runtimeId == runtimeId && !it.outcomeUnknown }
        AlertDialog(
            onDismissRequest = { restartTarget = null },
            title = { Text(if (pendingRestart == null) "Restart $runtimeId?" else "Retry restart for $runtimeId?") },
            text = {
                Text(
                    if (pendingRestart == null) {
                        "This sends one restart command to $runtimeId on ${host.hostDisplayName}. " +
                            "The runtime connection will be interrupted."
                    } else {
                        "This retries the same durable restart command for $runtimeId on " +
                            "${host.hostDisplayName} (sequence ${pendingRestart.commandSequence}). " +
                            "It does not issue a new sequence."
                    },
                )
            },
            confirmButton = {
                TextButton(onClick = {
                    restartTarget = null
                    scope.launch { controller.restart(host, runtimeId) }
                }) { Text("Restart runtime", color = RemoraTheme.danger) }
            },
            dismissButton = { TextButton(onClick = { restartTarget = null }) { Text("Cancel") } },
        )
    }

    acknowledgeRestartTarget?.let { host ->
        val pendingRestart = host.pendingRestart
        if (pendingRestart?.outcomeUnknown == true) {
            AlertDialog(
                onDismissRequest = { acknowledgeRestartTarget = null },
                title = { Text("Acknowledge checked host?") },
                text = {
                    Text(
                        "Only continue after checking ${host.hostDisplayName} directly. This clears the " +
                            "unknown outcome for ${pendingRestart.runtimeId} (sequence " +
                            "${pendingRestart.commandSequence}) and allows a later explicit restart.",
                    )
                },
                confirmButton = {
                    TextButton(onClick = {
                        acknowledgeRestartTarget = null
                        scope.launch { controller.acknowledgeUnknownRestart(host) }
                    }) { Text("I checked the host", color = RemoraTheme.danger) }
                },
                dismissButton = {
                    TextButton(onClick = { acknowledgeRestartTarget = null }) { Text("Cancel") }
                },
            )
        }
    }

    forgetTarget?.let { host ->
        AlertDialog(
            onDismissRequest = { forgetTarget = null },
            title = { Text("Forget local record?") },
            text = {
                Text(
                    "This does not revoke access on ${host.hostDisplayName}. If the host is reachable, revoke first. " +
                        "After forgetting, host-side cleanup may still be required.",
                )
            },
            confirmButton = {
                TextButton(onClick = {
                    forgetTarget = null
                    scope.launch { controller.forget(host) }
                }) { Text("Forget locally", color = RemoraTheme.danger) }
            },
            dismissButton = { TextButton(onClick = { forgetTarget = null }) { Text("Cancel") } },
        )
    }

    resumeTarget?.let { host ->
        val pending = host.pendingApproval
        if (pending != null && host.state != AppRemoraLinkHostState.FORGOTTEN) {
            ModalBottomSheet(
                onDismissRequest = ::dismissResumedPairingSheet,
                sheetState = rememberModalBottomSheetState(skipPartiallyExpanded = true),
                containerColor = RemoraTheme.background,
            ) {
                resumedPairingContent(
                    host.hostId,
                    pending,
                    ::dismissResumedPairingSheet,
                    { dismissResumedPairingSheet() },
                )
            }
        }
    }
}

@Composable
internal fun RemoraLinkHostsTitle(onBack: () -> Unit) {
    Row(
        modifier = Modifier.fillMaxWidth(),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        IconButton(
            onClick = onBack,
            modifier = Modifier.size(RemoraTheme.minimumTouchTarget),
        ) {
            Icon(Icons.AutoMirrored.Filled.ArrowBack, "Back to Settings", tint = RemoraTheme.accent)
        }
        Text(
            "Remora Link Hosts",
            color = RemoraTheme.textPrimary,
            fontSize = 17.sp,
            fontWeight = FontWeight.SemiBold,
            textAlign = TextAlign.Center,
            modifier = Modifier.weight(1f).padding(horizontal = 8.dp),
        )
        Spacer(Modifier.width(RemoraTheme.minimumTouchTarget))
    }
}

@Composable
internal fun RemoraLinkHostRow(
    host: AppRemoraLinkHostSummary,
    busy: Boolean,
    onRevoke: () -> Unit,
    onForget: () -> Unit,
    onContinueApproval: () -> Unit,
    onRestart: (String) -> Unit,
    onAcknowledgeRestart: () -> Unit,
) {
    Column(
        modifier = Modifier
            .fillMaxWidth()
            .background(RemoraTheme.surface, RoundedCornerShape(12.dp))
            .padding(12.dp),
        verticalArrangement = Arrangement.spacedBy(8.dp),
    ) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            Column(Modifier.weight(1f)) {
                Text(host.hostDisplayName, color = RemoraTheme.textPrimary, fontSize = 14.sp, fontWeight = FontWeight.Medium)
                Text(hostStateLabel(host.state), color = hostStateColor(host.state), fontSize = 11.sp)
            }
            if (busy) CircularProgressIndicator(Modifier.size(18.dp), strokeWidth = 2.dp, color = RemoraTheme.accent)
        }
        if (host.selectedRuntimeIds.isNotEmpty()) {
            Text(
                "Runtimes: ${host.selectedRuntimeIds.joinToString()}",
                color = RemoraTheme.textSecondary,
                fontSize = 11.sp,
                fontFamily = RemoraTheme.monoFont,
            )
        }
        if (host.grantedScopes.isNotEmpty()) {
            Text(
                "Permissions: ${host.grantedScopes.joinToString { hostScopeLabel(it) }}",
                color = RemoraTheme.textSecondary,
                fontSize = 11.sp,
            )
        }
        host.pendingRestart?.takeIf { it.outcomeUnknown }?.let { pendingRestart ->
            Column(
                modifier = Modifier
                    .fillMaxWidth()
                    .background(RemoraTheme.background, RoundedCornerShape(8.dp))
                    .padding(10.dp),
                verticalArrangement = Arrangement.spacedBy(4.dp),
            ) {
                Text(
                    "Restart outcome unknown",
                    color = RemoraTheme.danger,
                    fontSize = 12.sp,
                    fontWeight = FontWeight.SemiBold,
                )
                Text(
                    "Runtime: ${pendingRestart.runtimeId}",
                    color = RemoraTheme.textPrimary,
                    fontSize = 11.sp,
                    fontFamily = RemoraTheme.monoFont,
                )
                Text(
                    "Sequence: ${pendingRestart.commandSequence}",
                    color = RemoraTheme.textSecondary,
                    fontSize = 11.sp,
                    fontFamily = RemoraTheme.monoFont,
                )
                Text(
                    "Check the host directly before acknowledging this result.",
                    color = RemoraTheme.textSecondary,
                    fontSize = 11.sp,
                )
            }
            OutlinedButton(
                onClick = onAcknowledgeRestart,
                enabled = !busy,
                modifier = Modifier.fillMaxWidth().heightIn(min = RemoraTheme.minimumTouchTarget),
            ) {
                Text("Acknowledge after checking host", color = RemoraTheme.textSecondary)
            }
        }
        host.pendingRestart?.takeIf { !it.outcomeUnknown }?.let { pendingRestart ->
            Column(
                modifier = Modifier
                    .fillMaxWidth()
                    .background(RemoraTheme.background, RoundedCornerShape(8.dp))
                    .padding(10.dp),
                verticalArrangement = Arrangement.spacedBy(4.dp),
            ) {
                Text(
                    "Restart pending",
                    color = RemoraTheme.textPrimary,
                    fontSize = 12.sp,
                    fontWeight = FontWeight.SemiBold,
                )
                Text(
                    "Runtime: ${pendingRestart.runtimeId}",
                    color = RemoraTheme.textPrimary,
                    fontSize = 11.sp,
                    fontFamily = RemoraTheme.monoFont,
                )
                Text(
                    "Sequence: ${pendingRestart.commandSequence}",
                    color = RemoraTheme.textSecondary,
                    fontSize = 11.sp,
                    fontFamily = RemoraTheme.monoFont,
                )
                Text(
                    "Retrying reuses this durable command sequence.",
                    color = RemoraTheme.textSecondary,
                    fontSize = 11.sp,
                )
            }
        }
        host.pendingApproval?.let { pending ->
            Column(
                modifier = Modifier.fillMaxWidth().background(RemoraTheme.background, RoundedCornerShape(8.dp)).padding(10.dp),
            ) {
                Text("Approval pending", color = RemoraTheme.textPrimary, fontSize = 12.sp, fontWeight = FontWeight.SemiBold)
                Text(
                    pending.sas,
                    color = RemoraTheme.accent,
                    fontFamily = RemoraTheme.monoFont,
                    fontSize = 20.sp,
                    modifier = Modifier.semantics { contentDescription = "Pending security code ${pending.sas}" },
                )
                Text(
                    "Closing pairing did not cancel it. Continue here after approving this code on the host.",
                    color = RemoraTheme.textSecondary,
                    fontSize = 11.sp,
                )
            }
            Button(
                onClick = onContinueApproval,
                enabled = !busy,
                modifier = Modifier.fillMaxWidth().heightIn(min = RemoraTheme.minimumTouchTarget),
                colors = ButtonDefaults.buttonColors(
                    containerColor = RemoraTheme.accent.copy(alpha = 0.18f),
                    contentColor = RemoraTheme.accent,
                ),
            ) { Text("Continue approval") }
        }
        if (host.hostRevocationStillRequired) {
            Text("Host cleanup still required", color = RemoraTheme.danger, fontSize = 11.sp, fontWeight = FontWeight.SemiBold)
        }
        if (host.canOfferRestartActions()) {
            Text("Runtime restarts", color = RemoraTheme.textPrimary, fontSize = 12.sp, fontWeight = FontWeight.SemiBold)
            host.selectedRuntimeIds.distinct().forEach { runtimeId ->
                val retryingPreparedRestart = host.pendingRestart
                    ?.let { it.runtimeId == runtimeId && !it.outcomeUnknown }
                    ?: false
                Button(
                    onClick = { onRestart(runtimeId) },
                    enabled = !busy && host.canRestartRuntime(runtimeId),
                    modifier = Modifier.fillMaxWidth().heightIn(min = RemoraTheme.minimumTouchTarget),
                    colors = ButtonDefaults.buttonColors(
                        containerColor = RemoraTheme.accent.copy(alpha = 0.18f),
                        contentColor = RemoraTheme.accent,
                    ),
                ) { Text(if (retryingPreparedRestart) "Retry restart $runtimeId" else "Restart $runtimeId") }
            }
        }
        Row(
            modifier = Modifier.fillMaxWidth(),
            horizontalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            Button(
                onClick = onRevoke,
                enabled = !busy && host.state != AppRemoraLinkHostState.REVOKED && host.state != AppRemoraLinkHostState.FORGOTTEN,
                modifier = Modifier.weight(1f).heightIn(min = RemoraTheme.minimumTouchTarget),
                colors = ButtonDefaults.buttonColors(
                    containerColor = RemoraTheme.accent.copy(alpha = 0.18f),
                    contentColor = RemoraTheme.accent,
                ),
            ) { Text("Revoke") }
            OutlinedButton(
                onClick = onForget,
                enabled = !busy,
                modifier = Modifier.weight(1f).heightIn(min = RemoraTheme.minimumTouchTarget),
            ) { Text("Forget", color = RemoraTheme.textSecondary) }
        }
    }
}

internal fun AppRemoraLinkHostSummary.canOfferRestartActions(): Boolean =
    state == AppRemoraLinkHostState.PAIRED &&
        AppRemoraLinkScope.RESTART_RUNTIME in grantedScopes &&
        selectedRuntimeIds.isNotEmpty()

internal fun AppRemoraLinkHostSummary.canRestartRuntime(runtimeId: String): Boolean {
    val pending = pendingRestart
    return canOfferRestartActions() &&
        runtimeId in selectedRuntimeIds &&
        (pending == null || (pending.runtimeId == runtimeId && !pending.outcomeUnknown))
}

@Composable
private fun HostNotice(title: String, message: String) {
    Column(
        modifier = Modifier.fillMaxWidth().background(RemoraTheme.surface, RoundedCornerShape(10.dp)).padding(12.dp),
        verticalArrangement = Arrangement.spacedBy(4.dp),
    ) {
        Text(title, color = RemoraTheme.textPrimary, fontSize = 12.sp, fontWeight = FontWeight.SemiBold)
        Text(message, color = RemoraTheme.textSecondary, fontSize = 11.sp)
    }
}

private fun hostStateLabel(state: AppRemoraLinkHostState): String = when (state) {
    AppRemoraLinkHostState.INSPECTING -> "Inspecting"
    AppRemoraLinkHostState.READY -> "Ready to pair"
    AppRemoraLinkHostState.PAIRING -> "Pairing"
    AppRemoraLinkHostState.AWAITING_HOST_APPROVAL -> "Awaiting host approval"
    AppRemoraLinkHostState.PAIRED -> "Paired"
    AppRemoraLinkHostState.REVOKING -> "Revoking"
    AppRemoraLinkHostState.REVOKED -> "Revoked"
    AppRemoraLinkHostState.FORGETTING -> "Forgetting"
    AppRemoraLinkHostState.FORGOTTEN -> "Forgotten"
    AppRemoraLinkHostState.NEEDS_REPAIR -> "Needs pairing again"
}

@Composable
private fun hostStateColor(state: AppRemoraLinkHostState) = when (state) {
    AppRemoraLinkHostState.PAIRED -> RemoraTheme.success
    AppRemoraLinkHostState.NEEDS_REPAIR -> RemoraTheme.danger
    else -> RemoraTheme.textSecondary
}

private fun hostScopeLabel(scope: AppRemoraLinkScope): String = when (scope) {
    AppRemoraLinkScope.INSPECT_RUNTIMES -> "inspect"
    AppRemoraLinkScope.CONNECT_RUNTIME -> "connect"
    AppRemoraLinkScope.RESTART_RUNTIME -> "restart"
    AppRemoraLinkScope.SELF_REVOKE -> "self-revoke"
}
