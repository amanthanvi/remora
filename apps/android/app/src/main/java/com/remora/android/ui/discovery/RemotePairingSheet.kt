package com.remora.android.ui.discovery

import android.Manifest
import android.content.ClipData
import android.content.ClipboardManager
import android.content.Context
import android.content.pm.PackageManager
import android.os.Build
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.camera.core.CameraSelector
import androidx.camera.core.ImageAnalysis
import androidx.camera.core.Preview
import androidx.camera.lifecycle.ProcessCameraProvider
import androidx.camera.view.PreviewView
import androidx.compose.foundation.background
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.selection.toggleable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Check
import androidx.compose.material.icons.filled.ContentCopy
import androidx.compose.material.icons.filled.QrCodeScanner
import androidx.compose.material3.Button
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.Checkbox
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.Icon
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalClipboardManager
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.LiveRegionMode
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.liveRegion
import androidx.compose.ui.semantics.password
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.text.input.PasswordVisualTransformation
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.compose.ui.viewinterop.AndroidView
import androidx.core.content.ContextCompat
import androidx.lifecycle.LifecycleOwner
import androidx.lifecycle.compose.LocalLifecycleOwner
import com.google.mlkit.vision.barcode.BarcodeScannerOptions
import com.google.mlkit.vision.barcode.BarcodeScanning
import com.google.mlkit.vision.barcode.common.Barcode
import com.google.mlkit.vision.common.InputImage
import com.remora.android.ui.LocalAppModel
import com.remora.android.ui.RemoraTheme
import com.remora.android.util.LLog
import java.util.concurrent.ExecutorService
import java.util.concurrent.Executors
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import uniffi.codex_mobile_client.AppRemoraLinkAcceptance
import uniffi.codex_mobile_client.AppRemoraLinkConfirmationMode
import uniffi.codex_mobile_client.AppRemoraLinkInspection
import uniffi.codex_mobile_client.AppRemoraLinkPairingCancellationOutcome
import uniffi.codex_mobile_client.AppRemoraLinkPairingCode
import uniffi.codex_mobile_client.AppRemoraLinkPairingOutcome
import uniffi.codex_mobile_client.AppRemoraLinkPendingApproval
import uniffi.codex_mobile_client.AppRemoraLinkScope

private const val LOG_TAG = "RemotePairingSheet"

