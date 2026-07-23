package com.remora.android.ui.voice

import androidx.compose.foundation.Canvas
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.BoxWithConstraints
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.VolumeUp
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.geometry.Rect
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.Path
import androidx.compose.ui.graphics.drawscope.Fill
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import com.remora.android.ui.RemoraTheme
import uniffi.codex_mobile_client.AppVoiceSessionPhase
import kotlin.math.abs
import kotlin.math.max

@Composable
fun InlineVoiceStatusStrip(
    phase: AppVoiceSessionPhase,
    inputLevel: Float,
    outputLevel: Float,
    onToggleSpeaker: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val isListening = phase == AppVoiceSessionPhase.LISTENING
    val isSpeaking = phase == AppVoiceSessionPhase.SPEAKING

    val scaledInputLevel = if (isListening) max(0.08f, inputLevel) else max(0f, inputLevel)
    val scaledOutputLevel = if (isSpeaking) max(0.08f, outputLevel) else max(0f, outputLevel)

    BoxWithConstraints(
        modifier = modifier
            .fillMaxWidth()
            .background(RemoraTheme.surface.copy(alpha = 0.6f)),
    ) {
        val showWaveforms = maxWidth >= 420.dp
        Row(
            modifier = Modifier
                .fillMaxWidth()
                .padding(horizontal = 14.dp, vertical = 6.dp),
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(if (showWaveforms) 8.dp else 6.dp),
        ) {
            // YOU indicator
            Row(
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.spacedBy(5.dp),
            ) {
                Box(
                    modifier = Modifier
                        .size(5.dp)
                        .background(
                            if (isListening) RemoraTheme.accent else RemoraTheme.textMuted.copy(alpha = 0.4f),
                            CircleShape,
                        ),
                )
                Text(
                    text = "YOU",
                    color = if (isListening) RemoraTheme.textPrimary else RemoraTheme.textMuted,
                    fontSize = 10.sp,
                    fontWeight = FontWeight.Bold,
                    fontFamily = RemoraTheme.monoFont,
                )
                if (showWaveforms) {
                    AudioWaveform(
                        level = scaledInputLevel,
                        tint = RemoraTheme.accent,
                        modifier = Modifier.size(width = 48.dp, height = 14.dp),
                    )
                }
            }

            // CODEX indicator
            Row(
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.spacedBy(5.dp),
            ) {
                Box(
                    modifier = Modifier
                        .size(5.dp)
                        .background(
                            if (isSpeaking) RemoraTheme.warning else RemoraTheme.textMuted.copy(alpha = 0.4f),
                            CircleShape,
                        ),
                )
                Text(
                    text = "CODEX",
                    color = if (isSpeaking) RemoraTheme.textPrimary else RemoraTheme.textMuted,
                    fontSize = 10.sp,
                    fontWeight = FontWeight.Bold,
                    fontFamily = RemoraTheme.monoFont,
                )
                if (showWaveforms) {
                    AudioWaveform(
                        level = scaledOutputLevel,
                        tint = RemoraTheme.warning,
                        modifier = Modifier.size(width = 48.dp, height = 14.dp),
                    )
                }
            }

            Spacer(Modifier.weight(1f))

            Text(
                text = phaseLabel(phase),
                color = phaseColor(phase),
                fontSize = 10.sp,
                fontWeight = FontWeight.Medium,
                fontFamily = RemoraTheme.monoFont,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
            )

            IconButton(
                onClick = onToggleSpeaker,
                modifier = Modifier.size(RemoraTheme.minimumTouchTarget),
            ) {
                Icon(
                    Icons.AutoMirrored.Filled.VolumeUp,
                    contentDescription = "Toggle speaker",
                    tint = RemoraTheme.textPrimary,
                    modifier = Modifier.size(16.dp),
                )
            }
        }
    }
}

@Composable
private fun AudioWaveform(
    level: Float,
    tint: Color,
    modifier: Modifier = Modifier,
) {
    val barCount = 12
    Canvas(modifier = modifier) {
        val barWidth = 2f.dp.toPx()
        val totalBarWidth = barWidth * barCount
        val gap = if (barCount > 1) {
            (size.width - totalBarWidth) / (barCount - 1)
        } else {
            0f
        }
        val midY = size.height / 2f
        val center = (barCount - 1) / 2f

        for (index in 0 until barCount) {
            val distance = abs(index - center) / max(center, 1f)
            val base = 1f - distance * 0.5f
            val activeLevel = max(0.15f, level)
            val barHeight = max(0.1f, base * activeLevel) * size.height
            val x = index * (barWidth + gap)
            val cornerRadius = 1f.dp.toPx()

            val rect = Rect(
                left = x,
                top = midY - barHeight / 2f,
                right = x + barWidth,
                bottom = midY + barHeight / 2f,
            )
            drawPath(
                path = Path().apply {
                    addRoundRect(
                        androidx.compose.ui.geometry.RoundRect(
                            rect = rect,
                            radiusX = cornerRadius,
                            radiusY = cornerRadius,
                        )
                    )
                },
                color = tint,
                style = Fill,
            )
        }
    }
}

private fun phaseLabel(phase: AppVoiceSessionPhase): String =
    when (phase) {
        AppVoiceSessionPhase.CONNECTING -> "CONNECTING"
        AppVoiceSessionPhase.LISTENING -> "LISTENING"
        AppVoiceSessionPhase.SPEAKING -> "SPEAKING"
        AppVoiceSessionPhase.THINKING -> "THINKING"
        AppVoiceSessionPhase.HANDOFF -> "HANDOFF"
        AppVoiceSessionPhase.ERROR -> "ERROR"
    }

private fun phaseColor(phase: AppVoiceSessionPhase): Color =
    when (phase) {
        AppVoiceSessionPhase.CONNECTING,
        AppVoiceSessionPhase.THINKING,
        AppVoiceSessionPhase.HANDOFF,
        -> RemoraTheme.warning
        AppVoiceSessionPhase.LISTENING,
        AppVoiceSessionPhase.SPEAKING,
        -> RemoraTheme.accent
        AppVoiceSessionPhase.ERROR -> RemoraTheme.danger
    }
