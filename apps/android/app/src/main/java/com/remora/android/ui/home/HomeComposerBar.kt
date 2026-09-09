package com.remora.android.ui.home

import android.content.Context
import android.graphics.Bitmap
import android.graphics.BitmapFactory
import android.net.Uri
import android.provider.OpenableColumns
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.PickVisualMediaRequest
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.ExperimentalLayoutApi
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.WindowInsets
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.ime
import androidx.compose.foundation.layout.isImeVisible
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.Send
import androidx.compose.material.icons.filled.Add
import androidx.compose.material.icons.filled.Close
import androidx.compose.material.icons.filled.Mic
import androidx.compose.material.icons.filled.Stop
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.Text
import androidx.compose.material3.rememberModalBottomSheetState
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
import androidx.compose.ui.draw.clip
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.focus.focusRequester
import androidx.compose.ui.focus.onFocusChanged
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.TextRange
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.input.TextFieldValue
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import com.remora.android.state.AppComposerPayload
import com.remora.android.state.AppModel
import com.remora.android.state.ComposerDraftDestination
import com.remora.android.state.ComposerDraftRecoveryStatus
import com.remora.android.state.ComposerFileAttachment
import com.remora.android.state.ComposerImageAttachment
import com.remora.android.state.LocalAccountLoginRequiredException
import com.remora.android.state.VoiceTranscriptionManager
import com.remora.android.ui.RemoraTextStyle
import com.remora.android.ui.LocalAppModel
import com.remora.android.ui.RemoraTheme
import com.remora.android.ui.scaled
import com.remora.android.ui.conversation.ComposerTextInputChrome
import com.remora.android.ui.conversation.RecoverableDraftsRow
import java.io.ByteArrayOutputStream
import kotlinx.coroutines.launch
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import uniffi.codex_mobile_client.AppProject
import uniffi.codex_mobile_client.AuthStatusRequest
import uniffi.codex_mobile_client.ReasoningEffort
import uniffi.codex_mobile_client.ThreadKey

private val SUPPORTED_IMAGE_FILE_MIME_TYPES = arrayOf(
    "image/png",
    "image/jpeg",
    "image/gif",
    "image/webp",
)

private val ALL_FILE_MIME_TYPES = arrayOf("*/*")

/**
 * Lightweight composer for the home screen. When the user sends, it creates a
 * new thread on (project.serverId, project.cwd), submits the initial turn,
 * and stays on home — the thread streams in the task list.
 */