@Composable
fun RemotePairingSheet(
    onDismiss: () -> Unit,
    onPaired: (hostId: String) -> Unit,
    resumeHostId: String? = null,
    pendingApproval: AppRemoraLinkPendingApproval? = null,
) {
    val appModel = LocalAppModel.current
    val context = LocalContext.current
    val clipboard = LocalClipboardManager.current
    val scope = rememberCoroutineScope()
    val api = remember(appModel) {
        object : RemoraLinkPairingApi {
            override suspend fun checkAvailability() {
                appModel.withRemoraLinkV2 { Unit }
            }

            override suspend fun inspect(code: AppRemoraLinkPairingCode): AppRemoraLinkInspection =
                appModel.withRemoraLinkV2 { it.inspectRemoraLinkCode(code) }

            override suspend fun accept(
                acceptance: AppRemoraLinkAcceptance,
            ): AppRemoraLinkPairingOutcome =
                appModel.withRemoraLinkV2 { it.acceptRemoraLinkOffer(acceptance) }.also {
                    com.remora.android.background.BackgroundAwareness.onPairingChanged(context)
                }

            override suspend fun await(hostId: String): AppRemoraLinkPairingOutcome =
                appModel.withRemoraLinkV2 { it.awaitRemoraLinkPairing(hostId, null) }.also {
                    com.remora.android.background.BackgroundAwareness.onPairingChanged(context)
                }

            override suspend fun cancel(
                hostId: String,
            ): AppRemoraLinkPairingCancellationOutcome =
                appModel.withRemoraLinkV2 { it.cancelRemoraLinkPairing(hostId) }
        }
    }
    val controller = remember(api) {
        RemoraLinkPairingController(api, Build.MODEL.orEmpty())
    }
    val state by controller.state.collectAsState()

    var showScanner by remember { mutableStateOf(false) }
    var showPaste by remember { mutableStateOf(false) }
    var pastedCode by remember { mutableStateOf("") }
    var cameraDenied by remember { mutableStateOf(false) }

    fun inspectCode(code: String) {
        pastedCode = ""
        scope.launch { controller.submitCode(code) }
    }

    val permissionLauncher = rememberLauncherForActivityResult(
        ActivityResultContracts.RequestPermission(),
    ) { granted ->
        cameraDenied = !granted
        showScanner = granted
    }

    fun requestCameraAndScan() {
        if (
            ContextCompat.checkSelfPermission(context, Manifest.permission.CAMERA) ==
            PackageManager.PERMISSION_GRANTED
        ) {
            cameraDenied = false
            showScanner = true
        } else {
            permissionLauncher.launch(Manifest.permission.CAMERA)
        }
    }

    LaunchedEffect(controller, resumeHostId, pendingApproval) {
        if (resumeHostId != null && pendingApproval != null) {
            controller.resume(resumeHostId, pendingApproval)
        } else {
            controller.checkAvailability()
        }
    }

    if (showScanner) {
        QrScannerScreen(
            onScanned = { code ->
                showScanner = false
                inspectCode(code)
            },
            onCancel = { showScanner = false },
        )
        return
    }

    Column(
        modifier = Modifier
            .fillMaxWidth()
            .background(RemoraTheme.background)
            .verticalScroll(rememberScrollState())
            .padding(horizontal = 20.dp, vertical = 18.dp),
        verticalArrangement = Arrangement.spacedBy(14.dp),
    ) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            Text(
                text = "Remora Link",
                color = RemoraTheme.textPrimary,
                fontSize = 18.sp,
                fontWeight = FontWeight.SemiBold,
                modifier = Modifier.weight(1f),
            )
            TextButton(onClick = onDismiss, modifier = Modifier.heightIn(min = RemoraTheme.minimumTouchTarget)) {
                Text(
                    if (state is RemoraLinkPairingState.Awaiting) "Continue later" else "Close",
                    color = RemoraTheme.accent,
                )
            }
        }

        when (val current = state) {
            is RemoraLinkPairingState.Availability -> AvailabilityContent(
                current,
                onRetry = { scope.launch { controller.checkAvailability() } },
            )
            RemoraLinkPairingState.Ingress -> IngressContent(
                showPaste = showPaste,
                pastedCode = pastedCode,
                cameraDenied = cameraDenied,
                onScan = ::requestCameraAndScan,
                onShowPaste = { showPaste = true },
                onPasteChanged = { pastedCode = it },
                onPasteClipboard = { clipboard.getText()?.text?.let { pastedCode = it } },
                onInspect = { inspectCode(pastedCode) },
            )
            RemoraLinkPairingState.Inspecting -> ProgressContent("Checking pairing code…")
            is RemoraLinkPairingState.Offer -> OfferContent(
                state = current,
                onDeviceNameChange = controller::updateDeviceDisplayName,
                onToggleRuntime = controller::toggleRuntime,
                onToggleScope = controller::toggleScope,
                onAccept = { scope.launch { controller.acceptOffer() } },
            )
            is RemoraLinkPairingState.Accepting -> ProgressContent(
                "Securing ${current.hostDisplayName}…",
            )
            is RemoraLinkPairingState.Awaiting -> AwaitingContent(
                state = current,
                onAwait = { scope.launch { controller.awaitApproval() } },
                onCancel = { scope.launch { controller.cancelPairing() } },
                onDismiss = onDismiss,
            )
            is RemoraLinkPairingState.Cancelling -> ProgressContent("Cancelling pairing…")
            is RemoraLinkPairingState.OutcomeUnknown -> MessageContent(
                title = "Outcome unknown",
                message = current.message,
                actionLabel = "Check again",
                onAction = controller::returnToIngress,
            )
            is RemoraLinkPairingState.Success -> SuccessContent(
                state = current,
                onDone = { onPaired(current.hostId) },
            )
            is RemoraLinkPairingState.Failure -> MessageContent(
                title = "Couldn’t pair",
                message = current.message,
                actionLabel = "Try another code",
                onAction = controller::returnToIngress,
            )
        }
        Spacer(Modifier.height(12.dp))
    }
}

