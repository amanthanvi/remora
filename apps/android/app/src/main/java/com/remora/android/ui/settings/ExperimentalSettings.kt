package com.remora.android.ui.settings

import androidx.compose.foundation.background
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
import androidx.compose.runtime.Composable
import androidx.compose.runtime.remember
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import com.remora.android.ui.ExperimentalFeatures
import com.remora.android.ui.RemoraFeature
import com.remora.android.ui.RemoraTheme

// Experimental Sub-Screen (matches iOS ExperimentalFeaturesView)
// ═══════════════════════════════════════════════════════════════════════════════

@Composable
internal fun ExperimentalScreen(onBack: () -> Unit) {
    val context = LocalContext.current
    val features = remember { RemoraFeature.entries }

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
            Text("Experimental", color = RemoraTheme.textPrimary, fontSize = 17.sp, fontWeight = FontWeight.SemiBold)
            Spacer(Modifier.weight(1f))
            Spacer(Modifier.width(48.dp))
        }

        Spacer(Modifier.height(16.dp))

        SectionHeader("Features")
        Column(
            Modifier.fillMaxWidth().background(RemoraTheme.surface.copy(alpha = 0.6f), RoundedCornerShape(10.dp)),
        ) {
            features.forEachIndexed { idx, feature ->
                val enabled = ExperimentalFeatures.isEnabled(feature)
                Row(
                    verticalAlignment = Alignment.CenterVertically,
                    modifier = Modifier.fillMaxWidth().padding(horizontal = 12.dp, vertical = 10.dp),
                ) {
                    Column(Modifier.weight(1f)) {
                        Text(feature.displayName, color = RemoraTheme.textPrimary, fontSize = 14.sp)
                        Text(feature.description, color = RemoraTheme.textSecondary, fontSize = 11.sp)
                    }
                    Switch(
                        checked = enabled,
                        onCheckedChange = { ExperimentalFeatures.setEnabled(context, feature, it) },
                        colors = SwitchDefaults.colors(checkedTrackColor = RemoraTheme.accentStrong),
                    )
                }
                if (idx < features.lastIndex) HorizontalDivider(color = RemoraTheme.divider)
            }
        }
        Spacer(Modifier.height(8.dp))
        Text("Experimental features may be unstable or change without notice.", color = RemoraTheme.textMuted, fontSize = 11.sp, modifier = Modifier.padding(start = 4.dp))
    }
}