@OptIn(ExperimentalLayoutApi::class, ExperimentalMaterial3Api::class)
@Composable
fun HomeComposerBar(
    project: AppProject?,
    onThreadCreated: (ThreadKey) -> Unit,
    onLoginRequired: (String) -> Unit = {},
    onActiveChange: ((Boolean) -> Unit)? = null,
    onInputFocusChanged: (Boolean) -> Unit = {},
) {
    val appModel = LocalAppModel.current
    val context = LocalContext.current
    val scope = rememberCoroutineScope()

    val draftDestination = remember(project?.serverId, project?.cwd) {
        ComposerDraftDestination.Home(project?.serverId, project?.cwd)
    }
    val recoverableDrafts by appModel.recoverableComposerDrafts.entries.collectAsState()
    val recoveryStorageError by appModel.recoverableComposerDrafts.storageError.collectAsState()
    val destinationDrafts = recoverableDrafts.filter { it.destination == draftDestination }
    var textFieldValue by remember(draftDestination) {
        val saved = appModel.homeComposerDraft(draftDestination).text
        mutableStateOf(TextFieldValue(saved, selection = TextRange(saved.length)))
    }
    val text = textFieldValue.text
    var attachedImage by remember(draftDestination) {
        mutableStateOf(appModel.homeComposerDraft(draftDestination).attachment)
    }
    var attachedFiles by remember(draftDestination) {
        mutableStateOf(appModel.homeComposerDraft(draftDestination).fileAttachments)
    }
    LaunchedEffect(draftDestination, text, attachedImage, attachedFiles) {
        appModel.setHomeComposerDraft(draftDestination, AppModel.ComposerDraft(text, attachedImage, attachedFiles))
    }
    var errorMessage by remember(draftDestination) { mutableStateOf<String?>(null) }
    var isSavingRecovery by remember(draftDestination) { mutableStateOf(false) }
    val isSubmitting = isSavingRecovery || destinationDrafts.any { it.status == ComposerDraftRecoveryStatus.SUBMITTING }
    var isFocused by remember { mutableStateOf(false) }
    var showAttachMenu by remember { mutableStateOf(false) }
    var showExpanded by remember { mutableStateOf(false) }
    val focusRequester = remember { FocusRequester() }

    LaunchedEffect(isFocused, showExpanded, showAttachMenu) {
        onInputFocusChanged(isFocused || showExpanded || showAttachMenu)
    }
    DisposableEffect(Unit) {
        onDispose { onInputFocusChanged(false) }
    }

    // Auto-focus on first composition so the parent's `isComposerActive`
    // flag stays true (it's derived from internal isFocused/text/etc.). Without
    // this, expanding from a collapsed state would immediately collapse back
    // on the next recomposition because nothing has focus yet.
    LaunchedEffect(Unit) {
        runCatching { focusRequester.requestFocus() }
    }

    val transcriptionManager = remember { VoiceTranscriptionManager() }
    val isRecording by transcriptionManager.isRecording.collectAsState()
    val isTranscribing by transcriptionManager.isTranscribing.collectAsState()

    val micPermissionLauncher = rememberLauncherForActivityResult(
        ActivityResultContracts.RequestPermission(),
    ) { granted ->
        if (granted) transcriptionManager.startRecording(context)
    }
    val photoPicker = rememberLauncherForActivityResult(
        ActivityResultContracts.PickVisualMedia(),
    ) { uri ->
        uri?.let {
            attachedImage = readAttachmentFromUri(context, it)
        }
    }
    val filePicker = rememberLauncherForActivityResult(
        ActivityResultContracts.OpenDocument(),
    ) { uri ->
        uri?.let {
            when (val picked = readPickedComposerAttachment(context, it)) {
                is PickedComposerAttachment.Image -> attachedImage = picked.attachment
                is PickedComposerAttachment.File -> {
                    if (picked.attachment !in attachedFiles) {
                        attachedFiles = attachedFiles + picked.attachment
                    }
                }
                null -> Unit
            }
        }
    }

    val hasSendContent = text.isNotBlank() || attachedImage != null || attachedFiles.isNotEmpty()
    val canSend = !isSubmitting && hasSendContent

    // IME visibility is authoritative for "the user is interacting with the
    // composer". `isFocused` alone is unreliable because dismissing the
    // keyboard via system back/down doesn't always clear Compose focus, so
    // we omit it and use `imeVisible` instead. The composer stays active
    // while the user has text or an attachment so unsaved work isn't lost
    // when they briefly dismiss the keyboard to scroll.
    val imeVisible = WindowInsets.isImeVisible
    val isActive = imeVisible ||
        text.isNotBlank() ||
        attachedImage != null ||
        attachedFiles.isNotEmpty() ||
        destinationDrafts.isNotEmpty() ||
        isRecording ||
        isTranscribing
    // Only propagate `false` once the composer has actually become active
    // at least once. Otherwise the very first composition (before focus
    // lands) would emit `false` and the parent would collapse us back to
    // the + button on the next frame.
    var hasBeenActive by remember { mutableStateOf(false) }
    LaunchedEffect(isActive) {
        if (isActive) {
            hasBeenActive = true
            onActiveChange?.invoke(true)
        } else if (hasBeenActive) {
            onActiveChange?.invoke(false)
        }
    }

    // Single send path used by both the inline send button and the expanded
    // dialog. Keep in sync if you change thread startup or payload shape.
    val sendCurrent: () -> Unit = send@{
        val currentProject = project
        if (currentProject == null) {
            errorMessage = "Pick a project before sending."
        } else if (!isSavingRecovery && !isSubmitting && hasSendContent) {
            val payloadText = text.trim()
            val attachmentToSend = attachedImage
            val filesToSend = attachedFiles
            val draftToSend = AppModel.ComposerDraft(text, attachmentToSend, filesToSend)
            val serverIsLocal = appModel.snapshot.value
                ?.servers
                ?.firstOrNull { it.serverId == currentProject.serverId }
                ?.isLocal == true
            val launchSnapshot = appModel.launchState.snapshot.value
            val selectedModel = launchSnapshot.selectedModel.trim().ifEmpty { null }
            val selectedEffort = launchSnapshot.reasoningEffort.trim().ifEmpty { null }
                ?.let(::reasoningEffortFromServerValue)
            val threadStartRequest = appModel.launchState.threadStartRequest(
                currentProject.cwd,
                serverIsLocal = serverIsLocal,
            )
            val payload = AppComposerPayload(
                text = payloadText,
                fileAttachments = filesToSend,
                model = selectedModel,
                reasoningEffort = selectedEffort,
            )
            appModel.setHomeComposerDraft(draftDestination, draftToSend)
            isSavingRecovery = true
            appModel.launchComposerRecovery {
                try {
                    val preparedPayload = withContext(Dispatchers.IO) {
                        payload.copy(additionalInputs = listOfNotNull(attachmentToSend?.toUserInput()))
                    }
                    val submissionId = appModel.beginComposerSubmission(
                        destination = draftDestination,
                        draft = draftToSend,
                        payload = preparedPayload,
                        threadStartRequest = threadStartRequest,
                    ) ?: return@launchComposerRecovery
                    if (AppModel.ComposerDraft(textFieldValue.text, attachedImage, attachedFiles) == draftToSend &&
                        appModel.replaceComposerDraftIfUnchanged(draftDestination, draftToSend, AppModel.ComposerDraft.EMPTY)) {
                        textFieldValue = TextFieldValue("")
                        attachedImage = null
                        attachedFiles = emptyList()
                    }
                    errorMessage = null
                    appModel.submitComposerDraft(submissionId) {
                        try {
                            val threadKey = appModel.startThread(
                                currentProject.serverId,
                                threadStartRequest,
                            )
                            appModel.recoverableComposerDrafts.threadCreated(submissionId, threadKey)
                            com.remora.android.ui.RecentDirectoryStore(context)
                                .record(currentProject.serverId, currentProject.cwd)
                            appModel.startTurn(threadKey, preparedPayload)
                            appModel.recoverableComposerDrafts.complete(submissionId)
                            appModel.refreshThreadSnapshot(threadKey)
                            withContext(Dispatchers.Main) { onThreadCreated(threadKey) }
                        } catch (e: LocalAccountLoginRequiredException) {
                            withContext(Dispatchers.Main) { onLoginRequired(e.serverId) }
                            throw e
                        }
                    }
                } finally {
                    isSavingRecovery = false
                }
            }
        }
    }

    Column(modifier = Modifier.fillMaxWidth()) {
        RecoverableDraftsRow(
            drafts = destinationDrafts,
            storageError = recoveryStorageError,
            onDiscard = { appModel.discardComposerDraft(it) },
            onRestore = { id ->
                val current = AppModel.ComposerDraft(textFieldValue.text, attachedImage, attachedFiles)
                appModel.setHomeComposerDraft(draftDestination, current)
                appModel.launchComposerRecovery {
                    appModel.restoreComposerDraft(id, current)?.let { draft ->
                        if (AppModel.ComposerDraft(textFieldValue.text, attachedImage, attachedFiles) == current &&
                            appModel.replaceComposerDraftIfUnchanged(draftDestination, current, draft)) {
                            textFieldValue = TextFieldValue(draft.text, selection = TextRange(draft.text.length))
                            attachedImage = draft.attachment
                            attachedFiles = draft.fileAttachments
                        }
                    }
                }
            },
        )
        if (errorMessage != null) {
            Row(
                modifier = Modifier
                    .fillMaxWidth()
                    .padding(horizontal = 14.dp, vertical = 6.dp),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                Text(
                    text = errorMessage ?: "",
                    color = RemoraTheme.warning,
                    fontSize = RemoraTextStyle.caption.scaled,
                    modifier = Modifier.weight(1f),
                )
                IconButton(onClick = { errorMessage = null }) {
                    Icon(
                        imageVector = Icons.Default.Close,
                        contentDescription = "Dismiss",
                        tint = RemoraTheme.textMuted,
                        modifier = Modifier.size(14.dp),
                    )
                }
            }
        }

        if (attachedImage != null) {
            val bytes = attachedImage?.data
            val bitmap = remember(bytes) {
                bytes?.let { BitmapFactory.decodeByteArray(it, 0, it.size) }
            }
            Row(
                modifier = Modifier
                    .fillMaxWidth()
                    .padding(start = 16.dp, end = 16.dp, top = 8.dp),
            ) {
                Box {
                    bitmap?.let { bmp ->
                        androidx.compose.foundation.Image(
                            bitmap = bmp.asImageBitmap(),
                            contentDescription = "Attached image",
                            modifier = Modifier
                                .size(60.dp)
                                .clip(RoundedCornerShape(8.dp)),
                        )
                    }
                    IconButton(
                        onClick = { attachedImage = null },
                        modifier = Modifier
                            .align(Alignment.TopEnd)
                            .size(RemoraTheme.minimumTouchTarget),
                    ) {
                        Box(
                            modifier = Modifier
                                .size(22.dp)
                                .background(Color.Black.copy(alpha = 0.6f), CircleShape),
                            contentAlignment = Alignment.Center,
                        ) {
                            Icon(
                                imageVector = Icons.Default.Close,
                                contentDescription = "Remove attachment",
                                tint = Color.White,
                                modifier = Modifier.size(14.dp),
                            )
                        }
                    }
                }
                Spacer(Modifier.weight(1f))
            }
        }

        if (attachedFiles.isNotEmpty()) {
            Column(
                modifier = Modifier
                    .fillMaxWidth()
                    .padding(start = 16.dp, end = 16.dp, top = 8.dp),
                verticalArrangement = Arrangement.spacedBy(6.dp),
            ) {
                attachedFiles.forEach { file ->
                    HomeFileAttachmentRow(
                        attachment = file,
                        onRemove = {
                            attachedFiles = attachedFiles.filterNot { it == file }
                        },
                    )
                }
            }
        }

        Row(
            modifier = Modifier
                .fillMaxWidth()
                .padding(horizontal = 12.dp, vertical = 8.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            if (!isRecording && !isTranscribing && !isSubmitting) {
                IconButton(
                    onClick = { showAttachMenu = true },
                    modifier = Modifier.size(RemoraTheme.minimumTouchTarget),
                ) {
                    Icon(
                        imageVector = Icons.Default.Add,
                        contentDescription = "Attach",
                        tint = RemoraTheme.textPrimary,
                    )
                }
            }

            ComposerTextInputChrome(
                value = textFieldValue,
                onValueChange = { textFieldValue = it },
                showExpand = (text.contains('\n') || text.length > 60) &&
                    !isRecording && !isTranscribing,
                onExpand = { showExpanded = true },
                modifier = Modifier.weight(1f),
                textFieldModifier = Modifier
                    .focusRequester(focusRequester)
                    .onFocusChanged { isFocused = it.isFocused },
            ) {
                when {
                    isRecording -> {
                        Spacer(Modifier.width(8.dp))
                        IconButton(
                            onClick = {
                                val currentProject = project ?: run {
                                    transcriptionManager.cancelRecording()
                                    return@IconButton
                                }
                                scope.launch {
                                    val auth = runCatching {
                                        appModel.client.authStatus(
                                            currentProject.serverId,
                                            AuthStatusRequest(
                                                includeToken = true,
                                                refreshToken = false,
                                            ),
                                        )
                                    }.getOrNull()
                                    val transcript = transcriptionManager.stopAndTranscribe(
                                        authMethod = auth?.authMethod,
                                        authToken = auth?.authToken,
                                    )
                                    transcript?.let {
                                        textFieldValue = insertHomeComposerTranscript(textFieldValue, it)
                                    }
                                }
                            },
                            modifier = Modifier.size(RemoraTheme.minimumTouchTarget),
                        ) {
                            Icon(
                                imageVector = Icons.Default.Stop,
                                contentDescription = "Stop recording",
                                tint = RemoraTheme.accentStrong,
                            )
                        }
                    }

                    isTranscribing || isSubmitting -> {
                        Spacer(Modifier.width(8.dp))
                        CircularProgressIndicator(
                            strokeWidth = 2.dp,
                            color = RemoraTheme.accent,
                            modifier = Modifier.size(18.dp),
                        )
                    }

                    else -> {
                        Spacer(Modifier.width(8.dp))
                        IconButton(
                            onClick = {
                                micPermissionLauncher.launch(android.Manifest.permission.RECORD_AUDIO)
                            },
                            modifier = Modifier.size(RemoraTheme.minimumTouchTarget),
                        ) {
                            Icon(
                                imageVector = Icons.Default.Mic,
                                contentDescription = "Record",
                                tint = RemoraTheme.textSecondary,
                                modifier = Modifier.size(18.dp),
                            )
                        }
                    }
                }
            }

            if (hasSendContent) {
                Spacer(Modifier.width(8.dp))
                IconButton(
                    onClick = sendCurrent,
                    enabled = canSend && !isRecording && !isTranscribing,
                    modifier = Modifier
                        .size(RemoraTheme.minimumTouchTarget)
                        .clip(CircleShape)
                        .background(
                            if (canSend && !isRecording && !isTranscribing) {
                                RemoraTheme.accent
                            } else {
                                RemoraTheme.accent.copy(alpha = 0.45f)
                            },
                            CircleShape,
                        ),
                ) {
                    Icon(
                        imageVector = Icons.AutoMirrored.Filled.Send,
                        contentDescription = "Send",
                        tint = Color.Black,
                        modifier = Modifier.size(17.dp),
                    )
                }
            }
        }

        if (showExpanded) {
            com.remora.android.ui.conversation.ComposerExpandedDialog(
                text = text,
                onTextChange = {
                    textFieldValue = TextFieldValue(
                        text = it,
                        selection = TextRange(it.length),
                    )
                },
                onSend = sendCurrent,
                onDismiss = {
                    showExpanded = false
                    scope.launch {
                        kotlinx.coroutines.delay(80)
                        runCatching { focusRequester.requestFocus() }
                    }
                },
                canSend = hasSendContent,
            )
        }

        if (showAttachMenu) {
            ModalBottomSheet(
                onDismissRequest = { showAttachMenu = false },
                sheetState = rememberModalBottomSheetState(skipPartiallyExpanded = true),
                containerColor = RemoraTheme.background,
            ) {
                Column(
                    modifier = Modifier
                        .fillMaxWidth()
                        .padding(horizontal = 16.dp, vertical = 12.dp),
                    verticalArrangement = Arrangement.spacedBy(12.dp),
                ) {
                    Text(
                        text = "Attach",
                        color = RemoraTheme.textPrimary,
                        fontSize = 18.sp,
                        fontWeight = FontWeight.SemiBold,
                    )

                    HomeAttachmentActionRow(
                        title = "Photo Library",
                        onClick = {
                            showAttachMenu = false
                            photoPicker.launch(
                                PickVisualMediaRequest(ActivityResultContracts.PickVisualMedia.ImageOnly),
                            )
                        },
                    )

                    HomeAttachmentActionRow(
                        title = "Choose File",
                        onClick = {
                            showAttachMenu = false
                            filePicker.launch(ALL_FILE_MIME_TYPES)
                        },
                    )
                }
            }
        }
    }
}

@Composable
private fun HomeAttachmentActionRow(
    title: String,
    onClick: () -> Unit,
) {
    Row(
        modifier = Modifier
            .fillMaxWidth()
            .background(RemoraTheme.surface, RoundedCornerShape(18.dp))
            .clickable(onClick = onClick)
            .padding(horizontal = 16.dp, vertical = 14.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Text(
            text = title,
            color = RemoraTheme.textPrimary,
            fontSize = RemoraTextStyle.body.scaled,
            fontWeight = FontWeight.Medium,
            modifier = Modifier.fillMaxWidth(),
        )
    }
}

@Composable
private fun HomeFileAttachmentRow(
    attachment: ComposerFileAttachment,
    onRemove: () -> Unit,
) {
    Row(
        modifier = Modifier
            .fillMaxWidth()
            .background(RemoraTheme.codeBackground.copy(alpha = 0.72f), RoundedCornerShape(10.dp))
            .padding(horizontal = 10.dp, vertical = 8.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Text(
            text = "FILE",
            color = RemoraTheme.accent,
            fontSize = RemoraTextStyle.caption2.scaled,
            fontWeight = FontWeight.SemiBold,
        )
        Spacer(Modifier.width(8.dp))
        Column(modifier = Modifier.weight(1f)) {
            Text(
                text = attachment.label,
                color = RemoraTheme.textPrimary,
                fontSize = RemoraTextStyle.caption.scaled,
                fontWeight = FontWeight.SemiBold,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
            )
            Text(
                text = attachment.path,
                color = RemoraTheme.textMuted,
                fontSize = RemoraTextStyle.caption2.scaled,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
            )
        }
        IconButton(
            onClick = onRemove,
            modifier = Modifier.size(RemoraTheme.minimumTouchTarget),
        ) {
            Icon(
                imageVector = Icons.Default.Close,
                contentDescription = "Remove file",
                tint = RemoraTheme.textMuted,
                modifier = Modifier.size(14.dp),
            )
        }
    }
}

private fun insertHomeComposerTranscript(current: TextFieldValue, transcript: String): TextFieldValue {
    val insertion = transcript.trim()
    if (insertion.isEmpty()) return current

    val text = current.text
    val start = current.selection.min.coerceIn(0, text.length)
    val end = current.selection.max.coerceIn(0, text.length)
    val replacement = homeComposerInsertionText(insertion, text, start, end)
    val updated = text.replaceRange(start, end, replacement)
    val cursor = start + replacement.length
    return TextFieldValue(
        text = updated,
        selection = TextRange(cursor),
    )
}

private fun homeComposerInsertionText(insertion: String, text: String, start: Int, end: Int): String {
    var replacement = insertion
    if (start > 0 && !text[start - 1].isWhitespace()) {
        replacement = " $replacement"
    }
    if (end < text.length && !text[end].isWhitespace()) {
        replacement += " "
    }
    return replacement
}

private fun reasoningEffortFromServerValue(value: String): ReasoningEffort? =
    when (value.trim().lowercase()) {
        "none" -> ReasoningEffort.NONE
        "minimal" -> ReasoningEffort.MINIMAL
        "low" -> ReasoningEffort.LOW
        "medium" -> ReasoningEffort.MEDIUM
        "high" -> ReasoningEffort.HIGH
        "xhigh" -> ReasoningEffort.X_HIGH
        "max" -> ReasoningEffort.MAX
        else -> null
    }

private sealed interface PickedComposerAttachment {
    data class Image(val attachment: ComposerImageAttachment) : PickedComposerAttachment
    data class File(val attachment: ComposerFileAttachment) : PickedComposerAttachment
}

private fun readPickedComposerAttachment(
    context: Context,
    uri: Uri,
): PickedComposerAttachment? {
    val resolver = context.contentResolver
    val displayName = resolver.displayName(uri) ?: uri.lastPathSegment ?: "selected-file"
    val mimeType = resolver.getType(uri).orEmpty()
    if (isSupportedImageFile(displayName, mimeType)) {
        readAttachmentFromUri(context, uri)?.let { return PickedComposerAttachment.Image(it) }
    }
    return PickedComposerAttachment.File(
        ComposerFileAttachment(
            label = displayName.substringBeforeLast('.', displayName),
            path = uri.toString(),
        ),
    )
}

private fun android.content.ContentResolver.displayName(uri: Uri): String? =
    query(uri, arrayOf(OpenableColumns.DISPLAY_NAME), null, null, null)?.use { cursor ->
        if (!cursor.moveToFirst()) return@use null
        val index = cursor.getColumnIndex(OpenableColumns.DISPLAY_NAME)
        if (index < 0) null else cursor.getString(index)
    }

private fun isSupportedImageFile(displayName: String, mimeType: String): Boolean {
    val normalizedMimeType = mimeType.lowercase()
    if (SUPPORTED_IMAGE_FILE_MIME_TYPES.any { it == normalizedMimeType }) {
        return true
    }
    val extension = displayName.substringAfterLast('.', missingDelimiterValue = "").lowercase()
    return extension in setOf("png", "jpg", "jpeg", "gif", "webp")
}

private fun readAttachmentFromUri(context: Context, uri: Uri): ComposerImageAttachment? {
    val resolver = context.contentResolver
    val bytes = resolver.openInputStream(uri)?.use { it.readBytes() } ?: return null
    val mimeType = resolver.getType(uri).orEmpty()
    return prepareImageAttachment(bytes, mimeType)
}

private fun prepareBitmapAttachment(bitmap: Bitmap): ComposerImageAttachment? {
    val output = ByteArrayOutputStream()
    val format = if (bitmap.hasAlpha()) Bitmap.CompressFormat.PNG else Bitmap.CompressFormat.JPEG
    val mimeType = if (bitmap.hasAlpha()) "image/png" else "image/jpeg"
    val quality = if (bitmap.hasAlpha()) 100 else 85
    if (!bitmap.compress(format, quality, output)) return null
    return ComposerImageAttachment(output.toByteArray(), mimeType)
}

private fun prepareImageAttachment(bytes: ByteArray, mimeTypeHint: String): ComposerImageAttachment? {
    val bitmap = BitmapFactory.decodeByteArray(bytes, 0, bytes.size) ?: return null
    val inferredMime = mimeTypeHint.lowercase()
    if (inferredMime == "image/png" && bitmap.hasAlpha()) {
        return ComposerImageAttachment(bytes, "image/png")
    }
    return prepareBitmapAttachment(bitmap)
}
