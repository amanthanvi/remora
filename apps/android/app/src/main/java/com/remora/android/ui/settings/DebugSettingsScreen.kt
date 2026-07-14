package com.remora.android.ui.settings

import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.imePadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.Switch
import androidx.compose.material3.SwitchDefaults
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
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
import com.remora.android.state.DebugSettings
import com.remora.android.state.MessageRecorder
import com.remora.android.ui.LocalAppModel
import com.remora.android.ui.RemoraTheme

// Debug Sub-Screen

@Composable
internal fun DebugScreen(onBack: () -> Unit) {
    val context = LocalContext.current

    Column(
        Modifier
            .fillMaxSize()
            .imePadding()
            .padding(16.dp),
    ) {
        // Nav bar
        Row(Modifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically) {
            IconButton(onClick = onBack) {
                Icon(Icons.AutoMirrored.Filled.ArrowBack, "Back", tint = RemoraTheme.accent)
            }
            Spacer(Modifier.weight(1f))
            Text("Debug", color = RemoraTheme.textPrimary, fontSize = 17.sp, fontWeight = FontWeight.SemiBold)
            Spacer(Modifier.weight(1f))
            Spacer(Modifier.width(48.dp))
        }

        Spacer(Modifier.height(16.dp))

        SectionHeader("Rendering")
        Column(
            Modifier.fillMaxWidth().background(RemoraTheme.surface.copy(alpha = 0.6f), RoundedCornerShape(10.dp)),
        ) {
            Row(
                verticalAlignment = Alignment.CenterVertically,
                modifier = Modifier.fillMaxWidth().padding(horizontal = 12.dp, vertical = 10.dp),
            ) {
                Column(Modifier.weight(1f)) {
                    Text("Disable Markdown", color = RemoraTheme.textPrimary, fontSize = 14.sp)
                    Text("Show raw monospace text instead of rendered markdown", color = RemoraTheme.textSecondary, fontSize = 11.sp)
                }
                Switch(
                    checked = DebugSettings.disableMarkdown,
                    onCheckedChange = { DebugSettings.setDisableMarkdown(context, it) },
                    colors = SwitchDefaults.colors(checkedTrackColor = RemoraTheme.accentStrong),
                )
            }
            HorizontalDivider(color = RemoraTheme.divider)
            Row(
                verticalAlignment = Alignment.CenterVertically,
                modifier = Modifier.fillMaxWidth().padding(horizontal = 12.dp, vertical = 10.dp),
            ) {
                Column(Modifier.weight(1f)) {
                    Text("Show Turn Metrics", color = RemoraTheme.textPrimary, fontSize = 14.sp)
                    Text("Display elapsed time and token count on turn items", color = RemoraTheme.textSecondary, fontSize = 11.sp)
                }
                Switch(
                    checked = DebugSettings.showTurnMetrics,
                    onCheckedChange = { DebugSettings.setShowTurnMetrics(context, it) },
                    colors = SwitchDefaults.colors(checkedTrackColor = RemoraTheme.accentStrong),
                )
            }
        }

        // ── Recording ──
        Spacer(Modifier.height(12.dp))
        SectionHeader("Recording")

        val appModel = LocalAppModel.current
        val scope = rememberCoroutineScope()
        var isRecording by remember { mutableStateOf(MessageRecorder.isRecording(appModel.store)) }
        var recordings by remember { mutableStateOf(MessageRecorder.listRecordings(context)) }

        Column(
            Modifier.fillMaxWidth().background(RemoraTheme.surface.copy(alpha = 0.6f), RoundedCornerShape(10.dp)).padding(12.dp),
            verticalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            Row(
                verticalAlignment = Alignment.CenterVertically,
                modifier = Modifier.fillMaxWidth(),
            ) {
                Column(Modifier.weight(1f)) {
                    Text(
                        if (isRecording) "Recording..." else "Message Recording",
                        color = if (isRecording) RemoraTheme.danger else RemoraTheme.textPrimary,
                        fontSize = 14.sp,
                    )
                    Text("Record server messages for replay", color = RemoraTheme.textSecondary, fontSize = 11.sp)
                }
                TextButton(onClick = {
                    if (isRecording) {
                        MessageRecorder.stopRecording(context, appModel.store)
                        isRecording = false
                        recordings = MessageRecorder.listRecordings(context)
                    } else {
                        MessageRecorder.startRecording(appModel.store)
                        isRecording = true
                    }
                }) {
                    Text(
                        if (isRecording) "Stop" else "Start",
                        color = if (isRecording) RemoraTheme.danger else RemoraTheme.accent,
                    )
                }
            }

            if (recordings.isNotEmpty()) {
                HorizontalDivider(color = RemoraTheme.divider)
                Text("Saved Recordings", color = RemoraTheme.textSecondary, fontSize = 11.sp)
                recordings.forEach { file ->
                    Row(
                        verticalAlignment = Alignment.CenterVertically,
                        modifier = Modifier.fillMaxWidth().padding(vertical = 2.dp),
                    ) {
                        Text(
                            file.name,
                            color = RemoraTheme.textPrimary,
                            fontSize = 12.sp,
                            modifier = Modifier.weight(1f),
                        )
                        val sizeKb = file.length() / 1024
                        Text("${sizeKb}KB", color = RemoraTheme.textMuted, fontSize = 10.sp)
                        Spacer(Modifier.width(8.dp))
                        TextButton(onClick = {
                            MessageRecorder.deleteRecording(file)
                            recordings = MessageRecorder.listRecordings(context)
                        }) {
                            Text("Delete", color = RemoraTheme.danger, fontSize = 11.sp)
                        }
                    }
                }
            }
        }

        Spacer(Modifier.height(8.dp))
        Text("Debug features are for development and testing.", color = RemoraTheme.textMuted, fontSize = 11.sp, modifier = Modifier.padding(start = 4.dp))
    }
}