@Composable
private fun AvailabilityContent(
    state: RemoraLinkPairingState.Availability,
    onRetry: () -> Unit,
) {
    if (state.checking) {
        ProgressContent("Checking Remora Link availability…")
    } else {
        MessageContent(
            title = "Remora Link unavailable",
            message = state.message ?: "Secure pairing could not be initialized.",
            actionLabel = "Retry",
            onAction = onRetry,
        )
    }
}

@Composable
private fun IngressContent(
    showPaste: Boolean,
    pastedCode: String,
    cameraDenied: Boolean,
    onScan: () -> Unit,
    onShowPaste: () -> Unit,
    onPasteChanged: (String) -> Unit,
    onPasteClipboard: () -> Unit,
    onInspect: () -> Unit,
) {
    Text(
        "On the host, run this command. Then scan or paste the one-time pairing code.",
        color = RemoraTheme.textSecondary,
        fontSize = 13.sp,
    )
    PairCommandRow()
    Row(
        modifier = Modifier.fillMaxWidth(),
        horizontalArrangement = Arrangement.spacedBy(10.dp),
    ) {
        PrimaryIngressButton(
            label = "Scan QR",
            icon = Icons.Default.QrCodeScanner,
            onClick = onScan,
            modifier = Modifier.weight(1f),
        )
        PrimaryIngressButton(
            label = "Enter code",
            icon = Icons.Default.ContentCopy,
            onClick = onShowPaste,
            modifier = Modifier.weight(1f),
        )
    }
    if (cameraDenied) {
        Text(
            "Camera access was denied. You can paste the same pairing code instead.",
            color = RemoraTheme.textSecondary,
            fontSize = 12.sp,
        )
    }
    if (showPaste) {
        OutlinedTextField(
            value = pastedCode,
            onValueChange = onPasteChanged,
            label = { Text("One-time pairing code") },
            placeholder = { Text("Paste code", fontFamily = RemoraTheme.monoFont) },
            minLines = 2,
            maxLines = 4,
            keyboardOptions = KeyboardOptions(
                autoCorrectEnabled = false,
                keyboardType = KeyboardType.Password,
            ),
            visualTransformation = PasswordVisualTransformation(),
            modifier = Modifier
                .fillMaxWidth()
                .semantics {
                    contentDescription = "Remora Link one-time pairing code. Secure entry."
                    password()
                },
        )
        Row(
            modifier = Modifier.fillMaxWidth(),
            horizontalArrangement = Arrangement.SpaceBetween,
        ) {
            TextButton(onClick = onPasteClipboard, modifier = Modifier.heightIn(min = RemoraTheme.minimumTouchTarget)) {
                Text("Paste from clipboard", color = RemoraTheme.accent)
            }
            Button(
                onClick = onInspect,
                enabled = pastedCode.isNotBlank(),
                modifier = Modifier.heightIn(min = RemoraTheme.minimumTouchTarget),
                colors = pairingButtonColors(),
            ) { Text("Continue") }
        }
    }
}

