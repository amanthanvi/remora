package com.remora.android.ui.settings

import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Check
import androidx.compose.material.icons.filled.ChevronRight
import androidx.compose.material3.Icon
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import com.remora.android.ui.RemoraTheme

@Composable
internal fun SectionHeader(text: String) {
    Spacer(Modifier.height(8.dp))
    Text(text.uppercase(), color = RemoraTheme.textSecondary, fontSize = 11.sp, fontWeight = FontWeight.Medium, modifier = Modifier.padding(start = 4.dp, bottom = 4.dp))
}

@Composable
internal fun SettingsRow(
    label: String, subtitle: String? = null,
    icon: (@Composable () -> Unit)? = null,
    trailing: (@Composable () -> Unit)? = null,
    onClick: (() -> Unit)? = null,
) {
    Row(
        verticalAlignment = Alignment.CenterVertically,
        modifier = Modifier.fillMaxWidth()
            .background(RemoraTheme.surface.copy(alpha = 0.6f), RoundedCornerShape(10.dp))
            .then(if (onClick != null) Modifier.clickable(onClick = onClick) else Modifier)
            .padding(12.dp),
    ) {
        icon?.invoke()
        if (icon != null) Spacer(Modifier.width(10.dp))
        Column(Modifier.weight(1f)) {
            Text(label, color = RemoraTheme.textPrimary, fontSize = 14.sp)
            subtitle?.let { Text(it, color = RemoraTheme.textSecondary, fontSize = 11.sp) }
        }
        trailing?.invoke()
    }
}

@Composable
internal fun NavRow(icon: androidx.compose.ui.graphics.vector.ImageVector, label: String, onClick: () -> Unit) {
    SettingsRow(
        icon = { Icon(icon, null, tint = RemoraTheme.accent, modifier = Modifier.size(20.dp)) },
        label = label,
        trailing = { Icon(Icons.Default.ChevronRight, null, tint = RemoraTheme.textMuted, modifier = Modifier.size(16.dp)) },
        onClick = onClick,
    )
}

@Composable
internal fun FontRow(name: String, fontFamily: FontFamily, isSelected: Boolean, onClick: () -> Unit) {
    Row(
        verticalAlignment = Alignment.CenterVertically,
        modifier = Modifier.fillMaxWidth().clickable(onClick = onClick).padding(12.dp),
    ) {
        Column(Modifier.weight(1f)) {
            Text(name, color = RemoraTheme.textPrimary, fontSize = 14.sp)
            Text("The quick brown fox", color = RemoraTheme.textSecondary, fontSize = 13.sp, fontFamily = fontFamily)
        }
        if (isSelected) Icon(Icons.Default.Check, null, tint = RemoraTheme.accent, modifier = Modifier.size(18.dp))
    }
}
