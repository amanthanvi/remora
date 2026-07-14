package com.remora.android.ui.conversation

import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import com.remora.android.ui.RemoraTextStyle
import com.remora.android.ui.RemoraTheme
import com.remora.android.ui.scaled
import uniffi.codex_mobile_client.AppCollaborationModePreset
import uniffi.codex_mobile_client.AppModeKind
import uniffi.codex_mobile_client.ReasoningEffort

internal fun fallbackCollaborationModePresets(): List<AppCollaborationModePreset> =
    listOf(
        AppCollaborationModePreset(
            kind = AppModeKind.DEFAULT,
            name = "Default",
            model = null,
            reasoningEffort = null,
        ),
        AppCollaborationModePreset(
            kind = AppModeKind.PLAN,
            name = "Plan",
            model = null,
            reasoningEffort = ReasoningEffort.MEDIUM,
        ),
    )

@Composable
internal fun CollaborationModeSheet(
    presets: List<AppCollaborationModePreset>,
    selectedMode: AppModeKind,
    isLoading: Boolean,
    onDismiss: () -> Unit,
    onSelect: (AppModeKind) -> Unit,
) {
    Column(
        modifier = Modifier
            .fillMaxWidth()
            .padding(horizontal = 16.dp, vertical = 12.dp),
        verticalArrangement = Arrangement.spacedBy(12.dp),
    ) {
        Row(
            modifier = Modifier.fillMaxWidth(),
            horizontalArrangement = Arrangement.SpaceBetween,
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Text(
                text = "Collaboration Mode",
                color = RemoraTheme.textPrimary,
                fontSize = 18f.scaled,
                fontWeight = FontWeight.SemiBold,
            )
            TextButton(onClick = onDismiss) {
                Text("Done")
            }
        }

        if (isLoading && presets.isEmpty()) {
            CircularProgressIndicator(color = RemoraTheme.accent)
        }

        presets.forEach { preset ->
            Row(
                modifier = Modifier
                    .fillMaxWidth()
                    .background(RemoraTheme.surface, RoundedCornerShape(16.dp))
                    .clickable { onSelect(preset.kind) }
                    .padding(horizontal = 14.dp, vertical = 12.dp),
                horizontalArrangement = Arrangement.SpaceBetween,
                verticalAlignment = Alignment.CenterVertically,
            ) {
                Column(verticalArrangement = Arrangement.spacedBy(4.dp)) {
                    Text(
                        text = preset.name,
                        color = RemoraTheme.textPrimary,
                        fontSize = RemoraTextStyle.body.scaled,
                        fontWeight = FontWeight.SemiBold,
                    )
                    preset.reasoningEffort?.let { effort ->
                        Text(
                            text = collaborationModeEffortLabel(effort),
                            color = RemoraTheme.textSecondary,
                            fontSize = RemoraTextStyle.caption2.scaled,
                        )
                    }
                }
                if (preset.kind == selectedMode) {
                    Text(
                        text = "Selected",
                        color = RemoraTheme.accent,
                        fontSize = RemoraTextStyle.caption2.scaled,
                        fontWeight = FontWeight.SemiBold,
                    )
                }
            }
        }
    }
}

private fun collaborationModeEffortLabel(effort: ReasoningEffort): String =
    when (effort) {
        ReasoningEffort.NONE -> "None"
        ReasoningEffort.MINIMAL -> "Minimal"
        ReasoningEffort.LOW -> "Low"
        ReasoningEffort.MEDIUM -> "Medium"
        ReasoningEffort.HIGH -> "High"
        ReasoningEffort.X_HIGH -> "XHigh"
        ReasoningEffort.MAX -> "Max"
    }