@Composable
private fun OfferContent(
    state: RemoraLinkPairingState.Offer,
    onDeviceNameChange: (String) -> Unit,
    onToggleRuntime: (String) -> Unit,
    onToggleScope: (AppRemoraLinkScope) -> Unit,
    onAccept: () -> Unit,
) {
    Text(state.offer.hostDisplayName, color = RemoraTheme.textPrimary, fontSize = 17.sp, fontWeight = FontWeight.SemiBold)
    Text(
        if (state.offer.confirmationMode == AppRemoraLinkConfirmationMode.INTERACTIVE) {
            "This host requires approval. You’ll compare a short security code on both devices."
        } else {
            "Review what this host is offering before pairing."
        },
        color = RemoraTheme.textSecondary,
        fontSize = 12.sp,
    )
    OutlinedTextField(
        value = state.deviceDisplayName,
        onValueChange = onDeviceNameChange,
        label = { Text("This device’s name") },
        supportingText = { Text("Up to $REMORA_LINK_DEVICE_NAME_MAX_BYTES UTF-8 bytes") },
        singleLine = true,
        modifier = Modifier.fillMaxWidth(),
    )

    PairingSectionHeader("Runtimes")
    state.offer.runtimeOffers.forEach { runtime ->
        SelectablePairingRow(
            label = runtime.displayName,
            subtitle = when {
                !runtime.available -> "Unavailable"
                runtime.recommended -> "Recommended"
                else -> null
            },
            checked = runtime.runtimeId in state.selectedRuntimeIds,
            enabled = runtime.available,
            onClick = { onToggleRuntime(runtime.runtimeId) },
        )
    }

    PairingSectionHeader("Permissions")
    state.offer.maximumScopes.forEach { scope ->
        val required = scope in state.offer.requiredScopes
        SelectablePairingRow(
            label = scopeLabel(scope),
            subtitle = if (required) "Required by host" else scopeDescription(scope),
            checked = scope in state.selectedScopes,
            enabled = !required,
            onClick = { onToggleScope(scope) },
        )
    }
    state.validationMessage?.let {
        Text(
            it,
            color = RemoraTheme.danger,
            fontSize = 12.sp,
            modifier = Modifier.semantics { liveRegion = LiveRegionMode.Assertive },
        )
    }
    Button(
        onClick = onAccept,
        modifier = Modifier.fillMaxWidth().heightIn(min = RemoraTheme.minimumTouchTarget),
        colors = pairingButtonColors(),
    ) { Text("Pair securely") }
}

@Composable
private fun AwaitingContent(
    state: RemoraLinkPairingState.Awaiting,
    onAwait: () -> Unit,
    onCancel: () -> Unit,
    onDismiss: () -> Unit,
) {
    Text("Approve on the host", color = RemoraTheme.textPrimary, fontSize = 17.sp, fontWeight = FontWeight.SemiBold)
    Text(
        "Confirm that this security code matches the one shown on the host:",
        color = RemoraTheme.textSecondary,
        fontSize = 13.sp,
    )
    Text(
        state.sas,
        color = RemoraTheme.accent,
        fontFamily = RemoraTheme.monoFont,
        fontSize = 28.sp,
        fontWeight = FontWeight.Bold,
        textAlign = TextAlign.Center,
        modifier = Modifier
            .fillMaxWidth()
            .background(RemoraTheme.surface, RoundedCornerShape(12.dp))
            .padding(20.dp)
            .semantics { contentDescription = "Security code ${state.sas}" },
    )
    Text(
        "Closing this sheet does not cancel pairing. You can continue later from Remora Link Hosts.",
        color = RemoraTheme.textSecondary,
        fontSize = 12.sp,
    )
    Button(
        onClick = onAwait,
        enabled = !state.waiting,
        modifier = Modifier.fillMaxWidth().heightIn(min = RemoraTheme.minimumTouchTarget),
        colors = pairingButtonColors(),
    ) {
        if (state.waiting) {
            CircularProgressIndicator(Modifier.size(18.dp), strokeWidth = 2.dp, color = RemoraTheme.accent)
            Spacer(Modifier.width(8.dp))
        }
        Text(if (state.waiting) "Waiting for approval…" else "I approved it — continue")
    }
    OutlinedButton(
        onClick = onDismiss,
        modifier = Modifier.fillMaxWidth().heightIn(min = RemoraTheme.minimumTouchTarget),
    ) { Text("Dismiss and continue later", color = RemoraTheme.accent) }
    TextButton(
        onClick = onCancel,
        modifier = Modifier.fillMaxWidth().heightIn(min = RemoraTheme.minimumTouchTarget),
    ) { Text("Cancel pairing", color = RemoraTheme.danger) }
}

