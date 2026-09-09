package com.remora.android.ui.conversation

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Restore
import androidx.compose.material.icons.filled.DeleteOutline
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import com.remora.android.state.ComposerDraftRecoveryStatus
import com.remora.android.state.RecoverableComposerDraft
import com.remora.android.ui.RemoraTextStyle
import com.remora.android.ui.RemoraTheme
import com.remora.android.ui.scaled

@Composable
internal fun RecoverableDraftsRow(
    drafts: List<RecoverableComposerDraft>,
    storageError: String? = null,
    onDiscard: ((Long) -> Unit)? = null,
    onRestore: (Long) -> Unit,
) {
    if (storageError != null) {
        Text(
            text = storageError,
            color = RemoraTheme.danger,
            fontSize = RemoraTextStyle.caption.scaled,
            modifier = Modifier.fillMaxWidth().padding(horizontal = 12.dp, vertical = 8.dp),
        )
    }
    val recoverable = drafts.filter { it.status != ComposerDraftRecoveryStatus.SUBMITTING }
    if (recoverable.isEmpty()) return
    var expanded by remember { mutableStateOf(false) }
    var discardId by remember { mutableStateOf<Long?>(null) }
    if (discardId != null) {
        AlertDialog(
            onDismissRequest = { discardId = null },
            title = { Text("Discard saved draft?") },
            text = { Text("This deletes the local recovery copy. It does not cancel a remote submission.") },
            confirmButton = {
                TextButton(onClick = {
                    discardId?.let { onDiscard?.invoke(it) }
                    discardId = null
                }) { Text("Discard") }
            },
            dismissButton = { TextButton(onClick = { discardId = null }) { Text("Cancel") } },
        )
    }
    Row(
        modifier = Modifier.fillMaxWidth().padding(horizontal = 12.dp),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.SpaceBetween,
    ) {
        Text(
            text = if (recoverable.any { it.status == ComposerDraftRecoveryStatus.UNCONFIRMED }) {
                "Submission not confirmed"
            } else {
                "Saved drafts"
            },
            color = RemoraTheme.warning,
            fontSize = RemoraTextStyle.caption.scaled,
            modifier = Modifier.weight(1f),
        )
        Box {
            IconButton(onClick = { expanded = true }) {
                Icon(Icons.Default.Restore, contentDescription = "Saved drafts", tint = RemoraTheme.accent)
            }
            DropdownMenu(expanded = expanded, onDismissRequest = { expanded = false }) {
                for (draft in recoverable) {
                    DropdownMenuItem(
                        trailingIcon = if (onDiscard == null) null else {
                            {
                                IconButton(onClick = {
                                    expanded = false
                                    discardId = draft.id
                                }) {
                                    Icon(Icons.Default.DeleteOutline, "Discard saved draft", tint = RemoraTheme.danger)
                                }
                            }
                        },
                        text = {
                            Text(
                                text = draft.draft.text.ifBlank { "Attachments" },
                                maxLines = 2,
                                overflow = TextOverflow.Ellipsis,
                            )
                        },
                        onClick = {
                            expanded = false
                            onRestore(draft.id)
                        },
                    )
                }
            }
        }
    }
}
