package com.remora.android.ui.discovery

import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.KeyboardArrowRight
import androidx.compose.material3.Icon
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import com.remora.android.ui.RemoraTheme
import com.remora.android.ui.common.AgentIconView
import com.remora.android.ui.common.AgentRuntimeKind

/**
 * Canonical agent list shown on the remote pairing chooser card. Mirrors
 * the splash carousel order so cold-start presentation stays consistent.
 * New agents added in the remote host manifest still surface on connected
 * hosts via the real metadata store; this list only seeds the
 * pre-pair preview.
 */
internal val RemotePairingAgents: List<AgentRuntimeKind> = listOf(
    "codex",
    "pi",
    "amp",
    "opencode",
    "claude",
    "droid",
    "hermes",
    "devin",
    "grok",
)

internal val CodexOnlyAgents: List<AgentRuntimeKind> = listOf("codex")

@Composable
internal fun ChooserCard(
    title: String,
    subtitle: String,
    badge: String?,
    icon: ImageVector,
    supportedAgents: List<AgentRuntimeKind>,
    isRecommended: Boolean,
    onClick: () -> Unit,
) {
    val borderColor = if (isRecommended) {
        RemoraTheme.accent.copy(alpha = 0.45f)
    } else {
        RemoraTheme.accent.copy(alpha = 0.18f)
    }
    val backgroundColor = if (isRecommended) {
        RemoraTheme.surface.copy(alpha = 0.85f)
    } else {
        RemoraTheme.surface.copy(alpha = 0.6f)
    }
    val iconBubble = RemoraTheme.accent.copy(alpha = if (isRecommended) 0.16f else 0.10f)

    Column(
        modifier = Modifier
            .fillMaxWidth()
            .background(backgroundColor, RoundedCornerShape(14.dp))
            .border(
                width = if (isRecommended) 1.dp else 0.8.dp,
                color = borderColor,
                shape = RoundedCornerShape(14.dp),
            )
            .clickable(onClick = onClick)
            .padding(horizontal = 16.dp, vertical = 14.dp),
        verticalArrangement = Arrangement.spacedBy(12.dp),
    ) {
        Row(
            verticalAlignment = Alignment.Top,
            horizontalArrangement = Arrangement.spacedBy(14.dp),
            modifier = Modifier.fillMaxWidth(),
        ) {
            Box(
                modifier = Modifier
                    .padding(top = 2.dp)
                    .size(36.dp)
                    .background(iconBubble, RoundedCornerShape(50)),
                contentAlignment = Alignment.Center,
            ) {
                Icon(
                    imageVector = icon,
                    contentDescription = null,
                    tint = RemoraTheme.accent,
                    modifier = Modifier.size(20.dp),
                )
            }
            Column(
                modifier = Modifier.weight(1f),
                verticalArrangement = Arrangement.spacedBy(4.dp),
            ) {
                Row(
                    verticalAlignment = Alignment.CenterVertically,
                    horizontalArrangement = Arrangement.spacedBy(8.dp),
                ) {
                    Text(
                        text = title,
                        color = RemoraTheme.textPrimary,
                        fontSize = 15.sp,
                        fontWeight = FontWeight.SemiBold,
                    )
                    if (badge != null) {
                        Box(
                            modifier = Modifier
                                .background(
                                    RemoraTheme.accent.copy(alpha = 0.14f),
                                    RoundedCornerShape(50),
                                )
                                .border(
                                    width = 0.6.dp,
                                    color = RemoraTheme.accent.copy(alpha = 0.45f),
                                    shape = RoundedCornerShape(50),
                                )
                                .padding(horizontal = 6.dp, vertical = 2.dp),
                        ) {
                            Text(
                                text = badge,
                                color = RemoraTheme.accent,
                                fontSize = 10.sp,
                                fontWeight = FontWeight.SemiBold,
                                letterSpacing = 0.5.sp,
                            )
                        }
                    }
                }
                Text(
                    text = subtitle,
                    color = RemoraTheme.textSecondary,
                    fontSize = 12.sp,
                    modifier = Modifier.padding(top = 2.dp),
                )
            }
            Icon(
                imageVector = Icons.AutoMirrored.Filled.KeyboardArrowRight,
                contentDescription = null,
                tint = RemoraTheme.textMuted,
                modifier = Modifier.padding(top = 10.dp),
            )
        }

        if (supportedAgents.isNotEmpty()) {
            SupportedAgentsStrip(supportedAgents)
        }
    }
}

@Composable
private fun SupportedAgentsStrip(agents: List<AgentRuntimeKind>) {
    Row(
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(8.dp),
        modifier = Modifier.fillMaxWidth(),
    ) {
        Text(
            text = "Works with",
            color = RemoraTheme.textMuted,
            fontSize = 10.sp,
            letterSpacing = 0.4.sp,
            maxLines = 1,
        )
        Row(horizontalArrangement = Arrangement.spacedBy(5.dp)) {
            agents.forEach { agent ->
                AgentIconView(
                    kind = agent,
                    sizeDp = 18,
                )
            }
        }
    }
}