@Composable
private fun SuccessContent(state: RemoraLinkPairingState.Success, onDone: () -> Unit) {
    Text(
        if (state.alreadyPaired) "Already paired" else "Pairing complete",
        color = RemoraTheme.success,
        fontSize = 18.sp,
        fontWeight = FontWeight.SemiBold,
    )
    state.sas?.let {
        Text("Verified security code $it", color = RemoraTheme.textSecondary, fontSize = 12.sp)
    }
    Text(
        "${state.selectedRuntimeIds.size} runtime${if (state.selectedRuntimeIds.size == 1) "" else "s"} available through Remora Link.",
        color = RemoraTheme.textSecondary,
        fontSize = 13.sp,
    )
    Button(
        onClick = onDone,
        modifier = Modifier.fillMaxWidth().heightIn(min = RemoraTheme.minimumTouchTarget),
        colors = pairingButtonColors(),
    ) { Text("Done") }
}

@Composable
private fun ProgressContent(message: String) {
    Row(
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(12.dp),
        modifier = Modifier.fillMaxWidth().padding(vertical = 24.dp),
    ) {
        CircularProgressIndicator(Modifier.size(22.dp), strokeWidth = 2.dp, color = RemoraTheme.accent)
        Text(message, color = RemoraTheme.textSecondary, fontSize = 13.sp)
    }
}

@Composable
private fun MessageContent(
    title: String,
    message: String,
    actionLabel: String,
    onAction: () -> Unit,
) {
    Text(title, color = RemoraTheme.textPrimary, fontSize = 17.sp, fontWeight = FontWeight.SemiBold)
    Text(
        message,
        color = RemoraTheme.textSecondary,
        fontSize = 13.sp,
        modifier = Modifier.semantics { liveRegion = LiveRegionMode.Assertive },
    )
    Button(
        onClick = onAction,
        modifier = Modifier.fillMaxWidth().heightIn(min = RemoraTheme.minimumTouchTarget),
        colors = pairingButtonColors(),
    ) { Text(actionLabel) }
}

@Composable
private fun SelectablePairingRow(
    label: String,
    subtitle: String?,
    checked: Boolean,
    enabled: Boolean,
    onClick: () -> Unit,
) {
    Row(
        verticalAlignment = Alignment.CenterVertically,
        modifier = Modifier
            .fillMaxWidth()
            .background(RemoraTheme.surface, RoundedCornerShape(10.dp))
            .toggleable(
                value = checked,
                enabled = enabled,
                role = Role.Checkbox,
                onValueChange = { onClick() },
            )
            .padding(horizontal = 12.dp, vertical = 8.dp),
    ) {
        Column(Modifier.weight(1f)) {
            Text(label, color = if (enabled) RemoraTheme.textPrimary else RemoraTheme.textMuted, fontSize = 13.sp)
            subtitle?.let { Text(it, color = RemoraTheme.textSecondary, fontSize = 11.sp) }
        }
        Checkbox(
            checked = checked,
            onCheckedChange = null,
            enabled = enabled,
            modifier = Modifier.size(RemoraTheme.minimumTouchTarget),
        )
    }
}

@Composable
private fun PrimaryIngressButton(
    label: String,
    icon: androidx.compose.ui.graphics.vector.ImageVector,
    onClick: () -> Unit,
    modifier: Modifier,
) {
    Button(
        onClick = onClick,
        modifier = modifier.heightIn(min = RemoraTheme.minimumTouchTarget),
        colors = pairingButtonColors(),
    ) {
        Icon(icon, contentDescription = null, modifier = Modifier.size(18.dp))
        Spacer(Modifier.width(6.dp))
        Text(label)
    }
}

