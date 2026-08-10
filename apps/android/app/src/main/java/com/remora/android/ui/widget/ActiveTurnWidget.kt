package com.remora.android.ui.widget

import android.content.Context
import androidx.compose.runtime.Composable
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.glance.GlanceId
import androidx.glance.GlanceModifier
import androidx.glance.GlanceTheme
import androidx.glance.appwidget.GlanceAppWidget
import androidx.glance.appwidget.GlanceAppWidgetManager
import androidx.glance.appwidget.provideContent
import androidx.glance.background
import androidx.glance.layout.Alignment
import androidx.glance.layout.Column
import androidx.glance.layout.Spacer
import androidx.glance.layout.fillMaxSize
import androidx.glance.layout.height
import androidx.glance.layout.padding
import androidx.glance.text.FontWeight
import androidx.glance.text.Text
import androidx.glance.text.TextStyle
import androidx.glance.unit.ColorProvider
import com.remora.android.state.AppModel
import com.remora.android.state.hasActiveTurn

class ActiveTurnWidget : GlanceAppWidget() {

    companion object {
        /** Refresh active-turn widget content from the current Rust store snapshot. */
        suspend fun triggerUpdate(context: Context) {
            val manager = GlanceAppWidgetManager(context)
            val ids = manager.getGlanceIds(ActiveTurnWidget::class.java)
            val widget = ActiveTurnWidget()
            for (id in ids) {
                widget.update(context, id)
            }
        }
    }

    override suspend fun provideGlance(context: Context, id: GlanceId) {
        val appModel = runCatching { AppModel.init(context) }.getOrNull()
        val snapshot = appModel?.snapshot?.value
        val activeCount = snapshot?.threads?.count { it.hasActiveTurn } ?: 0
        val projection = activeTurnWidgetProjection(activeCount)

        provideContent {
            GlanceTheme {
                ActiveTurnContent(projection = projection)
            }
        }
    }
}

private val BgColor = ColorProvider(androidx.compose.ui.graphics.Color(0xFF02082C))
private val PrimaryText = ColorProvider(androidx.compose.ui.graphics.Color(0xFFEAFBFF))
private val SecondaryText = ColorProvider(androidx.compose.ui.graphics.Color(0xFF6E8FA8))
private val AccentOcean = ColorProvider(androidx.compose.ui.graphics.Color(0xFF0DD5F0))

@Composable
private fun ActiveTurnContent(
    projection: ActiveTurnWidgetProjection,
) {
    if (projection.isActive) {
        Column(
            modifier = GlanceModifier
                .fillMaxSize()
                .background(BgColor)
                .padding(12.dp),
        ) {
            Text(
                text = "Remora",
                style = TextStyle(
                    color = AccentOcean,
                    fontSize = 14.sp,
                    fontWeight = FontWeight.Bold,
                ),
            )
            Spacer(modifier = GlanceModifier.height(6.dp))
            Text(
                text = projection.countLabel,
                style = TextStyle(
                    color = PrimaryText,
                    fontSize = 12.sp,
                    fontWeight = FontWeight.Medium,
                ),
            )
            Spacer(modifier = GlanceModifier.height(4.dp))
            Text(
                text = projection.statusLabel,
                style = TextStyle(
                    color = AccentOcean,
                    fontSize = 11.sp,
                    fontWeight = FontWeight.Medium,
                ),
            )
        }
    } else {
        Column(
            modifier = GlanceModifier
                .fillMaxSize()
                .background(BgColor)
                .padding(12.dp),
            horizontalAlignment = Alignment.CenterHorizontally,
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Text(
                text = "Remora",
                style = TextStyle(
                    color = AccentOcean,
                    fontSize = 14.sp,
                    fontWeight = FontWeight.Bold,
                ),
            )
            Spacer(modifier = GlanceModifier.height(4.dp))
            Text(
                text = projection.countLabel,
                style = TextStyle(
                    color = SecondaryText,
                    fontSize = 12.sp,
                ),
            )
            Spacer(modifier = GlanceModifier.height(4.dp))
            Text(
                text = projection.statusLabel,
                style = TextStyle(
                    color = SecondaryText,
                    fontSize = 11.sp,
                ),
            )
        }
    }
}
