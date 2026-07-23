package com.remora.android.ui.common

import android.os.Build
import androidx.compose.foundation.layout.padding
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import androidx.compose.material3.Text
import androidx.compose.material3.MaterialTheme
import com.remora.android.ui.RemoraTheme
import com.remora.android.BuildConfig

object BuildInfo {
    /// True only for release builds that came from a Play Store install.
    /// Debug builds and sideloads (adb installs, ADB shell, etc.) all return
    /// false. Note: Play's open/closed-test tracks also report
    /// `com.android.vending` as the installer, so this hides the label for
    /// alpha/beta testers too — accept this trade-off until we add a
    /// `BuildConfig` flag flipped per-track.
    fun isPlayProductionInstall(context: android.content.Context): Boolean {
        if (BuildConfig.DEBUG) return false
        return playInstallerPackage(context.applicationContext) == "com.android.vending"
    }

    val marketingVersion: String = BuildConfig.VERSION_NAME

    val buildNumber: Int = BuildConfig.VERSION_CODE

    /// "1.5.0 · 53306" — last 5 digits of versionCode with leading zeros
    /// stripped.
    val shortLabel: String
        get() {
            val padded = buildNumber.toString()
            val suffix = padded.takeLast(5).trimStart('0').ifEmpty { "0" }
            return "$marketingVersion · $suffix"
        }

    private fun playInstallerPackage(ctx: android.content.Context): String? {
        return try {
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) {
                ctx.packageManager.getInstallSourceInfo(ctx.packageName).installingPackageName
            } else {
                @Suppress("DEPRECATION")
                ctx.packageManager.getInstallerPackageName(ctx.packageName)
            }
        } catch (_: Throwable) {
            null
        }
    }

}

@Composable
fun DebugBuildLabel(modifier: Modifier = Modifier) {
    val context = LocalContext.current
    if (BuildInfo.isPlayProductionInstall(context)) return
    Text(
        text = BuildInfo.shortLabel,
        style = MaterialTheme.typography.labelSmall,
        color = RemoraTheme.textMuted,
        textAlign = TextAlign.End,
        modifier = modifier.padding(horizontal = 14.dp, vertical = 2.dp),
    )
}