@Composable
private fun PairCommandRow() {
    val context = LocalContext.current
    val scope = rememberCoroutineScope()
    var copied by remember { mutableStateOf(false) }
    Column(
        verticalArrangement = Arrangement.spacedBy(8.dp),
        modifier = Modifier.fillMaxWidth(),
    ) {
        Row(
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(8.dp),
            modifier = Modifier
                .fillMaxWidth()
                .background(RemoraTheme.surface, RoundedCornerShape(10.dp))
                .padding(start = 12.dp, end = 4.dp, top = 4.dp, bottom = 4.dp),
        ) {
            Text(
                REMORA_LINK_PAIR_COMMAND,
                color = RemoraTheme.textPrimary,
                fontFamily = RemoraTheme.monoFont,
                fontSize = 12.sp,
                modifier = Modifier.weight(1f),
            )
            TextButton(
                onClick = {
                    val manager = context.getSystemService(Context.CLIPBOARD_SERVICE) as? ClipboardManager
                    manager?.setPrimaryClip(ClipData.newPlainText("Remora Link pairing command", REMORA_LINK_PAIR_COMMAND))
                    copied = true
                    scope.launch {
                        delay(1_400)
                        copied = false
                    }
                },
                modifier = Modifier.size(RemoraTheme.minimumTouchTarget),
                contentPadding = PaddingValues(0.dp),
            ) {
                Icon(
                    if (copied) Icons.Default.Check else Icons.Default.ContentCopy,
                    contentDescription = if (copied) "Pairing command copied" else "Copy pairing command",
                    tint = RemoraTheme.accent,
                    modifier = Modifier.size(18.dp),
                )
            }
        }
        Text(
            "Replace codex with an ID from remora-link agents, or repeat --runtime to authorize more than one harness.",
            color = RemoraTheme.textSecondary,
            fontFamily = RemoraTheme.monoFont,
            fontSize = 11.sp,
        )
    }
}

@Composable
private fun PairingSectionHeader(label: String) {
    Text(
        label.uppercase(),
        color = RemoraTheme.textSecondary,
        fontSize = 11.sp,
        fontWeight = FontWeight.SemiBold,
        modifier = Modifier.padding(top = 4.dp),
    )
}

@Composable
private fun pairingButtonColors() = ButtonDefaults.buttonColors(
    containerColor = RemoraTheme.accent.copy(alpha = 0.18f),
    contentColor = RemoraTheme.accent,
    disabledContainerColor = RemoraTheme.surface,
    disabledContentColor = RemoraTheme.textMuted,
)

private fun scopeLabel(scope: AppRemoraLinkScope): String = when (scope) {
    AppRemoraLinkScope.INSPECT_RUNTIMES -> "See available runtimes"
    AppRemoraLinkScope.CONNECT_RUNTIME -> "Connect to runtimes"
    AppRemoraLinkScope.RESTART_RUNTIME -> "Restart runtimes"
    AppRemoraLinkScope.SELF_REVOKE -> "Revoke this device"
}

private fun scopeDescription(scope: AppRemoraLinkScope): String = when (scope) {
    AppRemoraLinkScope.INSPECT_RUNTIMES -> "Read runtime availability"
    AppRemoraLinkScope.CONNECT_RUNTIME -> "Open runtime sessions"
    AppRemoraLinkScope.RESTART_RUNTIME -> "Request runtime restarts"
    AppRemoraLinkScope.SELF_REVOKE -> "Allow this device to revoke its host access"
}

