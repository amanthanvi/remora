package com.remora.android.ui.settings

import androidx.compose.foundation.Image
import androidx.compose.animation.animateContentSize
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.statusBarsPadding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.filled.ExpandLess
import androidx.compose.material.icons.filled.ExpandMore
import androidx.compose.material3.Button
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.Checkbox
import androidx.compose.material3.CheckboxDefaults
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.Slider
import androidx.compose.material3.SliderDefaults
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableFloatStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.blur
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.graphics.graphicsLayer
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import com.remora.android.ui.RemoraTheme
import com.remora.android.ui.VideoWallpaperPlayer
import com.remora.android.ui.WallpaperConfig
import com.remora.android.ui.WallpaperManager
import com.remora.android.ui.rememberWallpaperMotionTransform
import com.remora.android.ui.WallpaperScope
import com.remora.android.ui.WallpaperType
import com.remora.android.ui.colorFromHex
import com.remora.android.ui.wallpaperBlurRadius
import uniffi.codex_mobile_client.ThreadKey

@Composable
fun WallpaperAdjustScreen(
    threadKey: ThreadKey? = null,
    serverId: String? = null,
    onBack: () -> Unit,
    onApplied: () -> Unit,
) {
    val isServerOnly = threadKey == null
    val resolvedServerId = threadKey?.serverId ?: serverId
    val sourceScope: WallpaperScope? = if (threadKey != null) {
        WallpaperManager.resolvedScope(threadKey)
    } else {
        resolvedServerId?.let { WallpaperManager.resolvedScopeForServer(it) }
    }
    val currentConfig = WallpaperManager.pendingConfig ?: if (threadKey != null) WallpaperManager.resolvedConfig(threadKey)
        else resolvedServerId?.let { WallpaperManager.resolvedConfigForServer(it) }
    var blur by remember(currentConfig) { mutableFloatStateOf(currentConfig?.blur ?: 0f) }
    var brightness by remember(currentConfig) { mutableFloatStateOf(currentConfig?.brightness ?: 1f) }
    var motionEnabled by remember(currentConfig) { mutableStateOf(currentConfig?.motionEnabled ?: false) }
    var sheetMinimized by remember { mutableStateOf(false) }
    val isBlurred = blur > 0.01f

    val previewBitmap = remember(currentConfig) {
        currentConfig?.let { WallpaperManager.previewBitmapForConfig(it, threadKey = threadKey, serverId = resolvedServerId) }
    }

    Box(modifier = Modifier.fillMaxSize().background(RemoraTheme.background)) {
        // Live preview with effects
        val blurRadius = wallpaperBlurRadius(blur)
        val brightnessAlpha = brightness.coerceIn(0f, 1f)
        val motion = rememberWallpaperMotionTransform(motionEnabled)

        val isVideoType = currentConfig?.type == WallpaperType.CUSTOM_VIDEO || currentConfig?.type == WallpaperType.VIDEO_URL
        if (isVideoType) {
            val videoPath = currentConfig?.let {
                WallpaperManager.previewVideoPathForConfig(it, threadKey = threadKey, serverId = resolvedServerId)
            }
            if (videoPath != null) {
                VideoWallpaperPlayer(
                    filePath = videoPath,
                    blurAmount = blur,
                    brightnessAlpha = brightnessAlpha,
                    motionTransform = motion,
                    modifier = Modifier.fillMaxSize(),
                )
            }
        } else if (previewBitmap != null) {
            Image(
                bitmap = previewBitmap.asImageBitmap(),
                contentDescription = null,
                contentScale = ContentScale.Crop,
                modifier = Modifier
                    .fillMaxSize()
                    .blur(blurRadius)
                    .graphicsLayer {
                        alpha = brightnessAlpha
                        scaleX = motion.scale
                        scaleY = motion.scale
                        translationX = motion.translationX
                        translationY = motion.translationY
                    },
            )
        } else if (currentConfig?.type == WallpaperType.SOLID_COLOR) {
            val color = currentConfig.colorHex?.let { colorFromHex(it) } ?: RemoraTheme.background
            Box(
                modifier = Modifier
                    .fillMaxSize()
                    .background(color)
                    .graphicsLayer { alpha = brightnessAlpha },
            )
        }

        // Sample bubbles
        Column(
            modifier = Modifier
                .fillMaxWidth()
                .align(Alignment.TopCenter)
                .padding(start = 32.dp, end = 32.dp, top = 104.dp),
            verticalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            SampleBubble("Can you help me refactor this module?", isUser = true)
            SampleBubble("Sure! I'll analyze the code structure.", isUser = false)
        }

        // Top bar
        Row(
            verticalAlignment = Alignment.CenterVertically,
            modifier = Modifier
                .fillMaxWidth()
                .statusBarsPadding()
                .background(RemoraTheme.surface.copy(alpha = 0.85f))
                .padding(horizontal = 8.dp, vertical = 6.dp),
        ) {
            IconButton(onClick = onBack, modifier = Modifier.size(RemoraTheme.minimumTouchTarget)) {
                Icon(
                    Icons.AutoMirrored.Filled.ArrowBack,
                    contentDescription = "Back",
                    tint = RemoraTheme.textPrimary,
                    modifier = Modifier.size(20.dp),
                )
            }
            Spacer(Modifier.width(8.dp))
            Text(
                text = "Adjust Wallpaper",
                color = RemoraTheme.textPrimary,
                fontSize = 16.sp,
                fontWeight = FontWeight.SemiBold,
            )
        }

        // Bottom controls
        Column(
            modifier = Modifier
                .align(Alignment.BottomCenter)
                .fillMaxWidth()
                .navigationBarsPadding()
                .background(
                    RemoraTheme.surface.copy(alpha = 0.95f),
                    RoundedCornerShape(topStart = 20.dp, topEnd = 20.dp),
                )
                .animateContentSize()
                .padding(16.dp),
        ) {
            Row(
                modifier = Modifier.fillMaxWidth(),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                Box(
                    modifier = Modifier
                        .width(40.dp)
                        .height(4.dp)
                        .background(RemoraTheme.textMuted.copy(alpha = 0.5f), RoundedCornerShape(999.dp)),
                )
                Spacer(Modifier.weight(1f))
                IconButton(
                    onClick = { sheetMinimized = !sheetMinimized },
                    modifier = Modifier.size(RemoraTheme.minimumTouchTarget),
                ) {
                    Icon(
                        imageVector = if (sheetMinimized) Icons.Default.ExpandLess else Icons.Default.ExpandMore,
                        contentDescription = if (sheetMinimized) "Expand controls" else "Minimize controls",
                        tint = RemoraTheme.textPrimary,
                    )
                }
            }

            Spacer(Modifier.height(10.dp))

            Row(
                modifier = Modifier
                    .fillMaxWidth()
                    .clickable { sheetMinimized = !sheetMinimized },
                verticalAlignment = Alignment.CenterVertically,
            ) {
                Column(modifier = Modifier.weight(1f)) {
                    Text(
                        text = "Wallpaper Controls",
                        color = RemoraTheme.textPrimary,
                        fontSize = 14.sp,
                        fontWeight = FontWeight.SemiBold,
                    )
                    Text(
                        text = if (sheetMinimized) {
                            "Tap to expand blur, motion, brightness, and apply controls."
                        } else {
                            "Adjust the current wallpaper, or minimize this sheet to inspect it full-screen."
                        },
                        color = RemoraTheme.textMuted,
                        fontSize = 12.sp,
                    )
                }
                Icon(
                    imageVector = if (sheetMinimized) Icons.Default.ExpandLess else Icons.Default.ExpandMore,
                    contentDescription = if (sheetMinimized) "Expand controls" else "Minimize controls",
                    tint = RemoraTheme.textPrimary,
                )
            }

            if (!sheetMinimized) {
                Spacer(Modifier.height(12.dp))

                Row(
                    modifier = Modifier.fillMaxWidth(),
                    horizontalArrangement = Arrangement.spacedBy(24.dp),
                ) {
                    Row(verticalAlignment = Alignment.CenterVertically) {
                        Checkbox(
                            checked = isBlurred,
                            onCheckedChange = { checked ->
                                blur = if (checked) 0.75f else 0f
                            },
                            colors = CheckboxDefaults.colors(
                                checkedColor = RemoraTheme.accent,
                                uncheckedColor = RemoraTheme.textMuted,
                            ),
                        )
                        Text("Blurred", color = RemoraTheme.textPrimary, fontSize = 13.sp)
                    }
                    Row(verticalAlignment = Alignment.CenterVertically) {
                        Checkbox(
                            checked = motionEnabled,
                            onCheckedChange = { motionEnabled = it },
                            colors = CheckboxDefaults.colors(
                                checkedColor = RemoraTheme.accent,
                                uncheckedColor = RemoraTheme.textMuted,
                            ),
                        )
                        Text("Motion", color = RemoraTheme.textPrimary, fontSize = 13.sp)
                    }
                }

                Spacer(Modifier.height(12.dp))

                Row(
                    verticalAlignment = Alignment.CenterVertically,
                    modifier = Modifier.fillMaxWidth(),
                ) {
                    Text("\u2600", fontSize = 14.sp, color = RemoraTheme.textMuted)
                    Slider(
                        value = brightness,
                        onValueChange = { brightness = it },
                        valueRange = 0.2f..1f,
                        modifier = Modifier.weight(1f).padding(horizontal = 8.dp),
                        colors = SliderDefaults.colors(
                            thumbColor = RemoraTheme.accent,
                            activeTrackColor = RemoraTheme.accent,
                            inactiveTrackColor = RemoraTheme.border,
                        ),
                    )
                    Text("\u2600", fontSize = 20.sp, color = RemoraTheme.textPrimary)
                }

                Spacer(Modifier.height(16.dp))

                if (!isServerOnly) {
                    Button(
                        onClick = {
                            val config = currentConfig?.copy(
                                blur = blur,
                                brightness = brightness,
                                motionEnabled = motionEnabled,
                            ) ?: return@Button
                            if (WallpaperManager.applyWallpaper(config, WallpaperScope.Thread(threadKey!!), sourceScope)) {
                                onApplied()
                            }
                        },
                        colors = ButtonDefaults.buttonColors(
                            containerColor = RemoraTheme.accent,
                            contentColor = RemoraTheme.onAccentStrong,
                        ),
                        shape = RoundedCornerShape(10.dp),
                        modifier = Modifier.fillMaxWidth(),
                    ) {
                        Text("Apply for This Thread", fontSize = 13.sp, fontWeight = FontWeight.SemiBold)
                    }

                    Spacer(Modifier.height(8.dp))
                }

                if (resolvedServerId != null) {
                    Button(
                        onClick = {
                            val config = currentConfig?.copy(
                                blur = blur,
                                brightness = brightness,
                                motionEnabled = motionEnabled,
                            ) ?: return@Button
                            if (WallpaperManager.applyWallpaper(config, WallpaperScope.Server(resolvedServerId), sourceScope)) {
                                onApplied()
                            }
                        },
                        colors = ButtonDefaults.buttonColors(
                            containerColor = if (isServerOnly) RemoraTheme.accent else RemoraTheme.surface,
                            contentColor = if (isServerOnly) RemoraTheme.onAccentStrong else RemoraTheme.textPrimary,
                        ),
                        shape = RoundedCornerShape(10.dp),
                        modifier = Modifier.fillMaxWidth(),
                    ) {
                        Text(
                            "Apply for This Server",
                            fontSize = 13.sp,
                            fontWeight = if (isServerOnly) FontWeight.SemiBold else FontWeight.Normal,
                        )
                    }
                }
            }
        }
    }
}

@Composable
private fun SampleBubble(text: String, isUser: Boolean) {
    val bgColor = if (isUser) RemoraTheme.accent.copy(alpha = 0.15f) else RemoraTheme.surface.copy(alpha = 0.85f)

    Box(
        modifier = Modifier.fillMaxWidth(),
        contentAlignment = if (isUser) Alignment.CenterEnd else Alignment.CenterStart,
    ) {
        Text(
            text = text,
            color = RemoraTheme.textPrimary,
            fontSize = 13.sp,
            modifier = Modifier
                .background(bgColor, RoundedCornerShape(12.dp))
                .padding(horizontal = 12.dp, vertical = 8.dp),
        )
    }
}
