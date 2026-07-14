package com.remora.android.ui

import androidx.compose.animation.core.LinearEasing
import androidx.compose.animation.core.RepeatMode
import androidx.compose.animation.core.animateFloat
import androidx.compose.animation.core.infiniteRepeatable
import androidx.compose.animation.core.rememberInfiniteTransition
import androidx.compose.animation.core.tween
import androidx.compose.foundation.Canvas
import androidx.compose.foundation.layout.size
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.geometry.CornerRadius
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.geometry.Size
import androidx.compose.ui.graphics.StrokeCap
import androidx.compose.ui.graphics.StrokeJoin
import androidx.compose.ui.graphics.drawscope.Stroke
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp

/** Compact animated Remora mark for splash and navigation surfaces. */
@Composable
fun AnimatedLogo(size: Dp = 44.dp) {
    val transition = rememberInfiniteTransition(label = "remora-mark")
    val pulse by transition.animateFloat(
        initialValue = 0.45f,
        targetValue = 1f,
        animationSpec = infiniteRepeatable(
            animation = tween(durationMillis = 1_400, easing = LinearEasing),
            repeatMode = RepeatMode.Reverse,
        ),
        label = "remora-mark-pulse",
    )

    Canvas(modifier = Modifier.size(size)) {
        val unit = this.size.minDimension / 108f
        val origin = Offset(
            x = (this.size.width - 108f * unit) / 2f,
            y = (this.size.height - 108f * unit) / 2f,
        )
        val stroke = 5f * unit
        val accent = RemoraTheme.accent

        drawRoundRect(
            color = accent.copy(alpha = 0.28f + pulse * 0.22f),
            topLeft = origin + Offset(22f * unit, 22f * unit),
            size = Size(64f * unit, 64f * unit),
            cornerRadius = CornerRadius(15f * unit),
            style = Stroke(width = stroke),
        )

        val lineStyle = Stroke(width = stroke, cap = StrokeCap.Round, join = StrokeJoin.Round)
        val stemX = origin.x + 38f * unit
        drawLine(
            color = accent,
            start = Offset(stemX, origin.y + 71f * unit),
            end = Offset(stemX, origin.y + 37f * unit),
            strokeWidth = lineStyle.width,
            cap = StrokeCap.Round,
        )
        drawArc(
            color = accent,
            startAngle = -90f,
            sweepAngle = 180f,
            useCenter = false,
            topLeft = Offset(origin.x + 38f * unit, origin.y + 36f * unit),
            size = Size(28f * unit, 22f * unit),
            style = lineStyle,
        )
        drawLine(
            color = accent,
            start = Offset(origin.x + 55f * unit, origin.y + 58f * unit),
            end = Offset(origin.x + 70f * unit, origin.y + 72f * unit),
            strokeWidth = lineStyle.width,
            cap = StrokeCap.Round,
        )
        drawCircle(
            color = accent.copy(alpha = 0.35f + pulse * 0.4f),
            radius = 3.5f * unit,
            center = Offset(origin.x + 78f * unit, origin.y + 30f * unit),
        )
    }
}