@Composable
private fun QrScannerScreen(onScanned: (String) -> Unit, onCancel: () -> Unit) {
    val context = LocalContext.current
    val lifecycleOwner = LocalLifecycleOwner.current
    val executor = remember { Executors.newSingleThreadExecutor() }
    val barcodeScanner = remember {
        BarcodeScanning.getClient(
            BarcodeScannerOptions.Builder().setBarcodeFormats(Barcode.FORMAT_QR_CODE).build(),
        )
    }
    var scanned by remember { mutableStateOf(false) }

    DisposableEffect(Unit) {
        onDispose {
            executor.shutdown()
            barcodeScanner.close()
        }
    }

    Box(Modifier.fillMaxSize().background(Color.Black)) {
        AndroidView(
            modifier = Modifier.fillMaxSize(),
            factory = { scannerContext ->
                PreviewView(scannerContext).also { previewView ->
                    previewView.scaleType = PreviewView.ScaleType.FILL_CENTER
                    bindCameraUseCases(
                        context = scannerContext,
                        lifecycleOwner = lifecycleOwner,
                        previewView = previewView,
                        barcodeScanner = barcodeScanner,
                        executor = executor,
                        onResult = { code ->
                            if (!scanned) {
                                scanned = true
                                onScanned(code)
                            }
                        },
                    )
                }
            },
        )
        Column(
            modifier = Modifier.fillMaxSize().padding(16.dp),
            verticalArrangement = Arrangement.spacedBy(14.dp),
        ) {
            Row(Modifier.fillMaxWidth()) {
                Spacer(Modifier.weight(1f))
                TextButton(
                    onClick = onCancel,
                    modifier = Modifier
                        .heightIn(min = RemoraTheme.minimumTouchTarget)
                        .background(Color.Black.copy(alpha = 0.55f), RoundedCornerShape(24.dp)),
                ) { Text("Cancel", color = Color.White) }
            }
            Column(
                modifier = Modifier
                    .fillMaxWidth()
                    .background(Color.Black.copy(alpha = 0.62f), RoundedCornerShape(14.dp))
                    .padding(14.dp),
                verticalArrangement = Arrangement.spacedBy(10.dp),
            ) {
                Text("Scan Remora Link code", color = Color.White, fontSize = 16.sp, fontWeight = FontWeight.SemiBold)
                Text("Run this on the host:", color = Color.White.copy(alpha = 0.85f), fontSize = 12.sp)
                Text(
                    REMORA_LINK_PAIR_COMMAND,
                    color = Color.White,
                    fontFamily = FontFamily.Monospace,
                    fontSize = 13.sp,
                    modifier = Modifier.fillMaxWidth().background(Color.White.copy(alpha = 0.12f), RoundedCornerShape(8.dp)).padding(10.dp),
                )
                Text(
                    "Replace codex with an ID from remora-link agents, or repeat --runtime for multiple harnesses.",
                    color = Color.White.copy(alpha = 0.85f),
                    fontSize = 12.sp,
                )
                Text("Point the camera at the QR code it prints.", color = Color.White.copy(alpha = 0.85f), fontSize = 12.sp)
            }
            Spacer(Modifier.weight(1f))
            Text(
                "Hold steady — the QR code is detected automatically.",
                color = Color.White.copy(alpha = 0.8f),
                fontSize = 12.sp,
                textAlign = TextAlign.Center,
                modifier = Modifier.fillMaxWidth().background(Color.Black.copy(alpha = 0.5f), RoundedCornerShape(24.dp)).padding(10.dp),
            )
        }
    }
}

@androidx.annotation.OptIn(markerClass = [androidx.camera.core.ExperimentalGetImage::class])
private fun bindCameraUseCases(
    context: Context,
    lifecycleOwner: LifecycleOwner,
    previewView: PreviewView,
    barcodeScanner: com.google.mlkit.vision.barcode.BarcodeScanner,
    executor: ExecutorService,
    onResult: (String) -> Unit,
) {
    val providerFuture = ProcessCameraProvider.getInstance(context)
    providerFuture.addListener({
        val provider = providerFuture.get()
        val preview = Preview.Builder().build().also { it.setSurfaceProvider(previewView.surfaceProvider) }
        val analysis = ImageAnalysis.Builder()
            .setBackpressureStrategy(ImageAnalysis.STRATEGY_KEEP_ONLY_LATEST)
            .build()
        analysis.setAnalyzer(executor) { proxy ->
            val media = proxy.image
            if (media == null) {
                proxy.close()
                return@setAnalyzer
            }
            val image = InputImage.fromMediaImage(media, proxy.imageInfo.rotationDegrees)
            barcodeScanner.process(image)
                .addOnSuccessListener { barcodes ->
                    barcodes.firstOrNull { it.format == Barcode.FORMAT_QR_CODE }?.rawValue?.let(onResult)
                }
                .addOnFailureListener { error ->
                    LLog.w(LOG_TAG, "Barcode analysis failed")
                    LLog.debug(LOG_TAG, error) { "Barcode analysis failure details" }
                }
                .addOnCompleteListener { proxy.close() }
        }
        runCatching {
            provider.unbindAll()
            provider.bindToLifecycle(lifecycleOwner, CameraSelector.DEFAULT_BACK_CAMERA, preview, analysis)
        }.onFailure { error ->
            LLog.w(LOG_TAG, "Camera lifecycle binding failed")
            LLog.debug(LOG_TAG, error) { "Camera lifecycle binding failure details" }
        }
    }, ContextCompat.getMainExecutor(context))
}
